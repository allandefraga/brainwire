"""Run a protected, disposable real graph and open its connected TUI.

From the repository root (Rust dependencies must already be cached):
  uv run --no-project --with neo4j==6.3.0 python -m examples.python.local_demo

For an automated rehearsal and five plain-text UI captures:
  uv run --no-project --with neo4j==6.3.0 python -m examples.python.local_demo \
      --snapshot-dir /tmp/brainwire-demo-captures

Only synthetic data is used. This is a standalone RocksDB demo, not quorum/HA.
The server, private credentials and database are removed on exit. Captures and
the verification receipt are retained only in an explicitly supplied new folder.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import time

from neo4j import GraphDatabase
from neo4j.exceptions import ServiceUnavailable, SessionExpired

from examples.python.client import GraphClient
from examples.python.create_service_tokens import create


ROOT = Path(__file__).resolve().parents[2]


def expect(condition, message):
    if not condition:
        raise AssertionError(message)


def run(binary, rows, snapshot_dir, lang):
    started = time.perf_counter()
    digest = hashlib.sha256()
    with binary.open("rb") as stream:
        for block in iter(lambda: stream.read(1 << 20), b""):
            digest.update(block)
    receipt = {"mode": "synthetic-standalone", "nodes": rows, "edges": rows - 1,
               "binary_sha256": digest.hexdigest(), "checks": [], "seconds": {}}
    if snapshot_dir is not None:
        snapshot_dir.mkdir(parents=True, exist_ok=False)
    with tempfile.TemporaryDirectory(prefix="brainwire-local-demo-") as temporary:
        root = Path(temporary)
        env = {key: value for key, value in os.environ.items()
               if not key.startswith("BRAINWIRE_")}
        auth = create(root / "auth", "demo", include_observer=True)
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        address = f"127.0.0.1:{port}"
        config = root / "brainwire.toml"
        config.write_text(f'''[server]
listen = "{address}"
data_dir = {json.dumps(str(root / 'data'))}
default_database = "demo"
auth_file = {json.dumps(str(auth / 'policy.json'))}
max_message = "8MiB"
max_decoded_message = "16MiB"
io_timeout_ms = 300000
[memory]
cache = "64MiB"
memtables = "16MiB"
query = "64MiB"
''')
        ontology = root / "ontology.json"
        ontology.write_text(json.dumps({"version": "1", "nodes": [{
            "label": "Customer", "properties": ["email", "active", "tags"],
            "indexed_fields": ["email", "active"],
        }], "edges": [{"type": "RELATED", "from": {"label": "Customer"},
                       "to": {"label": "Customer"}, "properties": ["weight"]}]}))

        def cli(*args, extra_env=None):
            return subprocess.check_output([str(binary), *args], cwd=ROOT,
                                           env=env if extra_env is None else extra_env,
                                           text=True, timeout=30)

        cli("db", "create", "demo", "--ontology", str(ontology), "--config", str(config))
        writer = json.loads((auth / "writer.json").read_text())
        credentials = (writer["principal"], writer["credentials"])
        process = None

        def stop():
            nonlocal process
            if process is not None:
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=10)
                process = None

        def start(log):
            nonlocal process
            log.seek(0)
            log.truncate()
            log.flush()
            process = subprocess.Popen([str(binary), "serve", "--config", str(config)],
                                       cwd=ROOT, env=env, stdout=log, stderr=log)
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    raise RuntimeError((root / "server.log").read_text())
                if f"brainwire serve {address} " not in (root / "server.log").read_text():
                    time.sleep(0.05)
                    continue
                try:
                    with GraphDatabase.driver(f"bolt://{address}", auth=credentials,
                                              connection_timeout=0.5,
                                              max_transaction_retry_time=0) as driver:
                        driver.verify_connectivity()
                    return
                except (ServiceUnavailable, SessionExpired, OSError):
                    time.sleep(0.05)
            raise RuntimeError("demo server did not become ready within 15 seconds")

        def verify(driver):
            with driver.session(database="demo") as session:
                expect(session.run("MATCH (n:Customer) RETURN count(*) AS n").single()["n"] == rows, "node count")
                expect(session.run("MATCH (a:Customer)-[:RELATED]->(b:Customer) "
                                   "RETURN count(*) AS n").single()["n"] == rows - 1, "edge count")
                expect(session.run("MATCH (n:Customer {email:'updated@example.test'}) "
                                   "RETURN count(*) AS n").single()["n"] == 1, "new indexed value")
                expect(session.run("MATCH (n:Customer {email:'user-0@example.test'}) "
                                   "RETURN count(*) AS n").single()["n"] == 0, "old indexed value removed")
                node = session.run("MATCH (n:Customer {entity_id:'customer-0'}) RETURN n").single()["n"]
                expect("tags" not in node, "property removed")
                indexes = session.run("CALL db.indexes() YIELD label,property,state "
                                      "RETURN label,property,state").data()
                expect(len(indexes) == 2 and all(row["state"] == "scalar_equality_ready"
                                                 for row in indexes), f"ready indexes: {indexes}")

        try:
            with (root / "server.log").open("w") as log:
                start(log)
                print(f"BRAINWIRE | real protected graph on {address} | synthetic data", flush=True)
                with GraphDatabase.driver(f"bolt://{address}", auth=credentials,
                                          liveness_check_timeout=30,
                                          max_transaction_retry_time=0) as driver:
                    graph = GraphClient(driver, "demo", batch_size=250, batch_bytes=1 << 20)
                    began = time.perf_counter()
                    for _ in range(2):
                        written = graph.load_nodes("Customer", (
                            {"entity_id": f"customer-{i}", "email": f"user-{i}@example.test",
                             "active": i % 2 == 0, "tags": ["synthetic", "python"]}
                            for i in range(rows)))
                        expect(written == rows, "batch node write count")
                        written = graph.load_edges("Customer", "RELATED", "Customer", (
                            {"source_id": "customer-0", "target_id": f"customer-{i}", "weight": 0.8}
                            for i in range(1, rows)))
                        expect(written == rows - 1, "batch edge write count")
                    receipt["seconds"]["load_and_replay"] = round(time.perf_counter() - began, 3)
                    updated = graph.update_node("Customer", "customer-0", {}, remove=["tags"])
                    expect(updated, "node property removal")
                    queries = [
                        ("point lookup", "MATCH (n:Customer {entity_id:$id}) RETURN n.entity_id AS id,n.email AS email",
                         {"id": "customer-0"}),
                        ("indexed property filter", "MATCH (n:Customer) WHERE n.email=$email RETURN n.entity_id AS id",
                         {"email": "user-0@example.test"}),
                        ("typed traversal", "MATCH (a:Customer {entity_id:$id})-[:RELATED]->(b:Customer) "
                         "RETURN b.entity_id AS neighbor LIMIT 5", {"id": "customer-0"}),
                        ("relationship count", "MATCH (a:Customer)-[:RELATED]->(b:Customer) RETURN count(*) AS edges", {}),
                        ("parameterized mutation", "MATCH (n:Customer {entity_id:$id}) SET n.email=$email RETURN n.email AS email",
                         {"id": "customer-0", "email": "updated@example.test"}),
                        ("mutation readback", "MATCH (n:Customer {entity_id:$id}) RETURN n.email AS email",
                         {"id": "customer-0"}),
                    ]
                    receipt["cypher"] = []
                    for title, query, params in queries:
                        result = graph.run_cypher(query, params)
                        expect(bool(result), f"Cypher returned no rows: {title}")
                        print(f"\nCYPHER | {title}\n{query}\n"
                              f"params={json.dumps(params)}\n=> {json.dumps(result)}", flush=True)
                        receipt["cypher"].append({"name": title, "query": query,
                                                  "parameters": params, "result": result})
                    verify(driver)
                receipt["checks"].extend(["bounded_python_batches", "merge_replay_no_duplicates",
                                           "property_patch_and_removal", "ready_scalar_indexes", "six_real_cypher_queries"])
                print(f"PASS | {rows:,} nodes + {rows - 1:,} edges | replay, patch, removal, indexes", flush=True)
                stop()
                start(log)
                with GraphDatabase.driver(f"bolt://{address}", auth=credentials,
                                          max_transaction_retry_time=0) as driver:
                    verify(driver)
                receipt["checks"].append("retained_disk_restart_readback")
                print("PASS | stopped and reopened the same RocksDB graph; data and indexes survived", flush=True)
                shell = ["shell", "--connect", address, "--graph", "demo",
                         "--node-label", "Customer", "--entity-id", "customer-0",
                         "--edge-type", "RELATED", "--target-label", "Customer", "--lang", lang]
                observer_env = {**env, "BRAINWIRE_BOLT_AUTH_FILE": str(auth / "observer.json")}
                if snapshot_dir is not None:
                    for view in ("ontology", "memory", "indexes", "graph"):
                        frame = cli(*shell, "--view", view, "--snapshot", "--width", "160",
                                    "--height", "48", extra_env=observer_env)
                        (snapshot_dir / f"{view}.txt").write_text(frame)
                    receipt["checks"].append("four_real_connected_tui_snapshots")
                else:
                    print("TUI | 1 Ontology / 2 Memory / 3 Indexes / 4 Graph | r refresh | q exit", flush=True)
                    subprocess.run([str(binary), *shell], cwd=ROOT, env=observer_env, check=True)
                stop()
                audit = json.loads(cli("db", "check", "demo", "--json", "--config", str(config)))
                expect(audit.get("status") == "ok", f"offline integrity audit: {audit}")
                receipt["offline_audit"] = audit
                receipt["checks"].append("offline_integrity_audit")
                if snapshot_dir is not None:
                    frame = cli("shell", "--overview", "--graph", "demo", "--config", str(config),
                                "--view", "disk", "--snapshot", "--width", "160", "--height", "48",
                                "--lang", lang)
                    (snapshot_dir / "disk-offline.txt").write_text(frame)
                    receipt["checks"].append("real_offline_disk_tui_snapshot")
        finally:
            stop()
        receipt["status"] = "pass"
        receipt["seconds"]["total"] = round(time.perf_counter() - started, 3)
        if snapshot_dir is not None:
            (snapshot_dir / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
            print(f"Captures and verification receipt: {snapshot_dir.resolve()}", flush=True)
    print("PASS | server stopped; disposable data and credentials removed", flush=True)
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--binary", type=Path, help="Use an already-built binary; otherwise build debug offline")
    parser.add_argument("--nodes", type=int, default=1000, help="Synthetic nodes, 2..100000 (default 1000)")
    parser.add_argument("--snapshot-dir", type=Path, help="Create a new capture folder instead of opening the interactive TUI")
    parser.add_argument("--lang", choices=("en", "pt-BR"), default="en")
    args = parser.parse_args()
    if not 2 <= args.nodes <= 100000:
        parser.error("--nodes must be between 2 and 100000")
    if args.snapshot_dir is None and (not sys.stdin.isatty() or not sys.stdout.isatty()):
        parser.error("interactive TUI needs a terminal; use --snapshot-dir NEW_DIRECTORY for automated runs")
    if args.binary is None:
        subprocess.run(["cargo", "build", "--locked", "--offline", "-p", "brainwire-server"],
                       cwd=ROOT, check=True)
    binary = (args.binary or ROOT / "target/debug/brainwire").resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        parser.error(f"binary is not executable: {binary}")
    def interrupted(_signum, _frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, interrupted)
    run(binary, args.nodes, args.snapshot_dir, args.lang)


if __name__ == "__main__":
    main()
