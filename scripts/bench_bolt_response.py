#!/usr/bin/env python3
"""Opt-in native Bolt response diagnostic with private, disposable stores.

Example (after all builds/tests have stopped):
  .venv/bin/python scripts/bench_bolt_response.py \
    --baseline /path/to/baseline/brainwire --candidate /path/to/candidate/brainwire \
    --output /private/tmp/brainwire-response-receipt.json

Only --self-check is safe to run while building. Timed measurements include
client hashing and transaction completion. RSS is sampled, not a true peak.
This synthetic loopback diagnostic does not qualify production, cgroups, OOM,
ACL, TLS, executor streaming, power-loss recovery or a process-wide memory cap.
"""
from __future__ import annotations

import argparse
from collections import Counter
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import re
import socket
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from contextlib import suppress

QUERY = "MATCH (n:Payload) RETURN n.entity_id AS id, n.payload AS payload ORDER BY id"
BUILD_COMMANDS = {"cargo", "cargo-clippy", "rustc", "rust-lld", "clang", "clang++", "cc1", "cmake", "ninja", "make"}


def command(*args: str, timeout: float = 5) -> dict:
    try:
        result = subprocess.run(args, capture_output=True, text=True, timeout=timeout, check=False)
        return {"available": result.returncode == 0, "returncode": result.returncode,
                "stdout": result.stdout.strip(), "stderr": result.stderr.strip()}
    except (OSError, subprocess.SubprocessError) as error:
        return {"available": False, "error": type(error).__name__}


def build_processes(text: str) -> dict[str, int]:
    # Read executable names only: argument vectors may contain credentials.
    names = [Path(line.strip()).name for line in text.splitlines() if line.strip()]
    return dict(sorted(Counter(name for name in names if name in BUILD_COMMANDS).items()))


def assert_no_builds() -> dict:
    receipt = command("ps", "-axo", "comm=")
    if not receipt["available"]:
        raise RuntimeError("cannot inspect active builds; run outside timed trials with process inspection available")
    found = build_processes(receipt["stdout"])
    if found:
        raise RuntimeError("active build executable counts detected; finish builds/tests before timing: " + json.dumps(found, sort_keys=True))
    return {"inspection": "ps executable names only", "matched_commands": sorted(BUILD_COMMANDS),
            "detected": {}, "scope": "best-effort local process inspection; does not detect remote/VM builds"}


def machine() -> dict:
    result = {"system": platform.system(), "release": platform.release(),
              "machine": platform.machine(), "cpu_count": os.cpu_count(),
              "load_1_5_15": list(os.getloadavg())}
    if sys.platform == "darwin":
        result["hardware"] = command("sysctl", "-n", "hw.model", "hw.memsize", "machdep.cpu.brand_string")
        result["power"] = command("pmset", "-g", "batt")
        result["settings"] = command("pmset", "-g")
        result["thermal"] = command("pmset", "-g", "therm")
        ac = "AC Power" in result["power"].get("stdout", "")
        low_power_off = bool(re.search(r"(?m)^\s*lowpowermode\s+0\s*$", result["settings"].get("stdout", "")))
        result["power_qualified"] = ac and low_power_off
        result["power_qualification"] = "macOS AC Power and active lowpowermode=0; actual thermal throttling unverified"
    else:
        supplies = []
        for supply in sorted(Path("/sys/class/power_supply").glob("*")):
            try:
                supplies.append({"name": supply.name,
                                 "type": (supply / "type").read_text().strip(),
                                 "online": (supply / "online").read_text().strip()})
            except OSError:
                continue
        result["power_supplies"] = supplies
        result["power_qualified"] = False
        result["power_qualification"] = "power mode not independently verified on this platform"
    return result


def percentile(values: list[float], fraction: float) -> float:
    if not values:
        raise ValueError("cannot summarize zero samples")
    return sorted(values)[max(0, math.ceil(len(values) * fraction) - 1)]


