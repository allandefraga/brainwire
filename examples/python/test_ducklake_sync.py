"""DuckLake partition reads, change collapse and validation (no server needed).

Needs duckdb with cached ducklake/sqlite extensions; skips otherwise:
    uv run --no-project --with duckdb==1.5.5 python -m unittest examples.python.test_ducklake_sync -v
"""
import datetime
import decimal
import tempfile
import unittest
from pathlib import Path

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
        return lambda *args: setattr(self, name, args) or len(args[-1])


class ApplyTest(unittest.TestCase):
    def test_deletes_pass_plain_ids_and_endpoint_rows(self):
        client = Recorder()
        self.assertEqual(_apply(client, NODES, [{"entity_id": "a"}], [("b",), ("c",)]), (1, 2))
        self.assertEqual(client.delete_nodes, ("Customer", ["b", "c"]))
        _apply(client, EDGES, [], [("a", "b")])
        self.assertEqual(client.delete_edges, ("Customer", "FOLLOWS", "Customer",
                                               [{"source_id": "a", "target_id": "b"}]))


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
                    {"table": "main.x", "label": "A", "id": "rowid"}]:
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                Mapping.from_json(bad)


if __name__ == "__main__":
    unittest.main()
