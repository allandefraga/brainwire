"""Export trusted DuckDB SELECT results to an immutable client snapshot.

Use an immutable/versioned source and a caller-prepared connection with no active
transaction and exclusive use during export. SQL is trusted: SELECT validation
is not a sandbox. A transaction pins DuckDB table reads, but does not pin mutable external Parquet files.
fetch_rows bounds fetched Python rows, not SQL engine memory or individual values.
Tested with DuckDB 1.5.5. DuckLake attachment, CDC and deletion synchronization
are outside this example.
DuckDB is optional: uv run --no-project --with duckdb==1.5.5 python -m
examples.client.export_snapshot --database source.duckdb --query-file query.sql
--output snapshot.jsonl
"""

import argparse
from contextlib import suppress
import hashlib
import json
import os
from pathlib import Path
import tempfile
import sys

from examples.client.ingest_snapshot import CONTRACT_ID, normalize_row


def export_snapshot(connection, query, destination, fetch_rows=250,
                    line_bytes=65536, max_output_bytes=1 << 30):
    """Return counts, byte length, SHA256 and mapping contract after publication.

Own BEGIN/COMMIT; preflight refuses caller-active transactions before BEGIN.
The destination must not exist. Only a fully read, validated and committed
snapshot is linked into place. Interrupted exports can leave a private temp file,
never a partially written final input. The caller owns the connection lifecycle.
The directory is not fsynced: the receipt is not a power-loss durability promise.
"""
    if any(type(n) is not int or n <= 0 for n in (fetch_rows, line_bytes, max_output_bytes)):
        raise ValueError("fetch-rows, line-bytes and max-output-bytes must be positive integers")
    import duckdb

    statements = connection.extract_statements(query)
    if len(statements) != 1 or statements[0].type != duckdb.StatementType.SELECT:
        raise ValueError("query must contain exactly one SELECT statement")
    # Nested BEGIN aborts an existing DuckDB transaction; probe without mutating it.
    first = connection.execute("SELECT current_transaction_id()").fetchall()[0][0]
    second = connection.execute("SELECT current_transaction_id()").fetchall()[0][0]
    if type(first) is not int or type(second) is not int:
        raise ValueError("cannot establish connection transaction state")
    if first == second:
        raise ValueError("connection already has an active transaction")
    destination = Path(destination)
    receipt = dict(rows=0, tickets=0, comments=0, bytes=0, mapping_contract_id=CONTRACT_ID)
    digest = hashlib.sha256()
    temporary = None
    active = False
    try:
        connection.execute("BEGIN TRANSACTION")
        active = True
        cursor = connection.execute(query)
        columns = [column[0] for column in cursor.description]
        if len(columns) != len(set(columns)):
            raise ValueError("result column names must be unique")
        with tempfile.NamedTemporaryFile(mode="wb", dir=destination.parent,
                                         prefix=".snapshot-", suffix=".tmp", delete=False) as stream:
            temporary = Path(stream.name)
            seen_comment = False
            while batch := cursor.fetchmany(fetch_rows):
                for values in batch:
                    row = normalize_row(dict(zip(columns, values)))
                    kind = row["record_type"]
                    if kind == "ticket" and seen_comment:
                        raise ValueError("all tickets must precede comments")
                    seen_comment |= kind == "comment"
                    encoded = (json.dumps(row, allow_nan=False, ensure_ascii=False,
                                          sort_keys=True, separators=(",", ":")) + "\n").encode("utf-8")
                    if len(encoded) > line_bytes:
                        raise ValueError("encoded row exceeds line-bytes")
                    if receipt["bytes"] + len(encoded) > max_output_bytes:
                        raise ValueError("snapshot exceeds max-output-bytes")
                    stream.write(encoded)
                    digest.update(encoded)
                    receipt["bytes"] += len(encoded)
                    receipt["rows"] += 1
                    receipt["tickets" if kind == "ticket" else "comments"] += 1
                del batch
            stream.flush()
            os.fsync(stream.fileno())
        connection.execute("COMMIT")
        active = False
        os.link(temporary, destination)
        receipt["sha256"] = digest.hexdigest()
        return receipt
    except BaseException:
        if active:
            with suppress(Exception):
                connection.execute("ROLLBACK")
        raise
    finally:
        if temporary is not None:
            try:
                temporary.unlink(missing_ok=True)
            except OSError as error:
                # Cleanup must not mask a source failure or successful publication.
                with suppress(Exception):
                    print(f"Warning: could not remove temporary snapshot {temporary}: {error}", file=sys.stderr)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--database", required=True, help="read-only DuckDB source path")
    parser.add_argument("--query-file", required=True, help="trusted single SELECT SQL file")
    parser.add_argument("--output", required=True)
    parser.add_argument("--fetch-rows", type=int, default=250)
    parser.add_argument("--line-bytes", type=int, default=65536)
    parser.add_argument("--max-output-bytes", type=int, default=1 << 30)
    args = parser.parse_args()
    import duckdb

    with duckdb.connect(args.database, read_only=True) as connection:
        receipt = export_snapshot(connection, Path(args.query_file).read_text(encoding="utf-8"),
                                  args.output, args.fetch_rows, args.line_bytes, args.max_output_bytes)
    print(json.dumps(receipt, sort_keys=True))


if __name__ == "__main__":
    main()
