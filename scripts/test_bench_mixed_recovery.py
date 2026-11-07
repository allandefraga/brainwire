import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from scripts import bench_mixed_recovery as bench


class Result:
    def __init__(self, values):
        self.values = values

    def single(self, strict=False):
        return [self.values]

    def data(self):
        return self.values


class Session:
    def __init__(self, expected, incorrect=False):
        self.expected = expected
        self.incorrect = incorrect
        self.pages = []

    def run(self, query, **params):
        if "ids" in params:
            self.pages.append(params["ids"])
            rows = [{"id": item, "node_revision": self.expected[int(item[5:])],
                     "edge_revision": self.expected[int(item[5:])]} for item in params["ids"]]
            if self.incorrect:
                rows[0]["edge_revision"] += 1
            return Result(rows)
        if "Scratch" in query:
            return Result(0)
        if "sum(" in query:
            return Result(sum(self.expected))
        return Result(len(self.expected))


def query(text, **_kwargs):
    return text


class MixedRecoveryTests(unittest.TestCase):
    def args(self, *extra):
        return bench.parser().parse_args(["--image", "test-image", "--output", "receipt.json", *extra])

    def test_limits_are_checked_before_docker(self):
        for extra in (["--batch", "1000", "--rows", "100"], ["--pause", "nan"],
                      ["--pause", "0"], ["--seconds", "3600", "--deadline", "3600"]):
            with self.subTest(extra=extra), self.assertRaises(ValueError):
                bench.validate(self.args(*extra))
        for extra in (["--rows", "1000001"], ["--memory", "1"], ["--seconds", "0"]):
            with self.subTest(extra=extra), self.assertRaises(SystemExit):
                self.args(*extra)

    def test_batch_planning_stays_within_rows_and_bounds(self):
        for rows, batch in ((100, 100), (103, 20), (1000, 250)):
            for iteration in range(20):
                indices = list(bench.batch_indices(rows, batch, iteration))
                self.assertTrue(indices)
                self.assertLessEqual(len(indices), batch)
                self.assertTrue(all(0 <= index < rows for index in indices))
        self.assertEqual(bench.identifier(3), "item-00000003")

    def test_progress_requires_successful_work_in_each_full_window(self):
        windows = bench.progress_windows(120, 60, 2, 1)
        bench.record_progress(windows, 60, "writes", 10, 9)
        bench.record_progress(windows, 60, "writes", 20, 19)
        bench.record_progress(windows, 60, "reads", 30, 29)
        observed = bench.evaluate_progress(windows)
        self.assertTrue(observed[0]["passed"])
        self.assertFalse(observed[1]["passed"])
        bench.record_progress(windows, 60, "writes", 60, 59)
        bench.record_progress(windows, 60, "writes", 119, 118)
        bench.record_progress(windows, 60, "reads", 100, 99)
        self.assertTrue(all(window["passed"] for window in bench.evaluate_progress(windows)))
        self.assertEqual(windows[1]["last_completion_s"], 119)

    def test_trailing_partial_window_is_diagnostic_but_short_run_requires_work(self):
        windows = bench.progress_windows(125, 60, 1, 1)
        self.assertEqual([window["required"] for window in windows], [True, True, False])
        self.assertIsNone(bench.evaluate_progress(windows)[-1]["passed"])
        short = bench.progress_windows(10, 60, 1, 1)
        self.assertFalse(bench.evaluate_progress(short)[0]["passed"])
        bench.record_progress(short, 60, "writes", 2, 1)
        bench.record_progress(short, 60, "reads", 3, 1)
        self.assertTrue(bench.evaluate_progress(short)[0]["passed"])

    def test_inflight_completion_is_recorded_separately_during_shutdown(self):
        windows = bench.progress_windows(60, 60, 1, 1)
        bench.record_progress(windows, 60, "writes", 10, 9)
        bench.record_progress(windows, 60, "reads", 61, 59)
        observed = bench.evaluate_progress(windows)[0]
        self.assertTrue(observed["passed"])
        self.assertEqual(observed["reads"], 0)
        self.assertEqual(observed["reads_after_requested_end"], 1)
        self.assertEqual(observed["last_completion_s"], 61)
        bench.record_progress(windows, 60, "reads", 62, 60)
        bench.record_progress(windows, 60, "reads", -1, -2)
        self.assertEqual(windows[0]["reads_after_requested_end"], 1)

    def test_memory_requires_complete_numeric_non_oom_observations(self):
        good = "current 10\npeak 20\nlimit 100\nmax 0\noom 0\noom_kill 0\nVmRSS: 3 kB\n"
        self.assertEqual(bench.parse_memory(good)["rss_bytes"], 3072)
        for bad in (good.replace("peak 20", "peak max"), good.replace("limit 100", "limit 9"),
                    good.replace("VmRSS: 3 kB", "VmRSS: 3 B"), good.replace("oom 0\n", "")):
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                bench.parse_memory(bad)
        with self.assertRaises(RuntimeError):
            bench.parse_memory(good.replace("oom_kill 0", "oom_kill 1"))

    def test_quiescent_verification_checks_every_node_and_edge_revision(self):
        expected = list(range(1234))
        session = Session(expected)
        result = bench.verify(session, query, len(expected), expected)
        self.assertEqual(result["exact_rows_verified"], 1234)
        self.assertEqual([len(page) for page in session.pages], [500, 500, 234])
        with self.assertRaises(AssertionError):
            bench.verify(Session(expected, incorrect=True), query, len(expected), expected)

    def test_existing_output_prevents_resource_creation(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "receipt.json"
            output.write_text("keep me")
            args = self.args("--output", str(output))
            with patch.object(bench, "exercise") as exercise, patch.object(bench, "cleanup") as cleanup:
                with self.assertRaises(FileExistsError):
                    bench.main(args)
                exercise.assert_not_called()
                cleanup.assert_not_called()
            self.assertEqual(output.read_text(), "keep me")

    def test_failure_persists_receipt_cleans_and_omits_exception_message(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "receipt.json"
            args = self.args("--output", str(output))
            with patch.object(bench, "exercise", side_effect=RuntimeError("private-token")), \
                    patch.object(bench, "cleanup", return_value=True) as cleanup:
                with self.assertRaises(RuntimeError):
                    bench.main(args)
            receipt = json.loads(output.read_text())
            self.assertEqual(receipt["result"], "FAIL")
            self.assertTrue(receipt["cleanup"])
            self.assertEqual(receipt["error"], "RuntimeError")
            self.assertEqual(receipt["runner_schema"], 2)
            self.assertEqual(receipt["workload_settings"], {
                "rows": 100000, "batch": 250, "seconds": 600, "memory": 2,
                "payload_bytes": 256, "query_timeout": 30, "sample_seconds": 2,
                "deadline": 1800, "pause": .01, "progress_window": 60,
                "minimum_writes_per_window": 1, "minimum_reads_per_window": 1,
            })
            self.assertNotIn("private-token", output.read_text())
            self.assertTrue(cleanup.call_args.args[0].startswith("brainwire.benchmark.run="))

    def test_failed_cleanup_prevents_passing_report(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "receipt.json"
            args = self.args("--output", str(output))
            with patch.object(bench, "exercise"), patch.object(bench, "cleanup", return_value=False):
                with self.assertRaises(RuntimeError):
                    bench.main(args)
            receipt = json.loads(output.read_text())
            self.assertEqual(receipt["result"], "FAIL")
            self.assertFalse(receipt["cleanup"])


if __name__ == "__main__":
    unittest.main()
