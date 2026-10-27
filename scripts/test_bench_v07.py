import argparse
import contextlib
import hashlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from scripts import bench_v07


class HostReceiptTests(unittest.TestCase):
    def host(self, active):
        evidence = {
            ("pmset", "-g", "batt"): {"exit_code": 0, "stdout": "Now drawing from 'AC Power'", "stderr": ""},
            ("pmset", "-g"): {"exit_code": 0, "stdout": "Currently in use:\n lowpowermode " + active, "stderr": ""},
            ("pmset", "-g", "custom"): {"exit_code": 0, "stdout": "Battery Power:\n lowpowermode 1\nAC Power:\n lowpowermode 0", "stderr": ""},
        }
        with patch.object(bench_v07.platform, "system", return_value="Darwin"), patch.object(
            bench_v07, "command_output", side_effect=lambda args: evidence[tuple(args)]
        ):
            return bench_v07.host_receipt(), evidence

    def test_active_ac_zero_qualifies_even_when_battery_profile_is_one(self):
        host, evidence = self.host("0")
        self.assertTrue(host["power_qualified"])
        self.assertEqual(host["power_settings"], evidence[("pmset", "-g")])
        self.assertEqual(host["power_profiles"], evidence[("pmset", "-g", "custom")])

    def test_active_low_power_one_is_denied_despite_ac_profile_zero(self):
        host, _ = self.host("1")
        self.assertFalse(host["power_qualified"])
        self.assertIn("active", host["power_reason"])


class ReceiptTests(unittest.TestCase):
    def fake_baseline(self, folder, check_ok=True):
        binary = folder / "baseline"
        binary.write_text('''#!/usr/bin/env python3
import json, pathlib, sys
phase=sys.argv[1]
if phase == 'load': pathlib.Path(sys.argv[2]).mkdir()
print(json.dumps({'phase':phase,'status':%r if phase=='check' else 'ok','rows':4}))
''' % ("ok" if check_ok else "failed"))
        binary.chmod(0o700)
        return binary

    def args(self, binary, output):
        return argparse.Namespace(baseline=binary, output=output, source_revision="fixture-revision",
                                  rows_per_label=10, load_sessions=2, write_rows=10, sessions=[1, 2],
                                  phase_timeout=5, sample_seconds=0.01, rss_watchdog_bytes=0,
                                  allow_unqualified_host=False)

    def execute(self, args):
        host = {"power_qualified": True, "power_reason": "test fixture", "load_average": [0, 0, 0]}
        with patch.object(bench_v07, "host_receipt", return_value=host), contextlib.redirect_stdout(io.StringIO()):
            return bench_v07.run(args)

    def test_receipt_records_exact_binary_and_all_independent_phases(self):
        with tempfile.TemporaryDirectory() as temporary:
            folder = Path(temporary)
            binary = self.fake_baseline(folder)
            output = folder / "run"
            self.assertEqual(self.execute(self.args(binary, output)), 0)
            receipt = json.loads((output / "receipt.json").read_text())
            self.assertEqual(receipt["status"], "completed_embedded_only")
            self.assertEqual(receipt["baseline_executable"]["sha256"], hashlib.sha256(binary.read_bytes()).hexdigest())
            self.assertEqual([row["name"] for row in receipt["phases"]], ["load", "read", "write", "check"])
            self.assertEqual(receipt["phases"][-1]["records"][0]["status"], "ok")
            self.assertFalse(receipt["memory"]["hard_rss_limit_enforced"])
            self.assertEqual(receipt["dataset"]["fixture_nodes"], 40)
            self.assertTrue((output / "data" / "bench").is_dir())
            self.assertIn("not measured", receipt["qualification_gaps"][2])
            with self.assertRaises(FileExistsError):
                self.execute(self.args(binary, output))
            self.assertTrue((output / "receipt.json").exists())

    def test_zero_exit_check_without_ok_is_failed_receipt(self):
        with tempfile.TemporaryDirectory() as temporary:
            folder = Path(temporary)
            output = folder / "run"
            self.assertEqual(self.execute(self.args(self.fake_baseline(folder, check_ok=False), output)), 1)
            receipt = json.loads((output / "receipt.json").read_text())
            self.assertEqual(receipt["status"], "failed")
            self.assertIn("did not report", receipt["phases"][-1]["validation_error"])

    def test_unqualified_host_does_not_create_output(self):
        with tempfile.TemporaryDirectory() as temporary:
            folder = Path(temporary)
            args = self.args(self.fake_baseline(folder), folder / "run")
            with patch.object(bench_v07, "host_receipt", return_value={"power_qualified": False, "power_reason": "unknown"}):
                with self.assertRaisesRegex(ValueError, "unknown"):
                    bench_v07.run(args)
            self.assertFalse(args.output.exists())

    def test_empty_success_output_does_not_claim_measurements(self):
        with tempfile.TemporaryDirectory() as temporary:
            folder = Path(temporary)
            binary = folder / "empty"
            binary.write_text("#!/usr/bin/env python3\n")
            binary.chmod(0o700)
            result = bench_v07.run_phase("empty", [binary], folder, 1, 0, 0.01)
            self.assertEqual(result["exit_code"], 0)
            self.assertEqual(result["status"], "failed")
            self.assertEqual(result["records"], [])

    def test_timeout_stops_own_process_and_records_failure(self):
        with tempfile.TemporaryDirectory() as temporary:
            folder = Path(temporary)
            binary = folder / "sleeper"
            binary.write_text("#!/usr/bin/env python3\nimport time\ntime.sleep(60)\n")
            binary.chmod(0o700)
            result = bench_v07.run_phase("slow", [binary], folder, 0.05, 0, 0.01)
            self.assertEqual(result["status"], "failed")
            self.assertEqual(result["stopped_reason"], "phase timeout")
            self.assertNotEqual(result["exit_code"], 0)


if __name__ == "__main__":
    unittest.main()
