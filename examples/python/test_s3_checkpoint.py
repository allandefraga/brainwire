import base64
import hashlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from examples.python import checkpoint_backup
from examples.python import s3_checkpoint


URI = "s3://example-bucket/checkpoints/demo"
PREFIX = "checkpoints/demo/_brainwire/"


class S3:
    def __init__(self):
        self.objects = {}
        self.fail_key = None

    def get_paginator(self, operation):
        assert operation == "list_objects_v2"
        return self

    def paginate(self, *, Bucket, Prefix):
        for key in self.objects:
            if key.startswith(Prefix):
                yield {"Contents": [{"Key": key}]}

    def put_object(self, *, Bucket, Key, Body, ContentLength, IfNoneMatch, ChecksumSHA256):
        if Key == self.fail_key:
            raise OSError("injected upload failure")
        assert IfNoneMatch == "*" and Key not in self.objects
        data = Body if isinstance(Body, bytes) else Body.read()
        assert len(data) == ContentLength
        assert base64.b64encode(hashlib.sha256(data).digest()).decode() == ChecksumSHA256
        self.objects[Key] = data

    def get_object(self, *, Bucket, Key):
        data = self.objects[Key]
        return {"Body": io.BytesIO(data), "ContentLength": len(data)}


class S3CheckpointTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        root = Path(temp.name)
        source = root / "checkpoint"
        source.mkdir()
        (source / "CURRENT").write_bytes(b"MANIFEST-000001\n")
        (source / "000001.sst").write_bytes(b"fixture")
        self.copy = root / "backup"
        checkpoint_backup.copy_checkpoint(source, self.copy, {"name": source.name})
        self.destination = root / "restore"
        self.remote = S3()

    def test_roundtrip_and_no_reuse(self):
        self.assertEqual(s3_checkpoint.publish(self.remote, self.copy, URI), 2)
        self.assertEqual(list(self.remote.objects)[-1], PREFIX + checkpoint_backup.MANIFEST)
        with self.assertRaises(FileExistsError):
            s3_checkpoint.publish(self.remote, self.copy, URI)
        self.assertEqual(s3_checkpoint.restore(self.remote, URI, self.destination), 2)
        self.assertEqual(checkpoint_backup.verify_copy(self.destination), [])
        self.assertEqual(self.destination.stat().st_mode & 0o777, 0o700)
        self.assertEqual((self.destination / "CURRENT").stat().st_mode & 0o777, 0o600)

    def test_nested_backup_uris_have_separate_inventories(self):
        s3_checkpoint.publish(self.remote, self.copy, URI)
        s3_checkpoint.publish(self.remote, self.copy, URI + "/second")
        self.assertEqual(s3_checkpoint.restore(self.remote, URI, self.destination), 2)

    def test_upload_failure_does_not_publish_manifest(self):
        self.remote.fail_key = PREFIX + "CURRENT"
        with self.assertRaisesRegex(OSError, "injected"):
            s3_checkpoint.publish(self.remote, self.copy, URI)
        self.assertNotIn(PREFIX + checkpoint_backup.MANIFEST, self.remote.objects)

    def test_extra_objects_and_corrupted_objects_are_refused(self):
        s3_checkpoint.publish(self.remote, self.copy, URI)
        self.remote.objects[PREFIX + "not-in-manifest"] = b"extra"
        with self.assertRaisesRegex(ValueError, "unexpected object"):
            s3_checkpoint.restore(self.remote, URI, self.destination)
        del self.remote.objects[PREFIX + "not-in-manifest"]
        self.remote.objects[PREFIX + "000001.sst"] = b"altered"
        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            s3_checkpoint.restore(self.remote, URI, self.destination)
        self.assertFalse(self.destination.exists())
        self.assertFalse(self.destination.with_suffix(".partial").exists())

    def test_truncated_and_excessive_downloads_are_not_promoted(self):
        s3_checkpoint.publish(self.remote, self.copy, URI)
        original = self.remote.get_object
        for content in (b"short", b"fixture-more"):
            with self.subTest(content=content):
                def malformed(*, Bucket, Key):
                    result = original(Bucket=Bucket, Key=Key)
                    if Key.endswith(".sst"):
                        result["Body"] = io.BytesIO(content)
                    return result

                with patch.object(self.remote, "get_object", malformed):
                    with self.assertRaisesRegex(ValueError, "checksum mismatch"):
                        s3_checkpoint.restore(self.remote, URI, self.destination)
                self.assertFalse(self.destination.exists())
                self.assertFalse(self.destination.with_suffix(".partial").exists())

    def test_existing_partial_directory_belongs_to_other_restore(self):
        s3_checkpoint.publish(self.remote, self.copy, URI)
        partial = self.destination.with_suffix(".partial")
        partial.mkdir()
        sentinel = partial / "other-owner"
        sentinel.write_text("in progress")
        with self.assertRaises(FileExistsError):
            s3_checkpoint.restore(self.remote, URI, self.destination)
        self.assertEqual(sentinel.read_text(), "in progress")
        self.assertFalse(self.destination.exists())

    def test_manifest_limits_and_unsafe_names(self):
        raw = (self.copy / checkpoint_backup.MANIFEST).read_text()
        for name in ("../escape", "/abs", "parent/child", "folder\\child"):
            with self.subTest(name=name):
                manifest = json.loads(raw)
                manifest["files"][0]["path"] = name
                with self.assertRaisesRegex(ValueError, "inventory entry"):
                    s3_checkpoint.inventory(json.dumps(manifest).encode())
        with patch.object(s3_checkpoint, "MAX_FILES", 1):
            with self.assertRaisesRegex(ValueError, "inventory"):
                s3_checkpoint.inventory(raw.encode())
        with patch.object(s3_checkpoint, "MAX_TOTAL", 2):
            with self.assertRaisesRegex(ValueError, "total byte limit"):
                s3_checkpoint.inventory(raw.encode())

    def test_promotion_refuses_destination_created_after_preflight(self):
        s3_checkpoint.publish(self.remote, self.copy, URI)
        actual = s3_checkpoint.publish_directory
        inode = None

        def create_first(staging, destination):
            nonlocal inode
            destination.mkdir()
            inode = destination.stat().st_ino
            actual(staging, destination)

        with patch.object(s3_checkpoint, "publish_directory", create_first):
            with self.assertRaises(FileExistsError):
                s3_checkpoint.restore(self.remote, URI, self.destination)
        self.assertEqual(self.destination.stat().st_ino, inode)
        self.assertEqual(list(self.destination.iterdir()), [])

    def test_empty_listing_pages_are_bounded(self):
        requested = []

        def empty_pages(*, Bucket, Prefix):
            for page in range(5):
                requested.append(page)
                yield {"Contents": [], "NextContinuationToken": str(page)}

        with patch.object(s3_checkpoint, "MAX_LIST_PAGES", 2), \
                patch.object(self.remote, "paginate", empty_pages):
            with self.assertRaisesRegex(ValueError, "listing exceeds page limit"):
                s3_checkpoint.publish(self.remote, self.copy, URI)
        self.assertEqual(requested, [0, 1])
        self.assertEqual(self.remote.objects, {})

    def test_plaintext_endpoint_is_numeric_loopback_only(self):
        for endpoint in ("http://localhost:9000", "http://example.test", "https://name@host"):
            with self.subTest(endpoint=endpoint), self.assertRaises(ValueError):
                s3_checkpoint.client_options(endpoint, None)
        endpoint = "http://127.0.0.1:9000"
        self.assertEqual(s3_checkpoint.client_options(endpoint, None), {"endpoint_url": endpoint})


if __name__ == "__main__":
    unittest.main()
