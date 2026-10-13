"""Owned protected high-degree typed-neighborhood qualification and observation.

Small properties, one Ticket -> HAS_COMMENT -> TicketComment signature, LIMIT20.
No whole-graph, multi-hop, large-document or distributed latency claim.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import re
import socket
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))
from examples.python.create_service_tokens import create

GRAPH = "tickets"
SOURCE = "ticket-high-degree"
QUERY = "MATCH (n:Ticket {entity_id:$id})-[:HAS_COMMENT]->(m:TicketComment) RETURN m.entity_id AS entity_id LIMIT 20"
COUNT = "MATCH (n:Ticket {entity_id:$id})-[:HAS_COMMENT]->(m:TicketComment) RETURN count(m) AS total"


def percentiles(values):
    assert values and all(value >= 0 for value in values)
    ordered = sorted(values)
    return {"requests": len(values), **{name + "_us": ordered[math.ceil(len(values) * percentile) - 1]
            for name, percentile in (("p50", .5), ("p95", .95), ("p99", .99))},
            "min_us": ordered[0], "max_us": ordered[-1]}


def validate(rows, neighbors):
    identifiers = [row["entity_id"] for row in rows]
    assert len(identifiers) == 20 and len(set(identifiers)) == 20, identifiers
    assert all(identifier.startswith("comment-") and len(identifier) == 14
               and identifier[8:].isdigit() and 0 <= int(identifier[8:]) < neighbors
               for identifier in identifiers), identifiers
    return identifiers


def self_check():
    assert percentiles(list(range(1, 101)))["p95_us"] == 95
    assert len(validate([{"entity_id": f"comment-{i:06}"} for i in range(20)], 21)) == 20
    return {"fixture_check": "PASS", "runtime_exercised": False}


def run(binary, nodes, iterations, cli_iterations):
    from neo4j import GraphDatabase, READ_ACCESS, __version__ as driver_version
    from neo4j.exceptions import Neo4jError

    if not 22 <= nodes <= 100000 or not 1 <= iterations <= 2000 or not 0 <= cli_iterations <= 20:
        raise ValueError("nodes 22..100000, iterations 1..2000, cli_iterations 0..20")
    schema = ROOT / "examples/client/ontology.json"
    report = {"binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
              "ontology_sha256": hashlib.sha256(schema.read_bytes()).hexdigest(),
              "driver": driver_version, "os": platform.platform(), "nodes_requested": nodes,
              "edges_requested": nodes - 1, "batch_rows": 250, "query": QUERY,
              "profile": "synthetic small-property high-degree Ticket, localhost, sequential warm Python Bolt LIMIT20",
              "limits": {"cache_mib": 64, "memtables_mib": 16, "query_mib": 64,
                         "encoded_message_mib": 1, "decoded_message_mib": 16}, "checks": []}
    with tempfile.TemporaryDirectory(prefix="brainwire-typed-neighborhood-") as folder:
        path = Path(folder).resolve()
        path.chmod(0o700)
        credentials = create(path / "auth", GRAPH)
        writer_auth = json.loads((credentials / "writer.json").read_text())
        reader_auth = json.loads((credentials / "reader.json").read_text())
        with socket.socket() as reserved:
            reserved.bind(("127.0.0.1", 0))
            address = "%s:%s" % reserved.getsockname()
        env = {key: value for key, value in os.environ.items() if not key.startswith("BRAINWIRE_")}
        env.update(BRAINWIRE_SERVER_DATA_DIR=str(path / "data"), BRAINWIRE_SERVER_LISTEN=address,
                   BRAINWIRE_SERVER_DEFAULT_DATABASE=GRAPH, BRAINWIRE_SERVER_AUTH_FILE=str(credentials / "policy.json"),
                   BRAINWIRE_BOLT_AUTH_FILE=str(credentials / "health.json"), BRAINWIRE_MEMORY_CACHE="64MiB",
                   BRAINWIRE_MEMORY_MEMTABLES="16MiB", BRAINWIRE_MEMORY_QUERY="64MiB",
                   BRAINWIRE_SERVER_MAX_MESSAGE="1MiB", BRAINWIRE_SERVER_MAX_DECODED_MESSAGE="16MiB", NO_COLOR="1")
        server = None
        log = (path / "server.log").open("w")

        def cli(*args, reader=False):
            selected = dict(env, BRAINWIRE_BOLT_AUTH_FILE=str(credentials / "reader.json")) if reader else env
            return subprocess.run([str(binary), *args], env=selected, cwd=ROOT, capture_output=True,
                                  text=True, check=True, timeout=30).stdout

        def connect(auth):
            return GraphDatabase.driver("bolt://" + address, auth=(auth["principal"], auth["credentials"]),
                                       liveness_check_timeout=30, connection_timeout=3,
                                       connection_acquisition_timeout=5, max_transaction_retry_time=0)

        try:
            cli("db", "create", GRAPH, "--ontology", str(schema))
            server = subprocess.Popen([str(binary), "serve"], env=env, cwd=ROOT, stdout=log, stderr=log)
            for _ in range(150):
                assert server.poll() is None, "owned server exited during readiness"
                try:
                    cli("probe", "--address", address, "--timeout-ms", "100")
                    break
                except subprocess.CalledProcessError:
                    time.sleep(.03)
            else:
                raise AssertionError("owned server readiness timeout")
            began = time.perf_counter()
            with connect(writer_auth) as writer:
                with writer.session(database=GRAPH) as session:
                    session.run("CREATE (t:Ticket {entity_id:$id, subject:$subject, status:'open'})",
                                id=SOURCE, subject="Synthetic high-degree neighborhood fixture").consume()
                    for offset in range(0, nodes - 1, 250):
                        rows = [{"entity_id": f"comment-{i:06}", "ticket_entity_id": SOURCE}
                                for i in range(offset, min(offset + 250, nodes - 1))]
                        with session.begin_transaction() as tx:
                            tx.run("UNWIND $rows AS row CREATE (c:TicketComment {entity_id:row.entity_id, ticket_entity_id:row.ticket_entity_id})", rows=rows).consume()
                            tx.run("UNWIND $rows AS row MATCH (t:Ticket {entity_id:row.ticket_entity_id}), (c:TicketComment {entity_id:row.entity_id}) CREATE (t)-[:HAS_COMMENT]->(c)", rows=rows).consume()
                            tx.commit()
            report["import_seconds"] = time.perf_counter() - began
            with connect(reader_auth) as reader, connect(writer_auth) as writer:
                with reader.session(database=GRAPH, default_access_mode=READ_ACCESS) as session:
                    assert session.run("MATCH (n:TicketComment) RETURN count(n) AS total").single()["total"] == nodes - 1
                    assert session.run("MATCH (n:Ticket) RETURN count(n) AS total").single()["total"] == 1
                    assert session.run(COUNT, id=SOURCE).single()["total"] == nodes - 1
                    with session.begin_transaction() as read_tx:
                        first = validate(read_tx.run(QUERY, id=SOURCE).data(), nodes - 1)
                        with writer.session(database=GRAPH) as write_session:
                            pending = write_session.begin_transaction()
                            try:
                                pending.run("CREATE (c:TicketComment {entity_id:'isolation-pending', ticket_entity_id:$id})", id=SOURCE).consume()
                                pending.run("MATCH (t:Ticket {entity_id:$id}), (c:TicketComment {entity_id:'isolation-pending'}) CREATE (t)-[:HAS_COMMENT]->(c)", id=SOURCE).consume()
                                assert pending.run(COUNT, id=SOURCE).single()["total"] == nodes
                                assert read_tx.run(COUNT, id=SOURCE).single()["total"] == nodes - 1
                                assert validate(read_tx.run(QUERY, id=SOURCE).data(), nodes - 1) == first
                            finally:
                                pending.rollback()
                                pending.close()
                        read_tx.commit()
                    report["checks"].append("READ transaction returns 20 unique existing IDs; pending writer node/edge remains invisible and rolls back")
                    try:
                        session.run("CREATE (t:Ticket {entity_id:'reader-denied'})").consume()
                    except Neo4jError as error:
                        assert error.code == "Neo.ClientError.Security.Forbidden", error.code
                    else:
                        raise AssertionError("reader write accepted")
                    assert session.run("MATCH (t:Ticket {entity_id:'reader-denied'}) RETURN t.entity_id").data() == []
                    for _ in range(20):
                        validate(session.run(QUERY, id=SOURCE).data(), nodes - 1)
                    timings = []
                    for _ in range(iterations):
                        began = time.perf_counter_ns()
                        rows = session.run(QUERY, id=SOURCE).data()
                        timings.append(math.ceil((time.perf_counter_ns() - began) / 1000))
                        validate(rows, nodes - 1)
                    report["python_full_request"] = dict(percentiles(timings), errors=0)
                    report["checks"].append("all warm sequential Python requests consume full 20-row result with unique valid IDs and no errors")
            snapshot_args = ["shell", "--connect", address, "--graph", GRAPH, "--view", "graph", "--snapshot",
                             "--node-label", "Ticket", "--entity-id", SOURCE, "--edge-type", "HAS_COMMENT",
                             "--target-label", "TicketComment", "--width", "160", "--height", "40", "--timeout-ms", "5000"]
            text = cli(*snapshot_args, reader=True)
            for expected in (SOURCE, "20 outgoing rows / 20", "MORE AVAILABLE", "comment-"):
                assert expected in text, (expected, text)
            visible_ids = set(re.findall(r"comment-\d{6}", text))
            assert len(visible_ids) == 20
            validate([{"entity_id": identifier} for identifier in visible_ids], nodes - 1)
            report["snapshot"] = {"sha256": hashlib.sha256(text.encode()).hexdigest(), "text": text,
                                  "sample_rows": 20, "more_available": True}
            cli_times = []
            for _ in range(cli_iterations):
                began = time.perf_counter_ns()
                observed = cli(*snapshot_args, reader=True)
                cli_times.append(math.ceil((time.perf_counter_ns() - began) / 1000))
                assert "20 outgoing rows / 20" in observed and "MORE AVAILABLE" in observed
            report["cli_process_snapshot"] = percentiles(cli_times) if cli_times else None
            report["checks"].append("native read-only graph snapshot caps sample at20 and reports more; CLI timings include process/schema/two connections")
            server.terminate()
            server.wait(timeout=5)
            server = None
            report["offline_stats"] = json.loads(cli("db", "stats", GRAPH, "--json"))
            assert report["offline_stats"]["nodes"] == nodes and report["offline_stats"]["edges"] == nodes - 1
            report["audit"] = json.loads(cli("db", "check", GRAPH, "--json"))
            assert report["audit"]["status"] == "ok"
            report["result"] = "PASS"
        finally:
            if server is not None and server.poll() is None:
                server.kill()
            if server is not None:
                server.wait(timeout=5)
            log.close()
    assert not path.exists()
    report["ownedResourcesRemoved"] = True
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/brainwire")
    parser.add_argument("--nodes", type=int, default=1000)
    parser.add_argument("--iterations", type=int, default=1000)
    parser.add_argument("--cli-iterations", type=int, default=20)
    parser.add_argument("--self-check", action="store_true")
    options = parser.parse_args()
    print(json.dumps(self_check() if options.self_check else run(options.binary.resolve(), options.nodes,
                                                                options.iterations, options.cli_iterations), indent=2))
