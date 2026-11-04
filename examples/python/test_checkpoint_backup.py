import json
import os
import tempfile
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from threading import Barrier
from unittest.mock import patch

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

    def test_copy_refuses_destination_inside_source(self):
        alias = self.root / "source-alias"
        alias.symlink_to(self.source, target_is_directory=True)
        for destination in (self.source, self.source / "nested-copy", alias / "alias-copy"):
            with self.subTest(destination=destination), self.assertRaises(ValueError):
                copy_checkpoint(self.source, destination, receipt())
        self.assertEqual(sorted(path.relative_to(self.source).as_posix()
                                for path in self.source.rglob("*")),
                         ["000010.sst", "CURRENT", "sub", "sub/OPTIONS"])

    def test_copy_accepts_external_destination_through_directory_symlink(self):
        external = self.root / "external"
        external.mkdir()
        alias = self.root / "external-alias"
        alias.symlink_to(external, target_is_directory=True)
        destination = alias / "copy"
        copy_checkpoint(self.source, destination, receipt())
        self.assertEqual(verify_copy(external / "copy"), [])

    def test_copy_refuses_case_alias_of_source_on_case_insensitive_filesystem(self):
        alternate = self.source.with_name(self.source.name.upper())
        if not alternate.exists() or not alternate.samefile(self.source):
            self.skipTest("case-insensitive filesystem required")
        with self.assertRaises(ValueError):
            copy_checkpoint(self.source, alternate / "nested-copy", receipt())
        self.assertFalse((self.source / "nested-copy").exists())

    def test_verify_reports_changed_missing_and_extra_files(self):
        destination = self.root / "copy"
        copy_checkpoint(self.source, destination, receipt())
        (destination / "000010.sst").write_bytes(b"tampered")
        (destination / "CURRENT").unlink()
        (destination / "stray").write_text("x")
        problems = verify_copy(destination)
        self.assertEqual(sorted(problems), ["changed: 000010.sst", "extra: stray", "missing: CURRENT"])

    def test_copy_preserves_manifest_named_files_below_root(self):
        nested = self.source / "sub" / "brainwire-manifest.json"
        nested.write_text("nested checkpoint data")
        destination = self.root / "copy"
        manifest = copy_checkpoint(self.source, destination, receipt())
        self.assertIn("sub/brainwire-manifest.json", [entry["path"] for entry in manifest["files"]])
        self.assertEqual((destination / "sub" / nested.name).read_text(), nested.read_text())
        self.assertEqual(verify_copy(destination), [])

    def test_verify_reports_extra_nested_manifest_named_file(self):
        destination = self.root / "copy"
        copy_checkpoint(self.source, destination, receipt())
        (destination / "sub" / "brainwire-manifest.json").write_text("unlisted")
        self.assertEqual(verify_copy(destination), ["extra: sub/brainwire-manifest.json"])

    def test_verify_rejects_invalid_manifest_inventory(self):
        destination = self.root / "copy"
        original = copy_checkpoint(self.source, destination, receipt())
        path = destination / "brainwire-manifest.json"
        current = next(entry for entry in original["files"] if entry["path"] == "CURRENT")
        cases = [
            [], {}, {**original, "files": []}, {**original, "files": "CURRENT"},
            {**original, "files": [entry for entry in original["files"] if entry["path"] != "CURRENT"]},
            {**original, "files": [*original["files"], current]},
            {**original, "files": [*original["files"], "bad entry"]},
        ]
        for field, value in [
            ("path", "../CURRENT"), ("path", "/CURRENT"), ("path", "./CURRENT"),
            ("path", "sub//CURRENT"), ("path", "sub\\CURRENT"),
            ("path", "brainwire-manifest.json"), ("size", -1), ("size", True),
            ("sha256", "not a hash"),
        ]:
            cases.append({**original, "files": [{**current, field: value},
                                               *[e for e in original["files"] if e is not current]]})
        for manifest in cases:
            with self.subTest(manifest=manifest):
                path.write_text(json.dumps(manifest))
                self.assertTrue(verify_copy(destination))

    def test_verify_refuses_manifest_symlink(self):
        destination = self.root / "copy"
        copy_checkpoint(self.source, destination, receipt())
        path = destination / "brainwire-manifest.json"
        external = self.root / "external.json"
        path.rename(external)
        path.symlink_to(external)
        with patch.object(Path, "read_text", side_effect=AssertionError("symlink must not be read")):
            self.assertTrue(verify_copy(destination))

    @unittest.skipUnless(hasattr(os, "mkfifo"), "POSIX FIFO support required")
    def test_verify_refuses_manifest_fifo_without_reading_it(self):
        destination = self.root / "copy"
        copy_checkpoint(self.source, destination, receipt())
        path = destination / "brainwire-manifest.json"
        path.unlink()
        os.mkfifo(path)
        with patch.object(Path, "read_text", side_effect=AssertionError("FIFO must not be read")):
            self.assertTrue(verify_copy(destination))

    @unittest.skipUnless(hasattr(os, "mkfifo"), "POSIX FIFO support required")
    def test_copy_and_verify_refuse_nonregular_inventory_objects(self):
        destination = self.root / "copy"
        copy_checkpoint(self.source, destination, receipt())
        os.mkfifo(destination / "extra-pipe")
        with self.assertRaises(ValueError):
            verify_copy(destination)
        os.mkfifo(self.source / "extra-pipe")
        with self.assertRaises(ValueError):
            copy_checkpoint(self.source, self.root / "second", receipt())

    def test_copy_refuses_manifest_named_symlinks_at_any_depth(self):
        external = self.root / "external.json"
        external.write_text("unrelated")
        for directory in (self.source, self.source / "sub"):
            link = directory / "brainwire-manifest.json"
            link.symlink_to(external)
            with self.subTest(directory=directory), self.assertRaises(ValueError):
                copy_checkpoint(self.source, self.root / "copy", receipt())
            link.unlink()

    def test_verify_rejects_inventory_without_current_even_when_files_match(self):
        destination = self.root / "copy"
        manifest = copy_checkpoint(self.source, destination, receipt())
        (destination / "CURRENT").unlink()
        manifest["files"] = [entry for entry in manifest["files"] if entry["path"] != "CURRENT"]
        (destination / "brainwire-manifest.json").write_text(json.dumps(manifest))
        self.assertEqual(verify_copy(destination), ["manifest must include CURRENT"])

    def test_verify_rejects_empty_inventory_in_empty_directory(self):
        destination = self.root / "empty"
        destination.mkdir()
        (destination / "brainwire-manifest.json").write_text(json.dumps({
            "format": "brainwire-checkpoint-copy/1", "files": []}))
        self.assertTrue(verify_copy(destination))

    def test_verify_preserves_missing_and_malformed_json_errors(self):
        with self.assertRaises(FileNotFoundError):
            verify_copy(self.root / "missing")
        (self.source / "brainwire-manifest.json").write_text("invalid json")
        with self.assertRaises(json.JSONDecodeError):
            verify_copy(self.source)

    def test_copy_rejects_paths_its_inventory_cannot_represent(self):
        (self.source / "sub\\data").write_text("data")
        destination = self.root / "copy"
        with self.assertRaises(ValueError):
            copy_checkpoint(self.source, destination, receipt())
        self.assertFalse(destination.exists())

    def test_copy_verifies_existing_root_manifest_before_recopy(self):
        (self.source / "brainwire-manifest.json").write_text("unrelated file")
        destination = self.root / "copy"
        with self.assertRaises(ValueError):
            copy_checkpoint(self.source, destination, receipt())
        self.assertFalse(destination.exists())
        self.assertFalse((self.root / "copy.partial").exists())

    def test_valid_manifest_copy_can_be_copied_again(self):
        first_root = self.root / "first"
        first_root.mkdir()
        first = first_root / self.source.name
        copy_checkpoint(self.source, first, receipt())
        destination = self.root / "copy"
        copy_checkpoint(first, destination, receipt())
        self.assertEqual(verify_copy(destination), [])

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

    def test_copy_refuses_dangling_destination_symlink(self):
        destination = self.root / "copy"
        destination.symlink_to(self.root / "missing-target")
        with self.assertRaises(FileExistsError):
            copy_checkpoint(self.source, destination, receipt())
        self.assertTrue(destination.is_symlink())
        self.assertFalse((self.root / "copy.partial").exists())

    def test_losing_staging_race_does_not_remove_other_copy(self):
        destination = self.root / "copy"
        partial = self.root / "copy.partial"
        original_mkdir = Path.mkdir

        def competing_mkdir(path, *args, **kwargs):
            if path == partial:
                original_mkdir(path)
                (path / "in-progress").write_text("owned by the other copy")
                raise FileExistsError(str(path))
            return original_mkdir(path, *args, **kwargs)

        with patch.object(Path, "mkdir", competing_mkdir), self.assertRaises(FileExistsError):
            copy_checkpoint(self.source, destination, receipt())
        self.assertEqual((partial / "in-progress").read_text(), "owned by the other copy")

    def test_copy_rechecks_destination_after_acquiring_staging(self):
        destination = self.root / "copy"
        partial = self.root / "copy.partial"
        original_mkdir = Path.mkdir

        def late_mkdir(path, *args, **kwargs):
            result = original_mkdir(path, *args, **kwargs)
            if path == partial and not kwargs.get("exist_ok", False):
                original_mkdir(destination)
                (destination / "finished").write_text("existing backup")
            return result

        with patch.object(Path, "mkdir", late_mkdir), patch("shutil.copyfile") as copy_file:
            with self.assertRaises(FileExistsError):
                copy_checkpoint(self.source, destination, receipt())
            copy_file.assert_not_called()
        self.assertEqual((destination / "finished").read_text(), "existing backup")
        self.assertFalse(partial.exists())

    def test_sync_failure_after_rename_keeps_reused_staging_directory(self):
        destination = self.root / "copy"
        partial = self.root / "copy.partial"
        from examples.python import checkpoint_backup
        original_fsync = checkpoint_backup._fsync

        def failing_parent_sync(path):
            if path == self.root:
                partial.mkdir()
                (partial / "in-progress").write_text("owned by the other copy")
                raise OSError("injected parent sync failure")
            original_fsync(path)

        with patch.object(checkpoint_backup, "_fsync", failing_parent_sync):
            with self.assertRaises(OSError):
                copy_checkpoint(self.source, destination, receipt())
        self.assertEqual(verify_copy(destination), [])
        self.assertEqual((partial / "in-progress").read_text(), "owned by the other copy")

    def test_copy_failure_removes_own_staging_directory(self):
        destination = self.root / "copy"
        partial = self.root / "copy.partial"
        with patch("examples.python.checkpoint_backup._fsync", side_effect=OSError("injected")):
            with self.assertRaises(OSError):
                copy_checkpoint(self.source, destination, receipt())
        self.assertFalse(partial.exists())
        self.assertFalse(destination.exists())

    def test_competing_copies_leave_one_valid_backup(self):
        destination = self.root / "copy"
        partial = self.root / "copy.partial"
        original_mkdir = Path.mkdir
        barrier = Barrier(2, timeout=5)

        def competing_mkdir(path, *args, **kwargs):
            if path == partial and not kwargs.get("exist_ok", False):
                barrier.wait()
            return original_mkdir(path, *args, **kwargs)

        def attempt():
            try:
                copy_checkpoint(self.source, destination, receipt())
                return "copied"
            except FileExistsError:
                return "exists"

        with patch.object(Path, "mkdir", competing_mkdir):
            with ThreadPoolExecutor(max_workers=2) as pool:
                results = list(pool.map(lambda _: attempt(), range(2)))
        self.assertCountEqual(results, ["copied", "exists"])
        self.assertEqual(verify_copy(destination), [])

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
