#!/usr/bin/env python3
"""Measure keyed MERGE on one synthetic high-degree pair in an owned container."""

import argparse
import hashlib
import json
from pathlib import Path
import secrets
import signal
import subprocess
import tempfile
import time
import uuid

try:
    from .bench_relationship_types import bounded, cleanup, connect, deadline_exceeded, docker, interrupted
except ImportError:
    from bench_relationship_types import bounded, cleanup, connect, deadline_exceeded, docker, interrupted


MERGE = (
    "UNWIND $rows AS row "
    "MATCH (a:Ticket {entity_id:'source'}), (b:Ticket {entity_id:'target'}) "
    "MERGE (a)-[r:EVENT {event_key:row.event_key}]->(b) "
    "SET r.ordinal=row.ordinal, r.payload=row.payload"
)
READ = (
    "MATCH (:Ticket {entity_id:'source'})-[r:EVENT]->(:Ticket {entity_id:'target'}) "
    "RETURN r.event_key AS event_key, r.ordinal AS ordinal, r.payload AS payload "
    "ORDER BY r.ordinal"
)
INDEX_NAME = "keyed_event_key"
INDEX_STATUS = (
    "CALL db.indexes() YIELD name, state, entityType "
    "WHERE name = $name RETURN state, entityType"
)


def batches(start, end, batch):
    return [(lo, min(lo + batch, end)) for lo in range(start, end, batch)]


def phases(rows):
    mid = rows // 2
    return [("initial_half", 0, mid, mid), ("replay_half", 0, mid, mid),
            ("append_half", mid, rows, rows), ("replay_all", 0, rows, rows)]


def payload(index, size):
    # Differing fixed-size values expose accidental payload reuse across keys.
    return chr(ord("a") + index % 26) * size


def write_rows(lo, hi, size):
    return [{"event_key": f"event-{i:08d}", "ordinal": i, "payload": payload(i, size)}
            for i in range(lo, hi)]


def schema(extra_labels):
    return {"version": "1", "nodes": [{"label": "Ticket"}] + [
        {"label": f"Unused{i:03d}"} for i in range(extra_labels)
    ], "edges": [{
        "type": "EVENT", "from": {"label": "Ticket"}, "to": {"label": "Ticket"},
        "properties": ["event_key", "ordinal", "payload"], "merge_key_fields": ["event_key"],
    }]}


def validate_rows(records, expected, size):
    digest = hashlib.sha256()
    count = 0
    for i, row in enumerate(records):
        wanted = {"event_key": f"event-{i:08d}", "ordinal": i, "payload": payload(i, size)}
        if i >= expected or dict(row) != wanted:
            raise AssertionError(f"relationship contents differ at ordinal {i}")
        digest.update(json.dumps(wanted, sort_keys=True, separators=(",", ":")).encode())
        digest.update(b"\n")
        count += 1
    if count != expected:
        raise AssertionError(f"relationship count {count}, expected {expected}")
    return {"count": count, "sha256": digest.hexdigest()}


def memory_sample(identifier):
    sample = {}
    for key in ("memory.current", "memory.peak", "memory.max", "memory.events"):
        value = docker("exec", identifier, "cat", f"/sys/fs/cgroup/{key}").stdout.strip()
        sample[key] = ({k: int(v) for k, v in (line.split() for line in value.splitlines())}
                       if key == "memory.events" else int(value))
    for line in docker("exec", identifier, "cat", "/proc/1/status").stdout.splitlines():
        if line.startswith(("VmRSS:", "VmHWM:")):
            key, value, unit = line.split()
            if unit != "kB":
                raise ValueError("unknown process-memory unit")
            sample[key.rstrip(":") + "_bytes"] = int(value) * 1024
    if sample["memory.events"].get("oom", 0) or sample["memory.events"].get("oom_kill", 0):
        raise AssertionError("cgroup reported an OOM event")
    return sample


def validate_args(args):
    if args.rows * args.payload_bytes > 128 * 1024 * 1024:
        raise ValueError("total synthetic payload must not exceed 128 MiB")
    if args.batch * (args.payload_bytes + 128) > 4 * 1024 * 1024:
        raise ValueError("one input batch must not exceed 4 MiB of estimated payload")


