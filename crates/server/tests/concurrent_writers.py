"""Concurrent-writer acceptance: many Python processes writing one Brainwire graph over Bolt.

Proves the server accepts simultaneous writers (default 10 processes, one neo4j driver each)
doing upserts, hot overlapping updates, edge creation, DETACH DELETE and re-upserts, that
write conflicts surface only as retryable Neo.TransientError.* (handled by execute_write),
that the final state is exactly what the disjoint/idempotent design predicts, and that
`brainwire db check` is clean after shutdown.

Run with a venv containing neo4j 5.x/6.x:
  python crates/server/tests/concurrent_writers.py --binary target/debug/brainwire
Uses only a temporary synthetic database and always stops its server. Exit code 1 on any failure.
"""

import argparse
from collections import Counter
import json
import multiprocessing
import os
from pathlib import Path
import random
import socket
import subprocess
import tempfile
import time

from neo4j import GraphDatabase

GRAPH = "concurrent"
SET_OWNER = "UNWIND $rows AS row MERGE (n:Customer {entity_id: row.id}) SET n.owner = row.owner, n.x = row.x"
EDGE = ("UNWIND $rows AS row MATCH (a:Customer {entity_id: row.s}), (b:Customer {entity_id: row.t}) "
        "MERGE (a)-[:RELATED {weight: row.w}]->(b)")
DELETE = "UNWIND $ids AS id MATCH (n:Customer {entity_id: id}) DETACH DELETE n"
HOT, POOL, FANOUT, ROUNDS = 200, 300, 3, 5


def chunks(items, size):
    return [items[i:i + size] for i in range(0, len(items), size)]


def work(task):
    """Runs in a worker process: returns (retries, errors) for its list of (query, params) units."""
    port, units = task
    retries, errors = 0, []
    with GraphDatabase.driver(f"bolt://127.0.0.1:{port}", auth=None) as driver:
        with driver.session(database=GRAPH) as session:
            for query, params in units:
                calls = 0

                def tx_fn(tx):
                    nonlocal calls
                    calls += 1
                    tx.run(query, **params).consume()

                try:
                    session.execute_write(tx_fn)
                except Exception as error:  # anything escaping execute_write is a finding
                    errors.append(f"{getattr(error, 'code', type(error).__name__)}: {str(error)[:200]}")
                retries += max(calls - 1, 0)
    return retries, errors


class Harness:
    def __init__(self, port, workers, batch):
        self.port, self.workers, self.batch = port, workers, batch
        self.pool = multiprocessing.Pool(workers)
        self.failures = []
        self.phases = {}

    def phase(self, name, per_worker, rows):
        """Run one unit list per worker concurrently; record seconds, rows/s, retries, errors."""
        started = time.perf_counter()
        results = self.pool.map(work, [(self.port, units) for units in per_worker], chunksize=1)
        seconds = time.perf_counter() - started
        errors = [e for _, errs in results for e in errs]
        self.phases[name] = {"seconds": round(seconds, 2), "rows": rows, "rows_per_s": round(rows / seconds),
                             "retries": sum(r for r, _ in results), "errors": len(errors),
                             "error_kinds": dict(Counter(errors).most_common(5))}
        self.check(name, not errors, f"{len(errors)} non-retried errors, e.g. {errors[:3]}")

    def check(self, name, ok, message):
        if not ok:
            self.failures.append(f"{name}: {message}")

    def nodes(self, session):
        rows = session.run("MATCH (n:Customer) RETURN n.entity_id AS id, n.owner AS owner").data()
        ids = Counter(r["id"] for r in rows)
        return ids, {r["id"]: r["owner"] for r in rows}

    def edges(self, session):
        return [(r["s"], r["t"]) for r in session.run(
            "MATCH (a:Customer)-[:RELATED]->(b:Customer) RETURN a.entity_id AS s, b.entity_id AS t").data()]


def upserts(ids, owner, batch):
    return [(SET_OWNER, {"rows": [{"id": i, "owner": owner, "x": n} for n, i in enumerate(part)]})
            for part in chunks(ids, batch)]


