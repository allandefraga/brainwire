from contextlib import contextmanager
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from examples.semantic_metrics.metrics import MAX_CATALOG_BYTES, main, read_catalog, run_metric


def catalog(aggregation="count", property_name=None):
    spec = {"label": "Decision", "aggregation": aggregation, "dimensions": {"title": "string"}}
    if property_name is not None:
        spec["property"] = property_name
    return {"version": 1, "metrics": {"decisions": spec}}


class Graph:
    database = "company_demo"

    def __init__(self, rows=()):
        self.rows = rows
        self.calls = []

    def schema(self):
        self.calls.append("schema")
        return {"version": 12, "nodes": [{"label": "Decision", "properties": ["title", "hours"]}]}

    @contextmanager
    def stream_cypher(self, query, parameters=None, *, fetch_size):
        self.calls.append((query, parameters, fetch_size))
        yield iter(self.rows)


class MetricTests(unittest.TestCase):
    def test_grouped_query_reads_declared_dimension_and_preserves_null(self):
        rows = [{"dimension": "a", "value": 3}, {"dimension": None, "value": 1}]
        graph = Graph(rows)
        result = run_metric(graph, catalog(), "decisions", group_by="title", max_groups=2)
        self.assertEqual(result["rows"], rows)
        self.assertEqual(result["validated_catalog_version"], 12)
        self.assertEqual(graph.calls, ["schema", (
            "MATCH (n:`Decision`) RETURN n.`title` AS dimension, count(*) AS value "
            "ORDER BY dimension LIMIT $limit", {"limit": 3}, 3,
        )])

    def test_scalar_average_keeps_null(self):
        graph = Graph([{"value": None}])
        result = run_metric(graph, catalog("avg", "hours"), "decisions")
        self.assertEqual(result["rows"], [{"value": None}])
        self.assertEqual(graph.calls[1][0], "MATCH (n:`Decision`) RETURN avg(n.`hours`) AS value")

    def test_rejects_invalid_or_unknown_identifiers_before_querying(self):
        for field, value in (("label", "Decision`) DELETE n //"), ("aggregation", "max"),
                             ("dimensions", {"acl_allow": "string"}), ("dimensions", {"title": []}),
                             ("property", "hours")):
            with self.subTest(field=field, value=value):
                model, graph = catalog(), Graph()
                model["metrics"]["decisions"][field] = value
                with self.assertRaises(ValueError):
                    run_metric(graph, model, "decisions")
                self.assertEqual(graph.calls, [])
        for model, name, group in ((catalog(), "unknown", None), (catalog(), "decisions", "hours"),
                                    (catalog("sum", "missing"), "decisions", None)):
            with self.subTest(name=name, group=group):
                graph = Graph()
                with self.assertRaises(ValueError):
                    run_metric(graph, model, name, group_by=group)
                self.assertTrue(all(call == "schema" for call in graph.calls))

    def test_rejects_excess_groups_and_type_mismatches(self):
        for rows in ([{"dimension": "a", "value": 1}, {"dimension": "b", "value": 1}],
                     [{"dimension": 10, "value": 1}], [{"dimension": "a", "value": True}]):
            with self.subTest(rows=rows), self.assertRaises(ValueError):
                run_metric(Graph(rows), catalog(), "decisions", group_by="title", max_groups=1)

    def test_rejects_duplicate_and_oversized_catalog_files(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "metrics.json"
            for data in (b'{"version": 1, "version": 1, "metrics": {}}', b" " * (MAX_CATALOG_BYTES + 1)):
                path.write_bytes(data)
                with self.subTest(size=len(data)), self.assertRaises(ValueError):
                    read_catalog(path)

    def test_requires_verified_tls_on_non_loopback_endpoints(self):
        with patch("examples.semantic_metrics.metrics.auth_from_env") as auth, \
                patch("examples.semantic_metrics.metrics.connect") as connect, \
                patch("sys.stderr"):
            with self.assertRaises(SystemExit):
                main(["query", "decisions", "--uri", "bolt://192.0.2.1:7687"])
            auth.assert_not_called()
            connect.assert_not_called()


if __name__ == "__main__":
    unittest.main()
