import unittest

import brainwire
from brainwire import GraphClient


MEMORY_CODE = 'Neo.TransientError.General.MemoryPressure'
UNKNOWN_CODE = 'Neo.ClientError.Transaction.CommitOutcomeUnknown'
HINT = (' [brainwire.memory reason=query_pool requested_bytes=4096 '
        'available_bytes=1024 rolled_back=true]')


class ServerFailure(RuntimeError):
    def __init__(self, code, message):
        super().__init__(message)
        self.code, self.message = code, message


class Driver:
    def __init__(self, failure=None, phase='run', rows=None):
        self.failure, self.phase = failure, phase
        self.rows = [] if rows is None else rows
        self.sessions = 0
        self.commits = 0
        self.calls = []

    def session(self, **options):
        self.sessions += 1
        self.calls.append(('session', options))
        return self

    def __enter__(self):
        return self

    def __exit__(self, *args):
        return False

    def begin_transaction(self, **options):
        self.calls.append(('begin', options))
        if self.phase == 'begin' and self.failure:
            raise self.failure
        return self

    def commit(self):
        self.commits += 1
        if self.phase == 'commit' and self.failure:
            raise self.failure

    def run(self, query, parameters=None):
        self.calls.append(('run', query, parameters))
        if self.phase == 'run' and self.failure:
            raise self.failure
        return self

    def data(self):
        return self.rows


