#!/usr/bin/env python3
"""Run a small, reproducible local Cypher comparison in isolated Docker volumes.

Only containers/volumes whose names include this run's unique ID are removed.
Images must already be present locally and are intentionally retained.
Brainwire uses unauthenticated development access for synthetic data only; its
published port is bound to host loopback. Do not reuse this setup for real data.
This is a diagnostic, not an audited benchmark.
"""
from __future__ import annotations

import argparse
import json
import math
import importlib.metadata
import re
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
from neo4j import GraphDatabase, Query
from redis import Redis

ROOT = pathlib.Path(__file__).resolve().parents[3]
OUT = pathlib.Path(__file__).resolve().parent / "results"
LABEL = "brainwire.comparison"
IMAGES = {
    "brainwire": os.environ.get("BRAINWIRE_IMAGE", "brainwire-benchmark:20261019"),
    "neo4j": "neo4j:2026.09.0",
    "falkordb": "falkordb/falkordb-server:6.0.0",
}


def docker(*args: str, capture: bool = True, timeout: int = 120) -> str:
    try:
        p = subprocess.run(["docker", *args], check=True, text=True,
                           stdout=subprocess.PIPE if capture else None,
                           stderr=subprocess.PIPE if capture else None, timeout=timeout)
    except subprocess.TimeoutExpired as exc:
        raise RuntimeError(f"docker {args[0]} timed out after {timeout}s") from exc
    except subprocess.CalledProcessError as exc:
        # Do not echo argv: other fixtures can pass authentication in arguments.
        raise RuntimeError(f"docker {args[0]} failed (exit {exc.returncode})\n"
                           f"stdout:\n{exc.stdout or ''}\nstderr:\n{exc.stderr or ''}") from exc
    return p.stdout.strip() if capture else ""



def resolve_image(reference):
    info = json.loads(docker("image", "inspect", reference))[0]
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", info.get("Id", "")):
        raise RuntimeError("image did not resolve to an immutable local ID")
    labels = info.get("Config", {}).get("Labels") or {}
    source_keys = ("org.opencontainers.image.revision", "org.opencontainers.image.source",
                   "org.opencontainers.image.version", "brainwire.source.sha256",
                   "brainwire.source_hash", "io.brainwire.source.sha256",
                   "org.brainwire.benchmark-source-sha256")
    return {"reference": reference, "id": info["Id"],
            "repo_digests": info.get("RepoDigests", []),
            "source_labels": {key: labels[key] for key in source_keys if key in labels}}


def inspect_owned(kind, reference, run_id):
    result = subprocess.run(["docker", kind, "inspect", reference], text=True,
                            capture_output=True, timeout=30)
    if result.returncode:
        if any(marker in result.stderr.lower() for marker in
               ("no such container", "no such volume", "no such object")):
            return None
        raise RuntimeError("cannot establish resource ownership or absence")
    value = json.loads(result.stdout)[0]
    labels = value.get("Labels", {}) if kind == "volume" else value.get("Config", {}).get("Labels", {})
    if (labels or {}).get(LABEL) != run_id:
        raise RuntimeError("refusing to operate on an unowned benchmark resource")
    return value


def cleanup_owned(names, volume, run_id):
    errors = []
    for kind, reference in [("container", name) for name in names] + [("volume", volume)]:
        try:
            value = inspect_owned(kind, reference, run_id)
            if value is None:
                continue
            identity = value["Id"] if kind == "container" else value["Name"]
            try:
                if kind == "container":
                    docker("rm", "-f", identity, timeout=30)
                else:
                    docker("volume", "rm", identity, timeout=30)
            except Exception:
                if inspect_owned(kind, identity, run_id) is not None:
                    raise
            if inspect_owned(kind, identity, run_id) is not None:
                raise RuntimeError("owned benchmark resource remains")
        except Exception as error:
            errors.append({"kind": kind, "error_type": type(error).__name__})
    return errors


def container_logs(container: str, tail: int) -> str:
    # Docker emits application stderr on stderr even when `docker logs` succeeds.
    p = subprocess.run(["docker", "logs", "--tail", str(tail), container],
                       check=True, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30)
    return f"stdout:\n{p.stdout.strip()}\nstderr:\n{p.stderr.strip()}"


