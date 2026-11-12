import unittest
import tracemalloc
import json
import pickle
import random
from contextlib import contextmanager
from examples.python.client import GraphClient, request_key
from brainwire.client import _batch_budget_remaining


class Result:
    def __init__(self, count):
        self.count = count

    def single(self):
        return {"processed": self.count, "deleted": self.count, "updated": self.count, "ambiguous": self.count}


class QueryResult:
    def __init__(self, rows):
        self.rows = rows

    def data(self):
        return self.rows


class ArrowBatch:
    def __init__(self, rows):
        self.rows = rows
        self.converted = False
        self.num_rows = len(rows)
        self.root = self
        self.conversion_sizes = []

    def to_pylist(self):
        self.converted = True
        self.root.conversion_sizes.append(self.num_rows)
        return self.rows

    def slice(self, offset, length):
        batch = ArrowBatch(self.rows[offset:offset + length])
        batch.root = self.root
        return batch


class ArrowReader:
    def __init__(self, *batches):
        self.batches = batches
        self.position = 0

    def read_next_batch(self):
        if self.position == len(self.batches):
            raise StopIteration
        batch = self.batches[self.position]
        self.position += 1
        return batch


class ArrowTable:
    def __init__(self, *batches):
        self.batches = batches

    def to_batches(self, *, max_chunksize=None):
        self.max_chunksize = max_chunksize
        return iter(self.batches)


class Driver:
    def __init__(self, counts=(), relationships=None, schema_error=None):
        self.committed, self.counts = [], iter(counts)
        self.relationships = relationships
        self.schema_error = schema_error
        self.fail_on_write = None
        self.write_count = 0
        self.retry_write = False
        self.managed_calls = 0

    def session(self, *, database):
        self.database = database
        return self

    def __enter__(self):
        return self

    def __exit__(self, *args):
        pass

    def begin_transaction(self):
        self.write_count += 1
        if self.write_count == self.fail_on_write:
            raise RuntimeError("write failed")
        captured = []
        driver = self

        class Tx:
            closed = False

            def __enter__(self):
                return self

            def __exit__(self, *args):
                if not self.closed:
                    self.rollback()
                self.closed = True

            def run(self, query, **params):
                captured.append((query, params))
                if query.startswith("CALL db.schema()"):
                    if driver.schema_error is not None:
                        raise driver.schema_error
                    class SchemaResult:
                        def single(self):
                            return {"relationships": driver.relationships}
                    return SchemaResult()
                return Result(next(driver.counts, len(params.get("rows", [1]))))

            def commit(self):
                driver.committed.append(captured)
                self.closed = True

            def rollback(self):
                captured.clear()
                self.closed = True

        return Tx()

    def execute_write(self, callback):
        self.managed_calls += 1
        self.write_count += 1
        if self.write_count == self.fail_on_write:
            raise RuntimeError("write failed")
        captured = []
        driver = self

        class Tx:
            def run(self, query, **params):
                captured.append((query, params))
                return Result(next(driver.counts, len(params.get("rows", [1]))))

        result = callback(Tx())
        if self.retry_write:
            captured.clear()
            result = callback(Tx())
        self.committed.append(captured)
        return result

    def run(self, query, parameters):
        self.query = (query, parameters)
        return QueryResult([{"id": "c-1", "name": "Ada"}])