def run_phases(h, session, rows):
    w, batch = h.workers, h.batch
    # Phase 1: disjoint upserts.
    per = rows // w
    h.phase("1_disjoint_upserts", [upserts([f"p1-{k}-{i}" for i in range(per)], k, batch) for k in range(w)], per * w)
    ids, _ = h.nodes(session)
    h.check("1_disjoint_upserts", len(ids) == per * w and max(ids.values()) == 1, f"count {len(ids)} != {per * w}")
    base = len(ids)

    # Phase 2: every worker MERGE/SETs the same HOT ids, shuffled per batch, ROUNDS times.
    rng = random.Random(7)
    plans = []
    for k in range(w):
        units = []
        for _ in range(ROUNDS):
            order = [f"hot-{i}" for i in range(HOT)]
            rng.shuffle(order)
            units += upserts(order, k, 50)
        plans.append(units)
    h.phase("2_hot_updates", plans, w * ROUNDS * HOT)
    ids, owners = h.nodes(session)
    hot = {i: c for i, c in ids.items() if i.startswith("hot-")}
    h.check("2_hot_updates", len(hot) == HOT and set(hot.values()) == {1}, f"hot ids {len(hot)}, dup counts {Counter(hot.values())}")
    h.check("2_hot_updates", all(owners[i] in range(w) for i in hot), "hot owner not a worker number")
    h.check("2_hot_updates", len(ids) == base + HOT, f"total {len(ids)} != {base + HOT}")

    # Phase 3: edge creators, deleters and re-upserters at once, on disjoint id sets.
    e_ids, d_ids = [f"e-{i}" for i in range(POOL)], [f"d-{i}" for i in range(POOL)]
    setup = upserts(e_ids + d_ids, -1, batch)
    d_chain = [{"s": d_ids[i], "t": d_ids[(i + 1) % POOL], "w": 1.0} for i in range(POOL)]
    work((h.port, setup + [(EDGE, {"rows": part}) for part in chunks(d_chain, batch)]))
    ee = [{"s": e_ids[i], "t": e_ids[(i + k) % POOL], "w": float(k)} for i in range(POOL) for k in range(1, FANOUT + 1)]
    ed = [{"s": e_ids[i], "t": d_ids[i], "w": 9.0} for i in range(POOL)]  # may race with deletion
    n_edge, n_del = max(w * 4 // 10, 1), max(w * 3 // 10, 1)
    plans = []
    for k in range(w):
        if k < n_edge:
            mine = ee + ed
            rng.shuffle(mine)
            plans.append([(EDGE, {"rows": part}) for part in chunks(mine, 100)])
        elif k < n_edge + n_del:
            slice_ = d_ids[k - n_edge::n_del]
            plans.append([(DELETE, {"ids": part}) for part in chunks(slice_, 25)])
        else:
            plans.append(upserts([f"u-{i}" for i in range(400)] * 2, k, 100))
    h.phase("3_mixed", plans, len(ee + ed) * n_edge + POOL + 800 * (w - n_edge - n_del))
    ids, _ = h.nodes(session)
    edges = h.edges(session)
    h.check("3_mixed", not any(i.startswith("d-") for i in ids), f"{sum(i.startswith('d-') for i in ids)} deleted ids remain")
    h.check("3_mixed", all(ids.get(f"e-{i}") == 1 for i in range(POOL)), "e-pool node missing or duplicated")
    h.check("3_mixed", all(ids.get(f"u-{i}") == 1 for i in range(400)), "u-pool node missing or duplicated")
    dangling = [p for p in edges if p[0] not in ids or p[1] not in ids]
    h.check("3_mixed", not dangling, f"{len(dangling)} edges reference missing nodes, e.g. {dangling[:3]}")
    per_pair = Counter(edges)
    expected = {(r["s"], r["t"]) for r in ee}
    h.check("3_mixed", set(per_pair) == expected and set(per_pair.values()) == {1},
            f"edge pairs {len(per_pair)} vs expected {len(expected)}; max per pair {max(per_pair.values(), default=0)}; "
            f"touching d-: {sum(1 for p in per_pair if 'd-' in p[0] + p[1])}")

    # Phase 4: single-row transactions vs batched transactions, same volume.
    total = 2000
    ids4 = lambda tag: [f"{tag}-{i}" for i in range(total)]
    single = [[u for i in ids4("s")[k::w] for u in upserts([i], k, 1)] for k in range(w)]
    h.phase("4a_single_row", single, total)
    batched = [upserts(ids4("b")[k::w], k, batch) for k in range(w)]
    h.phase("4b_batched", batched, total)
    ids, _ = h.nodes(session)
    h.check("4_compare", sum(i.startswith(("s-", "b-")) for i in ids) == 2 * total, "single/batched row count mismatch")
    return len(ids), len(edges)


def run(binary, rows, workers, batch):
    with tempfile.TemporaryDirectory(prefix="brainwire-concurrent-") as temporary:
        root = Path(temporary)
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        env = {k: v for k, v in os.environ.items() if not k.startswith("BRAINWIRE_")}
        config = root / "brainwire.toml"
        config.write_text(f'''[server]
listen = "127.0.0.1:{port}"
data_dir = "{root / 'data'}"
default_database = "{GRAPH}"
[memory]
cache = "64MiB"
memtables = "4MiB"
query = "32MiB"
''')
        ontology = root / "ontology.json"
        ontology.write_text(json.dumps({"version": "1", "nodes": [{
            "label": "Customer", "properties": ["owner", "x"], "indexed_fields": ["owner"],
        }], "edges": [{"type": "RELATED", "from": {"label": "Customer"},
                       "to": {"label": "Customer"}, "properties": ["weight"]}]}))

        def cli(*args):
            return subprocess.check_output([str(binary), *args, "--config", str(config)], env=env, text=True)

        cli("db", "create", GRAPH, "--ontology", str(ontology))
        process = None
        try:
            with (root / "server.log").open("w") as log:
                process = subprocess.Popen([str(binary), "serve", "--config", str(config)], env=env, stdout=log, stderr=log)
                for _ in range(400):
                    if process.poll() is not None:
                        raise RuntimeError((root / "server.log").read_text())
                    try:
                        with socket.create_connection(("127.0.0.1", port), timeout=0.1):
                            break
                    except OSError:
                        time.sleep(0.025)
                else:
                    raise RuntimeError("server did not start")
                h = Harness(port, workers, batch)
                try:
                    with GraphDatabase.driver(f"bolt://127.0.0.1:{port}", auth=None) as driver:
                        with driver.session(database=GRAPH) as session:
                            final_nodes, final_edges = run_phases(h, session, rows)
                finally:
                    h.pool.terminate()
                process.kill()  # db check needs the store closed
                process.wait(timeout=10)
                process = None
            checked = json.loads(cli("db", "check", GRAPH, "--json"))
            h.check("5_db_check", checked.get("status") == "ok", f"db check reported problems: {json.dumps(checked)[:600]}")
            print(json.dumps({"workers": workers, "rows": rows, "batch_size": batch, "phases": h.phases,
                              "final_nodes": final_nodes, "final_edges": final_edges, "db_check": checked,
                              "failures": h.failures, "result": "FAIL" if h.failures else "PASS"}, indent=2))
            return 1 if h.failures else 0
        finally:
            if process is not None and process.poll() is None:
                process.kill()
                process.wait(timeout=10)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/brainwire"))
    parser.add_argument("--workers", type=int, default=10)
    parser.add_argument("--rows", type=int, default=20000)
    parser.add_argument("--batch-size", type=int, default=500)
    args = parser.parse_args()
    if args.workers < 3 or args.rows < args.workers or args.batch_size < 1:
        parser.error("need workers >= 3, rows >= workers and a positive batch-size")
    raise SystemExit(run(args.binary.resolve(), args.rows, args.workers, args.batch_size))
