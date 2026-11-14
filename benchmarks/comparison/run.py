#!/usr/bin/env python3
"""Compare indexed graph operations on deterministic synthetic data."""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import importlib.metadata
from itertools import islice
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import time
import uuid

# Reuse the historical harness' pinned-image resolution and ownership checks.
ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "benchmarks/archive/comparison"))
from run import (  # noqa: E402
    LABEL, cleanup_owned, container_logs, docker, falkor_configuration, inspect_owned,
    port, redis_ready, resolve_image, summary, wait_ready,
)
from falkordb import FalkorDB
from neo4j import GraphDatabase, Query
from redis.retry import Retry
from redis.backoff import NoBackoff

FALKOR_IMAGE = "falkordb/falkordb-server:6.0.0"
NEO4J_IMAGE = "neo4j:2026.09.0"
NODE_CREATE = "UNWIND $rows AS row CREATE (n:Item {entity_id: row.key, value: row.value}) RETURN count(n)"
EDGE_CREATE = "UNWIND $rows AS row MATCH (a:Item {entity_id: row.src}) MATCH (b:Item {entity_id: row.dst}) CREATE (a)-[:LINK]->(b) RETURN count(*)"
UPDATE = "UNWIND $rows AS row MATCH (n:Item {entity_id: row.key}) SET n.score = row.score RETURN count(n)"
POINT = "MATCH (n:Item {entity_id: $key}) RETURN n.value"
HOP = "MATCH (n:Item {entity_id: $key})-[:LINK]->(m:Item) RETURN m.entity_id"
SCORE = "MATCH (n:Item {entity_id: $key}) RETURN n.score"
COUNT_NODES = "MATCH (n:Item) RETURN count(n)"
COUNT_EDGES = "MATCH (:Item)-[r:LINK]->(:Item) RETURN count(r)"
SKIPS = (1, 17)
DURABILITY = {
    "brainwire": "default single-node RocksDB WAL, synced on commit",
    "neo4j": "default single-node Neo4j transaction-log settings",
    "falkordb": "Redis AOF enabled with appendfsync always; verified before and after recovery",
}


class Client:
    def __init__(self, system, published_port):
        self.driver = self.session = self.redis = self.graph = None
        if system == "falkordb":
            client = FalkorDB(host="127.0.0.1", port=published_port, socket_timeout=60,
                              retry=Retry(NoBackoff(), 0))
            self.redis = client.connection
            self.graph = client.select_graph("benchmark")
        else:
            auth = ("neo4j", "benchmark-only") if system == "neo4j" else None
            self.driver = GraphDatabase.driver(f"bolt://127.0.0.1:{published_port}", auth=auth,
                                               connection_timeout=5, max_transaction_retry_time=0)
            self.session = self.driver.session(database="neo4j" if auth else "benchmark")

    def run(self, query, **params):
        if self.graph is not None:
            return self.graph.query(query, params).result_set
        return list(self.session.run(Query(query, timeout=60), **params))

    def close(self):
        if self.redis is not None:
            self.redis.close()
        if self.driver is not None:
            self.driver.close()


def batches(rows, size):
    source = iter(rows)
    while batch := list(islice(source, size)):
        yield batch


def key(i):
    return f"n{i:07d}"


def value(i):
    return f"v{i % 1000:04d}"


def node_rows(count):
    return ({"key": key(i), "value": value(i)} for i in range(count))


def edge_rows(count):
    return ({"src": key(i), "dst": key((i + skip) % count)}
            for i in range(count) for skip in SKIPS)


def column(rows):
    return [row[0] for row in rows]


def expect(actual, expected, label):
    if actual != expected:
        raise RuntimeError(f"{label}: expected {expected!r}, got {actual!r}")


def write_batches(client, query, rows, batch_size):
    total = 0
    start = time.perf_counter()
    for batch in batches(rows, batch_size):
        expect(column(client.run(query, rows=batch)), [len(batch)], "batch row count")
        total += len(batch)
    elapsed = time.perf_counter() - start
    return {"records": total, "seconds": elapsed, "records_per_second": total / elapsed}


