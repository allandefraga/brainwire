"""Check that a Bolt transaction cannot commit an ACL stamp over a concurrent revocation.

Run with the project's Python Bolt driver and --binary pointing to a local build.
"""

import argparse
from contextlib import contextmanager
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time

from neo4j import GraphDatabase
from neo4j.exceptions import Neo4jError, ServiceUnavailable

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))
from examples.python.create_service_tokens import create

GRAPH = "acl_conflict"
PRINCIPAL = "user:synthetic:reader"
REF = {"label": "Document", "entityId": "document-1"}
FIND = "MATCH (n:Document {entity_id: $id}) RETURN n"
NODE_ACL = "CALL db.nodeAcls($refs) YIELD allow RETURN allow"
STAMP = "CALL db.setNodeAcls($entries) YIELD updated RETURN updated"


@contextmanager
def server(binary, folder, env, driver):
    with (folder / "server.log").open("ab") as log:
        process = subprocess.Popen([binary, "serve"], env=env, stdout=log, stderr=log)
        try:
            for attempt in range(100):
                if process.poll() is not None:
                    raise RuntimeError("temporary Brainwire server exited during startup")
                try:
                    driver.verify_connectivity()
                    break
                except ServiceUnavailable:
                    if attempt == 99:
                        raise
                    time.sleep(0.05)
            yield
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=10)


def check_visibility(writer, reader, expected_acl):
    with writer.session(database=GRAPH) as admin:
        assert admin.run(FIND, id=REF["entityId"]).single(strict=True)[0]["entity_id"] == REF["entityId"]
        assert admin.run(NODE_ACL, refs=[REF]).single(strict=True)["allow"] == expected_acl
    with reader.session(database=GRAPH) as viewer:
        nodes = viewer.run(FIND, id=REF["entityId"]).values()
        assert [node["entity_id"] for (node,) in nodes] == ([REF["entityId"]] if expected_acl else [])


def run(binary):
    binary = str(Path(binary).resolve())
    with tempfile.TemporaryDirectory(prefix="brainwire-acl-conflict-") as directory:
        folder = Path(directory)
        auth = create(folder / "auth", GRAPH, acl_writer=True)
        policy_path = auth / "policy.json"
        policy = json.loads(policy_path.read_text())
        reader_policy = next(entry for entry in policy["principals"] if entry["id"] == "reader")
        reader_policy["node_visibility"] = {"principals": [PRINCIPAL]}
        policy_path.write_text(json.dumps(policy))
        schema = folder / "schema.json"
        schema.write_text(json.dumps({"version": "1", "nodes": [{"label": "Document"}], "edges": []}))
        env = {key: value for key, value in os.environ.items() if not key.startswith("BRAINWIRE_")}
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            address = f"127.0.0.1:{probe.getsockname()[1]}"
        env.update(
            BRAINWIRE_SERVER_DATA_DIR=str(folder / "data"),
            BRAINWIRE_SERVER_DEFAULT_DATABASE=GRAPH,
            BRAINWIRE_SERVER_LISTEN=address,
            BRAINWIRE_SERVER_AUTH_FILE=str(policy_path),
        )
        subprocess.run([binary, "db", "create", GRAPH, "--ontology", str(schema)], env=env, check=True)

        def driver(role):
            credential = json.loads((auth / f"{role}.json").read_text())
            return GraphDatabase.driver(
                f"bolt://{address}", auth=(credential["principal"], credential["credentials"]),
                connection_timeout=1, max_transaction_retry_time=0,
            )

        with driver("writer") as writer, driver("reader") as reader, server(binary, folder, env, writer):
            with writer.session(database=GRAPH) as admin:
                admin.run("CREATE (:Document {entity_id: $id})", id=REF["entityId"]).consume()
                assert admin.run(STAMP, entries=[dict(REF, allow=[PRINCIPAL])]).single(strict=True)["updated"] == 1
            check_visibility(writer, reader, [PRINCIPAL])

            with writer.session(database=GRAPH) as session_a, session_a.begin_transaction() as tx_a:
                assert tx_a.run(FIND, id=REF["entityId"]).single(strict=True)[0]["entity_id"] == REF["entityId"]
                old_acl = tx_a.run(NODE_ACL, refs=[REF]).single(strict=True)["allow"]
                assert old_acl == [PRINCIPAL]
                with writer.session(database=GRAPH) as session_b, session_b.begin_transaction() as tx_b:
                    assert tx_b.run(STAMP, entries=[dict(REF, allow=[])]).single(strict=True)["updated"] == 1
                    tx_b.commit()
                check_visibility(writer, reader, [])

                # Snapshot reads register no conflict dependency. Stamping in A enables
                # conflict detection against changes since A's snapshot.
                # Carrying old_acl into a fresh transaction would bypass this check.
                try:
                    result = tx_a.run(STAMP, entries=[dict(REF, allow=old_acl)]).single(strict=True)
                    assert result["updated"] == 1
                    tx_a.commit()
                except Neo4jError as error:
                    assert error.code == "Neo.TransientError.Transaction.Outdated", error
                    assert error.is_retryable(), error
                else:
                    raise AssertionError("stale ACL stamp committed over a revocation")
            check_visibility(writer, reader, [])

        with driver("writer") as writer, driver("reader") as reader, server(binary, folder, env, writer):
            check_visibility(writer, reader, [])

    print("PASS: stale ACL stamp conflicted and the revocation survived restart")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default=str(ROOT / "target/release/brainwire"))
    run(parser.parse_args().binary)
