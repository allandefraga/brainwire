"""Synthetic native Bolt timeout qualification (requires the Neo4j Python driver).

python crates/server/tests/bolt_timeout.py --binary target/debug/brainwire
No build, existing database, external credential or remote service is used.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import time

from neo4j import GraphDatabase, Query
from neo4j.exceptions import Neo4jError, ServiceUnavailable

CODE = "Neo.ClientError.Statement.ExecutionTimedOut"


def expect_timeout(operation):
    began = time.monotonic()
    try:
        operation()
    except Neo4jError as error:
        assert error.code == CODE, error.code
        return time.monotonic() - began
    raise AssertionError("transaction timeout was ignored")


def qualify(binary):
    binary = Path(binary).resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="brainwire-bolt-timeout-") as root:
        root = Path(root)
        token = secrets.token_hex(32)
        policy = root / "policy.json"
        policy.write_text(json.dumps({"principals": [{"id": "tester", "token_sha256": hashlib.sha256(token.encode()).hexdigest(), "graphs": {"timeout": "write"}, "instance_admin": False}]}))
        policy.chmod(0o600)
        schema = root / "schema.json"
        schema.write_text(json.dumps({"version": "1", "nodes": [{"label": "Customer", "properties": []}], "edges": []}))
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            port = probe.getsockname()[1]
        env = {k: v for k, v in os.environ.items() if not k.startswith("BRAINWIRE_")}
        env.update(BRAINWIRE_SERVER_DATA_DIR=str(root / "data"), BRAINWIRE_SERVER_DEFAULT_DATABASE="timeout",
                   BRAINWIRE_SERVER_LISTEN=f"127.0.0.1:{port}", BRAINWIRE_SERVER_AUTH_FILE=str(policy),
                   BRAINWIRE_SERVER_QUERY_TIMEOUT_MS="2000", BRAINWIRE_MEMORY_CACHE="64MiB",
                   BRAINWIRE_MEMORY_MEMTABLES="16MiB", BRAINWIRE_MEMORY_QUERY="128MiB")
        subprocess.run([str(binary), "db", "create", "timeout", "--ontology", str(schema)], env=env, check=True, stdout=subprocess.DEVNULL)
        report = {}
        for phase in ("initial", "reopened"):
            with (root / "server.log").open("a") as log:
                process = subprocess.Popen([str(binary), "serve"], env=env, stdout=log, stderr=log)
                try:
                    with GraphDatabase.driver(f"bolt://127.0.0.1:{port}", auth=("tester", token), connection_timeout=1, max_transaction_retry_time=0) as driver:
                        for attempt in range(100):
                            try:
                                driver.verify_connectivity()
                                break
                            except ServiceUnavailable:
                                if process.poll() is not None or attempt == 99:
                                    raise
                                time.sleep(.05)
                        with driver.session(database="timeout") as session:
                            if phase == "initial":
                                # Query(...) sets RUN extra.tx_timeout. A timeout keyword to
                                # session.run would only create a Cypher parameter.
                                query = ("CREATE (:Customer {entity_id:'implicit-timeout'}) WITH $v AS v "
                                         "UNWIND v AS a UNWIND v AS b UNWIND v AS c UNWIND v AS d RETURN count(*)")
                                elapsed = expect_timeout(lambda: session.run(Query(query, timeout=.5), v=list(range(200))).consume())
                                report["implicit_active_timeout_seconds"] = elapsed
                                assert .35 <= elapsed <= .8, f"cooperative timeout took {elapsed:.3f}s"
                                # Exercise BEGIN metadata, idle expiry and staged-write rollback.
                                tx = session.begin_transaction(timeout=.5)
                                tx.run("CREATE (:Customer {entity_id:'explicit-timeout'})").consume()
                                time.sleep(.6)
                                report["explicit_expired_commit_seconds"] = expect_timeout(tx.commit)
                                tx.close()
                                # The driver resets failed connections before reuse.
                                assert session.run("RETURN 1 AS ok").single()["ok"] == 1
                            assert session.run("MATCH (n:Customer) RETURN count(*) AS n").single()["n"] == 0
                            report[phase + "_nodes"] = 0
                finally:
                    process.terminate()
                    try:
                        process.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=5)
        result = subprocess.run([str(binary), "db", "check", "timeout", "--json"], env=env, check=True, capture_output=True, text=True)
        report["integrity"] = json.loads(result.stdout)
        assert report["integrity"]["status"] == "ok"
        print(json.dumps(report, sort_keys=True))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True)
    qualify(parser.parse_args().binary)
