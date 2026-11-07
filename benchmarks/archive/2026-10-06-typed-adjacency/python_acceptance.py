"""Synthetic Bolt acceptance on a disposable COPY of the benchmark fixture.

uv run --no-project --with neo4j==5.28.6 python python_acceptance.py FIXTURE
Requires cargo build --locked --offline -p brainwire-server first.
"""
import concurrent.futures
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time

from neo4j import GraphDatabase
from neo4j.exceptions import ServiceUnavailable, SessionExpired

ROOT = Path(__file__).resolve().parents[3]
BINARY = str(ROOT / "target/debug/brainwire")
COUNT = "MATCH (h:Item {entity_id:'hub'})-[:WANTED]->(n:Item) RETURN count(*) AS n"
INGEST = "UNWIND $rows AS row MERGE (n:Item {entity_id:row.id}) SET n.status=row.status MERGE (h:Item {entity_id:'hub'}) MERGE (h)-[:WANTED]->(n)"


def main():
    fixture = Path(sys.argv[1])
    if not (fixture / "CURRENT").is_file():
        raise ValueError("expected a closed, synthetic typed_adjacency fixture")
    with tempfile.TemporaryDirectory(prefix="brainwire-typed-bolt-") as tmp:
        shutil.copytree(fixture, Path(tmp) / "data/perf")
        env = {k: v for k, v in os.environ.items() if not k.startswith("BRAINWIRE_")}
        env.update(BRAINWIRE_SERVER_DATA_DIR=tmp + "/data", BRAINWIRE_MEMORY_CACHE="64MiB", BRAINWIRE_MEMORY_MEMTABLES="16MiB")
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            address = "%s:%s" % sock.getsockname()
        env["BRAINWIRE_SERVER_LISTEN"] = address
        server = None

        def start():
            nonlocal server
            server = subprocess.Popen([BINARY, "serve"], cwd=ROOT, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            for _ in range(100):
                if server.poll() is not None:
                    raise RuntimeError("test server exited before readiness")
                probe = subprocess.run([BINARY, "probe", "--address", address, "--timeout-ms", "100"], cwd=ROOT, env=env, capture_output=True)
                if probe.returncode == 0:
                    return
                time.sleep(0.03)
            raise RuntimeError("test server did not become ready")

        def stop():
            nonlocal server
            if server is not None:
                server.kill()
                server.wait(timeout=5)
                server = None

        def count(session):
            return session.run(COUNT).single(strict=True)["n"]

        try:
            start()
            with GraphDatabase.driver("bolt://" + address, auth=None) as driver:
                with driver.session(database="perf") as session:
                    assert count(session) == 10
                    initial_nodes = session.run("MATCH (n:Item) RETURN count(*) AS n").single()["n"]
                    assert session.run("MATCH (h:Item {entity_id:'hub'})<-[:WANTED]-(n:Item) RETURN count(*) AS n").single()["n"] == 10
                with driver.session(database="perf") as old_session:
                    old = old_session.begin_transaction()
                    assert count(old) == 10  # Establish the snapshot before writes.

                    def writer():
                        with driver.session(database="perf") as session:
                            for batch in range(10):
                                session.run(INGEST, rows=[{"id":f"batch-{batch * 10 + i}", "status":"new"} for i in range(10)]).consume()

                    def reader():
                        with driver.session(database="perf") as session:
                            for _ in range(25):
                                value = count(session)
                                assert 10 <= value <= 110 and value % 10 == 0, value

                    with concurrent.futures.ThreadPoolExecutor(max_workers=5) as pool:
                        futures = [pool.submit(writer)] + [pool.submit(reader) for _ in range(4)]
                        for future in futures:
                            future.result()
                    assert count(old) == 10
                    old.rollback()
                with driver.session(database="perf") as session:
                    assert count(session) == 110
                    # Repeated MERGE updates the same IDs/edges, not duplicates.
                    session.run(INGEST, rows=[{"id":f"batch-{i}", "status":"updated"} for i in range(100)]).consume()
                    assert count(session) == 110
                    assert session.run("MATCH (n:Item) WHERE n.status='updated' RETURN count(*) AS n").single()["n"] == 100
                    tx = session.begin_transaction()
                    tx.run(INGEST, rows=[{"id":"rolled-back", "status":"temporary"}]).consume()
                    assert count(tx) == 111
                    tx.rollback()
                    assert count(session) == 110
                    session.run("MATCH (n:Item {entity_id:'batch-0'}) DETACH DELETE n").consume()
                    assert count(session) == 109
                    samples = []
                    for i in range(35):
                        started = time.perf_counter()
                        assert count(session) == 109
                        if i >= 5:
                            samples.append((time.perf_counter() - started) * 1000)
            # Kill the server with an acknowledged, uncommitted write still open.
            with GraphDatabase.driver("bolt://" + address, auth=None) as driver:
                with driver.session(database="perf") as session:
                    pending = session.begin_transaction()
                    pending.run(INGEST, rows=[{"id":"crashed-pending", "status":"uncommitted"}]).consume()
                    assert count(pending) == 110
                    stop()
                    try:
                        pending.rollback()
                    except (ServiceUnavailable, SessionExpired):
                        pass  # Recovery assertions below establish rollback correctness.
            start()
            with GraphDatabase.driver("bolt://" + address, auth=None) as driver:
                with driver.session(database="perf") as session:
                    assert count(session) == 109
                    assert session.run("MATCH (n:Item {entity_id:'rolled-back'}) RETURN count(*) AS n").single()["n"] == 0
                    assert session.run("MATCH (n:Item {entity_id:'crashed-pending'}) RETURN count(*) AS n").single()["n"] == 0
            stop()
            audit = subprocess.run([BINARY, "db", "check", "perf", "--json"], cwd=ROOT, env=env, check=True, capture_output=True, text=True)
            stats = subprocess.run([BINARY, "db", "stats", "perf", "--json"], cwd=ROOT, env=env, check=True, capture_output=True, text=True)
            final_stats = json.loads(stats.stdout)
            assert final_stats["nodes"] == initial_nodes + 99
            assert final_stats["edges"] == (initial_nodes - 1) * 20 + 20 + 99
            metrics = subprocess.run([BINARY, "db", "metrics", "perf", "--json"], cwd=ROOT, env=env, check=True, capture_output=True, text=True)
            sorted_samples = sorted(samples)
            print(json.dumps({"driver":"neo4j 5.28.6", "initialNodes":initial_nodes, "concurrentReaders":4, "concurrentReadQueries":100, "writeBatches":10, "newNodes":100, "finalSelectedEdges":109, "snapshotIsolation":"passed", "mergeOverwrite":"passed", "rollback":"passed", "detachDelete":"passed", "killRestartRecovery":"passed", "uncommittedCrashRollback":"passed", "postRestartAudit":json.loads(audit.stdout), "postRestartStats":final_stats, "postRestartRocksDBMetrics":json.loads(metrics.stdout), "warmBoltCount":{"p50Ms":sorted_samples[14], "p95Ms":sorted_samples[27], "samplesMs":samples}, "note":"debug localhost acceptance; includes driver/network, not a throughput benchmark"}, indent=2))
        finally:
            stop()


if __name__ == "__main__":
    main()