def summary(values: list[float]) -> dict:
    return {"samples": len(values), "p50_ms": statistics.median(values),
            "p95_ms": percentile(values, .95), "p99_ms": percentile(values, .99),
            "min_ms": min(values), "max_ms": max(values)}


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as file:
        for part in iter(lambda: file.read(1 << 20), b""):
            digest.update(part)
    return digest.hexdigest()


def payload(row: int, size: int) -> str:
    prefix = f"synthetic-row-{row:06d}:"
    if size < len(prefix):
        raise ValueError("payload-bytes is too small for the row identity")
    return prefix + "abcdefghijklmnopqrstuvwxyz" * ((size - len(prefix) + 25) // 26)


def row_payload(row: int, size: int) -> str:
    return payload(row, size)[:size]


def digest_row(digest, identity: str, value: str) -> None:
    for field in (identity, value):
        raw = field.encode("utf-8")
        digest.update(len(raw).to_bytes(8, "big"))
        digest.update(raw)


def expected_hash(rows: int, size: int) -> str:
    digest = hashlib.sha256()
    for row in range(rows):
        digest_row(digest, f"id-{row:06d}", row_payload(row, size))
    return digest.hexdigest()


class RssSampler:
    def __init__(self, pid: int, interval: float):
        self.pid, self.interval = pid, interval
        self.samples = []
        self.errors = 0
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self.sample, daemon=True)

    def sample(self):
        started = time.monotonic()
        while not self.stop.is_set():
            result = command("ps", "-o", "rss=", "-p", str(self.pid), timeout=2)
            try:
                if not result["available"]:
                    raise ValueError()
                self.samples.append({"elapsed_seconds": time.monotonic() - started,
                                     "rss_bytes": int(result["stdout"]) * 1024})
            except (ValueError, KeyError):
                self.errors += 1
            self.stop.wait(self.interval)

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *_):
        self.stop.set()
        self.thread.join(timeout=3)
        if self.thread.is_alive():
            raise RuntimeError("RSS sampler failed to stop")

    def receipt(self):
        return {"scope": "server process RSS sampled by ps, including warmup; not a true peak or cgroup memory",
                "requested_interval_seconds": self.interval,
                "ps_overhead_in_timed_trials": True, "sampling_errors": self.errors,
                "sampled_max_rss_bytes": max((s["rss_bytes"] for s in self.samples), default=None),
                "samples": self.samples}


def stop_owned(process):
    if process is None:
        return
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)