def check_reads(client, i, count, point_ms=None, hop_ms=None):
    start = time.perf_counter_ns()
    point = client.run(POINT, key=key(i))
    middle = time.perf_counter_ns()
    neighbors = client.run(HOP, key=key(i))
    end = time.perf_counter_ns()
    expect(column(point), [value(i)], f"point read {key(i)}")
    expect(sorted(column(neighbors)), sorted(key((i + skip) % count) for skip in SKIPS),
           f"one hop {key(i)}")
    if point_ms is not None:
        point_ms.append((middle - start) / 1e6)
        hop_ms.append((end - middle) / 1e6)


def check_counts(client, count):
    expect(column(client.run(COUNT_NODES)), [count], "node count")
    expect(column(client.run(COUNT_EDGES)), [count * len(SKIPS)], "edge count")


def attach(system, identity, expected_maxmemory):
    published_port = port(identity, "6379/tcp" if system == "falkordb" else "7687/tcp")
    config = None
    if system == "falkordb":
        wait_ready(lambda: redis_ready(published_port), system)
        config = falkor_configuration(published_port, expected_maxmemory)
        # Redis can enumerate identical modules in a different order after restart.
        config["modules"].sort(key=lambda module: module["name"])
    client = Client(system, published_port)
    try:
        wait_ready(lambda: expect(column(client.run("RETURN 1")), [1], "readiness"), system)
    except BaseException:
        client.close()
        raise
    return client, config


