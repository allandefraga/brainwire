"""Pinned DuckLake snapshot differences, not native CDC; one importer per source.

The graph must exclusively own source-shaped ticket/comment records. Physical
DETACH DELETE removes incident edges. Snapshot retention remains source-owned.
"""
import argparse
from contextlib import closing, suppress
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import tempfile

from examples.python.resumable_customers import fingerprint, ingest, signature
from examples.source_ingestion.export_ducklake_snapshot import export_ducklake_snapshot
from examples.source_ingestion.ingest_snapshot import CONTRACT_ID as SNAPSHOT_CONTRACT, identity, normalize_row, write_batch as write_snapshot_batch

CONTRACT_ID = "external-ducklake-diff-v1"
HEAD_CONTRACT = "external-ducklake-head-v1"
META = {"contract_id", "source_id", "source_identity", "from_version", "to_version", "from_snapshot_sha256", "to_snapshot_sha256", "changes"}
HEAD = """MATCH (s:ImportCheckpoint {entity_id:$id}) RETURN s.contract_id AS contract_id, s.current_version AS current_version, s.source_identity AS source_identity, s.snapshot_sha256 AS snapshot_sha256, s.active_artifact AS active_artifact"""
DELTA = """SELECT 'delete', o.row, CASE WHEN o.kind='comment' THEN 0 ELSE 1 END, o.id
 FROM records o LEFT JOIN records n ON n.version=1 AND n.id=o.id WHERE o.version=0 AND n.id IS NULL
 UNION ALL SELECT 'upsert', n.row, CASE WHEN n.kind='ticket' THEN 2 ELSE 3 END, n.id
 FROM records n LEFT JOIN records o ON o.version=0 AND o.id=n.id WHERE n.version=1 AND (o.id IS NULL OR o.row<>n.row)"""


def encoded(row):
    return (json.dumps(row, ensure_ascii=False, allow_nan=False, sort_keys=True, separators=(",", ":")) + "\n").encode()


def digest_string(value):
    return isinstance(value, str) and len(value) == 64 and all(c in "0123456789abcdef" for c in value)


def initialize_index(connection):
    connection.execute("CREATE TABLE records(version INTEGER,id TEXT,kind TEXT,parent TEXT,row TEXT,PRIMARY KEY(version,id))")


def load_snapshot(connection, path, version, max_rows, max_bytes, line_bytes):
    path = Path(path)
    _, original = fingerprint(path)
    if original[2] > max_bytes:
        raise ValueError("source snapshot exceeds max-input-bytes")
    count = 0
    with path.open("rb") as stream:
        while line := stream.readline(line_bytes + 1):
            if len(line) > line_bytes:
                raise ValueError("source snapshot exceeds line-bytes")
            row = normalize_row(json.loads(line))
            count += 1
            if count > max_rows:
                raise ValueError("source snapshot exceeds max-rows")
            try:
                connection.execute("INSERT INTO records VALUES (?,?,?,?,?)", (version, row["entity_id"], row["record_type"], row.get("ticket_entity_id"), encoded(row).decode()))
            except sqlite3.IntegrityError as error:
                raise ValueError("duplicate source entity ID") from error
    if signature(path) != original:
        raise ValueError("source changed during validation")
    missing = connection.execute("SELECT c.id FROM records c LEFT JOIN records t ON t.version=c.version AND t.id=c.parent AND t.kind='ticket' WHERE c.version=? AND c.kind='comment' AND t.id IS NULL LIMIT 1", (version,)).fetchone()
    if missing:
        raise ValueError("every comment must reference a snapshot ticket")
    digest = hashlib.sha256()
    for (row,) in connection.execute("SELECT row FROM records WHERE version=? ORDER BY id", (version,)):
        digest.update(row.encode())
    return digest.hexdigest(), count