def run_one(binary: Path, args, expected: str, graph_database) -> dict:
    # Every invocation owns a new private directory, database and process.
    with tempfile.TemporaryDirectory(prefix="brainwire-response-diagnostic-") as temporary:
        root = Path(temporary)
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        env = {k: v for k, v in os.environ.items() if not k.startswith("BRAINWIRE_")}
        config = root / "brainwire.toml"
        config.write_text(f'''[server]
listen = "127.0.0.1:{port}"
data_dir = {json.dumps(str(root / 'data'))}
default_database = "bench"
[memory]
cache = "64MiB"
memtables = "4MiB"
query = "256MiB"
ingress = "32MiB"
''')
        ontology = root / "ontology.json"
        ontology.write_text(json.dumps({"version": "1", "nodes": [
            {"label": "Payload", "properties": ["payload"]}], "edges": []}))
        subprocess.run([str(binary), "db", "create", "bench", "--ontology", str(ontology),
                        "--config", str(config)], env=env, check=True, capture_output=True, text=True, timeout=30)
        process = None
        result = {}
        try:
            with (root / "server.log").open("w") as log:
                process = subprocess.Popen([str(binary), "serve", "--config", str(config)],
                                           env=env, stdout=log, stderr=log)
                deadline = time.monotonic() + 30
                while True:
                    if process.poll() is not None:
                        raise RuntimeError("owned server exited: " + (root / "server.log").read_text()[-4000:])
                    try:
                        with socket.create_connection(("127.0.0.1", port), timeout=.1):
                            break
                    except OSError:
                        if time.monotonic() >= deadline:
                            raise RuntimeError("owned server startup timed out")
                        time.sleep(.025)
                with graph_database.driver(f"bolt://127.0.0.1:{port}", auth=None,
                                           max_transaction_retry_time=0, connection_timeout=10) as driver:
                    info = driver.get_server_info()
                    result["bolt_protocol_version"] = str(info.protocol_version)
                    result["server_agent"] = info.agent
                    with driver.session(database="bench", fetch_size=1) as session:
                        for row in range(args.rows):
                            with session.begin_transaction(timeout=args.query_timeout) as tx:
                                tx.run("CREATE (n:Payload {entity_id:$id, payload:$payload})",
                                       id=f"id-{row:06d}", payload=row_payload(row, args.payload_bytes)).consume()
                                tx.commit()
                        result["build_check_before_timing"] = assert_no_builds()
                        latencies = []
                        hashes = []
                        with RssSampler(process.pid, args.rss_interval) as sampler:
                            for repetition in range(args.warmup + args.repeats):
                                digest = hashlib.sha256()
                                count = byte_count = 0
                                started = time.perf_counter()
                                with session.begin_transaction(timeout=args.query_timeout) as tx:
                                    for record in tx.run(QUERY):
                                        identity, value = record["id"], record["payload"]
                                        digest_row(digest, identity, value)
                                        count += 1
                                        byte_count += len(value.encode("utf-8"))
                                    tx.commit()
                                elapsed_ms = (time.perf_counter() - started) * 1000
                                found = digest.hexdigest()
                                if count != args.rows or byte_count != args.rows * args.payload_bytes or found != expected:
                                    raise AssertionError(f"query data mismatch: rows={count}, bytes={byte_count}, hash={found}")
                                if repetition >= args.warmup:
                                    latencies.append(elapsed_ms)
                                    hashes.append(found)
                        result.update({"latency_ms": latencies, "latency_summary": summary(latencies),
                                       "payload_sha256_per_repeat": hashes, "rss": sampler.receipt(),
                                       "build_check_after_timing": assert_no_builds()})
                        if not sampler.samples or sampler.errors:
                            raise RuntimeError("RSS sampling unavailable or incomplete; inspect native process permissions")
        finally:
            stop_owned(process)
        result["owned_process_stopped"] = process is not None and process.poll() is not None
    result["owned_temp_store_removed"] = not root.exists()
    return result


def binary_info(path: Path) -> dict:
    if not path.is_file() or not os.access(path, os.X_OK):
        raise ValueError(f"binary is not an executable file: {path}")
    return {"path": str(path), "sha256": sha256(path), "version": command(str(path), "--version")}


