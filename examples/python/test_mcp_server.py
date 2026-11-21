import asyncio
from contextlib import contextmanager
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

from brainwire.mcp_server import _auth, _read, _transport, create_server, main
from brainwire.context import ContextLimitError


class MCPConfigurationTests(unittest.TestCase):
    def test_private_auth_file_and_invalid_content(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "credentials.json"
            path.write_text(json.dumps({"principal": "synthetic-reader", "credentials": "a" * 64}))
            path.chmod(0o600)
            self.assertEqual(_auth(path), ("synthetic-reader", "a" * 64))
            for content in ('{}', '{"principal":"a","principal":"b","credentials":"' + 'a' * 64 + '"}',
                            '"secret"', 'x' * 65537):
                path.write_text(content)
                with self.assertRaisesRegex(ValueError, "cannot load a private"):
                    _auth(path)
            path.write_text(json.dumps({"principal": "reader", "credentials": "a" * 64}))
            if os.name == "posix":
                path.chmod(0o644)
                with self.assertRaisesRegex(ValueError, "cannot load a private"):
                    _auth(path)

    def test_remote_plaintext_and_embedded_credentials_are_rejected(self):
        for uri in ("bolt://example.invalid", "bolt://10.0.0.1", "bolt://user:secret@localhost",
                    "https://localhost", "bolt://localhost/path", "bolt://localhost:0"):
            with self.subTest(uri=uri), self.assertRaises(ValueError):
                _transport(uri, None)
        self.assertEqual(_transport("bolt://127.0.0.1:7687", None), ("bolt://127.0.0.1:7687", {}))
        self.assertEqual(_transport("bolt://localhost", None), ("bolt://127.0.0.1:7687", {}))
        self.assertEqual(_transport("bolt://localhost:17687/", None), ("bolt://127.0.0.1:17687", {}))
        self.assertEqual(_transport("bolt://[::1]", None), ("bolt://[::1]:7687", {}))
        self.assertEqual(_transport("bolt://[0:0:0:0:0:0:0:1]:17687", None), ("bolt://[::1]:17687", {}))
        self.assertEqual(_transport("bolt+s://example.invalid:7687", None), ("bolt+s://example.invalid:7687", {}))

    def test_errors_never_return_exception_text(self):
        for error, code in ((RuntimeError("token=private"), "read_failed"),
                            (ValueError("private query"), "invalid_request"),
                            (ContextLimitError("private record"), "context_limit"),
                            (TimeoutError("private host"), "temporarily_unavailable")):
            result = _read(Mock(side_effect=error))
            self.assertEqual(result["error"]["code"], code)
            self.assertNotIn("private", json.dumps(result))
        error = RuntimeError("token=private")
        error.code = "Neo.ClientError.Security.Forbidden"
        self.assertEqual(_read(Mock(side_effect=error)),
                         {"ok": False, "error": {"code": "access_denied", "retryable": False}})


@unittest.skipUnless(importlib.util.find_spec("mcp"), "optional MCP SDK is not installed")
class MCPToolsTests(unittest.IsolatedAsyncioTestCase):
    async def test_tools_expose_only_fixed_graph_reads(self):
        graph = Mock(database="synthetic")

        @contextmanager
        def stream(query, parameters, **kwargs):
            self.assertNotIn("DELETE", query)
            yield iter([{"entity_id": "decision-1", "p0": "Retry requests"}])

        graph.stream_cypher.side_effect = stream
        server = create_server(graph)
        tools = await server.list_tools()
        self.assertEqual({tool.name for tool in tools},
                         {"brainwire_nodes", "brainwire_search_text", "brainwire_neighbors", "brainwire_evidence"})
        for tool in tools:
            self.assertTrue(tool.annotations.read_only_hint)
            self.assertFalse(tool.annotations.destructive_hint)
            self.assertTrue(tool.annotations.idempotent_hint)
            self.assertFalse(set(tool.input_schema["properties"]) & {"uri", "auth", "database", "cypher", "query"})
        response = await server.call_tool("brainwire_nodes", {
            "label": "Decision", "entity_ids": ["decision-1"], "properties": ["title"],
        })
        value = json.loads(response.content[0].text)
        self.assertEqual(value["context"]["graph"], "synthetic")
        self.assertEqual(value["context"]["records"][0]["properties"], {"title": "Retry requests"})
        graph.run_cypher.assert_not_called()
        graph.driver.assert_not_called()

    async def test_search_uses_explicit_candidates_and_parameterized_text(self):
        graph = Mock(database="synthetic")

        @contextmanager
        def stream(query, parameters, **kwargs):
            self.assertIn("n.entity_id IN $ids", query)
            self.assertIn("toLower(n.title) CONTAINS toLower($text)", query)
            self.assertNotIn("retry' OR true", query)
            self.assertEqual(parameters, {"ids": ["decision-1"], "text": "retry' OR true", "limit": 20})
            self.assertEqual(kwargs, {"fetch_size": 20})
            yield iter([{"entity_id": "decision-1", "p0": "Retry requests"}])

        graph.stream_cypher.side_effect = stream
        response = await create_server(graph).call_tool("brainwire_search_text", {
            "label": "Decision", "text": "retry' OR true", "field": "title",
            "entity_ids": ["decision-1"], "properties": ["title"],
        })
        payload = json.loads(response.content[0].text)
        self.assertTrue(payload["ok"])
        self.assertEqual(payload["context"]["search"], {"field": "title", "mode": "case_insensitive_substring"})
        self.assertEqual(payload["context"]["records"][0]["properties"], {"title": "Retry requests"})
        graph.stream_cypher.assert_called_once()

    async def test_search_rejects_invalid_inputs_and_empty_candidates_without_graph_access(self):
        graph = Mock(database="synthetic")
        server = create_server(graph)
        base = {"label": "Decision", "text": "retry", "field": "title", "entity_ids": ["decision-1"]}
        for change in ({"text": " "}, {"text": "é" * 2049}, {"field": "title) DELETE n"},
                       {"entity_ids": ["x"] * 101}, {"limit": 101},
                       {"max_bytes": 1048577}, {"properties": ["title"] * 17}):
            with self.subTest(change=change):
                response = await server.call_tool("brainwire_search_text", {**base, **change})
                self.assertEqual(json.loads(response.content[0].text), {
                    "ok": False, "error": {"code": "invalid_request", "retryable": False},
                })
        response = await server.call_tool("brainwire_search_text", {**base, "entity_ids": []})
        self.assertEqual(json.loads(response.content[0].text)["context"]["records"], [])
        graph.stream_cypher.assert_not_called()

    async def test_input_bounds_fail_without_graph_access(self):
        graph = Mock(database="synthetic")
        server = create_server(graph)
        for values in ({"limit": 101}, {"max_bytes": 1048577}, {"label": "Doc) DELETE n"},
                       {"entity_ids": ["x"] * 101}, {"properties": ["name"] * 17}):
            arguments = {"label": "Doc", "entity_ids": ["x"], **values}
            result = await server.call_tool("brainwire_nodes", arguments)
            self.assertFalse(json.loads(result.content[0].text)["ok"])
        graph.stream_cypher.assert_not_called()

    async def test_oversized_context_closes_the_stream_without_returning_data(self):
        tools = {
            "brainwire_nodes": {"label": "Doc", "entity_ids": ["first-record", "second-record"]},
            "brainwire_search_text": {"label": "Doc", "text": "private", "field": "title",
                                       "entity_ids": ["first-record", "second-record"]},
            "brainwire_neighbors": {"source_label": "Project", "source_id": "project",
                                    "relationship": "HAS_DOC", "target_label": "Doc"},
            "brainwire_evidence": {"label": "Doc", "entity_ids": ["first-record", "second-record"],
                                   "source_fields": {"url": "url"}},
        }
        for tool, arguments in tools.items():
            with self.subTest(tool=tool):
                graph = Mock(database="synthetic")
                consumed, closed = [], []

                def rows():
                    for identity, title in (("first-record", "first-private-value"),
                                            ("second-record", "second-private-value" * 150)):
                        consumed.append(identity)
                        yield {"entity_id": identity, "p0": title, "p1": "https://example.invalid/source"}

                @contextmanager
                def stream(*args, **kwargs):
                    try:
                        yield rows()
                    finally:
                        closed.append(True)

                graph.stream_cypher.side_effect = stream
                response = await create_server(graph).call_tool(tool, {
                    **arguments, "properties": ["title"], "max_bytes": 700,
                })
                value = json.loads(response.content[0].text)
                self.assertEqual(value, {"ok": False, "error": {"code": "context_limit", "retryable": False}})
                self.assertEqual(consumed, ["first-record", "second-record"])
                self.assertEqual(closed, [True])

    async def test_protocol_stdio_round_trip(self):
        from mcp import ClientSession
        from mcp.client.stdio import StdioServerParameters, stdio_client

        program = '''from contextlib import contextmanager
import sys
from brainwire.mcp_server import create_server
class Graph:
    database = "synthetic"
    @contextmanager
    def stream_cypher(self, query, parameters, **kwargs):
        print("SYNTHETIC_READ", file=sys.stderr)
        yield iter([{"entity_id":"decision-1","p0":"Synthetic title"}])
create_server(Graph()).run()
'''
        parameters = StdioServerParameters(command=sys.executable, args=["-c", program],
                                          cwd=Path(__file__).resolve().parents[2])
        with tempfile.TemporaryFile(mode="w+") as errors:
            async with stdio_client(parameters, errlog=errors) as (read, write):
                async with ClientSession(read, write, read_timeout_seconds=10) as client:
                    await client.initialize()
                    listed = await client.list_tools()
                    self.assertEqual(len(listed.tools), 4)
                    result = await client.call_tool("brainwire_nodes", {
                        "label": "Decision", "entity_ids": ["decision-1"], "properties": ["title"],
                    })
                    self.assertTrue(json.loads(result.content[0].text)["ok"])
                    invalid = await client.call_tool("brainwire_nodes", {
                        "label": "Decision", "entity_ids": "synthetic-invalid-input",
                    })
                    # SDK argument errors can echo caller input; they never reach the graph.
                    self.assertTrue(invalid.is_error)
            errors.seek(0)
            messages = errors.read()
            self.assertNotIn("Traceback", messages)
            self.assertEqual(messages.count("SYNTHETIC_READ"), 1)

    def test_cli_closes_connection_and_sanitizes_session_failure(self):
        closed = []

        @contextmanager
        def connection(uri, *args, **kwargs):
            self.assertEqual(uri, "bolt://127.0.0.1:7687")
            try:
                yield Mock(database="synthetic")
            finally:
                closed.append(True)

        with patch("brainwire.mcp_server._auth", return_value=("reader", "a" * 64)), \
                patch("brainwire.mcp_server.connect", side_effect=connection), \
                patch("brainwire.mcp_server.create_server", side_effect=RuntimeError("private token")), \
                patch("sys.stderr") as stderr:
            with self.assertRaises(SystemExit):
                main(["--uri", "bolt://localhost", "--database", "synthetic", "--auth-file", "synthetic.json"])
            self.assertNotIn("private token", "".join(call.args[0] for call in stderr.write.call_args_list))
        self.assertEqual(closed, [True])


if __name__ == "__main__":
    unittest.main()
