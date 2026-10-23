import json
import os
import tempfile
import unittest
from pathlib import Path

from examples.python.checkpoint_backup import copy_checkpoint, main, verify_copy
from examples.python.client import GraphClient


class Driver:
    def __init__(self):
        self.queries = []

    def session(self, *, database):
        self.database = database
        driver = self

        class Session:
            def __enter__(self):
                return self

            def __exit__(self, *_):
                return False

            def run(self, query, parameters):
                driver.queries.append((query, parameters))

                class Result:
                    def data(self):
                        if "db.schema" in query:
                            return [{"version": 3}]
                        return [{"name": parameters["name"], "path": "/backups/" + parameters["name"],
                                 "sequence": "42", "tracked": True}]
                return Result()

        return Session()


def receipt(name="nightly-1"):
    return {"graph": "research", "name": name, "path": "/backups/" + name,
            "sequence": "42", "tracked": True, "ontology_version": 3}


def store(root):
    root.mkdir()
    (root / "CURRENT").write_text("MANIFEST-000005\n")
    (root / "000010.sst").write_bytes(b"synthetic-sst" * 100)
    (root / "sub").mkdir()
    (root / "sub" / "OPTIONS").write_text("synthetic")
    return root


class CheckpointClientTest(unittest.TestCase):
    def test_checkpoint_returns_receipt_with_graph_and_ontology_version(self):
        driver = Driver()
        self.assertEqual(GraphClient(driver, "research").checkpoint("nightly-1"), receipt())
        self.assertEqual(driver.database, "research")
        self.assertIn("CALL db.checkpoint($name)", driver.queries[-1][0])
        self.assertEqual(driver.queries[-1][1], {"name": "nightly-1"})

    def test_checkpoint_rejects_paths_and_unsafe_names_before_connecting(self):
        driver = Driver()
        for bad in ["", ".hidden", "../x", "a/b", "x" * 129, None]:
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                GraphClient(driver, "research").checkpoint(bad)
        self.assertEqual(driver.queries, [])


class CheckpointCopyTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.source = store(self.root / "nightly-1")

    def tearDown(self):
        self.temp.cleanup()

    def test_copy_writes_manifest_and_verifies(self):
        destination = self.root / "offsite" / "nightly-1"
        destination.parent.mkdir()
        manifest = copy_checkpoint(self.source, destination, receipt())
        self.assertEqual(manifest["format"], "brainwire-checkpoint-copy/1")
        self.assertEqual(manifest["receipt"], receipt())
        self.assertEqual(sorted(f["path"] for f in manifest["files"]),
                         ["000010.sst", "CURRENT", "sub/OPTIONS"])
        self.assertEqual(json.loads((destination / "brainwire-manifest.json").read_text()), manifest)
        self.assertFalse(destination.with_name("nightly-1.partial").exists())
        self.assertEqual(verify_copy(destination), [])

    def test_verify_reports_changed_missing_and_extra_files(self):
        destination = self.root / "copy"
        copy_checkpoint(self.source, destination, receipt())
        (destination / "000010.sst").write_bytes(b"tampered")
        (destination / "CURRENT").unlink()
        (destination / "stray").write_text("x")
        problems = verify_copy(destination)
        self.assertEqual(sorted(problems), ["changed: 000010.sst", "extra: stray", "missing: CURRENT"])

    def test_copy_refuses_existing_destination_mismatched_receipt_and_symlinks(self):
        destination = self.root / "copy"
        destination.mkdir()
        with self.assertRaises(FileExistsError):
            copy_checkpoint(self.source, destination, receipt())
        with self.assertRaises(ValueError):
            copy_checkpoint(self.source, self.root / "other", receipt("different"))
        os.symlink(self.source / "CURRENT", self.source / "link")
        with self.assertRaises(ValueError):
            copy_checkpoint(self.source, self.root / "linked", receipt())
        self.assertFalse((self.root / "linked").exists())
        self.assertFalse((self.root / "linked.partial").exists())

    def test_cli_copy_and_verify_exit_codes(self):
        receipt_file = self.root / "receipt.json"
        receipt_file.write_text(json.dumps(receipt()))
        destination = self.root / "cli-copy"
        self.assertEqual(main(["copy", str(self.source), str(destination),
                               "--receipt", str(receipt_file)]), 0)
        self.assertEqual(main(["verify", str(destination)]), 0)
        (destination / "CURRENT").write_text("changed")
        self.assertEqual(main(["verify", str(destination)]), 1)


if __name__ == "__main__":
    unittest.main()
