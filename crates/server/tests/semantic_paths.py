"""Owned authenticated acceptance for ontology paths and authorized context search.

Declared paths describe graph-wide schema metadata, not stored or ACL-visible
instance paths. Provider search separately verifies visible record evidence.
"""
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

from brainwire import ContextLimitError, GraphClient, schema_paths, search_context
from examples.python.create_service_tokens import create


class SnapshotGraph:
    """Count catalog reads and reject any accidental stored-record traversal."""

    def __init__(self, graph, after_snapshot=None):
        self.graph, self.database = graph, graph.database
        self.after_snapshot = after_snapshot
        self.calls = 0
        self.version = None

    def schema(self):
        self.calls += 1
        snapshot = self.graph.schema()
        self.version = snapshot["version"]
        if self.after_snapshot:
            self.after_snapshot()
        return snapshot

    def stream_cypher(self, *args, **kwargs):
        raise AssertionError("schema paths must not traverse stored records")


def provider(ids):
    def retrieve(text, *, graph_name, label, limit, timeout):
        assert (graph_name, label, text, limit) == ("workspace", "Document", "release blocker", 100)
        assert 0 < timeout <= 10
        return ids
    return retrieve


def run(binary):
    with tempfile.TemporaryDirectory(prefix="brainwire-semantic-paths-") as temporary:
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
            {"label": "Person", "properties": ["name"], "description": "A workspace member"},
            {"label": "Project", "properties": ["title"], "description": "A deliverable"},
            {"label": "Document", "properties": ["title", "source_url"],
             "description": "A source document"}], "edges": [
            {"type": "KNOWS", "from": {"label": "Person"}, "to": {"label": "Person"},
             "description": "Declared acquaintance route"},
            {"type": "AUTHORED", "from": {"label": "Person"}, "to": {"label": "Document"},
             "description": "Declared authorship"},
            {"type": "OWNS", "from": {"label": "Person"}, "to": {"label": "Project"}},
            {"type": "HAS_DOC", "from": {"label": "Project"}, "to": {"label": "Document"}}]}))
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
                with connect("writer") as writer_driver, connect("reader") as reader_driver:
                    writer, reader = GraphClient(writer_driver, "workspace"), GraphClient(reader_driver, "workspace")
                    counted = SnapshotGraph(reader)
                    declared = schema_paths(counted, "Person", "Document", max_hops=2)
                    assert counted.calls == 1 and declared["catalog_version"] == counted.version
                    assert declared["schema_only"] is True and declared["truncated"] is False
                    assert [[edge["name"] for edge in path["relationships"]] for path in declared["paths"]] == [
                        ["AUTHORED"], ["KNOWS", "AUTHORED"], ["OWNS", "HAS_DOC"]]
                    assert declared["paths"][0]["nodes"][0]["description"] == "A workspace member"
                    assert declared["paths"][1]["relationships"][0]["description"] == "Declared acquaintance route"
                    assert schema_paths(reader, "Person", "Person")["paths"][0]["hops"] == 0
                    assert schema_paths(reader, "Document", "Person")["paths"] == []
                    assert schema_paths(reader, "Person", "Missing")["paths"] == []
                    limited = schema_paths(reader, "Person", "Document", max_hops=2, max_paths=1)
                    assert len(limited["paths"]) == 1 and limited["truncated"] is True
                    assert len(schema_paths(reader, "Person", "Document", max_hops=1)["paths"]) == 1
                    try:
                        schema_paths(reader, "Person", "Document", max_bytes=1)
                    except ContextLimitError:
                        pass
                    else:
                        raise AssertionError("schema output byte limit was ignored")

                    changing = SnapshotGraph(reader, lambda: writer.run_cypher("CALL db.createLabel('LaterType')"))
                    old_view = schema_paths(changing, "Person", "Document", max_hops=2)
                    assert changing.calls == 1 and old_view["catalog_version"] == changing.version
                    assert reader.schema()["version"] > old_view["catalog_version"]
                    assert old_view["paths"] == declared["paths"]

                    for identity, allow in [("a", ["reader"]), ("b", ["health"]),
                                            ("shared", ["reader", "health"])]:
                        writer.upsert_node_with_acl("Document", identity,
                            {"title": f"Release {identity}", "source_url": f"source://{identity}"},
                            {"allow": allow})

                for role, own in [("reader", "a"), ("health", "b")]:
                    with connect(role) as driver:
                        graph = GraphClient(driver, "workspace")
                        routes = schema_paths(graph, "Person", "Document", max_hops=2)
                        target = routes["paths"][0]["nodes"][-1]["label"]
                        assert routes["paths"] == declared["paths"], "catalog metadata unexpectedly follows instance ACL"
                        options = {"properties": ["title"], "source_fields": {"url": "source_url"}}
                        result = search_context(graph, target, "release blocker", providers={
                            "lexical": provider(["missing", "a", "b", "shared"]),
                            "vector": provider(["b", "a", "shared"])}, **options)
                        expected = search_context(graph, target, "release blocker", providers={
                            "lexical": provider([own, "shared"]),
                            "vector": provider([own, "shared"])}, **options)
                        assert result == expected, "hidden provider IDs changed context evidence"
                        assert [row["entity_id"] for row in result["records"]] == [own, "shared"]
                        assert [source["record"]["entity_id"] for source in result["sources"]] == [own, "shared"]
                        try:
                            graph.run_cypher("CREATE (:Person {entity_id:'forbidden'})")
                        except Neo4jError:
                            pass
                        else:
                            raise AssertionError("read-only principal could write")
                return {"result": "PASS", "synthetic": True,
                    "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                    "checks": ["one authorized schema read with catalog version", "descriptions and alternate routes",
                               "bounded self-loop walks and zero-hop route", "hop/path/output limits",
                               "catalog change after snapshot does not change returned paths",
                               "schema paths to provider search with sources and two record ACL identities",
                               "read grants reject writes"],
                    "limits": ["schema is graph-wide metadata, not instance ACL-filtered paths",
                               "no shared snapshot across separate API calls or providers",
                               "small synthetic fixture, not performance qualification"]}
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
