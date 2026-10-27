"""Exercise the actual Compose template with an isolated temporary project.

Requires Docker Compose and the Neo4j driver. Creates and removes only its own
project and volume; supply a locally built current image with --image.
"""
import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import uuid

from neo4j import GraphDatabase


def run(image):
    root = Path(__file__).resolve().parents[3]
    project = f"brainwire-compose-{uuid.uuid4().hex[:12]}"
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    env = {key: value for key, value in os.environ.items() if not key.startswith("BRAINWIRE_")}
    env.update(BRAINWIRE_IMAGE=image, BRAINWIRE_BIND_PORT=str(port))
    override_dir = tempfile.TemporaryDirectory(prefix=f"{project}-")
    override = Path(override_dir.name) / "local-unauthenticated-test.yaml"
    override.write_text(
        "services:\n  brainwire:\n    command: [\"serve\", \"--allow-unauthenticated-remote\"]\n"
    )
    compose_files = ["-f", str(root / "deploy/compose.yaml"), "-f", str(override)]

    def compose(*args, check=True):
        return subprocess.run(["docker", "compose", "-p", project, *compose_files, *args],
                              env=env, check=check, capture_output=True, text=True)

    def exit_code():
        container = compose("ps", "-a", "-q", "brainwire").stdout.strip()
        return int(subprocess.run(["docker", "inspect", "-f", "{{.State.ExitCode}}", container],
                                  check=True, capture_output=True, text=True).stdout)

    def ready():
        for _ in range(100):
            if compose("exec", "-T", "brainwire", "brainwire", "probe", "--mode", "ready", "--timeout-ms", "500", check=False).returncode == 0:
                return
            time.sleep(0.1)
        raise RuntimeError(compose("logs", check=False).stdout)

    try:
        compose("up", "-d", "--no-build")
        ready()
        compose("stop")
        compose("run", "--rm", "--no-deps", "brainwire", "db", "apply-ontology", "default", "/opt/brainwire/example-schema.json")
        compose("start")
        ready()
        # SIGTERM drain through the image: an open transaction still commits.
        with GraphDatabase.driver(f"bolt://127.0.0.1:{port}", auth=None) as driver:
            with driver.session(database="default") as session:
                tx = session.begin_transaction()
                tx.run("CREATE (c:Customer {entity_id:'drain-customer', email:'drain@example.test'})").consume()
                stopping = subprocess.Popen(["docker", "compose", "-p", project, *compose_files, "stop"],
                                            env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                for _ in range(100):
                    if "shutdown requested" in compose("logs", "brainwire", check=False).stdout:
                        break
                    time.sleep(0.1)
                else:
                    raise RuntimeError("container never received SIGTERM")
                assert stopping.poll() is None, "an open transaction must hold the drain"
                tx.commit()
        assert stopping.wait(timeout=60) == 0
        assert exit_code() == 0
        assert "drained; all sessions closed" in compose("logs", "brainwire", check=False).stdout
        compose("start")
        ready()
        with GraphDatabase.driver(f"bolt://127.0.0.1:{port}", auth=None) as driver:
            with driver.session(database="default") as session:
                found = session.run("MATCH (c:Customer) WHERE c.email='drain@example.test' RETURN c.entity_id AS id").single()
                assert found["id"] == "drain-customer"
                session.run("CREATE (c:Customer {entity_id:'compose-customer', email:'compose@example.test'})").consume()
        compose("kill", "-s", "SIGKILL")
        compose("up", "-d", "--no-build", "--force-recreate")
        ready()
        with GraphDatabase.driver(f"bolt://127.0.0.1:{port}", auth=None) as driver:
            with driver.session(database="default") as session:
                found = session.run("MATCH (c:Customer) WHERE c.email='compose@example.test' RETURN c.entity_id AS id").single()
                assert found["id"] == "compose-customer"
        compose("stop")
        assert exit_code() == 0, "idle-only stop must drain cleanly"
        audit = json.loads(compose("run", "--rm", "--no-deps", "brainwire", "db", "check", "default", "--json").stdout)
        assert audit["status"] == "ok", audit
        print(json.dumps({"result": "PASS", "image": image, "project": project, "audit": audit,
                          "checks": ["commit during SIGTERM drain survives restart; exit 0", "SIGKILL recovery",
                                     "idle-only stop exits 0", "offline audit ok"]}, indent=2))
    finally:
        compose("down", "--volumes", "--remove-orphans")
        override_dir.cleanup()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", default="brainwire:local")
    run(parser.parse_args().image)
