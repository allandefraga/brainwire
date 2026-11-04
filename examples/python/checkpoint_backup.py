"""Copy a Brainwire checkpoint to independent storage with a verifiable manifest.

    python -m examples.python.checkpoint_backup copy SOURCE DEST --receipt receipt.json
    python -m examples.python.checkpoint_backup verify DEST

SOURCE is a checkpoint directory created by ``CALL db.checkpoint(name)`` or
``brainwire db checkpoint``; it is immutable once created, so the server may
keep running. The copy is written to ``DEST.partial``, flushed, then renamed;
an interrupted copy never appears complete. ``verify`` rehashes every file.
"""
import argparse
import hashlib
import json
import os
import shutil
import stat
import sys
from datetime import datetime, timezone
from pathlib import Path

FORMAT = "brainwire-checkpoint-copy/1"
MANIFEST = "brainwire-manifest.json"


def _files(root):
    for path in sorted(root.rglob("*")):
        mode = path.lstat().st_mode
        if stat.S_ISLNK(mode):
            raise ValueError(f"checkpoint contains a symlink: {path}")
        if stat.S_ISDIR(mode):
            continue
        if not stat.S_ISREG(mode):
            raise ValueError(f"checkpoint contains a nonregular file: {path}")
        if path != root / MANIFEST:
            yield path


def _safe_inventory_path(name):
    return (isinstance(name, str) and bool(name) and "\\" not in name
            and all(part not in ("", ".", "..") for part in name.split("/"))
            and name != MANIFEST and "\x00" not in name)


def _sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _fsync(path):
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def copy_checkpoint(source, destination, receipt):
    """Copy SOURCE to DESTINATION atomically and return the written manifest."""
    source, destination = Path(source), Path(destination)
    if receipt.get("name") != source.name:
        raise ValueError(f"receipt names {receipt.get('name')!r}, source is {source.name!r}")
    if not (source / "CURRENT").is_file():
        raise ValueError(f"{source} is not a checkpoint directory")
    partial = destination.with_name(destination.name + ".partial")
    source_root = source.resolve()
    for path in (destination, partial):
        resolved = path.resolve()
        if any(ancestor.exists() and ancestor.samefile(source_root)
               for ancestor in (resolved, *resolved.parents)):
            raise ValueError("backup destination must be outside the source checkpoint")
    if (source / MANIFEST).exists() or (source / MANIFEST).is_symlink():
        if verify_copy(source):
            raise ValueError("source backup manifest does not verify")
    files = list(_files(source))
    if any(not _safe_inventory_path(path.relative_to(source).as_posix()) for path in files):
        raise ValueError("checkpoint contains an unsupported inventory path")
    if (destination.exists() or destination.is_symlink()
            or partial.exists() or partial.is_symlink()):
        raise FileExistsError(f"{destination} or {partial} already exists")
    # Only the invocation that created the staging directory may remove it.
    partial.mkdir()
    try:
        # Another invocation may have published after the initial check.
        if destination.exists() or destination.is_symlink():
            raise FileExistsError(f"{destination} already exists")
        entries = []
        for path in files:
            relative = path.relative_to(source)
            target = partial / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(path, target)
            _fsync(target)
            entries.append({"path": relative.as_posix(), "size": target.stat().st_size,
                            "sha256": _sha256(target)})
        manifest = {"format": FORMAT, "receipt": receipt, "files": entries,
                    "copied_at": datetime.now(timezone.utc).isoformat(timespec="seconds")}
        (partial / MANIFEST).write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
        _fsync(partial / MANIFEST)
        for directory in sorted({p.parent for p in partial.rglob("*")}, reverse=True):
            _fsync(directory)
        partial.rename(destination)
    except BaseException:
        shutil.rmtree(partial, ignore_errors=True)
        raise
    # The staging name can be reused by another copy after rename.
    _fsync(destination.parent)
    return manifest


def verify_copy(destination):
    """Return a list of problems; empty means every listed file matches."""
    destination = Path(destination)
    manifest_path = destination / MANIFEST
    if not stat.S_ISREG(manifest_path.lstat().st_mode):
        return ["manifest must be a regular file"]
    manifest = json.loads(manifest_path.read_text())
    if not isinstance(manifest, dict):
        return ["manifest must be an object"]
    if manifest.get("format") != FORMAT:
        return [f"unknown format: {manifest.get('format')!r}"]
    entries = manifest.get("files")
    if not isinstance(entries, list) or not entries:
        return ["manifest files must be a nonempty list"]
    expected = {}
    for index, entry in enumerate(entries):
        if not isinstance(entry, dict):
            return [f"invalid manifest entry: {index}"]
        name, size, digest = entry.get("path"), entry.get("size"), entry.get("sha256")
        if (not _safe_inventory_path(name) or name in expected
                or type(size) is not int or size < 0
                or not isinstance(digest, str) or len(digest) != 64
                or any(char not in "0123456789abcdef" for char in digest)):
            return [f"invalid manifest entry: {index}"]
        expected[name] = entry
    if "CURRENT" not in expected:
        return ["manifest must include CURRENT"]
    actual = {path.relative_to(destination).as_posix(): path for path in _files(destination)}
    problems = [f"missing: {name}" for name in expected if name not in actual]
    problems += [f"extra: {name}" for name in actual if name not in expected]
    for name, path in actual.items():
        entry = expected.get(name)
        if entry and (path.stat().st_size != entry["size"] or _sha256(path) != entry["sha256"]):
            problems.append(f"changed: {name}")
    return problems


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)
    copy = commands.add_parser("copy", help="copy a checkpoint and write its manifest")
    copy.add_argument("source")
    copy.add_argument("destination")
    copy.add_argument("--receipt", required=True, help="JSON receipt from GraphClient.checkpoint")
    verify = commands.add_parser("verify", help="rehash a copy against its manifest")
    verify.add_argument("destination")
    args = parser.parse_args(argv)
    if args.command == "copy":
        receipt = json.loads(Path(args.receipt).read_text())
        manifest = copy_checkpoint(args.source, args.destination, receipt)
        print(json.dumps({"status": "copied", "destination": args.destination,
                          "files": len(manifest["files"])}))
        return 0
    problems = verify_copy(args.destination)
    print(json.dumps({"status": "ok" if not problems else "failed", "problems": problems}))
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
