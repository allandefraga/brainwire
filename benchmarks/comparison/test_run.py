import importlib.util
import json
import os
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("brainwire_comparison", Path(__file__).with_name("run.py"))
comparison = importlib.util.module_from_spec(spec)
spec.loader.exec_module(comparison)


class FakeClient:
    def __init__(self, reported_counts):
        self.reported_counts = iter(reported_counts)
        self.submitted = []

    def run(self, query, **params):
        self.submitted.append(params["rows"])
        return [[next(self.reported_counts)]]


class WorkloadTests(unittest.TestCase):
    def test_bounded_batches_count_partial_tail(self):
        client = FakeClient([2, 2, 1])
        result = comparison.write_batches(client, comparison.NODE_CREATE, comparison.node_rows(5), 2)
        self.assertEqual(result["records"], 5)
        self.assertEqual([len(batch) for batch in client.submitted], [2, 2, 1])
        self.assertEqual([batch[0]["key"] for batch in client.submitted],
                         ["n0000000", "n0000002", "n0000004"])

    def test_reject_partial_write(self):
        client = FakeClient([1])
        with self.assertRaisesRegex(RuntimeError, "batch row count"):
            comparison.write_batches(client, comparison.NODE_CREATE, comparison.node_rows(5), 2)
        self.assertEqual(len(client.submitted), 1)

    def test_fixture_has_two_unique_edges_per_node_and_wraps(self):
        rows = list(comparison.edge_rows(100))
        self.assertEqual(len(rows), 200)
        self.assertEqual(len({(r["src"], r["dst"]) for r in rows}), 200)
        self.assertEqual(rows[-2:], [{"src": "n0000099", "dst": "n0000000"},
                                     {"src": "n0000099", "dst": "n0000016"}])

    def test_docker_context_pins_inspected_unix_endpoint(self):
        with patch.dict(os.environ, {"DOCKER_CONTEXT": "old", "DOCKER_HOST": "ssh://old"}), \
             patch.object(comparison, "docker", return_value=json.dumps("unix:///tmp/test.sock")) as docker:
            comparison.use_local_context("colima")
            self.assertEqual(os.environ["DOCKER_HOST"], "unix:///tmp/test.sock")
            self.assertNotIn("DOCKER_CONTEXT", os.environ)
            docker.assert_called_once_with(
                "context", "inspect", "--format", "{{json .Endpoints.docker.Host}}", "--", "colima")

    def test_remote_or_invalid_docker_endpoint_rejected(self):
        for endpoint in ("ssh://remote", "tcp://127.0.0.1:2375", "unix://relative", "unix:///", None):
            with self.subTest(endpoint=endpoint), \
                 patch.dict(os.environ, {"DOCKER_CONTEXT": "old", "DOCKER_HOST": "ssh://old"}), \
                 patch.object(comparison, "docker", return_value=json.dumps(endpoint)) as docker:
                with self.assertRaisesRegex(ValueError, "local Unix socket"):
                    comparison.use_local_context("remote")
                self.assertEqual(os.environ["DOCKER_CONTEXT"], "old")
                self.assertEqual(os.environ["DOCKER_HOST"], "ssh://old")
                self.assertEqual(docker.call_count, 1)

    def test_config_comparison_ignores_module_enumeration_order(self):
        graph = {"name": "graph", "ver": 60000}
        vectorset = {"name": "vectorset", "ver": 1}
        configurations = [{"modules": [graph, vectorset]}, {"modules": [vectorset, graph]}]
        with patch.object(comparison, "port", return_value=6379), \
             patch.object(comparison, "wait_ready"), \
             patch.object(comparison, "Client"), \
             patch.object(comparison, "falkor_configuration", side_effect=configurations):
            _, before = comparison.attach("falkordb", "fixture-id", 1)
            _, after = comparison.attach("falkordb", "fixture-id", 1)
        self.assertEqual(before, after)

    def test_hop_verification_rejects_duplicate_rows(self):
        class DuplicateClient:
            def run(self, query, **params):
                if query == comparison.POINT:
                    return [["v0000"]]
                return [["n0000001"], ["n0000001"], ["n0000017"]]

        with self.assertRaisesRegex(RuntimeError, "one hop"):
            comparison.check_reads(DuplicateClient(), 0, 100)


if __name__ == "__main__":
    unittest.main()
