import json
from contextlib import contextmanager
from unittest import TestCase
from unittest.mock import Mock

from examples.python.agent_memory import recall, remember


class AgentMemoryTests(TestCase):
    def test_invalid_write_fields_fail_before_query(self):
        graph = Mock()
        arguments = ["project", "title", "body", "source", "https://example.com"]
        for index, malformed in ((0, " "), (1, "á" * 129), (2, "x" * 2049),
                                 (3, None), (4, "x" * 1025)):
            with self.subTest(field=index):
                values = arguments[:]
                values[index] = malformed
                with self.assertRaises(ValueError):
                    remember(graph, *values)
        graph.run_cypher.assert_not_called()

    def test_invalid_allow_list_fails_before_opening_transaction(self):
        graph = Mock()
        for allow in ([], "reader", [None], [" "], ["á" * 129], ["reader"] * 5001):
            with self.assertRaises(ValueError):
                remember(graph, "project", "Title", "Body", "Source", "https://example.com", allow=allow)
        graph.driver.session.assert_not_called()
        graph.run_cypher.assert_not_called()

    def test_invalid_read_limits_fail_before_query(self):
        graph = Mock()
        for limit in (False, 0, 11, 1.5):
            with self.subTest(limit=limit), self.assertRaisesRegex(ValueError, "limit"):
                recall(graph, "project", limit)
        graph.stream_cypher.assert_not_called()

    def test_read_budget_returns_only_complete_pairs(self):
        graph = Mock()
        row = {"decision_id": "d", "title": "title", "body": "é" * 20 + ' "quoted"',
               "source_id": "s", "source_title": "review", "source_url": "https://example.invalid"}
        second = {**row, "decision_id": "d2"}
        expected = {"project": "project", "records": [row], "truncated": False}
        budget = len((json.dumps(expected, indent=2, ensure_ascii=False) + "\n").encode("utf-8"))
        opened = []

        @contextmanager
        def rows(*args, **kwargs):
            opened.append(True)
            try:
                yield iter([row, second])
            finally:
                opened.pop()

        graph.stream_cypher.side_effect = rows
        self.assertEqual(recall(graph, "project", max_bytes=budget),
                         {**expected, "truncated": True})
        self.assertEqual(recall(graph, "project", max_bytes=budget - 1),
                         {**expected, "records": [], "truncated": True})
        self.assertEqual(opened, [])

    def test_small_budget_fails_before_query(self):
        graph = Mock()
        for max_bytes in (False, 0, 1, 1.5):
            with self.subTest(max_bytes=max_bytes), self.assertRaisesRegex(ValueError, "max_bytes"):
                recall(graph, "project", max_bytes=max_bytes)
        graph.stream_cypher.assert_not_called()

    def test_out_of_contract_record_fails_and_closes_read(self):
        graph = Mock()
        closed = []

        @contextmanager
        def rows(*args, **kwargs):
            try:
                yield iter([{"decision_id": "d", "title": "title", "body": "á" * 1025}])
            finally:
                closed.append(True)

        graph.stream_cypher.side_effect = rows
        with self.assertRaisesRegex(ValueError, "body"):
            recall(graph, "project")
        self.assertEqual(closed, [True])

    def test_write_keeps_instructions_in_parameters(self):
        graph = Mock()
        graph.run_cypher.return_value = [{"decision_id": "d", "source_id": "s"}]
        text = "' MATCH (n) DETACH DELETE n //"
        remember(graph, "project", text, text, "source", "https://example.com")
        statement, parameters = graph.run_cypher.call_args.args
        self.assertNotIn(text, statement)
        self.assertEqual((parameters["title"], parameters["body"]), (text, text))
