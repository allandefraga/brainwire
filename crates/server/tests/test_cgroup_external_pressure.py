"""Pure checks for the 4 GiB pressure harness; starts no Docker resources."""
import unittest
from unittest.mock import patch

import cgroup_external_pressure as pressure


class PressureHarnessTests(unittest.TestCase):
    def test_tmpfs_working_set_retains_shmem_and_clamps_inactive_file(self):
        self.assertEqual(pressure.working_set(100, {'inactive_file': 30, 'shmem': 50}), 70)
        self.assertEqual(pressure.working_set(20, {'inactive_file': 30}), 0)
        self.assertEqual(pressure.working_set(100, {}), 100)

    def test_fill_steps_respect_raw_usage_safety_bound(self):
        self.assertEqual(pressure.next_step(0, 0), pressure.STEP)
        target = (pressure.LIMIT * pressure.TARGET_PERCENT + 99) // 100
        self.assertEqual(pressure.next_step(target, target), 0)
        with self.assertRaises(pressure.Inconclusive):
            pressure.next_step(pressure.LIMIT, 0)
        # A large inactive file cache can make raw usage unsafe even when the
        # estimated working set is small. Refuse injection instead of forcing OOM.
        with self.assertRaises(pressure.Inconclusive):
            pressure.next_step(pressure.LIMIT * 97 // 100, 100)

    def test_oom_evidence_uses_counter_deltas_and_multiple_sources(self):
        baseline = {'memory.events': {'oom': 1, 'oom_kill': 0}}
        self.assertFalse(pressure.oom_observed({'before': baseline,
            'after_relief': {'memory.events': {'oom': 1, 'oom_kill': 0}}}))
        self.assertTrue(pressure.oom_observed({'before': baseline,
            'cleanup_sample': {'memory.events': {'oom': 2, 'oom_kill': 1}}}))
        self.assertTrue(pressure.oom_observed({'cleanup_state': {'OOMKilled': True}}))
        self.assertTrue(pressure.oom_observed({'docker_events': [{'action': 'oom'}]}))

    def test_final_power_error_preserves_primary_failure(self):
        for error in (pressure.Inconclusive('AC power lost'), OSError('pmset missing')):
            failed = {'result': 'FAIL', 'error': 'primary assertion'}
            with patch.object(pressure, 'power_receipt', side_effect=error):
                pressure.record_final_power(failed)
            self.assertEqual(failed['result'], 'FAIL')
            self.assertEqual(failed['error'], 'primary assertion')
            self.assertEqual(failed['host_after']['measurement_error_type'], type(error).__name__)
        passed = {'result': 'PASS'}
        with patch.object(pressure, 'power_receipt', side_effect=pressure.Inconclusive('AC lost')):
            pressure.record_final_power(passed)
        self.assertEqual(passed['result'], 'INCONCLUSIVE')

    def test_ownership_check_rejects_unlabelled_resources(self):
        result = type('Result', (), {'returncode': 0, 'stdout': '[{"Labels": {}}]'})()
        with patch.object(pressure, 'command', return_value=result):
            with self.assertRaisesRegex(RuntimeError, 'unowned'):
                pressure.inspect_owned('volume', 'example', 'expected')

    def test_required_cgroup_conditions_reject_swap_and_mismatch(self):
        value = {'memory.max': pressure.LIMIT, 'memory.swap.max': 0,
                 'pid1.cgroup': '0::/', 'exec.cgroup': '0::/',
                 'memory.events': {'oom': 0, 'oom_kill': 0},
                 'memory.stat': {'anon': 1, 'inactive_file': 2, 'shmem': 3}}
        pressure.require_finite(value)
        for key, changed in (('memory.swap.max', 1), ('memory.max', pressure.LIMIT // 2),
                             ('exec.cgroup', '0::/another')):
            invalid = dict(value, **{key: changed})
            with self.assertRaises(pressure.Inconclusive):
                pressure.require_finite(invalid)


if __name__ == '__main__':
    unittest.main()
