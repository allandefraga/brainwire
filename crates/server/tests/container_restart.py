"""Optional Docker + Neo4j-driver acceptance for a retained single-node volume.

Run after `docker build -t brainwire:local .` with a Python environment that
has `neo4j` installed: `python container_restart.py`. This creates and removes
only its own uniquely named Docker container and volume.
"""

import argparse
from contextlib import suppress
import socket
import subprocess
import time
import uuid

from neo4j import GraphDatabase
from neo4j.exceptions import Neo4jError, ServiceUnavailable, SessionExpired

from mvp_demo import write


def docker(*args, check=True):
    return subprocess.run(
        ["docker", *args], capture_output=True, text=True, check=check
    )


def stop_container(name):
    docker("kill", name, check=False)
    docker("rm", "--force", name, check=False)
    for _ in range(50):
        if not docker("ps", "--all", "--quiet", "--filter", f"name=^{name}$").stdout.strip():
            return
        time.sleep(0.1)
    raise RuntimeError(f"container {name} did not stop")


def start_container(name, volume, image):
    docker(
        "run", "--detach", "--rm", "--name", name,
        "--publish", "127.0.0.1::7687",
        "--mount", f"type=volume,src={volume},dst=/data",
        "--memory", "1g",
        "--env", "BRAINWIRE_MEMORY_CACHE=256MiB",
        "--env", "BRAINWIRE_MEMORY_MEMTABLES=32MiB",
        "--env", "BRAINWIRE_MEMORY_QUERY=256MiB",
        image, "serve", "--allow-unauthenticated-remote",
    )
    address = docker("port", name, "7687/tcp").stdout.strip()
    host, port_text = address.rsplit(":", 1)
    port = int(port_text)
    for _ in range(100):
        try:
            with socket.create_connection((host, port), timeout=0.1):
                ready = docker("exec", name, "brainwire", "probe", "--mode", "ready", "--timeout-ms", "500", check=False)
                if ready.returncode == 0:
                    return f"bolt://{host}:{port}"
        except OSError:
            time.sleep(0.1)
    raise RuntimeError(f"Bolt listener did not become ready at {address}")


def run(image, rows, cycles):
    suffix = uuid.uuid4().hex[:12]
    container = f"brainwire-smoke-{suffix}"
    volume = f"brainwire-smoke-{suffix}"
    docker("volume", "create", volume)
    try:
        docker(
            "run", "--rm", "--mount", f"type=volume,src={volume},dst=/data",
            "--env", "BRAINWIRE_MEMORY_CACHE=256MiB",
            "--env", "BRAINWIRE_MEMORY_MEMTABLES=32MiB",
            image, "db", "create", "default", "--ontology",
            "/opt/brainwire/example-schema.json",
        )
        for cycle in range(cycles + 2):
            uri = start_container(container, volume, image)
            try:
                with GraphDatabase.driver(uri, auth=None) as driver:
                    driver.verify_connectivity()
                    if cycle == 0:
                        write(driver, rows)
                        with driver.session(database="default") as session:
                            session.run("MATCH (c:Customer {entity_id:'driver-1'}) SET c.email='cycle-0@example.test'").consume()
                    else:
                        with driver.session(database="default") as session:
                            count = session.run(
                                "MATCH (c:Customer) RETURN count(*) AS n"
                            ).single()["n"]
                            expected = rows + 2 + cycle - 1
                            assert count == expected, (cycle, count, expected)
                            assert session.run(
                                "MATCH (c:Customer {entity_id: 'discarded'}) "
                                "RETURN count(*) AS n"
                            ).single()["n"] == 0
                            email = f"cycle-{min(cycle - 1, cycles)}@example.test"
                            assert session.run("MATCH (c:Customer) WHERE c.email=$email RETURN c.entity_id AS id", email=email).single()["id"] == "driver-1"
                            assert session.run("MATCH (c:Customer {email:'uncommitted-crash'}) RETURN count(*) AS n").single()["n"] == 0
                            for previous in range(min(cycle - 1, cycles)):
                                assert session.run("MATCH (c:Customer {email:$email}) RETURN count(*) AS n", email=f"cycle-{previous}@example.test").single()["n"] == 0
                            state = session.run("CALL db.indexes() YIELD label, property, state WHERE label='Customer' AND property='email' RETURN state").single()["state"]
                            assert state == "scalar_equality_ready", state
                            if cycle <= cycles:
                                session.run(
                                    "CREATE (c:Customer {entity_id: $id})",
                                    id=f"container-cycle-{cycle}",
                                ).consume()
                                session.run("MATCH (c:Customer {entity_id:'driver-1'}) SET c.email=$email", email=f"cycle-{cycle}@example.test").consume()
                if cycle <= cycles:
                    # SIGKILL while an indexed overwrite is still uncommitted.
                    with GraphDatabase.driver(uri, auth=None) as driver:
                        with driver.session(database="default") as session:
                            tx = session.begin_transaction()
                            try:
                                tx.run("MATCH (c:Customer {entity_id:'driver-1'}) SET c.email='uncommitted-crash'").consume()
                                assert tx.run("MATCH (c:Customer) WHERE c.email='uncommitted-crash' RETURN count(*) AS n").single()["n"] == 1
                                stop_container(container)
                            finally:
                                with suppress(Neo4jError, ServiceUnavailable, SessionExpired):
                                    tx.close()
                print(f"cycle {cycle}: verified after container restart")
            except Exception:
                print(docker("logs", container, check=False).stdout)
                raise
            finally:
                stop_container(container)
        audit = docker("run", "--rm", "--mount", f"type=volume,src={volume},dst=/data",
                       "--env", "BRAINWIRE_MEMORY_CACHE=256MiB", "--env", "BRAINWIRE_MEMORY_MEMTABLES=32MiB",
                       image, "db", "check", "default", "--json")
        print(audit.stdout.strip())
        print("container restart and indexed-overwrite recovery: ok")
    finally:
        docker("rm", "--force", container, check=False)
        for _ in range(30):
            removed = docker("volume", "rm", volume, check=False)
            if removed.returncode == 0:
                break
            time.sleep(0.1)
        if removed.returncode != 0:
            raise RuntimeError(f"could not clean temporary volume {volume}: {removed.stderr}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--image", default="brainwire:local")
    parser.add_argument("--rows", type=int, default=10_000)
    parser.add_argument("--cycles", type=int, default=5)
    args = parser.parse_args()
    if args.rows < 0 or args.cycles < 0:
        parser.error("--rows and --cycles must be nonnegative")
    run(args.image, args.rows, args.cycles)
