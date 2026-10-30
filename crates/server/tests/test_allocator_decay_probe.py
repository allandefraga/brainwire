"""Pure checks for allocator probe precision and failure handling; no Docker."""
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

import allocator_decay_probe as probe


class AllocatorProbeTests(unittest.TestCase):
    def test_confirmation_compares_complete_numeric_values(self):
        text = '<jemalloc>: Set conf value: dirty_decay_ms:10000\n<jemalloc>: Set conf value: muzzy_decay_ms:10000'
        self.assertFalse(probe.confirmed(text, 1000))
        self.assertTrue(probe.confirmed(text, 10000))
        self.assertFalse(probe.confirmed(text.replace('10000', '1000garbage'), 1000))
        self.assertFalse(probe.confirmed('Set conf value: dirty_decay_ms:1000', 1000))
        self.assertTrue(probe.confirmed(text + '\nSet conf value: dirty_decay_ms:1000\nSet conf value: muzzy_decay_ms:1000', 1000))
        self.assertTrue(probe.confirmed('Set conf value: dirty_decay_ms:0\nSet conf value: muzzy_decay_ms:0', 0))

    def test_image_tag_is_resolved_once_and_all_runs_use_that_id(self):
        image_id = 'sha256:' + 'a' * 64
        calls = []
        state = {'Image': image_id, 'State': {'Running': True, 'ExitCode': 0},
                 'RestartCount': 0, 'NetworkSettings': {'Ports': {'7687/tcp': [{'HostPort': '17687'}]}}}

        def docker(*args, **kwargs):
            calls.append(args)
            if args[:2] == ('image', 'inspect'):
                return json.dumps([{'Id': image_id, 'Config': {'Labels': {}}}])
            if args[0] == 'stop':
                state['State']['Running'] = False
            if args[0] == 'run' and '-audit' in ' '.join(args):
                return json.dumps({'status': 'ok'})
            return 'owned'

        driver = MagicMock()
        driver.__enter__.return_value = driver
        session = driver.session.return_value.__enter__.return_value
        session.run.return_value.single.return_value = {'n': 1}
        sample = {'pid1_memory_kib': {'VmHWM': 100, 'VmRSS': 80},
                  'memory.peak': 200000, 'memory.events': {'oom': 0, 'oom_kill': 0}}
        logs = subprocess.CompletedProcess([], 0,
            'Set conf value: dirty_decay_ms:1000\nSet conf value: muzzy_decay_ms:1000', '')
        neo4j = types.SimpleNamespace(GraphDatabase=types.SimpleNamespace(driver=lambda *a, **kw: driver), __version__='test')
        args = argparse.Namespace(image='moving-tag:latest', decay_ms=1000, rows=1, batch=1,
                                  payload_bytes=1, idle_seconds=0)
        report = {'result': 'FAIL'}
        with patch.dict(sys.modules, {'neo4j': neo4j}), \
             patch.object(probe.pressure, 'docker', side_effect=docker), \
             patch.object(probe.pressure, 'inspect_owned', return_value=state), \
             patch.object(probe.pressure, 'sample', return_value=sample), \
             patch.object(probe.pressure, 'require_finite'), \
             patch.object(probe.pressure, 'power_receipt', return_value={}), \
             patch.object(probe.pressure, 'command', return_value=logs), \
             patch.object(probe.pressure, 'stop_events'), \
             patch.object(probe.subprocess, 'Popen'), \
             patch.object(probe, 'finalize'):
            probe.run(args, report)
        self.assertEqual(report['result'], 'PASS')
        self.assertEqual(report['image'], 'moving-tag:latest')
        self.assertEqual(report['image_id'], image_id)
        self.assertEqual(sum(call[:2] == ('image', 'inspect') for call in calls), 1)
        runs = [call for call in calls if call[0] == 'run']
        self.assertEqual(len(runs), 3)
        for call in runs:
            self.assertIn(image_id, call)
            self.assertNotIn('moving-tag:latest', call)

    def test_cleanup_attempts_other_resources_and_keeps_primary_error(self):
        resources = {'server': {'State': {'Running': False, 'ExitCode': 137}, 'RestartCount': 0},
                     'server-init': {}, 'server-audit': {}, 'volume': {}}
        removed = []

        def inspect(kind, name, identity):
            return resources.get(name)

        def docker(*args, **kwargs):
            name = args[-1]
            if name == 'server':
                raise RuntimeError('primary server removal failed')
            removed.append(name)
            resources.pop(name, None)
            return ''

        report = {'result': 'FAIL', 'error': 'original load failure'}
        with tempfile.TemporaryDirectory() as directory, \
             patch.object(probe.pressure, 'inspect_owned', side_effect=inspect), \
             patch.object(probe.pressure, 'docker', side_effect=docker), \
             patch.object(probe.pressure, 'command'), \
             patch.object(probe.pressure, 'stop_events', side_effect=RuntimeError('observer failure')), \
             patch.object(probe.pressure, 'record_final_power', side_effect=OSError('power unavailable')):
            probe.finalize(report, 'identity', 'server', 'volume', None, Path(directory) / 'events')
        self.assertEqual(report['error'], 'original load failure')
        self.assertEqual(report['result'], 'FAIL')
        self.assertFalse(report['owned_resources_cleaned'])
        self.assertEqual(set(removed), {'server-init', 'server-audit', 'volume'})
        self.assertGreaterEqual(len(report['cleanup_errors']), 3)

    def test_cleanup_never_removes_resource_with_unknown_ownership(self):
        def inspect(kind, name, identity):
            if name == 'server':
                raise RuntimeError('unowned container')
            return None
        report = {'result': 'PASS'}
        with tempfile.TemporaryDirectory() as directory, \
             patch.object(probe.pressure, 'inspect_owned', side_effect=inspect), \
             patch.object(probe.pressure, 'docker') as docker, \
             patch.object(probe.pressure, 'command') as command, \
             patch.object(probe.pressure, 'stop_events'), \
             patch.object(probe.pressure, 'record_final_power'):
            probe.finalize(report, 'identity', 'server', 'volume', None, Path(directory) / 'events')
        docker.assert_not_called()
        command.assert_not_called()
        self.assertEqual(report['result'], 'FAIL')
        self.assertFalse(report['owned_resources_cleaned'])

    def test_crash_and_idle_oom_evidence_are_failures(self):
        self.assertTrue(probe.failure_observed({'cleanup_state': {'Running': False, 'ExitCode': 137}}))
        self.assertTrue(probe.failure_observed({'cleanup_restart_count': 1}))
        self.assertFalse(probe.failure_observed({'stopped_state': {'Running': False, 'ExitCode': 0}}))
        self.assertTrue(probe.failure_observed({'before': {'memory.events': {'oom': 0}},
            'idle_samples': [{'memory.events': {'oom': 1}}]}))

    def test_inconclusive_after_crash_does_not_hide_failed_process(self):
        def failed(args, report):
            report['cleanup_state'] = {'Running': False, 'ExitCode': 137}
            raise probe.Inconclusive('sample unavailable after crash')
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'receipt.json'
            with patch.object(sys, 'argv', ['probe', '--image', 'local', '--output', str(output)]), \
                 patch.object(probe, 'run', side_effect=failed), contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(probe.main(), 1)
            receipt = json.loads(output.read_text())
        self.assertEqual(receipt['result'], 'FAIL')
        self.assertEqual(receipt['error'], 'sample unavailable after crash')


if __name__ == '__main__':
    unittest.main()
