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

from neo4j import GraphDatabase
from neo4j.exceptions import Neo4jError, ServiceUnavailable, SessionExpired

UPSERT = """
UNWIND $rows AS row
MERGE (c:Customer {entity_id: row.id})
SET c.email = row.email, c.active = row.active, c.tags = row.tags,
    c.embedding = row.embedding
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
    # Compare indexed lookup with the scan path for a remaining email.
    indexed = count(session, "{email:'user-3@example.test'}")
    scanned = session.run("MATCH (c:Customer) WHERE c.email='user-3@example.test' RETURN count(*) AS n").single()["n"]
    assert indexed == scanned == 1


def run(binary, rows, batch_size):
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
query = "32MiB"
''')
        ontology = root / "ontology.json"
        ontology.write_text(json.dumps({"version": "1", "nodes": [{
            "label": "Customer", "properties": ["email", "active", "tags", "embedding"],
            "indexed_fields": ["email", "active"],
        }]}))

        def cli(*args):
            return subprocess.check_output([str(binary), *args, "--config", str(config)], env=env, text=True)

        for graph in ("ingestion", "isolated", "example"):
            cli("db", "create", graph, "--ontology", str(ontology))
        process = None
        durations = []
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
                                                  "tags": ["synthetic", "python"], "embedding": [0.25, 0.5, 0.75]}
                                                 for i in range(first, min(first + batch_size, rows))]
                                        session.execute_write(lambda tx, values: tx.run(UPSERT, rows=values).consume(), batch)
                                    durations.append(time.perf_counter() - started)
                                    assert count(session, "") == rows, repetition
                                with session.begin_transaction() as tx:
                                    tx.run("MATCH (c:Customer {entity_id:'id-0'}) SET c.email='rolled-back'").consume()
                                    assert count(tx, "{email:'rolled-back'}") == 1
                                    tx.rollback()
                                session.run("MATCH (c:Customer {entity_id:'id-0'}) SET c.email='changed@example.test'").consume()
                                session.run("MATCH (c:Customer {entity_id:'id-1'}) SET c.email=null").consume()
                                session.run("MATCH (c:Customer {entity_id:'id-2'}) DETACH DELETE c").consume()
                            verify(session, rows)
                        with driver.session(database="example") as session:
                            assert count(session, "") == 2
                            assert count(session, "{email:'ada@example.test'}") == 1
                        with driver.session(database="isolated") as session:
                            assert count(session, "") == 0
                        # Kill with an explicitly open, uncommitted indexed overwrite.
                        pending_session = driver.session(database="ingestion")
                        pending = pending_session.begin_transaction()
                        pending.run("MATCH (c:Customer {entity_id:'id-3'}) SET c.email='uncommitted-crash'").consume()
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
            assert graph["secondary_index_status"] == "string_boolean_ready"
            assert graph["nodes"] == rows - 1
            assert "idx" in graph["storage"]
            print(json.dumps({"rows": rows, "batch_size": batch_size, "insert_seconds": durations[0],
                              "replay_seconds": durations[1], "recovered_nodes": graph["nodes"],
                              "sst_files_before_crash": sst_before_crash,
                              "driver": __import__("neo4j").__version__, "result": "PASS"}, indent=2))
        finally:
            if process is not None and process.poll() is None:
                process.kill()
                process.wait(timeout=10)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/brainwire"))
    parser.add_argument("--rows", type=int, default=5000)
    parser.add_argument("--batch-size", type=int, default=250)
    args = parser.parse_args()
    if args.rows < 4 or args.batch_size < 1:
        parser.error("rows must be >= 4 and batch-size must be positive")
    run(args.binary.resolve(), args.rows, args.batch_size)
