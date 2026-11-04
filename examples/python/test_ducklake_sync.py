"""DuckLake partition reads, change collapse and validation (no server needed).

Needs duckdb with cached ducklake/sqlite extensions; skips otherwise:
    uv run --no-project --with duckdb==1.5.5 python -m unittest examples.python.test_ducklake_sync -v
"""
import contextlib
import datetime
import decimal
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from examples.python import ducklake_sync
from examples.python.ducklake_sync import Mapping, _apply, stale_keys, latest_snapshot, lake, plain, read_partition, validate

try:
    import duckdb
except ImportError:
    duckdb = None

NODES = Mapping.from_json({"table": "main.customers", "label": "Customer", "id": "id",
                           "properties": {"name": "name", "score": "n"}})
EDGES = Mapping.from_json({"table": "main.follows", "type": "FOLLOWS",
                           "source": {"label": "Customer", "column": "src"},
                           "target": {"label": "Customer", "column": "dst"},
                           "properties": ["weight"]})
EDGE_SIGNATURE = {"name": "FOLLOWS", "source": "Customer", "target": "Customer"}


def collect(con, mapping, start, end, workers=3):
    upserts, deletes = {}, set()
    for k in range(workers):
        for key, row in read_partition(con, mapping, start, end, workers, k):
            assert key not in upserts and key not in deletes, f"{key} read by two partitions"
            if row is None:
                deletes.add(key)
            else:
                upserts[key] = row
    return upserts, deletes


