"""Exercise the published metric definitions against a temporary Bolt server."""

import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile

from neo4j import GraphDatabase
from neo4j.exceptions import Neo4jError

from brainwire.client import GraphClient
from examples.company_brain.demo import seed
from examples.python.create_service_tokens import create
from examples.python.duckdb_acceptance import server
from examples.semantic_metrics.metrics import read_catalog, run_metric


HERE = Path(__file__).resolve().parent
DATABASE = "company_demo"


def run(binary):
    catalog = read_catalog(HERE / "catalog.json")
    for aggregation in ("sum", "avg"):
        catalog["metrics"][f"{aggregation}_effort"] = {
            "label": "Decision", "aggregation": aggregation, "property": "effort_hours",
            "dimensions": {"title": "string"},
        }
    with tempfile.TemporaryDirectory(prefix="brainwire-metrics-test-") as temporary:
        root = Path(temporary)
        ontology = json.loads((HERE.parent / "company_brain" / "ontology.json").read_text(encoding="utf-8"))
        next(node for node in ontology["nodes"] if node["label"] == "Decision")["properties"].append("effort_hours")
        ontology_file = root / "ontology.json"
        ontology_file.write_text(json.dumps(ontology), encoding="utf-8")
        auth = create(root / "auth", DATABASE, acl_writer=True)
        policy_file = auth / "policy.json"
        policy = json.loads(policy_file.read_text())
        next(item for item in policy["principals"] if item["id"] == "reader")["node_visibility"] = {
            "principals": ["reader"],
        }
        policy_file.write_text(json.dumps(policy))

        def credentials(role):
            value = json.loads((auth / f"{role}.json").read_text())
            return value["principal"], value["credentials"]

        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            address = f"127.0.0.1:{probe.getsockname()[1]}"
        env = {key: value for key, value in os.environ.items() if not key.startswith("BRAINWIRE_")}
        env.update(BRAINWIRE_SERVER_DATA_DIR=str(root / "data"), BRAINWIRE_SERVER_DEFAULT_DATABASE=DATABASE,
                   BRAINWIRE_SERVER_LISTEN=address, BRAINWIRE_SERVER_AUTH_FILE=str(policy_file),
                   BRAINWIRE_MEMORY_CACHE="64MiB", BRAINWIRE_MEMORY_MEMTABLES="4MiB",
                   BRAINWIRE_MEMORY_QUERY="32MiB")
        subprocess.run([binary, "db", "create", DATABASE, "--ontology", str(ontology_file)],
                       env=env, check=True, capture_output=True, timeout=10)
        uri = f"bolt://{address}"
        with GraphDatabase.driver(uri, auth=credentials("writer"), connection_timeout=1) as driver, \
                server(binary, root, env, driver), \
                GraphDatabase.driver(uri, auth=credentials("reader"), connection_timeout=1) as reader_driver:
            writer, reader = GraphClient(driver, DATABASE), GraphClient(reader_driver, DATABASE)
            assert run_metric(writer, catalog, "avg_effort")["rows"] == [{"value": None}]
            seed(writer)
            writer.upsert_node("Decision", "decision-retry", {"effort_hours": 2})
            writer.upsert_node("Decision", "decision-other", {"effort_hours": 100})
            writer.upsert_node("Decision", "decision-untitled", {})
            for entity in ("decision-retry", "decision-untitled"):
                writer.set_node_acl("Decision", entity, {"allow": ["reader"]})
            writer.set_node_acl("Decision", "decision-other", {"allow": ["writer"]})
            assert run_metric(writer, catalog, "decisions")["rows"] == [{"value": 3}]
            assert run_metric(reader, catalog, "decisions")["rows"] == [{"value": 2}]
            assert run_metric(reader, catalog, "sum_effort")["rows"] == [{"value": 2}]
            assert run_metric(reader, catalog, "avg_effort")["rows"] == [{"value": 2.0}]
            result = subprocess.run([sys.executable, "-m", "examples.semantic_metrics.metrics", "query",
                                     "decisions", "--uri", uri, "--group-by", "title"],
                                    env={**env, "BRAINWIRE_BOLT_AUTH_FILE": str(auth / "reader.json")},
                                    check=True, capture_output=True, timeout=10)
            assert json.loads(result.stdout)["rows"] == [
                {"dimension": "Retry failed requests", "value": 1}, {"dimension": None, "value": 1},
            ]
            try:
                run_metric(reader, catalog, "decisions", group_by="title", max_groups=1)
            except ValueError as error:
                assert "exceeds its row limit" in str(error)
            else:
                raise AssertionError("grouped query returned a partial result")
            writer.upsert_node("Decision", "decision-retry", {"effort_hours": "two"})
            try:
                run_metric(reader, catalog, "sum_effort")
            except Neo4jError as error:
                assert "numeric aggregate does not accept" in str(error)
            else:
                raise AssertionError("numeric aggregate accepted a visible string")
    print("PASS: metric definitions, typed groups, numeric aggregates, nulls, and reader ACL over Bolt")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    run(str(parser.parse_args().binary.resolve()))
