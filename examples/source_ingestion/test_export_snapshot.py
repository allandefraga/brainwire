"""Optional real DuckDB tests: uv run --no-project --with duckdb==1.5.5 python
-m unittest examples.source_ingestion.test_export_snapshot -v
"""
from contextlib import redirect_stderr
import hashlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

from examples.python.resumable_customers import read_batch
from examples.source_ingestion import export_snapshot as exporter
from examples.source_ingestion.ingest_snapshot import CONTRACT_ID, normalize_row


QUERY = "SELECT * FROM source ORDER BY position"


class ObservedConnection:
    def __init__(self, connection, fail=None):
        self.connection, self.fail = connection, fail
        self.fetch_sizes, self.calls = [], []

    def extract_statements(self, query):
        return self.connection.extract_statements(query)

    def execute(self, query):
        self.calls.append(query)
        if query == self.fail:
            raise RuntimeError("synthetic source failure")
        self.connection.execute(query)
        return self

    @property
    def description(self):
        return self.connection.description

    def fetchall(self):
        return self.connection.fetchall()

    def fetchmany(self, size):
        self.fetch_sizes.append(size)
        return self.connection.fetchmany(size)


class ExportTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        try:
            import duckdb
        except ImportError:
            raise unittest.SkipTest("optional DuckDB dependency is not installed")
        cls.duckdb = duckdb

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.output = Path(self.directory.name) / "snapshot.jsonl"
        self.connection = self.duckdb.connect()
        self.addCleanup(self.connection.close)
        self.connection.execute("""CREATE TABLE source (
            position INTEGER, record_type VARCHAR, tenant_id VARCHAR, connector_id VARCHAR,
            external_id VARCHAR, subject VARCHAR, status VARCHAR, priority VARCHAR,
            tags VARCHAR[], ticket_id VARCHAR, body VARCHAR, is_public BOOLEAN)""")

    def add_ticket(self, position=0, external="t1", tags=None, subject=None):
        self.connection.execute("INSERT INTO source VALUES (?, 'ticket', 'tenant/é', 'zendesk', ?, ?, 'open', NULL, ?, NULL, NULL, NULL)",
                                [position, external, subject, tags])

    def add_comment(self, position=1):
        self.connection.execute("INSERT INTO source VALUES (?, 'comment', 'tenant/é', 'zendesk', 'c1', NULL, NULL, NULL, NULL, 't1', NULL, NULL)", [position])

    def assert_unpublished(self):
        self.assertFalse(self.output.exists())
        self.assertEqual(list(self.output.parent.glob(".snapshot-*.tmp")), [])

    def test_batches_preserve_nulls_lists_identities_and_mapper_compatibility(self):
        self.add_ticket(tags=["é", "synthetic"])
        self.add_ticket(1, "t2")
        self.add_comment(2)
        connection = ObservedConnection(self.connection)
        receipt = exporter.export_snapshot(connection, QUERY, self.output, fetch_rows=1)
        content = self.output.read_bytes()
        rows = [json.loads(line) for line in content.splitlines()]
        self.assertEqual([normalize_row(row) for row in rows], rows)
        self.assertEqual(rows[0]["tags"], ["é", "synthetic"])
        self.assertIsNone(rows[0]["subject"])
        self.assertIsNone(rows[1]["tags"])
        self.assertIsNone(rows[2]["is_public"])
        self.assertEqual(rows[2]["ticket_entity_id"], rows[0]["entity_id"])
        self.assertEqual(connection.fetch_sizes, [1, 1, 1, 1])
        self.assertEqual(receipt, dict(rows=3, tickets=2, comments=1, bytes=len(content),
                                      sha256=hashlib.sha256(content).hexdigest(), mapping_contract_id=CONTRACT_ID))
        offset, mapped = 0, []
        while offset < len(content):
            batch, offset = read_batch(self.output, offset, len(content), 1, 65536, 65536,
                                      normalize_row=normalize_row)
            mapped.extend(batch)
        self.assertEqual(mapped, rows)
        self.assertEqual(list(self.output.parent.glob(".snapshot-*.tmp")), [])

    def test_late_invalid_row_and_order_failure_clean_temporary_file(self):
        self.add_ticket()
        self.add_ticket(1, "")
        with self.assertRaisesRegex(ValueError, "external_id"):
            exporter.export_snapshot(self.connection, QUERY, self.output, fetch_rows=1)
        self.assert_unpublished()
        self.connection.execute("DELETE FROM source WHERE position = 1")
        self.add_comment(1)
        self.add_ticket(2, "t2")
        with self.assertRaisesRegex(ValueError, "precede"):
            exporter.export_snapshot(self.connection, QUERY, self.output, fetch_rows=1)
        self.assert_unpublished()
        self.assertEqual(self.connection.execute("SELECT count(*) FROM source").fetchone()[0], 3)

    def test_unique_columns_and_positive_bounds(self):
        self.add_ticket()
        with self.assertRaisesRegex(ValueError, "unique"):
            exporter.export_snapshot(self.connection, "SELECT 1 AS x, 2 AS x", self.output)
        self.assert_unpublished()
        for name, value in [("fetch_rows", 0), ("fetch_rows", True), ("line_bytes", -1),
                            ("max_output_bytes", 1.5)]:
            with self.subTest(name=name, value=value), self.assertRaises(ValueError):
                exporter.export_snapshot(self.connection, QUERY, self.output, **{name: value})
            self.assert_unpublished()

    def test_encoded_line_and_total_byte_limits(self):
        self.add_ticket(subject="é" * 200)
        with self.assertRaisesRegex(ValueError, "line-bytes"):
            exporter.export_snapshot(self.connection, QUERY, self.output, line_bytes=300)
        self.assert_unpublished()
        receipt = exporter.export_snapshot(self.connection, QUERY, self.output)
        size = receipt["bytes"]
        self.output.unlink()
        self.add_ticket(1, "t2", subject="é" * 200)
        with self.assertRaisesRegex(ValueError, "max-output-bytes"):
            exporter.export_snapshot(self.connection, QUERY, self.output, fetch_rows=1, max_output_bytes=size)
        self.assert_unpublished()
        self.connection.execute("DELETE FROM source WHERE position = 1")
        receipt = exporter.export_snapshot(self.connection, QUERY, self.output, line_bytes=size, max_output_bytes=size)
        self.assertEqual(receipt["bytes"], size)

    def test_no_dml_or_multiple_statements(self):
        self.add_ticket()
        for query in ["DELETE FROM source", "SELECT * FROM source; DELETE FROM source", "", "BEGIN", "CREATE TABLE bad AS SELECT 1"]:
            with self.subTest(query=query), self.assertRaises(ValueError):
                exporter.export_snapshot(self.connection, query, self.output)
            self.assert_unpublished()
        self.assertEqual(self.connection.execute("SELECT count(*) FROM source").fetchone()[0], 1)

    def test_existing_output_and_publication_race_leave_original_untouched(self):
        self.add_ticket()
        self.output.write_bytes(b"existing input")
        with self.assertRaises(FileExistsError):
            exporter.export_snapshot(self.connection, QUERY, self.output)
        self.assertEqual(self.output.read_bytes(), b"existing input")
        self.output.unlink()
        real_link = exporter.os.link
        observed = ObservedConnection(self.connection)

        def race(source, destination):
            self.assertEqual(observed.calls[-1], "COMMIT")
            Path(destination).write_bytes(b"racing input")
            real_link(source, destination)

        with patch.object(exporter.os, "link", side_effect=race), self.assertRaises(FileExistsError):
            exporter.export_snapshot(observed, QUERY, self.output)
        self.assertEqual(self.output.read_bytes(), b"racing input")
        self.assertEqual(list(self.output.parent.glob(".snapshot-*.tmp")), [])

    def test_active_caller_transaction_is_rejected_without_abort(self):
        self.connection.execute("BEGIN")
        self.add_ticket()
        observed = ObservedConnection(self.connection)
        with self.assertRaisesRegex(ValueError, "active transaction"):
            exporter.export_snapshot(observed, QUERY, self.output)
        self.assertEqual(observed.calls, ["SELECT current_transaction_id()"] * 2)
        self.assert_unpublished()
        self.assertEqual(self.connection.execute("SELECT count(*) FROM source").fetchone()[0], 1)
        self.connection.execute("ROLLBACK")
        self.assertEqual(self.connection.execute("SELECT count(*) FROM source").fetchone()[0], 0)

    def test_active_caller_can_commit_after_refusal(self):
        self.connection.execute("BEGIN")
        self.add_ticket()
        with self.assertRaisesRegex(ValueError, "active transaction"):
            exporter.export_snapshot(self.connection, QUERY, self.output)
        self.connection.execute("COMMIT")
        self.assertEqual(self.connection.execute("SELECT count(*) FROM source").fetchone()[0], 1)
        self.assert_unpublished()

    def test_introspection_error_never_begins_or_rolls_back(self):
        observed = ObservedConnection(self.connection, "SELECT current_transaction_id()")
        with self.assertRaisesRegex(RuntimeError, "source failure"):
            exporter.export_snapshot(observed, QUERY, self.output)
        self.assertEqual(observed.calls, ["SELECT current_transaction_id()"])
        self.assert_unpublished()

    def test_failed_begin_never_rolls_back_or_commits(self):
        observed = ObservedConnection(self.connection, "BEGIN TRANSACTION")
        with self.assertRaisesRegex(RuntimeError, "source failure"):
            exporter.export_snapshot(observed, QUERY, self.output)
        self.assertEqual(observed.calls, ["SELECT current_transaction_id()"] * 2 + ["BEGIN TRANSACTION"])
        self.assert_unpublished()

    def test_source_and_commit_errors_do_not_publish(self):
        self.add_ticket()
        for failure in [QUERY, "COMMIT"]:
            observed = ObservedConnection(self.connection, failure)
            with self.subTest(failure=failure), self.assertRaisesRegex(RuntimeError, "source failure"):
                exporter.export_snapshot(observed, QUERY, self.output)
            self.assertEqual(observed.calls[-1], "ROLLBACK")
            self.assert_unpublished()

    def test_cleanup_failure_preserves_validation_error_and_success_receipt(self):
        self.add_ticket()
        for fails_validation in (True, False):
            warning = io.StringIO()
            with self.subTest(fails_validation=fails_validation):
                with patch.object(Path, "unlink", side_effect=PermissionError("synthetic cleanup failure")), redirect_stderr(warning):
                    if fails_validation:
                        with self.assertRaisesRegex(ValueError, "line-bytes"):
                            exporter.export_snapshot(self.connection, QUERY, self.output, line_bytes=1)
                        self.assertFalse(self.output.exists())
                    else:
                        receipt = exporter.export_snapshot(self.connection, QUERY, self.output)
                        self.assertEqual(receipt["rows"], 1)
                        self.assertEqual(receipt["sha256"], hashlib.sha256(self.output.read_bytes()).hexdigest())
                self.assertIn("could not remove temporary snapshot", warning.getvalue())
                leftovers = list(self.output.parent.glob(".snapshot-*.tmp"))
                self.assertEqual(len(leftovers), 1)
                leftovers[0].unlink()
                self.output.unlink(missing_ok=True)

    def test_process_exit_mid_read_never_publishes_final_input(self):
        code = """
import os
import sys
import duckdb
from examples.source_ingestion import export_snapshot as exporter
connection = duckdb.connect()
query = "SELECT 'ticket' AS record_type, 'tenant' AS tenant_id, 'zendesk' AS connector_id, i::VARCHAR AS external_id, NULL::VARCHAR AS subject, NULL::VARCHAR AS status, NULL::VARCHAR AS priority, []::VARCHAR[] AS tags FROM range(3) v(i)"
original = exporter.normalize_row
calls = 0
def crash(row):
    global calls
    calls += 1
    if calls == 2:
        os._exit(23)
    return original(row)
exporter.normalize_row = crash
exporter.export_snapshot(connection, query, sys.argv[1], fetch_rows=1)
"""
        result = subprocess.run([sys.executable, "-c", code, str(self.output)], capture_output=True)
        self.assertEqual(result.returncode, 23, result.stderr.decode())
        self.assertFalse(self.output.exists())
        leftovers = list(self.output.parent.glob(".snapshot-*.tmp"))
        self.assertEqual(len(leftovers), 1)
        self.assertEqual(leftovers[0].stat().st_mode & 0o777, 0o600)

    def test_empty_snapshot_and_read_only_cli(self):
        database = Path(self.directory.name) / "source.duckdb"
        with self.duckdb.connect(str(database)) as connection:
            connection.execute("CREATE TABLE source AS SELECT * FROM (VALUES ('ticket')) v(record_type) WHERE false")
        query = Path(self.directory.name) / "query.sql"
        query.write_text("SELECT * FROM source", encoding="utf-8")
        result = subprocess.run([sys.executable, "-m", "examples.source_ingestion.export_snapshot",
                                 "--database", str(database), "--query-file", str(query),
                                 "--output", str(self.output)], check=True, text=True, capture_output=True)
        receipt = json.loads(result.stdout)
        self.assertEqual(receipt["rows"], 0)
        self.assertEqual(receipt["sha256"], hashlib.sha256(b"").hexdigest())
        self.assertEqual(self.output.read_bytes(), b"")


if __name__ == "__main__":
    unittest.main()
