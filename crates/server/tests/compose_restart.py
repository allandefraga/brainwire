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

    def compose(*args, check=True):
        return subprocess.run(["docker", "compose", "-p", project, "-f", str(root / "deploy/compose.yaml"), *args],
                              env=env, check=check, capture_output=True, text=True)

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
        compose("run", "--rm", "--no-deps", "brainwire", "db", "apply-ontology", "client_memory", "/opt/brainwire/example-schema.json")
        compose("start")
        ready()
        with GraphDatabase.driver(f"bolt://127.0.0.1:{port}", auth=None) as driver:
            with driver.session(database="client_memory") as session:
                session.run("CREATE (c:Customer {entity_id:'compose-customer', email:'compose@example.test'})").consume()
        compose("kill", "-s", "SIGKILL")
        compose("up", "-d", "--no-build", "--force-recreate")
        ready()
        with GraphDatabase.driver(f"bolt://127.0.0.1:{port}", auth=None) as driver:
            with driver.session(database="client_memory") as session:
                found = session.run("MATCH (c:Customer) WHERE c.email='compose@example.test' RETURN c.entity_id AS id").single()
                assert found["id"] == "compose-customer"
        compose("stop")
        audit = json.loads(compose("run", "--rm", "--no-deps", "brainwire", "db", "check", "client_memory", "--json").stdout)
        assert audit["status"] == "ok", audit
        print(json.dumps({"result": "PASS", "image": image, "project": project, "audit": audit}, indent=2))
    finally:
        compose("down", "--volumes", "--remove-orphans")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", default="brainwire:local")
    run(parser.parse_args().image)
