"""Preview or import a small Markdown vault into a new Brainwire graph."""

import argparse
from collections import defaultdict
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat

from brainwire import connect
from examples.python.connection import auth_from_env, bolt_connection_options


MAX_ENTRIES = 2048
MAX_DEPTH = 16
MAX_NOTES = 128
MAX_NOTE_BYTES = 32 * 1024
MAX_TOTAL_BYTES = 256 * 1024
MAX_REFERENCES = 512
MAX_PATH_BYTES = 512
BATCH_SIZE = 8

FENCE = re.compile(r" {0,3}(`{3,}|~{3,})(.*)")
INLINE_CODE_OR_LINK = re.compile(r"(?<!`) (`+) (?!`) .*? (?<!`) \1 (?!`)"
                                 r"| (?<![!\\]) \[\[ ([^\[\]\r\n]+) \]\]", re.VERBOSE)
CREATE_NOTES = (
    "UNWIND $rows AS row CREATE (n:Note {entity_id: row.path, title: row.title, "
    "body: row.body}) RETURN count(n) AS created"
)
CREATE_LINKS = (
    "UNWIND $rows AS row MATCH (a:Note {entity_id: row.source}) "
    "MATCH (b:Note {entity_id: row.target}) "
    "CREATE (a)-[r:LINKS_TO]->(b) RETURN count(r) AS created"
)


def read_notes(vault):
    if os.name != "posix" or not hasattr(os, "O_NOFOLLOW"):
        raise ValueError("vault import requires POSIX no-follow file access")
    directory_flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
    file_flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK
    root = Path(vault).absolute()
    if ".." in root.parts:
        raise ValueError("vault root cannot contain '..'")
    notes = []
    entries = total = 0

    def walk(parent, parts):
        nonlocal entries, total
        with os.scandir(parent) as directory:
            for entry in directory:
                entries += 1
                if entries > MAX_ENTRIES:
                    raise ValueError(f"vault exceeds {MAX_ENTRIES} directory entries")
                name = entry.name
                if name.startswith("."):
                    continue
                if entry.is_symlink():
                    raise ValueError("vault contains a visible symlink")
                child_parts = (*parts, name)
                if entry.is_dir(follow_symlinks=False):
                    if len(child_parts) >= MAX_DEPTH:
                        raise ValueError(f"vault exceeds {MAX_DEPTH} path components")
                    child = os.open(name, directory_flags, dir_fd=parent)
                    try:
                        walk(child, child_parts)
                    finally:
                        os.close(child)
                    continue
                path = "/".join(child_parts)
                if not name.endswith(".md"):
                    continue
                if len(path.encode("utf-8")) > MAX_PATH_BYTES or "\\" in path:
                    raise ValueError("Markdown path is too long or contains a backslash")
                if len(notes) >= MAX_NOTES:
                    raise ValueError(f"vault exceeds {MAX_NOTES} Markdown notes")
                with os.fdopen(os.open(name, file_flags, dir_fd=parent), "rb") as stream:
                    before = os.fstat(stream.fileno())
                    if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1:
                        raise ValueError("Markdown notes must be regular files with one hard link")
                    allowance = min(MAX_NOTE_BYTES, MAX_TOTAL_BYTES - total)
                    body = stream.read(allowance + 1)
                    after = os.fstat(stream.fileno())
                if (before.st_size, before.st_mtime_ns, before.st_ctime_ns) != (
                    after.st_size, after.st_mtime_ns, after.st_ctime_ns
                ):
                    raise ValueError("vault changed during import")
                if len(body) > allowance:
                    raise ValueError("vault exceeds the per-note or total Markdown byte limit")
                total += len(body)
                notes.append({"path": path, "title": name[:-3], "body": body.decode("utf-8")})

    parent = os.open(root.anchor, directory_flags)
    try:
        for component in root.parts[1:]:
            child = os.open(component, directory_flags, dir_fd=parent)
            os.close(parent)
            parent = child
        walk(parent, ())
    finally:
        os.close(parent)
    if not notes:
        raise ValueError("vault has no visible Markdown notes")
    return sorted(notes, key=lambda note: note["path"])