class ErrorContractTests(unittest.TestCase):
    def test_unavailable_process_monitor_is_recognized_as_rolled_back(self):
        hint = ('monitor unavailable [brainwire.memory reason=process_monitor '
                'requested_bytes=0 available_bytes=0 rolled_back=true]')
        with self.assertRaises(brainwire.MemoryPressureError) as caught:
            GraphClient(Driver(ServerFailure(MEMORY_CODE, hint)), 'demo').run_cypher('RETURN 1')
        error = caught.exception
        self.assertEqual((error.reason, error.requested_bytes, error.available_bytes,
                          error.rolled_back), ('process_monitor', 0, 0, True))

    def test_memory_error_preserves_verified_hint_and_cause(self):
        failure = ServerFailure(MEMORY_CODE, 'pool full' + HINT)
        driver = Driver(failure)
        with self.assertRaises(brainwire.MemoryPressureError) as caught:
            GraphClient(driver, 'demo').run_cypher('RETURN 1')
        error = caught.exception
        self.assertIs(error.__cause__, failure)
        self.assertIs(error.original_error, failure)
        self.assertEqual(error.code, MEMORY_CODE)
        self.assertEqual((error.reason, error.requested_bytes, error.available_bytes,
                          error.rolled_back), ('query_pool', 4096, 1024, True))
        self.assertEqual(driver.sessions, 1)

    def test_known_failures_are_translated_at_begin_and_checkpoint(self):
        for phase, operation in (
            ('begin', lambda graph: graph._write(lambda tx: 'not run')),
            ('begin', lambda graph: graph.write_transaction(lambda tx: 'not run', request_id='event-1')),
            ('run', lambda graph: graph.checkpoint('snapshot')),
            ('run', lambda graph: graph.request_status('event-1')),
        ):
            with self.subTest(phase=phase, operation=operation):
                failure = ServerFailure(MEMORY_CODE, 'busy' + HINT)
                driver = Driver(failure, phase)
                with self.assertRaises(brainwire.MemoryPressureError) as caught:
                    operation(GraphClient(driver, 'demo'))
                self.assertIs(caught.exception.__cause__, failure)
                self.assertEqual(driver.sessions, 1)
                self.assertEqual(driver.commits, 0)

    def test_malformed_hint_never_authorizes_retry(self):
        for message in ['rolled_back=true', 'pool full' + HINT + ' ignored suffix',
                        HINT.replace('4096', '-1'), HINT.replace('4096', str(1 << 64)),
                        HINT.replace('query_pool', 'unrecognized'),
                        HINT.replace('rolled_back=true', 'rolled_back=false')]:
            with self.subTest(message=message):
                with self.assertRaises(brainwire.MemoryPressureError) as caught:
                    GraphClient(Driver(ServerFailure(MEMORY_CODE, message)), 'demo').run_cypher('RETURN 1')
                error = caught.exception
                self.assertIsNone(error.rolled_back)
                self.assertIsNone(error.reason)
                self.assertIsNone(error.requested_bytes)
                self.assertIsNone(error.available_bytes)

    def test_generic_errors_and_message_only_matches_preserve_identity(self):
        for failure in [RuntimeError('CommitOutcomeUnknown'),
                        ServerFailure('Neo.ClientError.Statement.SyntaxError', HINT),
                        ServerFailure('Neo.TransientError.General.Other', 'MemoryPressure'),
                        ServerFailure([], 'not a Bolt code')]:
            with self.subTest(failure=failure):
                with self.assertRaises(type(failure)) as caught:
                    GraphClient(Driver(failure), 'demo').run_cypher('RETURN 1')
                self.assertIs(caught.exception, failure)

    def test_commit_unknown_is_never_replayed_and_preserves_cause(self):
        failure = ServerFailure(UNKNOWN_CODE, 'storage result unknown')
        for identified in (False, True):
            with self.subTest(identified=identified):
                driver, callbacks = Driver(failure, 'commit'), []
                graph = GraphClient(driver, 'demo')
                with self.assertRaises(brainwire.CommitOutcomeUnknownError) as caught:
                    operation = lambda tx: callbacks.append(tx) or 'unacknowledged'
                    if identified:
                        graph.write_transaction(operation, request_id='event-1')
                    else:
                        graph._write(operation)
                self.assertEqual(len(callbacks), 1)
                self.assertEqual(driver.commits, 1)
                self.assertEqual(driver.sessions, 1)
                self.assertIs(caught.exception.__cause__, failure)
                self.assertIsNone(caught.exception.rolled_back)

    def test_request_status_is_fresh_parameterized_and_not_a_retry_decision(self):
        row = {'status': 'not_recorded', 'requestId': 'event-1',
               'incarnation': '91694c36-9576-4849-af92-88cdfa7cfba3', 'sequence': None}
        driver = Driver(rows=[row])
        result = GraphClient(driver, 'demo').request_status('event-1')
        self.assertEqual(result, row)
        self.assertEqual(driver.calls, [('session', {'database': 'demo'}),
            ('run', 'CALL db.requestStatus($seed)', {'seed': 'event-1'})])
        self.assertEqual(driver.commits, 0)
        self.assertNotIn('can_retry', result)

    def test_committed_status_is_validated_and_extra_fields_are_not_retry_advice(self):
        row = {'status': 'committed', 'requestId': 'event-1',
               'incarnation': '91694c36-9576-4849-af92-88cdfa7cfba3',
               'sequence': str((1 << 64) - 1), 'can_retry': True}
        result = GraphClient(Driver(rows=[row]), 'demo').request_status('event-1')
        self.assertEqual(result['status'], 'committed')
        self.assertEqual(result['sequence'], str((1 << 64) - 1))
        self.assertNotIn('can_retry', result)
        with self.assertRaises(ValueError):
            GraphClient(Driver(rows=[{**row, 'sequence': str(1 << 64)}]), 'demo').request_status('event-1')

    def test_request_status_rejects_invalid_input_before_io_and_malformed_output(self):
        driver = Driver()
        with self.assertRaises(ValueError):
            GraphClient(driver, 'demo').request_status('invalid seed')
        self.assertEqual(driver.sessions, 0)
        base = {'status': 'committed', 'requestId': 'event-1',
                'incarnation': '91694c36-9576-4849-af92-88cdfa7cfba3', 'sequence': '12'}
        invalid = [[], [base, base], [{**base, 'requestId': 'other'}],
                   [{key: value for key, value in base.items() if key != 'sequence'}],
                   [{**base, 'sequence': None}], [{**base, 'sequence': '-1'}],
                   [{**base, 'incarnation': ''}], [{**base, 'status': 'unknown'}],
                   [{**base, 'status': 'not_recorded'}]]
        for rows in invalid:
            with self.subTest(rows=rows), self.assertRaises(ValueError):
                GraphClient(Driver(rows=rows), 'demo').request_status('event-1')


if __name__ == '__main__':
    unittest.main()
