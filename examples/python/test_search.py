import unittest
from contextlib import contextmanager
from unittest.mock import patch
import re

from brainwire import ContextLimitError, ContextProviderError, search_context


class Graph:
    database = "workspace"

    def __init__(self, rows=()):
        self.rows = rows
        self.calls = []
        self.closed = False

    @contextmanager
    def stream_cypher(self, query, parameters, *, fetch_size):
        self.calls.append((query, parameters, fetch_size))
        fields = re.findall(r"n\.(\w+) AS (p\d+)", query)
        rows = [{"entity_id": row["id"], **{alias: row.get(field) for field, alias in fields}}
                for row in self.rows if row["id"] in parameters["ids"]]
        try:
            yield iter(rows)
        finally:
            self.closed = True


def provider(ids):
    def invoke(text, *, graph_name, label, limit, timeout):
        assert text == "release blocker"
        assert graph_name == "workspace" and label == "Document"
        assert limit == 100 and 0 < timeout <= 10
        return ids
    return invoke


class SearchTests(unittest.TestCase):
    def test_collects_provider_candidates_reads_once_and_returns_ranked_sources(self):
        graph = Graph([{"id": "b", "title": "Second", "url": "source://b"},
                       {"id": "a", "title": "First", "url": "source://a"}])
        result = search_context(graph, "Document", "release blocker",
            providers={"lexical": provider(["a", "b", "a"]),
                       "vector": provider(["b", "a"])},
            source_fields={"url": "url"}, properties=["title"], limit=1)
        self.assertEqual(len(graph.calls), 1)
        self.assertEqual(graph.calls[0][1]["ids"], ["a", "b"])
        self.assertEqual(result["records"][0]["entity_id"], "a")
        self.assertEqual(result["records"][0]["properties"], {"title": "First"})
        self.assertEqual(result["records"][0]["matches"],
                         [{"provider": "lexical", "rank": 1}, {"provider": "vector", "rank": 2}])
        self.assertAlmostEqual(result["records"][0]["score"], 1 / 61 + 1 / 62)
        self.assertEqual(len(result["sources"]), 1)
        self.assertEqual(result["sources"][0]["record"]["entity_id"], "a")
        self.assertTrue(graph.closed)

    def test_hidden_candidates_cannot_change_scores_ranks_or_sources(self):
        graph = Graph([{"id": "a", "title": "Visible", "url": "source://a"}])
        options = {"source_fields": {"url": "url"}, "properties": ["title"]}
        baseline = search_context(graph, "Document", "release blocker",
            providers={"vector": provider(["a"])}, **options)
        hidden = search_context(graph, "Document", "release blocker",
            providers={"vector": provider(["secret", "missing", "a"])}, **options)
        self.assertEqual(hidden, baseline)
        self.assertNotIn("secret", str(hidden))

    def test_provider_failure_or_invalid_input_never_reads_the_graph(self):
        graph = Graph()
        def failure(*args, **kwargs):
            raise RuntimeError("provider unavailable")
        with self.assertRaises(ContextProviderError):
            search_context(graph, "Document", "release blocker", providers={"vector": failure})
        for options in [
            {"providers": {}}, {"providers": {"vector": []}},
            {"providers": {"x" * 33: provider([])}},
            {"providers": {"vector": provider([])}, "limit": True},
            {"providers": {"vector": provider([])}, "timeout": 0},
            {"providers": {"vector": provider([])}, "timeout": float("nan")},
            {"providers": {"vector": provider([])}, "properties": ["acl_allow"]},
            {"providers": {"vector": provider([])}, "source_fields": {"secret": "url"}},
        ]:
            with self.subTest(options=options):
                with self.assertRaises(ValueError):
                    search_context(graph, "Document", "release blocker", **options)
        self.assertEqual(graph.calls, [])

    def test_provider_exceptions_do_not_expose_private_messages(self):
        graph = Graph()
        def failure(*args, **kwargs):
            raise ValueError("secret-document-id")
        with self.assertRaises(ContextProviderError) as caught:
            search_context(graph, "Document", "release blocker", providers={"vector": failure})
        self.assertNotIn("secret-document-id", str(caught.exception))
        self.assertEqual(graph.calls, [])

    def test_rejects_lazy_or_oversized_provider_output_without_consuming_it(self):
        graph = Graph()
        consumed = []
        def infinite(*args, **kwargs):
            while True:
                consumed.append(True)
                yield "same"
        with self.assertRaises(ValueError):
            search_context(graph, "Document", "release blocker", providers={"vector": infinite})
        self.assertEqual(consumed, [])
        with self.assertRaises(ContextLimitError):
            search_context(graph, "Document", "release blocker", providers={"vector": provider(["same"] * 101)})
        self.assertEqual(graph.calls, [])

    def test_candidate_union_and_id_bytes_are_bounded_before_reading(self):
        graph = Graph()
        with self.assertRaises(ContextLimitError):
            search_context(graph, "Document", "release blocker", providers={
                "a": provider([str(i) for i in range(100)]), "b": provider(["extra"])})
        with self.assertRaises(ContextLimitError):
            search_context(graph, "Document", "release blocker", providers={
                "a": provider([str(i) + "x" * 4090 for i in range(65)])})
        self.assertEqual(graph.calls, [])

    def test_timeout_discards_completed_provider_result_without_querying(self):
        graph = Graph()
        with patch("brainwire.search.time.monotonic", side_effect=[0, 0, 11]):
            with self.assertRaises(TimeoutError):
                search_context(graph, "Document", "release blocker",
                               providers={"vector": provider(["a"])})
        self.assertEqual(graph.calls, [])

    def test_empty_provider_results_do_not_query_and_max_bytes_never_returns_partial(self):
        graph = Graph()
        result = search_context(graph, "Document", "release blocker",
                                providers={"vector": provider([])})
        self.assertEqual(result["records"], [])
        self.assertEqual(graph.calls, [])
        graph = Graph([{"id": "a", "body": "x" * 1000}])
        with self.assertRaises(ContextLimitError):
            search_context(graph, "Document", "release blocker",
                           providers={"vector": provider(["a"])}, properties=["body"], max_bytes=500)
        self.assertTrue(graph.closed)

    def test_invalid_ids_provider_shapes_and_fields_fail_before_reading(self):
        graph = Graph()
        for ids in ["one-id", {"a": 1}, [None], [True], [""], ["x" * 4097]]:
            with self.subTest(ids_type=type(ids).__name__):
                with self.assertRaises(ValueError):
                    search_context(graph, "Document", "release blocker",
                                   providers={"vector": provider(ids)})
        for text in ["", " ", "é" * 513]:
            with self.assertRaises(ValueError):
                search_context(graph, "Document", text, providers={"vector": provider([])})
        with self.assertRaises(ValueError):
            search_context(graph, "Document", "release blocker", providers={"vector": provider([])},
                           source_fields={"title": "title", "url": "title"})
        self.assertEqual(graph.calls, [])

    def test_output_budget_includes_utf8_sources_and_matches_at_exact_boundary(self):
        graph = Graph([{"id": "a", "title": "é" * 100, "url": "source://a"}])
        options = {"providers": {"vector": provider(["a"])}, "properties": ["title"],
                   "source_fields": {"url": "url"}}
        expected = search_context(graph, "Document", "release blocker", **options)
        import json
        size = len(json.dumps(expected, ensure_ascii=False, allow_nan=False).encode("utf-8"))
        self.assertEqual(search_context(graph, "Document", "release blocker", max_bytes=size, **options), expected)
        with self.assertRaises(ContextLimitError):
            search_context(graph, "Document", "release blocker", max_bytes=size - 1, **options)

    def test_timeout_after_authorized_read_closes_and_returns_no_partial_result(self):
        graph = Graph([{"id": "a"}])
        clock = [0.0]
        original = graph.stream_cypher
        @contextmanager
        def slow_read(*args, **kwargs):
            with original(*args, **kwargs) as rows:
                yield rows
                clock[0] = 11
        graph.stream_cypher = slow_read
        with patch("brainwire.search.time.monotonic", side_effect=lambda: clock[0]):
            with self.assertRaises(TimeoutError):
                search_context(graph, "Document", "release blocker", providers={"vector": provider(["a"])})
        self.assertTrue(graph.closed)

    def test_graph_failure_propagates_without_fallback_to_provider_content(self):
        graph = Graph()
        @contextmanager
        def failed_read(*args, **kwargs):
            raise RuntimeError("graph unavailable")
            yield
        graph.stream_cypher = failed_read
        with self.assertRaisesRegex(RuntimeError, "graph unavailable"):
            search_context(graph, "Document", "release blocker", providers={"vector": provider(["a"])})


if __name__ == "__main__":
    unittest.main()