def export_incremental(catalog, data_path, from_version, to_version, query, source_id, destination,
                       max_rows=100000, max_input_bytes=1 << 30, max_output_bytes=1 << 30,
                       line_bytes=65536, fetch_rows=250):
    if any(type(n) is not int or n < 0 or n > (1 << 63) - 1 for n in (from_version, to_version)) or to_version <= from_version:
        raise ValueError("snapshot versions must be ordered nonnegative Cypher integers")
    if not isinstance(source_id, str) or not 1 <= len(source_id.encode()) <= 128:
        raise ValueError("source-id requires 1..128 UTF-8 bytes")
    if any(type(n) is not int or n <= 0 for n in (max_rows, max_input_bytes, max_output_bytes, line_bytes, fetch_rows)):
        raise ValueError("export bounds must be positive integers")
    if max_rows > 100000:
        raise ValueError("max-rows must not exceed 100000")
    destination = Path(destination)
    if destination.exists():
        raise FileExistsError(destination)
    source_identity = hashlib.sha256(encoded(dict(catalog=str(Path(catalog).resolve()), data_path=str(Path(data_path).absolute()), query=query))).hexdigest()
    temporary = None
    try:
        with tempfile.TemporaryDirectory(prefix="brainwire-ducklake-diff-") as folder:
            root = Path(folder)
            receipts = [export_ducklake_snapshot(catalog, data_path, version, query, root / f"{side}.jsonl", fetch_rows, line_bytes, max_input_bytes)
                        for side, version in (("from", from_version), ("to", to_version))]
            with closing(sqlite3.connect(root / "index.sqlite")) as index:
                initialize_index(index)
                old_digest, old_rows = load_snapshot(index, root / "from.jsonl", 0, max_rows, max_input_bytes, line_bytes)
                new_digest, new_rows = load_snapshot(index, root / "to.jsonl", 1, max_rows, max_input_bytes, line_bytes)
                reparent = index.execute("SELECT o.id FROM records o JOIN records n ON n.version=1 AND n.id=o.id WHERE o.version=0 AND o.kind='comment' AND o.parent<>n.parent LIMIT 1").fetchone()
                if reparent:
                    raise ValueError("comment ticket parent is immutable")
                changes = index.execute("SELECT count(*) FROM (" + DELTA + ")").fetchone()[0]
                meta = dict(contract_id=CONTRACT_ID, source_id=source_id, source_identity=source_identity,
                            from_version=from_version, to_version=to_version, from_snapshot_sha256=old_digest,
                            to_snapshot_sha256=new_digest, changes=changes)
                digest = hashlib.sha256()
                total_bytes = 0
                counts = {"upserts": 0, "deletes": 0}
                with tempfile.NamedTemporaryFile(mode="wb", dir=destination.parent, prefix=".incremental-", suffix=".tmp", delete=False) as stream:
                    temporary = Path(stream.name)
                    def emit(row):
                        nonlocal total_bytes
                        data = encoded(row)
                        if len(data) > line_bytes:
                            raise ValueError("change record exceeds line-bytes")
                        if total_bytes + len(data) > max_output_bytes:
                            raise ValueError("changes exceed max-output-bytes")
                        stream.write(data)
                        digest.update(data)
                        total_bytes += len(data)
                    emit(dict(meta, operation="begin", record_type="control"))
                    for operation, payload, _, _ in index.execute(DELTA + " ORDER BY 3,4"):
                        row = json.loads(payload)
                        if operation == "delete":
                            row = {name: row[name] for name in ("record_type", "tenant_id", "connector_id", "external_id", "entity_id")}
                        emit(dict(row, **meta, operation=operation))
                        counts["upserts" if operation == "upsert" else "deletes"] += 1
                    emit(dict(meta, operation="end", record_type="control"))
                    stream.flush()
                    os.fsync(stream.fileno())
                validate_artifact(temporary, line_bytes, max_output_bytes, max_rows * 2)
                os.link(temporary, destination)
        return dict(meta, **counts, bytes=total_bytes, sha256=digest.hexdigest(), from_rows=old_rows, to_rows=new_rows,
                    duckdb_version=receipts[0]["duckdb_version"], ducklake_extension_version=receipts[0]["ducklake_extension_version"])
    finally:
        if temporary is not None:
            with suppress(OSError):
                temporary.unlink(missing_ok=True)


