"""Validate a typed ingestion batch locally, or send it in one Bolt transaction."""

import argparse
from collections import defaultdict
import json
import math
import os
from pathlib import Path
import stat
from urllib.parse import urlsplit

from brainwire import connect
from brainwire.client import identifier
from examples.python.connection import auth_from_env


MAX_FILE_BYTES = 256 * 1024
MAX_ROWS = 256
MAX_TYPES = 64
MAX_FIELDS = 32
MAX_TEXT_BYTES = 4096
MAX_ID_BYTES = 256
TYPES = {"boolean": bool, "integer": int, "float": float, "string": str}


def contract_object(value, where, required, optional=()):
    if type(value) is not dict or not set(required) <= value.keys() <= set(required) | set(optional):
        raise ValueError(f"{where} has missing or unknown fields")
    return value


def read_json(path):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError("duplicate JSON field")
            result[key] = value
        return result

    def nonfinite(_):
        raise ValueError("nonfinite JSON number")

    with os.fdopen(os.open(path, os.O_RDONLY | getattr(os, "O_NONBLOCK", 0)), "rb") as stream:
        if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
            raise ValueError("input must be a regular JSON file")
        raw = stream.read(MAX_FILE_BYTES + 1)
    if len(raw) > MAX_FILE_BYTES:
        raise ValueError(f"input exceeds {MAX_FILE_BYTES} bytes")
    try:
        return json.loads(raw.decode("utf-8"), object_pairs_hook=unique, parse_constant=nonfinite)
    except (UnicodeError, RecursionError) as error:
        raise ValueError("input must be UTF-8 JSON with bounded nesting") from error


def field_specs(fields, where):
    if type(fields) is not dict or len(fields) > MAX_FIELDS:
        raise ValueError(f"{where} must declare at most {MAX_FIELDS} properties")
    for field, spec in fields.items():
        identifier(field)
        if field == "entity_id":
            raise ValueError(f"{where}.entity_id is reserved for node identity")
        name = f"{where}.{field}"
        contract_object(spec, name, ("type",), ("required", "nullable"))
        kind = spec["type"]
        if type(kind) is not str or kind not in TYPES:
            raise ValueError(f"{name} has an unknown scalar type")
        if any(type(spec.get(flag, False)) is not bool for flag in ("required", "nullable")):
            raise ValueError(f"{name} flags must be boolean")
    return fields


def schema_specs(schema):
    contract_object(schema, "schema", ("version", "nodes", "relationships"))
    if type(schema["version"]) is not int or schema["version"] != 1:
        raise ValueError("schema version must be 1")
    nodes, edges = schema["nodes"], schema["relationships"]
    if (type(nodes) is not dict or type(edges) is not dict
            or not 1 <= len(nodes) + len(edges) <= MAX_TYPES):
        raise ValueError(f"schema must declare 1 to {MAX_TYPES} types")
    for label, fields in nodes.items():
        field_specs(fields, identifier(label))
    for name, edge in edges.items():
        identifier(name)
        contract_object(edge, name, ("from", "to", "properties"))
        if any(type(edge[key]) is not str or edge[key] not in nodes for key in ("from", "to")):
            raise ValueError(f"{name} endpoints must name declared node labels")
        field_specs(edge["properties"], name)
    return nodes, edges


def text_value(value, maximum, *, nonempty=False):
    if type(value) is not str or len(value) > maximum or (nonempty and not value):
        return False
    try:
        return len(value.encode("utf-8")) <= maximum
    except UnicodeError:
        return False


def properties(fields, values, where):
    if type(values) is not dict or len(values) > MAX_FIELDS or values.keys() - fields.keys():
        raise ValueError(f"{where} has unknown properties")
    for field, spec in fields.items():
        if field not in values:
            if spec.get("required", False):
                raise ValueError(f"{where}.{field} is required")
            continue
        value, kind = values[field], spec["type"]
        if value is None and spec.get("nullable", False):
            continue
        valid = type(value) is TYPES[kind]
        if valid and kind == "integer":
            valid = -(1 << 63) <= value < (1 << 63)
        elif valid and kind == "float":
            valid = math.isfinite(value)
        elif valid and kind == "string":
            valid = text_value(value, MAX_TEXT_BYTES)
        if not valid:
            raise ValueError(f"{where}.{field} must be {kind}")
    return dict(values)


