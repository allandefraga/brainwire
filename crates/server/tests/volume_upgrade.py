"""Populated native volume upgrade using two explicitly supplied binaries.

Creates one private synthetic store with the old binary, then opens and writes it
with the new binary. Never opens it with the old binary afterward. Every process,
credential and store belongs to this test. Requires the Neo4j Python driver.
This checks native local-volume compatibility, not Docker/HA or power loss.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import secrets
import shutil
import socket
import subprocess
import tempfile
import time

DATABASE = "upgrade"
VISIBLE = "synthetic:alice"
HIDDEN = "synthetic:bob"


def expected_state(rows, upgraded=False):
    customers = [{"id": f"customer-{i:04d}", "email": f"customer-{i:04d}@example.test",
                  "ordinal": i, "active": True} for i in range(rows)]
    segments = [{"id": f"segment-{i:02d}", "name": f"Segment {i}"} for i in range(4)]
    edges = [{"source": item["id"], "target": segments[i % 4]["id"], "weight": i}
             for i, item in enumerate(customers)]
    acls = [{"label": "Customer", "entityId": item["id"],
             "allow": [VISIBLE if i % 2 == 0 else HIDDEN], "deny": []}
            for i, item in enumerate(customers)]
    acls += [{"label": "Segment", "entityId": item["id"], "allow": [VISIBLE], "deny": []}
             for item in segments]
    if upgraded:
        customers[0]["email"] = "upgraded@example.test"
        edges[0]["weight"] = -1
        acls[0]["allow"] = [HIDDEN]
        customers.append({"id": "customer-new", "email": "new@example.test",
                          "ordinal": rows, "active": False})
        edges.append({"source": "customer-new", "target": "segment-00", "weight": rows})
        acls.append({"label": "Customer", "entityId": "customer-new", "allow": [VISIBLE], "deny": []})
    allowed = {item["entityId"] for item in acls
               if item["label"] == "Customer" and VISIBLE in item["allow"]}
    return {"customers": sorted(customers, key=lambda item: item["id"]),
            "segments": segments, "edges": sorted(edges, key=lambda item: item["source"]),
            "acls": sorted(acls, key=lambda item: (item["label"], item["entityId"])),
            "visible_customers": [item for item in sorted(customers, key=lambda item: item["id"])
                                  if item["id"] in allowed],
            "visible_edges": [item for item in sorted(edges, key=lambda item: item["source"])
                              if item["source"] in allowed]}


def verify_equal(actual, expected, context):
    if actual != expected:
        raise AssertionError(context + " does not match the exact synthetic expectation")


class UpgradeOrder:
    """Prevent accidental downgrade reopening of the owned volume."""
    def __init__(self):
        self.new_opened = False

    def use(self, phase):
        if phase not in ("old", "new"):
            raise ValueError("unknown binary phase")
        if phase == "old" and self.new_opened:
            raise ValueError("old binary cannot reopen a volume after the new binary")
        self.new_opened |= phase == "new"


def self_check():
    original, upgraded = expected_state(8), expected_state(8, True)
    assert len(original["customers"]) == len(original["edges"]) == 8
    assert len(original["visible_customers"]) == 4
    assert len(upgraded["customers"]) == len(upgraded["edges"]) == 9
    assert len(upgraded["visible_customers"]) == 4
    for key in ("customers", "edges", "acls", "visible_customers"):
        broken = dict(original)
        broken[key] = original[key][:-1]
        try:
            verify_equal(broken, original, key)
        except AssertionError:
            pass
        else:
            raise AssertionError("partial state passed exact reconciliation")
    order = UpgradeOrder()
    order.use("old"); order.use("new"); order.use("new")
    try:
        order.use("old")
    except ValueError:
        pass
    else:
        raise AssertionError("downgrade reopening was permitted")
    return {"fixture_check": "PASS", "runtime_exercised": False}


def run(old_binary, new_binary, rows, old_revision, new_revision):
    from neo4j import GraphDatabase, __version__ as driver_version
    from neo4j.exceptions import ServiceUnavailable, SessionExpired

    report = {"synthetic": True, "rows": rows, "driver": driver_version,
              "scope": "protected native single-node populated-volume upgrade",
              "source_revision_verification": "caller supplied; binary hashes measured",
              "binaries": {}, "phases": []}
    with tempfile.TemporaryDirectory(prefix="brainwire-volume-upgrade-") as temporary:
        root = Path(temporary).resolve()
        root.chmod(0o700)
        binaries = {}
        for phase, source, revision in (("old", old_binary, old_revision),
                                        ("new", new_binary, new_revision)):
            target = root / (phase + "-brainwire")
            # Snapshot each caller-supplied executable so concurrent builds cannot
            # change which bytes this test executes midway through the upgrade.
            with source.open("rb") as incoming, target.open("xb") as outgoing:
                shutil.copyfileobj(incoming, outgoing)
            target.chmod(0o700)
            version = subprocess.run([str(target), "--version"], capture_output=True,
                                     text=True, check=True, timeout=10).stdout.strip()
            report["binaries"][phase] = {"sha256": hashlib.sha256(target.read_bytes()).hexdigest(),
                                           "version": version, "source_revision": revision}
            binaries[phase] = target
        tokens = {name: secrets.token_hex(32) for name in ("writer", "reader")}
        policy = {"principals": [
            {"id": "writer", "token_sha256": hashlib.sha256(tokens["writer"].encode()).hexdigest(),
             "graphs": {DATABASE: "write"}, "instance_admin": True},
            {"id": "reader", "token_sha256": hashlib.sha256(tokens["reader"].encode()).hexdigest(),
             "graphs": {DATABASE: "read"}, "instance_admin": False,
             "node_visibility": {"principals": [VISIBLE]}},
        ]}
        policy_path = root / "policy.json"
        policy_path.write_text(json.dumps(policy)); policy_path.chmod(0o600)
        schema = root / "schema.json"
        schema.write_text(json.dumps({"version": "1", "nodes": [
            {"label": "Customer", "properties": ["email", "ordinal", "active"],
             "indexed_fields": ["email", "ordinal"]},
            {"label": "Segment", "properties": ["name"], "indexed_fields": ["name"]}],
            "edges": [{"type": "IN_SEGMENT", "from": {"label": "Customer"},
                       "to": {"label": "Segment"}, "properties": ["weight"]}]}))
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0)); port = probe.getsockname()[1]
        uri = f"bolt://127.0.0.1:{port}"
        env = {key: value for key, value in os.environ.items() if not key.startswith("BRAINWIRE_")}
        env.update(BRAINWIRE_SERVER_DATA_DIR=str(root / "data"),
                   BRAINWIRE_SERVER_LISTEN=f"127.0.0.1:{port}",
                   BRAINWIRE_SERVER_DEFAULT_DATABASE=DATABASE,
                   BRAINWIRE_SERVER_AUTH_FILE=str(policy_path),
                   BRAINWIRE_MEMORY_CACHE="64MiB", BRAINWIRE_MEMORY_MEMTABLES="16MiB",
                   BRAINWIRE_MEMORY_QUERY="64MiB", BRAINWIRE_SERVER_MAX_MESSAGE="8MiB")
        order, process, active_phase = UpgradeOrder(), None, None
        log = (root / "server.log").open("a")

        def cli(phase, *args):
            order.use(phase)
            result = subprocess.run([str(binaries[phase]), *args], env=env, cwd=root,
                                    capture_output=True, text=True, timeout=30)
            if result.returncode:
                raise RuntimeError("owned " + phase + " CLI failed: " + args[0])
            return result.stdout

        def driver(role):
            return GraphDatabase.driver(uri, auth=(role, tokens[role]), connection_timeout=1,
                                        connection_acquisition_timeout=3, max_transaction_retry_time=0)

        def query(text, params=None, role="writer"):
            with driver(role) as connection:
                with connection.session(database=DATABASE) as session:
                    return session.run(text, {} if params is None else params).data()

        def start(phase):
            nonlocal process, active_phase
            if process is not None:
                raise AssertionError("an owned server is already active")
            order.use(phase)
            active_phase = phase
            process = subprocess.Popen([str(binaries[phase]), "serve"], env=env, cwd=root,
                                       stdout=log, stderr=log)
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    raise RuntimeError("owned " + phase + " server exited before readiness")
                try:
                    verify_equal(query("RETURN 1 AS ready"), [{"ready": 1}], "readiness")
                    return
                except (ServiceUnavailable, SessionExpired, ConnectionError):
                    time.sleep(.05)
            raise TimeoutError("owned server readiness deadline")

        def stop():
            nonlocal process
            if process is None:
                return
            process.terminate()
            code = process.wait(timeout=15)
            process = None
            if code != 0:
                raise AssertionError("owned " + str(active_phase) + " server did not stop cleanly")

        def audit(phase):
            if process is not None:
                raise AssertionError("audit requires stopped owned server")
            result = json.loads(cli(phase, "db", "check", DATABASE, "--json"))
            assert result["status"] == "ok", "offline audit failed"
            return result

        def inspect(expected, schema_expected=None):
            for role, prefix in (("writer", ""), ("reader", "visible_")):
                actual_customers = query("MATCH (c:Customer) RETURN c.entity_id AS id,c.email AS email,c.ordinal AS ordinal,c.active AS active ORDER BY id", role=role)
                actual_edges = query("MATCH (c:Customer)-[r:IN_SEGMENT]->(s:Segment) RETURN c.entity_id AS source,s.entity_id AS target,r.weight AS weight ORDER BY source", role=role)
                verify_equal(actual_customers, expected[prefix + "customers"], role + " customers")
                verify_equal(actual_edges, expected[prefix + "edges"], role + " typed edges")
                verify_equal(query("MATCH (s:Segment) RETURN s.entity_id AS id,s.name AS name ORDER BY id", role=role), expected["segments"], role + " segments")
                for item in expected["customers"]:
                    result = query("MATCH (c:Customer {email:$email}) RETURN c.entity_id AS id", {"email": item["email"]}, role)
                    visible = not prefix or any(row["id"] == item["id"] for row in expected["visible_customers"])
                    verify_equal(result, [{"id": item["id"]}] if visible else [], role + " equality index")
            refs = [{"label": item["label"], "entityId": item["entityId"]} for item in expected["acls"]]
            actual_acls = query("CALL db.nodeAcls($refs) YIELD label,entityId,allow,deny RETURN label,entityId,allow,deny ORDER BY label,entityId", {"refs": refs})
            verify_equal(actual_acls, expected["acls"], "persisted ACL metadata")
            indexes = query("CALL db.indexes() YIELD label,property,state RETURN label,property,state ORDER BY label,property")
            verify_equal(indexes, [{"label": label, "property": prop, "state": "scalar_equality_ready"}
                                   for label, prop in [("Customer", "email"), ("Customer", "ordinal"), ("Segment", "name")]], "index declarations")
            catalog = query("CALL db.schema() YIELD version,nodes,relationships,indexesReady RETURN version,nodes,relationships,indexesReady")
            if schema_expected is not None:
                verify_equal(catalog, schema_expected, "ontology catalog")
            return catalog

        try:
            cli("old", "db", "create", DATABASE, "--ontology", str(schema))
            start("old")
            original = expected_state(rows)
            with driver("writer") as connection:
                with connection.session(database=DATABASE) as session:
                    with session.begin_transaction() as transaction:
                        transaction.run("UNWIND $rows AS row MERGE (c:Customer {entity_id:row.id}) SET c.email=row.email,c.ordinal=row.ordinal,c.active=row.active", rows=original["customers"]).consume()
                        transaction.run("UNWIND $rows AS row MERGE (s:Segment {entity_id:row.id}) SET s.name=row.name", rows=original["segments"]).consume()
                        transaction.run("UNWIND $rows AS row MATCH (c:Customer {entity_id:row.source}),(s:Segment {entity_id:row.target}) MERGE (c)-[r:IN_SEGMENT]->(s) SET r.weight=row.weight", rows=original["edges"]).consume()
                        verify_equal(transaction.run("CALL db.setNodeAcls($rows) YIELD updated RETURN updated", rows=original["acls"]).data(), [{"updated": rows + 4}], "old ACL write")
                        transaction.commit()
            catalog = inspect(original)
            stop()
            report["phases"].append({"phase": "old_populated", "customers": rows,
                                      "edges": rows, "audit": audit("old")})
            # This is the point of no return: the test never calls old again.
            report["new_preflight_audit"] = audit("new")
            start("new")
            inspect(original, catalog)
            changed = expected_state(rows, True)
            with driver("writer") as connection:
                with connection.session(database=DATABASE) as session:
                    with session.begin_transaction() as transaction:
                        transaction.run("MATCH (c:Customer {entity_id:'customer-0000'}) SET c.email='upgraded@example.test'").consume()
                        transaction.run("MATCH (:Customer {entity_id:'customer-0000'})-[r:IN_SEGMENT]->(:Segment) SET r.weight=-1").consume()
                        transaction.run("MERGE (c:Customer {entity_id:'customer-new'}) SET c.email='new@example.test',c.ordinal=$ordinal,c.active=false", ordinal=rows).consume()
                        transaction.run("MATCH (c:Customer {entity_id:'customer-new'}),(s:Segment {entity_id:'segment-00'}) MERGE (c)-[r:IN_SEGMENT]->(s) SET r.weight=$weight", weight=rows).consume()
                        transaction.run("CALL db.setNodeAcls($rows) YIELD updated RETURN updated", rows=[item for item in changed["acls"] if item["entityId"] in ("customer-0000", "customer-new")]).consume()
                        transaction.commit()
            inspect(changed, catalog)
            verify_equal(query("MATCH (c:Customer {email:'customer-0000@example.test'}) RETURN c.entity_id AS id"), [], "removed old index value")
            stop()
            report["phases"].append({"phase": "new_mutated", "customers": rows + 1,
                                      "edges": rows + 1, "audit": audit("new")})
            start("new")
            inspect(changed, catalog)
            stop()
            report["phases"].append({"phase": "new_reopened", "customers": rows + 1,
                                      "edges": rows + 1, "audit": audit("new")})
            report["result"] = "PASS"
        finally:
            if process is not None:
                if process.poll() is None:
                    process.kill()
                process.wait(timeout=10)
            log.close()
    report["owned_resources_removed"] = not root.exists()
    assert report["owned_resources_removed"]
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--old-binary", type=Path)
    parser.add_argument("--new-binary", type=Path)
    parser.add_argument("--old-revision", default="unspecified")
    parser.add_argument("--new-revision", default="unspecified")
    parser.add_argument("--rows", type=int, default=32)
    parser.add_argument("--self-check", action="store_true")
    options = parser.parse_args()
    if not 4 <= options.rows <= 1000:
        parser.error("rows must be between 4 and 1000 for the bounded synthetic fixture")
    if options.self_check:
        print(json.dumps(self_check(), indent=2))
    else:
        if options.old_binary is None or options.new_binary is None:
            parser.error("--old-binary and --new-binary are required for a live upgrade")
        print(json.dumps(run(options.old_binary.resolve(strict=True), options.new_binary.resolve(strict=True),
                             options.rows, options.old_revision, options.new_revision), indent=2))
