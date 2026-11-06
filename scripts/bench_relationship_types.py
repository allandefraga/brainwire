#!/usr/bin/env python3
"""Measure typed and unlabeled relationship counts on a disposable synthetic graph."""

import argparse
from contextlib import contextmanager
import hashlib
import json
import math
from pathlib import Path
import secrets
import signal
import statistics
import subprocess
import tempfile
import time
import uuid


def docker(*args, check=True, timeout=120):
    return subprocess.run(
        ["docker", *args], capture_output=True, text=True, check=check, timeout=timeout
    )


def bounded(low, high):
    def parse(value):
        value = int(value)
        if not low <= value <= high:
            raise argparse.ArgumentTypeError(f"must be between {low} and {high}")
        return value

    return parse


def deadline_exceeded(_signal, _frame):
    raise TimeoutError("benchmark deadline exceeded")


def interrupted(_signal, _frame):
    raise KeyboardInterrupt("benchmark interrupted")


@contextmanager
def connect(driver_factory, container_id, token, readiness_timeout):
    from neo4j.exceptions import ServiceUnavailable, SessionExpired

    port = docker("port", container_id, "7687/tcp").stdout.splitlines()[0].rsplit(":", 1)[1]
    with driver_factory(
        f"bolt://127.0.0.1:{port}", auth=("bench", token),
        connection_timeout=2, max_transaction_retry_time=0,
    ) as driver:
        deadline = time.monotonic() + readiness_timeout
        while True:
            try:
                driver.verify_connectivity()
                break
            except (ServiceUnavailable, SessionExpired) as error:
                if time.monotonic() >= deadline:
                    raise TimeoutError("server readiness deadline exceeded") from error
                time.sleep(.3)
        with driver.session(database="bench") as session:
            yield session


def count(session, statement, expected):
    actual = session.run(statement).single(strict=True)[0]
    if actual != expected:
        raise RuntimeError(f"{statement}: expected {expected}, got {actual}")
    return actual


def measure_counts(session, query_type, selected, args, report):
    for kind, source, target in selected:
        statements = {
            "typed": f"MATCH (:{source})-[r:{kind}]->(:{target}) RETURN count(r)",
            "untyped": f"MATCH ()-[r:{kind}]->() RETURN count(r)",
        }
        timings = {shape: [] for shape in statements}
        for sample in range(args.samples + 2):
            order = list(statements) if sample % 2 == 0 else reversed(statements)
            for shape in order:
                statement = statements[shape]
                if args.query_timeout:
                    statement = query_type(statement, timeout=args.query_timeout)
                start = time.monotonic()
                count(session, statement, args.rows)
                if sample >= 2:
                    timings[shape].append((time.monotonic() - start) * 1000)
        report["timings"][kind] = {
            shape: {
                "query": statements[shape], "count": args.rows,
                "p50_ms": statistics.median(values),
                "p95_ms": sorted(values)[math.ceil(.95 * len(values)) - 1],
                "samples_ms": values,
            }
            for shape, values in timings.items()
        }


def populate(session, query_type, labels, edges, args, report):
    def load(statements):
        started = time.monotonic()
        for statement in statements:
            if args.query_timeout:
                statement = query_type(statement, timeout=args.query_timeout)
            for lo in range(0, args.rows, args.batch):
                rows = list(range(lo, min(lo + args.batch, args.rows)))
                session.run(statement, rows=rows).consume()
        return time.monotonic() - started

    report["load_nodes_s"] = load(
        f"UNWIND $rows AS i CREATE (n:{label} {{entity_id: toString(i), ordinal: i}})"
        for label in labels
    )
    print(f"Loaded nodes in {report['load_nodes_s']:.2f}s", flush=True)
    report["load_edges_s"] = load(
        f"UNWIND $rows AS i MATCH (a:{source} {{entity_id: toString(i)}}), "
        f"(b:{target} {{entity_id: toString(i)}}) CREATE (a)-[:{kind} {{at: i}}]->(b)"
        for kind, source, target in edges
    )
    print(f"Loaded relationships in {report['load_edges_s']:.2f}s", flush=True)


