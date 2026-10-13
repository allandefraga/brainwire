"""Bounded JSONL ingestion example using Brainwire's tested Bolt/Cypher subset.

Each JSON line must contain entity_id, email, active, tags and embedding.
All listed fields are authoritative: null removes a stored property.
Install the Neo4j Python driver in your own virtual environment before running.
"""

import argparse
from itertools import islice
import json

from neo4j import GraphDatabase

QUERY = """
UNWIND $rows AS row
MERGE (c:Customer {entity_id: row.entity_id})
SET c.email = row.email, c.active = row.active,
    c.tags = row.tags, c.embedding = row.embedding
"""
REQUIRED = {"entity_id", "email", "active", "tags", "embedding"}


def read_rows(stream):
    for line_number, line in enumerate(stream, 1):
        if not line.strip():
            continue
        row = json.loads(line)
        if not isinstance(row, dict) or not REQUIRED.issubset(row):
            raise ValueError(f"line {line_number}: requires fields {sorted(REQUIRED)}")
        if not isinstance(row["entity_id"], str) or not row["entity_id"]:
            raise ValueError(f"line {line_number}: entity_id must be a nonempty string")
        yield row


def ingest(driver, database, rows, batch_size=250):
    """Each batch commits independently; earlier batches survive later errors."""
    if batch_size <= 0:
        raise ValueError("batch_size must be positive")
    rows = iter(rows)
    committed = 0
    with driver.session(database=database) as session:
        while batch := list(islice(rows, batch_size)):
            session.execute_write(lambda tx: tx.run(QUERY, rows=batch).consume())
            committed += len(batch)
    return committed


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("file")
    parser.add_argument("--database", default="customers")
    parser.add_argument("--uri", default="bolt://127.0.0.1:7687")
    parser.add_argument("--batch-size", type=int, default=250)
    args = parser.parse_args()
    if args.batch_size <= 0:
        parser.error("batch-size must be positive")
    if __package__:
        from .connection import auth_from_env
    else:
        from connection import auth_from_env
    with open(args.file) as stream, GraphDatabase.driver(args.uri, auth=auth_from_env(), liveness_check_timeout=30) as driver:
        total = ingest(driver, args.database, read_rows(stream), args.batch_size)
    print(f"Committed {total} input rows to {args.database}; repeated IDs update existing nodes.")
