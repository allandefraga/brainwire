"""Disposable synthetic Bolt/restart/checkpoint acceptance for commit state.

uv run --no-project --with neo4j==5.28.6 python this_file.py
Build brainwire-server first. No production directories or remote servers used.
"""
import argparse
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import platform
import socket
import statistics
import subprocess
import tempfile
import time

from neo4j import GraphDatabase
from neo4j.exceptions import ServiceUnavailable, SessionExpired

ROOT = Path(__file__).resolve().parents[3]
BINARY = ROOT / "target/debug/brainwire"
STATE = "CALL db.commitState() YIELD sequence, tracked, scope RETURN sequence, tracked, scope"
WRITE = "UNWIND $rows AS row MERGE (c:Customer {entity_id:row.id}) SET c.email=row.email, c.segment=row.segment"
COUNT = "MATCH (c:Customer) RETURN count(*) AS n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=BINARY)
    parser.add_argument("--rows", type=int, default=10000)
    parser.add_argument("--payload-bytes", type=int, default=0)
    args = parser.parse_args()
    binary = args.binary.resolve()
    total = args.rows
    if total < 250 or total % 250:
        parser.error("rows must be a positive multiple of 250")
    if not 0 <= args.payload_bytes <= 4096:
        parser.error("payload-bytes must be between 0 and 4096")
    write_query = WRITE + (", c.name=row.name" if args.payload_bytes else "")
    result = {"profile": "release" if binary.parent.name == "release" else "debug", "host": platform.platform(), "driver": "5.28.6",
              "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
              "synthetic_nodes": total, "batch_size": 250, "name_payload_bytes": args.payload_bytes}
    with tempfile.TemporaryDirectory(prefix="brainwire-commit-state-") as tmp:
        data = Path(tmp) / "data"
        data.mkdir()
        env = {k: v for k, v in os.environ.items() if not k.startswith("BRAINWIRE_")}
        env.update(BRAINWIRE_SERVER_DATA_DIR=str(data), BRAINWIRE_SERVER_DEFAULT_DATABASE="research",
                   BRAINWIRE_MEMORY_CACHE="64MiB", BRAINWIRE_MEMORY_MEMTABLES="16MiB")
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            address = "%s:%s" % sock.getsockname()
        env["BRAINWIRE_SERVER_LISTEN"] = address
        server = None
        log = open(Path(tmp) / "server.log", "w+")

        def cli(*args):
            completed = subprocess.run([str(binary), *args], cwd=ROOT, env=env,
                                       capture_output=True, text=True, timeout=30)
            if completed.returncode:
                raise RuntimeError(completed.stderr)
            return completed.stdout

        def start():
            nonlocal server
            server = subprocess.Popen([str(binary), "serve"], cwd=ROOT, env=env,
                                      stdout=log, stderr=log)
            for _ in range(100):
                if server.poll() is not None:
                    log.seek(0)
                    raise RuntimeError(log.read())
                try:
                    cli("probe", "--address", address, "--timeout-ms", "100")
                    return
                except RuntimeError:
                    time.sleep(0.03)
            raise RuntimeError("test server readiness timed out")

        def stop():
            nonlocal server
            if server is not None:
                server.kill()
                server.wait(timeout=5)
                server = None

        def state(session):
            value = session.run(STATE).single(strict=True).data()
            assert value["tracked"] is True and value["scope"] == "store_snapshot"
            assert isinstance(value["sequence"], str)
            return int(value["sequence"])

        def count(session):
            return session.run(COUNT).single(strict=True)["n"]

        try:
            cli("db", "create", "research", "--ontology", str(ROOT / "crates/core/tests/fixtures/graph_mapping.json"))
            initial = json.loads(cli("db", "commit-state", "research", "--json"))
            assert initial["sequence"] == "1" and initial["tracked"]
            start()
            with GraphDatabase.driver("bolt://" + address, auth=None) as driver:
                with driver.session(database="research") as session, driver.session(database="research") as old_session:
                    old = old_session.begin_transaction()
                    assert state(old) == 1
                    assert count(old) == 0
                    samples = []
                    sequences = []
                    started_all = time.perf_counter()
                    for offset in range(0, total, 250):
                        rows = [{"id": f"synthetic-{i}", "email": f"customer-{i}@example.test", "segment": "original"}
                                for i in range(offset, offset + 250)]
                        if args.payload_bytes:
                            for row in rows:
                                blocks = (hashlib.sha256(f"{row['id']}:{n}".encode()).hexdigest()
                                          for n in range((args.payload_bytes + 63) // 64))
                                row["name"] = "".join(blocks)[:args.payload_bytes]
                        started = time.perf_counter()
                        session.execute_write(lambda tx: tx.run(write_query, rows=rows).consume())
                        samples.append((time.perf_counter() - started) * 1000)
                        sequences.append(state(session))
                    result["ingest_seconds"] = time.perf_counter() - started_all
                    result["rows_per_second_with_state_reads"] = total / result["ingest_seconds"]
                    result["batch_latency_median_ms"] = statistics.median(samples)
                    result["batch_latency_p95_ms"] = sorted(samples)[int(len(samples) * .95) - 1]
                    assert all(a < b for a, b in zip(sequences, sequences[1:]))
                    assert count(session) == total and count(old) == 0 and state(old) == 1
                    old.rollback()
                    if args.payload_bytes:
                        for sample in (0, total - 1):
                            identity = f"synthetic-{sample}"
                            stored = session.run("MATCH (c:Customer {entity_id:$id}) RETURN c.name AS name", id=identity).single(strict=True)["name"]
                            expected = "".join(hashlib.sha256(f"{identity}:{n}".encode()).hexdigest()
                                               for n in range((args.payload_bytes + 63) // 64))[:args.payload_bytes]
                            assert stored == expected and len(stored.encode()) == args.payload_bytes
                        result["payload_roundtrip_verified"] = True
                    before_read = state(session)
                    for _ in range(25):
                        assert state(session) == before_read
                    result["read_only_position_stable"] = True

                def writer(i):
                    with driver.session(database="research") as session:
                        session.execute_write(lambda tx: tx.run(
                            "MATCH (c:Customer {entity_id:$id}) SET c.segment='updated'",
                            id=f"synthetic-{i}").consume())
                        return state(session)

                # Updates need no ID reservations and touch disjoint customers.
                with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
                    acknowledged = list(pool.map(writer, range(20)))
                with driver.session(database="research") as session:
                    assert session.run("MATCH (c:Customer {segment:'updated'}) RETURN count(*) AS n").single()["n"] == 20
                    assert state(session) == before_read + 20
                    result["disjoint_writers_committed"] = 20
                    result["acknowledged_sequence"] = state(session)
                    pending = session.begin_transaction()
                    pending.run("MATCH (c:Customer {entity_id:'synthetic-21'}) SET c.segment='pending'").consume()
                    assert pending.run("MATCH (c:Customer {segment:'pending'}) RETURN count(*) AS n").single()["n"] == 1
                    assert state(pending) == result["acknowledged_sequence"]
                    stop()  # SIGKILL with an uncommitted index update open.
                    try:
                        pending.rollback()
                    except (ServiceUnavailable, SessionExpired):
                        pass
            offline = json.loads(cli("db", "commit-state", "research", "--json"))
            assert int(offline["sequence"]) == result["acknowledged_sequence"]
            audit = json.loads(cli("db", "check", "research", "--json"))
            assert audit["status"] == "ok" and audit["scalar_indexes"] == "checked_active_declarations"
            receipt = json.loads(cli("db", "checkpoint", "research", "--destination", str(data / "copied")))
            copy = json.loads(cli("db", "commit-state", "copied", "--json"))
            assert receipt["commit_sequence"] == copy["sequence"] == offline["sequence"]
            assert receipt["commit_sequence_tracked"] and copy["tracked"]
            result["checkpoint_sequence"] = copy["sequence"]
            start()
            with GraphDatabase.driver("bolt://" + address, auth=None) as driver:
                with driver.session(database="research") as session:
                    assert count(session) == total
                    assert session.run("MATCH (c:Customer {segment:'pending'}) RETURN count(*) AS n").single()["n"] == 0
                    assert state(session) == result["acknowledged_sequence"]
                    session.run("MATCH (c:Customer {entity_id:'synthetic-21'}) SET c.segment='after-restart'").consume()
                    assert state(session) == result["acknowledged_sequence"] + 1
                    result["restarted_sequence"] = state(session)
                with driver.session(database="copied") as session:
                    assert state(session) == result["acknowledged_sequence"]
                    assert count(session) == total
                    assert session.run("MATCH (c:Customer {segment:'after-restart'}) RETURN count(*) AS n").single()["n"] == 0
            result.update(snapshot_stable=True, sigkill_recovery=True, scalar_index_audit=True,
                          checkpoint_independent=True, restart_continues_sequence=True)
        finally:
            stop()
            log.close()
    result["temporary_data_removed"] = True
    result["test_server_stopped"] = True
    print(json.dumps(result, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