class ClientTests(unittest.TestCase):
    def test_oversized_batch_exposes_typed_validation_error(self):
        from brainwire import BatchTooLargeError
        graph = GraphClient(Driver(), 'tickets', batch_bytes=64)
        operations = [
            ('node', lambda: graph.load_nodes('Ticket', [{'entity_id': 'a', 'data': 'x' * 1000}])),
            ('edge', lambda: graph.load_edges('Ticket', 'LINKS', 'Ticket',
                [{'source_id': 'a', 'target_id': 'b', 'data': 'x' * 1000}])),
            ('entity_id', lambda: graph.delete_nodes('Ticket', ['x' * 1000])),
            ('edge', lambda: graph.delete_edges('Ticket', 'LINKS', 'Ticket',
                [{'source_id': 'x' * 1000, 'target_id': 'b'}])),
            ('edge', lambda: graph.delete_keyed_edges('Ticket', 'LINKS', 'Ticket',
                [{'source_id': 'a', 'target_id': 'b', 'key': {'event': 'x' * 1000}}])),
        ]
        for kind, operation in operations:
            with self.subTest(kind=kind):
                with self.assertRaises(BatchTooLargeError) as caught:
                    operation()
                self.assertIsInstance(caught.exception, ValueError)
                self.assertEqual(caught.exception.item_type, kind)
                self.assertEqual(caught.exception.limit_bytes, 64)
                self.assertNotIn('x' * 1000, str(caught.exception))

    def test_exact_encoded_overflow_uses_the_same_typed_error(self):
        from brainwire import BatchTooLargeError
        cases = [
            (4, 'node', lambda graph: graph.load_nodes('Ticket', [{'entity_id': 'a'}])),
            (6, 'edge', lambda graph: graph.load_edges('Ticket', 'LINKS', 'Ticket',
                [{'source_id': 'a', 'target_id': 'b'}])),
            (9, 'edge', lambda graph: graph.delete_keyed_edges('Ticket', 'LINKS', 'Ticket',
                [{'source_id': 'a', 'target_id': 'b', 'key': {'event': 'x'}}])),
        ]
        for bound, kind, operation in cases:
            with self.subTest(kind=kind, bound=bound):
                driver = Driver()
                with self.assertRaises(BatchTooLargeError) as caught:
                    operation(GraphClient(driver, 'tickets', batch_bytes=bound))
                self.assertEqual((caught.exception.item_type, caught.exception.limit_bytes), (kind, bound))
                self.assertEqual(driver.write_count, 0)

    def test_batch_error_survives_process_serialization(self):
        from brainwire import BatchTooLargeError
        original = BatchTooLargeError('node', 1024)
        restored = pickle.loads(pickle.dumps(original))
        self.assertEqual((restored.item_type, restored.limit_bytes, str(restored)),
                         ('node', 1024, str(original)))

    @unittest.skipIf(tracemalloc.is_tracing(), 'requires isolated allocation tracking')
    def test_oversized_batch_values_fail_without_serialization_copies(self):
        large = 'x' * (8 << 20)
        driver = Driver()
        graph = GraphClient(driver, 'tickets', batch_bytes=1024)
        requests = (
            lambda: graph.load_nodes('Ticket', [{'entity_id': 'a', 'data': {'parts': [large]}}]),
            lambda: graph.load_nodes('Ticket', [{'entity_id': 'a', 'acl': {'allow': [large]}}],
                                     with_acl=True),
            lambda: graph.load_edges('Ticket', 'LINKS', 'Ticket',
                                     [{'source_id': 'a', 'target_id': 'b', 'data': [large]}]),
            lambda: graph.delete_nodes('Ticket', [large]),
            lambda: graph.delete_edges('Ticket', 'LINKS', 'Ticket',
                                       [{'source_id': large, 'target_id': 'b'}]),
            lambda: graph.delete_keyed_edges('Ticket', 'LINKS', 'Ticket',
                [{'source_id': 'a', 'target_id': 'b', 'key': {'event': large}}]),
        )
        for request in requests:
            tracemalloc.start()
            try:
                with self.assertRaisesRegex(ValueError, 'batch byte limit'):
                    request()
                peak = tracemalloc.get_traced_memory()[1]
            finally:
                tracemalloc.stop()
            self.assertLess(peak, 1 << 20)
        self.assertEqual(driver.write_count, 0)

    def test_oversized_later_row_preserves_committed_batch(self):
        driver = Driver()
        graph = GraphClient(driver, 'tickets', batch_size=1, batch_bytes=128)
        rows = [{'entity_id': 'first'}, {'entity_id': 'second', 'data': ['x' * 1000]}]
        with self.assertRaisesRegex(ValueError, 'batch byte limit'):
            graph.load_nodes('Ticket', rows)
        self.assertEqual(len(driver.committed), 1)
        self.assertEqual(driver.committed[0][0][1]['rows'][0]['entity_id'], 'first')

    def test_oversized_identified_batch_never_opens_a_transaction(self):
        driver = Driver()
        graph = GraphClient(driver, 'tickets', batch_bytes=128)
        rows = [{'entity_id': 'first'}, {'entity_id': 'second', 'data': ['x' * 1000]}]
        with self.assertRaisesRegex(ValueError, 'batch byte limit'):
            graph.load_nodes('Ticket', rows, request_id='one-batch')
        self.assertEqual(driver.write_count, 0)

    @unittest.skipIf(tracemalloc.is_tracing(), 'requires isolated allocation tracking')
    def test_many_small_values_do_not_materialize_an_oversized_batch(self):
        values = [0] * 600000
        graph = GraphClient(Driver(), 'tickets', batch_bytes=1 << 20)
        tracemalloc.start()
        try:
            with self.assertRaisesRegex(ValueError, 'batch byte limit'):
                graph.load_nodes('Ticket', [{'entity_id': 'a', 'data': values}])
            peak = tracemalloc.get_traced_memory()[1]
        finally:
            tracemalloc.stop()
        self.assertLess(peak, 1 << 20)

    @unittest.skipIf(tracemalloc.is_tracing(), 'requires independent allocation tracking')
    def test_many_properties_are_rejected_before_copying_the_mapping(self):
        properties = {f'p{i}': 0 for i in range(50000)}
        node = {'entity_id': 'a', **properties}
        edge = {'source_id': 'a', 'target_id': 'b', **properties}
        key = {name: 'v' for name in properties}
        deletion = {'source_id': 'a', 'target_id': 'b', 'key': key}
        graph = GraphClient(Driver(), 'tickets', batch_bytes=1024)
        for request in (lambda: graph.load_nodes('Ticket', [node]),
                        lambda: graph.load_edges('Ticket', 'LINKS', 'Ticket', [edge]),
                        lambda: graph.delete_keyed_edges('Ticket', 'LINKS', 'Ticket', [deletion])):
            tracemalloc.start()
            try:
                with self.assertRaisesRegex(ValueError, 'batch byte limit'):
                    request()
                peak = tracemalloc.get_traced_memory()[1]
            finally:
                tracemalloc.stop()
            self.assertLess(peak, 1 << 20)

        for request in (lambda: graph.delete_edges('Ticket', 'LINKS', 'Ticket', [edge]),
                        lambda: graph.delete_keyed_edges('Ticket', 'LINKS', 'Ticket', [edge]),
                        lambda: graph.load_nodes('Ticket', [{'entity_id': 'a', 'acl': properties}],
                                                 with_acl=True)):
            tracemalloc.start()
            try:
                with self.assertRaises(ValueError):
                    request()
                peak = tracemalloc.get_traced_memory()[1]
            finally:
                tracemalloc.stop()
            self.assertLess(peak, 1 << 20)

    @unittest.skipIf(tracemalloc.is_tracing(), 'requires isolated allocation tracking')
    def test_escape_expansion_is_checked_without_large_json_copies(self):
        text = '\x00' * 220000
        graph = GraphClient(Driver(), 'tickets', batch_bytes=1 << 20)
        tracemalloc.start()
        try:
            with self.assertRaisesRegex(ValueError, 'batch byte limit'):
                graph.load_nodes('Ticket', [{'entity_id': 'a', 'text': text}])
            peak = tracemalloc.get_traced_memory()[1]
        finally:
            tracemalloc.stop()
        self.assertLess(peak, 1 << 20)

    @unittest.skipIf(tracemalloc.is_tracing(), 'requires isolated allocation tracking')
    def test_combined_strings_are_checked_before_batch_serialization(self):
        parts = ['x' * 300000] * 4
        graph = GraphClient(Driver(), 'tickets', batch_bytes=1 << 20)
        tracemalloc.start()
        try:
            with self.assertRaisesRegex(ValueError, 'batch byte limit'):
                graph.load_nodes('Ticket', [{'entity_id': 'a', 'data': parts}])
            peak = tracemalloc.get_traced_memory()[1]
        finally:
            tracemalloc.stop()
        self.assertLess(peak, 1 << 20)

    @unittest.skipIf(tracemalloc.is_tracing(), 'requires independent allocation tracking')
    def test_budget_is_shared_across_properties(self):
        text = 'x' * 300000
        graph = GraphClient(Driver(), 'tickets', batch_bytes=1 << 20)
        tracemalloc.start()
        try:
            with self.assertRaisesRegex(ValueError, 'batch byte limit'):
                graph.load_nodes('Ticket', [{'entity_id': 'a', 'one': text, 'two': text,
                                             'three': text, 'four': text}])
            peak = tracemalloc.get_traced_memory()[1]
        finally:
            tracemalloc.stop()
        self.assertLess(peak, 1 << 20)

    @unittest.skipIf(tracemalloc.is_tracing(), 'requires isolated allocation tracking')
    def test_combined_unicode_strings_are_checked_before_serialization(self):
        parts = ['é' * 100000] * 7
        graph = GraphClient(Driver(), 'tickets', batch_bytes=1 << 20)
        tracemalloc.start()
        try:
            with self.assertRaisesRegex(ValueError, 'batch byte limit'):
                graph.load_nodes('Ticket', [{'entity_id': 'a', 'data': parts}])
            peak = tracemalloc.get_traced_memory()[1]
        finally:
            tracemalloc.stop()
        self.assertLess(peak, 1 << 20)

    def test_batch_precheck_never_rejects_values_that_fit(self):
        rng = random.Random(13)
        def value(depth):
            if depth and rng.randrange(3) == 0:
                return [value(depth - 1) for _ in range(rng.randrange(5))]
            if depth and rng.randrange(3) == 0:
                return {str(index): value(depth - 1) for index in range(rng.randrange(5))}
            return rng.choice([None, True, 0, 3.5, '', 'é🙂', '\n'])
        for _ in range(1000):
            item = value(4)
            row = {'entity_id': 'a', 'data': item}
            normalized = {'entity_id': 'a', 'properties': {'data': item}}
            bound = len(json.dumps(normalized, ensure_ascii=False, separators=(',', ':')).encode())
            self.assertEqual(GraphClient(Driver(), 'tickets', batch_bytes=bound)
                             .load_nodes('Ticket', [row]), 1)
            edge = {'source_id': 'a', 'target_id': 'b', 'data': item}
            normalized_edge = {'source_id': 'a', 'target_id': 'b', 'properties': {'data': item}}
            edge_bound = len(json.dumps(normalized_edge, ensure_ascii=False, separators=(',', ':')).encode())
            self.assertEqual(GraphClient(Driver(), 'tickets', batch_bytes=edge_bound)
                             .load_edges('Ticket', 'LINKS', 'Ticket', [edge]), 1)

    def test_batch_precheck_handles_unicode_and_escaping_across_chunks(self):
        for length in (4095, 4096, 4097, 8192, 8193):
            value = ('é\n"' * (length // 3 + 1))[:length]
            size = len(json.dumps(value, ensure_ascii=False, separators=(',', ':')).encode())
            with self.subTest(length=length):
                self.assertGreaterEqual(_batch_budget_remaining(value, size), 0)
                self.assertLess(_batch_budget_remaining(value, size - 1), 0)

    def test_acl_batch_accepts_an_exact_utf8_byte_budget(self):
        row = {'entity_id': 'a', 'data': ['é'], 'acl': {'allow': ['team:é']}}
        normalized = {'entity_id': 'a', 'properties': {'data': ['é']},
                      'acl': {'label': 'Ticket', 'entityId': 'a', 'allow': ['team:é']}}
        bound = len(json.dumps(normalized, ensure_ascii=False, separators=(',', ':')).encode())
        driver = Driver()
        self.assertEqual(GraphClient(driver, 'tickets', batch_bytes=bound)
                         .load_nodes('Ticket', [row], with_acl=True), 1)
        self.assertEqual(len(driver.committed), 1)

    def test_custom_container_lengths_do_not_reject_values_that_fit(self):
        class Text(str):
            def __len__(self):
                return 1000000
        class Items(list):
            def __len__(self):
                return 1000000
        class Fields(dict):
            def __len__(self):
                return 1000000
        for item in (Text('x'), Items([1]), Fields({'x': 1})):
            normalized = {'entity_id': 'a', 'properties': {'data': item}}
            bound = len(json.dumps(normalized, ensure_ascii=False, separators=(',', ':')).encode())
            with self.subTest(item=type(item).__name__):
                graph = GraphClient(Driver(), 'tickets', batch_bytes=bound)
                self.assertEqual(graph.load_nodes('Ticket', [{'entity_id': 'a', 'data': item}]), 1)
        row = Fields({'source_id': 'a', 'target_id': 'b'})
        self.assertEqual(GraphClient(Driver(), 'tickets').delete_edges('Ticket', 'LINKS', 'Ticket', [row]), 1)
        self.assertEqual(GraphClient(Driver(), 'tickets').load_edges('Ticket', 'LINKS', 'Ticket', [row]), 1)
        node = Fields({'entity_id': 'a'})
        self.assertEqual(GraphClient(Driver(), 'tickets').load_nodes('Ticket', [node]), 1)

    def test_run_cypher_uses_named_database_parameters_and_returns_materialized_records(self):
        driver = Driver()
        client = GraphClient(driver, "research")
        rows = client.run_cypher(
            "MATCH (c:Customer {entity_id:$id}) RETURN c.entity_id AS id, c.name AS name",
            {"id": "c-1"},
        )
        self.assertEqual(driver.database, "research")
        self.assertEqual(driver.query, (
            "MATCH (c:Customer {entity_id:$id}) RETURN c.entity_id AS id, c.name AS name",
            {"id": "c-1"},
        ))
        self.assertEqual(rows, [{"id": "c-1", "name": "Ada"}])

    def test_run_cypher_rejects_empty_queries_and_non_mapping_parameters_before_connecting(self):
        driver = Driver()
        client = GraphClient(driver, "research")
        for query, parameters in [("  ", {}), ("RETURN 1", [])]:
            with self.subTest(query=query, parameters=parameters), self.assertRaises(ValueError):
                client.run_cypher(query, parameters)
        self.assertFalse(hasattr(driver, "database"))

    def test_schema_reads_one_catalog_snapshot_and_closes_stream(self):
        expected = {"version": 3, "nodes": [{"label": "Ticket", "properties": ["entity_id"]}],
                    "relationships": [], "indexesReady": True}
        client = GraphClient(Driver(), "research")
        calls, closed = [], []
        @contextmanager
        def stream(query, parameters=None, *, fetch_size):
            calls.append((query, parameters, fetch_size))
            try:
                yield iter([expected])
            finally:
                closed.append(True)
        client.stream_cypher = stream
        self.assertEqual(client.schema(), expected)
        self.assertEqual(calls, [("CALL db.schema() YIELD version, nodes, relationships, indexesReady "
                                "RETURN version, nodes, relationships, indexesReady", None, 1)])
        self.assertEqual(closed, [True])

    def test_schema_rejects_missing_extra_or_malformed_rows_and_closes(self):
        valid = {"version": 1, "nodes": [], "relationships": [], "indexesReady": False}
        invalid = [[], [valid, valid], [{}], [{**valid, "version": True}],
                   [{**valid, "version": -1}], [{**valid, "nodes": {}}],
                   [{**valid, "nodes": ["Ticket"]}], [{**valid, "relationships": [None]}],
                   [{**valid, "indexesReady": "true"}]]
        for rows in invalid:
            with self.subTest(rows=rows):
                client, closed = GraphClient(Driver(), "research"), []
                @contextmanager
                def stream(*args, **kwargs):
                    try:
                        yield iter(rows)
                    finally:
                        closed.append(True)
                client.stream_cypher = stream
                with self.assertRaisesRegex(ValueError, "schema response"):
                    client.schema()
                self.assertEqual(closed, [True])

    def test_mixed_shapes_preserve_duplicate_order_and_bounded_batches(self):
        driver = Driver()
        graph = GraphClient(driver, "tickets", batch_size=3)
        rows = [{"entity_id":"same", "status":"first"},
                {"entity_id":"same", "status":"second", "tags":["x"]},
                {"entity_id":"same", "status":"last"},
                {"entity_id":"other", "status":None}]
        self.assertEqual(graph.load_nodes("Ticket", iter(rows)), 4)
        self.assertEqual([len(batch) for batch in driver.committed], [3, 1])
        records = [row for batch in driver.committed for _, params in batch for row in params["rows"]]
        self.assertEqual([row["status"] for row in records], ["first","second","last",None])
        self.assertEqual(driver.database,"tickets")

    def test_load_nodes_reports_only_committed_batches_with_cumulative_input_rows(self):
        driver = Driver()
        graph = GraphClient(driver, "tickets", batch_size=2)
        progress = []
        count = graph.load_nodes("Ticket", [
            {"entity_id": "a"}, {"entity_id": "b"}, {"entity_id": "c"}
        ], on_batch=lambda batch, total: progress.append((batch, total)))
        self.assertEqual(count, 3)
        self.assertEqual(progress, [(1, 2), (2, 3)])
        self.assertEqual(driver.write_count, 2)

    def test_full_load_batch_commits_before_requesting_the_next_source_row(self):
        cases = [
            ("nodes", {"entity_id": "a"}, 33),
            ("edges", {"source_id": "a", "target_id": "b"}, 49),
        ]
        for kind, row, encoded_size in cases:
            for bounds in ({"batch_size": 1}, {"batch_bytes": encoded_size}):
                with self.subTest(kind=kind, bounds=bounds):
                    driver, progress = Driver(), []
                    graph = GraphClient(driver, "tickets", **bounds)

                    def source():
                        yield row
                        self.assertEqual(progress, [(1, 1)])
                        self.assertEqual(len(driver.committed), 1)
                        raise RuntimeError("source failed after acknowledged batch")

                    callback = lambda batch, total: progress.append((batch, total))
                    with self.assertRaisesRegex(RuntimeError, "source failed"):
                        if kind == "nodes":
                            graph.load_nodes("Ticket", source(), on_batch=callback)
                        else:
                            graph.load_edges("Page", "LINK", "Page", source(), on_batch=callback)
                    self.assertEqual(progress, [(1, 1)])
                    self.assertEqual(len(driver.committed), 1)

    def test_incomplete_load_batch_is_not_committed_when_source_fails(self):
        for kind, row in (("nodes", {"entity_id": "a"}),
                          ("edges", {"source_id": "a", "target_id": "b"})):
            with self.subTest(kind=kind):
                driver, progress = Driver(), []
                graph = GraphClient(driver, "tickets", batch_size=2)

                def source():
                    yield row
                    raise RuntimeError("source failed before batch was full")

                callback = lambda batch, total: progress.append((batch, total))
                with self.assertRaisesRegex(RuntimeError, "source failed"):
                    if kind == "nodes":
                        graph.load_nodes("Ticket", source(), on_batch=callback)
                    else:
                        graph.load_edges("Page", "LINK", "Page", source(), on_batch=callback)
                self.assertEqual(progress, [])
                self.assertEqual(driver.committed, [])

    def test_load_nodes_with_acl_stamps_each_batch_in_the_same_transaction(self):
        driver = Driver()
        graph = GraphClient(driver, "tickets", batch_size=2)
        rows = [{"entity_id": f"t{i}", "subject": "s",
                 "acl": {"allow": ["user:a"], "connectorIds": ["c1"]}} for i in range(3)]
        self.assertEqual(graph.load_nodes("Ticket", rows, with_acl=True), 3)
        self.assertEqual(len(driver.committed), 2)
        upsert, stamp = driver.committed[0]
        self.assertNotIn("acl", upsert[1]["rows"][0])
        self.assertIn("CALL db.setNodeAcls($rows)", stamp[0])
        self.assertEqual(stamp[1]["rows"][0], {"label": "Ticket", "entityId": "t0",
                                               "allow": ["user:a"], "connectorIds": ["c1"]})

    def test_load_nodes_with_acl_rejects_missing_or_invalid_acl_before_writing(self):
        driver = Driver()
        graph = GraphClient(driver, "tickets")
        for row in [{"entity_id": "a"}, {"entity_id": "a", "acl": {"owner": ["x"]}},
                    {"entity_id": "a", "acl": ["user:a"]}]:
            with self.subTest(row=row), self.assertRaises(ValueError):
                graph.load_nodes("Ticket", [row], with_acl=True)
        with self.assertRaises(ValueError):
            graph.load_nodes("Ticket", [{"entity_id": "a", "acl": {"allow": []}}])
        self.assertEqual(driver.write_count, 0)

    def test_load_nodes_consumes_arrow_record_batch_reader_incrementally(self):
        first = ArrowBatch([{"entity_id": "a"}, {"entity_id": "b"}, {"entity_id": "c"}])
        driver = Driver()
        graph = GraphClient(driver, "tickets", batch_size=2)

        self.assertEqual(graph.load_nodes("Ticket", ArrowReader(first)), 3)
        self.assertEqual(first.conversion_sizes, [2, 1])
        self.assertEqual(
            [len(params["rows"]) for batch in driver.committed for _, params in batch],
            [2, 1],
        )

    def test_load_nodes_accepts_arrow_table_and_single_record_batch(self):
        table_batch = ArrowBatch([{"entity_id": "table-row"}])
        record_batch = ArrowBatch([{"entity_id": "batch-row"}])
        driver = Driver()
        graph = GraphClient(driver, "tickets")

        table = ArrowTable(table_batch)
        self.assertEqual(graph.load_nodes("Ticket", table), 1)
        self.assertEqual(graph.load_nodes("Ticket", record_batch), 1)
        self.assertEqual(table.max_chunksize, graph.batch_size)
        self.assertTrue(table_batch.converted)
        self.assertTrue(record_batch.converted)

    def test_load_edges_can_skip_rows_with_missing_endpoints(self):
        driver = Driver(counts=[1, 1])
        graph = GraphClient(driver, "tickets")
        rows = [{"source_id": "a", "target_id": "b"}, {"source_id": "a", "target_id": "missing"}]
        with self.assertRaises(ValueError):
            graph.load_edges("Person", "OPENED", "Ticket", rows)
        self.assertEqual(graph.load_edges("Person", "OPENED", "Ticket", rows, require_endpoints=False), 1)

    def test_load_edges_does_not_report_a_batch_that_failed_to_commit(self):
        driver = Driver()
        driver.fail_on_write = 2
        graph = GraphClient(driver, "tickets", batch_size=1)
        progress = []
        with self.assertRaisesRegex(RuntimeError, "write failed"):
            graph.load_edges("Page", "LINK", "Page", [
                {"source_id": "a", "target_id": "b"},
                {"source_id": "b", "target_id": "c"},
            ], on_batch=lambda batch, total: progress.append((batch, total)))
        self.assertEqual(progress, [(1, 1)])
        self.assertEqual(len(driver.committed), 1)

    def test_load_progress_callback_must_be_callable(self):
        graph = GraphClient(Driver(), "tickets")
        with self.assertRaisesRegex(ValueError, "on_batch must be callable"):
            graph.load_nodes("Ticket", [], on_batch=True)

    def test_load_progress_callback_runs_after_commit_and_cannot_undo_it(self):
        driver = Driver()
        graph = GraphClient(driver, "tickets", batch_size=1)
        def fail_after_commit(batch, total):
            raise RuntimeError("checkpoint failed")
        with self.assertRaisesRegex(RuntimeError, "checkpoint failed"):
            graph.load_nodes("Ticket", [{"entity_id": "committed"}], on_batch=fail_after_commit)
        self.assertEqual(len(driver.committed), 1)

    def test_load_progress_callback_does_not_use_managed_transaction_retries(self):
        driver = Driver()
        driver.retry_write = True
        graph = GraphClient(driver, "tickets", batch_size=1)
        progress = []
        graph.load_nodes("Ticket", [{"entity_id": "once"}],
                         on_batch=lambda batch, total: progress.append((batch, total)))
        self.assertEqual(progress, [(1, 1)])
        self.assertEqual(driver.write_count, 1)
        self.assertEqual(driver.managed_calls, 0)

    def test_delete_nodes_deduplicates_ids_and_detaches_in_atomic_batches(self):
        driver = Driver([1, 2, 0])
        graph = GraphClient(driver, "tickets", batch_size=2)
        self.assertEqual(graph.delete_nodes("Ticket", ["a", "a", "b", "c", "missing"]), 3)
        calls = [call for batch in driver.committed for call in batch]
        self.assertEqual([params["ids"] for _, params in calls],
                         [["a"], ["b", "c"], ["missing"]])
        self.assertTrue(all("DETACH DELETE n" in query for query, _ in calls))
        self.assertTrue(all("RETURN count(*) AS deleted" in query for query, _ in calls))

    def test_delete_nodes_replay_with_missing_ids_counts_only_the_first_removal(self):
        driver = Driver([1, 0])
        graph = GraphClient(driver, "tickets", batch_size=1)
        self.assertEqual(graph.delete_nodes("Ticket", ["same", "same"]), 1)
        self.assertEqual([params["ids"] for batch in driver.committed
                          for _, params in batch], [["same"], ["same"]])

    def test_delete_nodes_empty_iterable_is_a_noop_without_opening_a_session(self):
        driver = Driver()
        self.assertEqual(GraphClient(driver, "tickets").delete_nodes("Ticket", iter(())), 0)
        self.assertFalse(hasattr(driver, "database"))

    def test_delete_nodes_rejects_oversized_id_without_committing_that_batch(self):
        driver = Driver()
        graph = GraphClient(driver, "tickets", batch_size=2, batch_bytes=8)
        with self.assertRaisesRegex(ValueError, "entity ID exceeds"):
            graph.delete_nodes("Ticket", ["ok", "x" * 20])
        self.assertEqual(driver.committed, [])

    def test_delete_nodes_respects_utf8_byte_bound(self):
        driver = Driver([1, 1, 1])
        graph = GraphClient(driver, "tickets", batch_size=10, batch_bytes=4)
        self.assertEqual(graph.delete_nodes("Ticket", ["a", "é", "b"]), 3)
        calls = [call for batch in driver.committed for call in batch]
        self.assertEqual([params["ids"] for _, params in calls], [["a"], ["é"], ["b"]])

    def test_delete_nodes_rejects_single_values_and_mappings_before_connecting(self):
        driver = Driver()
        graph = GraphClient(driver, "tickets")
        for ids in ["ticket-1", b"ticket-1", {"entity_id": "ticket-1"}, None]:
            with self.subTest(ids=ids), self.assertRaises(ValueError):
                graph.delete_nodes("Ticket", ids)
        self.assertFalse(hasattr(driver, "database"))

    def test_delete_nodes_keeps_only_earlier_full_batches_on_invalid_later_input(self):
        driver = Driver([1])
        graph = GraphClient(driver, "tickets", batch_size=1)
        with self.assertRaisesRegex(ValueError, "entity_id"):
            graph.delete_nodes("Ticket", ["committed", ""])
        self.assertEqual(len(driver.committed), 1)
        self.assertEqual(driver.committed[0][0][1]["ids"], ["committed"])

    def test_delete_edges_deduplicates_pairs_and_counts_physical_relationships(self):
        driver = Driver([3, 0])
        graph = GraphClient(driver, "tickets", batch_size=2)
        pair = {"source_id": "a", "target_id": "b"}
        self.assertEqual(graph.delete_edges("Page", "LINK", "Page", [pair, pair, pair]), 3)
        calls = [call for batch in driver.committed for call in batch]
        self.assertEqual([params["rows"] for _, params in calls], [[pair], [pair]])
        self.assertTrue(all("DELETE r RETURN count(*) AS deleted" in query for query, _ in calls))
        self.assertTrue(all("[r:LINK]->" in query for query, _ in calls))

    def test_delete_edges_rejects_malformed_rows_without_flushing_pending_batch(self):
        for bad in [None, {}, {"source_id": "a", "target_id": "b", "extra": 1},
                    {"source_id": "a", "target_id": ""}]:
            driver = Driver()
            graph = GraphClient(driver, "tickets", batch_size=2)
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                graph.delete_edges("Page", "LINK", "Page", iter([{"source_id": "a", "target_id": "b"}, bad]))
            self.assertEqual(driver.committed, [])
        for rows in [None, "a", b"a", {}]:
            with self.assertRaises(ValueError):
                graph.delete_edges("Page", "LINK", "Page", rows)

    @staticmethod
    def keyed_relationships(*fields):
        return [{"name": "EVENT", "source": "Ticket", "target": "Ticket",
                 "mergeKeyFields": list(fields or ("kind", "revision"))}]

    def test_delete_keyed_edges_batches_deduplicate_complete_identity_and_parameterize(self):
        driver = Driver([0, 2, 0, 0], relationships=self.keyed_relationships())
        graph = GraphClient(driver, "tickets", batch_size=3)
        first = {"source_id": "a", "target_id": "b", "key": {"kind": "closed", "revision": "r-1"}}
        second = {"source_id": "a", "target_id": "b", "key": {"revision": "r-2", "kind": "closed"}}
        self.assertEqual(graph.delete_keyed_edges("Ticket", "EVENT", "Ticket", [first, first, second, first]), 2)
        self.assertEqual([len(batch) for batch in driver.committed], [3, 3])
        queries = driver.committed[0]
        self.assertTrue(queries[0][0].startswith("CALL db.schema()"))
        for query, params in queries[1:]:
            self.assertIn("[r:EVENT {kind:row.key_0, revision:row.key_1}]", query)
            self.assertEqual(params["rows"], [
                {"source_id": "a", "target_id": "b", "key_0": "closed", "key_1": "r-1"},
                {"source_id": "a", "target_id": "b", "key_0": "closed", "key_1": "r-2"}])
        self.assertIn("matched > 1", queries[1][0])
        self.assertIn("DELETE r RETURN count(*) AS deleted", queries[2][0])

    def test_delete_keyed_edges_rejects_partial_keys_and_ambiguity_before_mutation(self):
        good = {"source_id": "a", "target_id": "b", "key": {"kind": "closed", "revision": "r-1"}}
        for key in ({"kind": "closed"}, {"kind": "closed", "revision": "r-1", "extra": "x"}):
            driver = Driver(relationships=self.keyed_relationships())
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, "complete.*key"):
                GraphClient(driver, "tickets").delete_keyed_edges("Ticket", "EVENT", "Ticket", [good, {**good, "key": key}])
            self.assertEqual(driver.committed, [])
        driver = Driver([1], relationships=self.keyed_relationships())
        with self.assertRaisesRegex(ValueError, "one .*relationship"):
            GraphClient(driver, "tickets").delete_keyed_edges("Ticket", "EVENT", "Ticket", [good])
        self.assertEqual(driver.committed, [])

    def test_delete_keyed_edges_validates_rows_before_flushing_pending_batch(self):
        good = {"source_id": "a", "target_id": "b", "key": {"kind": "closed"}}
        bad_rows = [None, {}, {**good, "extra": 1}, {**good, "source_id": ""},
                    {**good, "key": {}}, {**good, "key": {"kind": None}},
                    {**good, "key": {"kind": ""}}, {**good, "key": {"acl_allow": "x"}},
                    {**good, "key": {"k}]->() DELETE r": "x"}}]
        for bad in bad_rows:
            driver = Driver(relationships=self.keyed_relationships("kind"))
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                GraphClient(driver, "tickets").delete_keyed_edges("Ticket", "EVENT", "Ticket", [good, bad])
            self.assertEqual(driver.committed, [])
        for rows in [None, "a", b"a", {}, 1]:
            driver = Driver()
            with self.subTest(rows=rows), self.assertRaises(ValueError):
                GraphClient(driver, "tickets").delete_keyed_edges("Ticket", "EVENT", "Ticket", rows)
            self.assertFalse(hasattr(driver, "database"))

    def test_delete_keyed_edges_late_bad_key_preserves_earlier_batches(self):
        driver = Driver([0, 1], relationships=self.keyed_relationships())
        graph = GraphClient(driver, "tickets", batch_size=1)
        row = {"source_id": "a", "target_id": "b", "key": {"kind": "closed", "revision": "r-1"}}
        with self.assertRaisesRegex(ValueError, "complete.*key"):
            graph.delete_keyed_edges("Ticket", "EVENT", "Ticket", [row, {**row, "key": {"kind": "closed"}}])
        self.assertEqual(len(driver.committed), 1)

    def test_delete_keyed_edges_utf8_bound_empty_input_and_caller_mutation(self):
        import json
        row = {"source_id": "a", "target_id": "b", "key": {"kind": "é"}}
        bound = len(json.dumps(row, ensure_ascii=False, separators=(",", ":")).encode())
        driver = Driver([0, 1, 0, 1], relationships=self.keyed_relationships("kind"))
        graph = GraphClient(driver, "tickets", batch_bytes=bound)
        self.assertEqual(graph.delete_keyed_edges("Ticket", "EVENT", "Ticket", []), 0)
        self.assertFalse(hasattr(driver, "database"))
        self.assertEqual(graph.delete_keyed_edges("Ticket", "EVENT", "Ticket", [row, row]), 2)
        self.assertEqual(len(driver.committed), 2)
        with self.assertRaisesRegex(ValueError, "byte limit"):
            graph.delete_keyed_edges("Ticket", "EVENT", "Ticket", [{**row, "key": {"kind": "éé"}}])
        driver = Driver([0, 1], relationships=self.keyed_relationships("kind"))
        def rows():
            yield row
            row["key"]["kind"] = "changed"
        self.assertEqual(GraphClient(driver, "tickets").delete_keyed_edges("Ticket", "EVENT", "Ticket", rows()), 1)
        self.assertEqual(driver.committed[0][-1][1]["rows"][0]["key_0"], "é")

    def test_delete_keyed_edges_propagates_schema_denial_and_rechecks_each_batch(self):
        denied = RuntimeError("schema forbidden")
        driver = Driver(schema_error=denied)
        row = {"source_id": "a", "target_id": "b", "key": {"kind": "closed"}}
        with self.assertRaises(RuntimeError) as raised:
            GraphClient(driver, "tickets").delete_keyed_edges("Ticket", "EVENT", "Ticket", [row])
        self.assertIs(raised.exception, denied)
        self.assertEqual(driver.committed, [])
        driver = Driver([0, 1], relationships=self.keyed_relationships("kind"))
        def rows():
            yield row
            driver.relationships = self.keyed_relationships("kind", "revision")
            yield row
        with self.assertRaisesRegex(ValueError, "complete.*key"):
            GraphClient(driver, "tickets", batch_size=1).delete_keyed_edges("Ticket", "EVENT", "Ticket", rows())
        self.assertEqual(len(driver.committed), 1)

    def test_delete_keyed_edges_selector_names_do_not_overwrite_endpoints(self):
        driver = Driver([0, 1], relationships=self.keyed_relationships("source_id", "target_id"))
        row = {"source_id": "left", "target_id": "right",
               "key": {"source_id": "event-left", "target_id": "event-right"}}
        self.assertEqual(GraphClient(driver, "tickets").delete_keyed_edges(
            "Ticket", "EVENT", "Ticket", [row]), 1)
        query, params = driver.committed[0][-1]
        self.assertIn("EVENT {source_id:row.key_0, target_id:row.key_1}", query)
        self.assertEqual(params["rows"], [{"source_id": "left", "target_id": "right",
                                          "key_0": "event-left", "key_1": "event-right"}])

    def test_delete_keyed_edges_rejects_unkeyed_and_malformed_schema(self):
        row = {"source_id": "a", "target_id": "b", "key": {"kind": "closed"}}
        invalid = [None, [], {}, [None], self.keyed_relationships("kind", "kind"),
                   [{"name": "EVENT", "source": "Ticket", "target": "Ticket"}],
                   self.keyed_relationships("kind") * 2]
        for relationships in invalid:
            driver = Driver(relationships=relationships)
            with self.subTest(relationships=relationships), self.assertRaises(ValueError):
                GraphClient(driver, "tickets").delete_keyed_edges("Ticket", "EVENT", "Ticket", [row])
            self.assertEqual(driver.committed, [])

    def test_delete_keyed_edges_values_are_parameters_and_second_batch_failure_is_local(self):
        driver = Driver([0, 1], relationships=self.keyed_relationships("kind"))
        value = "é'}]->() DELETE r //"
        row = {"source_id": "a", "target_id": "b", "key": {"kind": value}}
        graph = GraphClient(driver, "tickets", batch_size=1)
        driver.fail_on_write = 2
        with self.assertRaisesRegex(RuntimeError, "write failed"):
            graph.delete_keyed_edges("Ticket", "EVENT", "Ticket", [row, row])
        self.assertEqual(len(driver.committed), 1)
        for query, params in driver.committed[0][1:]:
            self.assertNotIn(value, query)
            self.assertEqual(params["rows"][0]["key_0"], value)
        self.assertEqual(driver.managed_calls, 0)

    def test_delete_edges_late_invalid_input_preserves_completed_batches(self):
        driver = Driver([1])
        graph = GraphClient(driver, "tickets", batch_size=1)
        with self.assertRaises(ValueError):
            graph.delete_edges("Page", "LINK", "Page", iter([{"source_id": "a", "target_id": "b"}, {}]))
        self.assertEqual(len(driver.committed), 1)

    def test_delete_edges_bytes_empty_and_oversized_rows(self):
        driver = Driver()
        graph = GraphClient(driver, "tickets", batch_bytes=34)
        self.assertEqual(graph.delete_edges("Page", "LINK", "Page", []), 0)
        self.assertFalse(hasattr(driver, "database"))
        self.assertEqual(graph.delete_edges("Page", "LINK", "Page", [
            {"source_id": "é", "target_id": "b"}, {"source_id": "a", "target_id": "b"}]), 2)
        self.assertEqual(len(driver.committed), 2)
        with self.assertRaisesRegex(ValueError, "byte limit"):
            graph.delete_edges("Page", "LINK", "Page", [{"source_id": "x" * 100, "target_id": "b"}])

    def test_edge_batches_are_bounded_and_preserve_relationship_properties(self):
        driver = Driver()
        graph = GraphClient(driver, "tickets", batch_size=2)
        progress = []
        rows = [
            {"source_id": "a", "target_id": "b", "weight": 0.2},
            {"source_id": "a", "target_id": "c", "weight": 0.8, "kind": "related"},
            {"source_id": "b", "target_id": "c", "weight": None},
        ]
        self.assertEqual(graph.load_edges(
            "Page", "LINK", "Page", rows,
            on_batch=lambda batch, total: progress.append((batch, total)),
        ), 3)
        self.assertEqual(progress, [(1, 2), (2, 3)])
        self.assertEqual([len(batch) for batch in driver.committed], [2, 1])
        calls = [call for batch in driver.committed for call in batch]
        self.assertTrue(all("UNWIND $rows AS row" in query for query, _ in calls))
        self.assertTrue(all("MERGE (a)-[:LINK" in query for query, _ in calls))
        self.assertTrue(any("weight:row.weight" in query and "kind:row.kind" in query for query, _ in calls))
        self.assertEqual([row for _, params in calls for row in params["rows"]], rows)

    def test_edge_batch_enforces_byte_bound_before_committing_that_row(self):
        driver = Driver()
        graph = GraphClient(driver, "tickets", batch_size=10, batch_bytes=100)
        with self.assertRaisesRegex(ValueError, "batch byte limit"):
            graph.load_edges("Page", "LINK", "Page", [
                {"source_id": "a", "target_id": "b"},
                {"source_id": "b", "target_id": "c", "note": "x" * 200},
            ])
        self.assertEqual(driver.committed, [])

    def test_patch_separates_unchanged_set_and_remove_and_missing_node(self):
        driver = Driver([1,0])
        graph = GraphClient(driver,"tickets")
        self.assertTrue(graph.update_node("Ticket","t-1",{"status":"open"},remove=["priority"]))
        query, params = driver.committed[0][0]
        self.assertNotIn("MERGE",query)
        self.assertEqual(params,{"id":"t-1","property_0":None,"property_1":"open"})
        self.assertFalse(graph.update_node("Ticket","absent",{"status":"closed"}))

    def test_edge_patch_checks_existence_and_null_removal_in_one_transaction(self):
        driver = Driver([1,1,0])
        graph = GraphClient(driver,"pages")
        self.assertTrue(graph.update_edge(("Page","a"),"LINK",("Page","b"),{"weight":2},remove=["old"]))
        self.assertEqual(len(driver.committed[0]),2)
        self.assertEqual(driver.committed[0][1][1],{"source":"a","target":"b","property_0":None,"property_1":2})
        self.assertFalse(graph.update_edge(("Page","a"),"LINK",("Page","missing"),{"weight":3}))
        self.assertEqual(len(driver.committed[1]),1)
        driver = Driver([0])
        with self.assertRaises(ValueError):
            GraphClient(driver,"pages").upsert_edge(("Page","a"),"LINK",("Page","missing"))
        self.assertEqual(driver.committed,[])

    def test_edge_patch_never_creates_a_relationship_from_update_properties(self):
        driver = Driver([1, 1])
        graph = GraphClient(driver, "tickets")
        self.assertTrue(graph.update_edge(("Ticket", "a"), "EVENT", ("Ticket", "b"),
                                          {"event_key": "absent", "summary": "changed"}))
        query, params = driver.committed[0][1]
        self.assertNotIn("MERGE", query)
        self.assertIn("MATCH (a:Ticket {entity_id:$source})-[r:EVENT]->", query)
        self.assertIn("SET r.event_key=$property_0, r.summary=$property_1", query)
        self.assertEqual(params["property_0"], "absent")

    def test_edge_patch_refuses_parallel_relationships_without_writing(self):
        driver = Driver([2])
        with self.assertRaisesRegex(ValueError, "one .*relationship"):
            GraphClient(driver, "tickets").update_edge(
                ("Ticket", "a"), "EVENT", ("Ticket", "b"), {"summary": "changed"})
        self.assertEqual(driver.committed, [])

    def test_keyed_event_patch_uses_complete_catalog_key_in_one_transaction(self):
        relationships = [{"name": "EVENT", "source": "Ticket", "target": "Ticket",
                          "mergeKeyFields": ["kind", "revision"]}]
        driver = Driver([1, 1], relationships=relationships)
        graph = GraphClient(driver, "tickets")
        self.assertTrue(graph.update_edge(("Ticket", "a"), "EVENT", ("Ticket", "b"),
                                         {"summary": "corrected"}, remove=["old"],
                                         key={"kind": "closed", "revision": "r-2"}))
        self.assertEqual(len(driver.committed), 1)
        queries = driver.committed[0]
        self.assertEqual(len(queries), 3)
        self.assertTrue(queries[0][0].startswith("CALL db.schema()"))
        for query, params in queries[1:]:
            self.assertIn("EVENT {kind:$key_0, revision:$key_1}", query)
            self.assertEqual((params["key_0"], params["key_1"]), ("closed", "r-2"))
        self.assertIn("SET r.old=$property_0, r.summary=$property_1", queries[-1][0])

    def test_keyed_event_patch_rejects_partial_or_unknown_keys_before_mutation(self):
        relationships = [{"name": "EVENT", "source": "Ticket", "target": "Ticket",
                          "mergeKeyFields": ["kind", "revision"]}]
        for key in ({"kind": "closed"}, {"kind": "closed", "revision": "r-2", "other": "x"}):
            driver = Driver([1, 1], relationships=relationships)
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, "complete.*key"):
                GraphClient(driver, "tickets").update_edge(
                    ("Ticket", "a"), "EVENT", ("Ticket", "b"), {"summary": "changed"}, key=key)
            self.assertEqual(driver.committed, [])
        for key in ({}, {"kind": ""}, {"kind": 1}, {"kind": None}, {"acl_allow": "x"}, {"kind}]->() DELETE r": "x"}):
            driver = Driver()
            with self.subTest(key=key), self.assertRaises(ValueError):
                GraphClient(driver, "tickets").update_edge(
                    ("Ticket", "a"), "EVENT", ("Ticket", "b"), {}, key=key)
            self.assertEqual(driver.committed, [])

    def test_keyed_event_values_are_parameters(self):
        driver = Driver([1, 1], relationships=[{
            "name": "EVENT", "source": "Ticket", "target": "Ticket", "mergeKeyFields": ["kind"]}])
        value = "é'}]->() DELETE r //"
        self.assertTrue(GraphClient(driver, "tickets").delete_edge(
            ("Ticket", "a"), "EVENT", ("Ticket", "b"), key={"kind": value}))
        for query, parameters in driver.committed[0][1:]:
            self.assertNotIn(value, query)
            self.assertEqual(parameters["key_0"], value)

    def test_keyed_event_patch_cannot_set_or_remove_selector_keys(self):
        for properties, remove in (({"kind": "opened"}, ()), ({}, ["kind"])):
            driver = Driver()
            with self.subTest(properties=properties, remove=remove), self.assertRaisesRegex(ValueError, "immutable"):
                GraphClient(driver, "tickets").update_edge(
                    ("Ticket", "a"), "EVENT", ("Ticket", "b"), properties,
                    remove=remove, key={"kind": "closed"})
            self.assertEqual(driver.committed, [])

    def test_delete_keyed_event_is_selective_and_repeated_delete_is_missing(self):
        relationships = [{"name": "EVENT", "source": "Ticket", "target": "Ticket",
                          "mergeKeyFields": ["kind", "revision"]}]
        driver = Driver([1, 1, 0], relationships=relationships)
        graph = GraphClient(driver, "tickets")
        arguments = (("Ticket", "a"), "EVENT", ("Ticket", "b"))
        key = {"kind": "closed", "revision": "r-2"}
        self.assertTrue(graph.delete_edge(*arguments, key=key))
        self.assertFalse(graph.delete_edge(*arguments, key=key))
        query, params = driver.committed[0][-1]
        self.assertIn("[r:EVENT {kind:$key_0, revision:$key_1}]", query)
        self.assertIn("DELETE r", query)
        self.assertNotIn("MERGE", query)
        self.assertEqual(params["key_1"], "r-2")
        self.assertEqual(len(driver.committed[1]), 2)

    def test_keyed_event_mutation_refuses_ambiguous_or_unkeyed_schema(self):
        for relationships, count in (([{"name": "EVENT", "source": "Ticket", "target": "Ticket",
                                       "mergeKeyFields": ["kind"]}], 2),
                                     ([{"name": "EVENT", "source": "Ticket", "target": "Ticket"}], 1),
                                     (None, 1)):
            driver = Driver([count], relationships=relationships)
            with self.subTest(relationships=relationships), self.assertRaises(ValueError):
                GraphClient(driver, "tickets").delete_edge(
                    ("Ticket", "a"), "EVENT", ("Ticket", "b"), key={"kind": "closed"})
            self.assertEqual(driver.committed, [])

    def test_keyed_event_mutation_propagates_schema_denial_without_writing(self):
        denied = RuntimeError("schema forbidden")
        driver = Driver(schema_error=denied)
        with self.assertRaises(RuntimeError) as raised:
            GraphClient(driver, "tickets").delete_edge(
                ("Ticket", "a"), "EVENT", ("Ticket", "b"), key={"kind": "closed"})
        self.assertIs(raised.exception, denied)
        self.assertEqual(driver.committed, [])

    def test_rejects_injection_reserved_fields_invalid_values_and_oversized_rows(self):
        graph = GraphClient(Driver(),"tickets",batch_bytes=100)
        for label, props in [("Ticket) RETURN 1",{}),("Ticket",{"acl_allow":["user"]}),
                             ("Ticket",{"entity_id":"new"}),("Ticket",{"x":float("nan")}),
                             ("Ticket",{"x":1<<64}),("Ticket",{"x":b"binary"}),
                             ("Ticket",{"x":"x"*101}),("Ticket",[])]:
            with self.subTest(label=label,props=props), self.assertRaises(ValueError):
                graph.upsert_node(label,"id",props)
        with self.assertRaises(ValueError):
            graph.update_node("Ticket","id",{"x":1},remove=["x"])
        self.assertEqual(graph.load_nodes("Ticket",[]),0)


