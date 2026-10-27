import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from types import SimpleNamespace

from scripts import bench_v07_bolt as bench


def fixture():
    return {"synthetic": True, "disposable": True, "frozen": True,
            "dataset_id": "synthetic-fixture-001", "server_id": "isolated-fixture-server",
            "database": "bench", "schema_version": 1, "source_revision": "a" * 40,
            "server_binary_sha256": "b" * 64,
            "schema": {"source_label": "Person", "target_label": "Car",
                       "relationship_type": "OWNS", "group_property": "brand", "cursor_label": "Person"},
            "expected": {"allow_all": {"count_pairs": 3, "cursor_rows": 3,
                                       "top_n": [{"group_value": "A", "count": 2}, {"group_value": "B", "count": 1}]},
                         "restricted": {"count_pairs": 1, "cursor_rows": 1,
                                        "top_n": [{"group_value": "A", "count": 1}]}}}


def environment():
    return {"BRAINWIRE_BENCH_URI": "bolt://127.0.0.1:7687", "BRAINWIRE_BENCH_PRINCIPAL": "private-principal",
            "BRAINWIRE_BENCH_TOKEN": "private-token-not-for-receipts",
            "BRAINWIRE_BENCH_RESTRICTED_VISIBILITY": json.dumps({"principals": ["private-claim"]}),
            "BRAINWIRE_BENCH_CONFIRM_SYNTHETIC": "synthetic-fixture-001"}


class FakeResult:
    def __init__(self, rows):
        self.rows = rows

    def data(self):
        return self.rows


class FakeTransaction:
    def __init__(self, owner, policy):
        self.owner, self.policy = owner, policy

    def __enter__(self):
        return self

    def __exit__(self, *args):
        return False

    def run(self, query, params):
        all_rows = self.policy == {"allow_all": True}
        self.owner.queries.append((query, params, self.policy))
        if query == bench.SCHEMA_QUERY:
            return FakeResult([{"version": 1}])
        if "group_value" in query:
            return FakeResult(fixture()["expected"]["allow_all" if all_rows else "restricted"]["top_n"])
        if "count(*)" in query:
            return FakeResult([{"count": 3 if all_rows else 1}])
        ids = ["p-001", "p-002", "p-003"] if all_rows else ["p-002"]
        ids = [value for value in ids if value > params["after"]]
        return FakeResult([{"id": value} for value in ids[:2]])

    def commit(self):
        self.owner.commits += 1


class FakeSession:
    def __init__(self, owner):
        self.owner = owner

    def __enter__(self):
        return self

    def __exit__(self, *args):
        return False

    def begin_transaction(self, *, metadata, timeout):
        assert set(metadata) == {"brainwire_visibility"}
        assert timeout > 0
        return FakeTransaction(self.owner, metadata["brainwire_visibility"])


class FakeDriver:
    def __init__(self):
        self.queries, self.commits, self.sessions = [], 0, []

    def __enter__(self):
        return self

    def __exit__(self, *args):
        return False

    def session(self, **kwargs):
        self.sessions.append(kwargs)
        return FakeSession(self)


