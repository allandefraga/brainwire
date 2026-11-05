"""Identified batches must never split one request ID across commits."""
import unittest

from brainwire import GraphClient
from examples.python.test_client import RetryingDriver, RetryableCommitDisconnect


class IdentifiedBatchTests(unittest.TestCase):
    def test_nodes_commit_once_with_seed_and_report_progress_after_ack(self):
        driver = RetryingDriver()
        progress = []
        graph = GraphClient(driver, 'demo', batch_size=2)
        count = graph.load_nodes('Item', [{'entity_id': 'a'}, {'entity_id': 'b', 'value': 2}],
                                 request_id='load-42',
                                 on_batch=lambda batch, total: progress.append((batch, total, driver.applied)))
        self.assertEqual(count, 2)
        self.assertEqual(driver.applied, 1)
        self.assertEqual(progress, [(1, 2, 1)])
        self.assertIn(('begin', {'brainwire_request_id': 'load-42'}), driver.events)
        self.assertEqual(sum(isinstance(e, tuple) and e[0] == 'run' for e in driver.events), 2)
        self.assertNotIn('managed', driver.events)

    def test_progress_callback_error_occurs_after_commit_without_replay(self):
        for edges in (False, True):
            driver = RetryingDriver()
            graph = GraphClient(driver, 'demo')
            def failed_callback(*_):
                raise RuntimeError('progress failed')
            with self.subTest(edges=edges), self.assertRaisesRegex(RuntimeError, 'progress failed'):
                if edges:
                    graph.load_edges('Item', 'NEXT', 'Item', [{'source_id': 'a', 'target_id': 'b'}],
                                     request_id='batch-1', on_batch=failed_callback)
                else:
                    graph.load_nodes('Item', [{'entity_id': 'a'}], request_id='batch-1',
                                     on_batch=failed_callback)
            self.assertEqual(driver.applied, 1)
            self.assertEqual(sum(isinstance(e, tuple) and e[0] == 'begin' for e in driver.events), 1)

    def test_edges_use_one_identified_transaction_without_catalog_calls(self):
        driver = RetryingDriver()
        graph = GraphClient(driver, 'demo', batch_size=2)
        self.assertEqual(graph.load_edges('Item', 'NEXT', 'Item', [
            {'source_id': 'a', 'target_id': 'b', 'revision': '1'},
            {'source_id': 'a', 'target_id': 'b', 'revision': '2', 'value': 3},
        ], request_id='edges-42'), 2)
        self.assertEqual(driver.applied, 1)
        queries = [e[1] for e in driver.events if isinstance(e, tuple) and e[0] == 'run']
        self.assertEqual(len(queries), 2)
        self.assertTrue(all('MERGE' in q and 'CALL' not in q for q in queries))

    def test_row_and_byte_overflow_refuse_before_opening_session(self):
        cases = [
            ('nodes', [{'entity_id': 'a'}, {'entity_id': 'b'}, {'entity_id': 'c'}], 2, 1000),
            ('nodes', [{'entity_id': 'a', 'value': 'x' * 50}, {'entity_id': 'b', 'value': 'x' * 50}], 10, 150),
            ('edges', [{'source_id': str(i), 'target_id': 'b'} for i in range(3)], 2, 1000),
            ('edges', [{'source_id': 'a', 'target_id': 'b', 'value': 'x' * 50}] * 2, 10, 170),
        ]
        for kind, rows, size, byte_limit in cases:
            driver = RetryingDriver()
            graph = GraphClient(driver, 'demo', batch_size=size, batch_bytes=byte_limit)
            with self.subTest(kind=kind, size=size, byte_limit=byte_limit), self.assertRaises(ValueError):
                if kind == 'nodes':
                    graph.load_nodes('Item', rows, request_id='one-batch')
                else:
                    graph.load_edges('Item', 'NEXT', 'Item', rows, request_id='one-batch')
            self.assertEqual(driver.events, [])

    def test_generator_failure_after_exact_limit_does_not_commit(self):
        for edges in (False, True):
            driver = RetryingDriver()
            graph = GraphClient(driver, 'demo', batch_size=1)
            def source():
                yield {'source_id': 'a', 'target_id': 'b'} if edges else {'entity_id': 'a'}
                raise RuntimeError('source interrupted')
            with self.subTest(edges=edges), self.assertRaisesRegex(RuntimeError, 'source interrupted'):
                if edges:
                    graph.load_edges('Item', 'NEXT', 'Item', source(), request_id='once')
                else:
                    graph.load_nodes('Item', source(), request_id='once')
            self.assertEqual(driver.events, [])

    def test_invalid_later_row_does_not_commit_prefix(self):
        for edges in (False, True):
            driver = RetryingDriver()
            graph = GraphClient(driver, 'demo')
            with self.subTest(edges=edges), self.assertRaises(ValueError):
                if edges:
                    graph.load_edges('Item', 'NEXT', 'Item', [{'source_id': 'a', 'target_id': 'b'}, {}], request_id='once')
                else:
                    graph.load_nodes('Item', [{'entity_id': 'a'}, {}], request_id='once')
            self.assertEqual(driver.events, [])

    def test_lost_commit_reply_does_not_replay_or_report_success(self):
        for edges in (False, True):
            driver = RetryingDriver(lose_commit_reply=True)
            graph = GraphClient(driver, 'demo')
            progress = []
            with self.subTest(edges=edges), self.assertRaises(RetryableCommitDisconnect):
                if edges:
                    graph.load_edges('Item', 'NEXT', 'Item', [{'source_id': 'a', 'target_id': 'b'}],
                                     request_id='once', on_batch=lambda *args: progress.append(args))
                else:
                    graph.load_nodes('Item', [{'entity_id': 'a'}], request_id='once',
                                     on_batch=lambda *args: progress.append(args))
            self.assertEqual(driver.applied, 1)
            self.assertEqual(progress, [])
            self.assertEqual(sum(isinstance(e, tuple) and e[0] == 'begin' for e in driver.events), 1)
            self.assertFalse(any(isinstance(e, tuple) and e[0] == 'run' and 'requestStatus' in e[1] for e in driver.events))

    def test_invalid_seed_and_unsupported_options_fail_before_io(self):
        driver = RetryingDriver()
        graph = GraphClient(driver, 'demo')
        for operation in (
            lambda: graph.load_nodes('Item', [], request_id='invalid seed'),
            lambda: graph.load_edges('Item', 'NEXT', 'Item', [], request_id='invalid seed'),
            lambda: graph.load_nodes('Item', [], request_id='once', with_acl=True),
            lambda: graph.load_edges('Item', 'NEXT', 'Item', [], request_id='once', require_endpoints=False),
        ):
            with self.assertRaises(ValueError):
                operation()
        self.assertEqual(driver.events, [])

    def test_unbounded_source_stops_at_first_overflow_without_writing(self):
        for edges in (False, True):
            driver = RetryingDriver()
            graph = GraphClient(driver, 'demo', batch_size=2)
            consumed = []
            def source():
                for i in range(1000000):
                    consumed.append(i)
                    yield ({'source_id': str(i), 'target_id': 'b'} if edges
                           else {'entity_id': str(i)})
            with self.subTest(edges=edges), self.assertRaisesRegex(ValueError, 'one bounded batch'):
                if edges:
                    graph.load_edges('Item', 'NEXT', 'Item', source(), request_id='once')
                else:
                    graph.load_nodes('Item', source(), request_id='once')
            self.assertEqual(consumed, [0, 1, 2])
            self.assertEqual(driver.events, [])

    def test_missing_endpoint_rolls_back_identified_edge_batch(self):
        class MissingEndpointDriver(RetryingDriver):
            class Tx(RetryingDriver.Tx):
                def run(self, query, **params):
                    result = super().run(query, **params)
                    result.count = 0
                    return result
        driver = MissingEndpointDriver()
        progress = []
        with self.assertRaisesRegex(ValueError, 'both endpoints'):
            GraphClient(driver, 'demo').load_edges('Item', 'NEXT', 'Item', [
                {'source_id': 'a', 'target_id': 'missing'}], request_id='once',
                on_batch=lambda *args: progress.append(args))
        self.assertEqual(driver.applied, 0)
        self.assertEqual(progress, [])
        self.assertEqual(driver.events.count('rollback'), 1)

    def test_empty_identified_input_sends_no_request(self):
        driver = RetryingDriver()
        graph = GraphClient(driver, 'demo')
        self.assertEqual(graph.load_nodes('Item', [], request_id='empty'), 0)
        self.assertEqual(graph.load_edges('Item', 'NEXT', 'Item', [], request_id='empty'), 0)
        self.assertEqual(driver.events, [])


if __name__ == '__main__':
    unittest.main()
