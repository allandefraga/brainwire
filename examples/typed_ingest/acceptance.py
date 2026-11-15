"""Exercise typed ingestion against an authenticated, disposable Brainwire server."""

import argparse
from copy import deepcopy
from hashlib import sha256
import json
import os
from pathlib import Path
from secrets import token_hex
import socket
import subprocess
import sys
from tempfile import TemporaryDirectory
import time

from neo4j.exceptions import Neo4jError

from brainwire import connect, schema_paths
from examples.typed_ingest.ingest import read_json, write_batch


HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
DATABASE = "typed_demo"
EXPECTED = {"nodes": 2, "relationships": 1}
QUERY = ("MATCH (p:Project)-[r:HAS_DECISION]->(d:Decision) "
         "RETURN p.name AS project, d.approved AS approved, r.confidence AS confidence")


def verify_writes(uri, env, scoped_credentials):
    schema_path, batch_path = HERE / "schema.json", HERE / "batch.json"
    schema, batch = read_json(schema_path), read_json(batch_path)
    cli = [sys.executable, "-m", "examples.typed_ingest.ingest", str(schema_path), str(batch_path)]
    preview = subprocess.run(cli, cwd=ROOT, env=env, check=True, capture_output=True, timeout=10)
    assert json.loads(preview.stdout) == {**EXPECTED, "written": False}

    auth = read_json(Path(env["BRAINWIRE_BOLT_AUTH_FILE"]))
    credentials = (auth["principal"], auth["credentials"])
    with connect(uri, DATABASE, auth=credentials, connection_timeout=5) as graph:
        path = schema_paths(graph, "Project", "Decision")["paths"][0]
        assert path["nodes"][0]["propertyTypes"] == {"name": "string"}
        assert path["relationships"][0]["propertyTypes"] == {"confidence": "float", "position": "integer"}
        assert graph.run_cypher("MATCH (n) RETURN count(n) AS count") == [{"count": 0}]
        loaded = subprocess.run(
            [*cli, "--database", DATABASE, "--uri", uri],
            cwd=ROOT, env=env, check=True, capture_output=True, timeout=10,
        )
        assert json.loads(loaded.stdout) == {**EXPECTED, "written": True}
        row = {"project": "Platform release", "approved": True, "confidence": 0.9}
        assert graph.run_cypher(QUERY) == [row]
        assert write_batch(graph, schema, batch) == EXPECTED
        assert graph.run_cypher(QUERY) == [row]
        assert graph.run_cypher("MATCH (n) RETURN count(n) AS count") == [{"count": 2}]

        omitted = deepcopy(batch)
        del omitted["relationships"][0]["properties"]["confidence"]
        assert write_batch(graph, schema, omitted) == EXPECTED
        assert graph.run_cypher(QUERY) == [row]

        changed = deepcopy(batch)
        changed["relationships"][0]["properties"]["confidence"] = None
        assert write_batch(graph, schema, changed) == EXPECTED
        row["confidence"] = None
        assert graph.run_cypher(QUERY) == [row]

        missing = deepcopy(batch)
        missing["nodes"][0]["properties"]["name"] = "Must roll back"
        missing["nodes"].append({
            "label": "Decision", "entity_id": "uncommitted",
            "properties": {"title": "Uncommitted", "approved": True},
        })
        missing["relationships"][0]["properties"]["confidence"] = 0.1
        missing["relationships"].extend([
            {**missing["relationships"][0], "target_id": "uncommitted"},
            {**missing["relationships"][0], "target_id": "missing"},
        ])
        try:
            write_batch(graph, schema, missing)
        except ValueError as error:
            assert "relationship write count" in str(error), error
        else:
            raise AssertionError("accepted a relationship with a missing endpoint")
        assert graph.run_cypher(QUERY) == [row]
        assert graph.run_cypher("MATCH (n) RETURN count(n) AS count") == [{"count": 2}]

        label_ids = (("Project", "platform-release", ["viewer"]),
                     ("Decision", "bounded-retries", ["other"]))
        for label, entity_id, allowed in label_ids:
            assert graph.run_cypher(
                "CALL db.setNodeAcl($options) YIELD updated RETURN updated",
                {"options": {"label": label, "entityId": entity_id, "allow": allowed}},
            ) == [{"updated": True}]

        with connect(uri, DATABASE, auth=scoped_credentials, connection_timeout=5) as scoped:
            assert scoped.run_cypher("MATCH (p:Project) RETURN p.name AS project") == [
                {"project": "Platform release"},
            ]
            assert scoped.run_cypher("MATCH (d:Decision) RETURN count(d) AS count") == [{"count": 0}]
            assert scoped.run_cypher(QUERY) == []
            try:
                write_batch(scoped, schema, batch)
            except Neo4jError as error:
                assert error.code == "Neo.ClientError.Security.Forbidden", error
            else:
                raise AssertionError("a visibility-restricted principal was allowed to write")
        assert graph.run_cypher(QUERY) == [row]