def run_system(system, args, image, run_id):
    name = f"bw-compare-{run_id}-{system}"
    volume = f"{name}-data"
    init = f"{name}-init"
    persisted = "/var/lib/falkordb/data" if system == "falkordb" else "/data"
    memory = args.memory_mib
    maxmemory = memory * 3 // 4
    limits = ["--memory", f"{memory}m", "--memory-swap", f"{memory}m", "--cpus", str(args.cpus)]
    volume_mount = ["-v", f"{volume}:{persisted}"]
    label = ["--label", f"{LABEL}={run_id}"]
    client = None
    identity = None
    try:
        docker("volume", "create", *label, volume)
        inspect_owned("volume", volume, run_id)
        if system == "brainwire":
            ontology = json.dumps({
                "version": "1",
                "nodes": [{"label": "Item", "properties": ["entity_id", "value", "score"],
                           "indexed_fields": ["entity_id"]}],
                "edges": [{"type": "LINK", "from": {"label": "Item"}, "to": {"label": "Item"}}],
            })
            docker("run", "--pull=never", "--rm", "--name", init, *label, *limits, *volume_mount,
                   "--entrypoint", "sh", image["id"], "-ec",
                   'printf "%s\\n" "$1" > /tmp/ontology.json; '
                   'exec brainwire db create benchmark --ontology /tmp/ontology.json', "fixture", ontology)
            options = ["-p", "127.0.0.1::7687", "-e", "BRAINWIRE_SERVER_DATA_DIR=/data",
                       "-e", "BRAINWIRE_SERVER_DEFAULT_DATABASE=benchmark",
                       "-e", "BRAINWIRE_SERVER_LISTEN=0.0.0.0:7687",
                       "-e", f"BRAINWIRE_MEMORY_CACHE={memory // 4}MiB",
                       "-e", f"BRAINWIRE_MEMORY_MEMTABLES={memory // 16}MiB",
                       "-e", f"BRAINWIRE_MEMORY_QUERY={memory // 32}MiB", "--entrypoint", "brainwire"]
            command = ["serve", "--allow-unauthenticated-remote"]
        elif system == "neo4j":
            options = ["-p", "127.0.0.1::7687", "-e", "NEO4J_AUTH=neo4j/benchmark-only",
                       "-e", f"NEO4J_server_memory_heap_initial__size={memory // 4}M",
                       "-e", f"NEO4J_server_memory_heap_max__size={memory // 4}M",
                       "-e", f"NEO4J_server_memory_pagecache_size={memory * 3 // 8}M"]
            command = []
        else:
            options = ["-p", "127.0.0.1::6379", "-e",
                       f"REDIS_ARGS=--appendonly yes --appendfsync always --maxmemory {maxmemory}mb",
                       "-e", f"FALKORDB_ARGS=THREAD_COUNT {args.cpus}"]
            command = []

        identity = docker("run", "--pull=never", "-d", "--name", name, *label, *limits,
                          *volume_mount, *options, image["id"], *command)
        state = inspect_owned("container", identity, run_id)
        expect(state["Image"], image["id"], "container image")
        expected_maxmemory = maxmemory * 1024 * 1024
        client, settings = attach(system, identity, expected_maxmemory)
        if system == "neo4j":
            client.run("CREATE INDEX item_entity_id_idx FOR (n:Item) ON (n.entity_id)")
            client.run("CALL db.awaitIndexes(60)")
        elif system == "falkordb":
            client.run("CREATE INDEX FOR (n:Item) ON (n.entity_id)")

        print(f"[{system}] creating {args.nodes:,} nodes", flush=True)
        nodes = write_batches(client, NODE_CREATE, node_rows(args.nodes), args.batch)
        print(f"[{system}] creating {args.nodes * len(SKIPS):,} edges", flush=True)
        edges = write_batches(client, EDGE_CREATE, edge_rows(args.nodes), args.batch)
        check_counts(client, args.nodes)
        for i in range(10):
            check_reads(client, i, args.nodes)
        point_ms, hop_ms = [], []
        for i in range(args.reads):
            check_reads(client, i * 7919 % args.nodes, args.nodes, point_ms, hop_ms)
        update_count = min(args.batch * args.update_batches, args.nodes)
        updates = write_batches(client, UPDATE,
                                ({"key": key(i), "score": i} for i in range(update_count)), args.batch)

        # Crash the database process immediately after acknowledged writes on its persistent volume.
        client.close()
        client = None
        print(f"[{system}] SIGKILL and recover", flush=True)
        inspect_owned("container", identity, run_id)
        start = time.perf_counter()
        docker("kill", "--signal=KILL", identity)
        docker("start", identity)
        client, recovered_settings = attach(system, identity, expected_maxmemory)
        ready_seconds = time.perf_counter() - start
        expect(recovered_settings, settings, "runtime configuration after SIGKILL")
        check_counts(client, args.nodes)
        for i in sorted({0, 1, args.nodes // 2, args.nodes - 1, update_count - 1}):
            check_reads(client, i, args.nodes)
        for i in {0, update_count - 1}:
            expect(column(client.run(SCORE, key=key(i))), [i], "property update after SIGKILL")
        return {
            "system": system, "image": image, "durability": DURABILITY[system], "runtime_settings": settings,
            "node_create": nodes, "edge_create": edges, "property_update": updates,
            "indexed_point_read": summary(point_ms), "outgoing_one_hop": summary(hop_ms),
            "sigkill_recovery": {"nodes": args.nodes, "edges": args.nodes * len(SKIPS),
                                 "verified_update_rows": len({0, update_count - 1}), "ready_seconds": ready_seconds},
        }
    except Exception:
        if identity and inspect_owned("container", identity, run_id):
            print(container_logs(identity, 25), file=sys.stderr)
        raise
    finally:
        try:
            if client is not None:
                client.close()
        finally:
            errors = cleanup_owned((name, init), volume, run_id)
            if errors:
                raise RuntimeError(f"owned benchmark resource cleanup failed: {errors}")


def use_local_context(context):
    endpoint = json.loads(docker("context", "inspect", "--format", "{{json .Endpoints.docker.Host}}", "--", context))
    if not isinstance(endpoint, str) or not endpoint.startswith("unix:///") or endpoint == "unix:///":
        raise ValueError("the Docker context must use a local Unix socket (unix:///path)")
    # Pin the inspected endpoint for the helpers and the client throughout the run.
    os.environ.pop("DOCKER_CONTEXT", None)
    os.environ["DOCKER_HOST"] = endpoint


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--context", required=True, help="explicit local Docker context, such as colima")
    parser.add_argument("--brainwire-image", required=True, help="local versioned image, digest or immutable ID")
    parser.add_argument("--neo4j-image", default=NEO4J_IMAGE)
    parser.add_argument("--falkordb-image", default=FALKOR_IMAGE)
    parser.add_argument("--nodes", type=int, default=10_000)
    parser.add_argument("--batch", type=int, default=500)
    parser.add_argument("--reads", type=int, default=500)
    parser.add_argument("--update-batches", type=int, default=20)
    parser.add_argument("--memory-mib", type=int, default=3072)
    parser.add_argument("--cpus", type=int, default=2)
    parser.add_argument("--out", type=Path, required=True, help="new JSON receipt path")
    args = parser.parse_args()
    if not (100 <= args.nodes <= 1_000_000 and 1 <= args.batch <= min(args.nodes, 1000)
            and 1 <= args.reads <= 10_000 and 1 <= args.update_batches <= 1000
            and 1536 <= args.memory_mib <= 16384 and 1 <= args.cpus <= 16):
        parser.error("use 100–1,000,000 nodes, a batch of 1–min(nodes,1000), 1–10,000 reads, "
                     "1–1000 update batches, 1536–16384 MiB and 1–16 CPUs")
    try:
        use_local_context(args.context)
    except (OSError, RuntimeError, ValueError) as error:
        parser.error(str(error))
    engine = json.loads(docker("info", "--format", "{{json .}}"))
    if args.memory_mib * 1024 * 1024 >= engine["MemTotal"] or args.cpus > engine["NCPU"]:
        parser.error("container limits must fit the Docker engine with memory left for the host")
    references = {"brainwire": args.brainwire_image, "falkordb": args.falkordb_image,
                  "neo4j": args.neo4j_image}
    images = {name: resolve_image(reference) for name, reference in references.items()}
    started = datetime.now(timezone.utc)
    run_id = uuid.uuid4().hex[:12]
    report = {
        "schema_version": 1, "run_id": run_id, "started_at_utc": started.isoformat(),
        "runner_git_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
        "runner_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "host": {"platform": platform.platform(), "architecture": platform.machine(), "python": platform.python_version()},
        "docker": {field: engine[field] for field in ("ServerVersion", "OperatingSystem", "Architecture", "NCPU", "MemTotal")},
        "images": images,
        "clients": {name: importlib.metadata.version(name) for name in ("neo4j", "falkordb", "redis")},
        "parameters": {name: getattr(args, name) for name in ("nodes", "batch", "reads", "update_batches", "memory_mib", "cpus")},
        "fixture": "one node label, one edge type, deterministic ring plus skip edges; outgoing degree 2",
        "configured_engine_memory_mib": {
            "brainwire": {"cache": args.memory_mib // 4, "memtables": args.memory_mib // 16,
                          "query": args.memory_mib // 32},
            "falkordb": {"redis_maxmemory": args.memory_mib * 3 // 4},
            "neo4j": {"heap": args.memory_mib // 4, "page_cache": args.memory_mib * 3 // 8},
        },
        "durability_scope": "process SIGKILL and volume reuse; durability and authentication settings vary by engine",
        "status": "running", "results": [],
    }
    with args.out.open("x") as receipt:
        def save_receipt():
            receipt.seek(0)
            json.dump(report, receipt, indent=2)
            receipt.write("\n")
            receipt.truncate()
            receipt.flush()

        save_receipt()
        try:
            for system, image in images.items():
                report["active_system"] = system
                save_receipt()
                print(f"[{system}] starting isolated fixture {run_id}", flush=True)
                report["results"].append(run_system(system, args, image, run_id))
                save_receipt()
            report.pop("active_system")
            report["status"] = "passed"
        except BaseException as error:
            report["status"] = "failed"
            report["failure"] = {"type": type(error).__name__, "message": str(error)}
            raise
        finally:
            report["finished_at_utc"] = datetime.now(timezone.utc).isoformat()
            save_receipt()
            print(args.out, flush=True)


if __name__ == "__main__":
    main()
