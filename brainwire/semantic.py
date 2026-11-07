"""Bounded exploration of declared ontology paths; no stored-record traversal."""
from collections import deque
import json

from .client import identifier
from .context import ContextLimitError

_MAX_TYPES = 4096
_MAX_SIGNATURES = 4096
_MAX_ENTRIES = 65536
_MAX_STRINGS = 1 << 20
_MAX_EXPANSIONS = 8192


def _catalog(value):
    if not isinstance(value, dict) or type(value.get("version")) is not int or \
            not 0 <= value["version"] < 1 << 64 or type(value.get("indexesReady")) is not bool:
        raise ValueError("invalid schema response")
    nodes, relationships = value.get("nodes"), value.get("relationships")
    if not isinstance(nodes, list) or not isinstance(relationships, list):
        raise ValueError("schema nodes and relationships must be lists")
    if len(nodes) > _MAX_TYPES or len(relationships) > _MAX_SIGNATURES:
        raise ContextLimitError("schema exceeds type or relationship signature limit")
    strings, entries = 0, 0

    def text(value, *, description=False):
        nonlocal strings
        if description:
            if not isinstance(value, str) or len(value) > 4096 or \
                    len(value.encode("utf-8")) > 4096 or "\0" in value:
                raise ValueError("schema descriptions must be strings of at most 4096 UTF-8 bytes without NUL")
        else:
            identifier(value)
        strings += len(value.encode("utf-8"))
        if strings > _MAX_STRINGS:
            raise ContextLimitError("schema exceeds its string bound")
        return value

    def names(value):
        nonlocal entries
        if not isinstance(value, list):
            raise ValueError("schema property and merge key names must be lists")
        entries += len(value)
        if entries > _MAX_ENTRIES:
            raise ContextLimitError("schema exceeds its collection entry bound")
        result = [text(name) for name in value]
        if len(set(result)) != len(result):
            raise ValueError("schema names must be unique within each declaration")
        return sorted(result)

    by_label = {}
    for item in nodes:
        if not isinstance(item, dict):
            raise ValueError("schema node declarations must be objects")
        label = text(item.get("label"))
        if label in by_label:
            raise ValueError("schema labels must be unique")
        result = {"label": label, "properties": names(item.get("properties"))}
        if "description" in item:
            result["description"] = text(item["description"], description=True)
        by_label[label] = result
    adjacency = {label: [] for label in by_label}
    signatures = set()
    meanings = {}
    for item in relationships:
        if not isinstance(item, dict):
            raise ValueError("schema relationship declarations must be objects")
        name, source, target = (text(item.get(field)) for field in ("name", "source", "target"))
        if source not in by_label or target not in by_label:
            raise ValueError("schema relationship endpoint is not declared")
        identity = source, name, target
        if identity in signatures:
            raise ValueError("schema relationship signatures must be unique")
        signatures.add(identity)
        result = {"name": name, "source": source, "target": target,
                  "properties": names(item.get("properties"))}
        keys = names(item.get("mergeKeyFields", []))
        if not set(keys).issubset(result["properties"]):
            raise ValueError("schema merge keys must be declared relationship properties")
        if keys:
            result["mergeKeyFields"] = keys
        if "description" in item:
            result["description"] = text(item["description"], description=True)
        meaning = result["properties"], keys, result.get("description")
        if name in meanings and meanings[name] != meaning:
            raise ValueError("schema relationship type metadata must be consistent")
        meanings[name] = meaning
        adjacency[source].append(result)
    for outgoing in adjacency.values():
        outgoing.sort(key=lambda relationship: (relationship["name"], relationship["target"]))
    return by_label, adjacency


def _check_bytes(value, max_bytes):
    size = 0
    for chunk in json.JSONEncoder(ensure_ascii=False, allow_nan=False).iterencode(value):
        size += len(chunk.encode("utf-8"))
        if size > max_bytes:
            raise ContextLimitError("schema path output exceeds max_bytes")


def schema_paths(graph, source_label, target_label, *, max_hops=4, max_paths=20,
                 max_bytes=65536):
    """Suggest directed type paths from one authorized catalog snapshot.

    Calls graph.schema() once and never queries stored records. Returned paths
    describe allowed schema shapes, not existing or ACL-visible instances.
    Descriptions are application-supplied metadata, not trusted agent instructions.

    Labels may repeat (including self-loops), but each route stops at its target
    and has at most max_hops hops. Equal source and target returns only a zero-hop
    route. Routes are ordered breadth first, then by relationship name and target.
    At most max_paths are returned; truncated means another route was found.
    More than 8192 candidate extensions raises ContextLimitError instead of
    returning incomplete output. The output bound is not a process RSS limit;
    graph.schema() and the driver allocate their response before this validation.
    """
    source_label, target_label = identifier(source_label), identifier(target_label)
    for name, value, upper in (("max_hops", max_hops, 4), ("max_paths", max_paths, 20),
                               ("max_bytes", max_bytes, 1 << 20)):
        if type(value) is not int or not 1 <= value <= upper:
            raise ValueError(f"{name} must be an integer from 1 to {upper}")
    database = graph.database
    if not isinstance(database, str) or not database or len(database) > 4096 or \
            len(database.encode("utf-8")) > 4096:
        raise ValueError("graph name must be a nonempty string of at most 4096 UTF-8 bytes")
    snapshot = graph.schema()
    nodes, adjacency = _catalog(snapshot)
    result = {"graph": database, "schema_only": True, "catalog_version": snapshot["version"],
              "source_label": source_label, "target_label": target_label,
              "max_hops": max_hops, "max_paths": max_paths, "paths": [], "truncated": False}
    if source_label not in nodes or target_label not in nodes:
        _check_bytes(result, max_bytes)
        return result
    pending = deque([(source_label, (source_label,), ())])
    extensions = 0
    while pending:
        current, labels, relationships = pending.popleft()
        if current == target_label:
            if len(result["paths"]) == max_paths:
                result["truncated"] = True
                break
            result["paths"].append({"hops": len(relationships),
                                    "nodes": [nodes[label] for label in labels],
                                    "relationships": list(relationships)})
            continue
        if len(relationships) == max_hops:
            continue
        for relationship in adjacency[current]:
            extensions += 1
            if extensions > _MAX_EXPANSIONS:
                raise ContextLimitError("schema path expansion exceeds 8192 candidates")
            target = relationship["target"]
            pending.append((target, (*labels, target), (*relationships, relationship)))
    _check_bytes(result, max_bytes)
    return result
