#!/usr/bin/env python3
"""Run a small, reproducible local Cypher comparison in isolated Docker volumes.

Only containers/volumes whose names include this run's unique ID are removed.
Images are intentionally retained. This is a diagnostic, not an audited benchmark.
"""
from __future__ import annotations

import argparse
import json
import math
import os
import pathlib
import platform
import statistics
import subprocess
import sys
import time
import uuid
from datetime import datetime, timezone

from falkordb import FalkorDB
from neo4j import GraphDatabase
from redis import Redis

ROOT = pathlib.Path(__file__).resolve().parents[3]
OUT = pathlib.Path(__file__).resolve().parent / "results"
IMAGES = {
    "brainwire": os.environ.get("BRAINWIRE_IMAGE", "brainwire-benchmark:20261019"),
    "neo4j": "neo4j:2026.09.0",
    "falkordb": "falkordb/falkordb-server:6.0.0",
}


def docker(*args: str, capture: bool = True) -> str:
    p = subprocess.run(["docker", *args], check=True, text=True,
                       stdout=subprocess.PIPE if capture else None,
                       stderr=subprocess.PIPE if capture else None)
    return p.stdout.strip() if capture else ""


def port(container: str, inside: str) -> int:
    return int(docker("port", container, inside).rsplit(":", 1)[1])


def wait_ready(check, label: str, timeout: int = 180) -> None:
    until = time.monotonic() + timeout
    last = None
    while time.monotonic() < until:
        try:
            check()
            return
        except Exception as exc:  # startup races are expected
            last = exc
            time.sleep(1)
    raise RuntimeError(f"{label} did not become ready: {last}")


def percentile(values, p):
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, math.ceil(p * len(ordered)) - 1)]


def summary(values):
    return {"count": len(values), "p50_ms": round(percentile(values, .50), 3),
            "p95_ms": round(percentile(values, .95), 3),
            "p99_ms": round(percentile(values, .99), 3),
            "mean_ms": round(statistics.mean(values), 3)}


class Client:
    def __init__(self, system, uri):
        self.system = system
        self.driver = None
        self.graph = None
        self.session = None
        if system in ("brainwire", "neo4j"):
            self.driver = GraphDatabase.driver(uri, auth=None if system == "brainwire" else ("neo4j", "benchmark-only"))
            self.session = self.driver.session(database="neo4j" if system == "neo4j" else "benchmark")
        else:
            self.graph = FalkorDB(host="127.0.0.1", port=int(uri), socket_timeout=300).select_graph("benchmark")

    def run(self, query, params=None):
        params = params or {}
        if self.graph:
            return self.graph.query(query, params).result_set
        return list(self.session.run(query, **params))

    def close(self):
        if self.driver:
            self.session.close()
            self.driver.close()


def node_create(system):
    return "UNWIND $rows AS row CREATE (n:Item {entity_id: row.key, value: row.value}) RETURN count(n)"


def edge_create(system):
    return "UNWIND $rows AS row MATCH (a:Item {entity_id: row.src}) MATCH (b:Item {entity_id: row.dst}) CREATE (a)-[:LINK]->(b) RETURN count(*)"


def fetch_rows(result):
    if result and isinstance(result[0], (list, tuple)):
        return result
    return result