class ExplicitDriver:
    """Only explicit transactions; the cleanup mirrors Neo4j's transaction context."""
    def __init__(self, *, begin_error=None, commit_error=None):
        self.events = []
        self.begin_error, self.commit_error = begin_error, commit_error
        self.tx = self.Tx(self)

    def session(self, *, database):
        self.events.append(("session", database))
        return self

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.events.append("session-close")

    def begin_transaction(self, *, metadata=None):
        self.events.append(("begin", metadata))
        if self.begin_error:
            raise self.begin_error
        return self.tx

    def execute_write(self, callback):
        raise AssertionError("explicit retained writes must not use managed retries")

    class Tx:
        def __init__(self, driver):
            self.driver, self.closed = driver, False

        def __enter__(self):
            return self

        def __exit__(self, *args):
            if not self.closed:
                self.rollback()
            self.close()

        def commit(self):
            self.driver.events.append("commit")
            self.closed = True  # The real driver also closes when a commit fails.
            if self.driver.commit_error:
                raise self.driver.commit_error

        def rollback(self):
            self.driver.events.append("rollback")
            self.closed = True

        def close(self):
            self.driver.events.append("tx-close")


class RetryableCommitDisconnect(ConnectionError):
    """A transport failure for which a managed driver would retry its callback."""


