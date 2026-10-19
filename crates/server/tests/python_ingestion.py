"""Real Python/Bolt ingestion, indexed updates and SIGKILL recovery acceptance.

Run with a venv containing neo4j 5.x/6.x:
  python crates/server/tests/python_ingestion.py --binary target/debug/brainwire
Uses only temporary synthetic databases and always stops its server.
"""

import argparse
from contextlib import suppress
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))

from neo4j import GraphDatabase
from neo4j.exceptions import Neo4jError, ServiceUnavailable, SessionExpired
from examples.python.client import GraphClient

UPSERT = """
UNWIND $rows AS row
MERGE (c:Customer {entity_id: row.id})
SET c.email = row.email, c.active = row.active, c.tags = row.tags,
    c.embedding = row.embedding, c.segment = row.segment
"""


def count(session, pattern, **params):
    return session.run(f"MATCH (c:Customer {pattern}) RETURN count(*) AS n", **params).single()["n"]


def verify(session, rows):
    assert count(session, "") == rows - 1
    assert count(session, "{email: $email}", email="changed@example.test") == 1
    assert count(session, "{email: $email}", email="user-0@example.test") == 0
    assert count(session, "{email: $email}", email="user-1@example.test") == 0
    assert count(session, "{email: $email}", email="user-2@example.test") == 0
    assert count(session, "{email: $email}", email="rolled-back") == 0
    assert count(session, "{email: $email}", email="uncommitted-crash") == 0
    expected_active = sum(i % 2 == 0 for i in range(rows) if i != 2)
    assert count(session, "{active: true}") == expected_active
    record = session.run("MATCH (c:Customer {entity_id:'id-0'}) RETURN c").single()["c"]
    assert record["tags"] == ["synthetic", "python"]
    assert record["embedding"] == [0.25, 0.5, 0.75]
    # Pattern and WHERE must agree; both can now seek the same ready index.
    indexed = count(session, "{email:'user-3@example.test'}")
    scanned = session.run("MATCH (c:Customer) WHERE c.email='user-3@example.test' RETURN count(*) AS n").single()["n"]
    assert indexed == scanned == 1
    state = session.run("CALL db.indexes() YIELD label, property, state WHERE label='Customer' AND property='email' RETURN state").single()["state"]
    assert state == "scalar_equality_ready", state
    assert session.run("MATCH (c:Customer) WHERE c.segment=0.0 RETURN count(*) AS n").single()["n"] == (rows + 6) // 7
    assert session.run("MATCH (c:Customer) WHERE c.segment=999999 RETURN count(*) AS n").single()["n"] == 0
    path = session.run("CALL graph.shortestPath({source:'id-0',sourceLabel:'Customer',target:'id-0',targetLabel:'Customer'}) YIELD node,depth RETURN node.entity_id AS id,depth").single()
    assert path["id"] == "id-0" and path["depth"] == 0
    reachable = session.run("CALL graph.reachable({source:'id-0', sourceLabel:'Customer', maxDepth:1}) YIELD node, depth RETURN node.entity_id AS id, depth").single()
    assert reachable["id"] == "id-0" and reachable["depth"] == 0
    edges = session.run("MATCH (a:Customer)-[:RELATED]->(b:Customer) "
                        "RETURN a.entity_id AS source,b.entity_id AS target").data()
    edges.sort(key=lambda edge: (edge["source"], edge["target"]))
    assert edges == [
        {"source": "id-3", "target": "id-4"},
        {"source": "id-4", "target": "id-5"},
    ], edges


