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
from brainwire import connect
from examples.company_brain.demo import read_context, seed


def run(binary):
    with tempfile.TemporaryDirectory(prefix="brainwire-company-demo-") as temporary:
        root = Path(temporary)
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        config = root / "brainwire.toml"
        config.write_text(f'''[server]
listen = "127.0.0.1:{port}"
data_dir = "{root / 'data'}"
default_database = "company_demo"
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
                with connect(f"bolt://127.0.0.1:{port}", "company_demo", auth=None) as graph:
                    seed(graph)
                    seed(graph)
                    result = read_context(graph)
                    assert [row["entity_id"] for row in result["context"]["records"]] == ["decision-retry"]
                    assert result["context"]["sources"][0]["source"]["url"] == "https://example.com/meeting/release"
                    assert len(result["source_links"]["paths"]) == 1
                    assert result["scope_size"] == 1 and not result["scope_may_be_truncated"]
                    assert graph.run_cypher("MATCH (n:Decision) RETURN count(n) AS count")[0]["count"] == 2
                cli = subprocess.run(
                    [sys.executable, "-m", "examples.company_brain.demo", "--seed",
                     "--uri", f"bolt://127.0.0.1:{port}", "--database", "company_demo"],
                    cwd=ROOT, env=env, check=True, capture_output=True, text=True,
                )
                assert json.loads(cli.stdout)["context"]["records"][0]["entity_id"] == "decision-retry"
                print("PASS: company context, bounded scope, source link and repeated seed")
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
