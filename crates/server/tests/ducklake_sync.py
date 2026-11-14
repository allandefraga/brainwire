"""Real-server acceptance for examples/python/ducklake_sync.py.

Builds a synthetic local DuckLake (customers + FOLLOWS edges), starts a private
server, runs a full parallel load, mutates the lake (updates, deletes, inserts,
key and endpoint changes), runs the incremental sync and compares the whole
graph with the lake at that snapshot. Also checks no-op reruns, the source
lease and the offline consistency audit. Needs duckdb with installed ducklake
and sqlite extensions plus the neo4j driver:

    python3 crates/server/tests/ducklake_sync.py --binary target/debug/brainwire --rows 100000
"""
import argparse
import json
import os
import random
import socket
import subprocess
import sys
import tempfile
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from unittest.mock import patch

import duckdb
from neo4j import GraphDatabase
from neo4j.exceptions import Neo4jError

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))
from examples.python import ducklake_sync  # noqa: E402
from examples.python.ducklake_sync import Mapping, claim, release, sync  # noqa: E402

NODES = Mapping.from_json({"table": "main.customers", "label": "Customer", "id": "id",
                           "properties": {"name": "name", "segment": "segment", "score": "score"}})
EDGES = Mapping.from_json({"table": "main.follows", "type": "FOLLOWS",
                           "source": {"label": "Customer", "column": "src"},
                           "target": {"label": "Customer", "column": "dst"}, "properties": ["weight"]})
ONTOLOGY = {"version": "1", "nodes": [
    {"label": "Customer", "properties": ["name", "segment", "score"], "indexed_fields": ["segment"]},
    {"label": "ImportCheckpoint", "properties": ["snapshot", "lease", "contract_fingerprint"], "indexed_fields": []}],
    "edges": [{"type": "FOLLOWS", "from": {"label": "Customer"}, "to": {"label": "Customer"},
               "properties": ["weight"]}]}


def graph_state(session):
    nodes = {r["id"]: (r["name"], r["segment"], r["score"]) for r in session.run(
        "MATCH (c:Customer) RETURN c.entity_id AS id, c.name AS name, c.segment AS segment, c.score AS score")}
    edges = sorted((r["s"], r["t"], r["w"]) for r in session.run(
        "MATCH (a:Customer)-[f:FOLLOWS]->(b:Customer) RETURN a.entity_id AS s, b.entity_id AS t, f.weight AS w"))
    return nodes, edges


def lake_state(writer):
    nodes = {r[0]: tuple(r[1:]) for r in writer.execute("SELECT id, name, segment, score FROM w.customers").fetchall()}
    edges = sorted(writer.execute("SELECT src, dst, weight FROM w.follows").fetchall())
    return nodes, edges


def check_concurrent_claims(driver, preexisting):
    source_id = "existing-claim-race" if preexisting else "new-claim-race"
    contracts = [ducklake_sync._contract_fingerprint(NODES, f"racing-catalog-{i}", "lake", None, True)
                 for i in ((0, 0) if preexisting else (0, 1))]
    expected_snapshot = 7 if preexisting else None
    if preexisting:
        with driver.session(database="lake") as session:
            claim(session, source_id, "previous-run", contract_fingerprint=contracts[0])
            release(session, source_id, "previous-run", None, expected_snapshot, contract_fingerprint=contracts[0])
    cursor = ducklake_sync._cursor
    barrier = threading.Barrier(2, timeout=5)

    def synchronized_cursor(tx, source_id):
        state = cursor(tx, source_id)
        barrier.wait()
        return state

    def take(i):
        with driver.session(database="lake") as session:
            try:
                snapshot = claim(session, source_id, f"run-{i}", contract_fingerprint=contracts[i])
                assert snapshot == expected_snapshot
                return i, None
            except Neo4jError as error:
                return i, error.code

    with patch.object(ducklake_sync, "_cursor", synchronized_cursor), ThreadPoolExecutor(2) as pool:
        outcomes = list(pool.map(take, range(2)))

    winners = [i for i, code in outcomes if code is None]
    assert len(winners) == 1, outcomes
    winner = winners[0]
    with driver.session(database="lake") as session:
        row = session.run("MATCH (c:ImportCheckpoint {entity_id:$id}) "
                          "RETURN c.lease AS lease, c.contract_fingerprint AS contract, c.snapshot AS snapshot",
                          id=source_id).single(strict=True)
        assert dict(row) == dict(lease=f"run-{winner}", contract=contracts[winner], snapshot=expected_snapshot), row
        release(session, source_id, f"run-{winner}", expected_snapshot, contract_fingerprint=contracts[winner])
    return next(code for _, code in outcomes if code is not None)


