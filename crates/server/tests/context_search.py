"""Owned synthetic acceptance for provider search and graph ACL filtering."""
import argparse
import hashlib
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
from neo4j.exceptions import Neo4jError

from brainwire import GraphClient, search_context
from examples.python.create_service_tokens import create


def provider(ids):
    def retrieve(text, *, graph_name, label, limit, timeout):
        assert (graph_name, label, text, limit) == ("workspace", "Document", "release blocker", 100)
        assert 0 < timeout <= 10
        return ids
    return retrieve


def run(binary):
    with tempfile.TemporaryDirectory(prefix="brainwire-context-search-") as temporary:
        root = Path(temporary)
        create(root / "auth", "workspace", acl_writer=True)
        policy_path = root / "auth/policy.json"
        policy = json.loads(policy_path.read_text())
        for principal in policy["principals"]:
            if principal["id"] in ("reader", "health"):
                principal["node_visibility"] = {"principals": [principal["id"]]}
        policy_path.write_text(json.dumps(policy))
        credentials = {role: json.loads((root / f"auth/{role}.json").read_text())
                       for role in ("writer", "reader", "health")}
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        config = root / "brainwire.toml"
        config.write_text(f'''[server]
listen = "127.0.0.1:{port}"
data_dir = "{root / 'data'}"
default_database = "workspace"
auth_file = "{policy_path}"
[memory]
cache = "64MiB"
memtables = "4MiB"
query = "32MiB"
limit = "2GiB"
''')
        ontology = root / "ontology.json"
        ontology.write_text(json.dumps({"version": "1", "nodes": [
            {"label": "Document", "properties": ["title", "source_url"]}], "edges": []}))
        env = {key: value for key, value in os.environ.items() if not key.startswith("BRAINWIRE_")}
        subprocess.run([str(binary), "db", "create", "workspace", "--ontology", str(ontology),
                        "--config", str(config)], env=env, check=True, capture_output=True)

        def connect(role):
            auth = credentials[role]
            return GraphDatabase.driver(f"bolt://127.0.0.1:{port}",
                auth=(auth["principal"], auth["credentials"]), connection_timeout=1,
                max_transaction_retry_time=0)

        with (root / "server.log").open("w") as log:
            process = subprocess.Popen([str(binary), "serve", "--config", str(config)],
                                       env=env, stdout=log, stderr=log)
            try:
                for _ in range(200):
                    if process.poll() is not None:
                        raise RuntimeError("temporary server stopped during startup")
                    try:
                        with socket.create_connection(("127.0.0.1", port), timeout=.1):
                            break
                    except OSError:
                        time.sleep(.025)
                else:
                    raise RuntimeError("temporary server did not start")
                with connect("writer") as driver:
                    graph = GraphClient(driver, "workspace")
                    for identity, allow in [("a", ["reader"]), ("b", ["health"]),
                                            ("shared", ["reader", "health"])]:
                        graph.upsert_node_with_acl("Document", identity,
                            {"title": f"Release {identity}", "source_url": f"source://{identity}"},
                            {"allow": allow})
                for role, own, hidden in [("reader", "a", "b"), ("health", "b", "a")]:
                    with connect(role) as driver:
                        graph = GraphClient(driver, "workspace")
                        options = {"properties": ["title"], "source_fields": {"url": "source_url"}}
                        result = search_context(graph, "Document", "release blocker", providers={
                            "lexical": provider(["missing", "a", "b", "shared"]),
                            "vector": provider(["b", "a", "shared", "a"])}, **options)
                        visible_only = search_context(graph, "Document", "release blocker", providers={
                            "lexical": provider([own, "shared"]),
                            "vector": provider([own, "shared"])}, **options)
                        assert result == visible_only, "hidden candidates influenced authorized context"
                        assert [row["entity_id"] for row in result["records"]] == [own, "shared"]
                        assert [source["record"]["entity_id"] for source in result["sources"]] == [own, "shared"]
                        assert f"source://{hidden}" not in json.dumps(result)
                        empty = search_context(graph, "Document", "release blocker",
                            providers={"lexical": provider([hidden, "missing"])}, **options)
                        assert empty["records"] == empty["sources"] == []
                        scoped = search_context(graph, "Document", "release blocker",
                            providers={"lexical": provider(["shared", hidden, own])},
                            candidate_ids=[hidden, own], **options)
                        assert [row["entity_id"] for row in scoped["records"]] == [own]
                        assert scoped["records"][0]["matches"][0]["rank"] == 1
                        assert [source["record"]["entity_id"] for source in scoped["sources"]] == [own]
                        assert f"source://{hidden}" not in json.dumps(scoped)
                        try:
                            graph.run_cypher("CREATE (:Document {entity_id:'forbidden'})")
                        except Neo4jError:
                            pass
                        else:
                            raise AssertionError("read-only principal could write")
                return {"result": "PASS", "synthetic": True,
                    "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                    "checks": ["two static ACL identities", "hidden leading provider IDs removed",
                               "visible ranks and sources invariant", "scoped candidates remain ACL filtered", "read grants reject writes"],
                    "limits": ["providers are synthetic callbacks, not external search indexes",
                               "no shared snapshot across providers and graph", "no performance qualification"]}
            finally:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/brainwire")
    args = parser.parse_args()
    print(json.dumps(run(args.binary.resolve()), indent=2))
