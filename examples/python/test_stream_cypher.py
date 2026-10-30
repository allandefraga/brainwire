import unittest

from brainwire import GraphClient, MemoryPressureError


MEMORY_CODE = 'Neo.TransientError.General.MemoryPressure'
HINT = (' [brainwire.memory reason=query_pool requested_bytes=4096 '
        'available_bytes=1024 rolled_back=true]')


class Failure(RuntimeError):
    def __init__(self, code=MEMORY_CODE):
        super().__init__('server failure' + HINT)
        self.code, self.message = code, str(self)


class Record:
    def __init__(self, number):
        self.number = number

    def data(self):
        return {'number': self.number}


class Driver:
    def __init__(self, count=1000000, *, failure=None, phase=None):
        self.count, self.failure, self.phase = count, failure, phase
        self.sessions = self.reads = self.consumes = self.closes = 0
        self.options = self.query = self.parameters = None

    def session(self, **options):
        self.sessions += 1
        self.options = options
        return self

    def run(self, query, parameters):
        self.query, self.parameters = query, parameters
        if self.phase == 'run':
            raise self.failure
        return self

    def __iter__(self):
        return self

    def __next__(self):
        if self.phase == 'next' and self.reads == 1:
            raise self.failure
        if self.reads >= self.count:
            raise StopIteration
        number = self.reads
        self.reads += 1
        return Record(number)

    def consume(self):
        self.consumes += 1
        if self.phase == 'consume':
            raise self.failure

    def close(self):
        self.closes += 1
        if self.phase == 'close':
            raise self.failure


class StreamCypherTests(unittest.TestCase):
    def test_stream_does_not_prefetch_and_closes_after_early_exit(self):
        driver = Driver()
        with GraphClient(driver, 'demo').stream_cypher('MATCH (n) RETURN n', {'key': 3}, fetch_size=17) as rows:
            self.assertEqual(driver.reads, 0)
            self.assertEqual(next(rows), {'number': 0})
            self.assertEqual(driver.reads, 1)
            self.assertEqual(driver.closes, 0)
        self.assertEqual(driver.options, {'database': 'demo', 'fetch_size': 17, 'default_access_mode': 'READ'})
        self.assertEqual(driver.parameters, {'key': 3})
        self.assertEqual((driver.reads, driver.consumes, driver.closes), (1, 1, 1))
        with self.assertRaises(StopIteration):
            next(rows)

    def test_complete_iteration_returns_dicts_and_consumes_terminal_summary(self):
        driver = Driver(count=3)
        with GraphClient(driver, 'demo').stream_cypher('RETURN 1') as rows:
            self.assertEqual(list(rows), [{'number': 0}, {'number': 1}, {'number': 2}])
        self.assertEqual((driver.consumes, driver.closes), (1, 1))

    def test_failures_at_run_next_consume_and_close_are_typed_and_never_retried(self):
        for phase in ('run', 'next', 'consume', 'close'):
            with self.subTest(phase=phase):
                failure = Failure()
                driver = Driver(count=3, failure=failure, phase=phase)
                with self.assertRaises(MemoryPressureError) as caught:
                    with GraphClient(driver, 'demo').stream_cypher('RETURN 1') as rows:
                        list(rows)
                self.assertIs(caught.exception.__cause__, failure)
                self.assertEqual((driver.sessions, driver.closes), (1, 1))
                self.assertTrue(caught.exception.rolled_back)

    def test_body_exception_is_preserved_even_if_close_fails(self):
        body_error = Failure()
        driver = Driver(failure=Failure(), phase='close')
        with self.assertRaises(Failure) as caught:
            with GraphClient(driver, 'demo').stream_cypher('RETURN 1') as rows:
                next(rows)
                raise body_error
        self.assertIs(caught.exception, body_error)
        self.assertEqual((driver.consumes, driver.closes), (0, 1))
        with self.assertRaises(StopIteration):
            next(rows)

    def test_invalid_inputs_never_open_a_session(self):
        for query, parameters, fetch_size in [('', None, 200), (' ', {}, 200),
                (None, {}, 200), ('RETURN 1', [], 200), ('RETURN 1', {}, 0),
                ('RETURN 1', {}, -1), ('RETURN 1', {}, True), ('RETURN 1', {}, 1.5)]:
            with self.subTest(query=query, parameters=parameters, fetch_size=fetch_size):
                driver = Driver()
                with self.assertRaises(ValueError):
                    with GraphClient(driver, 'demo').stream_cypher(query, parameters, fetch_size=fetch_size):
                        pass
                self.assertEqual(driver.sessions, 0)


if __name__ == '__main__':
    unittest.main()
