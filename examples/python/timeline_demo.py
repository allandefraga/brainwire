"""Explicit milestone nodes for a small application-managed timeline.

Print the required catalog, initialize a graph with it, and create the subject:
    python -m examples.python.timeline_demo --print-ontology > timeline.json
    brainwire db create demo --ontology timeline.json
    # Create Ticket/entity_id=ticket-1 through your ordinary authenticated client.

Record an event after starting the server (credentials use BRAINWIRE_BOLT_AUTH_FILE):
    python -m examples.python.timeline_demo --database demo --subject-id ticket-1 \
      --event-key opened-1 --kind opened --occurred-at 2026-11-03T10:00:00Z

Each event is a TimelineEvent node linked from its subject by HAS_EVENT. The
application chooses stable event keys and access rules; this example does not
capture changes automatically, provide AS OF reads, or implement retention/ACLs.
It attempts one explicit transaction. Reconcile an unknown commit result by the
returned/deterministic event ID before retrying; no managed retries occur here.

Read at most 100 milestones in chronological order with read_events(). Equal
timestamps are ordered by event ID. This caps returned rows; the server may
still scan and sort the subject's full timeline within its query budget.
Use read_event_page() for a timezone-aware time window and keyset cursor. Pages
are separate snapshots; the application must keep event time and ID immutable.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import re

from examples.python.client import identifier


def _text(value, name, limit=1000):
    if not isinstance(value, str) or not value or len(value.encode("utf-8")) > limit:
        raise ValueError(f"{name} must be a nonempty string of at most {limit} UTF-8 bytes")
    return value


def _timestamp(value, name):
    timestamp_error = f"{name} must be an ISO timestamp with a timezone (Z or ±HH:MM)"
    if not isinstance(value, str) or not re.search(r"(?:Z|[+-][0-9]{2}:[0-5][0-9])\Z", value):
        raise ValueError(timestamp_error)
    # fromisoformat can silently truncate fractional seconds after six digits.
    if re.search(r"[.,][0-9]{7}", value):
        raise ValueError(f"{name} must use at most six fractional digits (microsecond precision)")
    try:
        instant = datetime.fromisoformat(value.replace("Z", "+00:00"))
        if instant.tzinfo is None or instant.utcoffset() is None:
            raise ValueError()
        return instant.astimezone(timezone.utc).isoformat()
    except (ValueError, OverflowError):
        raise ValueError(timestamp_error) from None


def event_id(subject, key):
    """Stable identity within one graph; changing content does not change the key."""
    if not isinstance(subject, tuple) or len(subject) != 2:
        raise ValueError("subject must be (label, entity_id)")
    label = identifier(subject[0])
    subject_id = _text(subject[1], "subject entity_id")
    key = _text(key, "event key", 256)
    encoded = json.dumps([label, subject_id, key], ensure_ascii=False,
                         separators=(",", ":")).encode("utf-8")
    return "timeline:" + hashlib.sha256(encoded).hexdigest()


def ontology(subject_label="Ticket"):
    """Apply this catalog before recording events; use one signature per subject label."""
    subject_label = identifier(subject_label)
    if subject_label == "TimelineEvent":
        raise ValueError("subject label must differ from TimelineEvent")
    return {"version": "1", "nodes": [
        {"label": subject_label, "properties": ["title"]},
        {"label": "TimelineEvent", "properties": ["kind", "occurred_at", "payload_json"]}],
        "edges": [{"type": "HAS_EVENT", "from": {"label": subject_label},
                   "to": {"label": "TimelineEvent"}, "properties": []}]}


def record_event(driver, database, subject, key, kind, occurred_at, payload):
    """Commit one explicit event and link, rejecting a changed replay observed in the tx.

    Concurrent writers can receive a transaction conflict. This example has no
    retry/fencing policy and does not claim global immutability against other clients.
    """
    identity = event_id(subject, key)
    label = identifier(subject[0])
    if label == "TimelineEvent":
        raise ValueError("subject label must differ from TimelineEvent")
    _text(database, "database")
    kind = _text(kind, "event kind", 128)
    timestamp = _timestamp(occurred_at, "occurred_at")
    if not isinstance(payload, dict):
        raise ValueError("payload must be a JSON object")
    try:
        encoded = json.dumps(payload, ensure_ascii=False, sort_keys=True,
                             allow_nan=False, separators=(",", ":"))
    except (ValueError, TypeError, OverflowError):
        raise ValueError("payload must contain JSON values") from None
    if len(encoded.encode("utf-8")) > 65536:
        raise ValueError("payload exceeds 64 KiB of JSON")
    values = {"kind": kind, "occurred_at": timestamp, "payload_json": encoded}
    params = {"subject": subject[1], "event": identity, **values}
    with driver.session(database=database) as session:
        with session.begin_transaction() as tx:
            row = tx.run(f"MATCH (s:{label} {{entity_id:$subject}}) RETURN count(s) AS found", params).single()
            if row is None or row["found"] != 1:
                raise ValueError("exactly one existing subject is required")
            row = tx.run("MATCH (e:TimelineEvent {entity_id:$event}) "
                         "RETURN e.kind AS kind, e.occurred_at AS occurred_at, "
                         "e.payload_json AS payload_json", params).single()
            if row is not None and dict(row) != values:
                raise ValueError("event key already has different content")
            if row is None:
                row = tx.run("MERGE (e:TimelineEvent {entity_id:$event}) "
                             "SET e.kind=$kind, e.occurred_at=$occurred_at, e.payload_json=$payload_json "
                             "RETURN count(e) AS written", params).single()
                if row is None or row["written"] != 1:
                    raise RuntimeError("event write was not acknowledged")
            row = tx.run(f"MATCH (s:{label} {{entity_id:$subject}}), "
                         "(e:TimelineEvent {entity_id:$event}) MERGE (s)-[:HAS_EVENT]->(e) "
                         "RETURN count(*) AS linked", params).single()
            if row is None or row["linked"] != 1:
                raise RuntimeError("event link was not acknowledged")
            tx.commit()
    return identity


def read_events(driver, database, subject, *, limit=50):
    """Return the first 1–100 explicit milestones ordered by UTC timestamp and ID.

    Intended for timelines written by record_event(), which normalizes timestamps.
    This caps returned rows, not server scan/sort memory or response bytes. There
    is no pagination or snapshot guarantee across repeated calls.
    """
    if not isinstance(subject, tuple) or len(subject) != 2:
        raise ValueError("subject must be (label, entity_id)")
    label = identifier(subject[0])
    if label == "TimelineEvent":
        raise ValueError("subject label must differ from TimelineEvent")
    subject_id = _text(subject[1], "subject entity_id")
    _text(database, "database")
    if type(limit) is not int or not 1 <= limit <= 100:
        raise ValueError("limit must be an integer between 1 and 100")
    with driver.session(database=database) as session:
        rows = session.run(
            f"MATCH (s:{label} {{entity_id:$subject}})-[:HAS_EVENT]->(e:TimelineEvent) "
            "RETURN e.entity_id AS id, e.kind AS kind, e.occurred_at AS occurred_at, "
            "e.payload_json AS payload_json ORDER BY e.occurred_at, e.entity_id LIMIT $limit",
            {"subject": subject_id, "limit": limit})
        return [dict(row) for row in rows]


def read_event_page(driver, database, subject, *, start_at=None, end_at=None,
                    after=None, limit=50):
    """Read explicit milestones in [start_at, end_at), ordered by time and ID.

    Bounds accept ISO timestamps with up to six fractional digits and timezone
    Z or ±HH:MM. They normalize to the UTC format written by record_event().
    Pass the returned next_cursor as after, retaining the same subject and bounds.
    An extra visible row determines whether a next_cursor is returned.

    Keep event time and identity immutable in the application. Each call has its
    own read snapshot: concurrent insertions, updates and deletions can change
    later pages. LIMIT bounds output rows, not server scan/sort work, response
    bytes or process memory. This reads current explicit events, not AS OF data.
    """
    if not isinstance(subject, tuple) or len(subject) != 2:
        raise ValueError("subject must be (label, entity_id)")
    label = identifier(subject[0])
    if label == "TimelineEvent":
        raise ValueError("subject label must differ from TimelineEvent")
    subject_id = _text(subject[1], "subject entity_id")
    _text(database, "database")
    if type(limit) is not int or not 1 <= limit <= 100:
        raise ValueError("limit must be an integer between 1 and 100")
    start = None if start_at is None else _timestamp(start_at, "start_at")
    end = None if end_at is None else _timestamp(end_at, "end_at")
    if start is not None and end is not None and start >= end:
        raise ValueError("start_at must be earlier than end_at")
    position = None
    if after is not None:
        if not isinstance(after, dict) or set(after) != {"occurred_at", "id"}:
            raise ValueError("after must be a returned cursor with occurred_at and id")
        timestamp = _timestamp(after["occurred_at"], "cursor occurred_at")
        if timestamp != after["occurred_at"]:
            raise ValueError("cursor occurred_at must use the returned canonical UTC format")
        identity = _text(after["id"], "cursor event id")
        if (start is not None and timestamp < start) or (end is not None and timestamp >= end):
            raise ValueError("cursor must be inside the requested time window")
        position = (timestamp, identity)

    parameters = {"subject": subject_id, "limit": limit + 1}
    predicates = []
    if start is not None:
        predicates.append("e.occurred_at >= $start_at")
        parameters["start_at"] = start
    if end is not None:
        predicates.append("e.occurred_at < $end_at")
        parameters["end_at"] = end
    if position is not None:
        predicates.append("(e.occurred_at > $after_at OR "
                          "(e.occurred_at = $after_at AND e.entity_id > $after_id))")
        parameters.update(after_at=position[0], after_id=position[1])
    where = "WHERE " + " AND ".join(predicates) + " " if predicates else ""
    query = (f"MATCH (s:{label} {{entity_id:$subject}})-[:HAS_EVENT]->(e:TimelineEvent) "
             f"{where}RETURN e.entity_id AS id, e.kind AS kind, e.occurred_at AS occurred_at, "
             "e.payload_json AS payload_json ORDER BY e.occurred_at, e.entity_id LIMIT $limit")
    rows = []
    with driver.session(database=database, default_access_mode="READ",
                        fetch_size=min(limit + 1, 100)) as session:
        for record in session.run(query, parameters):
            if len(rows) >= limit + 1:
                raise ValueError("timeline query returned more than the requested row bound")
            row = dict(record)
            timestamp = _timestamp(row.get("occurred_at"), "stored occurred_at")
            if timestamp != row["occurred_at"]:
                raise ValueError("stored events must use record_event's canonical UTC format")
            identity = _text(row.get("id"), "stored event id")
            current = (timestamp, identity)
            if position is not None and current <= position:
                raise ValueError("timeline rows must be strictly ordered after the cursor")
            if (start is not None and timestamp < start) or (end is not None and timestamp >= end):
                raise ValueError("timeline query returned an event outside the time window")
            position = current
            rows.append(row)
    next_cursor = None
    if len(rows) > limit:
        rows = rows[:limit]
        next_cursor = {"occurred_at": rows[-1]["occurred_at"], "id": rows[-1]["id"]}
    return {"events": rows, "next_cursor": next_cursor}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--print-ontology", action="store_true")
    parser.add_argument("--subject-label", default="Ticket")
    parser.add_argument("--subject-id")
    parser.add_argument("--event-key")
    parser.add_argument("--kind")
    parser.add_argument("--occurred-at")
    parser.add_argument("--payload-json", default="{}")
    parser.add_argument("--uri", default="bolt://127.0.0.1:7687")
    parser.add_argument("--database")
    args = parser.parse_args(argv)
    if args.print_ontology:
        print(json.dumps(ontology(args.subject_label), indent=2))
        return
    if not all((args.database, args.subject_id, args.event_key, args.kind, args.occurred_at)):
        parser.error("recording requires --database, --subject-id, --event-key, --kind and --occurred-at")
    from neo4j import GraphDatabase
    from examples.python.connection import auth_from_env
    with GraphDatabase.driver(args.uri, auth=auth_from_env()) as driver:
        identity = record_event(driver, args.database, (args.subject_label, args.subject_id),
                                args.event_key, args.kind, args.occurred_at, json.loads(args.payload_json))
    print(identity)


if __name__ == "__main__":
    main()
