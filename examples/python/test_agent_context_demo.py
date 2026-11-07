import io
import unittest
from contextlib import contextmanager, redirect_stdout
from unittest.mock import patch

from examples.python.agent_context_demo import main, read_demo


class ReadOnlyDemoGraph:
    database = "demo"

    def __init__(self, visible=True, connected=True):
        self.visible, self.connected = visible, connected
        self.queries = []

    def schema(self):
        return {"version": 1, "indexesReady": True, "nodes": [
            {"label": "Customer", "properties": ["name"]},
            {"label": "Segment", "properties": ["name"]}],
            "relationships": [{"name": "IN_SEGMENT", "source": "Customer",
                               "target": "Segment", "properties": []}]}

    @contextmanager
    def stream_cypher(self, query, parameters, *, fetch_size):
        self.queries.append(query)
        if "MATCH p =" in query:
            rows = [{"nodes": [{"label": "Customer", "entity_id": "customer-1"},
                               {"label": "Segment", "entity_id": "segment-1"}],
                     "relationships": ["IN_SEGMENT"]}] if self.visible and self.connected else []
        else:
            rows = [{"entity_id": "customer-1", "p0": "Ada"}] if self.visible else []
        yield iter(rows)


class AgentContextDemoTests(unittest.TestCase):
    def test_real_helpers_return_distinct_schema_and_record_evidence(self):
        graph = ReadOnlyDemoGraph()
        result = read_demo(graph)
        self.assertEqual(result["status"], "ok")
        self.assertTrue(result["schema_routes"]["schema_only"])
        self.assertFalse(result["stored_connections"]["schema_only"])
        self.assertEqual(result["candidate_context"]["records"][0]["properties"], {"name": "Ada"})
        self.assertEqual(result["candidate_context"]["providers"], ["fixed_demo_id"])
        self.assertEqual(len(graph.queries), 2)
        self.assertTrue(all(query.startswith("MATCH ") for query in graph.queries))

    def test_missing_or_hidden_data_does_not_become_a_schema_based_success(self):
        for visible, connected in ((False, True), (True, False)):
            with self.subTest(visible=visible, connected=connected):
                result = read_demo(ReadOnlyDemoGraph(visible, connected))
                self.assertEqual(result["status"], "demo_data_unavailable")
                self.assertTrue(result["schema_routes"]["paths"])
                self.assertIn("README quickstart", result["message"])

    def test_cli_closes_connection_and_reports_missing_demo_with_nonzero_exit(self):
        graph = ReadOnlyDemoGraph(False)
        closed = []

        @contextmanager
        def connection(*args, **kwargs):
            try:
                yield graph
            finally:
                closed.append(True)

        with patch("examples.python.agent_context_demo.connect", side_effect=connection) as connect, \
                patch("examples.python.agent_context_demo.auth_from_env", return_value=("reader", "token")), \
                redirect_stdout(io.StringIO()) as output:
            self.assertEqual(main(["--uri", "bolt://localhost:17687"]), 1)
        connect.assert_called_once_with("bolt://localhost:17687", "demo",
                                        auth=("reader", "token"), connection_timeout=5)
        self.assertEqual(closed, [True])
        self.assertIn('"status": "demo_data_unavailable"', output.getvalue())


if __name__ == "__main__":
    unittest.main()
