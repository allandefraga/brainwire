"""Publish and download verified Brainwire checkpoint copies in S3-compatible storage."""
import argparse
import base64
from contextlib import closing
import ctypes
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import shutil
import stat
import sys
from urllib.parse import urlsplit

from examples.python import checkpoint_backup as local


CLAIM = "_brainwire-upload"
MAX_MANIFEST = 4 << 20
MAX_FILES = 20_000
MAX_LIST_PAGES = 100
MAX_FILE = 5_000_000_000
MAX_TOTAL = 64 << 30
CHUNK = 1 << 20
SAFE_NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,254}")
SHA256 = re.compile(r"[0-9a-f]{64}")


def inventory(raw):
    if len(raw) > MAX_MANIFEST:
        raise ValueError("checkpoint manifest is too large")
    manifest = json.loads(raw)
    if not isinstance(manifest, dict) or manifest.get("format") != local.FORMAT:
        raise ValueError("invalid checkpoint manifest format")
    entries = manifest.get("files")
    if not isinstance(entries, list) or not 1 <= len(entries) <= MAX_FILES:
        raise ValueError("invalid checkpoint inventory")
    files = {}
    total = 0
    for entry in entries:
        if not isinstance(entry, dict):
            raise ValueError("invalid checkpoint inventory entry")
        name, size, digest = entry.get("path"), entry.get("size"), entry.get("sha256")
        if (not isinstance(name, str) or not SAFE_NAME.fullmatch(name)
                or name == local.MANIFEST or name in files
                or type(size) is not int or not 0 <= size <= MAX_FILE
                or not isinstance(digest, str) or not SHA256.fullmatch(digest)):
            raise ValueError("invalid checkpoint inventory entry")
        total += size
        if total > MAX_TOTAL:
            raise ValueError("checkpoint exceeds the total byte limit")
        files[name] = (size, digest)
    if "CURRENT" not in files:
        raise ValueError("checkpoint inventory is missing CURRENT")
    return files, total


def s3_location(uri):
    parsed = urlsplit(uri)
    parts = parsed.path.removeprefix("/").split("/")
    if (parsed.scheme != "s3" or not re.fullmatch(r"[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]", parsed.netloc)
            or parsed.query or parsed.fragment or not all(SAFE_NAME.fullmatch(part) for part in parts)
            or len(parsed.path.encode()) > 700):
        raise ValueError("expected s3://bucket/new-prefix with ASCII path components")
    # User prefixes cannot contain this component, so nested backup URIs stay isolated.
    return parsed.netloc, "/".join(parts) + "/_brainwire/"


def object_keys(client, bucket, prefix):
    pages = client.get_paginator("list_objects_v2").paginate(Bucket=bucket, Prefix=prefix)
    for index, page in enumerate(pages):
        if index >= MAX_LIST_PAGES or (
            index == MAX_LIST_PAGES - 1 and page.get("NextContinuationToken")
        ):
            raise ValueError("backup object listing exceeds page limit")
        for item in page.get("Contents", []):
            yield item["Key"]


def put_new(client, bucket, key, body, size, digest):
    client.put_object(
        Bucket=bucket, Key=key, Body=body, ContentLength=size, IfNoneMatch="*",
        ChecksumSHA256=base64.b64encode(bytes.fromhex(digest)).decode("ascii"),
    )


def publish(client, source, uri):
    source = Path(source)
    if source.is_symlink():
        raise ValueError("checkpoint copy must be a real directory")
    manifest_path = source / local.MANIFEST
    if not stat.S_ISREG(manifest_path.lstat().st_mode):
        raise ValueError("checkpoint manifest must be a regular file")
    with open(manifest_path, "rb", opener=local._open_source) as manifest:
        raw = manifest.read(MAX_MANIFEST + 1)
    files, _ = inventory(raw)
    problems = local.verify_copy(source)
    if problems:
        raise ValueError(f"checkpoint verification failed: {problems}")

    bucket, prefix = s3_location(uri)
    if next(object_keys(client, bucket, prefix), None) is not None:
        raise FileExistsError("backup prefix already contains objects")
    claim = b"brainwire-checkpoint-upload/1\n"
    put_new(client, bucket, prefix + CLAIM, claim, len(claim), hashlib.sha256(claim).hexdigest())
    for name, (size, digest) in files.items():
        with open(source / name, "rb", opener=local._open_source) as handle:
            put_new(client, bucket, prefix + name, handle, size, digest)
    put_new(client, bucket, prefix + local.MANIFEST, raw, len(raw), hashlib.sha256(raw).hexdigest())
    return len(files)


def verify_keys(client, bucket, prefix, files):
    expected = {prefix + name for name in (*files, CLAIM, local.MANIFEST)}
    for key in object_keys(client, bucket, prefix):
        if key not in expected:
            raise ValueError(f"unexpected object in backup prefix: {key!r}")
        expected.remove(key)
    if expected:
        raise ValueError("backup prefix is incomplete")


