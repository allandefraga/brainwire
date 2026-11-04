import contextlib
import io
import json
import tempfile
import unittest
from pathlib import Path

from scripts.compare_read_soaks import compare_reports, main


def receipt():
    samples = []
    for index in range(19):
        samples.append({"phase": "read-soak", "monotonic_seconds": 10 + index * 100,
                        "process": {"VmRSS_bytes": 1000 + index * 10,
                                    "RssAnon_bytes": 700 + index * 5,
                                    "RssFile_bytes": 300 + index * 5},
                        "current_bytes": 2000 + index * 10, "peak_bytes": 3000,
                        "max_bytes": 2 << 30, "events": {"oom": 0, "oom_kill": 0}})
    return {"status": "passed-diagnostic", "synthetic": True,
            "rows_per_label": 1000, "memory_limit_bytes": 2 << 30, "writers": 2,
            "readers": ["scan", "aggregation", "neighborhood", "cursor"],
            "driver": "5.28.8", "read_soak_seconds": 1801,
            "counts": {key: 1000 for key in ("Usuario", "Carro", "Endereco", "Casa",
                       "TEM_CARRO", "TEM_ENDERECO", "TEM_CASA", "CASA_FICA_EM")},
            "ack_rows_per_label": 1000, "cursor": {"rows": 1000, "page_size": 200, "delegated": True},
            "integrity": {"status": "ok"},
            "oom_evidence": {"observed": False, "kernel_log_verified": False, "restart_count": 0},
            "image_identity": {"image_id": "sha256:" + "a" * 64},
            "client_results": [{"role": role, "latency": {"requests": 12}, "denials": {}}
                               for role in ("scan", "aggregation", "neighborhood", "cursor")],
            "samples": samples}


