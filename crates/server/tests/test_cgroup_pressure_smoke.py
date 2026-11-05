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

    def test_working_set_controls_pressure_verdict(self):
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
            self.assertEqual(smoke.pressured(sample), False if expected is not None else None)
            with patch.object(smoke, 'cgroup', return_value=sample):
                with self.assertRaises(smoke.SmokeInconclusive):
                    smoke.require_pressure_error('owned', lambda: None, [])


    def test_external_pressure_cannot_establish_pressure_at_admitted_run(self):
        for effective in ((smoke.LIMIT * 95 + 99) // 100, smoke.LIMIT):
            sample = {'memory.max': smoke.LIMIT, 'memory.current': smoke.LIMIT,
                      'diagnostic_working_set': {'bytes': effective}}
            self.assertTrue(smoke.pressured(sample))
            with patch.object(smoke, 'cgroup', return_value=sample):
                checks = []
                with self.assertRaisesRegex(smoke.SmokeInconclusive, 'decision-time pressure'):
                    smoke.require_pressure_error('owned', lambda: None, checks)
            self.assertEqual(checks[0]['before'], sample)
            self.assertEqual(checks[0]['after'], sample)

    def test_external_pressure_admission_is_inconclusive_receipt_never_pass(self):
        sample = {'memory.max': smoke.LIMIT,
                  'diagnostic_working_set': {'bytes': smoke.LIMIT}}
        def run(args, report):
            report['owned_resources_cleaned'] = True
            smoke.require_pressure_error('owned', lambda: None,
                                         report.setdefault('run_pressure_checks', []),
                                         sample=lambda *a, **kw: sample, trace=True)
            report['result'] = 'PASS'
        with tempfile.TemporaryDirectory() as folder:
            output = Path(folder) / 'receipt.json'
            with patch.object(sys, 'argv', ['smoke', '--image', 'local', '--output', str(output)]), \
                 patch.object(smoke, 'run', side_effect=run), contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(smoke.main(), 1)
            receipt = json.loads(output.read_text())
        self.assertEqual(receipt['result'], 'INCONCLUSIVE')
        check = receipt['run_pressure_checks'][0]
        self.assertEqual(check['before'], sample)
        self.assertEqual(check['after'], sample)
        self.assertLessEqual(check['operation_wall_start_ns'], check['operation_wall_end_ns'])

    def test_oom_and_cleanup_failure_dominate_unproven_decision_pressure(self):
        sample = {'memory.max': smoke.LIMIT,
                  'diagnostic_working_set': {'bytes': smoke.LIMIT}}
        for extra in ({'cleanup_cgroup': {'memory.events': {'oom': 1}}},
                      {'owned_resources_cleaned': False},
                      {'cleanup_errors': [{'operation': 'remove', 'error_type': 'TimeoutExpired'}]}):
            def run(args, report):
                report.update(owned_resources_cleaned=True)
                report.update(extra)
                smoke.require_pressure_error('owned', lambda: None, [],
                                             sample=lambda *a, **kw: sample)
            with self.subTest(extra=extra), tempfile.TemporaryDirectory() as folder:
                output = Path(folder) / 'receipt.json'
                with patch.object(sys, 'argv', ['smoke', '--image', 'local', '--output', str(output)]), \
                     patch.object(smoke, 'run', side_effect=run), contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(smoke.main(), 1)
                self.assertEqual(json.loads(output.read_text())['result'], 'FAIL')

    def test_invalid_or_changed_limits_cannot_fail_an_admitted_run(self):
        for cap in (0, -1, None, 'max', True):
            sample = {'memory.max': cap, 'diagnostic_working_set': {'bytes': 100}}
            self.assertIsNone(smoke.pressured(sample))
        for capacities in ((smoke.LIMIT, smoke.LIMIT // 2),
                           (smoke.LIMIT // 2, smoke.LIMIT // 2)):
            samples = [{'memory.max': cap, 'memory.current': cap,
                        'diagnostic_working_set': {'bytes': cap}} for cap in capacities]
            with patch.object(smoke, 'cgroup', side_effect=samples):
                with self.assertRaises(smoke.SmokeInconclusive):
                    smoke.require_pressure_error('owned', lambda: None, [])

    def test_fill_never_crosses_raw_safety_margin_to_offset_cache(self):
        sample = {'memory.max': smoke.LIMIT, 'memory.current': smoke.LIMIT * 96 // 100,
                  'diagnostic_working_set': {'bytes': smoke.LIMIT // 2}}
        with patch.object(smoke, 'cgroup', return_value=sample), \
             patch.object(smoke, 'docker') as docker:
            with self.assertRaises(smoke.SmokeInconclusive):
                smoke.fill_to_pressure('owned', '/owned-fill', [])
        docker.assert_not_called()

    def test_cgroup_pressure_hint_passes_even_without_external_bracketing_samples(self):
        class Refused(Exception):
            code = smoke.PRESSURE_CODE
            message = ('container pressure [brainwire.memory reason=cgroup requested_bytes=0 '
                       'available_bytes=0 rolled_back=true]')
        def operation():
            raise Refused()
        with patch.object(smoke, 'cgroup', return_value={}):
            checks = []
            evidence = smoke.require_pressure_error('owned', operation, checks)
        self.assertEqual(evidence['code'], smoke.PRESSURE_CODE)
        self.assertEqual(evidence['reason'], 'cgroup')
        self.assertTrue(evidence['rolled_back'])
        self.assertEqual(len(checks), 1)

    def test_other_memory_limits_or_untrusted_hints_are_inconclusive(self):
        hint = ('[brainwire.memory reason=cgroup requested_bytes=0 '
                'available_bytes=0 rolled_back=true]')
        for message in ('', hint.replace('cgroup', 'query_pool'),
                        hint.replace('cgroup', 'process_limit'),
                        hint.replace('cgroup', 'statement_limit'),
                        hint.replace('cgroup', 'ingress'), hint + ' suffix',
                        hint.replace('rolled_back=true', 'rolled_back=false'),
                        hint.replace('requested_bytes=0', f'requested_bytes={1 << 64}')):
            with self.subTest(message=message):
                class Refused(Exception):
                    code = smoke.PRESSURE_CODE
                failure = Refused()
                failure.message = message
                def operation():
                    raise failure
                with patch.object(smoke, 'cgroup', return_value={}):
                    with self.assertRaises(smoke.SmokeInconclusive):
                        smoke.require_pressure_error('owned', operation, [])

    def test_unavailable_memory_stat_preserves_raw_samples_and_is_inconclusive(self):
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
                self.assertIsNone(smoke.pressured(sample))
                with patch.object(smoke, 'cgroup', return_value=sample):
                    with self.assertRaises(smoke.SmokeInconclusive):
                        smoke.require_pressure_error('owned', lambda: None, [])


if __name__ == '__main__':
    unittest.main()