def self_check() -> None:
    assert percentile([4, 1, 3, 2], .5) == 2
    assert percentile([4, 1, 3, 2], .99) == 4
    assert summary([4, 1, 3, 2])["p50_ms"] == 2.5
    assert len(row_payload(3, 128)) == 128
    assert row_payload(3, 128).startswith("synthetic-row-000003:")
    assert expected_hash(2, 128) == expected_hash(2, 128)
    assert expected_hash(2, 128) != expected_hash(3, 128)
    a, b = hashlib.sha256(), hashlib.sha256()
    digest_row(a, "ab", "c")
    digest_row(b, "a", "bc")
    assert a.digest() != b.digest()
    assert build_processes("/Users/a/.cargo/bin/cargo\n/usr/bin/python\n/usr/bin/rustc\n/other/cargo") == {"cargo": 2, "rustc": 1}
    assert build_processes("/a path/cargo\n/usr/bin/python") == {"cargo": 1}
    print("self-check PASS (no server or timed benchmark started)")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--baseline", type=Path)
    parser.add_argument("--candidate", type=Path)
    parser.add_argument("--output", type=Path, help="new private receipt; existing files are refused")
    parser.add_argument("--rows", type=int, default=8)
    parser.add_argument("--payload-bytes", type=int, default=512 * 1024)
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--warmup", type=int, default=5)
    parser.add_argument("--repeats", type=int, default=30)
    parser.add_argument("--rss-interval", type=float, default=.05)
    parser.add_argument("--query-timeout", type=float, default=30)
    parser.add_argument("--allow-unqualified-power", action="store_true",
                        help="record a diagnostic even when AC/low-power status cannot be qualified")
    parser.add_argument("--self-check", action="store_true")
    args = parser.parse_args()
    if args.self_check:
        self_check()
        return 0
    if not args.baseline or not args.candidate or not args.output:
        parser.error("--baseline, --candidate and --output are required")
    if not (1 <= args.rows <= 10000 and 32 <= args.payload_bytes <= 16 * 1024 * 1024
            and args.rows * args.payload_bytes <= 64 * 1024 * 1024
            and args.rounds >= 1 and args.warmup >= 0 and args.repeats >= 2
            and .01 <= args.rss_interval <= 5 and args.query_timeout > 0):
        parser.error("invalid workload; total payload must be <=64MiB, each record <=16MiB, repeats>=2, rss interval .01..5s")
    report = {"result": "FAIL", "synthetic": True, "scope": "native loopback response diagnostic",
              "limits": ["sampled RSS is not a true peak or process memory ceiling", "no cgroup/kernel OOM evidence",
                         "not production qualification", "latencies include client hashing and transaction completion",
                         "small sample p99 is descriptive, not a service-level guarantee"],
              "workload": {k: getattr(args, k) for k in ("rows", "payload_bytes", "rounds", "warmup", "repeats", "rss_interval", "query_timeout")},
              "query": QUERY, "runs": []}
    # Open exclusively before starting resources, and always leave a failure receipt.
    fd = os.open(args.output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w") as output:
        try:
            import neo4j
            from neo4j import GraphDatabase
            report["driver_version"] = neo4j.__version__
            report["python_version"] = platform.python_version()
            report["host_start"] = machine()
            report["build_check_start"] = assert_no_builds()
            if not report["host_start"]["power_qualified"] and not args.allow_unqualified_power:
                raise RuntimeError("host power mode unqualified; connect AC and disable low power, or explicitly allow an unqualified diagnostic")
            binaries = {"baseline": args.baseline.resolve(), "candidate": args.candidate.resolve()}
            report["binaries"] = {name: binary_info(path) for name, path in binaries.items()}
            expected = expected_hash(args.rows, args.payload_bytes)
            report["expected_payload_sha256"] = expected
            for round_number in range(args.rounds):
                order = ("baseline", "candidate") if round_number % 2 == 0 else ("candidate", "baseline")
                for name in order:
                    print(f"round {round_number + 1}/{args.rounds}: {name}", file=sys.stderr, flush=True)
                    run = run_one(binaries[name], args, expected, GraphDatabase)
                    run.update({"binary": name, "round": round_number + 1, "host_after": machine()})
                    report["runs"].append(run)
                    if sha256(binaries[name]) != report["binaries"][name]["sha256"]:
                        raise RuntimeError("binary changed during diagnostic")
                    if not run["host_after"]["power_qualified"] and not args.allow_unqualified_power:
                        raise RuntimeError("host power qualification changed during diagnostic")
            report["summaries"] = {name: summary([value for run in report["runs"] if run["binary"] == name for value in run["latency_ms"]])
                                   for name in binaries}
            report["candidate_p50_change_percent"] = (report["summaries"]["candidate"]["p50_ms"] /
                                                       report["summaries"]["baseline"]["p50_ms"] - 1) * 100
            report["candidate_p99_change_percent"] = (report["summaries"]["candidate"]["p99_ms"] /
                                                       report["summaries"]["baseline"]["p99_ms"] - 1) * 100
            report["host_end"] = machine()
            report["all_payloads_verified"] = True
            report["result"] = "PASS"
            report["interpretation"] = "Correctness checks passed for this workload. Evaluate host load, variance, p99 and per-round values before interpreting performance."
        except Exception as error:
            report["error_type"] = type(error).__name__
            report["error"] = str(error)
            report["error_code"] = getattr(error, "code", None)
        json.dump(report, output, indent=2)
        output.write("\n")
    print(json.dumps({"result": report["result"], "receipt": str(args.output),
                      "candidate_p50_change_percent": report.get("candidate_p50_change_percent")}))
    return 0 if report["result"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
