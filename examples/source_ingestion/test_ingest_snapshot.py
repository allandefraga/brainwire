"""Run from the repository root: python3 -m unittest discover -s examples/source_ingestion."""
import unittest
from contextlib import redirect_stdout
from io import StringIO
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import MagicMock, patch
from examples.source_ingestion import ingest_snapshot as adapter


def ticket():
    return dict(record_type="ticket", tenant_id="synthetic-tenant", connector_id="zendesk-shadow",
                external_id="t1", subject="Synthetic ticket", status="open", priority=None,
                tags=["synthetic"])


def comment():
    return dict(record_type="comment", tenant_id="synthetic-tenant", connector_id="zendesk-shadow",
                external_id="c1", ticket_id="t1", body="Synthetic comment", is_public=True)


class Result:
    def __init__(self, rows=()):
        self.rows = rows
        self.consumed = False
    def __iter__(self):
        return iter(self.rows)
    def consume(self):
        self.consumed = True


class Transaction:
    def __init__(self, existing=(), missing=False):
        self.existing, self.missing = existing, missing
        self.calls, self.results = [], []
    def run(self, query, **params):
        self.calls.append(query)
        rows = []
        if query == adapter.EXISTING_COMMENTS:
            rows = self.existing
        elif query == adapter.COMMENT_TICKETS and not self.missing:
            rows = [{"entity_id": r["entity_id"], "ticket_entity_id": r["ticket_entity_id"]} for r in params["rows"]]
        result = Result(rows)
        self.results.append(result)
        return result


class SnapshotTests(unittest.TestCase):
    def test_cli_reports_complete_after_successful_import(self):
        with TemporaryDirectory() as directory:
            path = Path(directory) / "input.jsonl"
            path.write_text("{}\n")
            with patch("sys.argv", ["ingest_snapshot", str(path), "--source-id", "synthetic"]), \
                    patch("neo4j.GraphDatabase.driver", return_value=MagicMock()), \
                    patch.object(adapter, "ingest_snapshot", return_value=path.stat().st_size), \
                    redirect_stdout(StringIO()) as output:
                adapter.main()
            self.assertIn("complete", output.getvalue())

    def test_identity_is_collision_free_and_nullable_fields_are_authoritative(self):
        first = adapter.normalize_row(ticket())
        self.assertEqual(first["priority"], None)
        self.assertNotEqual(adapter.identity("a/b", "c", "ticket", "d"), adapter.identity("a", "b/c", "ticket", "d"))
        self.assertNotEqual(adapter.identity("a", "b", "ticket", "d"), adapter.identity("a", "b", "comment", "d"))
        normalized = adapter.normalize_row(comment())
        self.assertEqual(normalized["ticket_entity_id"], first["entity_id"])
        for value in ({}, [], None, {**ticket(), "external_id": 7}, {**ticket(), "tags": [1]},
                      {**ticket(), "priority": False}, {**comment(), "is_public": 1},
                      {**comment(), "ticket_id": ""}, {**comment(), "body": []}):
            with self.assertRaises(ValueError):
                adapter.normalize_row(value)

    def test_writer_consumes_queries_and_accounts_for_duplicate_rows(self):
        rows = [adapter.normalize_row(ticket()), adapter.normalize_row(comment())]
        tx = Transaction()
        adapter.write_batch(tx, [*rows, rows[1]])
        self.assertEqual(tx.calls, [adapter.TICKETS, adapter.EXISTING_COMMENTS,
                                   adapter.COMMENT_TICKETS, adapter.COMMENTS, adapter.HAS_COMMENTS])
        self.assertTrue(all(r.consumed for r in tx.results))

    def test_missing_ticket_and_parent_changes_fail_before_comment_upsert(self):
        row = adapter.normalize_row(comment())
        for tx, rows in [
            (Transaction(missing=True), [row]),
            (Transaction(existing=[{"entity_id": row["entity_id"], "ticket_entity_id": "other"}]), [row]),
            (Transaction(), [row, {**row, "ticket_entity_id": "other"}]),
        ]:
            with self.assertRaises(ValueError):
                adapter.write_batch(tx, rows)
            self.assertNotIn(adapter.COMMENTS, tx.calls)
            self.assertNotIn(adapter.HAS_COMMENTS, tx.calls)


if __name__ == "__main__":
    unittest.main()