def normalize_change(row):
    if not isinstance(row, dict) or not META.issubset(row) or row["contract_id"] != CONTRACT_ID:
        raise ValueError("invalid incremental mapping contract")
    if not isinstance(row["source_id"], str) or not 1 <= len(row["source_id"].encode()) <= 128:
        raise ValueError("invalid incremental source ID")
    if any(not digest_string(row[key]) for key in ("source_identity", "from_snapshot_sha256", "to_snapshot_sha256")):
        raise ValueError("invalid source/snapshot digest")
    if any(type(row[key]) is not int or not 0 <= row[key] <= (1 << 63) - 1 for key in ("from_version", "to_version", "changes")) or row["to_version"] <= row["from_version"]:
        raise ValueError("invalid ordered versions/change count")
    operation = row.get("operation")
    if operation in ("begin", "end"):
        if row.get("record_type") != "control" or set(row) != META | {"operation", "record_type"}:
            raise ValueError("invalid boundary record")
    elif operation == "upsert":
        normalized = normalize_row(row)
        if set(row) != META | {"operation"} | set(normalized) or any(row[key] != value for key, value in normalized.items()):
            raise ValueError("invalid normalized upsert record")
    elif operation == "delete":
        if set(row) != META | {"operation", "record_type", "tenant_id", "connector_id", "external_id", "entity_id"} or row.get("record_type") not in ("ticket", "comment"):
            raise ValueError("invalid delete record")
        if any(not isinstance(row[key], str) or not row[key] for key in ("tenant_id", "connector_id", "external_id")) or row["entity_id"] != identity(row["tenant_id"], row["connector_id"], row["record_type"], row["external_id"]):
            raise ValueError("invalid delete identity")
    else:
        raise ValueError("invalid incremental operation")
    return row


def validate_artifact(file, line_bytes=65536, max_bytes=1 << 30, max_changes=200000):
    if any(type(n) is not int or n <= 0 for n in (line_bytes, max_bytes, max_changes)):
        raise ValueError("artifact bounds must be positive integers")
    path = Path(file)
    digest, original = fingerprint(path)
    if original[2] > max_bytes:
        raise ValueError("artifact exceeds max-bytes")
    meta = None
    phase = "begin"
    count = 0
    previous = None
    with tempfile.TemporaryDirectory(prefix="brainwire-diff-validation-") as directory, closing(sqlite3.connect(Path(directory) / "seen.sqlite")) as seen:
        seen.execute("CREATE TABLE seen(id TEXT PRIMARY KEY)")
        with path.open("rb") as stream:
            while line := stream.readline(line_bytes + 1):
                if len(line) > line_bytes or not line.endswith(b"\n"):
                    raise ValueError("invalid artifact line length/terminator")
                row = normalize_change(json.loads(line))
                metadata = {key: row[key] for key in META}
                if meta is None:
                    meta = metadata
                elif meta != metadata:
                    raise ValueError("artifact metadata changed")
                operation = row["operation"]
                if phase == "begin":
                    if operation != "begin":
                        raise ValueError("artifact must start with begin")
                    phase = "changes"
                elif phase == "end":
                    raise ValueError("records follow end")
                elif operation == "end":
                    phase = "end"
                elif operation in ("upsert", "delete"):
                    rank = (0 if row["record_type"] == "comment" else 1) if operation == "delete" else (2 if row["record_type"] == "ticket" else 3)
                    order = (rank, row["entity_id"])
                    if previous is not None and order <= previous:
                        raise ValueError("changes are not deterministically ordered")
                    previous = order
                    try:
                        seen.execute("INSERT INTO seen VALUES (?)", (row["entity_id"],))
                    except sqlite3.IntegrityError as error:
                        raise ValueError("duplicate change entity ID") from error
                    count += 1
                    if count > max_changes:
                        raise ValueError("artifact exceeds max-changes")
                else:
                    raise ValueError("duplicate begin")
    if phase != "end" or count != meta["changes"]:
        raise ValueError("artifact incomplete or change count mismatched")
    if signature(path) != original:
        raise ValueError("artifact changed during validation")
    return meta, digest, original


