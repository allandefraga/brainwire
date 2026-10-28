"""Small safety/correctness tests; never start Docker or load a database."""
import json
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from scripts import bench_v08_container as bench


def raw_sample(events='low 0\nhigh 0\nmax 0\noom 0\noom_kill 0'):
    return f'''@@memory.current
12345
@@memory.peak
20000
@@memory.max
{bench.LIMIT}
@@memory.events
{events}
@@memory.stat
anon 10000
file 2345
@@process_status
Name: brainwire
VmRSS: 11 kB
RssAnon: 9 kB
RssFile: 2 kB
'''


class SafetyTests(unittest.TestCase):
    def test_cgroup_and_process_bytes_are_distinct(self):
        sample = bench.parse_sample(raw_sample())
        self.assertEqual(sample['current_bytes'], 12345)
        self.assertEqual(sample['process']['VmRSS_bytes'], 11 * 1024)
        self.assertEqual(sample['process']['RssAnon_bytes'], 9 * 1024)
        self.assertEqual(sample['stat']['anon'], 10000)
        bench.evaluate_memory([sample])

    def test_missing_cgroup_files_fail_closed(self):
        with self.assertRaises(KeyError):
            bench.parse_sample('@@memory.current\n0')

    def test_oom_even_below_cap_fails(self):
        sample = bench.parse_sample(raw_sample('oom 1\noom_kill 0'))
        with self.assertRaisesRegex(AssertionError, 'OOM'):
            bench.evaluate_memory([sample])

    def test_wrong_or_unbounded_cap_fails(self):
        sample = bench.parse_sample(raw_sample().replace(str(bench.LIMIT), 'max'))
        with self.assertRaisesRegex(AssertionError, 'hard cap'):
            bench.evaluate_memory([sample])
        with self.assertRaises(AssertionError):
            bench.evaluate_memory([])

    def test_all_batches_are_disjoint_complete_and_repeatable(self):
        for rows, batch_size, workers in [(100, 7, 8), (100, 250, 8), (1001, 17, 3)]:
            values = [row for worker in range(workers) for batch in bench.batches(rows, batch_size, worker, workers) for row in batch]
            self.assertEqual(sorted(row['sequence'] for row in values), list(range(1, rows)))
            self.assertEqual(len({row['id'] for row in values}), rows - 1)
            self.assertEqual(len(values[0]['payload']), 512)
            self.assertEqual(list(bench.batches(rows, batch_size, 0, workers)), list(bench.batches(rows, batch_size, 0, workers)))

    def test_cleanup_refuses_resource_without_exact_ownership(self):
        with patch.object(bench, 'docker', return_value=json.dumps([{'Config': {'Labels': {bench.LABEL: 'someone-else'}}}])) as command:
            with self.assertRaisesRegex(RuntimeError, 'ownership mismatch'):
                bench.owned_cleanup('bench-test', 'volume-test', 'mine')
            self.assertEqual(command.call_count, 1)

    def test_cleanup_removes_only_exact_owned_resources(self):
        calls = []
        def docker(*args):
            calls.append(args)
            if args[1] == 'inspect':
                return json.dumps([{'Config': {'Labels': {bench.LABEL: 'mine'}}, 'Labels': {bench.LABEL: 'mine'}}])
            return ''
        with patch.object(bench, 'docker', side_effect=docker):
            bench.owned_cleanup('bench-test', 'volume-test', 'mine')
        self.assertEqual([c for c in calls if c[1] == 'rm'], [
            ('container', 'rm', '-f', 'bench-test-offline'), ('container', 'rm', '-f', 'bench-test'),
            ('volume', 'rm', 'volume-test')])

    def test_idle_sampling_preserves_original_process_observation(self):
        seen = []
        with patch.object(bench.time, 'monotonic', side_effect=[0, 0, 0, 1]), patch.object(bench.time, 'sleep') as sleep:
            bench.idle_samples(seen.append, 'post-load-idle', 1, 1)
        self.assertEqual(seen, ['post-load-idle', 'post-load-idle-end'])
        sleep.assert_called_once_with(1)

    def test_restart_observes_idle_before_kill_and_reconnects_before_readiness(self):
        events = []
        def idle(sample, phase, seconds, interval):
            sample(phase)
            sample(phase + '-end')
        def docker(*args):
            events.append(args[0])
            return json.dumps([{'State': {'OOMKilled': False}}]) if args[0] == 'inspect' else ''
        with patch.object(bench, 'idle_samples', side_effect=idle), patch.object(bench, 'docker', side_effect=docker):
            bench.restart_after_idle('test', events.append, lambda: events.append('fresh-driver'),
                                     lambda: events.append('ready'), 1, 1, lambda value: None)
        self.assertEqual(events, ['post-load-idle', 'post-load-idle-end', 'kill', 'inspect', 'start',
                                  'fresh-driver', 'ready', 'recovered'])

    def test_reconnect_refreshes_dynamic_port_and_closes_previous_pool(self):
        from unittest.mock import Mock
        old = Mock()
        factory = Mock(return_value='fresh')
        inspect = [{'NetworkSettings': {'Ports': {'7687/tcp': [{'HostPort': '40002'}]}}}]
        with patch.object(bench, 'docker', return_value=json.dumps(inspect)):
            driver, uri = bench.reconnect_driver(factory, 'test', 'secret', 12, previous=old)
        old.close.assert_called_once_with()
        self.assertEqual(driver, 'fresh')
        self.assertEqual(uri, 'bolt://127.0.0.1:40002')
        self.assertEqual(factory.call_args.args, (uri,))
        self.assertEqual(factory.call_args.kwargs['max_connection_pool_size'], 14)

    def test_cursor_uses_ordinary_auth_without_delegated_metadata(self):
        class Result:
            def __init__(self, rows):
                self.rows = rows
            def data(self):
                return self.rows
        class Transaction:
            def __enter__(self):
                return self
            def __exit__(self, *args):
                pass
            def run(self, query, **params):
                after = params['after']
                values = [bench.identifier(i) for i in range(3)]
                return Result([{'id': value} for value in values if value > after][:int(query.rsplit(' ', 1)[1])])
            def commit(self):
                pass
        class OrdinarySession:
            def __init__(self):
                self.calls = []
            def begin_transaction(self, **options):
                self.calls.append(options)
                if 'metadata' in options:
                    raise AssertionError('ordinary principal cannot delegate visibility')
                return Transaction()
        session = OrdinarySession()
        result = bench.walk_cursor(session, 3, 7, 30, page_size=2)
        self.assertTrue(result['complete'])
        self.assertEqual(result['rows_seen'], 3)
        self.assertEqual(len(result['pages']), 3)
        self.assertEqual(session.calls, [{'timeout': 7}] * 3)
        default_result = bench.walk_cursor(OrdinarySession(), 3, 7, 30)
        self.assertEqual(default_result['page_size'], 1000)
        self.assertEqual(len(default_result['pages']), 2)

    def test_query_timing_includes_materialized_execute_and_preserves_parameters(self):
        from unittest.mock import Mock
        execute = Mock(return_value=[{'count': 42}])
        session = object()
        with patch.object(bench.time, 'perf_counter', side_effect=[5, 5.125]):
            rows, seconds = bench.measure_query(execute, session, 'query', id='n1')
        execute.assert_called_once_with(session, 'query', id='n1')
        self.assertEqual(rows, [{'count': 42}])
        self.assertEqual(seconds, .125)

    def test_histogram_empty_and_ranked(self):
        self.assertEqual(bench.percentile({}), {'requests': 0})
        self.assertEqual(bench.percentile({1000: 99, 9000: 1})['p99_ms'], 1)


if __name__ == '__main__':
    unittest.main()