def run_system(system, args, run_id):
    cname = f"brainwire-comparison-{run_id}-{system}"
    vname = f"bw-compare-{run_id}-{system}"
    docker("volume", "create", vname)
    started = time.monotonic()
    container = None
    try:
        persisted_path = "/var/lib/falkordb/data" if system == "falkordb" else "/data"
        common = ["run", "-d", "--name", cname, "--memory", "8g", "--cpus", "4",
                  "-v", f"{vname}:{persisted_path}"]
        if system == "neo4j":
            container = docker(*common, "-p", "127.0.0.1::7687", "-e", "NEO4J_AUTH=neo4j/benchmark-only",
                               "-e", "NEO4J_server_memory_heap_initial__size=1G",
                               "-e", "NEO4J_server_memory_heap_max__size=2G",
                               "-e", "NEO4J_server_memory_pagecache_size=3G", IMAGES[system])
            p = port(cname, "7687/tcp")
            uri = f"bolt://127.0.0.1:{p}"
        elif system == "falkordb":
            container = docker(*common, "-p", "127.0.0.1::6379", "-e", "REDIS_ARGS=--appendonly yes --appendfsync always --maxmemory 6gb",
                               "-e", "FALKORDB_ARGS=THREAD_COUNT 4",
                               IMAGES[system])
            p = port(cname, "6379/tcp")
            uri = str(p)
        else:
            ontology = pathlib.Path(args.workdir) / f"{run_id}-ontology.json"
            ontology.write_text(json.dumps({"version": "1", "nodes": [{"label": "Item", "properties": ["entity_id", "value", "score"], "indexed_fields": ["entity_id"]}], "edges": [{"type": "LINK", "from": {"label": "Item"}, "to": {"label": "Item"}}]}))
            # Initialize the volume with the exact local candidate image.
            docker("run", "--rm", "--name", f"{cname}-init", "--memory", "8g", "--cpus", "4",
                   "-v", f"{vname}:/data", "-v", f"{ontology}:/benchmark/ontology.json:ro",
                   "-e", "BRAINWIRE_SERVER_DATA_DIR=/data", "--entrypoint", "brainwire", IMAGES[system],
                   "db", "create", "benchmark", "--ontology", "/benchmark/ontology.json")
            ontology.unlink(missing_ok=True)
            container = docker(*common, "-p", "127.0.0.1::7687", "-e", "BRAINWIRE_SERVER_DATA_DIR=/data",
                               "-e", "BRAINWIRE_SERVER_DEFAULT_DATABASE=benchmark",
                               "-e", "BRAINWIRE_SERVER_LISTEN=0.0.0.0:7687",
                               "-e", "BRAINWIRE_MEMORY_CACHE=2GiB", "-e", "BRAINWIRE_MEMORY_MEMTABLES=512MiB",
                               "-e", "BRAINWIRE_MEMORY_QUERY=256MiB", "--entrypoint", "brainwire",
                               IMAGES[system], "serve")
            p = port(cname, "7687/tcp")
            uri = f"bolt://127.0.0.1:{p}"

        if system == "falkordb":
            wait_ready(lambda: Redis(host="127.0.0.1", port=int(uri), socket_connect_timeout=2).ping(), system)
        client = Client(system, uri)
        wait_ready(lambda: client.run("RETURN 1 AS ready"), system)

        if system == "neo4j":
            client.run("CREATE INDEX item_entity_id_idx IF NOT EXISTS FOR (n:Item) ON (n.entity_id)")
            client.run("CALL db.awaitIndexes(120)")
        elif system == "falkordb":
            client.run("CREATE INDEX FOR (n:Item) ON (n.entity_id)")

        batch = args.batch
        nodes = [{"key": f"n{i:07d}", "value": f"v{i % 1000:04d}"} for i in range(args.nodes)]
        ingest_start = time.monotonic()
        for i in range(0, len(nodes), batch):
            client.run(node_create(system), {"rows": nodes[i:i + batch]})
        edge_query = edge_create(system)
        edge_count = 0
        edge_batch = []
        for i in range(args.nodes):
            edge_batch.append({"src": f"n{i:07d}", "dst": f"n{(i + 1) % args.nodes:07d}"})
            edge_batch.append({"src": f"n{i:07d}", "dst": f"n{(i + 17) % args.nodes:07d}"})
            if len(edge_batch) >= batch:
                client.run(edge_query, {"rows": edge_batch})
                edge_count += len(edge_batch)
                edge_batch = []
        if edge_batch:
            client.run(edge_query, {"rows": edge_batch})
            edge_count += len(edge_batch)
        ingest_seconds = time.monotonic() - ingest_start

        count_result = fetch_rows(client.run("MATCH (n:Item) RETURN count(n) AS count"))
        edge_result = fetch_rows(client.run("MATCH (:Item)-[r:LINK]->(:Item) RETURN count(r) AS count"))
        def field(row, key, index=0):
            return row[key] if hasattr(row, "__getitem__") and not isinstance(row, (list, tuple)) else row[index]
        node_count = int(field(count_result[0], "count"))
        actual_edges = int(field(edge_result[0], "count"))
        if node_count != args.nodes or actual_edges != edge_count:
            raise RuntimeError(f"count mismatch: expected {args.nodes}/{edge_count}, got {node_count}/{actual_edges}")

        # Warm-up and verify representative query semantics before timing.
        point_query = "MATCH (n:Item {entity_id: $key}) RETURN n.value AS value"
        neighbor_query = "MATCH (n:Item {entity_id: $key})-[:LINK]->(m:Item) RETURN m.entity_id AS key LIMIT 20"
        client.run(point_query, {"key": "n0000001"})
        client.run(neighbor_query, {"key": "n0000001"})
        point_times, neighbor_times, update_times = [], [], []
        for repeat in range(args.repeats):
            for i in range(args.reads):
                key = f"n{(i * 7919 + repeat * 104729) % args.nodes:07d}"
                t = time.perf_counter_ns(); rows = client.run(point_query, {"key": key}); point_times.append((time.perf_counter_ns() - t) / 1e6)
                expected_value = f"v{int(key[1:]) % 1000:04d}"
                if len(rows) != 1 or field(rows[0], "value") != expected_value:
                    raise RuntimeError(f"point-read mismatch for {key}: expected {expected_value}, got {rows}")
            for i in range(args.neighborhoods):
                key = f"n{(i * 3571 + repeat * 65537) % args.nodes:07d}"
                t = time.perf_counter_ns(); rows = client.run(neighbor_query, {"key": key}); neighbor_times.append((time.perf_counter_ns() - t) / 1e6)
                expected_neighbors = {f"n{(int(key[1:]) + 1) % args.nodes:07d}",
                                      f"n{(int(key[1:]) + 17) % args.nodes:07d}"}
                actual_neighbors = {field(row, "key") for row in rows}
                if actual_neighbors != expected_neighbors:
                    raise RuntimeError(f"neighbor mismatch for {key}: expected {expected_neighbors}, got {actual_neighbors}")
            t = time.perf_counter_ns()
            for batch_index in range(args.update_batches):
                offset = (batch_index * batch + repeat * args.update_batches * batch) % args.nodes
                updates = [{"key": f"n{(offset + j) % args.nodes:07d}", "score": repeat * args.update_batches + batch_index} for j in range(batch)]
                updated = client.run("UNWIND $rows AS row MATCH (n:Item {entity_id: row.key}) SET n.score = row.score RETURN count(n) AS count", {"rows": updates})
                if len(updated) != 1 or int(field(updated[0], "count")) != len(updates):
                    raise RuntimeError(f"property-update count mismatch: expected {len(updates)}, got {updated}")
            update_times.append((time.perf_counter_ns() - t) / 1e6)

        stats = docker("stats", "--no-stream", "--format", "{{.MemUsage}}|{{.CPUPerc}}", cname).split("|", 1)
        # Exercise durable volume recovery outside the timed section.
        client.close()
        docker("restart", cname)
        # A dynamically published host port may be reassigned when a container restarts.
        p = port(cname, "6379/tcp" if system == "falkordb" else "7687/tcp")
        uri = str(p) if system == "falkordb" else f"bolt://127.0.0.1:{p}"
        if system == "falkordb":
            wait_ready(lambda: Redis(host="127.0.0.1", port=int(uri), socket_connect_timeout=2).ping(), f"{system} restart")
        client = Client(system, uri)
        wait_ready(lambda: client.run("RETURN 1 AS ready"), f"{system} restart")
        recovered_nodes = fetch_rows(client.run("MATCH (n:Item) RETURN count(n) AS count"))
        recovered_edges = fetch_rows(client.run("MATCH (:Item)-[r:LINK]->(:Item) RETURN count(r) AS count"))
        recovered = (int(field(recovered_nodes[0], "count")), int(field(recovered_edges[0], "count")))
        if recovered != (node_count, actual_edges):
            raise RuntimeError(f"restart recovery mismatch: expected {(node_count, actual_edges)}, got {recovered}")
        sample_after_restart = client.run(point_query, {"key": "n0000001"})
        if len(sample_after_restart) != 1 or field(sample_after_restart[0], "value") != "v0001":
            raise RuntimeError(f"property recovery mismatch: {sample_after_restart}")
        last_offset = ((args.repeats - 1) * args.update_batches * batch +
                       (args.update_batches - 1) * batch) % args.nodes
        last_key = f"n{last_offset:07d}"
        last_score = args.repeats * args.update_batches - 1
        score_rows = client.run("MATCH (n:Item {entity_id: $key}) RETURN n.score AS score", {"key": last_key})
        if len(score_rows) != 1 or int(field(score_rows[0], "score")) != last_score:
            raise RuntimeError(f"property update recovery mismatch for {last_key}: expected {last_score}, got {score_rows}")
        try:
            disk = int(docker("exec", cname, "sh", "-c", f"du -sb {persisted_path} 2>/dev/null | cut -f1"))
        except Exception:
            disk = None
        result = {
            "system": system, "image": IMAGES[system],
            "image_id": docker("image", "inspect", "--format", "{{.Id}}", IMAGES[system]),
            "image_repo_digests": docker("image", "inspect", "--format", '{{join .RepoDigests ","}}', IMAGES[system]),
            "run_id": run_id,
            "nodes": node_count, "edges": actual_edges, "batch_size": batch,
            "persistent_data_path": persisted_path,
            "restart_recovered_nodes": recovered[0], "restart_recovered_edges": recovered[1],
            "ingest_seconds": round(ingest_seconds, 3),
            "ingest_records_per_second": round((node_count + actual_edges) / ingest_seconds, 1),
            "point_read": summary(point_times), "neighborhood_1hop_limit20": summary(neighbor_times),
            "update_batch_series": summary(update_times),
            "update_batches_per_repeat": args.update_batches,
            "rows_per_update_batch": batch,
            "timing_samples_ms": {
                "point_read": [round(v, 4) for v in point_times],
                "neighborhood_1hop_limit20": [round(v, 4) for v in neighbor_times],
                "update_batch_series": [round(v, 4) for v in update_times],
            },
            "container_memory_sample": stats[0], "container_cpu_sample": stats[1], "data_dir_bytes": disk,
            "container_logs_tail": docker("logs", "--tail", "5", cname),
        }
        client.close()
        return result
    except Exception as exc:
        logs = ""
        if container:
            try:
                logs = docker("logs", "--tail", "25", cname)
            except Exception:
                pass
        raise RuntimeError(f"{system} benchmark failed: {exc}\nContainer log tail:\n{logs}") from exc
    finally:
        # Exact unique names only; images and unrelated user Docker state are retained.
        subprocess.run(["docker", "rm", "-f", cname], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        subprocess.run(["docker", "volume", "rm", vname], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--nodes", type=int, default=10_000)
    ap.add_argument("--batch", type=int, default=100)
    ap.add_argument("--repeats", type=int, default=5)
    ap.add_argument("--reads", type=int, default=200)
    ap.add_argument("--neighborhoods", type=int, default=100)
    ap.add_argument("--update-batches", type=int, default=20)
    ap.add_argument("--systems", nargs="+", choices=IMAGES, default=list(IMAGES))
    ap.add_argument("--workdir", default="/tmp")
    args = ap.parse_args()
    if args.nodes < 100 or args.batch < 1 or args.nodes < args.batch:
        ap.error("use at least 100 nodes and a positive batch size no larger than the graph")
    run_id = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ") + "-" + uuid.uuid4().hex[:6]
    OUT.mkdir(parents=True, exist_ok=True)
    metadata = {
        "started_at_utc": datetime.now(timezone.utc).isoformat(), "run_id": run_id,
        "git_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
        "worktree_dirty": bool(subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT, text=True).strip()),
        "host": platform.platform(), "architecture": platform.machine(),
        "python": sys.version.split()[0],
        "docker_server": docker("version", "--format", "{{.Server.Version}}"),
        "limits_per_db_container": {"cpus": 4, "memory": "8g", "one_database_at_a_time": True},
        "client_versions": {"neo4j_python": "5.28.8", "falkordb_python": "1.2.2"},
        "methodology": "synthetic deterministic ring+skip-edge graph; localhost clients; one persistent driver session per system; warm-up then repeated operations; diagnostic only",
        "parameters": vars(args), "results": [],
    }
    for system in args.systems:
        print(f"[{system}] starting isolated run {run_id}", flush=True)
        metadata["results"].append(run_system(system, args, run_id))
        path = OUT / f"{run_id}.json"
        path.write_text(json.dumps(metadata, indent=2) + "\n")
        print(f"[{system}] complete", flush=True)
    metadata["finished_at_utc"] = datetime.now(timezone.utc).isoformat()
    path = OUT / f"{run_id}.json"
    path.write_text(json.dumps(metadata, indent=2) + "\n")
    print(path)


if __name__ == "__main__":
    main()
