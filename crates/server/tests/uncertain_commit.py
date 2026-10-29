"""Unknown COMMIT outcome rehearsal against an owned protected native server.

Stage an explicit transaction, send COMMIT without reading its acknowledgement,
then SIGKILL only this harness's server. Reconcile every node, edge and indexed
value after restart. The commit may have succeeded: never retry it blindly.
This tests process-crash atomicity, not power loss or an exact fsync kill hook.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time

from compose_auth import DATABASE, fixture
from termination_recovery import HANDSHAKE, exact, message, request

ROOT = Path(__file__).resolve().parents[3]
CODE_SUCCESS = bytes.fromhex("b170")
CODE_FAILURE = bytes.fromhex("b17f")


def classify_batch(actual_nodes, actual_edges, expected_nodes, expected_edges,
                   actual_root_email, before_email, committed_email):
    """Return known recovered state; reject partial or contradictory mutations."""
    if actual_nodes == expected_nodes:
        assert actual_edges == expected_edges, "committed nodes have partial or incorrect edges"
        assert actual_root_email == committed_email, "committed nodes lack indexed overwrite"
        return "committed"
    assert actual_nodes == [], "uncertain batch contains partial or incorrect nodes"
    assert actual_edges == [], "absent nodes have committed edges"
    assert actual_root_email == before_email, "absent nodes have committed indexed overwrite"
    return "not_committed"


def self_check():
    nodes = [{"id": "uncertain-0", "email": "uncertain-0@example.test"}]
    edges = [{"source": "uncertain-0", "target": "root"}]
    assert classify_batch(nodes, edges, nodes, edges, "new", "old", "new") == "committed"
    assert classify_batch([], [], nodes, edges, "old", "old", "new") == "not_committed"
    for actual_nodes, actual_edges, email in ((nodes, [], "new"), ([], [], "new"),
                                             (nodes, edges, "old"), ([], edges, "old"),
                                             (nodes + nodes, edges, "new")):
        try:
            classify_batch(actual_nodes, actual_edges, nodes, edges, email, "old", "new")
        except AssertionError:
            pass
        else:
            raise AssertionError("partial state accepted by reconciliation")
    return {"reconciliation_check": "PASS", "runtime_exercised": False}


def run(binary, rows, delays):
    from neo4j import GraphDatabase, __version__ as driver_version

    report = {"binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
              "driver": driver_version, "synthetic": True, "rows_per_batch": rows,
              "scope": "native protected single-node COMMIT submitted without acknowledgement; process SIGKILL, not power loss or exact commit-phase injection",
              "cycles": []}
    with tempfile.TemporaryDirectory(prefix="brainwire-uncertain-commit-") as temporary:
        root = Path(temporary).resolve()
        root.chmod(0o700)
        tokens, policy, health = fixture()
        for name, value in (("policy.json", policy), ("health.json", health)):
            target = root / name
            target.write_text(json.dumps(value))
            target.chmod(0o600)
        ontology = root / "schema.json"
        ontology.write_text(json.dumps({"version": "1", "nodes": [{
            "label": "Customer", "properties": ["email", "epoch"],
            "indexed_fields": ["email", "epoch"]}], "edges": [{
                "type": "RELATED", "from": {"label": "Customer"},
                "to": {"label": "Customer"}, "properties": []}]}))
        with socket.socket() as reserved:
            reserved.bind(("127.0.0.1", 0))
            address = reserved.getsockname()
        endpoint = "%s:%s" % address
        env = {k: v for k, v in os.environ.items() if not k.startswith("BRAINWIRE_")}
        env.update(BRAINWIRE_SERVER_DATA_DIR=str(root / "data"),
                   BRAINWIRE_SERVER_LISTEN=endpoint, BRAINWIRE_SERVER_DEFAULT_DATABASE=DATABASE,
                   BRAINWIRE_SERVER_AUTH_FILE=str(root / "policy.json"),
                   BRAINWIRE_BOLT_AUTH_FILE=str(root / "health.json"),
                   BRAINWIRE_MEMORY_CACHE="64MiB", BRAINWIRE_MEMORY_MEMTABLES="16MiB",
                   BRAINWIRE_MEMORY_QUERY="64MiB", BRAINWIRE_SERVER_MAX_MESSAGE="8MiB")
        process = None
        raw = None
        log = (root / "server.log").open("w")

        def cli(*args):
            result = subprocess.run([str(binary), *args], env=env, cwd=root,
                                    capture_output=True, text=True, timeout=30)
            if result.returncode:
                # No raw protocol errors or authentication payloads are emitted.
                raise RuntimeError("owned CLI failed: " + args[0])
            return result.stdout

        def start():
            nonlocal process
            process = subprocess.Popen([str(binary), "serve"], env=env, cwd=root,
                                       stdout=log, stderr=log)
            for _ in range(200):
                assert process.poll() is None, "owned server exited before readiness"
                try:
                    cli("probe", "--address", endpoint, "--timeout-ms", "100")
                    return
                except RuntimeError:
                    time.sleep(.025)
            raise AssertionError("owned protected server readiness timeout")

        def stop():
            nonlocal process
            if process is not None:
                process.terminate()
                assert process.wait(timeout=12) == 0, "owned clean shutdown failed"
                process = None

        def query(text, params=None, writer=False):
            name = "writer" if writer else "reader"
            with GraphDatabase.driver("bolt://" + endpoint, auth=(name, tokens[name]),
                                      connection_timeout=3, max_transaction_retry_time=0) as driver:
                with driver.session(database=DATABASE) as session:
                    return session.run(text, params or {}).data()

        def raw_begin():
            connection = socket.create_connection(address, timeout=5)
            connection.settimeout(10)
            try:
                connection.sendall(HANDSHAKE)
                assert exact(connection, 4) == bytes.fromhex("00000405")
                request(connection, 0x01, {"user_agent": "brainwire-uncertain-commit"})
                assert message(connection)[:2] == CODE_SUCCESS
                request(connection, 0x6a, {"scheme": "basic", "principal": "writer",
                                           "credentials": tokens["writer"]})
                assert message(connection)[:2] == CODE_SUCCESS
                request(connection, 0x11, {"db": DATABASE})
                assert message(connection)[:2] == CODE_SUCCESS
                return connection
            except Exception:
                connection.close()
                raise

        def raw_run(text, params):
            request(raw, 0x10, text, params, {})
            assert message(raw)[:2] == CODE_SUCCESS, "raw RUN rejected"
            request(raw, 0x3f, {"n": -1})
            while True:
                payload = message(raw)
                assert payload[:2] != CODE_FAILURE, "raw PULL rejected"
                if payload[:2] == CODE_SUCCESS:
                    return
                assert payload[:2] == bytes.fromhex("b171"), "unexpected raw response"

        def nodes(epoch):
            return query("MATCH (c:Customer) WHERE c.epoch=$epoch RETURN c.entity_id AS id,c.email AS email ORDER BY id", {"epoch": epoch})

        def edges(epoch):
            return query("MATCH (c:Customer)-[:RELATED]->(r:Customer) WHERE c.epoch=$epoch RETURN c.entity_id AS source,r.entity_id AS target ORDER BY source", {"epoch": epoch})

        try:
            cli("db", "create", DATABASE, "--ontology", str(ontology))
            start()
            root_email = "root-initial@example.test"
            query("CREATE (c:Customer {entity_id:'root',email:$email,epoch:-2})", {"email": root_email}, True)
            acknowledged = [{"id": f"ack-{i:04d}", "email": f"ack-{i:04d}@example.test"} for i in range(rows)]
            query("UNWIND $rows AS row CREATE (c:Customer {entity_id:row.id,email:row.email,epoch:-1})", {"rows": acknowledged}, True)
            query("MATCH (c:Customer),(r:Customer {entity_id:'root'}) WHERE c.epoch=-1 CREATE (c)-[:RELATED]->(r)", writer=True)
            acknowledged_edges = [{"source": row["id"], "target": "root"} for row in acknowledged]
            assert nodes(-1) == acknowledged and edges(-1) == acknowledged_edges
            reconciled = {-1: (acknowledged, acknowledged_edges)}
            for cycle, delay in enumerate(delays):
                candidate = f"root-cycle-{cycle}@example.test"
                batch = [{"id": f"uncertain-{cycle}-{i:04d}",
                          "email": f"uncertain-{cycle}-{i:04d}@example.test"} for i in range(rows)]
                expected_edges = [{"source": row["id"], "target": "root"} for row in batch]
                raw = raw_begin()
                raw_run("UNWIND $rows AS row CREATE (c:Customer {entity_id:row.id,email:row.email,epoch:$epoch})",
                        {"rows": batch, "epoch": cycle})
                raw_run("MATCH (c:Customer {entity_id:'root'}) SET c.email=$email", {"email": candidate})
                raw_run("MATCH (c:Customer),(r:Customer {entity_id:'root'}) WHERE c.epoch=$epoch CREATE (c)-[:RELATED]->(r)", {"epoch": cycle})
                assert nodes(cycle) == [] and edges(cycle) == [], "reader observed staged batch"
                assert query("MATCH (c:Customer {entity_id:'root'}) RETURN c.email AS email") == [{"email": root_email}]
                request(raw, 0x12)  # COMMIT submitted; no acknowledgement is read.
                sent = time.monotonic()
                if delay:
                    time.sleep(delay)
                process.kill()
                code = process.wait(timeout=10)
                actual_delay = time.monotonic() - sent
                process = None
                raw.close()
                raw = None
                assert code < 0, "owned server was not killed by a signal"
                start()
                actual_nodes, actual_edges = nodes(cycle), edges(cycle)
                root_value = query("MATCH (c:Customer {entity_id:'root'}) RETURN c.email AS email")[0]["email"]
                outcome = classify_batch(actual_nodes, actual_edges, batch, expected_edges,
                                         root_value, root_email, candidate)
                expected_candidate = [{"id": "root"}] if outcome == "committed" else []
                expected_old = [] if outcome == "committed" else [{"id": "root"}]
                assert query("MATCH (c:Customer {email:$email}) RETURN c.entity_id AS id", {"email": candidate}) == expected_candidate
                assert query("MATCH (c:Customer {email:$email}) RETURN c.entity_id AS id", {"email": root_email}) == expected_old
                for row in batch:
                    expected = [{"id": row["id"]}] if outcome == "committed" else []
                    assert query("MATCH (c:Customer {email:$email}) RETURN c.entity_id AS id", {"email": row["email"]}) == expected
                for epoch, (known_nodes, known_edges) in reconciled.items():
                    assert nodes(epoch) == known_nodes and edges(epoch) == known_edges, "previously acknowledged or reconciled state changed"
                reconciled[cycle] = (actual_nodes, actual_edges)
                if outcome == "committed":
                    root_email = candidate
                report["cycles"].append({"cycle": cycle, "requested_delay_seconds": delay,
                                         "signal_wait_seconds": round(actual_delay, 6),
                                         "acknowledgement_consumed": False, "recovered_outcome": outcome,
                                         "reconciled_nodes": len(actual_nodes), "reconciled_edges": len(actual_edges)})
            stop()
            report["audit"] = json.loads(cli("db", "check", DATABASE, "--json"))
            assert report["audit"]["status"] == "ok"
            report["result"] = "PASS"
        finally:
            if raw is not None:
                raw.close()
            if process is not None:
                if process.poll() is None:
                    process.kill()
                process.wait(timeout=10)
            log.close()
    report["owned_resources_removed"] = not root.exists()
    assert report["owned_resources_removed"]
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/brainwire")
    parser.add_argument("--rows", type=int, default=32)
    parser.add_argument("--self-check", action="store_true")
    options = parser.parse_args()
    if not 1 <= options.rows <= 100:
        parser.error("rows must be between 1 and 100 for the bounded raw protocol fixture")
    result = self_check() if options.self_check else run(options.binary.resolve(strict=True), options.rows, (0, .001, .01))
    print(json.dumps(result, indent=2))