def head_id(source_id):
    return json.dumps(["ducklake-incremental-head", source_id], separators=(",", ":"))


def initialize_source(driver, database, changes_file, baseline_file, baseline_source_id):
    """Register the verified baseline once; reconcile an unknown commit before retrying."""
    meta, _, change_signature = validate_artifact(changes_file)
    baseline_digest, baseline_signature = fingerprint(Path(baseline_file))
    with tempfile.TemporaryDirectory(prefix="brainwire-diff-baseline-") as directory, closing(sqlite3.connect(Path(directory) / "baseline.sqlite")) as index:
        initialize_index(index)
        logical_digest, _ = load_snapshot(index, baseline_file, 0, 100000, 1 << 30, 65536)
    if logical_digest != meta["from_snapshot_sha256"]:
        raise ValueError("baseline artifact does not match the from snapshot")
    def register(tx):
        if signature(Path(baseline_file)) != baseline_signature or signature(Path(changes_file)) != change_signature:
            raise ValueError("registration source changed")
        completed = tx.run("MATCH (s:ImportCheckpoint {entity_id:$id}) RETURN s.sha256 AS sha256,s.next_offset AS next_offset,s.contract_id AS contract_id", id=baseline_source_id).single()
        if completed is None or completed["sha256"] != baseline_digest or completed["next_offset"] != baseline_signature[2] or completed["contract_id"] != SNAPSHOT_CONTRACT:
            raise ValueError("baseline graph checkpoint is not a completed matching snapshot")
        current = tx.run(HEAD, id=head_id(meta["source_id"])).single()
        if current is not None:
            if current["contract_id"] != HEAD_CONTRACT or type(current["current_version"]) is not int or current["current_version"] != meta["from_version"] or current["source_identity"] != meta["source_identity"] or current["snapshot_sha256"] != logical_digest or current["active_artifact"] is not None:
                raise ValueError("source head already differs from baseline")
            return
        tx.run("MERGE (s:ImportCheckpoint {entity_id:$id}) SET s.contract_id=$contract,s.current_version=$version,s.source_identity=$identity,s.snapshot_sha256=$digest", id=head_id(meta["source_id"]), contract=HEAD_CONTRACT, version=meta["from_version"], identity=meta["source_identity"], digest=logical_digest).consume()
    with driver.session(database=database) as session:
        # A lost COMMIT reply must not silently replay registration.
        with session.begin_transaction() as tx:
            register(tx)
            tx.commit()