@unittest.skipIf(duckdb is None, "duckdb is not installed")
class DuckLakeSyncTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        root = Path(self.temp.name)
        self.target = f"ducklake:sqlite:{root / 'catalog.sqlite'}"
        try:
            self.writer = duckdb.connect()
            self.writer.execute("LOAD ducklake")
            self.writer.execute("LOAD sqlite")
        except duckdb.Error as error:
            self.skipTest(f"cached DuckLake extensions unavailable: {error}")
        self.writer.execute(f"ATTACH '{self.target}' AS w (DATA_PATH '{root / 'data'}/')")
        self.sql("CREATE TABLE w.customers(id VARCHAR, name VARCHAR, n BIGINT)")
        self.sql("CREATE TABLE w.follows(src VARCHAR, dst VARCHAR, weight DOUBLE)")
        self.sql("INSERT INTO w.customers VALUES ('a','A',1),('b','B',2),('c','C',3)")
        self.sql("INSERT INTO w.follows VALUES ('a','b',0.5),('b','c',1.0)")
        self.base = self.latest()

    def tearDown(self):
        self.writer.close()
        self.temp.cleanup()

    def sql(self, query):
        self.writer.execute(query)

    def latest(self):
        with lake(self.target) as con:
            return latest_snapshot(con)

    def test_full_snapshot_partitions_are_disjoint_and_complete(self):
        self.sql("INSERT INTO w.customers VALUES ('d','D',4)")
        with lake(self.target) as con:
            upserts, deletes = collect(con, NODES, None, self.base, workers=4)
        self.assertEqual(deletes, set())
        self.assertEqual(upserts, {
            ("a",): {"entity_id": "a", "name": "A", "score": 1},
            ("b",): {"entity_id": "b", "name": "B", "score": 2},
            ("c",): {"entity_id": "c", "name": "C", "score": 3},
        })

    def test_changes_collapse_to_final_state_including_key_changes(self):
        self.sql("UPDATE w.customers SET name='B2' WHERE id='b'")
        self.sql("DELETE FROM w.customers WHERE id='c'")
        self.sql("UPDATE w.customers SET id='a2' WHERE id='a'")
        self.sql("INSERT INTO w.customers VALUES ('e','E',5)")
        self.sql("DELETE FROM w.customers WHERE id='e'")
        self.sql("INSERT INTO w.customers VALUES ('c','C again',6)")
        end = self.latest()
        with lake(self.target) as con:
            upserts, deletes = collect(con, NODES, self.base, end)
        self.assertEqual(upserts, {
            ("a2",): {"entity_id": "a2", "name": "A", "score": 1},
            ("b",): {"entity_id": "b", "name": "B2", "score": 2},
            ("c",): {"entity_id": "c", "name": "C again", "score": 6},
        })
        self.assertEqual(deletes, {("a",), ("e",)})
        with lake(self.target) as con:
            self.assertEqual(collect(con, NODES, end, end), ({}, set()))

    def test_edge_endpoint_change_deletes_the_old_pair(self):
        self.sql("UPDATE w.follows SET dst='a' WHERE src='b'")
        self.sql("UPDATE w.follows SET weight=0.75 WHERE src='a'")
        with lake(self.target) as con:
            upserts, deletes = collect(con, EDGES, self.base, self.latest())
        self.assertEqual(upserts, {
            ("b", "a"): {"source_id": "b", "target_id": "a", "weight": 1.0},
            ("a", "b"): {"source_id": "a", "target_id": "b", "weight": 0.75},
        })
        self.assertEqual(deletes, {("b", "c")})

    def test_acl_columns_become_a_private_acl_dict(self):
        self.sql("CREATE TABLE w.docs(id VARCHAR, title VARCHAR, acl_allow VARCHAR[], acl_connector_ids VARCHAR[], "
                 "acl_source_table VARCHAR)")
        self.sql("INSERT INTO w.docs VALUES ('d1','One',['group:a'],['c1'],'clean/docs'), ('d2','Two',NULL,['c1'],NULL)")
        docs = Mapping.from_json({"table": "main.docs", "label": "Doc", "id": "id", "properties": ["title"],
                                  "acl": {"allow": "acl_allow", "connectorIds": "acl_connector_ids",
                                          "sourceTable": "acl_source_table"}})
        with lake(self.target) as con:
            upserts, _ = collect(con, docs, None, self.latest())
        self.assertEqual(upserts[("d1",)], {"entity_id": "d1", "title": "One", "acl": {
            "allow": ["group:a"], "connectorIds": ["c1"], "sourceTable": "clean/docs"}})
        self.assertEqual(upserts[("d2",)]["acl"], {"connectorIds": ["c1"]})

    def test_validate_rejects_duplicate_null_keys_and_a_cursor_ahead(self):
        with lake(self.target) as con:
            validate(con, NODES, None, self.base)
        self.sql("INSERT INTO w.customers VALUES ('a','dup',9)")
        with lake(self.target) as con, self.assertRaisesRegex(ValueError, "duplicate"):
            validate(con, NODES, self.base, self.latest())
        self.sql("DELETE FROM w.customers WHERE name='dup'")
        self.sql("INSERT INTO w.customers VALUES (NULL,'nobody',0)")
        with lake(self.target) as con, self.assertRaisesRegex(ValueError, "null"):
            validate(con, NODES, None, self.latest())
        with lake(self.target) as con, self.assertRaisesRegex(ValueError, "ahead"):
            validate(con, NODES, self.latest() + 5, self.latest())

    def test_stale_keys_are_graph_keys_missing_from_the_source_snapshot(self):
        graph_nodes = [("a",), ("b",), ("gone",), ("it's, \"quoted\"",)]
        graph_edges = [("a", "b"), ("b", "c"), ("a", "c"), ("", "b")]
        with lake(self.target) as con:
            self.assertEqual(sorted(stale_keys(con, NODES, self.base, graph_nodes)),
                             [("gone",), ("it's, \"quoted\"",)])
            self.assertEqual(sorted(stale_keys(con, EDGES, self.base, graph_edges)), [("", "b"), ("a", "c")])
            self.assertEqual(stale_keys(con, NODES, self.base, []), [])


class Recorder:
    def __getattr__(self, name):
        return lambda *args, **_: setattr(self, name, args) or len(args[-1])


class ApplyTest(unittest.TestCase):
    def test_deletes_pass_plain_ids_and_endpoint_rows(self):
        client = Recorder()
        self.assertEqual(_apply(client, NODES, [{"entity_id": "a"}], [("b",), ("c",)]), (1, 2))
        self.assertEqual(client.delete_nodes, ("Customer", ["b", "c"]))
        _apply(client, EDGES, [], [("a", "b")])
        self.assertEqual(client.delete_edges, ("Customer", "FOLLOWS", "Customer",
                                               [{"source_id": "a", "target_id": "b"}]))