def prepare_batch(schema, batch):
    nodes, edges = schema_specs(schema)
    contract_object(batch, "batch", ("nodes", "relationships"))
    node_rows, edge_rows = batch["nodes"], batch["relationships"]
    if (type(node_rows) is not list or type(edge_rows) is not list
            or len(node_rows) + len(edge_rows) > MAX_ROWS):
        raise ValueError(f"batch must contain at most {MAX_ROWS} rows")

    node_groups, edge_groups = defaultdict(list), defaultdict(list)
    total_bytes = 0
    for rows, specs, groups, kind, ids in (
        (node_rows, nodes, node_groups, "label", ("entity_id",)),
        (edge_rows, edges, edge_groups, "type", ("source_id", "target_id")),
    ):
        seen = set()
        for index, row in enumerate(rows, 1):
            where = f"{kind} row {index}"
            contract_object(row, where, (kind, "properties", *ids))
            name = identifier(row[kind])
            if name not in specs:
                raise ValueError(f"{where} references an unknown {kind}")
            key = tuple(row[field] for field in ids)
            if any(not text_value(value, MAX_ID_BYTES, nonempty=True) for value in key):
                raise ValueError(f"{where} identities must be nonempty UTF-8 up to {MAX_ID_BYTES} bytes")
            if (name, key) in seen:
                raise ValueError(f"{where} duplicates an identity in this batch")
            seen.add((name, key))

            spec = specs[name]
            is_edge = kind == "type"
            fields = spec["properties"] if is_edge else spec
            record = {field: row[field] for field in ids}
            record["properties"] = properties(fields, row["properties"], where)
            total_bytes += len(json.dumps(record, ensure_ascii=False, separators=(",", ":")).encode("utf-8"))
            if total_bytes > MAX_FILE_BYTES:
                raise ValueError(f"batch exceeds {MAX_FILE_BYTES} validated UTF-8 JSON payload bytes")
            group = (name, spec["from"], spec["to"]) if is_edge else name
            groups[group].append(record)

    return node_groups, edge_groups


def write_prepared(graph, node_groups, edge_groups):
    counts = {"nodes": 0, "relationships": 0}
    if not node_groups and not edge_groups:
        return counts

    with graph.driver.session(database=graph.database) as session:
        with session.begin_transaction() as tx:
            for label, rows in node_groups.items():
                result = tx.run(
                    f"UNWIND $rows AS row MERGE (n:{label} {{entity_id:row.entity_id}}) "
                    "SET n += row.properties RETURN count(*) AS processed",
                    rows=rows,
                ).single(strict=True)
                if result["processed"] != len(rows):
                    raise ValueError("node write count does not match the validated batch")
                counts["nodes"] += len(rows)
            for (name, source, target), rows in edge_groups.items():
                result = tx.run(
                    f"UNWIND $rows AS row MATCH (a:{source} {{entity_id:row.source_id}}), "
                    f"(b:{target} {{entity_id:row.target_id}}) "
                    f"MERGE (a)-[r:{name}]->(b) SET r += row.properties "
                    "RETURN count(*) AS processed",
                    rows=rows,
                ).single(strict=True)
                if result["processed"] != len(rows):
                    raise ValueError("relationship write count does not match the validated batch")
                counts["relationships"] += len(rows)
            tx.commit()
    return counts


def write_batch(graph, schema, batch):
    """Validate the whole batch before opening a session; write once without retry."""
    return write_prepared(graph, *prepare_batch(schema, batch))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("schema", type=Path)
    parser.add_argument("batch", type=Path)
    parser.add_argument("--database", help="write to this graph instead of only validating")
    parser.add_argument("--uri", default="bolt://127.0.0.1:7687")
    args = parser.parse_args(argv)
    try:
        node_groups, edge_groups = prepare_batch(read_json(args.schema), read_json(args.batch))
        if args.database is None:
            counts = {"nodes": sum(map(len, node_groups.values())),
                      "relationships": sum(map(len, edge_groups.values()))}
        else:
            uri = urlsplit(args.uri)
            if uri.scheme != "bolt" or uri.hostname not in ("127.0.0.1", "::1"):
                raise ValueError("the example CLI requires a numeric loopback Bolt URI")
            with connect(args.uri, args.database, auth=auth_from_env(), connection_timeout=5) as graph:
                counts = write_prepared(graph, node_groups, edge_groups)
    except (OSError, ValueError) as error:
        parser.exit(2, f"{error}\n")
    print(json.dumps({**counts, "written": args.database is not None}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