def run(binary, rows, workers):
    rng = random.Random(7)
    report = {}
    with tempfile.TemporaryDirectory(prefix="brainwire-ducklake-sync-") as temporary:
        root = Path(temporary)
        target = f"ducklake:sqlite:{root / 'catalog.sqlite'}"
        writer = duckdb.connect()
        writer.execute("LOAD ducklake")
        writer.execute("LOAD sqlite")
        writer.execute(f"ATTACH '{target}' AS w (DATA_PATH '{root / 'lake'}/')")
        writer.execute("CREATE TABLE w.customers(id VARCHAR, name VARCHAR, segment VARCHAR, score BIGINT)")
        writer.execute("CREATE TABLE w.follows(src VARCHAR, dst VARCHAR, weight DOUBLE)")
        writer.execute(f"""INSERT INTO w.customers SELECT 'c' || i, 'name ' || i,
            'segment-' || (i % 7), i FROM range({rows}) t(i)""")
        writer.execute(f"""INSERT INTO w.follows SELECT 'c' || i, 'c' || ((i * 7919 + 1) % {rows}),
            (i % 100) / 100.0 FROM range({rows}) t(i)""")

        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        env = {k: v for k, v in os.environ.items() if not k.startswith("BRAINWIRE_")}
        config = root / "brainwire.toml"
        config.write_text(f'[server]\nlisten = "127.0.0.1:{port}"\ndata_dir = "{root / "data"}"\n'
                          'default_database = "lake"\n[memory]\ncache = "128MiB"\nmemtables = "32MiB"\n'
                          'query = "256MiB"\n')
        (root / "ontology.json").write_text(json.dumps(ONTOLOGY))

        def cli(*args):
            return subprocess.check_output([str(binary), *args, "--config", str(config)], env=env, text=True)

        cli("db", "create", "lake", "--ontology", str(root / "ontology.json"))
        uri = f"bolt://127.0.0.1:{port}"
        common = dict(lake_target=target, uri=uri, database="lake", source_identity="synthetic-catalog-v1",
                      workers=workers, batch_size=500)
        log = (root / "server.log").open("w")
        process = subprocess.Popen([str(binary), "serve", "--config", str(config)], env=env, stdout=log, stderr=log)
        try:
            for _ in range(200):
                if process.poll() is not None:
                    raise RuntimeError((root / "server.log").read_text())
                try:
                    socket.create_connection(("127.0.0.1", port), timeout=0.1).close()
                    break
                except OSError:
                    time.sleep(0.025)
            report["full_nodes"] = sync(NODES, source_id="customers", **common)
            report["full_edges"] = sync(EDGES, source_id="follows", **common)

            # Mutate: 10% updates, 5% deletes (with their edges), inserts, a key change, endpoint moves.
            ids = rng.sample(range(rows), rows // 5)
            updates, deletes = ids[: rows // 10], ids[rows // 10: rows // 10 + rows // 20]
            writer.execute("UPDATE w.customers SET name = name || ' v2', score = score + 1 "
                           "WHERE id IN (SELECT unnest(?))", [[f"c{i}" for i in updates]])
            gone = [f"c{i}" for i in deletes]
            writer.execute("DELETE FROM w.follows WHERE src IN (SELECT unnest(?)) OR dst IN (SELECT unnest(?))",
                           [gone, gone])
            writer.execute("DELETE FROM w.customers WHERE id IN (SELECT unnest(?))", [gone])
            writer.execute(f"INSERT INTO w.customers SELECT 'n' || i, 'new ' || i, 'segment-new', i "
                           f"FROM range({rows // 20}) t(i)")
            writer.execute("UPDATE w.customers SET id = 'renamed' WHERE id = (SELECT min(id) FROM w.customers "
                           "WHERE id NOT IN (SELECT src FROM w.follows UNION SELECT dst FROM w.follows))")
            writer.execute(f"UPDATE w.follows SET dst = 'n' || (hash(src) % {rows // 20}), weight = 2.0 "
                           f"WHERE src IN (SELECT 'c' || i FROM range(0, {rows}, 50) t(i))")

            report["changes_nodes"] = sync(NODES, source_id="customers", **common)
            report["changes_edges"] = sync(EDGES, source_id="follows", **common)
            report["rerun_nodes"] = sync(NODES, source_id="customers", **common)
            assert report["rerun_nodes"]["upserted"] == report["rerun_nodes"]["deleted"] == 0, report

            with GraphDatabase.driver(uri, auth=None) as driver, driver.session(database="lake") as session:
                started = time.monotonic()
                assert graph_state(session) == lake_state(writer), "graph differs from lake snapshot"
                report["compare_seconds"] = round(time.monotonic() - started, 3)
                edge_contract = ducklake_sync._contract_fingerprint(EDGES, common["source_identity"], "lake", None, True)
                claim(session, "follows", "someone-else", contract_fingerprint=edge_contract)
                try:
                    sync(EDGES, source_id="follows", **common)
                    raise AssertionError("a leased source must refuse a second run")
                except RuntimeError as error:
                    assert "leased" in str(error), error
                try:
                    sync(EDGES, source_id="follows", break_lease=True, full=True, require_endpoints=False, **common)
                    raise AssertionError("explicit recovery flags must still enforce the contract")
                except RuntimeError as error:
                    assert "checkpoint contract mismatch" in str(error), error
                assert sync(EDGES, source_id="follows", break_lease=True, **common)["upserted"] == 0
                session.run("CREATE (:ImportCheckpoint {entity_id:'legacy-empty'})").consume()
                try:
                    sync(NODES, source_id="legacy-empty", break_lease=True, full=True, **common)
                    raise AssertionError("a legacy checkpoint without cursor or lease must be rejected")
                except RuntimeError as error:
                    assert "legacy checkpoint" in str(error), error
                report["checkpoint_contract"] = {
                    "new_claim_conflict": check_concurrent_claims(driver, preexisting=False),
                    "existing_claim_conflict": check_concurrent_claims(driver, preexisting=True),
                }
                session.run("CREATE (:Customer {entity_id:'stray-1'}), (:Customer {entity_id:'stray-2'})").consume()
                session.run("MATCH (a:Customer {entity_id:'stray-1'}), (b:Customer {entity_id:'c1'}) "
                            "MERGE (a)-[:FOLLOWS]->(b)").consume()
                session.run("MATCH (a:Customer {entity_id:'c2'}), (b:Customer {entity_id:'c3'}) "
                            "MERGE (a)-[:FOLLOWS {weight: 9.0}]->(b)").consume()
                report["prune_edges"] = sync(EDGES, source_id="follows", full=True, prune=True, **common)
                report["prune_nodes"] = sync(NODES, source_id="customers", full=True, prune=True, **common)
                assert graph_state(session) == lake_state(writer), "prune did not restore the lake state"
            nodes, edges = lake_state(writer)
            report["final"] = {"nodes": len(nodes), "edges": len(edges)}
        finally:
            process.terminate()
            process.wait(timeout=20)
            log.close()
        audit = json.loads(cli("db", "check", "lake", "--json"))
        assert audit["status"] == "ok", audit
        report["db_check"] = audit["status"]
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/brainwire")
    parser.add_argument("--rows", type=int, default=20000)
    parser.add_argument("--workers", type=int, default=10)
    args = parser.parse_args()
    print(json.dumps(run(args.binary.resolve(), args.rows, args.workers), indent=1))