def download_file(client, bucket, key, path, size, digest):
    response = client.get_object(Bucket=bucket, Key=key)
    with closing(response["Body"]) as body:
        if response["ContentLength"] != size:
            raise ValueError(f"checkpoint file size mismatch: {path.name}")
        sha = hashlib.sha256()
        remaining = size
        with open(path, "xb", opener=local._open_private) as output:
            while remaining:
                data = body.read(min(CHUNK, remaining))
                if not data or len(data) > remaining:
                    break
                sha.update(data)
                output.write(data)
                remaining -= len(data)
            if remaining or body.read(1) or sha.hexdigest() != digest:
                raise ValueError(f"checkpoint file checksum mismatch: {path.name}")
    local._fsync(path)


def publish_directory(staging, destination):
    # Both calls refuse an existing destination, including an empty directory.
    libc = ctypes.CDLL(None, use_errno=True)
    source_bytes = os.fsencode(staging)
    dest_bytes = os.fsencode(destination)
    if sys.platform == "darwin":
        rename = libc.renamex_np
        rename.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint]
        result = rename(source_bytes, dest_bytes, 0x00000004)  # RENAME_EXCL
    elif sys.platform.startswith("linux") and hasattr(libc, "renameat2"):
        rename = libc.renameat2
        rename.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
        result = rename(-100, source_bytes, -100, dest_bytes, 1)  # AT_FDCWD, RENAME_NOREPLACE
    else:
        raise OSError("atomic no-replace directory promotion requires macOS or Linux")
    if result != 0:
        error = ctypes.get_errno()
        raise OSError(error, os.strerror(error), destination)


def restore(client, uri, destination):
    """Download a verified copy to a new local directory."""
    bucket, prefix = s3_location(uri)
    destination = Path(destination)
    partial = destination.with_name(destination.name + ".partial")
    if destination.exists() or destination.is_symlink():
        raise FileExistsError(f"{destination} already exists")
    response = client.get_object(Bucket=bucket, Key=prefix + local.MANIFEST)
    with closing(response["Body"]) as body:
        if response["ContentLength"] > MAX_MANIFEST:
            raise ValueError("checkpoint manifest is too large")
        raw = body.read(MAX_MANIFEST + 1)
        if len(raw) != response["ContentLength"]:
            raise ValueError("checkpoint manifest is incomplete")
    files, total = inventory(raw)
    verify_keys(client, bucket, prefix, files)
    if shutil.disk_usage(destination.parent).free < total + len(raw):
        raise ValueError("insufficient free space to restore checkpoint")
    partial.mkdir(mode=0o700)
    try:
        for name, (size, digest) in files.items():
            download_file(client, bucket, prefix + name, partial / name, size, digest)
        manifest = partial / local.MANIFEST
        with open(manifest, "xb", opener=local._open_private) as handle:
            handle.write(raw)
        local._fsync(manifest)
        local._fsync(partial)
        if destination.exists() or destination.is_symlink():
            raise FileExistsError(f"{destination} already exists")
        publish_directory(partial, destination)
    except BaseException:
        shutil.rmtree(partial)
        raise
    local._fsync(destination.parent)
    return len(files)


def client_options(endpoint, ca_bundle):
    if endpoint:
        parsed = urlsplit(endpoint)
        if (parsed.scheme not in ("http", "https") or not parsed.hostname
                or parsed.username is not None or parsed.password is not None
                or parsed.path not in ("", "/") or parsed.query or parsed.fragment):
            raise ValueError("expected an HTTP(S) endpoint with no user info, path, query or fragment")
        if parsed.scheme == "http":
            try:
                loopback = ipaddress.ip_address(parsed.hostname).is_loopback
            except ValueError:
                loopback = False
            if not loopback:
                raise ValueError("HTTP endpoints must use a numeric loopback IP")
    options = {}
    if endpoint:
        options["endpoint_url"] = endpoint
    if ca_bundle:
        options["verify"] = ca_bundle
    return options


def client_config(endpoint):
    from botocore.config import Config

    return Config(
        signature_version="s3v4",
        ignore_configured_endpoint_urls=True,
        s3={"addressing_style": "path"} if endpoint else None,
        proxies={} if endpoint and urlsplit(endpoint).scheme == "http" else None,
        connect_timeout=5,
        read_timeout=60,
        retries={"mode": "standard", "total_max_attempts": 3},
    )


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--endpoint", help="custom S3 endpoint, such as an HTTPS MinIO address")
    parser.add_argument("--ca-bundle", help="PEM CA bundle for verification")
    commands = parser.add_subparsers(dest="command", required=True)
    upload = commands.add_parser("upload", help="publish a verified local backup in a fresh prefix")
    upload.add_argument("source")
    upload.add_argument("uri")
    download = commands.add_parser("download", help="verify and download into a new local directory")
    download.add_argument("uri")
    download.add_argument("destination")
    args = parser.parse_args(argv)
    try:
        import boto3
        from botocore.exceptions import BotoCoreError, ClientError
    except ImportError:
        parser.exit(1, "S3 checkpoint commands require boto3 (python -m pip install 'boto3>=1.36,<2')\n")
    try:
        options = client_options(args.endpoint, args.ca_bundle)
        with closing(boto3.client("s3", config=client_config(args.endpoint), **options)) as client:
            if args.command == "upload":
                files = publish(client, args.source, args.uri)
            else:
                files = restore(client, args.uri, args.destination)
    except (ValueError, OSError, BotoCoreError, ClientError) as exc:
        parser.exit(1, f"{exc}\n")
    print(json.dumps({"status": "ok", "files": files}))


if __name__ == "__main__":
    sys.exit(main())