def wikilinks(body):
    fence = None
    for line in body.splitlines():
        delimiter = FENCE.fullmatch(line)
        if fence:
            if (delimiter and delimiter[1][0] == fence[0]
                    and len(delimiter[1]) >= len(fence) and not delimiter[2].strip()):
                fence = None
        elif delimiter:
            fence = delimiter[1]
        else:
            for match in INLINE_CODE_OR_LINK.finditer(line):
                if match[2] is not None:
                    yield match[2].split("|", 1)[0].split("#", 1)[0].strip()


def prepare(vault):
    notes = read_notes(vault)
    paths = {note["path"] for note in notes}
    stems = defaultdict(list)
    for note in notes:
        stems[note["title"]].append(note["path"])
    edges = set()
    references = 0
    for note in notes:
        source = note["path"]
        for target in wikilinks(note["body"]):
            references += 1
            if references > MAX_REFERENCES:
                raise ValueError(f"vault exceeds {MAX_REFERENCES} wikilink references")
            if not target:  # Headings within the current note need no graph edge.
                continue
            components = target.split("/")
            if "\\" in target or any(part in {"", ".", ".."} for part in components):
                raise ValueError(f"unsupported wikilink path in {ascii(source)}")
            if len(components) == 1:
                stem = target[:-3] if target.endswith(".md") else target
                matches = stems[stem]
            else:
                path = target if target.endswith(".md") else target + ".md"
                matches = [path] if path in paths else []
            if len(matches) != 1:
                reason = "ambiguous" if matches else "unresolved"
                raise ValueError(f"{reason} wikilink in {ascii(source)}: {ascii(target)}")
            edges.add((source, matches[0]))
    return notes, [{"source": source, "target": target} for source, target in sorted(edges)]


def import_notes(graph, notes, links):
    with graph.driver.session(database=graph.database) as session:
        with session.begin_transaction() as tx:
            occupied = tx.run("MATCH (n) RETURN 1 AS found LIMIT 1").single()
            if occupied is not None:
                raise ValueError("destination graph must be empty")
            for statement, records in ((CREATE_NOTES, notes), (CREATE_LINKS, links)):
                for start in range(0, len(records), BATCH_SIZE):
                    rows = records[start:start + BATCH_SIZE]
                    if tx.run(statement, rows=rows).single(strict=True)["created"] != len(rows):
                        raise ValueError("creation count did not match the prepared vault")
            tx.commit()


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("vault", type=Path)
    parser.add_argument("--database", help="write once to the named, dedicated empty graph")
    parser.add_argument("--uri", default="bolt://127.0.0.1:7687")
    parser.add_argument("--tls-ca", help="verify Bolt TLS using this PEM CA file")
    args = parser.parse_args(argv)
    if args.database is not None and not re.fullmatch(r"[A-Za-z0-9_-]{1,128}", args.database):
        parser.error("--database must be an explicit Brainwire graph name")
    try:
        notes, links = prepare(args.vault)
    except OSError:
        parser.exit(2, "Cannot read the vault safely. Use its physical path and check file access.\n")
    except UnicodeError:
        parser.exit(2, "Markdown paths and bodies must be UTF-8.\n")
    except ValueError as error:
        parser.exit(2, f"{error}\n")
    if args.database is not None:
        from neo4j.exceptions import DriverError, Neo4jError

        try:
            uri, config = bolt_connection_options(args.uri, args.tls_ca)
            with connect(uri, args.database, auth=auth_from_env(), connection_timeout=5, **config) as graph:
                import_notes(graph, notes, links)
        except (DriverError, Neo4jError):
            parser.exit(2, "Bolt import failed. Inspect the destination graph before retrying.\n")
        except ValueError as error:
            parser.exit(2, f"{error}\n")
    print(json.dumps({"notes": len(notes), "links": len(links), "written": args.database is not None}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
