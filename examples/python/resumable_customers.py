"""Resume an immutable JSONL source using a graph cursor committed with its data.

Apply resumable_customers_ontology.json before importing. Run only one importer
per source ID: this example has no source lease. All customer fields are
authoritative; null removes a property. SHA256 binds the source ID to its file.
The file must stay immutable throughout hashing and ingestion; stat checks catch
ordinary modifications, not adversarial replacement. The graph byte cursor is
the resume authority, independent of local receipts or db.commitState().
"""

import argparse
import hashlib
import json
from pathlib import Path

CUSTOMERS = """
UNWIND $rows AS row
MERGE (c:Customer {entity_id: row.entity_id})
SET c.email = row.email, c.active = row.active,
    c.tags = row.tags, c.embedding = row.embedding
"""
CHECKPOINT = """
MATCH (s:ImportCheckpoint {entity_id: $source_id})
RETURN s.sha256 AS sha256, s.next_offset AS next_offset
"""
INITIALIZE = """
MERGE (s:ImportCheckpoint {entity_id: $source_id})
SET s.sha256 = $sha256, s.next_offset = 0
"""
ADVANCE = """
MATCH (s:ImportCheckpoint {entity_id: $source_id})
SET s.next_offset = $next_offset
"""
REQUIRED = {"entity_id", "email", "active", "tags", "embedding"}
MAX_INTEGER = (1 << 63) - 1


def signature(path):
    stat = path.stat()
    return (stat.st_dev, stat.st_ino, stat.st_size, stat.st_mtime_ns, stat.st_ctime_ns)


def fingerprint(path):
    before = signature(path)
    if before[2] > MAX_INTEGER:
        raise ValueError("source size exceeds the Cypher integer range")
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        while block := stream.read(1 << 20):
            digest.update(block)
    if signature(path) != before:
        raise ValueError("source changed while hashing")
    return digest.hexdigest(), before


def read_batch(path, offset, size, batch_size, batch_bytes, line_bytes):
    if type(offset) is not int or not 0 <= offset <= min(size, MAX_INTEGER):
        raise ValueError("invalid checkpoint byte offset")
    rows = []
    consumed = 0
    with path.open("rb") as stream:
        if offset and offset < size:
            stream.seek(offset - 1)
            if stream.read(1) != b"\n":
                raise ValueError("checkpoint offset is not a line boundary")
        stream.seek(offset)
        while len(rows) < batch_size and consumed < batch_bytes:
            line = stream.readline(line_bytes + 1)
            if not line:
                break
            if len(line) > line_bytes:
                raise ValueError(f"input line at byte {offset + consumed} exceeds line-bytes")
            if consumed + len(line) > batch_bytes:
                break
            if line.strip():
                row = json.loads(line)
                if not isinstance(row, dict) or not REQUIRED.issubset(row):
                    raise ValueError(f"row at byte {offset + consumed} requires {sorted(REQUIRED)}")
                if not isinstance(row["entity_id"], str) or not row["entity_id"]:
                    raise ValueError("entity_id must be a nonempty string")
                rows.append({key: row[key] for key in REQUIRED})
            consumed += len(line)
    return rows, offset + consumed


def ingest(driver, database, file, source_id, batch_size=250,
           batch_bytes=1 << 20, line_bytes=64 << 10):
    """Return the durable byte cursor; every retry rereads its graph checkpoint."""
    if not isinstance(source_id, str) or not source_id:
        raise ValueError("source_id must be a nonempty string")
    if any(type(n) is not int or n <= 0 for n in (batch_size, batch_bytes, line_bytes)):
        raise ValueError("batch-size, batch-bytes and line-bytes must be positive integers")
    if line_bytes > batch_bytes:
        raise ValueError("line-bytes must not exceed batch-bytes")
    path = Path(file)
    digest, original = fingerprint(path)

    def check_source():
        if signature(path) != original:
            raise ValueError("source changed during import")

    def commit_batch(tx):
        check_source()
        checkpoint = tx.run(CHECKPOINT, source_id=source_id).single()
        if checkpoint is None:
            tx.run(INITIALIZE, source_id=source_id, sha256=digest).consume()
            checkpoint = tx.run(CHECKPOINT, source_id=source_id).single()
        if checkpoint is None or checkpoint["sha256"] != digest:
            raise ValueError("source ID already refers to a different SHA256")
        rows, next_offset = read_batch(
            path, checkpoint["next_offset"], original[2],
            batch_size, batch_bytes, line_bytes,
        )
        check_source()
        if rows:
            tx.run(CUSTOMERS, rows=rows).consume()
        if next_offset != checkpoint["next_offset"]:
            tx.run(ADVANCE, source_id=source_id, next_offset=next_offset).consume()
        check_source()
        return next_offset

    with driver.session(database=database) as session:
        while True:
            offset = session.execute_write(commit_batch)
            if offset == original[2]:
                check_source()
                return offset


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("file")
    parser.add_argument("--source-id", required=True)
    parser.add_argument("--database", default="customers")
    parser.add_argument("--uri", default="bolt://127.0.0.1:7687")
    parser.add_argument("--batch-size", type=int, default=250)
    parser.add_argument("--batch-bytes", type=int, default=1 << 20)
    parser.add_argument("--line-bytes", type=int, default=64 << 10)
    args = parser.parse_args()
    from neo4j import GraphDatabase

    with GraphDatabase.driver(args.uri, auth=None) as driver:
        offset = ingest(driver, args.database, args.file, args.source_id,
                        args.batch_size, args.batch_bytes, args.line_bytes)
    print(f"Source {args.source_id}: durable next_offset={offset} bytes")


if __name__ == "__main__":
    main()
