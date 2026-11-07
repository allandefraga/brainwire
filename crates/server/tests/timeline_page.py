"""Owned synthetic Bolt acceptance for paginated timelines and endpoint ACLs.

Run with a local Brainwire binary and a Python environment containing neo4j.
Creates only temporary data, credentials and one owned child server process.
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

from brainwire import GraphClient
from examples.python.create_service_tokens import create
from examples.python.timeline_demo import ontology, read_event_page, record_event


class ReadOnlyDriver:
    """Check the helper's session mode while forwarding to the real Bolt driver."""

    def __init__(self, driver):
        self.driver = driver
        self.sessions = 0

    def session(self, **options):
        assert options.get("default_access_mode") == "READ", options
        self.sessions += 1
        return self.driver.session(**options)


def pages(driver, subject, start, end, limit):
    output, cursor, cursors = [], None, []
    for _ in range(20):
        result = read_event_page(driver, "timeline", subject, start_at=start,
                                 end_at=end, after=cursor, limit=limit)
        assert len(result["events"]) <= limit, result
        output.extend(result["events"])
        cursor = result["next_cursor"]
        if cursor is None:
            return output, cursors
        assert result["events"], "empty page returned a cursor"
        assert cursor == {key: result["events"][-1][key] for key in ("occurred_at", "id")}
        assert cursor not in cursors, "cursor failed to advance"
        cursors.append(cursor)
    raise AssertionError("bounded timeline failed to terminate")


def run(binary):
    with tempfile.TemporaryDirectory(prefix="brainwire-timeline-page-") as temporary:
        root = Path(temporary)
        create(root / "auth", "timeline", acl_writer=True)
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
default_database = "timeline"
auth_file = "{policy_path}"
query_timeout_ms = 5000
[memory]
cache = "64MiB"
memtables = "4MiB"
query = "32MiB"
limit = "2GiB"
''')
        schema = root / "ontology.json"
        schema.write_text(json.dumps(ontology("Ticket")))
        env = {key: value for key, value in os.environ.items() if not key.startswith("BRAINWIRE_")}
        subprocess.run([str(binary), "db", "create", "timeline", "--ontology", str(schema),
                        "--config", str(config)], env=env, check=True, capture_output=True, timeout=30)

        def connect(role):
            auth = credentials[role]
            return GraphDatabase.driver(f"bolt://127.0.0.1:{port}",
                auth=(auth["principal"], auth["credentials"]), connection_timeout=2,
                connection_acquisition_timeout=5, max_transaction_retry_time=0)

        with (root / "server.log").open("w") as log:
            process = subprocess.Popen([str(binary), "serve", "--config", str(config)],
                                       env=env, stdout=log, stderr=log)
            try:
                deadline = time.monotonic() + 10
                while time.monotonic() < deadline:
                    if process.poll() is not None:
                        raise RuntimeError("temporary timeline server exited before readiness")
                    try:
                        with socket.create_connection(("127.0.0.1", port), timeout=.1):
                            break
                    except OSError:
                        time.sleep(.025)
                else:
                    raise RuntimeError("temporary timeline server did not start")

                subject = ("Ticket", "ticket-1")
                hidden_subject = ("Ticket", "ticket-private")
                fixtures = [
                    ("before", "2026-11-07T09:59:59Z", ["reader", "health"]),
                    ("tie-a", "2026-11-07T11:00:00+01:00", ["reader", "health"]),
                    ("tie-b", "2026-11-07T10:00:00Z", ["health"]),
                    ("tie-c", "2026-11-07T10:00:00Z", ["reader", "health"]),
                    ("fraction-a", "2026-11-07T10:00:00.000001Z", ["reader"]),
                    ("fraction-b", "2026-11-07T10:00:00.000002Z", ["health"]),
                    ("later", "2026-11-07T10:00:01Z", ["reader", "health"]),
                    ("at-end", "2026-11-07T10:00:02Z", ["reader", "health"]),
                ]
                identities, allowances = {}, {}
                with connect("writer") as driver:
                    graph = GraphClient(driver, "timeline")
                    graph.upsert_node_with_acl("Ticket", subject[1], {"title": "Synthetic timeline"},
                                               {"allow": ["reader", "health"]})
                    graph.upsert_node_with_acl("Ticket", hidden_subject[1], {"title": "Hidden subject"},
                                               {"allow": ["health"]})
                    for key, timestamp, allowed in fixtures:
                        identity = record_event(driver, "timeline", subject, key, "milestone", timestamp,
                                                {"marker": key})
                        identities[key] = identity
                        allowances[identity] = allowed
                        graph.upsert_node_with_acl("TimelineEvent", identity, {}, {"allow": allowed})
                    hidden_event = record_event(driver, "timeline", hidden_subject, "visible-target",
                                                "milestone", "2026-11-07T10:00:00Z", {})
                    graph.upsert_node_with_acl("TimelineEvent", hidden_event, {},
                                               {"allow": ["reader", "health"]})
                    expected = read_event_page(ReadOnlyDriver(driver), "timeline", subject,
                        start_at="2026-11-07T10:00:00Z", end_at="2026-11-07T10:00:02Z",
                        limit=100)["events"]
                    assert len(expected) == 6, expected
                    assert [row["occurred_at"] for row in expected] == sorted(
                        row["occurred_at"] for row in expected)

                for role in ("reader", "health"):
                    expected_visible = [row for row in expected if role in allowances[row["id"]]]
                    with connect(role) as driver:
                        reader = ReadOnlyDriver(driver)
                        actual, cursors = pages(reader, subject, "2026-11-07T11:00:00+01:00",
                                                "2026-11-07T10:00:02Z", 1)
                        assert actual == expected_visible, (role, actual, expected_visible)
                        assert reader.sessions == len(expected_visible), reader.sessions
                        forbidden = {identity for identity, allowed in allowances.items() if role not in allowed}
                        assert not forbidden.intersection(row["id"] for row in actual)
                        assert not forbidden.intersection(cursor["id"] for cursor in cursors)
                        first = read_event_page(reader, "timeline", subject, limit=100)
                        assert len(first["events"]) <= 100 and first["next_cursor"] is None
                        if role == "reader":
                            hidden = read_event_page(reader, "timeline", hidden_subject, limit=1)
                            missing = read_event_page(reader, "timeline", ("Ticket", "missing"), limit=1)
                            assert hidden == missing == {"events": [], "next_cursor": None}
                            assert read_event_page(reader, "timeline", ("Ticket", "x' OR true")) == missing
                        else:
                            assert [row["id"] for row in read_event_page(
                                reader, "timeline", hidden_subject)["events"]] == [hidden_event]
                        with driver.session(database="timeline") as session:
                            try:
                                session.run("CREATE (:Ticket {entity_id:'forbidden-write'})").consume()
                            except Neo4jError as error:
                                assert error.code == "Neo.ClientError.Security.Forbidden", error
                            else:
                                raise AssertionError("read grant accepted a write")

                return {"result": "PASS", "synthetic": True,
                        "scope": "temporary native Bolt server with two static ACL identities",
                        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                        "checks": ["UTC fractions and timestamp ties", "exclusive upper bound",
                                   "read-only keyset pages", "event ACL and cursor visibility",
                                   "source ACL and missing-source equivalence", "read grants reject writes"],
                        "limits": ["no snapshot across pages", "no automatic history or AS OF",
                                   "small synthetic fixture; not performance or production qualification"]}
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
    arguments = parser.parse_args()
    print(json.dumps(run(arguments.binary.resolve()), indent=2))
