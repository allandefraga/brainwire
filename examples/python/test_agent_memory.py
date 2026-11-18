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


class AgentMemoryPageTests(TestCase):
    @staticmethod
    def row(decision, source):
        from examples.python.agent_memory import PREFIX
        return {"decision_id": PREFIX + f"{decision:064x}", "title": "Retry requests",
                "body": 'Review "é" before retrying.',
                "source_id": PREFIX + f"{source:064x}", "source_title": "Release meeting",
                "source_url": "https://example.invalid/meeting/release"}

    @staticmethod
    def cursor(row):
        return {"graph": "company_demo", "project": "project",
                **{field: row[field] for field in ("decision_id", "source_id")}}

    def graph(self, records):
        graph = Mock(database="company_demo")
        calls = []
        closed = []

        @contextmanager
        def rows(query, parameters, **options):
            calls.append((query, parameters, options))
            position = (parameters.get("after_decision", ""), parameters.get("after_source", ""))
            eligible = [row for row in records
                        if (row["decision_id"], row["source_id"]) > position]
            try:
                yield iter(eligible[:parameters["limit"]])
            finally:
                closed.append(True)

        graph.stream_cypher.side_effect = rows
        return graph, calls, closed

    def test_pages_resume_after_last_emitted_pair_and_deleted_source(self):
        from examples.python.agent_memory import recall_page
        records = [self.row(1, 1), self.row(1, 2), self.row(2, 1)]
        graph, calls, closed = self.graph(records)
        first = recall_page(graph, "project", limit=1)
        self.assertEqual(first["records"], records[:1])
        self.assertTrue(first["truncated"])
        self.assertEqual(first["next_cursor"], self.cursor(records[0]))
        removed = records.pop(0)
        second = recall_page(graph, "project", limit=1, after=first["next_cursor"])
        self.assertEqual(second["records"], records[:1])
        self.assertEqual(second["next_cursor"], self.cursor(records[0]))
        final = recall_page(graph, "project", limit=1, after=second["next_cursor"])
        self.assertEqual(final["records"], records[1:])
        self.assertFalse(final["truncated"])
        self.assertIsNone(final["next_cursor"])
        query, params, _ = calls[1]
        self.assertIn("d.entity_id > $after_decision", query)
        self.assertIn("d.entity_id = $after_decision AND s.entity_id > $after_source", query)
        self.assertNotIn(removed["decision_id"], query)
        self.assertNotIn(removed["source_id"], query)
        self.assertEqual(params["after_decision"], removed["decision_id"])
        self.assertEqual(params["after_source"], removed["source_id"])
        self.assertEqual(len(closed), 3)

    def test_invalid_cursors_and_budgets_fail_before_query(self):
        from examples.python.agent_memory import PREFIX, recall_page
        graph = Mock(database="company_demo")
        good = self.cursor(self.row(1, 1))
        malformed = [[], (), {}, {**good, "extra": "x"},
                     {**good, "graph": "other"}, {**good, "project": "other"},
                     {**good, "source_id": "x"}, {**good, "source_id": PREFIX + "A" * 64},
                     {**good, "source_id": PREFIX + "é" * 64},
                     {**good, "decision_id": "' MATCH (n) DETACH DELETE n //"},
                     {**good, "decision_id": None}]
        for after in malformed:
            with self.subTest(after=after), self.assertRaises(ValueError):
                recall_page(graph, "project", after=after)
        for budget in (False, 0, 1.5, 1 << 21):
            with self.subTest(budget=budget), self.assertRaisesRegex(ValueError, "max_bytes"):
                recall_page(graph, "project", max_bytes=budget)
        graph.stream_cypher.assert_not_called()

    def test_budget_includes_cursor_and_returns_only_complete_pairs(self):
        from examples.python.agent_memory import _output_json, recall_page
        records = [self.row(1, 1), self.row(1, 2)]
        graph, _, closed = self.graph(records)
        expected = {"project": "project", "records": records[:1], "truncated": True,
                    "next_cursor": self.cursor(records[0])}
        budget = len(_output_json(expected).encode("utf-8"))
        result = recall_page(graph, "project", max_bytes=budget)
        self.assertEqual(result, expected)
        self.assertLessEqual(len(_output_json(result).encode("utf-8")), budget)
        with self.assertRaisesRegex(ValueError, "one complete memory pair and cursor"):
            recall_page(graph, "project", max_bytes=budget - 1)
        self.assertEqual(len(closed), 2)

    def test_empty_page_has_no_continuation(self):
        from examples.python.agent_memory import recall_page
        graph, _, closed = self.graph([])
        self.assertEqual(recall_page(graph, "project"), {
            "project": "project", "records": [], "truncated": False, "next_cursor": None})
        self.assertEqual(closed, [True])

    def test_out_of_order_rows_fail_and_close_stream(self):
        from examples.python.agent_memory import recall_page
        graph, _, closed = self.graph([self.row(2, 1), self.row(1, 1)])
        with self.assertRaisesRegex(ValueError, "strictly after"):
            recall_page(graph, "project")
        self.assertEqual(closed, [True])

    def test_cursor_json_rejects_duplicates_and_oversize_before_connect(self):
        from unittest.mock import patch
        from examples.python.agent_memory import CURSOR_BYTES, _cursor_json, main
        good = self.cursor(self.row(1, 1))
        self.assertEqual(_cursor_json(json.dumps(good)), good)
        for text in ('{"graph":"a","graph":"b"}', " " * (CURSOR_BYTES + 1),
                     '["not-a-cursor"]', json.dumps({**good, "project": "other"})):
            with self.subTest(cursor=text[:50]), patch('examples.python.agent_memory.connect') as connect:
                with self.assertRaises(SystemExit) as error:
                    main(["recall", "--project", "project", "--after", text])
                self.assertEqual(error.exception.code, 2)
                connect.assert_not_called()

    def test_cli_uses_scoped_cursor_and_default_page_budget(self):
        from unittest.mock import patch
        from examples.python.agent_memory import main
        cursor = self.cursor(self.row(1, 1))
        graph = Mock(database="company_demo")
        with patch('examples.python.agent_memory.connect') as connect, \
                patch('examples.python.agent_memory.recall_page', return_value={}) as page, \
                patch('examples.python.agent_memory.auth_from_env', return_value=None), \
                patch('builtins.print'):
            connect.return_value.__enter__.return_value = graph
            main(["recall", "--project", "project", "--limit", "1", "--after", json.dumps(cursor)])
        page.assert_called_once_with(graph, "project", 1, after=cursor, max_bytes=65536)
