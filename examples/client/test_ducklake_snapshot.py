"""Real cached-extension tests: uv run --no-project --with duckdb==1.5.5 python
-m unittest examples.client.test_ducklake_snapshot -v. Never installs extensions.
"""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from examples.client.export_ducklake_snapshot import export_ducklake_snapshot, sql_literal
from examples.client.ingest_snapshot import normalize_row

QUERY_FILE = Path(__file__).with_name("export_ducklake_snapshot.sql")


class DuckLakeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        try:
            import duckdb
        except ImportError:
            raise unittest.SkipTest("optional DuckDB dependency is not installed")
        cls.duckdb = duckdb
        with duckdb.connect(config={"autoinstall_known_extensions": "false",
                                    "autoload_known_extensions": "false"}) as connection:
            try:
                connection.execute("LOAD ducklake")
                connection.execute("LOAD sqlite")
            except duckdb.Error as error:
                raise unittest.SkipTest(f"optional cached DuckLake/SQLite extensions unavailable: {error}")

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        root = Path(self.directory.name).resolve(strict=True)
        self.catalog = root / "synthetic'catalog.sqlite"
        managed = root / "managed-data"
        managed.mkdir()
        self.data = root / "synthetic'data"
        self.data.symlink_to(managed, target_is_directory=True)
        self.output = root / "snapshot.jsonl"
        self.writer = self.duckdb.connect(config={"autoinstall_known_extensions": "false",
                                                 "autoload_known_extensions": "false"})
        self.addCleanup(self.writer.close)
        self.writer.execute("LOAD ducklake")
        self.writer.execute("LOAD sqlite")
        self.writer.execute(f"ATTACH {sql_literal('ducklake:sqlite:' + str(self.catalog))} AS writer (DATA_PATH {sql_literal(str(self.data) + '/')})")
        self.writer.execute("CALL writer.set_option('data_inlining_row_limit', 0)")
        self.writer.execute("""CREATE TABLE writer.main.source (
            record_type VARCHAR, tenant_id VARCHAR, connector_id VARCHAR, external_id VARCHAR,
            subject VARCHAR, status VARCHAR, priority VARCHAR, tags VARCHAR[],
            ticket_id VARCHAR, body VARCHAR, is_public BOOLEAN)""")
        self.writer.execute("""INSERT INTO writer.main.source VALUES
            ('ticket', 'synthetic-tenant', 'zendesk', 't1', NULL, 'open', NULL, ['é', 'synthetic'], NULL, NULL, NULL),
            ('comment', 'synthetic-tenant', 'zendesk', 'c1', NULL, NULL, NULL, NULL, 't1', 'old body', true)""")
        self.version = self.writer.execute("SELECT max(snapshot_id) FROM ducklake_snapshots('writer')").fetchone()[0]
        self.query = QUERY_FILE.read_text(encoding="utf-8")

    def export(self, version=None, **bounds):
        return export_ducklake_snapshot(self.catalog, self.data,
                                       self.version if version is None else version,
                                       self.query, self.output, **bounds)

    def assert_unpublished(self):
        self.assertFalse(self.output.exists())
        self.assertEqual(list(self.output.parent.glob(".snapshot-*.tmp")), [])

    def test_explicit_snapshot_retains_old_rows_after_update_and_delete(self):
        self.assertTrue(list(self.data.rglob("*.parquet")), "fixture must use managed Parquet files")
        self.writer.execute("UPDATE writer.main.source SET subject = 'new subject', tags = ['new'], status = 'closed' WHERE record_type = 'ticket'")
        self.writer.execute("DELETE FROM writer.main.source WHERE record_type = 'comment'")
        latest = self.writer.execute("SELECT max(snapshot_id) FROM ducklake_snapshots('writer')").fetchone()[0]
        self.assertGreater(latest, self.version)
        receipt = self.export(fetch_rows=1)
        rows = [json.loads(line) for line in self.output.read_bytes().splitlines()]
        self.assertEqual([normalize_row(row) for row in rows], rows)
        self.assertEqual(receipt["rows"], 2)
        self.assertEqual(receipt["snapshot_version"], self.version)
        self.assertEqual(receipt["source_type"], "ducklake-sqlite")
        self.assertEqual(receipt["duckdb_version"], self.duckdb.__version__)
        self.assertTrue(receipt["ducklake_extension_version"])
        self.assertIsNone(rows[0]["subject"])
        self.assertEqual(rows[0]["tags"], ["é", "synthetic"])
        self.assertEqual(rows[0]["status"], "open")
        self.assertEqual(rows[1]["body"], "old body")
        self.output.unlink()
        receipt = self.export(latest)
        self.assertEqual(receipt["rows"], 1)
        self.assertEqual(json.loads(self.output.read_text())["subject"], "new subject")

    def test_versions_and_existing_local_paths_are_required(self):
        for version in [-1, True, None, 1.5]:
            with self.subTest(version=version), self.assertRaises(ValueError):
                export_ducklake_snapshot(self.catalog, self.data, version, self.query, self.output)
            self.assert_unpublished()
        missing = self.output.parent / "missing.sqlite"
        with self.assertRaises(FileNotFoundError):
            export_ducklake_snapshot(missing, self.data, self.version, self.query, self.output)
        self.assertFalse(missing.exists())
        with self.assertRaises(FileNotFoundError):
            export_ducklake_snapshot(self.catalog, self.output.parent / "missing-data",
                                    self.version, self.query, self.output)
        with self.assertRaises(ValueError):
            export_ducklake_snapshot(self.data, self.data, self.version, self.query, self.output)
        with self.assertRaises(ValueError):
            export_ducklake_snapshot(self.catalog, self.catalog, self.version, self.query, self.output)
        self.assert_unpublished()

    def test_lexical_symlink_path_is_preserved_and_other_directory_is_refused(self):
        self.assertTrue(self.data.is_symlink())
        receipt = self.export()
        self.assertEqual(receipt["rows"], 2)
        self.output.unlink()
        other = self.output.parent / "other-data"
        other.mkdir()
        with self.assertRaises(self.duckdb.Error):
            export_ducklake_snapshot(self.catalog, other, self.version, self.query, self.output)
        self.assert_unpublished()
        with self.assertRaises(self.duckdb.Error):
            export_ducklake_snapshot(self.catalog, self.data.resolve(), self.version,
                                    self.query, self.output)
        self.assert_unpublished()

    def test_nonexistent_version_and_uninitialized_catalog_do_not_publish(self):
        with self.assertRaises(self.duckdb.Error):
            self.export(self.version + 1000)
        self.assert_unpublished()
        empty = self.output.parent / "empty.sqlite"
        empty.touch()
        with self.assertRaises(self.duckdb.Error):
            export_ducklake_snapshot(empty, self.data, 0, self.query, self.output)
        self.assertEqual(empty.stat().st_size, 0)
        self.assert_unpublished()

    def test_zero_is_valid_snapshot_and_validation_bounds_are_reused(self):
        receipt = export_ducklake_snapshot(self.catalog, self.data, 0,
                                            "SELECT NULL AS record_type WHERE false", self.output)
        self.assertEqual(receipt["snapshot_version"], 0)
        self.assertEqual(receipt["rows"], 0)
        self.output.unlink()
        self.assert_unpublished()
        with self.assertRaisesRegex(ValueError, "line-bytes"):
            self.export(line_bytes=1)
        self.assert_unpublished()
        with self.assertRaisesRegex(ValueError, "exactly one SELECT"):
            export_ducklake_snapshot(self.catalog, self.data, self.version,
                                    "DELETE FROM source_lake.main.source", self.output)
        self.assert_unpublished()

    def test_cli_requires_explicit_version_and_emits_receipt(self):
        command = [sys.executable, "-m", "examples.client.export_ducklake_snapshot",
                   "--catalog", str(self.catalog), "--data-path", str(self.data),
                   "--query-file", str(QUERY_FILE), "--output", str(self.output)]
        missing = subprocess.run(command, text=True, capture_output=True)
        self.assertEqual(missing.returncode, 2)
        self.assert_unpublished()
        result = subprocess.run(command + ["--snapshot-version", str(self.version)],
                                check=True, text=True, capture_output=True)
        receipt = json.loads(result.stdout)
        content = self.output.read_bytes()
        self.assertEqual(receipt["snapshot_version"], self.version)
        self.assertEqual(receipt["rows"], 2)
        self.assertEqual(receipt["tickets"], 1)
        self.assertEqual(receipt["comments"], 1)
        self.assertEqual(receipt["bytes"], len(content))
        self.assertEqual(receipt["sha256"], hashlib.sha256(content).hexdigest())
        self.assertEqual(receipt["duckdb_version"], self.duckdb.__version__)
        rows = [json.loads(line) for line in content.splitlines()]
        self.assertEqual([normalize_row(row) for row in rows], rows)
        repeated = subprocess.run(command + ["--snapshot-version", str(self.version)],
                                  text=True, capture_output=True)
        self.assertNotEqual(repeated.returncode, 0)
        self.assertIn("FileExistsError", repeated.stderr)
        self.assertEqual(self.output.read_bytes(), content)
        self.assertEqual(list(self.output.parent.glob(".snapshot-*.tmp")), [])


if __name__ == "__main__":
    unittest.main()