def prepare_index(session, timeout, query_type):
    began = time.monotonic()
    deadline = began + timeout

    def run(statement, **params):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("keyed relationship index readiness deadline exceeded")
        return session.run(query_type(statement, timeout=remaining), **params)

    run(f"CREATE INDEX {INDEX_NAME} FOR ()-[r:EVENT]-() ON (r.event_key)").consume()
    while True:
        states = run(INDEX_STATUS, name=INDEX_NAME).data()
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("keyed relationship index readiness deadline exceeded")
        if states == [{"state": "scalar_range_ready", "entityType": "relationship"}]:
            return time.monotonic() - began
        if states != [{"state": "populating", "entityType": "relationship"}]:
            raise RuntimeError(f"unexpected keyed relationship index state: {states}")
        time.sleep(min(.05, remaining))


def exercise(args, report, directory, ownership):
    from neo4j import GraphDatabase, Query

    token = secrets.token_hex(32)
    policy = directory / "policy.json"
    policy.write_text(json.dumps({"principals": [{
        "id": "bench", "token_sha256": hashlib.sha256(token.encode()).hexdigest(),
        "graphs": {"bench": "write"}, "instance_admin": True,
    }]}))
    policy.chmod(0o444)
    ontology = directory / "schema.json"
    ontology_json = json.dumps(schema(args.extra_labels))
    ontology.write_text(ontology_json)
    report["ontology"] = {"declared_node_types": 1 + args.extra_labels,
                          "declared_relationship_types": 1,
                          "unused_node_types": args.extra_labels,
                          "json_bytes": len(ontology_json.encode("utf-8")),
                          "scope": "input ontology JSON size, not persisted catalog allocation"}
    ontology.chmod(0o444)
    image = docker("image", "inspect", "--format", "{{.Id}}", args.image).stdout.strip()
    report["image_id"] = image
    name = f"bw-keyedhub-{report['run']}"
    volume = f"{name}-data"
    if docker("volume", "inspect", volume, check=False).returncode == 0:
        raise RuntimeError("generated volume already exists")
    docker("volume", "create", "--label", ownership, volume)
    if volume not in docker("volume", "ls", "-q", "--filter", f"label={ownership}").stdout.splitlines():
        raise RuntimeError("generated volume has a different ownership label")
    common = (
        "--pull", "never", "--label", ownership, "--memory", f"{args.memory}g",
        "--memory-swap", f"{args.memory}g", "--cpus", "2",
        "--mount", f"type=volume,src={volume},dst=/data",
        "-e", "BRAINWIRE_SERVER_DEFAULT_DATABASE=bench",
        "-e", "BRAINWIRE_MEMORY_CACHE=256MiB", "-e", "BRAINWIRE_MEMORY_MEMTABLES=64MiB",
        "-e", f"BRAINWIRE_MEMORY_QUERY={args.query_mib}MiB",
    )
    if args.indexed:
        common += ("-e", "BRAINWIRE_SERVER_RELATIONSHIP_INDEXES=true")
    docker("run", "--rm", "--network", "none", *common,
           "--mount", f"type=bind,src={ontology},dst=/ontology.json,readonly",
           image, "db", "create", "bench", "--ontology", "/ontology.json")
    identifier = docker("run", "-d", "--name", name, *common,
                        "--mount", f"type=bind,src={policy},dst=/policy.json,readonly",
                        "-e", "BRAINWIRE_SERVER_AUTH_FILE=/policy.json",
                        "-p", "127.0.0.1::7687", image).stdout.strip()
    with connect(GraphDatabase.driver, identifier, token, 60) as session:
        session.run("CREATE (:Ticket {entity_id:'source'}), (:Ticket {entity_id:'target'})").consume()
        if args.indexed:
            report["active_phase"] = "index_setup"
            report["index_ready_seconds"] = prepare_index(session, 60, Query)
        report["memory_initial"] = memory_sample(identifier)
        for phase, lo, hi, expected in phases(args.rows):
            measured = {"name": phase, "rows": hi - lo, "batches": []}
            report["phases"].append(measured)
            report["active_phase"] = phase
            began = time.monotonic()
            for start, end in batches(lo, hi, args.batch):
                started = time.monotonic()
                session.run(Query(MERGE, timeout=args.query_timeout),
                            rows=write_rows(start, end, args.payload_bytes)).consume()
                elapsed = time.monotonic() - started
                measured["batches"].append({"start": start, "end": end, "seconds": elapsed})
            measured["seconds"] = time.monotonic() - began
            report["active_phase"] = phase + "_verification"
            measured["verification"] = validate_rows(
                session.run(Query(READ, timeout=args.query_timeout)), expected, args.payload_bytes)
            measured["memory"] = memory_sample(identifier)
            print(f"{phase}: {measured['seconds']:.3f}s, {expected} verified relationships", flush=True)
        before = report["phases"][-1]["verification"]
    report["active_phase"] = "restart"
    docker("kill", identifier)
    docker("start", identifier)
    with connect(GraphDatabase.driver, identifier, token, 60) as session:
        after = validate_rows(session.run(Query(READ, timeout=args.query_timeout)), args.rows, args.payload_bytes)
        if after != before:
            raise AssertionError("relationship contents changed after restart")
        report["restart_verification"] = after
        report["memory_after_restart"] = memory_sample(identifier)
    docker("stop", "--time", "30", identifier)
    report["active_phase"] = "offline_integrity"
    report["integrity"] = json.loads(docker(
        "run", "--rm", "--network", "none", *common,
        image, "db", "check", "bench", "--json").stdout)
    if report["integrity"].get("status") != "ok":
        raise AssertionError("offline integrity check did not pass")
    report.pop("active_phase", None)