class CompareReadSoaksTests(unittest.TestCase):
    def test_uses_time_windows_and_returns_only_safe_fields(self):
        before, after = receipt(), receipt()
        after["host_start"] = {"private": "/private/customer/token"}
        after["image"] = "secret-image-name"
        after["samples"][1]["monotonic_seconds"] = 11
        after["samples"][2]["monotonic_seconds"] = 12
        after["samples"][3]["monotonic_seconds"] = 13
        result = compare_reports(before, after, window_seconds=300)
        self.assertEqual(result["baseline"]["first_window"]["median_process_rss_bytes"], 1010)
        self.assertEqual(result["baseline"]["last_window"]["median_process_rss_bytes"], 1170)
        self.assertEqual(result["baseline"]["window_change_bytes"], 160)
        self.assertEqual(result["baseline"]["window_change_anon_bytes"], 80)
        self.assertEqual(result["baseline"]["window_change_file_bytes"], 80)
        self.assertEqual(result["difference_in_anon_window_changes_bytes"], -2.5)
        self.assertEqual(result["difference_in_window_changes_bytes"], -5)
        self.assertEqual(result["candidate"]["first_window"]["samples"], 4)
        self.assertEqual(result["workload_settings"]["status"], "unavailable")
        self.assertFalse(result["kernel_logs_verified_for_both"])
        text = json.dumps(result)
        self.assertNotIn("customer", text)
        self.assertNotIn("secret-image", text)
        self.assertIn("does not identify a leak", text)

    def test_missing_optional_process_components_remain_unavailable(self):
        before, after = receipt(), receipt()
        del after["samples"][0]["process"]["RssAnon_bytes"]
        for sample in after["samples"]:
            del sample["process"]["RssFile_bytes"]
        result = compare_reports(before, after)
        self.assertIsNone(result["candidate"]["first_window"]["median_process_rss_anon_bytes"])
        self.assertIsNone(result["candidate"]["window_change_anon_bytes"])
        self.assertIsNone(result["candidate"]["window_change_file_bytes"])
        self.assertIsNone(result["difference_in_anon_window_changes_bytes"])
        self.assertIsNone(result["difference_in_file_window_changes_bytes"])
        after["samples"][0]["process"]["RssAnon_bytes"] = True
        with self.assertRaises(ValueError):
            compare_reports(before, after)

    def test_rejects_incomplete_or_incompatible_receipts(self):
        for key, value in (("status", "failed"), ("synthetic", False),
                           ("integrity", {"status": "failed"}),
                           ("ack_rows_per_label", 999), ("cursor", {"rows": 999}),
                           ("oom_evidence", {"observed": True}),
                           ("client_results", []), ("integrity", None),
                           ("image_identity", []), ("readers", [["scan"]])):
            with self.subTest(key=key):
                invalid = receipt()
                invalid[key] = value
                with self.assertRaises(ValueError):
                    compare_reports(receipt(), invalid)
        different = receipt()
        different["cursor"]["page_size"] = 100
        with self.assertRaisesRegex(ValueError, "workload"):
            compare_reports(receipt(), different)

    def test_compares_new_settings_and_preserves_legacy_uncertainty(self):
        before, after = receipt(), receipt()
        settings = {"batch_size": 100, "neighborhood_rows": 1000,
                    "sample_seconds": 5, "reader_pause": .5, "query_timeout": 30}
        before["workload_settings"] = dict(settings)
        self.assertEqual(compare_reports(before, after)["workload_settings"]["status"], "unavailable")
        after["workload_settings"] = dict(settings)
        result = compare_reports(before, after)["workload_settings"]
        self.assertEqual(result, {"status": "complete-match", "matched_values": settings,
                                  "unavailable_fields": []})
        after["workload_settings"]["reader_pause"] = .75
        with self.assertRaisesRegex(ValueError, "workload"):
            compare_reports(before, after)
        after["workload_settings"] = {"sample_seconds": "private-value"}
        with self.assertRaises(ValueError):
            compare_reports(before, after)

    def test_rejects_bad_time_values_insufficient_or_oom_samples(self):
        changes = [lambda s: s[1].update(monotonic_seconds=float("nan")),
                   lambda s: s[1].update(monotonic_seconds=s[0]["monotonic_seconds"]),
                   lambda s: s[1]["process"].update(VmRSS_bytes=True),
                   lambda s: s[1]["process"].update(VmRSS_bytes=10 ** 400),
                   lambda s: s[1]["events"].update(oom_kill=1),
                   lambda s: s[1].update(monotonic_seconds=10 ** 400),
                   lambda s: s.clear(),
                   lambda s: s.__delitem__(slice(2, 17))]
        for change in changes:
            invalid = receipt()
            change(invalid["samples"])
            with self.assertRaises(ValueError):
                compare_reports(receipt(), invalid)
        for window in (0, -1, float("inf"), True):
            with self.assertRaises(ValueError):
                compare_reports(receipt(), receipt(), window_seconds=window)

    def test_rejects_central_samples_without_full_soak_coverage(self):
        shortened = receipt()
        shortened["samples"] = shortened["samples"][3:-3]
        with self.assertRaisesRegex(ValueError, "cover the recorded"):
            compare_reports(receipt(), shortened)

    def test_cli_reads_local_json_and_hides_input_paths_on_failure(self):
        with tempfile.TemporaryDirectory() as folder:
            before, after = Path(folder) / "before.json", Path(folder) / "secret-customer.json"
            before.write_text(json.dumps(receipt()))
            after.write_text(json.dumps(receipt()))
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                self.assertEqual(main([str(before), str(after)]), 0)
            self.assertEqual(json.loads(output.getvalue())["status"], "descriptive-comparison")
            after.write_text("not JSON with secret-data")
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                self.assertEqual(main([str(before), str(after)]), 1)
            self.assertNotIn(folder, output.getvalue())
            self.assertNotIn("secret", output.getvalue())
            huge = receipt()
            huge["samples"][0]["monotonic_seconds"] = 10 ** 400
            after.write_text(json.dumps(huge))
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                self.assertEqual(main([str(before), str(after)]), 1)
            self.assertEqual(json.loads(output.getvalue())["status"], "invalid")
            self.assertNotIn(folder, output.getvalue())
            huge = receipt()
            huge["samples"][3]["monotonic_seconds"] = 211
            for sample in huge["samples"]:
                sample["process"]["VmRSS_bytes"] = 10 ** 400
            after.write_text(json.dumps(huge))
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                self.assertEqual(main([str(before), str(after)]), 1)
            self.assertEqual(json.loads(output.getvalue())["status"], "invalid")
            self.assertNotIn(folder, output.getvalue())
            after.write_text("[" * 2000 + "]" * 2000)
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                self.assertEqual(main([str(before), str(after)]), 1)
            self.assertEqual(json.loads(output.getvalue())["status"], "invalid")


if __name__ == "__main__":
    unittest.main()
