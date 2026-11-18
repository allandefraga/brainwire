"""Record and recall project decisions with sources over Bolt."""
import argparse
import hashlib
import json
import re

from brainwire import connect
from examples.python.connection import auth_from_env, bolt_connection_options


PREFIX = "agent-memory-"
KEY_BYTES = len(PREFIX) + 2 * hashlib.sha256().digest_size
PROJECT_BYTES = 256
TITLE_BYTES = 256
BODY_BYTES = 2048
URL_BYTES = 1024
MAX_RESULTS = 10
CURSOR_BYTES = 32768


def bounded_text(value, field, maximum):
    if (not isinstance(value, str) or not value.strip() or len(value) > maximum
            or len(value.encode("utf-8")) > maximum):
        raise ValueError(f"{field} must be nonempty text of at most {maximum} UTF-8 bytes")
    return value


def memory_key(*values):
    data = json.dumps(values, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    return PREFIX + hashlib.sha256(data).hexdigest()


def remember(graph, project, title, body, source_title, source_url, *, allow=None):
    """Record an example decision and its source.

    A trusted graph writer with instance administration can pass a nonempty
    allow list to publish both nodes and their ACLs atomically. Shared sources
    and replays require matching content and ACLs; change grants separately.
    """
    project = bounded_text(project, "project", PROJECT_BYTES)
    title = bounded_text(title, "title", TITLE_BYTES)
    body = bounded_text(body, "body", BODY_BYTES)
    source_title = bounded_text(source_title, "source title", TITLE_BYTES)
    source_url = bounded_text(source_url, "source URL", URL_BYTES)
    source = memory_key(project, source_title, source_url)
    decision = memory_key(project, title, body, source)
    query = (
        "MATCH (p:Project {entity_id: $project}) "
        "MERGE (d:Decision {entity_id: $decision}) "
        "ON CREATE SET d.title = $title, d.body = $body "
        "MERGE (s:Source {entity_id: $source}) "
        "ON CREATE SET s.title = $source_title, s.url = $source_url "
        "MERGE (p)-[:HAS_DECISION]->(d) "
        "MERGE (d)-[:SUPPORTED_BY]->(s) "
        "RETURN d.entity_id AS decision_id, s.entity_id AS source_id"
    )
    params = {"project": project, "decision": decision, "source": source, "title": title,
              "body": body, "source_title": source_title, "source_url": source_url}
    missing_project = "project is unavailable; create it before recording a memory"
    if allow is None:
        rows = graph.run_cypher(query, params)
        if not rows:
            raise ValueError(missing_project)
        return rows[0]

    if not isinstance(allow, (list, tuple)) or not 1 <= len(allow) <= 5000:
        raise ValueError("allow must contain 1 to 5000 principals")
    principals = {bounded_text(principal, "principal", 256) for principal in allow}
    refs = [{"label": "Decision", "entityId": decision}, {"label": "Source", "entityId": source}]
    with graph.driver.session(database=graph.database) as session, session.begin_transaction() as tx:
        existing = {}
        for row in tx.run(
            "CALL db.nodeAcls($refs) YIELD label, entityId, allow, deny, connectorIds, sourceTable, sources "
            "RETURN label, entityId, allow, deny, connectorIds, sourceTable, sources",
            refs=refs,
        ).data():
            if (set(row["allow"]) != principals or row["deny"] or row["connectorIds"]
                    or row["sourceTable"] is not None or row["sources"]):
                raise ValueError("existing decision or source has a different ACL")
            existing[(row["label"], row["entityId"])] = row
        rows = tx.run(
            query + ", d.title = $title AND d.body = $body AND s.title = $source_title "
            "AND s.url = $source_url AS content_matches",
            params,
        ).data()
        if not rows:
            raise ValueError(missing_project)
        record = rows[0]
        if record.pop("content_matches") is not True:
            raise ValueError("existing decision or source has different content")
        # Restamp the observed ACL to register conflicts with concurrent revocations.
        grants = sorted(principals)
        entries = [existing.get((ref["label"], ref["entityId"]), {**ref, "allow": grants})
                   for ref in refs]
        updated = tx.run(
            "CALL db.setNodeAcls($rows) YIELD updated RETURN updated", rows=entries,
        ).single(strict=True)["updated"]
        if updated != len(refs):
            raise ValueError("decision and source ACLs were not acknowledged")
        tx.commit()
        return record


def _output_json(result):
    return json.dumps(result, indent=2, ensure_ascii=False, allow_nan=False) + "\n"


def recall(graph, project, limit=5, *, max_bytes=None):
    """Read visible decisions with visible sources in one graph snapshot."""
    return _recall(graph, project, limit, max_bytes=max_bytes)


def _cursor(value, graph_name, project):
    if not isinstance(value, dict) or set(value) != {"graph", "project", "decision_id", "source_id"}:
        raise ValueError("after must be a returned cursor with graph, project, decision_id and source_id")
    graph_value = bounded_text(value["graph"], "cursor graph", 4096)
    project_value = bounded_text(value["project"], "cursor project", PROJECT_BYTES)
    if graph_value != graph_name or project_value != project:
        raise ValueError("cursor graph and project must match this recall request")
    result = {"graph": graph_name, "project": project}
    for field in ("decision_id", "source_id"):
        identity = bounded_text(value[field], f"cursor {field}", KEY_BYTES)
        if re.fullmatch(re.escape(PREFIX) + r"[0-9a-f]{64}", identity) is None:
            raise ValueError(f"cursor {field} must be a generated memory ID")
        result[field] = identity
    return result


def recall_page(graph, project, limit=5, *, after=None, max_bytes=65536):
    """Read a bounded page of visible decision/source pairs ordered by their IDs.

    Pass next_cursor as after with the same graph and project. The cursor is a
    position, never authorization, and remains usable if its source is deleted.
    Each page has a new snapshot: concurrent changes can omit or repeat records.
    max_bytes caps the encoded response, including its cursor, not server work or
    process memory. An error is raised if one complete pair and cursor cannot fit.
    """
    project = bounded_text(project, "project", PROJECT_BYTES)
    graph_name = bounded_text(graph.database, "graph", 4096)
    position = None if after is None else _cursor(after, graph_name, project)
    if type(max_bytes) is not int or not 1 <= max_bytes <= 1 << 20:
        raise ValueError("max_bytes must be an integer from 1 to 1048576")
    return _recall(graph, project, limit, max_bytes=max_bytes, paged=True, after=position,
                   graph_name=graph_name)


def _recall(graph, project, limit, *, max_bytes, paged=False, after=None, graph_name=None):
    project = bounded_text(project, "project", PROJECT_BYTES)
    if type(limit) is not int or not 1 <= limit <= MAX_RESULTS:
        raise ValueError(f"limit must be an integer from 1 to {MAX_RESULTS}")
    result = {"project": project, "records": [], "truncated": False}
    if paged:
        result["next_cursor"] = None
    if max_bytes is not None and (type(max_bytes) is not int
                                  or max_bytes < len(_output_json(result).encode("utf-8"))):
        raise ValueError("max_bytes must be an integer large enough for an empty recall response")
    records = result["records"]
    parameters = {"project": project, "prefix": PREFIX, "limit": limit + 1}
    predicate = ""
    if after is not None:
        predicate = ("AND (d.entity_id > $after_decision OR "
                     "(d.entity_id = $after_decision AND s.entity_id > $after_source)) ")
        parameters.update(after_decision=after["decision_id"], after_source=after["source_id"])
    previous = None if after is None else (after["decision_id"], after["source_id"])
    with graph.stream_cypher(
        "MATCH (p:Project {entity_id: $project})-[:HAS_DECISION]->(d:Decision)"
        "-[:SUPPORTED_BY]->(s:Source) "
        "WHERE d.entity_id STARTS WITH $prefix AND s.entity_id STARTS WITH $prefix "
        + predicate + "RETURN DISTINCT d.entity_id AS decision_id, d.title AS title, d.body AS body, "
        "s.entity_id AS source_id, s.title AS source_title, s.url AS source_url "
        "ORDER BY decision_id, source_id LIMIT $limit",
        parameters, fetch_size=limit + 1,
    ) as rows:
        fields = {"decision_id": KEY_BYTES, "title": TITLE_BYTES, "body": BODY_BYTES,
                  "source_id": KEY_BYTES, "source_title": TITLE_BYTES, "source_url": URL_BYTES}
        for row in rows:
            if len(records) == limit:
                result["truncated"] = True
                break
            for field, maximum in fields.items():
                bounded_text(row[field], field, maximum)
            if paged:
                position = _cursor({"graph": graph_name, "project": project,
                                    **{field: row[field] for field in ("decision_id", "source_id")}},
                                   graph_name, project)
                order = (position["decision_id"], position["source_id"])
                if previous is not None and order <= previous:
                    raise ValueError("memory page records are not strictly after the cursor")
                previous = order
                result.update(next_cursor=position, truncated=True)
            records.append(row)
            if max_bytes is not None and len(_output_json(result).encode("utf-8")) > max_bytes:
                records.pop()
                result["truncated"] = True
                if paged:
                    if not records:
                        raise ValueError("max_bytes must fit one complete memory pair and cursor")
                    result["next_cursor"] = {"graph": graph_name, "project": project,
                                             **{field: records[-1][field]
                                                for field in ("decision_id", "source_id")}}
                break
        else:
            if paged:
                result.update(next_cursor=None, truncated=False)
    return result


def _cursor_json(value):
    bounded_text(value, "after JSON", CURSOR_BYTES)

    def unique(pairs):
        result = {}
        for key, item in pairs:
            if key in result:
                raise ValueError("cursor JSON must not repeat object keys")
            result[key] = item
        return result

    try:
        return json.loads(value, object_pairs_hook=unique)
    except RecursionError:
        raise ValueError("cursor JSON nesting is too deep") from None


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--uri", default="bolt://127.0.0.1:7687")
    parser.add_argument("--tls-ca", help="verify Bolt TLS using this PEM CA file")
    parser.add_argument("--database", default="company_demo")
    subparsers = parser.add_subparsers(dest="command", required=True)
    write = subparsers.add_parser("remember", help="record a decision and source in an existing project")
    for argument in ("project", "title", "body", "source-title", "source-url"):
        write.add_argument(f"--{argument}", required=True)
    write.add_argument("--allow", action="append", metavar="PRINCIPAL",
                       help="stamp an ACL atomically (repeat for each principal; requires instance admin)")
    read = subparsers.add_parser("recall", help="read visible decisions and sources for a project")
    read.add_argument("--project", required=True)
    read.add_argument("--limit", type=int, default=5)
    read.add_argument("--max-bytes", type=int, help="cap the UTF-8 JSON response size")
    read.add_argument("--page", action="store_true", help="return a bounded page and continuation cursor")
    read.add_argument("--after", metavar="CURSOR_JSON",
                      help="continue with the returned cursor JSON object (implies --page)")
    args = parser.parse_args(argv)
    try:
        after = None
        if args.command == "recall" and args.after is not None:
            after = _cursor(_cursor_json(args.after), args.database, args.project)
        uri, config = bolt_connection_options(args.uri, args.tls_ca)
        with connect(uri, args.database, auth=auth_from_env(), connection_timeout=5, **config) as graph:
            if args.command == "remember":
                result = remember(graph, args.project, args.title, args.body, args.source_title, args.source_url,
                                  allow=args.allow)
            elif args.page or args.after is not None:
                result = recall_page(graph, args.project, args.limit, after=after,
                                     max_bytes=65536 if args.max_bytes is None else args.max_bytes)
            else:
                result = recall(graph, args.project, args.limit, max_bytes=args.max_bytes)
    except ValueError as error:
        parser.exit(2, f"{error}\n")
    print(_output_json(result), end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
