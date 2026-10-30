"""Pure safety/receipt checks for sustained memory harness; no Docker."""
import argparse
import contextlib
import copy
import io
import json
from pathlib import Path
import sys
import subprocess
import tempfile
import threading
import unittest
from unittest.mock import MagicMock, patch

import memory_soak as soak


class SoakTests(unittest.TestCase):
    def power(self):
        return {'power_evidence': {'eligible': True, 'ac_confirmed': True,
            'supply_scan_complete': True, 'normal_power_mode': True}}

    def point(self):
        return {'memory.max': 2 << 30, 'memory.swap.max': 0,
            'memory.current': 100, 'memory.peak': 200,
            'memory.events': {'oom': 0, 'oom_kill': 0},
            'pid1.cgroup': '0::/', 'exec.cgroup': '0::/',
            'pid1_memory_kib': {'VmRSS': 20, 'VmHWM': 30},
            'host_power': self.power()}

    def receipt(self):
        return {'result': 'PASS', 'duration_seconds': 1800, 'elapsed_seconds': 1800.1,
            'writers': 8, 'readers': 4, 'workers': [
                {'operations': 1, 'errors': []} for _ in range(12)],
            'samples': [self.point() for _ in range(3)],
            'audit': {'status': 'ok'}, 'kernel_log': {'available': True,
                'identity_verified': True, 'continuous_prefix': True, 'matched_lines': []},
            'owned_resources_cleaned': True,
            'host_before': self.power(), 'host_after': self.power()}

    def power_fixture(self, supply_values, policy_values, profile=None):
        with tempfile.TemporaryDirectory() as directory:
            supplies, policies = Path(directory) / 'supplies', Path(directory) / 'policies'
            supplies.mkdir()
            policies.mkdir()
            for root, values in ((supplies, supply_values), (policies, policy_values)):
                for name, fields in values.items():
                    path = root / name
                    path.mkdir()
                    for key, value in fields.items():
                        (path / key).write_text(value)
            with patch.object(soak, 'read_power_profile', return_value=profile):
                return soak.linux_power_evidence(supplies, policies)

    def test_battery_requires_external_online_and_normal_mode(self):
        battery = {'BAT0': {'type': 'Battery', 'present': '1', 'status': 'Discharging'}}
        performance = {'policy0': {'scaling_governor': 'performance'}}
        report = self.power_fixture(battery, performance)
        self.assertFalse(report['eligible'])
        self.assertTrue(report['battery_detected'])
        self.assertFalse(report['ac_confirmed'])
        supplied = {**battery, 'AC': {'type': 'Mains', 'online': '1'}}
        report = self.power_fixture(supplied, performance)
        self.assertTrue(report['eligible'])
        self.assertTrue(report['ac_confirmed'])
        report = self.power_fixture(supplied, {}, profile='power-saver')
        self.assertFalse(report['eligible'])
        self.assertFalse(report['normal_power_mode'])

    def test_no_battery_records_observation_without_claiming_physical_ac(self):
        report = self.power_fixture({}, {}, profile='balanced')
        self.assertFalse(report['eligible'])
        self.assertFalse(report['battery_detected'])
        self.assertEqual(report['power_source'], 'no_battery_detected')
        self.assertFalse(report['ac_confirmed'])
        self.assertTrue(report['normal_power_mode'])

    def test_virtual_machine_missing_power_policy_is_inconclusive(self):
        report = self.power_fixture({}, {})
        self.assertFalse(report['eligible'])
        self.assertIsNone(report['normal_power_mode'])
        self.assertFalse(report['battery_detected'])
        with self.assertRaises(soak.pressure.Inconclusive):
            soak.require_power({'power_evidence': report})

    def test_cpu_policy_low_power_overrides_profile_and_unknown_is_not_normal(self):
        report = self.power_fixture({}, {'policy0': {
            'scaling_governor': 'performance', 'energy_performance_preference': 'power'}},
            profile='balanced')
        self.assertFalse(report['eligible'])
        self.assertFalse(report['normal_power_mode'])
        report = self.power_fixture({}, {'policy0': {'scaling_governor': 'schedutil'}})
        self.assertIsNone(report['normal_power_mode'])
        self.assertFalse(report['eligible'])
        report = self.power_fixture({'AC': {'type': 'Mains', 'online': '1'}}, {'policy0': {
            'scaling_governor': 'powersave', 'energy_performance_preference': 'balance_performance'}})
        self.assertTrue(report['eligible'])

    def test_unknown_supply_type_cannot_prove_absence_of_battery(self):
        report = self.power_fixture({'unknown': {'online': '1'}}, {}, profile='performance')
        self.assertFalse(report['supply_scan_complete'])
        self.assertIsNone(report['battery_detected'])
        self.assertFalse(report['eligible'])

    def test_servers_need_external_online_despite_no_battery(self):
        for supplied in ({}, {'AC': {'type': 'Mains', 'online': '0'}}):
            report = self.power_fixture(supplied, {}, profile='performance')
            self.assertFalse(report['eligible'])
            self.assertFalse(report['ac_confirmed'])
        report = self.power_fixture({'AC': {'type': 'Mains', 'online': '1'}},
                                    {}, profile='performance')
        self.assertTrue(report['eligible'])
        self.assertTrue(report['ac_confirmed'])
        self.assertFalse(report['battery_detected'])

    def test_usb_ups_and_unknown_online_do_not_count_as_mains(self):
        for kind in ('USB', 'USB_C', 'USB_PD', 'UPS', 'Unknown'):
            report = self.power_fixture({'source': {'type': kind, 'online': '1'}},
                                        {}, profile='performance')
            self.assertFalse(report['eligible'], kind)
            self.assertFalse(report['ac_confirmed'], kind)
        report = self.power_fixture({'source': {'type': 'Mains', 'online': '1'}},
                                    {}, profile='performance')
        self.assertTrue(report['eligible'])

    def test_power_sample_changes_or_missing_data_invalidate_receipt(self):
        receipt = self.receipt()
        self.assertTrue(soak.qualification(receipt))
        for replacement in ({}, {'power_evidence': {'eligible': True}},
                            {'power_evidence': {**self.power()['power_evidence'], 'ac_confirmed': False}},
                            {'power_evidence': {**self.power()['power_evidence'], 'profile': 'changed'}}):
            receipt = self.receipt()
            receipt['samples'][1]['host_power'] = replacement
            self.assertFalse(soak.qualification(receipt))
            with self.assertRaises(soak.pressure.Inconclusive):
                soak.require_same_power(receipt['host_before'], replacement)
        receipt = self.receipt()
        receipt['host_after']['power_evidence']['profile'] = 'changed'
        self.assertFalse(soak.qualification(receipt))
        soak.require_same_power(self.power(), self.power())

    def test_profile_command_unavailable_does_not_claim_normal_mode(self):
        with patch.object(soak.pressure, 'command', side_effect=FileNotFoundError):
            self.assertIsNone(soak.read_power_profile())
        for value in ('balanced', 'performance', 'power-saver'):
            result = subprocess.CompletedProcess([], 0, value + '\n', '')
            with patch.object(soak.pressure, 'command', return_value=result):
                self.assertEqual(soak.read_power_profile(), value)
        result = subprocess.CompletedProcess([], 0, 'unknown', '')
        with patch.object(soak.pressure, 'command', return_value=result):
            self.assertIsNone(soak.read_power_profile())

    def test_boot_identity_requires_native_linux_and_valid_uuid(self):
        identity = '4caa4006-be2e-467a-8bc0-d25813527643'
        with patch.object(soak.platform, 'system', return_value='Darwin'):
            with self.assertRaises(soak.pressure.Inconclusive):
                soak.host_boot_id()
        with patch.object(soak.platform, 'system', return_value='Linux'), \
             patch.object(soak.Path, 'read_text', return_value=identity + '\n'):
            self.assertEqual(soak.host_boot_id(), identity)
        with patch.object(soak.platform, 'system', return_value='Linux'), \
             patch.object(soak.Path, 'read_text', return_value='invalid'):
            with self.assertRaises(soak.pressure.Inconclusive):
                soak.host_boot_id()

    def test_host_kernel_read_fails_closed_for_permission_empty_and_boot_change(self):
        for code, text in ((1, 'denied'), (0, '')):
            result = subprocess.CompletedProcess([], code, text, '')
            with self.subTest(code=code, text=text), \
                 patch.object(soak, 'host_boot_id', return_value='boot'), \
                 patch.object(soak.pressure, 'command', return_value=result):
                with self.assertRaises(soak.pressure.Inconclusive):
                    soak.kernel_snapshot()
        result = subprocess.CompletedProcess([], 0, '[1] kernel ready\n', '')
        with patch.object(soak, 'host_boot_id', side_effect=['one', 'two']), \
             patch.object(soak.pressure, 'command', return_value=result):
            with self.assertRaises(soak.pressure.Inconclusive):
                soak.kernel_snapshot()
        with patch.object(soak, 'host_boot_id', return_value='one'), \
             patch.object(soak.pressure, 'command', return_value=result) as command:
            self.assertEqual(soak.kernel_snapshot(),
                             {'boot_id': 'one', 'lines': ['[1] kernel ready']})
            command.assert_called_once_with('dmesg', '--raw', '--color=never',
                                            check=False, timeout=10)

    def test_remote_or_desktop_kernel_and_permission_errors_are_rejected(self):
        for code, identity in ((1, 'host'), (0, 'remote')):
            result = subprocess.CompletedProcess([], code, identity + '\n', '')
            with self.subTest(code=code, identity=identity), \
                 patch.object(soak, 'host_boot_id', return_value='host'), \
                 patch.object(soak.pressure, 'command', return_value=result):
                with self.assertRaises(soak.pressure.Inconclusive):
                    soak.verify_kernel_host('owned', {'boot_id': 'host'})
        result = subprocess.CompletedProcess([], 0, 'host\n', '')
        with patch.object(soak, 'host_boot_id', return_value='host'), \
             patch.object(soak.pressure, 'command', return_value=result):
            soak.verify_kernel_host('owned', {'boot_id': 'host'})

    def test_kernel_increment_ignores_old_oom_and_reports_only_new_records(self):
        before = {'boot_id': 'one', 'lines': ['[1] old killed process 1', '[2] ready']}
        after = {'boot_id': 'one', 'lines': before['lines'] + ['[3] idle']}
        evidence = soak.kernel_increment(before, after)
        self.assertEqual(evidence['matched_lines'], [])
        self.assertEqual(evidence['new_line_count'], 1)
        self.assertTrue(evidence['identity_verified'])
        self.assertTrue(evidence['continuous_prefix'])
        self.assertNotIn('old killed process', json.dumps(evidence))
        after['lines'].append('[4] Out of memory: Killed process 42')
        evidence = soak.kernel_increment(before, after)
        self.assertEqual(evidence['matched_lines'], ['[4] Out of memory: Killed process 42'])
        self.assertNotEqual(evidence['before']['sha256'], evidence['after']['sha256'])

    def test_kernel_rollover_clear_changed_prefix_and_boot_change_fail_closed(self):
        before = {'boot_id': 'one', 'lines': ['[1] a', '[2] b']}
        for after in ({'boot_id': 'two', 'lines': before['lines']},
                      {'boot_id': 'one', 'lines': []},
                      {'boot_id': 'one', 'lines': ['[2] b', '[3] c']},
                      {'boot_id': 'one', 'lines': ['[1] changed', '[2] b']}):
            with self.subTest(after=after), self.assertRaises(soak.pressure.Inconclusive):
                soak.kernel_increment(before, after)

    def test_immutable_id_validation(self):
        identity = 'sha256:' + 'a' * 64
        self.assertEqual(soak.image_identity({'Id': identity}), identity)
        for value in ({}, {'Id': 'latest'}, {'Id': identity + 'bad'}):
            with self.assertRaises(ValueError):
                soak.image_identity(value)

    def test_required_kernel_and_cgroup_counters(self):
        point = self.point()
        soak.validate_sample(point, 2 << 30)
        mutations = [('memory.max', 4 << 30), ('memory.swap.max', 10),
            ('exec.cgroup', 'other'), ('memory.events', {'oom': 0}),
            ('pid1_memory_kib', {'VmRSS': 20}), ('memory.peak', None)]
        for key, value in mutations:
            with self.subTest(key=key), self.assertRaises(soak.pressure.Inconclusive):
                soak.validate_sample({**point, key: value}, 2 << 30)

    def test_qualify_requires_duration_kernel_audit_cleanup_and_progress(self):
        receipt = self.receipt()
        self.assertTrue(soak.qualification(receipt))
        mutations = [('duration_seconds', 30), ('elapsed_seconds', 1799),
            ('writers', 7), ('readers', 3), ('workers', []), ('samples', []),
            ('result', 'FAIL'), ('audit', {'status': 'failed'}),
            ('kernel_log', {'available': False}),
            ('kernel_log', {'available': True, 'matched_lines': ['oom']}),
            ('owned_resources_cleaned', False), ('host_before', {}),
            ('host_after', {'power_evidence': {'eligible': False}})]
        for key, value in mutations:
            with self.subTest(key=key):
                self.assertFalse(soak.qualification({**receipt, key: value}))
        receipt = copy.deepcopy(receipt)
        receipt['workers'][0]['errors'] = [{'code': 'MemoryPressure'}]
        self.assertFalse(soak.qualification(receipt))
        receipt['workers'][0] = {'operations': 0, 'errors': []}
        self.assertFalse(soak.qualification(receipt))

    def test_owned_server_must_keep_image_and_not_restart(self):
        state = {'Image': 'id', 'RestartCount': 0, 'State': {'Running': True, 'OOMKilled': False}}
        soak.validate_state(state, 'id')
        for value in (None, {**state, 'Image': 'other'}, {**state, 'RestartCount': 1},
                      {**state, 'State': {'Running': False}},
                      {**state, 'State': {'Running': True, 'OOMKilled': True}}):
            with self.assertRaises(AssertionError):
                soak.validate_state(value, 'id')

    def test_writer_keys_are_bounded_disjoint_and_wrap(self):
        rows = soak.rows_for(1, 2, 3, 7, 'payload')
        self.assertEqual([r['id'] for r in rows],
            ['soak-1-00000006', 'soak-1-00000000', 'soak-1-00000001'])
        self.assertTrue(all(r['payload'] == 'payload' for r in rows))
        keys = {r['id'] for r in rows}
        self.assertFalse(keys & {r['id'] for r in soak.rows_for(2, 2, 3, 7, 'p')})

    def test_workload_error_stops_all_workers_without_recording_secret(self):
        driver = MagicMock()
        session = driver.session.return_value.__enter__.return_value
        failure = RuntimeError('secret-token')
        failure.code = 'Neo.TransientError.General.MemoryPressure'
        session.begin_transaction.side_effect = failure
        args = argparse.Namespace(payload_bytes=1, batch=1, rows_per_writer=1)
        result = {'operations': 0, 'max_operation_seconds': 0, 'errors': []}
        stop, ready, go = threading.Event(), threading.Event(), threading.Event()
        go.set()
        soak.worker(driver, 'writer', 0, args, stop, ready, go, result)
        self.assertTrue(stop.is_set())
        self.assertEqual(result['operations'], 0)
        self.assertEqual(result['errors'][0]['code'], failure.code)
        self.assertNotIn('secret-token', json.dumps(result))

    def test_worker_joins_share_one_deadline(self):
        threads = [MagicMock(), MagicMock()]
        with patch.object(soak.time, 'monotonic', side_effect=[0, 1, 4]):
            soak.join_threads(threads, 3)
        threads[0].join.assert_called_once_with(2)
        threads[1].join.assert_called_once_with(0)

    def test_exception_after_pass_still_fails_closed(self):
        def failed(args, report):
            report.update(self.receipt())
            raise RuntimeError('unexpected finalization failure')
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'receipt.json'
            with patch.object(sys, 'argv', ['soak', '--image', 'local', '--output', str(output)]), \
                 patch.object(soak, 'run', side_effect=failed), contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(soak.main(), 1)
            receipt = json.loads(output.read_text())
            self.assertEqual(receipt['result'], 'FAIL')
            self.assertFalse(receipt['qualified'])

    def test_cli_inconclusive_never_claims_qualified(self):
        def failed(args, report):
            raise soak.pressure.Inconclusive('isolated host required')
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'receipt.json'
            with patch.object(sys, 'argv', ['soak', '--image', 'local', '--output', str(output)]), \
                 patch.object(soak, 'run', side_effect=failed), contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(soak.main(), 1)
            report = json.loads(output.read_text())
        self.assertEqual(report['result'], 'INCONCLUSIVE')
        self.assertFalse(report['qualified'])

    def test_cleanup_failure_cannot_be_downgraded_to_inconclusive(self):
        def failed(args, report):
            report['cleanup_errors'] = [{'error': 'owned resource remains'}]
            raise soak.pressure.Inconclusive('kernel unavailable')
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'receipt.json'
            with patch.object(sys, 'argv', ['soak', '--image', 'local', '--output', str(output)]), \
                 patch.object(soak, 'run', side_effect=failed), contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(soak.main(), 1)
            self.assertEqual(json.loads(output.read_text())['result'], 'FAIL')


if __name__ == '__main__':
    unittest.main()
