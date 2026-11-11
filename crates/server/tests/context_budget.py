"""Check context byte budgets against a temporary authenticated Bolt server."""
import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time

from neo4j import GraphDatabase

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))

from brainwire import ContextClient, ContextLimitError, GraphClient
from examples.python.create_service_tokens import create


def run(binary):
    with tempfile.TemporaryDirectory(prefix="brainwire-context-budget-") as temporary:
        root = Path(temporary)
        auth_dir = create(root / "auth", "workspace")
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        config = root / "brainwire.toml"
        config.write_text(f'''[server]
listen = "127.0.0.1:{port}"
data_dir = "{root / 'data'}"
default_database = "workspace"
auth_file = "{auth_dir / 'policy.json'}"
query_timeout_ms = 5000
[memory]
cache = "64MiB"
memtables = "4MiB"
query = "32MiB"
limit = "2GiB"
''')
        ontology = root / "ontology.json"
        ontology.write_text(json.dumps({"version": "1", "nodes": [
            {"label": "Document", "properties": ["sections", "source_url"]}], "edges": []}))
        env = {key: value for key, value in os.environ.items() if not key.startswith("BRAINWIRE_")}
        subprocess.run([str(binary), "db", "create", "workspace", "--ontology", str(ontology),
                        "--config", str(config)], env=env, check=True, capture_output=True, timeout=30)

        def connect(role):
            auth = json.loads((auth_dir / f"{role}.json").read_text())
            return GraphDatabase.driver(f"bolt://127.0.0.1:{port}",
                auth=(auth["principal"], auth["credentials"]), connection_timeout=2,
                connection_acquisition_timeout=5, max_connection_pool_size=1,
                max_transaction_retry_time=0)

        with (root / "server.log").open("w") as log:
            process = subprocess.Popen([str(binary), "serve", "--config", str(config)],
                                       env=env, stdout=log, stderr=log)
            try:
                deadline = time.monotonic() + 10
                while time.monotonic() < deadline:
                    if process.poll() is not None:
                        raise RuntimeError("temporary context server exited before readiness")
                    try:
                        with socket.create_connection(("127.0.0.1", port), timeout=.1):
                            break
                    except OSError:
                        time.sleep(.025)
                else:
                    raise RuntimeError("temporary context server did not start")

                sections = [["é\n" * 16384]]
                source_url = "source://synthetic"
                with connect("writer") as driver:
                    GraphClient(driver, "workspace").upsert_node("Document", "one", {
                        "sections": sections, "source_url": source_url})

                with connect("reader") as driver:
                    context = ContextClient(GraphClient(driver, "workspace"))
                    for read, options in ((context.nodes, {}), (context.evidence,
                            {"source_fields": {"url": "source_url"}})):
                        try:
                            read("Document", ["one"], properties=["sections"],
                                 max_bytes=48 << 10, **options)
                        except ContextLimitError:
                            pass
                        else:
                            raise AssertionError(f"{read.__name__} accepted an oversized response")
                        result = read("Document", ["one"], properties=["sections"],
                                      max_bytes=128 << 10, **options)
                        [record] = result["records"]
                        assert record["entity_id"] == "one"
                        assert record["properties"] == {"sections": sections}
                        if options:
                            [source] = result["sources"]
                            assert source["source"] == {"url": source_url}
                            assert record["citation_id"] == source["citation_id"]
                return {"result": "PASS", "synthetic": True,
                        "checks": ["nodes and evidence refuse oversized nested UTF-8/escaped text",
                                   "reads and citations succeed within budget after refusal"]}
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
