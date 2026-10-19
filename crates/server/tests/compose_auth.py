"""Protected Compose acceptance using only an owned UUID project and synthetic secrets.

Build the supplied image first. Requires Docker Compose and the Neo4j driver.
No Swarm or Kubernetes runtime is exercised. Always removes its own containers,
volume and temporary credential files; never uses an existing project or store.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import sys
import tempfile
import time
import uuid

DATABASE = "default"
ROWS = 1000


def fixture():
    tokens = {name: secrets.token_hex(32) for name in ("monitor", "reader", "writer")}
    policy = {"principals": [{"id": name,
                              "token_sha256": hashlib.sha256(token.encode("ascii")).hexdigest(),
                              "graphs": {DATABASE: "write" if name == "writer" else "read"},
                              "instance_admin": False} for name, token in tokens.items()]}
    client = {"principal": "monitor", "credentials": tokens["monitor"]}
    return tokens, policy, client


def self_check():
    tokens, policy, client = fixture()
    assert len(set(tokens.values())) == 3 and all(len(token) == 64 for token in tokens.values())
    assert client == {"principal": "monitor", "credentials": tokens["monitor"]}
    for principal in policy["principals"]:
        token = tokens[principal["id"]]
        assert principal["token_sha256"] == hashlib.sha256(token.encode("ascii")).hexdigest()
        assert principal["token_sha256"] != hashlib.sha256(bytes.fromhex(token)).hexdigest()
        assert principal["graphs"] == {DATABASE: "write" if principal["id"] == "writer" else "read"}
        assert not principal["instance_admin"]
    assert all(token not in json.dumps(policy) for token in tokens.values())
    return {"fixture_check": "PASS", "runtime_exercised": False}


def run(image):
    from neo4j import GraphDatabase, __version__ as driver_version
    from neo4j.exceptions import Neo4jError, ServiceUnavailable, SessionExpired

    root = Path(__file__).resolve().parents[3]
    sys.path.insert(0, str(root))
    from examples.python.client import GraphClient

    project = f"brainwire-auth-{uuid.uuid4().hex[:12]}"
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    tokens, policy, client = fixture()
    checks = []
    with tempfile.TemporaryDirectory(prefix=f"{project}-") as directory:
        temporary = Path(directory)
        policy_path, health_path, invalid_path = (temporary / name for name in ("policy.json", "health.json", "invalid.json"))
        for path, content in ((policy_path, policy), (health_path, client), (invalid_path, {})):
            path.write_text(json.dumps(content))
            # Compose file mounts retain host permissions; these are random test-only secrets.
            path.chmod(0o444)
        env = {key: value for key, value in os.environ.items() if not key.startswith("BRAINWIRE_")}
        env.update(BRAINWIRE_IMAGE=image, BRAINWIRE_BIND_PORT=str(port),
                   BRAINWIRE_AUTH_POLICY_FILE=str(policy_path), BRAINWIRE_HEALTH_AUTH_FILE=str(health_path))
        base = ["docker", "compose", "--env-file", os.devnull, "-p", project,
                "-f", str(root / "deploy/compose.yaml"), "-f", str(root / "deploy/compose.auth.yaml")]

        def redact(text):
            for token in tokens.values():
                text = text.replace(token, "[redacted]")
            return text

        def command(args, check=True, timeout=30):
            try:
                result = subprocess.run(args, cwd=root, env=env, capture_output=True, text=True, timeout=timeout)
            except subprocess.TimeoutExpired as error:
                raise RuntimeError(f"owned-project command exceeded {timeout}s: {' '.join(args[:5])}") from error
            if check and result.returncode:
                raise RuntimeError(redact(f"{' '.join(args[:5])}: {result.stdout}\n{result.stderr}"))
            return result

        def compose(*args, check=True, timeout=30):
            return command(base + list(args), check=check, timeout=timeout)

        def container_id():
            identifier = compose("ps", "-q", "brainwire").stdout.strip()
            assert identifier, "owned Brainwire container is not running"
            return identifier

        def ready():
            deadline = time.monotonic() + 90
            while time.monotonic() < deadline:
                probe = compose("exec", "-T", "brainwire", "brainwire", "probe", "--mode", "ready",
                                "--timeout-ms", "500", check=False, timeout=8)
                if probe.returncode == 0:
                    identifier = container_id()
                    state = json.loads(command(["docker", "inspect", "--format", "{{json .State}}", identifier]).stdout)
                    if state.get("Health", {}).get("Status") == "healthy":
                        return identifier
                time.sleep(0.25)
            logs = compose("logs", "--tail", "80", check=False).stdout
            raise RuntimeError(redact(f"protected readiness did not become healthy within 90s:\n{logs}"))

        def query(auth, text, params=None):
            with GraphDatabase.driver(f"bolt://127.0.0.1:{port}", auth=auth, connection_timeout=3,
                                      connection_acquisition_timeout=5, max_transaction_retry_time=0) as driver:
                with driver.session(database=DATABASE) as session:
                    return session.run(text, params or {}).data()

        def denied(auth, text, code):
            try:
                query(auth, text)
            except Neo4jError as error:
                assert error.code == code, error.code
            else:
                raise AssertionError(f"expected {code} for synthetic denied operation")

        receipt = None
        try:
            compose("up", "-d", "--no-build", timeout=90)
            first_id = ready()
            first_image_id = command(["docker", "inspect", "--format", "{{.Image}}", first_id]).stdout.strip()
            kernel = compose("exec", "-T", "brainwire", "uname", "-sr").stdout.strip()
            mounts = json.loads(command(["docker", "inspect", "--format", "{{json .Mounts}}", first_id]).stdout)
            volume = next(mount["Name"] for mount in mounts if mount["Destination"] == "/data")
            assert volume.startswith(f"{project}_"), "data volume is not owned by this project"
            for destination in ("/run/secrets/brainwire-policy.json", "/run/secrets/brainwire-health-auth.json"):
                assert not next(mount for mount in mounts if mount["Destination"] == destination)["RW"]
            compose("stop", timeout=90)
            compose("run", "--rm", "--no-deps", "brainwire", "db", "apply-ontology", DATABASE,
                    "/opt/brainwire/example-schema.json", timeout=60)
            # Both are separate one-off containers while the database service is stopped.
            for extra in (
                ["-e", "BRAINWIRE_SERVER_AUTH_FILE=/run/secrets/missing-policy.json"],
                ["-e", "BRAINWIRE_SERVER_AUTH_FILE=/run/secrets/invalid-policy.json", "-v",
                 f"{invalid_path}:/run/secrets/invalid-policy.json:ro"],
            ):
                rejected = compose("run", "--rm", "--no-deps", *extra, "brainwire", "serve", check=False)
                assert rejected.returncode != 0, "invalid or missing policy unexpectedly served"
                assert "auth" in rejected.stderr.lower(), redact(rejected.stderr)
            checks.append("missing and invalid policy refuse startup")
            compose("start")
            ready()
            checks.append("mounted read-only monitor credential makes inherited readiness healthy")
            denied(None, "RETURN 1", "Neo.ClientError.Security.Unauthorized")
            denied(("reader", "0" * 64), "RETURN 1", "Neo.ClientError.Security.Unauthorized")
            checks.append("anonymous and wrong-token connections rejected")
            reader, writer = ("reader", tokens["reader"]), ("writer", tokens["writer"])
            assert query(reader, "RETURN 1 AS ok") == [{"ok": 1}]
            assert query(reader, "RETURN coalesce(null, 'fallback') AS value, coalesce(false, true) AS flag") == [{"value": "fallback", "flag": False}]
            checks.append("protected reader coalesce preserves fallback and false")
            denied(reader, "MERGE (c:Customer {entity_id:'reader-denied'})", "Neo.ClientError.Security.Forbidden")
            assert query(reader, "MATCH (c:Customer) RETURN count(*) AS n") == [{"n": 0}]
            checks.append("reader RETURN succeeds; MERGE denied before mutation")
            rows = [{"id": f"auth-{i}", "email": f"auth-{i}@example.test"} for i in range(ROWS)]
            query(writer, "UNWIND $rows AS row MERGE (c:Customer {entity_id:row.id}) SET c.email = row.email", {"rows": rows})
            assert query(reader, "MATCH (c:Customer) RETURN count(*) AS n") == [{"n": ROWS}]
            checks.append("writer commits 1000 indexed MERGE/SET records")
            with GraphDatabase.driver(f"bolt://127.0.0.1:{port}", auth=writer) as driver:
                graph = GraphClient(driver, DATABASE, batch_size=2)
                assert graph.load_nodes("Customer", [{"entity_id": "auth-cleanup"}]) == 1
            try:
                with GraphDatabase.driver(f"bolt://127.0.0.1:{port}", auth=reader) as driver:
                    GraphClient(driver, DATABASE).delete_nodes("Customer", ["auth-cleanup"])
            except Neo4jError as error:
                assert error.code == "Neo.ClientError.Security.Forbidden", error.code
            else:
                raise AssertionError("graph reader unexpectedly deleted a node")
            assert query(reader, "MATCH (c:Customer) RETURN count(*) AS n") == [{"n": ROWS + 1}]
            with GraphDatabase.driver(f"bolt://127.0.0.1:{port}", auth=writer) as driver:
                graph = GraphClient(driver, DATABASE)
                assert graph.delete_nodes("Customer", ["auth-cleanup"]) == 1
                assert graph.delete_nodes("Customer", ["auth-cleanup"]) == 0
            assert query(reader, "MATCH (c:Customer) RETURN count(*) AS n") == [{"n": ROWS}]
            checks.append("Python batched delete enforces graph writer role and is replay-safe for missing IDs")
            pending_driver = GraphDatabase.driver(f"bolt://127.0.0.1:{port}", auth=writer,
                                                  connection_timeout=3, connection_acquisition_timeout=5,
                                                  max_transaction_retry_time=0)
            pending_session = pending_tx = None
            killed = False
            try:
                pending_session = pending_driver.session(database=DATABASE)
                pending_tx = pending_session.begin_transaction()
                pending_tx.run("CREATE (c:Customer {entity_id:'auth-pending', email:'pending-created@example.test'})").consume()
                pending_tx.run("MATCH (c:Customer {entity_id:'auth-500'}) SET c.email='pending-update@example.test'").consume()
                assert pending_tx.run("MATCH (c:Customer) RETURN count(*) AS n").single()["n"] == ROWS + 1
                assert pending_tx.run("MATCH (c:Customer {entity_id:'auth-500'}) RETURN c.email AS email").single()["email"] == "pending-update@example.test"
                assert query(reader, "MATCH (c:Customer) RETURN count(*) AS n") == [{"n": ROWS}]
                assert query(reader, "MATCH (c:Customer {entity_id:'auth-pending'}) RETURN c.entity_id AS id") == []
                assert query(reader, "MATCH (c:Customer) WHERE c.email = 'auth-500@example.test' RETURN c.entity_id AS id") == [{"id": "auth-500"}]
                for email in ("pending-created@example.test", "pending-update@example.test"):
                    assert query(reader, "MATCH (c:Customer) WHERE c.email = $email RETURN c.entity_id AS id", {"email": email}) == []
                checks.append("protected explicit transaction sees pending CREATE/index update; separate reader sees only committed records")
                # Keep the transaction open until the process dies; no client rollback precedes SIGKILL.
                compose("kill", "-s", "SIGKILL")
                killed = True
            finally:
                verification_failure = sys.exc_info()[1]
                cleanup_failure = None
                for resource in (pending_tx, pending_session, pending_driver):
                    if resource is None:
                        continue
                    try:
                        resource.close()
                    except (ServiceUnavailable, SessionExpired, OSError) as error:
                        if not killed:
                            cleanup_failure = cleanup_failure or error
                    except Exception as error:
                        cleanup_failure = cleanup_failure or error
                if cleanup_failure is not None:
                    if verification_failure is not None:
                        verification_failure.add_note(f"Pending connection cleanup also failed: {type(cleanup_failure).__name__}")
                    else:
                        raise cleanup_failure
            compose("up", "-d", "--no-build", "--force-recreate", timeout=90)
            second_id = ready()
            assert second_id != first_id
            second_image_id = command(["docker", "inspect", "--format", "{{.Image}}", second_id]).stdout.strip()
            assert second_image_id == first_image_id
            recovered_mounts = json.loads(command(["docker", "inspect", "--format", "{{json .Mounts}}", second_id]).stdout)
            assert next(mount["Name"] for mount in recovered_mounts if mount["Destination"] == "/data") == volume
            assert query(reader, "MATCH (c:Customer) RETURN count(*) AS n") == [{"n": ROWS}]
            assert query(reader, "MATCH (c:Customer) WHERE c.email = 'auth-500@example.test' RETURN c.entity_id AS id") == [{"id": "auth-500"}]
            assert query(reader, "MATCH (c:Customer {entity_id:'auth-pending'}) RETURN c.entity_id AS id") == []
            assert query(reader, "MATCH (c:Customer {entity_id:'auth-500'}) RETURN c.email AS email") == [{"email": "auth-500@example.test"}]
            for email in ("pending-created@example.test", "pending-update@example.test"):
                assert query(reader, "MATCH (c:Customer) WHERE c.email = $email RETURN c.entity_id AS id", {"email": email}) == []
            checks.append("SIGKILL/recreate discards pending CREATE and index update; old indexed value and 1000 committed rows survive")
            compose("stop", timeout=90)
            audit = json.loads(compose("run", "--rm", "--no-deps", "brainwire", "db", "check", DATABASE,
                                       "--json", timeout=60).stdout)
            assert audit["status"] == "ok", audit
            checks.append("offline graph/index audit succeeds")
            receipt = {"result": "PASS", "image": image, "imageId": first_image_id, "project": project,
                       "containers": [first_id, second_id], "kernel": kernel, "database": DATABASE,
                       "driverVersion": driver_version, "volume": volume, "store": "RocksDB",
                       "secretMountsReadOnly": True,
                       "rows": ROWS, "memory": {"cache": "256MiB", "memtables": "32MiB", "query": "256MiB",
                                               "containerLimit": "2GiB"},
                       "checks": checks, "audit": audit, "scope": "single-node Docker Compose only"}
        finally:
            compose("down", "--volumes", "--remove-orphans", timeout=90)
            remaining = command(["docker", "ps", "-aq", "--filter", f"label=com.docker.compose.project={project}"]).stdout.strip()
            volumes = command(["docker", "volume", "ls", "-q", "--filter", f"label=com.docker.compose.project={project}"]).stdout.strip()
            assert not remaining and not volumes, "owned project cleanup incomplete"
        receipt["ownedResourcesRemoved"] = True
        return receipt


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", default="brainwire:auth-20261012")
    parser.add_argument("--self-check", action="store_true", help="Check synthetic policy construction without Docker or the driver")
    options = parser.parse_args()
    print(json.dumps(self_check() if options.self_check else run(options.image), indent=2))