class EdgeIdentityTest(unittest.TestCase):
    def test_keyed_type_is_rejected_before_claim_or_source_work(self):
        for prune in (False, True):
            with self.subTest(prune=prune), \
                    patch.object(ducklake_sync, "_driver", return_value=contextlib.nullcontext(object())), \
                    patch.object(ducklake_sync.GraphClient, "schema", return_value={"relationships": [
                        dict(EDGE_SIGNATURE, mergeKeyFields=["event_key"])]}), \
                    patch.object(ducklake_sync, "claim") as claim, \
                    patch.object(ducklake_sync, "lake") as source, \
                    patch.object(ducklake_sync, "ProcessPoolExecutor") as pool:
                with self.assertRaisesRegex(ValueError, "keyed relationship"):
                    ducklake_sync.sync(EDGES, lake_target="lake", uri="bolt://unused",
                                       database="demo", source_id="edges", workers=1,
                                       full=prune, prune=prune)
                claim.assert_not_called()
                source.assert_not_called()
                pool.assert_not_called()

    def test_schema_failure_precedes_lease_mutation(self):
        with patch.object(ducklake_sync, "_driver", return_value=contextlib.nullcontext(object())), \
                patch.object(ducklake_sync.GraphClient, "schema", side_effect=RuntimeError("schema unavailable")), \
                patch.object(ducklake_sync, "claim") as claim:
            with self.assertRaisesRegex(RuntimeError, "schema unavailable"):
                ducklake_sync.sync(EDGES, lake_target="lake", uri="bolt://unused",
                                   database="demo", source_id="edges", workers=1)
            claim.assert_not_called()

    def test_legacy_identity_and_unrelated_keyed_types_are_allowed(self):
        for relationships in ([EDGE_SIGNATURE], [dict(EDGE_SIGNATURE, mergeKeyFields=[])],
                              [EDGE_SIGNATURE, dict(EDGE_SIGNATURE, name="OTHER", mergeKeyFields=["event_key"])]):
            with self.subTest(relationships=relationships), \
                    patch.object(ducklake_sync.GraphClient, "schema", return_value={"relationships": relationships}):
                ducklake_sync._validate_edge_identity(object(), "demo", EDGES)

    def test_missing_or_filtered_relationship_is_refused_before_mutation(self):
        for relationships in ([], [dict(EDGE_SIGNATURE, name="OTHER")],
                              [dict(EDGE_SIGNATURE, target="Other")]):
            with self.subTest(relationships=relationships), \
                    patch.object(ducklake_sync, "_driver", return_value=contextlib.nullcontext(object())), \
                    patch.object(ducklake_sync.GraphClient, "schema", return_value={"relationships": relationships}), \
                    patch.object(ducklake_sync, "claim") as claim, \
                    patch.object(ducklake_sync, "lake") as source:
                with self.assertRaisesRegex(ValueError, "not visible"):
                    ducklake_sync.sync(EDGES, lake_target="lake", uri="bolt://unused",
                                       database="demo", source_id="edges", workers=1)
                claim.assert_not_called()
                source.assert_not_called()

    def test_malformed_relationship_collection_is_refused(self):
        for schema in (None, {}, {"relationships": None}, {"relationships": {}},
                       {"relationships": [None]}, {"relationships": [{}]}):
            with self.subTest(schema=schema), \
                    patch.object(ducklake_sync.GraphClient, "schema", return_value=schema):
                with self.assertRaisesRegex(ValueError, "invalid relationship"):
                    ducklake_sync._validate_edge_identity(object(), "demo", EDGES)

    def test_node_mapping_does_not_require_schema_inspection(self):
        with patch.object(ducklake_sync.GraphClient, "schema") as schema:
            ducklake_sync._validate_edge_identity(object(), "demo", NODES)
            schema.assert_not_called()

    def test_malformed_key_metadata_is_refused(self):
        for keys in (None, "event_key", [1], [""]):
            with self.subTest(keys=keys), \
                    patch.object(ducklake_sync.GraphClient, "schema", return_value={"relationships": [
                        dict(EDGE_SIGNATURE, mergeKeyFields=keys)]}):
                with self.assertRaisesRegex(ValueError, "invalid relationship"):
                    ducklake_sync._validate_edge_identity(object(), "demo", EDGES)