def port(container: str, inside: str) -> int:
    return int(docker("port", container, inside).rsplit(":", 1)[1])



def redis_ready(uri):
    return Redis(host="127.0.0.1", port=int(uri), socket_connect_timeout=2,
                 socket_timeout=2).ping()



def falkor_configuration(uri, expected_maxmemory):
    """Verify actual Redis persistence/resource settings before timing."""
    redis = Redis(host="127.0.0.1", port=int(uri), socket_connect_timeout=2,
                  socket_timeout=2, decode_responses=True)
    try:
        settings = {}
        for name in ("appendonly", "appendfsync", "maxmemory"):
            values = redis.config_get(name)
            if name not in values:
                raise RuntimeError(f"FalkorDB did not report {name}")
            settings[name] = values[name]
        try:
            maxmemory = int(settings["maxmemory"])
        except (TypeError, ValueError):
            raise RuntimeError("FalkorDB reported invalid maxmemory") from None
        if (settings["appendonly"] != "yes" or settings["appendfsync"] != "always"
                or maxmemory != expected_maxmemory):
            raise RuntimeError("FalkorDB runtime persistence/resource settings differ from the benchmark fixture")
        settings["maxmemory"] = maxmemory
        server = redis.info("server")
        modules = redis.module_list()
        return {"config": settings, "redis_version": server.get("redis_version"),
                "modules": [{key: module[key] for key in ("name", "ver") if key in module}
                            for module in modules]}
    finally:
        redis.close()


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
            self.driver = GraphDatabase.driver(uri, auth=None if system == "brainwire" else ("neo4j", "benchmark-only"), connection_timeout=5, connection_acquisition_timeout=30, max_transaction_retry_time=0)
            self.session = self.driver.session(database="neo4j" if system == "neo4j" else "benchmark")
        else:
            self.graph = FalkorDB(host="127.0.0.1", port=int(uri), socket_timeout=60).select_graph("benchmark")

    def run(self, query, params=None):
        params = params or {}
        if self.graph:
            return self.graph.query(query, params).result_set
        return list(self.session.run(Query(query, timeout=60), **params))

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
    started = time.monotonic()
    container = None
    client = None
    ontology = None
    ontology_created = False
    result = None
    runtime_settings = None
    image = args.images[system]
    memory_mib = args.memory_gib * 1024
    try:
        docker("volume", "create", "--label", f"{LABEL}={run_id}", vname)
        inspect_owned("volume", vname, run_id)
        persisted_path = "/var/lib/falkordb/data" if system == "falkordb" else "/data"
        common = ["run", "--pull=never", "-d", "--name", cname, "--label", f"{LABEL}={run_id}",
                  "--memory", f"{args.memory_gib}g", "--memory-swap", f"{args.memory_gib}g", "--cpus", str(args.cpus),
                  "-v", f"{vname}:{persisted_path}"]
        if system == "neo4j":
            container = docker(*common, "-p", "127.0.0.1::7687", "-e", "NEO4J_AUTH=neo4j/benchmark-only",
                               "-e", f"NEO4J_server_memory_heap_initial__size={memory_mib // 4}M",
                               "-e", f"NEO4J_server_memory_heap_max__size={memory_mib // 4}M",
                               "-e", f"NEO4J_server_memory_pagecache_size={memory_mib * 3 // 8}M", image["id"])
            p = port(container, "7687/tcp")
            uri = f"bolt://127.0.0.1:{p}"
        elif system == "falkordb":
            container = docker(*common, "-p", "127.0.0.1::6379", "-e", f"REDIS_ARGS=--appendonly yes --appendfsync always --maxmemory {memory_mib * 3 // 4}mb",
                               "-e", f"FALKORDB_ARGS=THREAD_COUNT {args.cpus}",
                               image["id"])
            p = port(container, "6379/tcp")
            uri = str(p)
        else:
            ontology = pathlib.Path(args.workdir) / f"{run_id}-ontology.json"
            with ontology.open("x") as output:
                ontology_created = True
                output.write(json.dumps({"version": "1", "nodes": [{"label": "Item", "properties": ["entity_id", "value", "score"], "indexed_fields": ["entity_id"]}], "edges": [{"type": "LINK", "from": {"label": "Item"}, "to": {"label": "Item"}}]}))
            # Initialize the volume with the exact local candidate image.
            docker("run", "--pull=never", "--rm", "--name", f"{cname}-init", "--label", f"{LABEL}={run_id}", "--memory", f"{args.memory_gib}g",
                   "--memory-swap", f"{args.memory_gib}g", "--cpus", str(args.cpus),
                   "-v", f"{vname}:/data", "-v", f"{ontology}:/benchmark/ontology.json:ro",
                   "-e", "BRAINWIRE_SERVER_DATA_DIR=/data", "--entrypoint", "brainwire", image["id"],
                   "db", "create", "benchmark", "--ontology", "/benchmark/ontology.json")
            ontology.unlink(missing_ok=True)
            container = docker(*common, "-p", "127.0.0.1::7687", "-e", "BRAINWIRE_SERVER_DATA_DIR=/data",
                               "-e", "BRAINWIRE_SERVER_DEFAULT_DATABASE=benchmark",
                               "-e", "BRAINWIRE_SERVER_LISTEN=0.0.0.0:7687",
                               "-e", f"BRAINWIRE_MEMORY_CACHE={memory_mib // 4}MiB", "-e", f"BRAINWIRE_MEMORY_MEMTABLES={memory_mib // 16}MiB",
                               "-e", f"BRAINWIRE_MEMORY_QUERY={memory_mib // 32}MiB", "--entrypoint", "brainwire",
                               image["id"], "serve", "--allow-unauthenticated-remote")
            p = port(container, "7687/tcp")
            uri = f"bolt://127.0.0.1:{p}"

        state = inspect_owned("container", cname, run_id)
        if state is None or state.get("Image") != image["id"]:
            raise RuntimeError("benchmark container image differs from the pinned local image ID")
        container = state["Id"]
        if system == "falkordb":
            wait_ready(lambda: redis_ready(uri), system)
            runtime_settings = falkor_configuration(uri, memory_mib * 3 // 4 * 1024 * 1024)
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

        stats = docker("stats", "--no-stream", "--format", "{{.MemUsage}}|{{.CPUPerc}}", container).split("|", 1)
        # Exercise durable volume recovery outside the timed section.
        client.close()
        client = None
        docker("restart", container)
        # A dynamically published host port may be reassigned when a container restarts.
        p = port(container, "6379/tcp" if system == "falkordb" else "7687/tcp")
        uri = str(p) if system == "falkordb" else f"bolt://127.0.0.1:{p}"
        if system == "falkordb":
            wait_ready(lambda: redis_ready(uri), f"{system} restart")
            recovered_settings = falkor_configuration(uri, memory_mib * 3 // 4 * 1024 * 1024)
            if recovered_settings != runtime_settings:
                raise RuntimeError("FalkorDB runtime configuration changed after restart")
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
            disk = int(docker("exec", container, "sh", "-c", f"du -sb {persisted_path} 2>/dev/null | cut -f1"))
        except Exception:
            disk = None
        result = {
            "system": system, "image": image["reference"],
            "image_id": image["id"],
            "image_repo_digests": image["repo_digests"],
            "image_source_labels": image.get("source_labels", {}),
            "runtime_settings": runtime_settings,
            "run_id": run_id,
            "nodes": node_count, "edges": actual_edges, "batch_size": batch,
            "persistent_data_path": persisted_path,
            "access_mode": ("unauthenticated development access; synthetic data only; host loopback"
                            if system == "brainwire" else "benchmark fixture; host loopback"),
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
            "container_logs_tail": container_logs(container, 5),
        }
        client.close()
        client = None
        return result
    except Exception as exc:
        logs = ""
        if container:
            try:
                owned = inspect_owned("container", container, run_id)
                if owned is not None:
                    logs = container_logs(owned["Id"], 25)
            except Exception as log_exc:
                logs = f"Could not collect container logs: {log_exc}"
        raise RuntimeError(f"{system} benchmark failed: {exc}\nContainer log tail:\n{logs}") from exc
    finally:
        errors = []
        if client is not None:
            try:
                client.close()
            except Exception as error:
                errors.append({"kind": "client", "error_type": type(error).__name__})
        if ontology_created:
            try:
                ontology.unlink(missing_ok=True)
            except Exception as error:
                errors.append({"kind": "fixture", "error_type": type(error).__name__})
        errors.extend(cleanup_owned((cname, f"{cname}-init"), vname, run_id))
        if result is not None:
            result["cleanup_errors"] = errors
            result["owned_resources_cleaned"] = not errors
        if errors:
            if sys.exc_info()[0] is None:
                raise RuntimeError(f"benchmark cleanup failed: {errors}")
            sys.exc_info()[1].cleanup_errors = errors
            print(f"benchmark cleanup also failed: {errors}", file=sys.stderr)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--cpus", type=int, choices=range(1, 5), default=2)
    ap.add_argument("--memory-gib", type=int, choices=(4, 8), default=4)
    ap.add_argument("--nodes", type=int, default=10_000)
    ap.add_argument("--batch", type=int, default=100)
    ap.add_argument("--repeats", type=int, default=5)
    ap.add_argument("--reads", type=int, default=200)
    ap.add_argument("--neighborhoods", type=int, default=100)
    ap.add_argument("--update-batches", type=int, default=20)
    ap.add_argument("--systems", nargs="+", choices=IMAGES, default=list(IMAGES))
    ap.add_argument("--workdir", default="/tmp")
    args = ap.parse_args()
    if not 100 <= args.nodes <= 1_000_000 or not 1 <= args.batch <= min(args.nodes, 1000):
        ap.error("use 100..1000000 nodes and batch size 1..min(nodes,1000)")
    if not 1 <= args.repeats <= 20 or not 1 <= args.reads <= 10000 or not 1 <= args.neighborhoods <= 10000 or not 1 <= args.update_batches <= 1000:
        ap.error("timing parameters exceed bounded diagnostic limits")
    if len(set(args.systems)) != len(args.systems):
        ap.error("systems must be unique")
    # Resolve every image before creating any volume or container.
    args.images = {system: resolve_image(IMAGES[system]) for system in args.systems}
    run_id = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ") + "-" + uuid.uuid4().hex[:6]
    OUT.mkdir(parents=True, exist_ok=True)
    metadata = {
        "started_at_utc": datetime.now(timezone.utc).isoformat(), "run_id": run_id,
        "git_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
        "worktree_dirty": bool(subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT, text=True).strip()),
        "host": platform.platform(), "architecture": platform.machine(),
        "python": sys.version.split()[0],
        "docker_server": docker("version", "--format", "{{.Server.Version}}"),
        "limits_per_db_container": {"cpus": args.cpus, "memory": f"{args.memory_gib}g", "swap": "disabled", "one_database_at_a_time": True},
        "client_versions": {"neo4j_python": importlib.metadata.version("neo4j"), "falkordb_python": importlib.metadata.version("falkordb")},
        "methodology": "synthetic deterministic ring+skip-edge graph; localhost clients; one persistent driver session per system; warm-up then repeated operations; diagnostic only",
        "parameters": {key: value for key, value in vars(args).items() if key != "images"},
        "images": args.images, "results": [],
    }
    for system in args.systems:
        print(f"[{system}] starting isolated run {run_id}", flush=True)
        if system == "brainwire":
            print("[brainwire] UNAUTHENTICATED development fixture; synthetic data only; "
                  "published on host 127.0.0.1 only", flush=True)
        path = OUT / f"{run_id}.json"
        try:
            metadata["results"].append(run_system(system, args, run_id))
        except Exception as error:
            metadata["result"] = "FAIL"
            metadata["failed_system"] = system
            metadata["error_type"] = type(error).__name__
            metadata["error"] = str(error)
            metadata["cleanup_errors"] = getattr(error, "cleanup_errors", [])
            metadata["finished_at_utc"] = datetime.now(timezone.utc).isoformat()
            path.write_text(json.dumps(metadata, indent=2) + "\n")
            raise
        path.write_text(json.dumps(metadata, indent=2) + "\n")
        print(f"[{system}] complete", flush=True)
    metadata["result"] = "PASS"
    metadata["finished_at_utc"] = datetime.now(timezone.utc).isoformat()
    path = OUT / f"{run_id}.json"
    path.write_text(json.dumps(metadata, indent=2) + "\n")
    print(path)


if __name__ == "__main__":
    main()