class RetryingDriver(ExplicitDriver):
    def __init__(self, *, lose_commit_reply=False):
        super().__init__()
        self.lose_commit_reply = lose_commit_reply
        self.failure = RetryableCommitDisconnect("COMMIT response lost")
        self.applied = 0

    def begin_transaction(self, *, metadata=None):
        self.events.append(("begin", metadata))
        self.tx = self.Tx(self)
        return self.tx

    def execute_write(self, callback):
        self.events.append("managed")
        for attempt in range(2):
            with self.begin_transaction() as tx:
                result = callback(tx)
                try:
                    tx.commit()
                except RetryableCommitDisconnect:
                    continue
                return result
        raise self.failure

    class Tx(ExplicitDriver.Tx):
        def run(self, query, **params):
            self.driver.events.append(("run", query, params))
            return Result(len(params.get("rows", [1])))

        def commit(self):
            self.driver.events.append("commit")
            self.closed = True
            self.driver.applied += 1
            if self.driver.lose_commit_reply and self.driver.applied == 1:
                raise self.driver.failure


class GenericWriteTests(unittest.TestCase):
    def test_lost_commit_reply_does_not_replay_callback_or_report_success(self):
        driver = RetryingDriver(lose_commit_reply=True)
        calls = []
        with self.assertRaises(RetryableCommitDisconnect) as caught:
            GraphClient(driver, "tickets")._write(lambda tx: calls.append(tx) or "unacknowledged")
        self.assertIs(caught.exception, driver.failure)
        self.assertEqual(calls, [driver.tx])
        self.assertEqual(driver.applied, 1)
        self.assertNotIn("managed", driver.events)
        self.assertEqual(driver.events[-2:], ["tx-close", "session-close"])

    def test_lost_batch_commit_reply_does_not_report_progress_or_replay(self):
        driver = RetryingDriver(lose_commit_reply=True)
        progress = []
        with self.assertRaises(RetryableCommitDisconnect):
            GraphClient(driver, "tickets").load_nodes("Ticket", [{"entity_id": "one"}],
                on_batch=lambda batch, count: progress.append((batch, count)))
        self.assertEqual(progress, [])
        self.assertEqual(driver.applied, 1)
        self.assertEqual(len([event for event in driver.events
                              if isinstance(event, tuple) and event[0] == "run"]), 1)
        self.assertNotIn("managed", driver.events)

    def test_one_shot_success_returns_callback_value_after_commit(self):
        driver = RetryingDriver()
        value = object()
        result = GraphClient(driver, "tickets")._write(lambda tx: value)
        self.assertIs(result, value)
        self.assertEqual(driver.applied, 1)
        self.assertNotIn("managed", driver.events)
        self.assertEqual(driver.events[-3:], ["commit", "tx-close", "session-close"])

    def test_callback_error_rolls_back_once_and_preserves_exception(self):
        driver = RetryingDriver()
        failure = ValueError("invalid batch")
        def fail(tx):
            raise failure
        with self.assertRaises(ValueError) as caught:
            GraphClient(driver, "tickets")._write(fail)
        self.assertIs(caught.exception, failure)
        self.assertEqual(driver.applied, 0)
        self.assertEqual(driver.events.count("rollback"), 1)
        self.assertNotIn("managed", driver.events)