class PruneBatchTest(unittest.TestCase):
    def test_failed_worker_retains_lease_and_does_not_advance_cursor(self):
        class Driver:
            def session(self, **kwargs):
                return contextlib.nullcontext(object())

        class FailedPool:
            def __init__(self, *args, **kwargs):
                pass
            def __enter__(self):
                return self
            def __exit__(self, *args):
                return False
            def map(self, *args):
                raise RuntimeError("conflict retries exhausted")

        with patch.object(ducklake_sync, "_driver", return_value=contextlib.nullcontext(Driver())), \
                patch.object(ducklake_sync, "lake", return_value=contextlib.nullcontext(object())), \
                patch.object(ducklake_sync, "claim", return_value=1), \
                patch.object(ducklake_sync, "latest_snapshot", return_value=2), \
                patch.object(ducklake_sync, "validate"), \
                patch.object(ducklake_sync, "ProcessPoolExecutor", FailedPool), \
                patch.object(ducklake_sync, "release") as release:
            with self.assertRaisesRegex(RuntimeError, "retries exhausted"):
                ducklake_sync.sync(NODES, lake_target="lake", uri="bolt://localhost:7687",
                                   database="demo", source_id="test", workers=2)
            release.assert_not_called()

    def test_prune_deletes_each_batch_and_closes_the_source_on_failure(self):
        class Session:
            def __enter__(self):
                return self

            def __exit__(self, *args):
                pass

            def run(self, query):
                return []

        class Driver:
            def session(self, **kwargs):
                return Session()

        for fail in (False, True):
            closed, seen = [], []

            def batches(*args):
                try:
                    yield [("a",), ("b",)]
                    yield [("c",)]
                    yield [("d",)]
                finally:
                    closed.append(True)

            def apply(client, mapping, upserts, deletes):
                seen.append(deletes)
                if fail and len(seen) == 2:
                    raise RuntimeError("delete failed")
                return 0, len(deletes)

            with patch.object(ducklake_sync, "lake", return_value=contextlib.nullcontext(None)), \
                    patch.object(ducklake_sync, "_stale_key_batches", side_effect=batches), \
                    patch.object(ducklake_sync, "_apply", side_effect=apply), \
                    patch.object(ducklake_sync, "_ConflictRetryClient"):
                if fail:
                    with self.assertRaisesRegex(RuntimeError, "delete failed"):
                        ducklake_sync._prune(Driver(), "demo", NODES, "lake", (), None, 1, 2)
                else:
                    self.assertEqual(ducklake_sync._prune(Driver(), "demo", NODES, "lake", (), None, 1, 2), 4)
            self.assertEqual(closed, [True])
            self.assertEqual(seen, [[("a",), ("b",)], [("c",)]] if fail else
                             [[("a",), ("b",)], [("c",)], [("d",)]])

    def test_only_confirmed_rolled_back_transaction_conflicts_retry(self):
        class Error(Exception):
            def __init__(self, code, message):
                self.code, self.message = code, message

        conflict = Error("Neo.TransientError.Transaction.Outdated",
                         "transaction conflict [brainwire.commit outcome=rolled_back]")
        client = ducklake_sync._ConflictRetryClient(None, "demo")
        with patch.object(ducklake_sync.GraphClient, "_write", side_effect=[conflict, 7]) as write, \
                patch.object(ducklake_sync.time, "sleep"):
            self.assertEqual(client._write(lambda tx: None), 7)
            self.assertEqual(write.call_count, 2)
        for error in (OSError("connection lost"),
                      Error(conflict.code, "transaction conflict"),
                      Error(conflict.code, "[brainwire.commit outcome=unknown]"),
                      Error("Neo.ClientError.Transaction.CommitOutcomeUnknown", conflict.message)):
            with patch.object(ducklake_sync.GraphClient, "_write", side_effect=error) as write, \
                    patch.object(ducklake_sync.time, "sleep"):
                with self.assertRaises(type(error)):
                    client._write(lambda tx: None)
                self.assertEqual(write.call_count, 1)
        with patch.object(ducklake_sync.GraphClient, "_write", side_effect=conflict) as write, \
                patch.object(ducklake_sync.time, "sleep"):
            with self.assertRaises(Error):
                client._write(lambda tx: None)
            self.assertEqual(write.call_count, 8)

    def test_stale_keys_are_fetched_in_bounded_batches_and_spool_is_removed(self):
        class Cursor:
            def __init__(self):
                self.rows = iter([("a",), ("b",), ("c",), ("d",), ("e",)])
                self.requests = []

            def execute(self, query):
                self.spool = Path(query.split("read_json('", 1)[1].split("'", 1)[0])
                self.assert_spool()
                return self

            def assert_spool(self):
                assert self.spool.exists()

            def fetchmany(self, size):
                self.assert_spool()
                self.requests.append(size)
                return [row for _, row in zip(range(size), self.rows)]

            def fetchall(self):
                raise AssertionError("pruning must not materialize all stale keys")

        cursor = Cursor()
        batches = list(ducklake_sync._stale_key_batches(cursor, NODES, 1, [("a",)], 2))
        self.assertEqual(batches, [[("a",), ("b",)], [("c",), ("d",)], [("e",)]])
        self.assertEqual(cursor.requests, [2, 2, 2, 2])
        self.assertFalse(cursor.spool.exists())
        cursor = Cursor()
        pending = ducklake_sync._stale_key_batches(cursor, NODES, 1, [("a",)], 2)
        self.assertEqual(next(pending), [("a",), ("b",)])
        self.assertTrue(cursor.spool.exists())
        pending.close()
        self.assertFalse(cursor.spool.exists())

    def test_spool_is_removed_when_source_keys_fail(self):
        def broken_keys():
            yield ("a",)
            raise RuntimeError("source read failed")

        created = []
        original = tempfile.NamedTemporaryFile

        def track(*args, **kwargs):
            stream = original(*args, **kwargs)
            created.append(Path(stream.name))
            return stream

        with patch.object(ducklake_sync.tempfile, "NamedTemporaryFile", side_effect=track):
            with self.assertRaisesRegex(RuntimeError, "source read failed"):
                list(ducklake_sync._stale_key_batches(None, NODES, 1, broken_keys(), 2))
        self.assertEqual(len(created), 1)
        self.assertFalse(created[0].exists())