def run(binary):
    with TemporaryDirectory(prefix="brainwire-typed-ingest-") as temporary:
        root = Path(temporary).resolve()
        auth_dir = root / "auth"
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        address = f"127.0.0.1:{port}"
        env = {key: value for key, value in os.environ.items() if not key.startswith("BRAINWIRE_")}
        env.update(
            BRAINWIRE_SERVER_DATA_DIR=str(root / "data"),
            BRAINWIRE_SERVER_LISTEN=address,
            BRAINWIRE_SERVER_DEFAULT_DATABASE=DATABASE,
            BRAINWIRE_SERVER_AUTH_FILE=str(auth_dir / "policy.json"),
            BRAINWIRE_BOLT_AUTH_FILE=str(auth_dir / "admin.json"),
            BRAINWIRE_MEMORY_CACHE="64MiB",
            BRAINWIRE_MEMORY_MEMTABLES="4MiB",
            BRAINWIRE_MEMORY_QUERY="32MiB",
        )
        for command in (
            ("auth", "init", "--directory", str(auth_dir), "--graph", DATABASE),
            ("db", "create", DATABASE, "--ontology", str(HERE / "ontology.json")),
        ):
            subprocess.run([binary, *command], env=env, cwd=root, check=True,
                           capture_output=True, timeout=15)

        scoped_token = token_hex(32)
        policy = auth_dir / "policy.json"
        policy_data = read_json(policy)
        policy_data["principals"].append({
            "id": "scoped", "token_sha256": sha256(scoped_token.encode("ascii")).hexdigest(),
            "graphs": {DATABASE: "write"}, "instance_admin": False,
            "node_visibility": {"principals": ["viewer"]},
        })
        policy.write_text(json.dumps(policy_data), encoding="utf-8")

        with (root / "server.log").open("w", encoding="utf-8") as log:
            server = subprocess.Popen([binary, "serve"], env=env, cwd=root, stdout=log, stderr=log)
            try:
                deadline = time.monotonic() + 10
                while time.monotonic() < deadline and server.poll() is None:
                    probe = subprocess.run(
                        [binary, "probe", "--mode", "ready", "--address", address, "--timeout-ms", "500"],
                        env=env, cwd=root, capture_output=True, timeout=2,
                    )
                    if probe.returncode == 0 and server.poll() is None:
                        break
                    time.sleep(.05)
                else:
                    raise RuntimeError(f"typed ingestion server did not start (exit {server.poll()})")
                verify_writes(f"bolt://{address}", env, ("scoped", scoped_token))
            finally:
                if server.poll() is None:
                    server.terminate()
                try:
                    server.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait(timeout=5)
    print("PASS: typed batch preview, authenticated writes, replay, omission, null removal, rollback and ACL")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/brainwire")
    run(str(parser.parse_args().binary.resolve(strict=True)))
