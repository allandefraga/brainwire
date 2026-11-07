#!/usr/bin/env python3
"""Run bounded mixed writes/reads and SIGKILL recovery on an owned synthetic graph."""

import argparse
from collections import Counter
import hashlib
import json
import math
from pathlib import Path
import secrets
import signal
import tempfile
import threading
import time
import uuid

if __package__:
    from .bench_relationship_types import bounded, cleanup, connect, deadline_exceeded, docker, interrupted
else:
    from bench_relationship_types import bounded, cleanup, connect, deadline_exceeded, docker, interrupted

MEMORY_CODES = {
    "Neo.TransientError.General.MemoryPressure",
    "Neo.TransientError.General.QueryMemoryPoolExhausted",
    "Neo.ClientError.Statement.MemoryLimitExceeded",
}
SAMPLE_COMMAND = """printf 'current '; cat /sys/fs/cgroup/memory.current
printf 'peak '; cat /sys/fs/cgroup/memory.peak
printf 'limit '; cat /sys/fs/cgroup/memory.max
cat /sys/fs/cgroup/memory.events
cat /proc/1/status"""


def identifier(index):
    return f"item-{index:08d}"


def batch_indices(rows, batch, iteration):
    start = (iteration * batch) % rows
    return range(start, min(start + batch, rows))


def progress_windows(seconds, window_seconds, minimum_writes, minimum_reads):
    windows = []
    for start in range(0, seconds, window_seconds):
        end = min(start + window_seconds, seconds)
        required = end - start == window_seconds or seconds <= window_seconds
        windows.append({"start_s": start, "end_s": end, "required": required,
                        "minimum_writes": minimum_writes if required else 0,
                        "minimum_reads": minimum_reads if required else 0,
                        "writes": 0, "reads": 0, "writes_after_requested_end": 0,
                        "reads_after_requested_end": 0, "last_completion_s": None})
    return windows