def benchmark(args, report, directory, ownership):
    from neo4j import GraphDatabase, Query
    from neo4j.exceptions import Neo4jError

    run = report["run"]
    container = f"bw-relbench-{run}"
    volume = f"{container}-data"
    labels = [f"L{i:03d}" for i in range(args.labels)]
    edges = [(f"R{i:03d}_A", labels[i], labels[(i + 1) % args.labels]) for i in range(args.labels)]
    edges += [
        (f"R{i:03d}_B", labels[i], labels[(i + 2) % args.labels])
        for i in range(args.labels // 10)
    ]
    report["relationship_types"] = len(edges)
    report["expected_nodes"] = len(labels) * args.rows
    report["expected_relationships"] = len(edges) * args.rows

    image_id = docker("image", "inspect", "--format", "{{.Id}}", args.image).stdout.strip()
    report["image_id"] = image_id
    token = secrets.token_hex(32)
    policy = directory / "policy.json"
    policy.write_text(json.dumps({"principals": [{
        "id": "bench", "token_sha256": hashlib.sha256(token.encode()).hexdigest(),
        "graphs": {"bench": "write"}, "instance_admin": True,
    }]}))
    policy.chmod(0o444)
    schema = directory / "schema.json"
    schema.write_text(json.dumps({
        "version": "1",
        "nodes": [{"label": label, "properties": ["ordinal"]} for label in labels],
        "edges": [{"type": kind, "from": {"label": source}, "to": {"label": target},
                   "properties": ["at"]} for kind, source, target in edges],
    }))
    schema.chmod(0o444)
    if docker("volume", "inspect", volume, check=False).returncode == 0:
        raise RuntimeError("generated volume already exists")
    docker("volume", "create", "--label", ownership, volume)
    owned = docker("volume", "ls", "-q", "--filter", f"label={ownership}").stdout.splitlines()
    if volume not in owned:
        raise RuntimeError("generated volume has a different ownership label")
    memory = f"{args.memory}g"
    common = (
        "--pull", "never", "--label", ownership, "--memory", memory, "--memory-swap", memory,
        "--cpus", "2", "--mount", f"type=volume,src={volume},dst=/data",
        "-e", "BRAINWIRE_MEMORY_CACHE=256MiB", "-e", "BRAINWIRE_MEMORY_MEMTABLES=64MiB",
        "-e", "BRAINWIRE_MEMORY_QUERY=256MiB", "-e", "BRAINWIRE_SERVER_DEFAULT_DATABASE=bench",
    )
    docker(
        "run", "--rm", "--network", "none", *common,
        "--mount", f"type=bind,src={schema},dst=/ontology.json,readonly",
        image_id, "db", "create", "bench", "--ontology", "/ontology.json",
    )
    container_id = docker(
        "run", "-d", "--name", container, *common,
        "--mount", f"type=bind,src={policy},dst=/policy.json,readonly",
        "-e", "BRAINWIRE_SERVER_AUTH_FILE=/policy.json",
        "-p", "127.0.0.1::7687", image_id,
    ).stdout.strip()
    selected = [edges[i] for i in sorted({0, args.labels // 2, len(edges) - 1})]
    with connect(GraphDatabase.driver, container_id, token, 120) as session:
        populate(session, Query, labels, edges, args, report)
        count(session, "MATCH (n) RETURN count(n)", report["expected_nodes"])
        count(session, "MATCH ()-[r]->() RETURN count(r)", report["expected_relationships"])
        measure_counts(session, Query, selected, args, report)
        if args.deadline_probe:
            kind, _, _ = selected[len(selected) // 2]
            statement = Query(f"MATCH ()-[r:{kind}]->() RETURN count(r)", timeout=1)
            started = time.monotonic()
            try:
                result = {"status": "success", "count": count(session, statement, args.rows)}
            except Neo4jError as error:
                result = {"status": "server_error", "code": error.code}
            report["one_second_query_probe"] = {
                "type": kind, "timeout_s": 1, "wall_ms": (time.monotonic() - started) * 1000, **result
            }

    report["memory_cgroup_bytes"] = {
        key: int(docker("exec", container_id, "cat", f"/sys/fs/cgroup/memory.{key}").stdout)
        for key in ("current", "peak", "max")
    }
    events = docker("exec", container_id, "cat", "/sys/fs/cgroup/memory.events").stdout
    report["memory_events"] = {key: int(value) for key, value in map(str.split, events.splitlines())}
    if report["memory_events"].get("oom_kill") != 0:
        raise RuntimeError("benchmark container recorded an OOM kill")
    if args.restart:
        docker("restart", container_id, timeout=90)
        with connect(GraphDatabase.driver, container_id, token, 60) as session:
            report["restart_counts"] = {
                kind: count(session, Query(f"MATCH ()-[r:{kind}]->() RETURN count(r)", timeout=60), args.rows)
                for kind, _, _ in selected
            }


def cleanup(ownership):
    containers = docker("ps", "-aq", "--filter", f"label={ownership}").stdout.split()
    if containers:
        docker("rm", "-f", *containers)
    volumes = docker("volume", "ls", "-q", "--filter", f"label={ownership}").stdout.split()
    if volumes:
        docker("volume", "rm", *volumes)
    return (
        not docker("ps", "-aq", "--filter", f"label={ownership}").stdout.strip()
        and not docker("volume", "ls", "-q", "--filter", f"label={ownership}").stdout.strip()
    )


def main(args):
    run = uuid.uuid4().hex
    ownership = f"brainwire.benchmark.run={run}"
    report = {
        "result": "FAIL", "run": run, "image": args.image, "labels": args.labels,
        "rows_per_label": args.rows, "batch": args.batch, "memory_gib": args.memory,
        "cpus": 2, "samples": args.samples, "query_timeout_s": args.query_timeout,
        "deadline_s": args.deadline, "timings": {},
        "scope": "synthetic local Bolt counts using one warm sequential client on a shared host",
    }
    # Reserve the report before creating any Docker resources.
    with args.output.open("x", encoding="utf-8") as output:
        with tempfile.TemporaryDirectory(prefix="bw-relbench-", dir=args.output.resolve().parent) as tmp:
            signal.signal(signal.SIGALRM, deadline_exceeded)
            signal.signal(signal.SIGTERM, interrupted)
            signal.alarm(args.deadline)
            try:
                benchmark(args, report, Path(tmp), ownership)
                report["result"] = "PASS"
            except (Exception, KeyboardInterrupt) as error:
                report["error"] = type(error).__name__
                if isinstance(error, subprocess.CalledProcessError):
                    report["detail"] = error.stderr[-2000:]
                else:
                    report["detail"] = str(error)
                raise
            finally:
                signal.alarm(0)
                try:
                    report["cleanup"] = cleanup(ownership)
                except (OSError, subprocess.SubprocessError):
                    report["cleanup"] = False
                if not report["cleanup"]:
                    report["result"] = "FAIL"
                json.dump(report, output, indent=2)
                output.write("\n")
                print(f"{report['result']} {args.output}", flush=True)
    if not report["cleanup"]:
        raise RuntimeError(f"benchmark cleanup incomplete for label {ownership}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True, help="locally available Brainwire image")
    parser.add_argument("--output", required=True, type=Path, help="new JSON report path in a Docker-shared directory")
    parser.add_argument("--labels", type=bounded(2, 100), default=100)
    parser.add_argument("--rows", type=bounded(1, 20_000), default=20_000)
    parser.add_argument("--batch", type=bounded(1, 1000), default=500)
    parser.add_argument("--samples", type=bounded(1, 30), default=9)
    parser.add_argument("--memory", type=bounded(2, 8), default=8)
    parser.add_argument("--restart", action="store_true")
    parser.add_argument("--deadline-probe", action="store_true")
    parser.add_argument("--deadline", type=bounded(1, 900), default=900)
    parser.add_argument("--query-timeout", type=bounded(0, 120), default=60,
                        help="per-query timeout in seconds, 0 to use the server default")
    main(parser.parse_args())
