"""Protected Python/Bolt WCC/SCC qualification on a disposable synthetic graph.

From the repository root, after building the release server:
uv run --no-project --with neo4j==6.3.0 python -m examples.analytics.qualify_components
"""
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time

from neo4j import GraphDatabase
from neo4j.exceptions import Neo4jError, ServiceUnavailable
from examples.python.create_service_tokens import create


def main():
    binary = Path("target/release/brainwire").resolve()
    with tempfile.TemporaryDirectory(prefix="brainwire-components-") as directory:
        root = Path(directory)
        create(root / "auth", "pages")
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            port = probe.getsockname()[1]
        env = dict(os.environ, BRAINWIRE_SERVER_DATA_DIR=str(root / "data"),
                   BRAINWIRE_SERVER_DEFAULT_DATABASE="pages",
                   BRAINWIRE_SERVER_LISTEN=f"127.0.0.1:{port}",
                   BRAINWIRE_SERVER_AUTH_FILE=str(root / "auth/policy.json"),
                   BRAINWIRE_MEMORY_CACHE="64MiB", BRAINWIRE_MEMORY_MEMTABLES="16MiB",
                   BRAINWIRE_MEMORY_QUERY="16MiB")
        subprocess.run([str(binary), "db", "create", "pages", "--ontology",
                        "examples/analytics/schema.json"], env=env, check=True,
                       stdout=subprocess.DEVNULL)
        credentials = {}
        for role in ("writer", "reader"):
            value = json.loads((root / f"auth/{role}.json").read_text())
            credentials[role] = (value["principal"], value["credentials"])
        expected = {
            "wcc": [("a", "a", 3), ("b", "a", 3), ("c", "a", 3),
                    ("d", "d", 2), ("e", "d", 2), ("f", "f", 1)],
            "scc": [("a", "a", 2), ("b", "a", 2), ("c", "c", 1),
                    ("d", "d", 2), ("e", "d", 2), ("f", "f", 1)],
        }
        with (root / "server.log").open("w") as log:
            for phase in ("initial", "reopened"):
                process = subprocess.Popen([str(binary), "serve"], env=env,
                                           stdout=log, stderr=log)
                try:
                    with GraphDatabase.driver(f"bolt://127.0.0.1:{port}",
                                              auth=credentials["writer"],
                                              connection_timeout=1,
                                              max_transaction_retry_time=0) as writer:
                        for attempt in range(100):
                            try:
                                writer.verify_connectivity()
                                break
                            except ServiceUnavailable:
                                if process.poll() is not None or attempt == 99:
                                    raise
                                time.sleep(0.05)
                        if phase == "initial":
                            with writer.session(database="pages") as session:
                                for entity in "abcdef":
                                    session.run("MERGE (n:Page {entity_id:$id}) RETURN n.entity_id", id=entity).consume()
                                for source, target in [("a", "b"), ("b", "a"), ("b", "c"), ("d", "e"), ("e", "d")]:
                                    session.run("MATCH (a:Page {entity_id:$a}), (b:Page {entity_id:$b}) MERGE (a)-[:LINK {weight:0}]->(b) RETURN a.entity_id", a=source, b=target).consume()
                    with GraphDatabase.driver(f"bolt://127.0.0.1:{port}", auth=credentials["reader"],
                                              max_transaction_retry_time=0) as reader:
                        with reader.session(database="pages") as session:
                            for procedure, wanted in expected.items():
                                rows = session.run(f"CALL graph.{procedure}({{relationship:$type}}) YIELD node,component,size RETURN node.entity_id,component.entity_id,size", type="LINK")
                                assert [tuple(record.values()) for record in rows] == wanted
                            try:
                                session.run("MERGE (n:Page {entity_id:'forbidden'}) RETURN n.entity_id").consume()
                            except Neo4jError as error:
                                assert error.code == "Neo.ClientError.Security.Forbidden", error.code
                            else:
                                raise AssertionError("reader unexpectedly wrote")
                finally:
                    process.terminate()
                    try:
                        process.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=10)
        print(json.dumps({"status": "PASS", "nodes": 6, "relationships": 5,
                          "driver": "neo4j", "phases": ["initial", "reopened"],
                          "checks": ["wcc", "scc", "zero-weight topology", "reader access", "write denied"],
                          "serverSha256": hashlib.sha256(binary.read_bytes()).hexdigest()}, indent=2))


if __name__ == "__main__":
    main()