def main(args):
    validate_args(args)
    run = uuid.uuid4().hex
    ownership = f"brainwire.benchmark.run={run}"
    report = {"result": "FAIL", "run": run, "image": args.image, "rows": args.rows,
              "payload_bytes": args.payload_bytes, "batch": args.batch, "memory_gib": args.memory,
              "query_mib": args.query_mib, "cpus": 2, "query_timeout_s": args.query_timeout,
              "relationship_index_enabled": args.indexed,
              "extra_labels": args.extra_labels,
              "phases": [], "scope": "synthetic keyed MERGE on one pair in one local standalone container",
              "memory_scope": "external phase samples and cgroup peak; no process RSS ceiling or continuous RSS measurement",
              "limits": ["not a production SLO", "not sustained memory qualification", "not distributed recovery",
                         "verification scans and Docker observers affect timing and memory"]}
    with args.output.open("x", encoding="utf-8") as output:
        with tempfile.TemporaryDirectory(prefix="bw-keyedhub-", dir=args.output.resolve().parent) as tmp:
            signal.signal(signal.SIGALRM, deadline_exceeded)
            signal.signal(signal.SIGTERM, interrupted)
            signal.alarm(args.deadline)
            try:
                exercise(args, report, Path(tmp), ownership)
                report["result"] = "PASS"
            except (Exception, KeyboardInterrupt) as error:
                report["error"] = type(error).__name__
                report["detail"] = error.stderr[-1000:] if isinstance(error, subprocess.CalledProcessError) else str(error)
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
        raise RuntimeError(f"cleanup incomplete for {ownership}")


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--image", required=True, help="already available local Docker image")
    result.add_argument("--output", type=Path, required=True, help="new report in a Docker-shared directory")
    result.add_argument("--rows", type=bounded(2, 20_000), default=2000)
    result.add_argument("--extra-labels", type=bounded(0, 100), default=0,
                        help="declare unused node types to vary catalog size without adding data")
    result.add_argument("--payload-bytes", type=bounded(0, 65_536), default=4096)
    result.add_argument("--batch", type=bounded(1, 500), default=100)
    result.add_argument("--memory", type=bounded(2, 8), default=3)
    result.add_argument("--query-mib", type=bounded(16, 256), default=256)
    result.add_argument("--indexed", action="store_true",
                        help="opt in to the standalone relationship-index format and build event_key index")
    result.add_argument("--query-timeout", type=bounded(1, 120), default=60)
    result.add_argument("--deadline", type=bounded(1, 1800), default=900)
    return result


if __name__ == "__main__":
    main(parser().parse_args())
