"""Validate named metrics and query them over Bolt with the caller's access."""

import argparse
import json
import math
import os
from pathlib import Path
import stat

from brainwire import connect
from brainwire.client import identifier
from examples.python.connection import auth_from_env, bolt_connection_options


MAX_CATALOG_BYTES = 65536
MAX_GROUPS = 50
DIMENSION_TYPES = ("string", "integer", "number", "boolean")
AGGREGATIONS = ("count", "sum", "avg")


def validate_catalog(catalog):
    if (not isinstance(catalog, dict) or set(catalog) != {"version", "metrics"}
            or type(catalog["version"]) is not int or catalog["version"] != 1):
        raise ValueError("metrics catalog requires version 1 and a metrics object")
    metrics = catalog["metrics"]
    if not isinstance(metrics, dict) or not 1 <= len(metrics) <= 64:
        raise ValueError("declare 1 to 64 named metrics")
    for name, metric in metrics.items():
        identifier(name)
        if (not isinstance(metric, dict)
                or set(metric) - {"label", "aggregation", "property", "dimensions", "description"}):
            raise ValueError(f"unknown fields in metric {name}")
        identifier(metric.get("label"))
        aggregation = metric.get("aggregation")
        if aggregation not in AGGREGATIONS:
            raise ValueError(f"metric {name} requires count, sum or avg")
        if aggregation == "count":
            if "property" in metric:
                raise ValueError(f"count metric {name} counts rows and takes no property")
        else:
            identifier(metric.get("property"))
        dimensions = metric.get("dimensions", {})
        if not isinstance(dimensions, dict) or len(dimensions) > 16:
            raise ValueError(f"metric {name} dimensions must be an object with at most 16 properties")
        for dimension, kind in dimensions.items():
            identifier(dimension)
            if kind not in DIMENSION_TYPES:
                raise ValueError(f"metric {name} dimension {dimension} requires a scalar type")
        if "description" in metric:
            description = metric["description"]
            if (not isinstance(description, str) or len(description) > 1024
                    or len(description.encode("utf-8")) > 1024):
                raise ValueError(f"metric {name} description must be at most 1024 UTF-8 bytes")
    return metrics


def read_catalog(path):
    def unique_object(pairs):
        value = {}
        for key, item in pairs:
            if key in value:
                raise ValueError("duplicate key in metrics catalog")
            value[key] = item
        return value

    with os.fdopen(os.open(path, os.O_RDONLY | getattr(os, "O_NONBLOCK", 0)), "rb") as stream:
        if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
            raise ValueError("metrics catalog must be a regular file")
        raw = stream.read(MAX_CATALOG_BYTES + 1)
    if len(raw) > MAX_CATALOG_BYTES:
        raise ValueError("metrics catalog exceeds 64 KiB")
    try:
        catalog = json.loads(raw.decode("utf-8"), object_pairs_hook=unique_object)
    except RecursionError as error:
        raise ValueError("metrics catalog nesting exceeds JSON parser limit") from error
    validate_catalog(catalog)
    return catalog


def _number(value):
    return type(value) is int or (type(value) is float and math.isfinite(value))


def _dimension(value, kind):
    if value is None:
        return True
    if kind == "string":
        return (isinstance(value, str) and len(value) <= 4096
                and len(value.encode("utf-8")) <= 4096)
    if kind == "integer":
        return type(value) is int
    if kind == "number":
        return _number(value)
    return type(value) is bool


def run_metric(graph, catalog, name, *, group_by=None, max_groups=MAX_GROUPS):
    """Run a declared aggregate in one read against the caller's visible nodes.

    The separately observed schema version records validation provenance. The
    aggregate runs in its own snapshot and follows the server's query budgets.
    """
    metrics = validate_catalog(catalog)
    identifier(name)
    if name not in metrics:
        raise ValueError("unknown metric")
    if type(max_groups) is not int or not 1 <= max_groups <= MAX_GROUPS:
        raise ValueError(f"max_groups must be an integer from 1 to {MAX_GROUPS}")
    metric = metrics[name]
    dimensions = metric.get("dimensions", {})
    if group_by is not None and identifier(group_by) not in dimensions:
        raise ValueError("group-by property must be declared as a metric dimension")
    label, aggregation = metric["label"], metric["aggregation"]
    schema = graph.schema()
    declared = next((node["properties"] for node in schema["nodes"] if node["label"] == label), None)
    if declared is None:
        raise ValueError(f"metric {name} label {label} is absent from the graph schema")
    fields = set(dimensions)
    if aggregation != "count":
        fields.add(metric["property"])
    missing = fields.difference(declared)
    if missing:
        raise ValueError(f"metric {name} properties are absent from the graph schema: {', '.join(sorted(missing))}")

    # Names already passed the ASCII identifier check, including backtick exclusion.
    expression = "count(*)" if aggregation == "count" else f"{aggregation}(n.`{metric['property']}`)"
    query = f"MATCH (n:`{label}`) RETURN "
    if group_by is None:
        query += f"{expression} AS value"
        limit, parameters = 1, {}
    else:
        query += f"n.`{group_by}` AS dimension, {expression} AS value ORDER BY dimension LIMIT $limit"
        limit, parameters = max_groups, {"limit": max_groups + 1}
    rows = []
    with graph.stream_cypher(query, parameters, fetch_size=limit + 1) as stream:
        for record in stream:
            if len(rows) == limit:
                raise ValueError(f"metric result exceeds its row limit of {limit}")
            value = record["value"]
            if aggregation == "count":
                valid = type(value) is int and value >= 0
            else:
                valid = _number(value) or (aggregation == "avg" and value is None)
            if not valid:
                raise ValueError("invalid metric result value")
            if group_by is None:
                rows.append({"value": value})
            else:
                dimension = record["dimension"]
                if not _dimension(dimension, dimensions[group_by]):
                    raise ValueError("metric group does not match the declared scalar type or size")
                rows.append({"dimension": dimension, "value": value})
    if group_by is None and not rows:
        raise ValueError("ungrouped metric must return one row")
    return {"metric": name, "label": label, "aggregation": aggregation, "graph": graph.database,
            "model_version": catalog["version"], "validated_catalog_version": schema["version"],
            "group_by": group_by, "rows": rows}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--catalog", type=Path, default=Path(__file__).with_name("catalog.json"))
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("describe", help="show metric definitions without connecting")
    query = commands.add_parser("query", help="calculate a declared metric")
    query.add_argument("name")
    query.add_argument("--uri", default="bolt://127.0.0.1:7687")
    query.add_argument("--database", default="company_demo")
    query.add_argument("--tls-ca", help="verify Bolt TLS using this PEM CA file")
    query.add_argument("--group-by", help="one declared dimension property")
    query.add_argument("--max-groups", type=int, default=MAX_GROUPS)
    args = parser.parse_args(argv)
    try:
        catalog = read_catalog(args.catalog)
        if args.command == "describe":
            result = catalog
        else:
            uri, config = bolt_connection_options(args.uri, args.tls_ca)
            with connect(uri, args.database, auth=auth_from_env(), connection_timeout=5, **config) as graph:
                result = run_metric(graph, catalog, args.name, group_by=args.group_by, max_groups=args.max_groups)
    except (OSError, ValueError) as error:
        parser.exit(2, f"{error}\n")
    print(json.dumps(result, indent=2, ensure_ascii=False, allow_nan=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
