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
import sys
from datetime import datetime, timezone
from pathlib import Path

FORMAT = "brainwire-checkpoint-copy/1"
MANIFEST = "brainwire-manifest.json"


def _files(root):
    for path in sorted(root.rglob("*")):
        if path.is_symlink():
            raise ValueError(f"checkpoint contains a symlink: {path}")
        if path.is_file() and path.name != MANIFEST:
            yield path


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
    files = list(_files(source))
    partial = destination.with_name(destination.name + ".partial")
    if destination.exists() or partial.exists():
        raise FileExistsError(f"{destination} or {partial} already exists")
    try:
        partial.mkdir()
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
        _fsync(destination.parent)
    except BaseException:
        shutil.rmtree(partial, ignore_errors=True)
        raise
    return manifest


def verify_copy(destination):
    """Return a list of problems; empty means every listed file matches."""
    destination = Path(destination)
    manifest = json.loads((destination / MANIFEST).read_text())
    if manifest.get("format") != FORMAT:
        return [f"unknown format: {manifest.get('format')!r}"]
    expected = {entry["path"]: entry for entry in manifest["files"]}
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
