"""Import immutable synthetic Zendesk-shaped tickets/comments into a shadow graph.

This is a snapshot adapter, not CDC, deletion synchronization or source history.
Apply ontology.json first. Ticket parents must precede comments, or share their
batch. Comment parents are immutable. Run one importer per source ID; apply
overlapping snapshots serially in source order per tenant/connector. Resuming
an older snapshot after a newer one can overwrite newer properties.
"""
import argparse
from collections import Counter
import json
from pathlib import Path

from examples.python.resumable_customers import ingest

CONTRACT_ID = "external-zendesk-snapshot-v1"
TICKETS = """
UNWIND $rows AS row
MERGE (t:Ticket {entity_id: row.entity_id})
SET t.tenant_id = row.tenant_id, t.connector_id = row.connector_id,
    t.external_id = row.external_id, t.subject = row.subject,
    t.status = row.status, t.priority = row.priority, t.tags = row.tags
"""
EXISTING_COMMENTS = """
UNWIND $rows AS row
MATCH (c:TicketComment {entity_id: row.entity_id})
RETURN c.entity_id AS entity_id, c.ticket_entity_id AS ticket_entity_id
"""
COMMENT_TICKETS = """
UNWIND $rows AS row
MATCH (t:Ticket {entity_id: row.ticket_entity_id})
RETURN row.entity_id AS entity_id, t.entity_id AS ticket_entity_id
"""
COMMENTS = """
UNWIND $rows AS row
MERGE (c:TicketComment {entity_id: row.entity_id})
SET c.tenant_id = row.tenant_id, c.connector_id = row.connector_id,
    c.external_id = row.external_id, c.ticket_id = row.ticket_id,
    c.ticket_entity_id = row.ticket_entity_id, c.body = row.body,
    c.is_public = row.is_public
"""
HAS_COMMENTS = """
UNWIND $rows AS row
MATCH (t:Ticket {entity_id: row.ticket_entity_id}),
      (c:TicketComment {entity_id: row.entity_id})
MERGE (t)-[:HAS_COMMENT]->(c)
"""


def identity(tenant, connector, kind, external):
    return json.dumps([tenant, connector, kind, external], separators=(",", ":"), ensure_ascii=False)


def normalize_row(row):
    if not isinstance(row, dict):
        raise ValueError("snapshot row must be an object")
    kind = row.get("record_type")
    if kind not in ("ticket", "comment"):
        raise ValueError("record_type must be ticket or comment")
    common = {"tenant_id", "connector_id", "external_id"}
    fields = {"subject", "status", "priority", "tags"} if kind == "ticket" else {"ticket_id", "body", "is_public"}
    if not (common | fields).issubset(row):
        raise ValueError(f"{kind} requires {sorted(common | fields)}")
    for name in common | ({"ticket_id"} if kind == "comment" else set()):
        if not isinstance(row[name], str) or not row[name]:
            raise ValueError(f"{name} must be a nonempty string")
    for name in ({"subject", "status", "priority"} if kind == "ticket" else {"body"}):
        if row[name] is not None and not isinstance(row[name], str):
            raise ValueError(f"{name} must be a string or null")
    if kind == "ticket":
        tags = row["tags"]
        if tags is not None and (not isinstance(tags, list) or any(not isinstance(tag, str) for tag in tags)):
            raise ValueError("tags must be a list of strings or null")
    elif row["is_public"] is not None and type(row["is_public"]) is not bool:
        raise ValueError("is_public must be a boolean or null")
    normalized = {name: row[name] for name in common | fields}
    normalized["record_type"] = kind
    normalized["entity_id"] = identity(row["tenant_id"], row["connector_id"], kind, row["external_id"])
    if kind == "comment":
        normalized["ticket_entity_id"] = identity(row["tenant_id"], row["connector_id"], "ticket", row["ticket_id"])
    return normalized


def write_batch(tx, rows):
    tickets = [row for row in rows if row["record_type"] == "ticket"]
    comments = [row for row in rows if row["record_type"] == "comment"]
    parents = {}
    for row in comments:
        previous = parents.setdefault(row["entity_id"], row["ticket_entity_id"])
        if previous != row["ticket_entity_id"]:
            raise ValueError("duplicate comment has conflicting ticket parents")
    if tickets:
        tx.run(TICKETS, rows=tickets).consume()
    if not comments:
        return
    result = tx.run(EXISTING_COMMENTS, rows=comments)
    existing = list(result)
    result.consume()
    for record in existing:
        if parents[record["entity_id"]] != record["ticket_entity_id"]:
            raise ValueError("comment ticket parent is immutable")
    result = tx.run(COMMENT_TICKETS, rows=comments)
    found = Counter((record["entity_id"], record["ticket_entity_id"]) for record in result)
    result.consume()
    expected = Counter((row["entity_id"], row["ticket_entity_id"]) for row in comments)
    if found != expected:
        raise ValueError("every comment must reference an existing ticket")
    tx.run(COMMENTS, rows=comments).consume()
    tx.run(HAS_COMMENTS, rows=comments).consume()


def ingest_snapshot(driver, database, file, source_id, batch_size=250,
                    batch_bytes=1 << 20, line_bytes=64 << 10, *,
                    max_batches=None):
    return ingest(driver, database, file, source_id, batch_size, batch_bytes, line_bytes,
                  normalize_row=normalize_row, write_batch=write_batch,
                  contract_id=CONTRACT_ID, max_batches=max_batches)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("file")
    parser.add_argument("--source-id", required=True)
    parser.add_argument("--database", default="support-demo")
    parser.add_argument("--uri", default="bolt://127.0.0.1:7687")
    parser.add_argument("--batch-size", type=int, default=250)
    parser.add_argument("--batch-bytes", type=int, default=1 << 20)
    parser.add_argument("--line-bytes", type=int, default=64 << 10)
    parser.add_argument("--max-batches", type=int,
                        help="pause after this many committed source batches; rerun to resume")
    args = parser.parse_args()
    from neo4j import GraphDatabase
    from examples.python.connection import auth_from_env
    with GraphDatabase.driver(args.uri, auth=auth_from_env(), liveness_check_timeout=30) as driver:
        offset = ingest_snapshot(driver, args.database, args.file, args.source_id,
                                 args.batch_size, args.batch_bytes, args.line_bytes,
                                 max_batches=args.max_batches)
    size = Path(args.file).stat().st_size
    state = "complete" if offset == size else "paused"
    print(f"Source {args.source_id}: {state}; durable next_offset={offset}/{size} bytes")


if __name__ == "__main__":
    main()