def run(binary, rows, batch_size, query_mib):
    with tempfile.TemporaryDirectory(prefix="brainwire-python-ingestion-") as temporary:
        root = Path(temporary)
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        # Clear inherited Brainwire overrides so this test cannot touch user data.
        env = {k: v for k, v in os.environ.items() if not k.startswith("BRAINWIRE_")}
        config = root / "brainwire.toml"
        config.write_text(f'''[server]
listen = "127.0.0.1:{port}"
data_dir = "{root / 'data'}"
default_database = "ingestion"
[memory]
cache = "64MiB"
memtables = "4MiB"
query = "{query_mib}MiB"
''')
        ontology = root / "ontology.json"
        ontology.write_text(json.dumps({"version": "1", "nodes": [{
            "label": "Customer", "properties": ["email", "active", "tags", "embedding", "segment"],
            "indexed_fields": ["email", "active", "segment"],
        }], "edges": [{"type": "RELATED", "from": {"label": "Customer"},
                       "to": {"label": "Customer"}, "properties": ["weight", "note"]}]}))

        def cli(*args):
            return subprocess.check_output([str(binary), *args, "--config", str(config)], env=env, text=True)

        for graph in ("ingestion", "isolated", "example"):
            cli("db", "create", graph, "--ontology", str(ontology))
        process = None
        durations = []
        read_timings = {}
        sst_before_crash = []
        try:
            with (root / "server.log").open("w") as log:
                for cycle in range(2):
                    process = subprocess.Popen([str(binary), "serve", "--config", str(config)], env=env, stdout=log, stderr=log)
                    for attempt in range(200):
                        if process.poll() is not None:
                            raise RuntimeError((root / "server.log").read_text())
                        try:
                            with socket.create_connection(("127.0.0.1", port), timeout=0.1):
                                break
                        except OSError:
                            time.sleep(0.025)
                    else:
                        raise RuntimeError("server did not start")
                    if cycle == 0:
                        examples = Path(__file__).resolve().parents[3] / "examples" / "python"
                        for _ in range(2):
                            subprocess.run([sys.executable, str(examples / "ingest_customers.py"),
                                            str(examples / "customers.jsonl"), "--database", "example",
                                            "--uri", f"bolt://127.0.0.1:{port}", "--batch-size", "1"],
                                           check=True, capture_output=True, text=True)
                    with GraphDatabase.driver(f"bolt://127.0.0.1:{port}", auth=None) as driver:
                        driver.verify_connectivity()
                        with driver.session(database="ingestion") as session:
                            if cycle == 0:
                                try:
                                    session.run("UNWIND $rows AS row CREATE (c:Customer {entity_id:row.id, email:row.email})", rows=[
                                        {"id": "duplicate", "email": "must-rollback"},
                                        {"id": "duplicate", "email": "must-rollback"},
                                    ]).consume()
                                except Neo4jError as error:
                                    assert "Constraint" in error.code, error
                                else:
                                    raise AssertionError("duplicate CREATE should fail atomically")
                                assert count(session, "") == 0
                                assert count(session, "{email:'must-rollback'}") == 0
                                for repetition in range(2):
                                    started = time.perf_counter()
                                    for first in range(0, rows, batch_size):
                                        batch = [{"id": f"id-{i}", "email": f"user-{i}@example.test", "active": i % 2 == 0,
                                                  "tags": ["synthetic", "python"], "embedding": [0.25, 0.5, 0.75], "segment": i % 7}
                                                 for i in range(first, min(first + batch_size, rows))]
                                        session.execute_write(lambda tx, values: tx.run(UPSERT, rows=values).consume(), batch)
                                    durations.append(time.perf_counter() - started)
                                    assert count(session, "") == rows, repetition
                                try:
                                    with session.begin_transaction() as tx:
                                        tx.run("MATCH (c:Customer {entity_id:'id-0'}) SET c.email='guard-rollback', c.segment=999999").consume()
                                        tx.run("UNWIND $values AS a UNWIND $values AS b UNWIND $values AS c RETURN a,b,c,$padding", values=list(range(400)), padding="x"*4096).consume()
                                except Neo4jError as error:
                                    assert error.code == "Neo.ClientError.Statement.MemoryLimitExceeded", error
                                else:
                                    raise AssertionError("fanout must exceed the per-RUN allocation guard")
                                assert count(session,"{email:'guard-rollback'}") == 0
                                assert count(session,"{segment:999999}") == 0
                                with session.begin_transaction() as tx:
                                    tx.run("MATCH (c:Customer {entity_id:'id-0'}) SET c.email='rolled-back'").consume()
                                    assert count(tx, "{email:'rolled-back'}") == 1
                                    tx.rollback()
                                session.run("MATCH (c:Customer {entity_id:'id-0'}) SET c.email='changed@example.test'").consume()
                                session.run("MATCH (c:Customer {entity_id:'id-1'}) SET c.email=null").consume()
                                session.run("MATCH (c:Customer {entity_id:'id-2'}) DETACH DELETE c").consume()
                                graph = GraphClient(driver, "ingestion", batch_size=2)
                                assert graph.load_edges("Customer", "RELATED", "Customer", [
                                    {"source_id": "id-3", "target_id": "id-4", "weight": 0.2, "note": "keep-me"},
                                    {"source_id": "id-3", "target_id": "id-4", "weight": 0.8},
                                    {"source_id": "id-4", "target_id": "id-5", "weight": None},
                                ]) == 3
                                try:
                                    graph.load_edges("Customer", "RELATED", "Customer", [
                                        {"source_id": "id-5", "target_id": "id-4", "weight": 0.1, "note": "rollback"},
                                        {"source_id": "id-3", "target_id": "missing", "weight": 9.0},
                                    ])
                                except ValueError as error:
                                    assert "endpoints" in str(error)
                                else:
                                    raise AssertionError("missing edge endpoint must fail the batch")
                            verify(session, rows)
                            if cycle == 1:
                                graph = GraphClient(driver, "ingestion", batch_size=250)
                                cleanup_count = 1000
                                assert graph.load_nodes("Customer", (
                                    {"entity_id": f"cleanup-{index}"}
                                    for index in range(cleanup_count)
                                )) == cleanup_count
                                assert graph.load_edges("Customer", "RELATED", "Customer", (
                                    {"source_id": "id-3", "target_id": "cleanup-0"}
                                    if index == 0 else
                                    {"source_id": f"cleanup-{index-1}", "target_id": f"cleanup-{index}"}
                                    for index in range(cleanup_count)
                                )) == cleanup_count
                                unlink = {"source_id": "id-3", "target_id": "cleanup-0"}
                                assert graph.delete_edges("Customer", "RELATED", "Customer", [
                                    unlink, unlink,
                                    {"source_id": "cleanup-0", "target_id": "id-3"},
                                    {"source_id": "missing", "target_id": "cleanup-0"},
                                ]) == 1
                                assert graph.delete_edges("Customer", "RELATED", "Customer", [unlink]) == 0
                                assert count(session, "{entity_id:'id-3'}") == 1
                                assert count(session, "{entity_id:'cleanup-0'}") == 1
                                assert session.run(
                                    "MATCH (a:Customer {entity_id:'cleanup-0'})-[:RELATED]->"
                                    "(b:Customer {entity_id:'cleanup-1'}) RETURN count(*) AS n"
                                ).single()["n"] == 1
                                delete_started = time.perf_counter()
                                deleted = graph.delete_nodes("Customer", (
                                    f"cleanup-{index}" for index in range(cleanup_count)
                                ))
                                deletion_seconds = time.perf_counter() - delete_started
                                assert deleted == cleanup_count
                                assert graph.delete_nodes("Customer", ["cleanup-999", "cleanup-999"]) == 0
                                assert count(session, "") == rows - 1
                                for entity_id in ("cleanup-0", "cleanup-999"):
                                    assert count(session, f"{{entity_id:'{entity_id}'}}") == 0
                                remaining_from_source = session.run(
                                    "MATCH (a:Customer {entity_id:'id-3'})-[:RELATED]->(b:Customer) "
                                    "RETURN b.entity_id AS target"
                                ).data()
                                assert remaining_from_source == [{"target": "id-4"}]
                            if cycle == 0:
                                for name, query in (
                                    ("primaryPoint", "MATCH (c:Customer {entity_id:$id}) RETURN c.entity_id AS id"),
                                    ("textPoint", "MATCH (c:Customer) WHERE c.email=$email RETURN c.entity_id AS id"),
                                    ("numericGroupCount", "MATCH (c:Customer) WHERE c.segment=0.0 RETURN count(*) AS n"),
                                ):
                                    samples = []
                                    for sample in range(30):
                                        i = 3 + (sample * 7919) % (rows - 3)
                                        started = time.perf_counter()
                                        record = session.run(query, id=f"id-{i}", email=f"user-{i}@example.test").single()
                                        elapsed = (time.perf_counter() - started) * 1000
                                        if name == "numericGroupCount":
                                            assert record["n"] == (rows + 6) // 7
                                        else:
                                            assert record["id"] == f"id-{i}"
                                        if sample >= 5:
                                            samples.append(elapsed)
                                    samples.sort()
                                    read_timings[name] = {"samples": len(samples), "p50Ms": samples[len(samples)//2], "p95Ms": samples[(len(samples)-1)*95//100]}
                        with driver.session(database="example") as session:
                            assert count(session, "") == 2
                            assert count(session, "{email:'ada@example.test'}") == 1
                        with driver.session(database="isolated") as session:
                            assert count(session, "") == 0
                        # Kill with an explicitly open, uncommitted indexed overwrite.
                        pending_session = driver.session(database="ingestion")
                        pending = pending_session.begin_transaction()
                        pending.run("MATCH (c:Customer {entity_id:'id-3'}) SET c.email='uncommitted-crash', c.segment=999999").consume()
                        assert count(pending, "{email:'uncommitted-crash'}") == 1
                        sst_before_crash.append(len(list((root / "data" / "ingestion").glob("*.sst"))))
                        process.kill()  # No graceful flush; acknowledged commits must recover from WAL.
                        process.wait(timeout=10)
                        process = None
                        with suppress(Neo4jError, ServiceUnavailable, SessionExpired):
                            pending.rollback()
                        with suppress(Neo4jError, ServiceUnavailable, SessionExpired):
                            pending_session.close()
            checked = json.loads(cli("db", "check", "ingestion", "--json"))
            assert checked["scalar_indexes"] == "checked_active_declarations"
            overview = json.loads(cli("db", "overview", "--json"))
            graph = next(g for g in overview["graphs"] if g["name"] == "ingestion")
            assert graph["secondary_index_status"] == "scalar_equality_ready"
            assert graph["nodes"] == rows - 1
            assert "idx" in graph["storage"]
            print(json.dumps({"rows": rows, "batch_size": batch_size, "query_guard_mib": query_mib, "insert_seconds": durations[0],
                              "replay_seconds": durations[1], "recovered_nodes": graph["nodes"],
                              "cleanup_nodes": cleanup_count, "delete_seconds": deletion_seconds,
                              "relationship_delete_replay": "PASS",
                              "sst_files_before_crash": sst_before_crash,
                              "read_timings": read_timings, "driver": __import__("neo4j").__version__, "query_guard_rollback": "PASS", "numeric_index_recovery": "PASS", "result": "PASS"}, indent=2))
        finally:
            if process is not None and process.poll() is None:
                process.kill()
                process.wait(timeout=10)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/brainwire"))
    parser.add_argument("--rows", type=int, default=5000)
    parser.add_argument("--batch-size", type=int, default=250)
    parser.add_argument("--query-mib", type=int, default=32)
    args = parser.parse_args()
    if args.rows < 6 or args.batch_size < 1:
        parser.error("rows must be >= 6 and batch-size must be positive")
    if not 1 <= args.query_mib <= 256:
        parser.error("query-mib must be between 1 and 256")
    run(args.binary.resolve(), args.rows, args.batch_size, args.query_mib)
