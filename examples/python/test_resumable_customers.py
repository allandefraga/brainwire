"""Run: python3 -m unittest discover -s examples/python -p test_resumable_customers.py"""

import copy
import json
from pathlib import Path
import tempfile
import unittest

import resumable_customers as importer


class Result:
    def __init__(self, record=None):
        self.record = record

    def single(self):
        return self.record

    def consume(self):
        return None


class Transaction:
    def __init__(self, driver):
        self.driver = driver
        self.checkpoints = copy.deepcopy(driver.checkpoints)
        self.customers = copy.deepcopy(driver.customers)

    def run(self, query, **params):
        if query == importer.CHECKPOINT:
            return Result(self.checkpoints.get(params["source_id"]))
        if query == importer.INITIALIZE:
            self.checkpoints[params["source_id"]] = {
                "sha256": params["sha256"], "next_offset": 0, "contract_id": params.get("contract_id"),
            }
        elif query == importer.CUSTOMERS:
            self.driver.attempted_batches.append(copy.deepcopy(params["rows"]))
            for row in params["rows"]:
                self.customers[row["entity_id"]] = row
        elif query == importer.ADVANCE:
            self.checkpoints[params["source_id"]]["next_offset"] = params["next_offset"]
        else:
            raise AssertionError(query)
        return Result()


class Driver:
    def __init__(self, retry=None, interrupt=False):
        self.checkpoints = {}
        self.customers = {}
        self.attempted_batches = []
        self.retry = retry
        self.interrupt = interrupt
        self.commits = 0

    def session(self, **kwargs):
        return self

    def __enter__(self):
        return self

    def __exit__(self, *args):
        return False

    def execute_write(self, callback):
        tx = Transaction(self)
        result = callback(tx)
        retry, self.retry = self.retry, None
        if retry == "before_commit":
            # Drop the entire attempt, as a driver retryable transaction would.
            tx = Transaction(self)
            result = callback(tx)
        self.checkpoints, self.customers = tx.checkpoints, tx.customers
        self.commits += 1
        if self.interrupt:
            self.interrupt = False
            raise RuntimeError("stopped after durable commit, before acknowledgement")
        if retry == "after_commit":
            tx = Transaction(self)
            result = callback(tx)
            self.checkpoints, self.customers = tx.checkpoints, tx.customers
            self.commits += 1
        return result


def customer(entity_id):
    return {"entity_id": entity_id, "email": f"{entity_id}@example.test",
            "active": True, "tags": ["synthetic"], "embedding": [0.25]}


class ResumableCustomersTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.path = Path(self.directory.name) / "customers.jsonl"
        self.lines = [(json.dumps(customer(str(i))) + "\n").encode() for i in range(3)]
        self.path.write_bytes(b"".join(self.lines))

    def ingest(self, driver, **kwargs):
        return importer.ingest(driver, "customers", self.path, "synthetic-source",
                               batch_size=1, **kwargs)

    def test_custom_hooks_require_contract_and_completed_mapping_mismatch_fails(self):
        driver = Driver()
        self.ingest(driver)
        before = copy.deepcopy((driver.checkpoints, driver.customers))
        with self.assertRaisesRegex(ValueError, "contract"):
            self.ingest(driver, normalize_row=lambda row: row)
        with self.assertRaisesRegex(ValueError, "contract"):
            self.ingest(driver, normalize_row=lambda row: row, contract_id="another-mapping")
        self.assertEqual((driver.checkpoints, driver.customers), before)

    def test_matching_custom_contract_resumes_and_mismatch_rejects_completed_source(self):
        driver = Driver()
        hook = lambda row: importer.normalize_customer(row)
        self.ingest(driver, normalize_row=hook, contract_id="mapping-v1")
        self.assertEqual(driver.checkpoints["synthetic-source"]["contract_id"], "mapping-v1")
        before = copy.deepcopy((driver.checkpoints, driver.customers, driver.attempted_batches))
        self.ingest(driver, normalize_row=hook, contract_id="mapping-v1")
        self.assertEqual((driver.checkpoints, driver.customers, driver.attempted_batches), before)
        with self.assertRaisesRegex(ValueError, "contract"):
            self.ingest(driver, normalize_row=hook, contract_id="mapping-v2")
        with self.assertRaisesRegex(ValueError, "contract"):
            self.ingest(driver)
        self.assertEqual((driver.checkpoints, driver.customers, driver.attempted_batches), before)

    def test_legacy_checkpoint_accepts_only_default_mapping(self):
        driver = Driver()
        self.ingest(driver)
        driver.checkpoints["synthetic-source"].pop("contract_id")
        self.ingest(driver)
        with self.assertRaisesRegex(ValueError, "contract"):
            self.ingest(driver, normalize_row=importer.normalize_customer, contract_id="custom")

    def test_writer_failure_does_not_advance_cursor_or_initialize_checkpoint(self):
        def fail(tx, rows):
            tx.run(importer.CUSTOMERS, rows=rows).consume()
            raise ValueError("writer failed")
        driver = Driver()
        with self.assertRaisesRegex(ValueError, "writer failed"):
            self.ingest(driver, write_batch=fail, contract_id="test-mapping")
        self.assertEqual(driver.checkpoints, {})
        self.assertEqual(driver.customers, {})
        self.assertEqual(driver.commits, 0)

    def test_retry_before_commit_replays_identical_batch_and_cursor(self):
        driver = Driver(retry="before_commit")
        self.assertEqual(self.ingest(driver), self.path.stat().st_size)
        self.assertEqual(driver.attempted_batches[0], driver.attempted_batches[1])
        self.assertEqual(set(driver.customers), {"0", "1", "2"})
        self.assertEqual(driver.checkpoints["synthetic-source"]["next_offset"], self.path.stat().st_size)

    def test_lost_ack_retry_reads_durable_checkpoint_and_resume_is_noop(self):
        driver = Driver(retry="after_commit")
        self.ingest(driver)
        self.assertEqual([batch[0]["entity_id"] for batch in driver.attempted_batches], ["0", "1", "2"])
        attempts = copy.deepcopy(driver.attempted_batches)
        self.ingest(driver)
        self.assertEqual(driver.attempted_batches, attempts)

    def test_interruption_resumes_from_graph_cursor_with_new_import_call(self):
        driver = Driver(interrupt=True)
        with self.assertRaisesRegex(RuntimeError, "before acknowledgement"):
            self.ingest(driver)
        self.assertEqual(driver.checkpoints["synthetic-source"]["next_offset"], len(self.lines[0]))
        self.assertEqual(set(driver.customers), {"0"})
        self.ingest(driver)
        self.assertEqual([batch[0]["entity_id"] for batch in driver.attempted_batches], ["0", "1", "2"])

    def test_digest_mismatch_rejects_before_customer_changes(self):
        driver = Driver()
        self.ingest(driver)
        before = copy.deepcopy((driver.checkpoints, driver.customers, driver.attempted_batches))
        self.path.write_bytes((json.dumps(customer("changed")) + "\n").encode())
        with self.assertRaisesRegex(ValueError, "different SHA256"):
            self.ingest(driver)
        self.assertEqual((driver.checkpoints, driver.customers, driver.attempted_batches), before)

    def test_malformed_later_row_keeps_earlier_commits_and_does_not_skip_on_resume(self):
        self.path.write_bytes(self.lines[0] + self.lines[1] + b'{"entity_id":"missing-fields"}\n')
        driver = Driver()
        with self.assertRaisesRegex(ValueError, "requires"):
            self.ingest(driver)
        self.assertEqual(set(driver.customers), {"0", "1"})
        self.assertEqual(driver.checkpoints["synthetic-source"]["next_offset"],
                         len(self.lines[0]) + len(self.lines[1]))
        before = copy.deepcopy((driver.checkpoints, driver.customers, driver.attempted_batches))
        with self.assertRaisesRegex(ValueError, "requires"):
            self.ingest(driver)
        self.assertEqual((driver.checkpoints, driver.customers, driver.attempted_batches), before)

    def test_row_and_byte_bounds_blank_lines_and_unterminated_final_line(self):
        self.path.write_bytes(b"\n\n" + self.lines[0] + b"\n" + self.lines[1].rstrip(b"\n"))
        size = self.path.stat().st_size
        offset = 0
        seen = []
        while offset < size:
            previous = offset
            rows, offset = importer.read_batch(self.path, offset, size, 1, 200, 200)
            self.assertLessEqual(len(rows), 1)
            self.assertLessEqual(offset - previous, 200)
            self.assertGreater(offset, previous)
            seen.extend(rows)
        self.assertEqual([row["entity_id"] for row in seen], ["0", "1"])
        with self.assertRaisesRegex(ValueError, "exceeds line-bytes"):
            importer.read_batch(self.path, 0, size, 10, 10, 10)

    def test_invalid_cursor_and_invalid_row_do_not_commit(self):
        digest, _ = importer.fingerprint(self.path)
        for offset in (-1, 1, True, importer.MAX_INTEGER + 1):
            driver = Driver()
            driver.checkpoints["synthetic-source"] = {"sha256": digest, "next_offset": offset}
            with self.assertRaises(ValueError):
                self.ingest(driver)
            self.assertEqual(driver.customers, {})
            self.assertEqual(driver.commits, 0)
        self.path.write_bytes(b'{"entity_id": "missing-fields"}\n')
        driver = Driver()
        with self.assertRaisesRegex(ValueError, "requires"):
            self.ingest(driver)
        self.assertEqual(driver.checkpoints, {})
        self.assertEqual(driver.commits, 0)

    def test_empty_and_blank_only_sources_commit_checkpoint_without_customers(self):
        for contents in (b"", b"\n\n\n\n\n"):
            self.path.write_bytes(contents)
            driver = Driver()
            self.assertEqual(self.ingest(driver, batch_bytes=2, line_bytes=2), len(contents))
            self.assertEqual(driver.checkpoints["synthetic-source"]["next_offset"], len(contents))
            self.assertEqual(driver.customers, {})
            self.assertEqual(driver.attempted_batches, [])

    def test_changed_source_during_callback_and_bad_limits_are_rejected(self):
        class MutatingDriver(Driver):
            def execute_write(inner, callback):
                self.path.write_bytes(b"changed\n")
                return super().execute_write(callback)

        driver = MutatingDriver()
        with self.assertRaisesRegex(ValueError, "source changed"):
            self.ingest(driver)
        self.assertEqual(driver.commits, 0)
        for limits in ({"batch_bytes": 0}, {"line_bytes": 0},
                       {"batch_bytes": 10, "line_bytes": 11}):
            with self.assertRaises(ValueError):
                self.ingest(Driver(), **limits)


if __name__ == "__main__":
    unittest.main()