class QuorumClientTests(unittest.TestCase):
    def test_explicit_metadata_commit_returns_callback_value_once(self):
        driver = ExplicitDriver()
        seen = []
        result = GraphClient(driver, "tickets").write_transaction(
            lambda tx: seen.append(tx) or {"value": 3}, request_id="retained-1_:.")
        self.assertEqual(result, {"value": 3})
        self.assertEqual(seen, [driver.tx])
        self.assertEqual(driver.events, [("session", "tickets"),
            ("begin", {"brainwire_request_id": "retained-1_:."}), "commit", "tx-close", "session-close"])

    def test_callback_error_rolls_back_and_preserves_exception(self):
        driver = ExplicitDriver()
        failure = RuntimeError("callback failed")
        def operation(tx):
            self.assertIs(tx, driver.tx)
            raise failure
        with self.assertRaises(RuntimeError) as caught:
            GraphClient(driver, "tickets").write_transaction(operation, request_id="once")
        self.assertIs(caught.exception, failure)
        self.assertEqual(driver.events[-3:], ["rollback", "tx-close", "session-close"])
        self.assertNotIn("commit", driver.events)

    def test_begin_and_commit_errors_propagate_without_retry_or_result(self):
        for phase, message in [("begin", "AlreadyCommitted"), ("commit", "CommitOutcomeUnknown")]:
            with self.subTest(phase=phase):
                failure = RuntimeError(message)
                driver = ExplicitDriver(**{phase + "_error": failure})
                calls = []
                with self.assertRaises(RuntimeError) as caught:
                    GraphClient(driver, "tickets").write_transaction(
                        lambda tx: calls.append(tx) or "unacknowledged", request_id="same-seed")
                self.assertIs(caught.exception, failure)
                self.assertEqual(len(calls), phase == "commit")
                self.assertEqual(sum(isinstance(event, tuple) and event[0] == "begin" for event in driver.events), 1)
                self.assertEqual(driver.events.count("commit"), phase == "commit")
                self.assertNotIn("rollback", driver.events)
                self.assertEqual(driver.events[-1], "session-close")

    def test_invalid_seed_rejected_before_session_opens(self):
        for seed in [None, 123, "", "a" * 129, "with space", "á", "new\nline", "slash/", "emoji😀"]:
            with self.subTest(seed=seed):
                driver = ExplicitDriver()
                with self.assertRaises(ValueError):
                    GraphClient(driver, "tickets").write_transaction(lambda tx: None, request_id=seed)
                self.assertEqual(driver.events, [])
        driver = ExplicitDriver()
        GraphClient(driver, "tickets").write_transaction(lambda tx: None, request_id="x" * 128)

    def test_request_key_matches_compact_utf8_rust_namespace(self):
        # sha256 of UTF-8 ["clústér","tickets","usuário","seed-1"] without JSON whitespace/ASCII escaping.
        self.assertEqual(request_key("clústér", "tickets", "usuário", "seed-1"),
                         "469587db5e52647c501ea9c0d81a913d1a62babff515e19d30de65e954edf1c8")
        baseline = request_key("cluster", "tickets", "writer", "seed")
        for values in [("other", "tickets", "writer", "seed"), ("cluster", "other", "writer", "seed"),
                       ("cluster", "tickets", "reader", "seed"), ("cluster", "tickets", "writer", "other")]:
            self.assertNotEqual(request_key(*values), baseline)
        for values in [("", "g", "p", "s"), ("c", None, "p", "s"), ("c", "g", 1, "s"),
                       ("c", "g", "p", "with space")]:
            with self.assertRaises(ValueError):
                request_key(*values)


