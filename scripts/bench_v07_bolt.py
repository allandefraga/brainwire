"""Measure delegated read queries and 200-row cursors on a frozen synthetic graph.

This helper never creates data, writes to a graph, starts a server, or retries
transactions. The manifest and environment must explicitly identify a disposable
synthetic fixture. Install the repository's pinned Neo4j driver before execution.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import statistics
import subprocess
import time
from urllib.parse import urlsplit

PREFIX = "BRAINWIRE_BENCH_"
IDENTIFIER = re.compile(r"[A-Za-z_][A-Za-z0-9_]{0,127}\Z")
SCHEMA_QUERY = "CALL db.schema() YIELD version RETURN version"
GAPS = [
    "This is a sequential Bolt read diagnostic on a caller-declared frozen synthetic fixture.",
    "It does not run concurrent ingestion, write throughput, or the requested complete load/read workload.",
    "It neither measures server RSS nor enforces a process/cgroup memory limit.",
    "Server binary identity is caller-supplied; the remote process artifact is not independently verified.",
    "Each cursor page is a separate read transaction, not a single cross-page snapshot.",
    "Query latency includes the Python driver, connection use, Bolt transfer and transaction commit.",
]


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def integer(value, name, minimum=0, maximum=None):
    if type(value) is not int or value < minimum or (maximum is not None and value > maximum):
        raise ValueError(f"{name} must be an integer in the supported range")
    return value


def required_text(value, name):
    if not isinstance(value, str) or not value or len(value) > 1024:
        raise ValueError(f"{name} must be a nonempty string of at most 1024 characters")
    return value


def read_manifest(path):
    raw = Path(path).read_bytes()
    manifest = json.loads(raw)
    if not isinstance(manifest, dict) or any(manifest.get(k) is not True for k in ("synthetic", "disposable", "frozen")):
        raise ValueError("manifest must declare synthetic, disposable and frozen as true")
    for field in ("dataset_id", "server_id", "database"):
        required_text(manifest.get(field), field)
    integer(manifest.get("schema_version"), "schema_version", 0, (1 << 32) - 1)
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]{0,63}", manifest["database"]):
        raise ValueError("database must be a supported named graph identifier")
    for field, length in (("source_revision", 40), ("server_binary_sha256", 64)):
        if not isinstance(manifest.get(field), str) or not re.fullmatch(rf"[0-9a-f]{{{length}}}", manifest[field]):
            raise ValueError(f"{field} must be a complete lowercase hexadecimal identity")
    schema = manifest.get("schema")
    if not isinstance(schema, dict):
        raise ValueError("manifest schema is required")
    for field in ("source_label", "target_label", "relationship_type", "group_property", "cursor_label"):
        if not isinstance(schema.get(field), str) or not IDENTIFIER.fullmatch(schema[field]):
            raise ValueError(f"schema.{field} must be a supported ASCII identifier")
    integer(manifest.get("top_n", 5), "top_n", 1, 100)
    expected = manifest.get("expected")
    if not isinstance(expected, dict):
        raise ValueError("expected results are required for both visibility modes")
    for mode in ("allow_all", "restricted"):
        values = expected.get(mode)
        if not isinstance(values, dict):
            raise ValueError(f"expected.{mode} is required")
        integer(values.get("count_pairs"), f"expected.{mode}.count_pairs")
        integer(values.get("cursor_rows"), f"expected.{mode}.cursor_rows")
        validate_top(values.get("top_n"), manifest.get("top_n", 5))
    manifest["manifest_sha256"] = hashlib.sha256(raw).hexdigest()
    return manifest


def validate_top(rows, top_n):
    if not isinstance(rows, list) or len(rows) > top_n:
        raise ValueError("top-N results must be a list within top_n")
    for row in rows:
        if not isinstance(row, dict) or set(row) != {"group_value", "count"} or not isinstance(row["group_value"], str):
            raise ValueError("top-N rows require a string group_value and an integer count")
        integer(row["count"], "top-N count")
    if rows != sorted(rows, key=lambda row: (-row["count"], row["group_value"])):
        raise ValueError("top-N rows must be ordered by descending count, then group_value")


def configuration(manifest, env):
    required = ("URI", "PRINCIPAL", "TOKEN", "RESTRICTED_VISIBILITY", "CONFIRM_SYNTHETIC")
    if any(not env.get(PREFIX + key) for key in required):
        raise ValueError("required benchmark environment is incomplete; see bench_v07_bolt.md")
    if env[PREFIX + "CONFIRM_SYNTHETIC"] != manifest["dataset_id"]:
        raise ValueError("synthetic confirmation must match manifest dataset_id")
    uri = env[PREFIX + "URI"]
    parsed = urlsplit(uri)
    if (parsed.scheme not in ("bolt", "bolt+s") or not parsed.hostname or parsed.username is not None
            or parsed.password is not None or parsed.path not in ("", "/") or parsed.query or parsed.fragment):
        raise ValueError("URI must be a direct bolt:// or bolt+s:// address without embedded credentials")
    try:
        port = parsed.port
    except ValueError as error:
        raise ValueError("URI has an invalid port") from error
    if port is None:
        raise ValueError("URI requires an explicit port")
    policy = json.loads(env[PREFIX + "RESTRICTED_VISIBILITY"])
    if (not isinstance(policy, dict) or policy.get("allow_all", False) is not False
            or policy.get("deny_all", False) is not False or not isinstance(policy.get("principals"), list)
            or not 1 <= len(policy["principals"]) <= 5000
            or not all(isinstance(x, str) and 1 <= len(x.encode()) <= 256 for x in policy["principals"])):
        raise ValueError("restricted visibility must have nonempty principals and no allow_all/deny_all override")
    return {"uri": uri, "auth": (env[PREFIX + "PRINCIPAL"], env[PREFIX + "TOKEN"]),
            "policies": {"allow_all": {"allow_all": True}, "restricted": policy}}


def queries(manifest, page_size):
    s = manifest["schema"]
    pattern = f"MATCH (a:{s['source_label']})-[:{s['relationship_type']}]->(b:{s['target_label']}) "
    return {
        "count_pairs": pattern + "RETURN count(*) AS count",
        "top_n": pattern + f"RETURN b.{s['group_property']} AS group_value, count(*) AS count "
                 + f"ORDER BY count DESC, group_value LIMIT {manifest.get('top_n', 5)}",
        "cursor": f"MATCH (n:{s['cursor_label']}) WHERE n.entity_id > $after "
                  + f"RETURN n.entity_id AS id ORDER BY n.entity_id LIMIT {page_size}",
    }


def host_receipt():
    result = {"system": platform.system(), "machine": platform.machine(),
              "load_average": list(os.getloadavg()) if hasattr(os, "getloadavg") else None,
              "power_qualified": False}
    if result["system"] == "Darwin":
        try:
            power = subprocess.run(["pmset", "-g", "batt"], capture_output=True, text=True, timeout=5)
            settings = subprocess.run(["pmset", "-g"], capture_output=True, text=True, timeout=5)
            modes = [parts[1] for line in settings.stdout.splitlines()
                     if len(parts := line.split()) == 2 and parts[0] == "lowpowermode"]
            result["power_qualified"] = (power.returncode == settings.returncode == 0
                                         and "'AC Power'" in power.stdout and modes == ["0"])
            result["power_source"] = power.stdout.strip()
            result["power_settings"] = settings.stdout.strip()
        except (OSError, subprocess.TimeoutExpired):
            result["power_error"] = "power state unavailable"
    return result


def fetch(session, query, params, policy, timeout):
    started = time.perf_counter()
    # No managed transaction retry: every timing represents one attempt.
    with session.begin_transaction(metadata={"brainwire_visibility": policy}, timeout=timeout) as tx:
        rows = tx.run(query, params).data()
        tx.commit()
    return time.perf_counter() - started, rows


def measured_query(session, query, policy, expected, repeats, timeout, name):
    def check(rows):
        if rows != expected:
            raise ValueError(f"{name} result differs from declared synthetic fixture")
    warmup, rows = fetch(session, query, {}, policy, timeout)
    check(rows)
    timings = []
    for _ in range(repeats):
        elapsed, rows = fetch(session, query, {}, policy, timeout)
        check(rows)
        timings.append(elapsed)
    return {"query": query, "parameters": {}, "rows": rows, "warmup_seconds": warmup,
            "sample_seconds": timings, "median_seconds": statistics.median(timings),
            "correctness": "matched declared fixture"}


def cursor_walk(session, query, policy, expected_rows, page_size, timeout, max_seconds, max_pages,
                result=None, on_page=None):
    after, seen, pages = "", 0, []
    started = time.perf_counter()
    result = {} if result is None else result
    result.update(query=query, page_size=page_size, pages=pages, rows_seen=0,
                  expected_rows=expected_rows, complete=False, status="failed",
                  query_seconds=0.0, page_journal_seconds=0.0,
                  seconds_scope="wall clock including page journaling and validation")
    try:
        while len(pages) < max_pages and time.perf_counter() - started < max_seconds:
            elapsed, rows = fetch(session, query, {"after": after}, policy, timeout)
            result["query_seconds"] += elapsed
            if len(rows) > page_size:
                raise ValueError("cursor returned more rows than its limit")
            ids = []
            for row in rows:
                if not isinstance(row, dict) or set(row) != {"id"} or not isinstance(row["id"], str) or not row["id"]:
                    raise ValueError("cursor returned an invalid entity ID row")
                ids.append(row["id"])
            if any(value <= previous for previous, value in zip([after] + ids, ids)):
                raise ValueError("cursor IDs are repeated or not strictly increasing")
            page = {"page": len(pages) + 1, "after": after, "rows": len(ids),
                    "last_id": ids[-1] if ids else None, "seconds": elapsed}
            pages.append(page)
            seen += len(ids)
            result["rows_seen"] = seen
            if on_page is not None:
                journal_started = time.perf_counter()
                on_page(page)
                result["page_journal_seconds"] += time.perf_counter() - journal_started
            if seen > expected_rows:
                raise ValueError("cursor exceeded declared fixture cardinality")
            if not ids:
                result["complete"] = seen == expected_rows
                break
            after = ids[-1]
        result["status"] = "ok" if result["complete"] else "incomplete"
        return result
    finally:
        result["seconds"] = time.perf_counter() - started


def save_receipt(path, receipt):
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(receipt, indent=2) + "\n")
    temporary.replace(path)


def run(args, driver_factory=None, env=None, host_reader=host_receipt):
    manifest = read_manifest(args.manifest)
    config = configuration(manifest, os.environ if env is None else env)
    host = host_reader()
    if not host["power_qualified"] and not args.allow_unqualified_host:
        raise ValueError("AC power/low-power-off was not verified; --allow-unqualified-host records an unqualified diagnostic")
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    query = queries(manifest, args.page_size)
    receipt = {"format_version": 1, "kind": "synthetic delegated Bolt reads", "status": "running",
               "manifest_sha256": manifest["manifest_sha256"], "dataset_id": manifest["dataset_id"],
               "server": {"id": manifest["server_id"], "uri": config["uri"], "database": manifest["database"],
                          "source_revision": manifest["source_revision"], "binary_sha256": manifest["server_binary_sha256"],
                          "identity_provenance": "caller supplied; schema and declared result sets checked remotely"},
               "host_before": host, "qualification_gaps": GAPS, "modes": {}}
    path = output / "receipt.json"
    save_receipt(path, receipt)
    try:
        if driver_factory is None:
            from neo4j import GraphDatabase
            driver_factory = GraphDatabase.driver
        with driver_factory(config["uri"], auth=config["auth"], max_transaction_retry_time=0,
                            connection_timeout=min(args.query_timeout, 10), max_connection_pool_size=1) as driver:
            for mode, policy in config["policies"].items():
                mode_result = {"visibility_sha256": digest(policy), "status": "running"}
                receipt["modes"][mode] = mode_result
                with driver.session(database=manifest["database"], default_access_mode="READ") as session:
                    elapsed, schema = fetch(session, SCHEMA_QUERY, {}, policy, args.query_timeout)
                    if (len(schema) != 1 or not isinstance(schema[0], dict)
                            or set(schema[0]) != {"version"} or type(schema[0]["version"]) is not int
                            or schema[0]["version"] != manifest["schema_version"]):
                        raise ValueError("remote schema version differs from manifest")
                    mode_result["schema"] = {"query": SCHEMA_QUERY, "rows": schema, "seconds": elapsed}
                    expected = manifest["expected"][mode]
                    mode_result["count_pairs"] = measured_query(session, query["count_pairs"], policy,
                        [{"count": expected["count_pairs"]}], args.repeats, args.query_timeout, "count_pairs")
                    save_receipt(path, receipt)
                    mode_result["top_n"] = measured_query(session, query["top_n"], policy,
                        expected["top_n"], args.repeats, args.query_timeout, "top_n")
                    save_receipt(path, receipt)
                    page_file = mode + "-cursor.jsonl"
                    mode_result["cursor"] = {"page_file": page_file}
                    save_receipt(path, receipt)
                    with (output / page_file).open("x") as page_log:
                        def log_page(page):
                            page_log.write(json.dumps(page) + "\n")
                            page_log.flush()
                        cursor_walk(session, query["cursor"], policy, expected["cursor_rows"],
                            args.page_size, args.query_timeout, args.cursor_seconds, args.max_cursor_pages,
                            mode_result["cursor"], log_page)
                    mode_result["status"] = "ok" if mode_result["cursor"]["complete"] else "incomplete"
                    save_receipt(path, receipt)
        receipt["status"] = ("completed_read_diagnostic" if all(mode["status"] == "ok"
                             for mode in receipt["modes"].values()) else "incomplete")
    except BaseException as error:
        receipt["status"] = "interrupted" if isinstance(error, KeyboardInterrupt) else "failed"
        # Driver error messages can contain request details; preserve type/code only.
        receipt["error"] = {"type": type(error).__name__, "code": getattr(error, "code", None)}
        raise
    finally:
        receipt["host_after"] = host_reader()
        save_receipt(path, receipt)
    print(path)
    return 0 if receipt["status"] == "completed_read_diagnostic" else 1


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--manifest", type=Path, required=True)
    result.add_argument("--output", type=Path, required=True, help="new output directory; reuse is refused")
    result.add_argument("--page-size", type=int, default=200)
    result.add_argument("--repeats", type=int, default=3)
    result.add_argument("--query-timeout", type=float, default=60)
    result.add_argument("--cursor-seconds", type=float, default=60)
    result.add_argument("--max-cursor-pages", type=int, default=10000)
    result.add_argument("--allow-unqualified-host", action="store_true")
    return result


def main():
    args = parser().parse_args()
    if (not 1 <= args.page_size <= 10000 or not 1 <= args.repeats <= 20
            or not 0 < args.query_timeout <= 600 or args.cursor_seconds <= 0 or args.max_cursor_pages <= 0):
        raise SystemExit("invalid page, repeat, timeout or cursor limit")
    try:
        return run(args)
    except Exception as error:
        raise SystemExit(type(error).__name__ + "; inspect the receipt and benchmark setup") from None


if __name__ == "__main__":
    raise SystemExit(main())