class PlainValueTest(unittest.TestCase):
    def test_source_types_become_bolt_values(self):
        self.assertEqual(plain(datetime.date(2026, 1, 2)), "2026-01-02")
        self.assertEqual(plain(datetime.datetime(2026, 1, 2, 3, 4, 5)), "2026-01-02T03:04:05")
        self.assertEqual(plain(decimal.Decimal("12.50")), 12.5)
        self.assertEqual(plain({"tags": [decimal.Decimal("1"), None]}), {"tags": [1.0, None]})
        with self.assertRaises(ValueError):
            plain(b"raw")

    def test_mapping_rejects_unsafe_identifiers(self):
        for bad in [{"table": "main.x;drop", "label": "A", "id": "id"},
                    {"table": "main.x", "label": "A", "id": "id", "properties": ["entity_id"]},
                    {"table": "main.x", "label": "A b", "id": "id"},
                    {"table": "main.x", "label": "A", "id": "id", "type": "R"},
                    {"table": "main.x", "label": "A", "id": "id", "properties": ["change_type"]},
                    {"table": "main.x", "label": "A", "id": "rowid"},
                    {"table": "main.x", "label": "A", "id": "id", "acl": {"owner": "o"}},
                    {"table": "main.x", "type": "R", "source": {"label": "A", "column": "s"},
                     "target": {"label": "A", "column": "t"}, "acl": {"allow": "a"}}]:
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                Mapping.from_json(bad)


if __name__ == "__main__":
    unittest.main()
