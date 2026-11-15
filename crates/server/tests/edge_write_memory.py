"""Exercise typed relationship validation and write admission over authenticated Bolt."""

import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile

from neo4j import GraphDatabase, Query
from neo4j.exceptions import Neo4jError

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))

from examples.python.create_service_tokens import create
from examples.python.duckdb_acceptance import server


MEMORY_REFUSED = "Neo.TransientError.General.MemoryPressure"
CONSTRAINT_REFUSED = "Neo.ClientError.Schema.ConstraintValidationFailed"
DATABASE = "edge_budget"
MATCH_PAIR = "MATCH (a:Item {entity_id:'a'}), (b:Item {entity_id:'b'}) "
UPDATE_REVISION = MATCH_PAIR + "MATCH (a)-[r:EVENT]->(b) SET r.revision = $revision"
MUTATIONS = (
    MATCH_PAIR + "MERGE (a)-[:LINK {payload:'small'}]->(b)",
    MATCH_PAIR + "MATCH (a)-[r:LINK]->(b) SET r.payload = null",
    MATCH_PAIR + "MATCH (a)-[r:EVENT]->(b) SET r.payload = null",
)


def run(binary):
    payload = "x" * (256 << 10)
    with tempfile.TemporaryDirectory(prefix="brainwire-edge-write-memory-") as temporary:
        root = Path(temporary)
        auth_dir = create(root / "auth", DATABASE)
        ontology = root / "ontology.json"
        ontology.write_text(json.dumps({"version": "1", "nodes": [{"label": "Item"}], "edges": [
            {"type": "LINK", "from": {"label": "Item"}, "to": {"label": "Item"},
             "properties": ["payload"], "property_types": {"payload": "string"}},
            {"type": "EVENT", "from": {"label": "Item"}, "to": {"label": "Item"},
             "properties": ["kind", "payload", "revision"], "merge_key_fields": ["kind"],
             "property_types": {"kind": "string", "payload": "string", "revision": "integer"}},
        ]}))
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            address = f"127.0.0.1:{listener.getsockname()[1]}"
        env = {key: value for key, value in os.environ.items() if not key.startswith("BRAINWIRE_")}
        env.update(
            BRAINWIRE_SERVER_LISTEN=address,
            BRAINWIRE_SERVER_DATA_DIR=str(root / "data"),
            BRAINWIRE_SERVER_DEFAULT_DATABASE=DATABASE,
            BRAINWIRE_SERVER_AUTH_FILE=str(auth_dir / "policy.json"),
            BRAINWIRE_MEMORY_CACHE="64MiB",
            BRAINWIRE_MEMORY_MEMTABLES="4MiB",
            BRAINWIRE_MEMORY_QUERY="16MiB",
        )
        subprocess.run([binary, "db", "create", DATABASE, "--ontology", str(ontology)],
                       env=env, check=True, capture_output=True, timeout=30)

        def connect(role):
            credential = json.loads((auth_dir / f"{role}.json").read_text())
            return GraphDatabase.driver(
                f"bolt://{address}", auth=(credential["principal"], credential["credentials"]),
                connection_timeout=2, connection_acquisition_timeout=5, max_transaction_retry_time=0,
            )

        def reject_write(driver, mutation, expected, **params):
            prior_write_completed = False
            try:
                with driver.session(database=DATABASE) as session, session.begin_transaction(timeout=5) as tx:
                    tx.run("CREATE (:Item {entity_id:'must-rollback'})").consume()
                    prior_write_completed = True
                    tx.run(mutation, **params).consume()
            except Neo4jError as error:
                assert prior_write_completed, "the small write failed before the relationship mutation"
                assert error.code == expected, error.code
                if expected == MEMORY_REFUSED:
                    assert "rolled_back=true" in error.message, error.message
            else:
                raise AssertionError(f"relationship write completed instead of returning {expected}: {mutation}")

        def check_rollback(driver):
            with driver.session(database=DATABASE) as session:
                record = session.run(Query(
                    "MATCH (n:Item {entity_id:'must-rollback'}) RETURN count(n) AS count",
                    timeout=5,
                )).single(strict=True)
                assert record["count"] == 0

        def check_payloads(session):
            return session.run(Query(
                "MATCH ()-[r]->() WHERE r.payload = $payload RETURN count(r) AS count",
                timeout=5,
            ), payload=payload).single(strict=True)["count"]

        def read_revision(session):
            return session.run(Query(
                "MATCH ()-[r:EVENT]->() RETURN r.revision AS revision",
                timeout=5,
            )).single(strict=True)["revision"]

        with connect("writer") as writer, server(binary, root, env, writer):
            with writer.session(database=DATABASE) as session:
                session.run(Query("CREATE (:Item {entity_id:'a'}), (:Item {entity_id:'b'})", timeout=5)).consume()
                for pattern in ("LINK", "EVENT {kind:'opening'}"):
                    session.run(
                        Query(MATCH_PAIR + f"MERGE (a)-[r:{pattern}]->(b) SET r.payload = $payload", timeout=5),
                        payload=payload,
                    ).consume()
                session.run(Query(UPDATE_REVISION, timeout=5), revision=1).consume()
            reject_write(writer, UPDATE_REVISION, CONSTRAINT_REFUSED, revision="invalid")
            check_rollback(writer)
            with writer.session(database=DATABASE) as session:
                assert read_revision(session) == 1
                assert check_payloads(session) == 2

        pressured = env | {"BRAINWIRE_MEMORY_QUERY": "128KiB"}
        with connect("writer") as writer, server(binary, root, pressured, writer), connect("reader") as reader:
            for mutation in (*MUTATIONS, UPDATE_REVISION):
                params = {"revision": 2} if mutation == UPDATE_REVISION else {}
                reject_write(writer, mutation, MEMORY_REFUSED, **params)
                check_rollback(reader)
                with reader.session(database=DATABASE) as session:
                    assert session.run(
                        Query("MATCH ()-[r]->() RETURN count(r) AS count", timeout=5)
                    ).single(strict=True)["count"] == 2

        with connect("writer") as writer, server(binary, root, env, writer):
            with writer.session(database=DATABASE) as session:
                assert read_revision(session) == 1
                assert check_payloads(session) == 2
                session.run(Query(UPDATE_REVISION, timeout=5), revision=3).consume()
                revision = read_revision(session)
                assert type(revision) is int and revision == 3
                assert check_payloads(session) == 2
    return {"result": "PASS", "error": MEMORY_REFUSED, "constraint_error": CONSTRAINT_REFUSED,
            "mutation_cases": len(MUTATIONS) + 1, "typed_write": "verified",
            "rollback": "verified", "original_payloads": "verified", "other_client": "responsive"}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/brainwire")
    args = parser.parse_args()
    print(json.dumps(run(str(args.binary.resolve()))))