def ingest_incremental(driver, database, file, batch_size=250, batch_bytes=1 << 20,
                       line_bytes=65536, *, max_batches=None):
    if any(type(n) is not int or n <= 0 for n in (batch_size, batch_bytes, line_bytes)) or line_bytes > batch_bytes:
        raise ValueError("batch bounds must be positive and line-bytes <= batch-bytes")
    meta, digest, original = validate_artifact(file, line_bytes)
    checkpoint_id = json.dumps([CONTRACT_ID, meta["source_id"], meta["from_version"], meta["to_version"], digest], separators=(",", ":"))
    def apply(tx, rows):
        if signature(Path(file)) != original:
            raise ValueError("artifact changed since validation")
        current = tx.run(HEAD, id=head_id(meta["source_id"])).single()
        if current is None or current["contract_id"] != HEAD_CONTRACT or current["source_identity"] != meta["source_identity"] or type(current["current_version"]) is not int or current["current_version"] != meta["from_version"] or current["snapshot_sha256"] != meta["from_snapshot_sha256"]:
            raise ValueError("source head has wrong baseline/version/identity")
        if current["active_artifact"] not in (None, digest) or (current["active_artifact"] is None and rows[0]["operation"] != "begin"):
            raise ValueError("competing or unclaimed incremental artifact")
        tx.run("MATCH (s:ImportCheckpoint {entity_id:$id}) SET s.active_artifact=$digest", id=head_id(meta["source_id"]), digest=digest).consume()
        for row in rows:
            if {key: row[key] for key in META} != meta:
                raise ValueError("incremental metadata changed")
        for kind, label in (("comment", "TicketComment"), ("ticket", "Ticket")):
            deletions = [row for row in rows if row["operation"] == "delete" and row["record_type"] == kind]
            if deletions:
                tx.run(f"UNWIND $rows AS row MATCH (n:{label} {{entity_id:row.entity_id}}) DETACH DELETE n", rows=deletions).consume()
        upserts = [row for row in rows if row["operation"] == "upsert"]
        if upserts:
            write_snapshot_batch(tx, upserts)
        if any(row["operation"] == "end" for row in rows):
            tx.run("MATCH (s:ImportCheckpoint {entity_id:$id}) SET s.current_version=$version,s.snapshot_sha256=$digest,s.active_artifact=null", id=head_id(meta["source_id"]), version=meta["to_version"], digest=meta["to_snapshot_sha256"]).consume()
    if signature(Path(file)) != original:
        raise ValueError("artifact changed before import")
    offset = ingest(driver, database, file, checkpoint_id, batch_size, batch_bytes, line_bytes,
                    normalize_row=normalize_change, write_batch=apply,
                    contract_id=CONTRACT_ID, max_batches=max_batches)
    if signature(Path(file)) != original:
        raise ValueError("artifact changed since validation")
    return offset


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    export = sub.add_parser("export")
    for name in ("catalog", "data-path", "query-file", "source-id", "output"):
        export.add_argument("--" + name, required=True)
    for name in ("from-version", "to-version"):
        export.add_argument("--" + name, type=int, required=True)
    export.add_argument("--max-rows", type=int, default=100000)
    export.add_argument("--max-input-bytes", type=int, default=1 << 30)
    export.add_argument("--max-output-bytes", type=int, default=1 << 30)
    for command in ("register", "ingest"):
        child = sub.add_parser(command)
        child.add_argument("file")
        child.add_argument("--database", default="support-demo")
        child.add_argument("--uri", default="bolt://127.0.0.1:7687")
        if command == "register":
            child.add_argument("--baseline-file", required=True)
            child.add_argument("--baseline-source-id", required=True)
        else:
            child.add_argument("--batch-size", type=int, default=250)
            child.add_argument("--max-batches", type=int,
                               help="pause after this many committed source batches; rerun to resume")
    args = parser.parse_args()
    if args.command == "export":
        print(json.dumps(export_incremental(args.catalog, args.data_path, args.from_version, args.to_version,
              Path(args.query_file).read_text(), args.source_id, args.output, args.max_rows, args.max_input_bytes, args.max_output_bytes), sort_keys=True))
    else:
        from neo4j import GraphDatabase
        from examples.python.connection import auth_from_env
        with GraphDatabase.driver(args.uri, auth=auth_from_env(), liveness_check_timeout=30) as driver:
            if args.command == "register":
                initialize_source(driver, args.database, args.file, args.baseline_file, args.baseline_source_id)
                print("Registered completed baseline checkpoint")
            else:
                offset = ingest_incremental(driver, args.database, args.file,
                                            args.batch_size,
                                            max_batches=args.max_batches)
                size = Path(args.file).stat().st_size
                state = "complete" if offset == size else "paused"
                print(f"{state}: durable next_offset={offset}/{size} bytes")


if __name__ == "__main__":
    main()
