import argparse
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from unittest.mock import MagicMock

from scripts import bench_keyed_hub as bench


class KeyedHubTests(unittest.TestCase):
    @staticmethod
    def query_type(statement, timeout):
        return statement

    def arguments(self, output="receipt.json", *extra):
        return bench.parser().parse_args(["--image", "local:test", "--output", output, *extra])

    def test_phase_ranges_preserve_replay_and_cover_odd_final_row(self):
        self.assertEqual(bench.phases(5), [
            ("initial_half", 0, 2, 2), ("replay_half", 0, 2, 2),
            ("append_half", 2, 5, 5), ("replay_all", 0, 5, 5),
        ])
        self.assertEqual(bench.batches(2, 5, 2), [(2, 4), (4, 5)])

    def test_rows_have_unique_stable_keys_and_exact_variable_payload(self):
        rows = bench.write_rows(0, 29, 7)
        self.assertEqual(len({r["event_key"] for r in rows}), 29)
        self.assertEqual(rows, bench.write_rows(0, 29, 7))
        self.assertEqual(rows[0]["payload"], "a" * 7)
        self.assertEqual(rows[26]["payload"], "a" * 7)
        self.assertEqual(rows[27]["payload"], "b" * 7)
        self.assertEqual(bench.payload(7, 0), "")

    def test_extra_catalog_labels_add_no_fixture_nodes_or_relationship_types(self):
        baseline = bench.schema(0)
        wide = bench.schema(100)
        self.assertEqual(len(baseline["nodes"]), 1)
        self.assertEqual(len(wide["nodes"]), 101)
        self.assertEqual(len({node["label"] for node in wide["nodes"]}), 101)
        self.assertEqual(wide["edges"], baseline["edges"])
        self.assertEqual(wide["nodes"][0], {"label": "Ticket"})
        self.assertTrue(all(node["label"].startswith("Unused") for node in wide["nodes"][1:]))
        self.assertEqual(self.arguments().extra_labels, 0)
        self.assertEqual(self.arguments("unused.json", "--extra-labels", "100").extra_labels, 100)

    def test_streamed_verification_checks_count_order_keys_and_payload(self):
        rows = bench.write_rows(0, 3, 10)
        result = bench.validate_rows(iter(rows), 3, 10)
        self.assertEqual(result["count"], 3)
        self.assertEqual(result, bench.validate_rows(iter(rows), 3, 10))
        bad_rows = [rows[:-1], rows + [rows[-1]], list(reversed(rows))]
        for field, value in [("event_key", "wrong"), ("ordinal", 7), ("payload", "x" * 10)]:
            changed = [dict(row) for row in rows]
            changed[1][field] = value
            bad_rows.append(changed)
        for bad in bad_rows:
            with self.subTest(bad=bad), self.assertRaises(AssertionError):
                bench.validate_rows(iter(bad), 3, 10)

    def test_payload_limits_fail_before_docker(self):
        args = self.arguments("unused.json", "--rows", "20000", "--payload-bytes", "65536")
        with patch.object(bench, "docker") as docker, self.assertRaisesRegex(ValueError, "128 MiB"):
            bench.main(args)
        docker.assert_not_called()
        args = self.arguments("unused.json", "--rows", "2", "--batch", "500", "--payload-bytes", "65536")
        with self.assertRaisesRegex(ValueError, "4 MiB"):
            bench.validate_args(args)

    def test_existing_output_fails_before_resource_creation(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "receipt.json"
            path.write_text("keep")
            with patch.object(bench, "exercise") as exercise, self.assertRaises(FileExistsError):
                bench.main(self.arguments(str(path)))
            exercise.assert_not_called()
            self.assertEqual(path.read_text(), "keep")

    def test_failure_records_progress_and_still_cleans_owned_resources(self):
        def fail(_args, report, _directory, _ownership):
            report["phases"].append({"name": "initial_half", "batches": [{"start": 0, "end": 2}]})
            report["active_phase"] = "initial_half"
            raise RuntimeError("synthetic failure")

        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp) / "receipt.json"
            with patch.object(bench, "exercise", side_effect=fail), \
                    patch.object(bench, "cleanup", return_value=True) as cleanup, \
                    patch.object(bench.signal, "signal"), patch.object(bench.signal, "alarm"), \
                    self.assertRaisesRegex(RuntimeError, "synthetic failure"):
                bench.main(self.arguments(str(output)))
            report = json.loads(output.read_text())
            self.assertEqual(report["result"], "FAIL")
            self.assertEqual(report["active_phase"], "initial_half")
            self.assertEqual(report["phases"][0]["batches"][0]["end"], 2)
            self.assertTrue(report["cleanup"])
            cleanup.assert_called_once_with("brainwire.benchmark.run=" + report["run"])

    def test_cleanup_failure_prevents_pass(self):
        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp) / "receipt.json"
            with patch.object(bench, "exercise"), patch.object(bench, "cleanup", return_value=False), \
                    patch.object(bench.signal, "signal"), patch.object(bench.signal, "alarm"), \
                    self.assertRaisesRegex(RuntimeError, "cleanup incomplete"):
                bench.main(self.arguments(str(output)))
            self.assertEqual(json.loads(output.read_text())["result"], "FAIL")

    def test_cli_bounds_reject_unbounded_work(self):
        for option, value in [("--rows", "1"), ("--rows", "20001"), ("--batch", "501"),
                              ("--payload-bytes", "65537"), ("--deadline", "1801"),
                              ("--extra-labels", "-1"), ("--extra-labels", "101")]:
            with self.subTest(option=option, value=value), \
                    patch("sys.stderr"), self.assertRaises(SystemExit):
                self.arguments("unused.json", option, value)

    def test_index_is_explicit_and_default_baseline_is_unchanged(self):
        self.assertFalse(self.arguments().indexed)
        self.assertTrue(self.arguments("unused.json", "--indexed").indexed)

    def test_index_setup_waits_for_a_ready_relationship_index(self):
        session = MagicMock()
        ddl, populating, ready = MagicMock(), MagicMock(), MagicMock()
        populating.data.return_value = [{"state": "populating", "entityType": "relationship"}]
        ready.data.return_value = [{"state": "scalar_range_ready", "entityType": "relationship"}]
        session.run.side_effect = [ddl, populating, ready]
        with patch.object(bench.time, "sleep") as sleep:
            self.assertGreaterEqual(bench.prepare_index(session, 1, self.query_type), 0)
        ddl.consume.assert_called_once()
        self.assertIn("FOR ()-[r:EVENT]-() ON (r.event_key)", session.run.call_args_list[0].args[0])
        self.assertEqual(session.run.call_args_list[1].kwargs, {"name": bench.INDEX_NAME})
        sleep.assert_called_once_with(.05)

    def test_index_setup_rejects_wrong_entity_failed_state_and_timeout(self):
        for state in ([], [{"state": "scalar_range_ready", "entityType": "node"}],
                      [{"state": "failed_value_too_large", "entityType": "relationship"}]):
            session = MagicMock()
            session.run.return_value.data.return_value = state
            with self.subTest(state=state), self.assertRaisesRegex(RuntimeError, "unexpected"):
                bench.prepare_index(session, 1, self.query_type)
        session = MagicMock()
        session.run.return_value.data.return_value = [{"state": "populating", "entityType": "relationship"}]
        with patch.object(bench.time, "monotonic", side_effect=[0, 0, .5, 2]), \
                self.assertRaisesRegex(TimeoutError, "readiness"):
            bench.prepare_index(session, 1, self.query_type)

    def test_index_deadline_includes_ddl_and_prevents_late_poll(self):
        session = MagicMock()
        query = MagicMock(side_effect=self.query_type)
        with patch.object(bench.time, "monotonic", side_effect=[10, 10.25, 11.1]), \
                self.assertRaisesRegex(TimeoutError, "readiness"):
            bench.prepare_index(session, 1, query)
        self.assertEqual(session.run.call_count, 1)
        self.assertEqual(query.call_args.kwargs, {"timeout": .75})

    def test_index_queries_share_remaining_deadline_and_late_ready_is_refused(self):
        session = MagicMock()
        session.run.return_value.data.return_value = [{"state": "scalar_range_ready", "entityType": "relationship"}]
        query = MagicMock(side_effect=self.query_type)
        with patch.object(bench.time, "monotonic", side_effect=[10, 10.1, 10.8, 11.1]), \
                self.assertRaisesRegex(TimeoutError, "readiness"):
            bench.prepare_index(session, 1, query)
        self.assertEqual(query.call_count, 2)
        self.assertAlmostEqual(query.call_args_list[0].kwargs["timeout"], .9)
        self.assertAlmostEqual(query.call_args_list[1].kwargs["timeout"], .2)


if __name__ == "__main__":
    unittest.main()