class HarnessTests(unittest.TestCase):
    def args(self, folder, manifest=None):
        path = folder / "manifest.json"
        path.write_text(json.dumps(fixture() if manifest is None else manifest))
        return bench.parser().parse_args(["--manifest", str(path), "--output", str(folder / "output"),
                                          "--page-size", "2", "--repeats", "2"])

    def test_read_transactions_carry_both_policies_and_exact_cursor_receipts(self):
        with tempfile.TemporaryDirectory() as temporary:
            folder = Path(temporary)
            args = self.args(folder)
            driver = FakeDriver()
            factories = []
            def factory(uri, **options):
                factories.append((uri, options))
                return driver
            with patch.object(bench, "save_receipt", wraps=bench.save_receipt) as saved:
                status = bench.run(args, factory, environment(), lambda: {"power_qualified": True})
            # Writes occur at query/mode boundaries, never once per cursor page.
            self.assertEqual(saved.call_count, 10)
            self.assertEqual(status, 0)
            raw = (args.output / "receipt.json").read_text()
            receipt = json.loads(raw)
            self.assertEqual(receipt["status"], "completed_read_diagnostic")
            self.assertEqual(receipt["modes"]["allow_all"]["cursor"]["rows_seen"], 3)
            self.assertEqual([page["rows"] for page in receipt["modes"]["allow_all"]["cursor"]["pages"]], [2, 1, 0])
            self.assertEqual(receipt["modes"]["restricted"]["cursor"]["rows_seen"], 1)
            cursor = receipt["modes"]["allow_all"]["cursor"]
            page_rows = [json.loads(line) for line in (args.output / cursor["page_file"]).read_text().splitlines()]
            self.assertEqual(page_rows, cursor["pages"])
            self.assertAlmostEqual(cursor["query_seconds"], sum(page["seconds"] for page in page_rows))
            self.assertGreaterEqual(cursor["seconds"], cursor["query_seconds"])
            self.assertIn("journaling", cursor["seconds_scope"])
            self.assertTrue(all(set(session) == {"database", "default_access_mode"} and session["default_access_mode"] == "READ"
                                for session in driver.sessions))
            self.assertTrue(any(policy == {"allow_all": True} for _, _, policy in driver.queries))
            self.assertTrue(any(policy == {"principals": ["private-claim"]} for _, _, policy in driver.queries))
            self.assertEqual(factories[0][1]["max_transaction_retry_time"], 0)
            for private in ("private-token-not-for-receipts", "private-principal", "private-claim"):
                self.assertNotIn(private, raw)
            self.assertIn("LIMIT 2", receipt["modes"]["allow_all"]["cursor"]["query"])
            with self.assertRaises(FileExistsError):
                bench.run(args, factory, environment(), lambda: {"power_qualified": True})

    def test_refuses_unconfirmed_non_synthetic_injected_schema_or_uri_credentials_before_connection(self):
        with tempfile.TemporaryDirectory() as temporary:
            folder = Path(temporary)
            invalid = fixture()
            invalid["synthetic"] = False
            with self.assertRaises(ValueError):
                bench.read_manifest(self.args(folder, invalid).manifest)
            invalid = fixture()
            invalid["schema"]["source_label"] = "Person) DELETE n"
            with self.assertRaises(ValueError):
                bench.read_manifest(self.args(folder, invalid).manifest)
            valid = bench.read_manifest(self.args(folder).manifest)
            for key, value in (("BRAINWIRE_BENCH_CONFIRM_SYNTHETIC", "wrong"),
                               ("BRAINWIRE_BENCH_URI", "bolt://user:secret@127.0.0.1:7687"),
                               ("BRAINWIRE_BENCH_RESTRICTED_VISIBILITY", '{"allow_all":true,"principals":["x"]}')):
                env = environment()
                env[key] = value
                with self.assertRaises(ValueError):
                    bench.configuration(valid, env)
            with self.assertRaises(ValueError):
                bench.configuration(valid, {})

    def test_default_pages_are_200_and_cursor_incomplete_is_not_success(self):
        with tempfile.TemporaryDirectory() as temporary:
            args = self.args(Path(temporary))
            self.assertEqual(bench.parser().parse_args(["--manifest", "a", "--output", "b"]).page_size, 200)
            args.max_cursor_pages = 1
            driver = FakeDriver()
            status = bench.run(args, lambda *a, **k: driver, environment(), lambda: {"power_qualified": True})
            receipt = json.loads((args.output / "receipt.json").read_text())
            self.assertEqual(status, 1)
            self.assertEqual(receipt["status"], "incomplete")
            self.assertFalse(receipt["modes"]["allow_all"]["cursor"]["complete"])

    def test_cursor_rejects_repeated_or_unsorted_ids_and_preserves_prior_pages(self):
        class Session:
            def __init__(self): self.calls = 0
        session = Session()
        original = bench.fetch
        snapshots = []
        result = {}
        def fetch(*args):
            session.calls += 1
            return 0.01, ([{"id": "p-1"}] if session.calls == 1 else [{"id": "p-1"}])
        try:
            bench.fetch = fetch
            with self.assertRaises(ValueError):
                bench.cursor_walk(session, "query", {}, 3, 200, 1, 10, 10, result,
                                  lambda page: snapshots.append(json.loads(json.dumps(result))))
        finally:
            bench.fetch = original
        self.assertEqual(len(snapshots), 1)
        self.assertEqual(snapshots[0]["pages"][0]["last_id"], "p-1")
        self.assertEqual(result["rows_seen"], 1)

    def test_fixture_mismatch_and_driver_failure_have_no_secret_exception_text(self):
        with tempfile.TemporaryDirectory() as temporary:
            args = self.args(Path(temporary))
            class BrokenDriver(FakeDriver):
                def session(self, **kwargs):
                    raise RuntimeError("private-token-not-for-receipts")
            with self.assertRaises(RuntimeError):
                bench.run(args, lambda *a, **k: BrokenDriver(), environment(), lambda: {"power_qualified": True})
            raw = (args.output / "receipt.json").read_text()
            receipt = json.loads(raw)
            self.assertEqual(receipt["status"], "failed")
            self.assertEqual(receipt["error"], {"type": "RuntimeError", "code": None})
            self.assertNotIn("private-token-not-for-receipts", raw)
        invalid = fixture()
        invalid["expected"]["allow_all"]["top_n"] = [{"group_value": "B", "count": 1}, {"group_value": "A", "count": 2}]
        with tempfile.TemporaryDirectory() as temporary:
            with self.assertRaises(ValueError):
                bench.read_manifest(self.args(Path(temporary), invalid).manifest)

    def test_power_check_uses_active_ac_settings_not_inactive_battery_profile(self):
        calls = []
        def command(args, **kwargs):
            calls.append(args)
            values = {
                ("pmset", "-g", "batt"): "Now drawing from 'AC Power'\n",
                ("pmset", "-g"): "Currently in use:\n lowpowermode 0\n",
                ("pmset", "-g", "custom"): "Battery Power:\n lowpowermode 1\nAC Power:\n lowpowermode 0\n",
            }
            return SimpleNamespace(returncode=0, stdout=values[tuple(args)], stderr="")
        with patch.object(bench.platform, "system", return_value="Darwin"), patch.object(bench.subprocess, "run", side_effect=command):
            host = bench.host_receipt()
        self.assertTrue(host["power_qualified"])
        self.assertEqual(calls, [["pmset", "-g", "batt"], ["pmset", "-g"]])
        self.assertIn("Currently in use", host["power_settings"])

    def test_active_low_power_mode_or_battery_power_rejects_qualification(self):
        for source, active in [("AC Power", "1"), ("Battery Power", "0")]:
            def command(args, **kwargs):
                stdout = (f"Now drawing from '{source}'\n" if args[-1] == "batt"
                          else f"Currently in use:\n lowpowermode {active}\n")
                return SimpleNamespace(returncode=0, stdout=stdout, stderr="")
            with patch.object(bench.platform, "system", return_value="Darwin"), patch.object(bench.subprocess, "run", side_effect=command):
                self.assertFalse(bench.host_receipt()["power_qualified"])

    def test_schema_version_uses_the_integer_bolt_contract(self):
        with tempfile.TemporaryDirectory() as temporary:
            folder = Path(temporary)
            for version in ("1", True, 1.0, -1, 1 << 32):
                manifest = fixture()
                manifest["schema_version"] = version
                with self.assertRaises(ValueError):
                    bench.read_manifest(self.args(folder, manifest).manifest)
            args = self.args(folder)
            with patch.object(bench, "fetch", return_value=(0.01, [{"version": True}])):
                with self.assertRaises(ValueError):
                    bench.run(args, lambda *a, **k: FakeDriver(), environment(), lambda: {"power_qualified": True})
            receipt = json.loads((args.output / "receipt.json").read_text())
            self.assertEqual(receipt["status"], "failed")
            self.assertNotIn("count_pairs", receipt["modes"]["allow_all"])

    def test_unqualified_host_does_not_create_output(self):
        with tempfile.TemporaryDirectory() as temporary:
            args = self.args(Path(temporary))
            with self.assertRaises(ValueError):
                bench.run(args, lambda *a, **k: self.fail("network must not start"), environment(),
                          lambda: {"power_qualified": False})
            self.assertFalse(args.output.exists())


if __name__ == "__main__":
    unittest.main()
