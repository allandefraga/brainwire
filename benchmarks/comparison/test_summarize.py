from copy import deepcopy
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
from tempfile import TemporaryDirectory
import unittest
from xml.etree import ElementTree

spec = importlib.util.spec_from_file_location("brainwire_comparison_summary", Path(__file__).with_name("summarize.py"))
summary = importlib.util.module_from_spec(spec)
spec.loader.exec_module(summary)


class SummaryTests(unittest.TestCase):
    def setUp(self):
        path = Path(__file__).with_name("results") / "2026-11-14-1m-local-020b2-headroom.json"
        actual = json.loads(path.read_text(encoding="utf-8"))
        self.runs = [deepcopy(actual) for _ in range(3)]
        for run, offset in zip(self.runs, (1, 3, 2)):
            run["run_id"] = str(offset)
            for system_index, result in enumerate(run["results"], 1):
                result["node_create"]["seconds"] = offset + system_index
                result["edge_create"]["seconds"] = offset * system_index
                result["indexed_point_read"]["p95_ms"] = offset / system_index

    def test_metric_medians_are_deterministic_and_keep_units(self):
        medians = summary.summarize(self.runs)
        self.assertEqual(medians["brainwire"][:2], [3, 2])
        self.assertEqual(medians["falkordb"][:2], [4, 4])
        self.assertEqual(medians["neo4j"][3], 2 / 3)
        self.assertEqual(medians, summary.summarize(list(reversed(self.runs))))
        markdown = summary.markdown(medians)
        self.assertIn("| Neo4j | 5.000 s | 6.000 s |", markdown)
        self.assertIn("0.667 ms", markdown)
        self.assertIn("medians of per-run p95s", markdown)
        root = ElementTree.fromstring(summary.svg(medians))
        self.assertIn("Brainwire: 3.00 seconds for nodes and 2.00 for edges.", root.find("{*}desc").text)
        self.assertEqual(len(root.findall(".//{*}rect")), 9)

    def test_incompatible_or_incomplete_receipts_fail(self):
        cases = (
            (("status",), "failed", "passed"),
            (("run_id",), self.runs[0]["run_id"], "distinct run_id"),
            (("parameters", "nodes"), 100, "1,000,000 nodes"),
            (("parameters", "memory_mib"), 2048, "different parameters"),
            (("runner_sha256",), "different", "different runner_sha256"),
            (("images", "brainwire", "id"), "different", "different brainwire image"),
            (("results", 0, "image", "id"), "different", "different brainwire image"),
            (("results", 0, "durability"), "different", "different brainwire durability"),
            (("results", 1, "runtime_settings"), None, "different falkordb runtime_settings"),
            (("results", 1, "system"), "brainwire", "three systems once"),
            (("results", 0, "outgoing_one_hop", "count"), 0, "inconsistent brainwire workload counts"),
            (("results", 0, "indexed_point_read", "p95_ms"), float("nan"), "invalid brainwire measurements"),
            (("results", 0, "sigkill_recovery", "nodes"), 0, "inconsistent brainwire recovery counts"),
            (("results", 0, "sigkill_recovery", "edges"), 0, "inconsistent brainwire recovery counts"),
            (("results", 0, "sigkill_recovery", "verified_update_rows"), 1, "inconsistent brainwire recovery counts"),
        )
        for path, replacement, expected in cases:
            with self.subTest(path=path):
                runs = deepcopy(self.runs)
                field = runs[1]
                for key in path[:-1]:
                    field = field[key]
                field[path[-1]] = replacement
                with self.assertRaisesRegex(ValueError, expected):
                    summary.summarize(runs)

    def test_cli_does_not_overwrite_a_receipt_through_a_hardlink(self):
        with TemporaryDirectory() as temp:
            root = Path(temp)
            inputs = [root / f"receipt-{i}.json" for i in range(3)]
            for path, run in zip(inputs, self.runs):
                path.write_text(json.dumps(run), encoding="utf-8")
            old_receipt = inputs[1].read_bytes()
            svg = root / "chart.svg"
            os.link(inputs[1], svg)
            command = [sys.executable, summary.__file__, *map(str, inputs), "--svg", str(svg)]
            result = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(result.returncode, 2)
            self.assertIn("SVG output must differ from the input receipts", result.stderr)
            self.assertEqual(inputs[1].read_bytes(), old_receipt)


if __name__ == "__main__":
    unittest.main()
