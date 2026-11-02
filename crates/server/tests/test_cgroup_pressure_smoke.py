"""Ownership and immutable-image regressions; never contacts Docker."""
import argparse
import contextlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import types
import unittest
from unittest.mock import MagicMock, patch

import cgroup_pressure_smoke as smoke


class SmokeOwnershipTests(unittest.TestCase):
    def test_timeout_cleanup_removes_all_owned_containers(self):
        resources = {'server': {'Id': 'server'}, 'server-init': {'Id': 'server-init'}, 'server-audit': {'Id': 'server-audit'}, 'volume': {}}
        removed = []
        def inspect(kind, name, identity):
            return resources.get(name)
        def docker(*args, **kwargs):
            removed.append(args[-1])
            resources.pop(args[-1], None)
        with tempfile.TemporaryDirectory() as folder, \
             patch.object(smoke, 'inspect_owned', side_effect=inspect), \
             patch.object(smoke, 'docker', side_effect=docker), \
             patch.object(smoke, 'command'), patch.object(smoke, 'stop_events'):
            report = {'result': 'FAIL', 'error': 'original timeout'}
            smoke.finalize(report, 'identity', 'server', 'volume', True, None, Path(folder)/'events')
        self.assertEqual(set(removed), set(('server', 'server-init', 'server-audit', 'volume')))
        self.assertEqual(report['error'], 'original timeout')
        self.assertTrue(report['owned_resources_cleaned'])

    def test_unowned_container_is_never_stopped_or_removed(self):
        def inspect(kind, name, identity):
            if name == 'server-init':
                raise RuntimeError('refusing unowned container')
            return None
        with tempfile.TemporaryDirectory() as folder, \
             patch.object(smoke, 'inspect_owned', side_effect=inspect), \
             patch.object(smoke, 'docker') as docker, \
             patch.object(smoke, 'command') as command, patch.object(smoke, 'stop_events'):
            report = {'result': 'PASS'}
            smoke.finalize(report, 'identity', 'server', 'volume', True, None, Path(folder)/'events')
        docker.assert_not_called()
        command.assert_not_called()
        self.assertEqual(report['result'], 'FAIL')
        self.assertFalse(report['owned_resources_cleaned'])

    def test_init_timeout_uses_image_id_and_still_cleans_auxiliary_container(self):
        image_id = 'sha256:' + 'a'*64
        resources = {}
        runs = []
        def docker(*args, **kwargs):
            if args[:2] == ('volume', 'create'):
                resources[args[-1]] = {}
            elif args[:2] == ('image', 'inspect'):
                return json.dumps([{'Id': image_id}])
            elif args[0] == 'run':
                runs.append(args)
                name = args[args.index('--name')+1]
                resources[name] = {'Id': name}
                raise subprocess.TimeoutExpired('docker', 120)
            elif args[0] == 'rm' or args[:2] == ('volume', 'rm'):
                resources.pop(args[-1], None)
            return ''
        neo4j = types.SimpleNamespace(GraphDatabase=None, __version__='test')
        report = {'result': 'FAIL'}
        with patch.dict(sys.modules, {'neo4j': neo4j}), \
             patch.object(smoke, 'docker', side_effect=docker), \
             patch.object(smoke, 'inspect_owned', side_effect=lambda kind,name,identity: resources.get(name)), \
             patch.object(smoke, 'command'), patch.object(smoke, 'stop_events'):
            with self.assertRaises(subprocess.TimeoutExpired):
                smoke.run(argparse.Namespace(image='moving:latest'), report)
        self.assertIn(image_id, runs[0])
        self.assertNotIn('moving:latest', runs[0])
        self.assertFalse(resources)
        self.assertTrue(report['owned_resources_cleaned'])


    def test_server_and_audit_use_same_local_image_id_and_cleanup_on_audit_timeout(self):
        image_id = 'sha256:' + 'b'*64
        resources, runs = {}, []
        snapshot = {'memory.max': smoke.LIMIT, 'memory.current': 1000,
                    'memory.swap.max': 0, 'memory.events': {'oom': 0, 'oom_kill': 0}}
        def docker(*args, **kwargs):
            if args[:2] == ('volume', 'create'):
                resources[args[-1]] = {}
            elif args[:2] == ('image', 'inspect'):
                return json.dumps([{'Id': image_id}])
            elif args[0] == 'run':
                runs.append(args)
                name = args[args.index('--name')+1]
                resources[name] = {'Id': name, 'Image': image_id, 'State': {'Running': True},
                    'NetworkSettings': {'Ports': {'7687/tcp': [{'HostPort': '17687'}]}}}
                if name.endswith('-audit'):
                    raise subprocess.TimeoutExpired('docker', 120)
            elif args[0] == 'rm' or args[:2] == ('volume', 'rm'):
                resources.pop(args[-1], None)
            return ''
        driver = MagicMock()
        driver.__enter__.return_value = driver
        driver.get_server_info.return_value.protocol_version = 'test'
        session = driver.session.return_value.__enter__.return_value
        session.run.side_effect = lambda query, **kw: types.SimpleNamespace(
            consume=lambda: None, single=lambda: {'ok': 1, 'n': int(kw.get('id') == 'acknowledged')})
        neo4j = types.SimpleNamespace(GraphDatabase=types.SimpleNamespace(driver=lambda *a, **kw: driver), __version__='test')
        report = {'result': 'FAIL'}
        with patch.dict(sys.modules, {'neo4j': neo4j}), \
             patch.object(smoke, 'docker', side_effect=docker), \
             patch.object(smoke, 'inspect_owned', side_effect=lambda kind,name,identity: resources.get(name)), \
             patch.object(smoke, 'cgroup', return_value=snapshot), \
             patch.object(smoke, 'fill_to_pressure'), patch.object(smoke, 'require_pressure_error'), \
             patch.object(smoke, 'command', return_value=subprocess.CompletedProcess([], 0, '', '')), \
             patch.object(smoke, 'stop_events'), patch.object(smoke.subprocess, 'Popen'):
            with self.assertRaises(subprocess.TimeoutExpired):
                smoke.run(argparse.Namespace(image='moving:latest'), report)
        self.assertEqual(len(runs), 3)
        for call in runs:
            self.assertIn(image_id, call)
            self.assertNotIn('moving:latest', call)
        self.assertTrue(report['owned_resources_cleaned'])
        self.assertFalse(resources)

    def test_inspect_rejects_wrong_uuid_before_resource_operation(self):
        state = {'Config': {'Labels': {smoke.LABEL: 'someone-else'}}}
        completed = subprocess.CompletedProcess([], 0, json.dumps([state]), '')
        with patch.object(smoke, 'command', return_value=completed):
            with self.assertRaisesRegex(RuntimeError, 'unowned'):
                smoke.inspect_owned('container', 'server', 'mine')


    def test_mismatched_server_image_fails_before_connecting(self):
        image_id = 'sha256:' + 'c'*64
        def docker(*args, **kwargs):
            return json.dumps([{'Id': image_id}]) if args[:2] == ('image', 'inspect') else ''
        state = {'Image': 'sha256:' + 'd'*64, 'State': {'Running': False}}
        graph_database = MagicMock()
        neo4j = types.SimpleNamespace(GraphDatabase=graph_database, __version__='test')
        with patch.dict(sys.modules, {'neo4j': neo4j}), \
             patch.object(smoke, 'docker', side_effect=docker), \
             patch.object(smoke, 'inspect_owned', return_value=state), \
             patch.object(smoke, 'stop_events'), patch.object(smoke.subprocess, 'Popen'), \
             patch.object(smoke, 'finalize'):
            with self.assertRaisesRegex(RuntimeError, 'image does not match'):
                smoke.run(argparse.Namespace(image='moving:latest'), {'result': 'FAIL'})
        graph_database.driver.assert_not_called()


    def test_cleanup_uses_immutable_id_and_accepts_auto_removed_container(self):
        state = {'Id': 'immutable-container-id', 'State': {'Running': False}}
        live = {'present': True}
        def inspect(kind, name, identity):
            return state if name in ('server-init', 'immutable-container-id') and live['present'] else None
        def command(*args, **kwargs):
            self.assertEqual(args[-1], 'immutable-container-id')
            live['present'] = False  # --rm removes the container during stop.
            return subprocess.CompletedProcess(args, 0, '', '')
        def docker(*args, **kwargs):
            self.assertEqual(args[-1], 'immutable-container-id')
            raise RuntimeError('no such container')
        report = {'result': 'PASS'}
        with tempfile.TemporaryDirectory() as folder, \
             patch.object(smoke, 'inspect_owned', side_effect=inspect), \
             patch.object(smoke, 'command', side_effect=command), \
             patch.object(smoke, 'docker', side_effect=docker), patch.object(smoke, 'stop_events'):
            smoke.finalize(report, 'identity', 'server', 'volume', False, None, Path(folder)/'events')
        self.assertTrue(report['owned_resources_cleaned'], report)
        self.assertEqual(report['result'], 'PASS')

    def test_cleanup_failure_remains_failure_after_inconclusive_exception(self):
        def run(args, report):
            report.update(result='FAIL', owned_resources_cleaned=False,
                          cleanup_errors=[{'operation': 'remove', 'error_type': 'TimeoutExpired'}])
            raise smoke.SmokeInconclusive('pressure changed')
        with tempfile.TemporaryDirectory() as folder:
            output = Path(folder)/'receipt.json'
            with patch.object(sys, 'argv', ['smoke', '--image', 'local', '--output', str(output)]), \
                 patch.object(smoke, 'run', side_effect=run), contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(smoke.main(), 1)
            self.assertEqual(json.loads(output.read_text())['result'], 'FAIL')


    def test_volume_create_timeout_cleans_effective_owned_volume(self):
        resources = {}
        def docker(*args, **kwargs):
            if args[:2] == ('volume', 'create'):
                resources[args[-1]] = {}
                raise subprocess.TimeoutExpired('docker', 60)
            if args[:2] == ('volume', 'rm'):
                resources.pop(args[-1], None)
                return ''
            raise AssertionError(f'unexpected Docker mutation {args}')
        neo4j = types.SimpleNamespace(GraphDatabase=None, __version__='test')
        report = {'result': 'FAIL'}
        with patch.dict(sys.modules, {'neo4j': neo4j}), \
             patch.object(smoke, 'docker', side_effect=docker), \
             patch.object(smoke, 'inspect_owned', side_effect=lambda kind,name,identity: resources.get(name)), \
             patch.object(smoke, 'command'), patch.object(smoke, 'stop_events'):
            with self.assertRaises(subprocess.TimeoutExpired):
                smoke.run(argparse.Namespace(image='local'), report)
        self.assertFalse(resources)
        self.assertTrue(report['owned_resources_cleaned'])


    def test_optional_working_set_diagnostic_is_strict(self):
        for text in ('anon 42\n', 'inactive_file -1', 'inactive_file 1KiB',
                     'inactive_file 1 extra', 'inactive_file 1\ninactive_file 2',
                     'inactive_file 18446744073709551616', 'inactive_file invalid'):
            with self.subTest(text=text):
                self.assertIsNone(smoke.inactive_file_bytes(text))
        self.assertEqual(smoke.inactive_file_bytes('anon 4\ninactive_file 10\n'), 10)
        self.assertEqual(smoke.inactive_file_bytes('inactive_file 0'), 0)

    def test_working_set_diagnostics_do_not_change_pressure_verdict(self):
        for stat, expected in [('inactive_file 10', 90), ('anon 12', None),
                               ('inactive_file invalid', None)]:
            def command(*args, **kwargs):
                key = args[-1].rsplit('/', 1)[-1]
                value = {'memory.max': '100', 'memory.current': '100', 'memory.peak': '100',
                         'memory.swap.max': '0', 'memory.events': 'oom 0', 'memory.stat': stat}[key]
                return subprocess.CompletedProcess(args, 0, value, '')
            with patch.object(smoke, 'command', side_effect=command):
                sample = smoke.cgroup('owned')
            self.assertEqual(sample['diagnostic_working_set']['bytes'], expected)
            self.assertEqual(sample['diagnostic_working_set']['unit'], 'bytes')
            self.assertTrue(smoke.pressured(sample))
            with patch.object(smoke, 'cgroup', return_value=sample):
                with self.assertRaisesRegex(AssertionError, 'RUN admitted'):
                    smoke.require_pressure_error('owned', lambda: None, [])


    def test_unavailable_memory_stat_preserves_raw_samples_and_failure(self):
        for failure in ('nonzero', OSError('diagnostic unavailable'),
                        subprocess.TimeoutExpired('docker exec', 10)):
            with self.subTest(failure=type(failure).__name__):
                def command(*args, **kwargs):
                    key = args[-1].rsplit('/', 1)[-1]
                    if key == 'memory.stat':
                        if isinstance(failure, Exception):
                            raise failure
                        return subprocess.CompletedProcess(args, 1, '', 'unavailable')
                    value = {'memory.max': '100', 'memory.current': '100', 'memory.peak': '100',
                             'memory.swap.max': '0', 'memory.events': 'oom 0'}[key]
                    return subprocess.CompletedProcess(args, 0, value, '')
                with patch.object(smoke, 'command', side_effect=command):
                    sample = smoke.cgroup('owned')
                self.assertEqual(sample['memory.current'], 100)
                self.assertEqual(sample['memory.max'], 100)
                self.assertIsNone(sample['memory.stat.inactive_file'])
                self.assertIsNone(sample['diagnostic_working_set']['bytes'])
                self.assertTrue(smoke.pressured(sample))
                with patch.object(smoke, 'cgroup', return_value=sample):
                    with self.assertRaisesRegex(AssertionError, 'RUN admitted'):
                        smoke.require_pressure_error('owned', lambda: None, [])


if __name__ == '__main__':
    unittest.main()
