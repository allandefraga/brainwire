"""Run the synthetic company-brain example against an owned temporary server."""
import argparse
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
from neo4j.exceptions import Neo4jError

from brainwire import connect
from examples.company_brain.demo import read_context, seed
from examples.python.agent_memory import recall, remember
from examples.python.create_service_tokens import create


def verify_memory(graph):
    arguments = ("Retry policy", "Review the retry window after each release.",
                 "Engineering review", "https://example.com/notes/retry")
    assert recall(graph, "project-release")["records"] == []
    try:
        remember(graph, "missing-project", *arguments)
    except ValueError as error:
        assert "project is unavailable" in str(error)
    else:
        raise AssertionError("missing project unexpectedly accepted a memory")
    assert graph.run_cypher("MATCH (n:Decision) RETURN count(n) AS count")[0]["count"] == 2
    first = remember(graph, "project-release", *arguments)
    assert remember(graph, "project-release", *arguments) == first
    result = recall(graph, "project-release")
    assert result == {"project": "project-release", "truncated": False, "records": [{
        **first, "title": arguments[0], "body": arguments[1],
        "source_title": arguments[2], "source_url": arguments[3],
    }]}
    second = remember(graph, "project-release", "Rotation", "Rotate the key.", *arguments[2:])
    assert first["source_id"] == second["source_id"]
    assert first["decision_id"] != second["decision_id"]
    limited = recall(graph, "project-release", limit=1)
    assert limited["truncated"] and len(limited["records"]) == 1
    graph.run_cypher("CREATE (:Project {entity_id: 'project-other'})")
    assert recall(graph, "project-other")["records"] == []
    assert graph.run_cypher("MATCH (n:Decision) RETURN count(n) AS count")[0]["count"] == 4
    return first, second


def run(binary):
    with tempfile.TemporaryDirectory(prefix="brainwire-company-demo-") as temporary:
        root = Path(temporary)
        auth = create(root / "auth", "company_demo", acl_writer=True)
        policy = auth / "policy.json"
        value = json.loads(policy.read_text())
        for principal in value["principals"]:
            if principal["id"] == "reader":
                principal["node_visibility"] = {"principals": ["reader"]}
        policy.write_text(json.dumps(value))

        def credentials(role):
            value = json.loads((auth / f"{role}.json").read_text())
            return value["principal"], value["credentials"]

        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        config = root / "brainwire.toml"
        config.write_text(f'''[server]
listen = "127.0.0.1:{port}"
data_dir = "{root / 'data'}"
default_database = "company_demo"
auth_file = "{policy}"
[memory]
cache = "64MiB"
memtables = "4MiB"
query = "32MiB"
limit = "2GiB"
''')
        env = {key: value for key, value in os.environ.items() if not key.startswith("BRAINWIRE_")}
        subprocess.run([str(binary), "db", "create", "company_demo", "--ontology",
                        str(ROOT / "examples/company_brain/ontology.json"), "--config", str(config)],
                       check=True, env=env, capture_output=True)
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
                uri = f"bolt://127.0.0.1:{port}"
                with connect(uri, "company_demo", auth=credentials("writer")) as graph:
                    seed(graph)
                    seed(graph)
                    result = read_context(graph)
                    assert [row["entity_id"] for row in result["context"]["records"]] == ["decision-retry"]
                    assert result["context"]["sources"][0]["source"]["url"] == "https://example.com/meeting/release"
                    assert len(result["source_links"]["paths"]) == 1
                    assert result["scope_size"] == 1 and not result["scope_may_be_truncated"]
                    assert graph.run_cypher("MATCH (n:Decision) RETURN count(n) AS count")[0]["count"] == 2
                    first, second = verify_memory(graph)
                    third = remember(graph, "project-release", "An internal decision",
                                     "Keep this private.", "Internal notes", "https://example.com/private")
                    graph.set_node_acl("Project", "project-release", {"allow": ["reader"]})
                    for record in (first, second, third):
                        graph.set_node_acl("Decision", record["decision_id"], {"allow": ["reader"]})
                    graph.set_node_acl("Source", first["source_id"], {"allow": ["reader"]})
                    graph.set_node_acl("Source", third["source_id"], {"allow": ["writer"]})
                with connect(uri, "company_demo", auth=credentials("reader")) as graph:
                    result = recall(graph, "project-release")
                    assert {record["decision_id"] for record in result["records"]} == {
                        first["decision_id"], second["decision_id"],
                    }, "a decision with hidden evidence was returned"
                    assert "https://example.com/private" not in json.dumps(result)
                    assert not result["truncated"], "hidden evidence affected the visible result count"
                    try:
                        remember(graph, "project-release", "Forbidden", "Test", "Notes", "https://example.com")
                    except Neo4jError as error:
                        assert error.code == "Neo.ClientError.Security.Forbidden"
                    else:
                        raise AssertionError("read-only principal could record a memory")
                cli = subprocess.run(
                    [sys.executable, "-m", "examples.company_brain.demo", "--seed",
                     "--uri", f"bolt://127.0.0.1:{port}", "--database", "company_demo"],
                    cwd=ROOT, env={**env, "BRAINWIRE_BOLT_AUTH_FILE": str(auth / "writer.json")},
                    check=True, capture_output=True, text=True,
                )
                assert json.loads(cli.stdout)["context"]["records"][0]["entity_id"] == "decision-retry"
                cli = subprocess.run(
                    [sys.executable, "-m", "examples.python.agent_memory", "--uri", uri,
                     "recall", "--project", "project-release", "--limit", "1"],
                    cwd=ROOT, env={**env, "BRAINWIRE_BOLT_AUTH_FILE": str(auth / "reader.json")},
                    check=True, capture_output=True, text=True,
                )
                result = json.loads(cli.stdout)
                assert result["truncated"] and len(result["records"]) == 1
                print("PASS: company context and memory replay, project scope, source ACL and read-only CLI")
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
    run(parser.parse_args().binary.resolve())