def record_progress(windows, window_seconds, role, elapsed, operation_started):
    if elapsed < 0:
        return
    if elapsed < windows[-1]["end_s"]:
        window = windows[int(elapsed // window_seconds)]
        window[role] += 1
    elif 0 <= operation_started < windows[-1]["end_s"]:
        window = windows[-1]
        window[role + "_after_requested_end"] += 1
    else:
        return
    window["last_completion_s"] = max(window["last_completion_s"] or 0, elapsed)


def evaluate_progress(windows):
    values = []
    for window in windows:
        observed = dict(window)
        observed["passed"] = (
            window["writes"] + window["writes_after_requested_end"] >= window["minimum_writes"]
            and window["reads"] + window["reads_after_requested_end"] >= window["minimum_reads"]
        ) if window["required"] else None
        values.append(observed)
    return values


def sample_memory(container):
    return parse_memory(docker("exec", container, "sh", "-c", SAMPLE_COMMAND, timeout=10).stdout)


def parse_memory(text):
    sample = {}
    for line in text.splitlines():
        values = line.split()
        if not values:
            continue
        key = values[0].rstrip(":")
        if key in {"current", "peak", "limit", "oom", "oom_kill"}:
            if len(values) != 2 or not values[1].isdigit():
                raise ValueError("invalid cgroup memory observation")
            sample["max" if key == "limit" else key] = int(values[1])
        elif key == "VmRSS":
            if len(values) != 3 or values[2] != "kB" or not values[1].isdigit():
                raise ValueError("invalid process memory observation")
            sample["rss_bytes"] = int(values[1]) * 1024
    if set(sample) != {"current", "peak", "max", "oom", "oom_kill", "rss_bytes"}:
        raise ValueError("incomplete memory observation")
    if sample["current"] > sample["max"] or sample["peak"] > sample["max"]:
        raise ValueError("memory observation exceeds configured cgroup")
    if sample["oom"] or sample["oom_kill"]:
        raise RuntimeError("cgroup reported an OOM event")
    return sample


def scalar(session, query, statement, **params):
    return session.run(query(statement, timeout=30), **params).single(strict=True)[0]


def verify(session, query, rows, expected):
    actual = {
        "nodes": scalar(session, query, "MATCH (n:Item) RETURN count(n)"),
        "edges": scalar(session, query, "MATCH (:Item)-[r:LINK]->(:Anchor) RETURN count(r)"),
        "node_revision_sum": scalar(session, query, "MATCH (n:Item) RETURN sum(n.revision)"),
        "edge_revision_sum": scalar(session, query, "MATCH (:Item)-[r:LINK]->(:Anchor) RETURN sum(r.revision)"),
        "scratch": scalar(session, query, "MATCH (n:Scratch) RETURN count(n)"),
    }
    wanted = {"nodes": rows, "edges": rows, "node_revision_sum": sum(expected),
              "edge_revision_sum": sum(expected), "scratch": 0}
    if actual != wanted:
        raise AssertionError(f"quiescent data checksum mismatch: {actual} != {wanted}")
    for start in range(0, rows, 500):
        indices = range(start, min(start + 500, rows))
        ids = [identifier(index) for index in indices]
        values = session.run(query(
            "UNWIND $ids AS id MATCH (n:Item {entity_id:id})-[r:LINK]->(:Anchor) "
            "RETURN n.entity_id AS id, n.revision AS node_revision, r.revision AS edge_revision ORDER BY id",
            timeout=30,
        ), ids=ids).data()
        wanted_rows = [{"id": identifier(index), "node_revision": expected[index],
                        "edge_revision": expected[index]} for index in indices]
        if values != wanted_rows:
            raise AssertionError("quiescent row-by-row verification failed")
    actual["exact_rows_verified"] = rows
    return actual


def exercise(args, report, directory, ownership):
    from neo4j import GraphDatabase, Query, __version__

    run = report["run"]
    container, volume = f"bw-mixed-{run}", f"bw-mixed-{run}-data"
    image = docker("image", "inspect", "--format", "{{.Id}}", args.image).stdout.strip()
    report.update(image_id=image, driver=__version__)
    token = secrets.token_hex(32)
    policy = directory / "policy.json"
    policy.write_text(json.dumps({"principals": [{
        "id": "bench", "token_sha256": hashlib.sha256(token.encode()).hexdigest(),
        "graphs": {"bench": "write"}, "instance_admin": True,
    }]}))
    policy.chmod(0o444)
    schema = directory / "ontology.json"
    schema.write_text(json.dumps({
        "version": "1", "nodes": [{"label": "Item", "properties": ["revision", "payload"]},
                                     {"label": "Anchor"}, {"label": "Scratch"}],
        "edges": [{"type": "LINK", "from": {"label": "Item"}, "to": {"label": "Anchor"},
                   "properties": ["revision"]}],
    }))
    schema.chmod(0o444)
    if docker("volume", "inspect", volume, check=False).returncode == 0:
        raise RuntimeError("generated volume already exists")
    docker("volume", "create", "--label", ownership, volume)
    if volume not in docker("volume", "ls", "-q", "--filter", f"label={ownership}").stdout.splitlines():
        raise RuntimeError("volume ownership not confirmed")
    common = (
        "--pull", "never", "--label", ownership, "--memory", f"{args.memory}g",
        "--memory-swap", f"{args.memory}g", "--cpus", "2",
        "--mount", f"type=volume,src={volume},dst=/data",
        "-e", "BRAINWIRE_MEMORY_CACHE=256MiB", "-e", "BRAINWIRE_MEMORY_MEMTABLES=64MiB",
        "-e", "BRAINWIRE_MEMORY_QUERY=256MiB", "-e", "BRAINWIRE_SERVER_DEFAULT_DATABASE=bench",
    )
    docker("run", "--rm", "--network", "none", *common,
           "--mount", f"type=bind,src={schema},dst=/ontology.json,readonly",
           image, "db", "create", "bench", "--ontology", "/ontology.json")
    container_id = docker("run", "-d", "--name", container, *common,
                          "--mount", f"type=bind,src={policy},dst=/policy.json,readonly",
                          "-e", "BRAINWIRE_SERVER_AUTH_FILE=/policy.json",
                          "-p", "127.0.0.1::7687", image).stdout.strip()
    expected = [0] * args.rows
    with connect(GraphDatabase.driver, container_id, token, 120) as session:
        session.run(Query("CREATE (:Anchor {entity_id:'root'})", timeout=args.query_timeout)).consume()
        for start in range(0, args.rows, args.batch):
            rows = [{"id": identifier(i), "payload": "x" * args.payload_bytes}
                    for i in range(start, min(start + args.batch, args.rows))]
            with session.begin_transaction(timeout=args.query_timeout) as tx:
                tx.run("UNWIND $rows AS row CREATE (:Item {entity_id:row.id, revision:0, payload:row.payload})",
                       rows=rows).consume()
                tx.run("UNWIND $rows AS row MATCH (n:Item {entity_id:row.id}), (a:Anchor {entity_id:'root'}) "
                       "CREATE (n)-[:LINK {revision:0}]->(a)", rows=rows).consume()
                tx.commit()
        report["initial"] = verify(session, Query, args.rows, expected)

    stop = threading.Event()
    failures = []
    writer_stats, reader_stats = Counter(), Counter()
    progress = progress_windows(args.seconds, args.progress_window,
                                args.minimum_writes_per_window, args.minimum_reads_per_window)
    progress_lock = threading.Lock()
    started = time.monotonic()

    def writer():
        try:
            with connect(GraphDatabase.driver, container_id, token, 30) as session:
                iteration = 0
                while not stop.is_set():
                    indices = list(batch_indices(args.rows, args.batch, iteration))
                    revision = iteration + 1
                    rows = [{"id": identifier(i), "revision": revision} for i in indices]
                    committing = False
                    operation_started = time.monotonic() - started
                    try:
                        with session.begin_transaction(timeout=args.query_timeout) as tx:
                            tx.run("UNWIND $rows AS row MATCH (n:Item {entity_id:row.id}) "
                                   "SET n.revision=row.revision", rows=rows).consume()
                            tx.run("UNWIND $rows AS row MATCH (n:Item {entity_id:row.id})-[r:LINK]->(:Anchor) "
                                   "SET r.revision=row.revision", rows=rows).consume()
                            tx.run("CREATE (:Scratch {entity_id:'temporary'})").consume()
                            tx.run("MATCH (n:Scratch {entity_id:'temporary'}) DELETE n").consume()
                            committing = True
                            tx.commit()
                    except Exception as error:
                        if not committing and getattr(error, "code", None) in MEMORY_CODES:
                            writer_stats["memory_denials"] += 1
                            stop.wait(.05)
                            continue
                        raise
                    for index in indices:
                        expected[index] = revision
                    writer_stats["committed_batches"] += 1
                    writer_stats["updated_rows"] += len(indices)
                    with progress_lock:
                        record_progress(progress, args.progress_window, "writes", time.monotonic() - started, operation_started)
                    iteration += 1
                    stop.wait(args.pause)
        except Exception as error:
            failures.append({"role": "writer", "error": type(error).__name__, "code": getattr(error, "code", None)})
            stop.set()

    def reader():
        try:
            with connect(GraphDatabase.driver, container_id, token, 30) as session:
                while not stop.is_set():
                    operation_started = time.monotonic() - started
                    try:
                        total = session.run(Query("MATCH (n:Item) RETURN count(n)", timeout=args.query_timeout)).single(strict=True)[0]
                        if total != args.rows:
                            raise AssertionError("concurrent node count mismatch")
                        values = session.run(Query("MATCH (n:Item) RETURN n.entity_id AS id, n.revision AS revision LIMIT 64",
                                                   timeout=args.query_timeout)).data()
                        if len(values) != min(args.rows, 64) or any(
                            not isinstance(row["id"], str) or type(row["revision"]) is not int or row["revision"] < 0
                            for row in values
                        ):
                            raise AssertionError("concurrent bounded read mismatch")
                        reader_stats["successful_reads"] += 1
                        with progress_lock:
                            record_progress(progress, args.progress_window, "reads", time.monotonic() - started, operation_started)
                    except Exception as error:
                        if getattr(error, "code", None) not in MEMORY_CODES:
                            raise
                        reader_stats["memory_denials"] += 1
                    stop.wait(args.pause)
        except Exception as error:
            failures.append({"role": "reader", "error": type(error).__name__, "code": getattr(error, "code", None)})
            stop.set()

    threads = [threading.Thread(target=writer, daemon=True), threading.Thread(target=reader, daemon=True)]
    try:
        for thread in threads:
            thread.start()
        until = started + args.seconds
        while not stop.is_set() and time.monotonic() < until:
            sample = sample_memory(container_id)
            if sample["max"] != args.memory * 1024**3:
                raise AssertionError("container memory cap differs from requested cap")
            sample["elapsed_s"] = time.monotonic() - started
            with progress_lock:
                sample["completed_writes"] = sum(window["writes"] for window in progress)
                sample["completed_reads"] = sum(window["reads"] for window in progress)
            report["memory_samples"].append(sample)
            stop.wait(min(args.sample_seconds, max(0, until - time.monotonic())))
    finally:
        stop.set()
        # A reader iteration contains two separately timed queries.
        for thread in threads:
            thread.join(timeout=2 * args.query_timeout + 5)
        if any(thread.is_alive() for thread in threads):
            docker("kill", container_id, check=False, timeout=15)
            for thread in threads:
                thread.join(timeout=5)
            raise RuntimeError("client thread exceeded shutdown deadline")
        report["mixed_seconds"] = time.monotonic() - started
        report["writer"] = dict(writer_stats)
        report["reader"] = dict(reader_stats)
        report["client_failures"] = failures
        report["progress_windows"] = evaluate_progress(progress)
        report["progress_status"] = "passed" if all(
            window["passed"] is not False for window in report["progress_windows"]
        ) else "failed"
    if (failures or not writer_stats["committed_batches"] or not reader_stats["successful_reads"]
            or report["progress_status"] != "passed"):
        raise AssertionError("mixed workload failed its configured progress requirement")
    with connect(GraphDatabase.driver, container_id, token, 30) as session:
        report["before_kill"] = verify(session, Query, args.rows, expected)
    docker("kill", container_id)
    docker("start", container_id)
    # Resolve the dynamically assigned port again after restart.
    with connect(GraphDatabase.driver, container_id, token, 60) as session:
        report["after_kill_restart"] = verify(session, Query, args.rows, expected)
    report["final_memory"] = sample_memory(container_id)
    docker("stop", "--time", "30", container_id)
    report["integrity"] = json.loads(docker("run", "--rm", "--network", "none", *common,
                                                   image, "db", "check", "bench", "--json", timeout=120).stdout)
    if report["integrity"].get("status") != "ok":
        raise AssertionError("offline integrity check failed")


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--image", required=True, help="locally available image; never pulled")
    result.add_argument("--output", required=True, type=Path, help="new receipt path in a Docker-shared directory")
    result.add_argument("--rows", type=bounded(100, 1_000_000), default=100_000)
    result.add_argument("--batch", type=bounded(1, 1000), default=250)
    result.add_argument("--seconds", type=bounded(1, 3600), default=600)
    result.add_argument("--memory", type=int, choices=(2, 3, 4, 8), default=2)
    result.add_argument("--payload-bytes", type=bounded(0, 4096), default=256)
    result.add_argument("--query-timeout", type=bounded(1, 120), default=30)
    result.add_argument("--sample-seconds", type=bounded(1, 30), default=2)
    result.add_argument("--deadline", type=bounded(60, 7200), default=1800)
    result.add_argument("--pause", type=float, default=.01)
    result.add_argument("--progress-window", type=bounded(1, 600), default=60,
                        help="completion-count window in seconds; trailing partial windows are diagnostic")
    result.add_argument("--minimum-writes-per-window", type=bounded(1, 100000), default=1,
                        help="minimum acknowledged batches in each required progress window")
    result.add_argument("--minimum-reads-per-window", type=bounded(1, 100000), default=1,
                        help="minimum successful reader iterations in each required progress window")
    return result


def validate(args):
    if args.batch > args.rows:
        raise ValueError("batch must not exceed row count")
    if args.deadline < args.seconds + 180 + 2 * args.query_timeout:
        raise ValueError("deadline must include setup and recovery time")
    if not math.isfinite(args.pause) or not .001 <= args.pause <= 10:
        raise ValueError("pause must be finite and between .001 and 10 seconds")


def main(args):
    validate(args)
    run = uuid.uuid4().hex
    ownership = f"brainwire.benchmark.run={run}"
    report = {"result": "FAIL", "synthetic": True, "run": run, "image": args.image,
              "runner_schema": 2,
              "workload_settings": {key: getattr(args, key) for key in (
                  "rows", "batch", "seconds", "memory", "payload_bytes", "query_timeout",
                  "sample_seconds", "deadline", "pause", "progress_window",
                  "minimum_writes_per_window", "minimum_reads_per_window")},
              "progress_semantics": "completion timestamps; full windows required, trailing partial diagnostic, sole short window required; an operation started before the requested end may complete during bounded shutdown and count in the final window",
              "rows": args.rows, "batch": args.batch, "requested_seconds": args.seconds,
              "memory_gib": args.memory, "cpus": 2, "payload_bytes": args.payload_bytes,
              "memory_samples": [], "scope": "one local standalone container, concurrent writer and reader",
              "limitations": ["sampled RSS is not a process-wide ceiling", "no kernel log verification",
                              "SIGKILL occurs after acknowledged writes and quiescent verification",
                              "no lost-ack, disk-loss, power-loss, quorum or multi-machine simulation"]}
    with args.output.open("x", encoding="utf-8") as output:
        with tempfile.TemporaryDirectory(prefix="bw-mixed-", dir=args.output.resolve().parent) as tmp:
            previous = {kind: signal.getsignal(kind) for kind in (signal.SIGALRM, signal.SIGTERM)}
            signal.signal(signal.SIGALRM, deadline_exceeded)
            signal.signal(signal.SIGTERM, interrupted)
            signal.alarm(args.deadline)
            try:
                exercise(args, report, Path(tmp), ownership)
                report["result"] = "PASS"
            except (Exception, KeyboardInterrupt) as error:
                report["error"] = type(error).__name__
                raise
            finally:
                signal.alarm(0)
                try:
                    report["cleanup"] = cleanup(ownership)
                except Exception as error:
                    report["cleanup"] = False
                    report["cleanup_error"] = type(error).__name__
                if not report["cleanup"]:
                    report["result"] = "FAIL"
                json.dump(report, output, indent=2)
                output.write("\n")
                for kind, handler in previous.items():
                    signal.signal(kind, handler)
                print(f"{report['result']} {args.output}", flush=True)
    if not report["cleanup"]:
        raise RuntimeError(f"cleanup incomplete for ownership label {ownership}")


if __name__ == "__main__":
    options = parser().parse_args()
    try:
        main(options)
    except ValueError as error:
        raise SystemExit(str(error)) from error
