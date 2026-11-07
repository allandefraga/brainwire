"""Bounded-duration synthetic mixed-load soak; not the beta's one-hour gate.

uv run --no-project --with neo4j==5.28.6 python this_file.py --seconds 600
The default release binary opens disposable data only. Runs one batch writer,
four readers and samples native server RSS. Stop, reopen and audit after load.
"""
import argparse
from collections import deque
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import socket
import statistics
import subprocess
import tempfile
import threading
import time

from neo4j import GraphDatabase

ROOT = Path(__file__).resolve().parents[3]
STATE = "CALL db.commitState() YIELD sequence RETURN sequence"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--seconds", type=int, default=600)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/brainwire")
    args = parser.parse_args()
    if not 10 <= args.seconds <= 3600:
        parser.error("seconds must be between 10 and 3600")
    binary = args.binary.resolve()
    result = {"profile": "release" if binary.parent.name == "release" else "debug",
              "driver": "5.28.6", "synthetic_nodes": 100000, "readers": 4,
              "writers": 1, "requested_seconds": args.seconds,
              "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest()}
    with tempfile.TemporaryDirectory(prefix="brainwire-mixed-soak-") as tmp:
        env = {k: v for k, v in os.environ.items() if not k.startswith("BRAINWIRE_")}
        env.update(BRAINWIRE_SERVER_DATA_DIR=tmp + "/data", BRAINWIRE_MEMORY_CACHE="64MiB",
                   BRAINWIRE_MEMORY_MEMTABLES="16MiB", BRAINWIRE_SERVER_DEFAULT_DATABASE="research")
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            address = "%s:%s" % sock.getsockname()
        env["BRAINWIRE_SERVER_LISTEN"] = address
        server = None
        log = open(Path(tmp) / "server.log", "w+")
        stop = threading.Event()
        rss = []

        def cli(*parts):
            output = subprocess.run([str(binary), *parts], cwd=ROOT, env=env,
                                    capture_output=True, text=True, timeout=60)
            if output.returncode:
                raise RuntimeError(output.stderr)
            return output.stdout

        def start():
            nonlocal server
            server = subprocess.Popen([str(binary), "serve"], cwd=ROOT, env=env, stdout=log, stderr=log)
            for _ in range(100):
                if server.poll() is not None:
                    log.seek(0)
                    raise RuntimeError(log.read())
                try:
                    cli("probe", "--address", address, "--timeout-ms", "100")
                    return
                except RuntimeError:
                    time.sleep(.03)
            raise RuntimeError("server readiness timeout")

        def kill():
            nonlocal server
            if server is not None:
                server.kill()
                server.wait(timeout=5)
                server = None

        def state(session):
            return int(session.run(STATE).single(strict=True)["sequence"])

        try:
            cli("db", "create", "research", "--ontology", str(ROOT / "crates/core/tests/fixtures/graph_mapping.json"))
            start()
            with GraphDatabase.driver("bolt://" + address, auth=None) as driver:
                with driver.session(database="research") as session:
                    for offset in range(0, 100000, 250):
                        session.run("UNWIND $rows AS row CREATE (c:Customer {entity_id:row.id, email:row.email, segment:'initial'})",
                                    rows=[{"id": f"soak-{i}", "email": f"soak-{i}@example.test"}
                                          for i in range(offset, offset + 250)]).consume()
                    initial_sequence = state(session)
                barrier = threading.Barrier(5)

                def writer():
                    durations = deque(maxlen=10000)
                    batches = 0
                    try:
                        with driver.session(database="research") as session:
                            barrier.wait(timeout=10)
                            while not stop.is_set():
                                offset = (batches * 250) % 100000
                                segment = f"version-{(batches // 400) % 4}"
                                started = time.perf_counter()
                                session.run("UNWIND $rows AS row MATCH (c:Customer {entity_id:row.id}) SET c.segment=$segment",
                                            rows=[{"id": f"soak-{i}"} for i in range(offset, offset + 250)],
                                            segment=segment).consume()
                                durations.append((time.perf_counter() - started) * 1000)
                                batches += 1
                            final = state(session)
                            assert final == initial_sequence + batches
                        return {"batches": batches, "updated_rows": batches * 250, "final_sequence": str(final),
                                "latency_last_10000_median_ms": statistics.median(durations),
                                "latency_last_10000_p95_ms": sorted(durations)[int(len(durations) * .95) - 1]}
                    finally:
                        stop.set()

                def reader(index):
                    count = 0
                    durations = deque(maxlen=10000)
                    previous = initial_sequence
                    try:
                        with driver.session(database="research") as session:
                            barrier.wait(timeout=10)
                            while not stop.is_set():
                                customer = (count * 997 + index * 1009) % 100000
                                started = time.perf_counter()
                                row = session.run("MATCH (c:Customer {entity_id:$id}) RETURN c.email AS email, c.segment AS segment",
                                                  id=f"soak-{customer}").single(strict=True)
                                assert row["email"] == f"soak-{customer}@example.test"
                                assert row["segment"] == "initial" or row["segment"] in {f"version-{i}" for i in range(4)}
                                durations.append((time.perf_counter() - started) * 1000)
                                count += 1
                                if count % 100 == 0:
                                    current = state(session)
                                    assert current >= previous
                                    previous = current
                        return {"queries": count, "latency_last_10000_median_ms": statistics.median(durations),
                                "latency_last_10000_p95_ms": sorted(durations)[int(len(durations) * .95) - 1]}
                    finally:
                        stop.set()

                started = time.monotonic()
                with concurrent.futures.ThreadPoolExecutor(max_workers=5) as pool:
                    writing = pool.submit(writer)
                    reading = [pool.submit(reader, i) for i in range(4)]
                    try:
                        while time.monotonic() - started < args.seconds and not stop.is_set():
                            value = subprocess.check_output(["ps", "-o", "rss=", "-p", str(server.pid)], text=True)
                            rss.append({"elapsed_seconds": round(time.monotonic() - started, 3), "rss_kib": int(value.strip())})
                            stop.wait(5)
                    finally:
                        stop.set()
                    result["writer"] = writing.result()
                    result["reader_results"] = [future.result() for future in reading]
                result["load_seconds"] = time.monotonic() - started
                assert result["load_seconds"] >= args.seconds
                with driver.session(database="research") as session:
                    assert session.run("MATCH (c:Customer) RETURN count(*) AS n").single()["n"] == 100000
                    # Every customer belongs to exactly one indexed segment.
                    counts = session.run("MATCH (c:Customer) RETURN c.segment AS segment, count(*) AS n").data()
                    assert sum(row["n"] for row in counts) == 100000
                    for group in counts:
                        indexed = session.run("MATCH (c:Customer {segment:$segment}) RETURN count(*) AS n",
                                              segment=group["segment"]).single()["n"]
                        assert indexed == group["n"]
                    result["final_segment_counts"] = counts
            kill()  # Native crash recovery after the acknowledged load.
            offline = json.loads(cli("db", "commit-state", "research", "--json"))
            assert offline["sequence"] == result["writer"]["final_sequence"]
            assert json.loads(cli("db", "check", "research", "--json"))["status"] == "ok"
            result["restart_position_and_full_index_audit"] = True
            result["rss_samples"] = rss
            result["rss_peak_mib"] = max(row["rss_kib"] for row in rss) / 1024
            result["rss_first_mib"] = rss[0]["rss_kib"] / 1024
            result["rss_last_mib"] = rss[-1]["rss_kib"] / 1024
            result["configured_cache_mib"] = 64
            result["configured_memtables_mib"] = 16
            result["rss_is_not_an_enforced_budget"] = True
        finally:
            stop.set()
            kill()
            log.close()
    result["test_server_stopped"] = True
    result["temporary_data_removed"] = True
    print(json.dumps(result, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