class AclDriver(ExplicitDriver):
    def __init__(self, results=({"processed": 1}, {"updated": True}), **kwargs):
        self.results = iter(results)
        super().__init__(**kwargs)

    def execute_write(self, callback):
        self.events.append("managed")
        with self.tx as tx:
            result = callback(tx)
            tx.commit()
            return result

    class Tx(ExplicitDriver.Tx):
        def run(self, query, **params):
            self.driver.events.append(("run", query, params))
            row = next(self.driver.results)
            class Record:
                def single(self):
                    return row
            return Record()


class AtomicAclClientTests(unittest.TestCase):
    def test_node_patch_and_acl_share_one_transaction_in_both_modes(self):
        for request_id in [None, "acl-operation-1"]:
            with self.subTest(request_id=request_id):
                driver = AclDriver()
                graph = GraphClient(driver, "tickets")
                self.assertTrue(graph.upsert_node_with_acl("Ticket", "t-1",
                    {"subject": "new", "priority": None}, {"allow": ["reader"], "sourceTable": "tickets"},
                    request_id=request_id))
                calls = [event for event in driver.events if isinstance(event, tuple) and event[0] == "run"]
                self.assertEqual(len(calls), 2)
                self.assertTrue(calls[0][1].startswith("MERGE (n:Ticket {entity_id:$id}) SET "))
                self.assertEqual(calls[0][2], {"id": "t-1", "property_0": None, "property_1": "new"})
                self.assertEqual(calls[1][1], "CALL db.setNodeAcl($options) YIELD updated RETURN updated")
                self.assertEqual(calls[1][2], {"options": {"label": "Ticket", "entityId": "t-1",
                    "allow": ["reader"], "sourceTable": "tickets"}})
                self.assertEqual(driver.events.count("commit"), 1)
                self.assertNotIn("rollback", driver.events)
                if request_id is None:
                    self.assertIn(("begin", None), driver.events)
                    self.assertNotIn("managed", driver.events)
                else:
                    self.assertIn(("begin", {"brainwire_request_id": request_id}), driver.events)
                    self.assertNotIn("managed", driver.events)

    def test_invalid_acl_properties_or_request_seed_fail_before_open(self):
        for props, acl, seed in [({}, {"unknown": []}, None), ({}, [], None),
                ({}, {"allow": object()}, None), ({"acl_allow": []}, {}, None),
                ({"entity_id": "changed"}, {}, None), (None, {}, None), ({}, {}, "bad seed")]:
            with self.subTest(props=props, acl=acl, seed=seed):
                driver = AclDriver()
                with self.assertRaises(ValueError):
                    GraphClient(driver, "tickets").upsert_node_with_acl("Ticket", "t-1", props, acl, request_id=seed)
                self.assertEqual(driver.events, [])

    def test_acl_refusal_or_unexpected_node_count_rolls_back_without_commit(self):
        for rows in [({"processed": 1}, {"updated": False}), ({"processed": 0},), ({"processed": 2},)]:
            for seed in [None, "retained-acl"]:
                with self.subTest(rows=rows, seed=seed):
                    driver = AclDriver(rows)
                    with self.assertRaises(ValueError):
                        GraphClient(driver, "tickets").upsert_node_with_acl("Ticket", "t-1", {}, {}, request_id=seed)
                    self.assertIn("rollback", driver.events)
                    self.assertNotIn("commit", driver.events)
                    self.assertEqual(driver.events[-2:], ["tx-close", "session-close"])

    def test_acl_upsert_does_not_return_success_after_unknown_commit(self):
        failure = RuntimeError("CommitOutcomeUnknown")
        driver = AclDriver(commit_error=failure)
        with self.assertRaises(RuntimeError) as caught:
            GraphClient(driver, "tickets").upsert_node_with_acl("Ticket", "t-1", {}, {}, request_id="retained")
        self.assertIs(caught.exception, failure)
        self.assertEqual(driver.events.count("commit"), 1)

    def test_existing_acl_setter_uses_one_shot_transaction(self):
        driver = AclDriver(({"updated": True},))
        self.assertTrue(GraphClient(driver, "tickets").set_node_acl("Ticket", "t-1", {}))
        self.assertNotIn("managed", driver.events)
        self.assertIn(("begin", None), driver.events)
        self.assertEqual(len([event for event in driver.events if isinstance(event, tuple) and event[0] == "run"]), 1)


if __name__ == "__main__":
    unittest.main()
