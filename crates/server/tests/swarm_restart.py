"""Isolated single-engine Swarm restart/checkpoint acceptance inside an owned DinD daemon.

Never initializes the host daemon's Swarm. Requires a prebuilt candidate image;
the privileged outer daemon shares the Docker host kernel and is not a security sandbox.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import uuid

ROOT = Path(__file__).resolve().parents[3]
DIND = "docker:29.7.2-dind"
DATABASE = "client_memory"
INNER_PORT = 17687


def override(image):
    return {"version": "3.8", "services": {"brainwire": {
        "image": image,
        "ports": [{"target": 7687, "published": INNER_PORT, "protocol": "tcp", "mode": "ingress"}],
        "environment": {"BRAINWIRE_MEMORY_CACHE": "64MiB", "BRAINWIRE_MEMORY_MEMTABLES": "16MiB",
                        "BRAINWIRE_MEMORY_QUERY": "64MiB", "BRAINWIRE_SERVER_MAX_MESSAGE": "8MiB",
                        "BRAINWIRE_SERVER_MAX_DECODED_MESSAGE": "16MiB", "BRAINWIRE_SERVER_IO_TIMEOUT_MS": "300000"}}}}


def validate_manifests(base, auth):
    for required in ("replicas: 1", "order: stop-first", "brainwire-swarm-data", "external: true",
                     "node.id == ${BRAINWIRE_SWARM_NODE_ID:?", "driver: overlay"):
        if required not in base:
            raise ValueError(f"required retained-owner Swarm manifest rule missing: {required}")
    for required in ("brainwire-auth-policy", "brainwire-health-auth", "mode: 0400", "uid: \"10001\"", "gid: \"10001\""):
        if required not in auth:
            raise ValueError(f"required protected Swarm manifest rule missing: {required}")


def outer_plan(name, data_volume, cert_volume):
    return ["docker", "run", "-d", "--privileged", "--name", name,
            "--label", f"brainwire.acceptance={name}", "-p", f"127.0.0.1::{INNER_PORT}",
            "-v", f"{data_volume}:/var/lib/docker", "-v", f"{cert_volume}:/certs", DIND]


def self_check():
    base = (ROOT / "deploy/swarm.yaml").read_text()
    auth = (ROOT / "deploy/swarm.auth.yaml").read_text()
    validate_manifests(base, auth)
    for field, replacement in (("replicas: 1", "replicas: 2"), ("order: stop-first", "order: start-first"),
                               ("node.id == ${BRAINWIRE_SWARM_NODE_ID:?", "node.role == manager")):
        try:
            validate_manifests(base.replace(field, replacement), auth)
        except ValueError:
            pass
        else:
            raise AssertionError("accepted unsafe manifest mutation")
    plan = outer_plan("brainwire-swarm-test", "brainwire-swarm-test-data", "brainwire-swarm-test-certs")
    assert "--privileged" in plan and "127.0.0.1::17687" in plan
    assert not any("docker.sock" in item or "2375" in item or "2376" in item for item in plan)
    ports = override("candidate")['services']['brainwire']['ports']
    assert ports == [{"target": 7687, "published": 17687, "protocol": "tcp", "mode": "ingress"}]
    return {"self_check": "PASS", "runtime_exercised": False,
            "checks": ["manifest owner/single-replica/stop-first mutations rejected", "owned DinD has no host socket or daemon port publication"]}


def run(image, rows):
    from neo4j import GraphDatabase, __version__ as driver_version
    from neo4j.exceptions import Neo4jError, ServiceUnavailable, SessionExpired
    sys.path.insert(0, str(ROOT / "examples/python"))
    from create_service_tokens import create
    if not 1 <= rows <= 10000:
        raise ValueError("rows must be between 1 and 10000")
    validate_manifests((ROOT / "deploy/swarm.yaml").read_text(), (ROOT / "deploy/swarm.auth.yaml").read_text())
    name = "brainwire-swarm-" + uuid.uuid4().hex[:12]
    volumes = [name + "-data", name + "-certs"]
    tokens = {}
    env = {key: value for key, value in os.environ.items() if not key.startswith("BRAINWIRE_")}
    owned_volumes = []
    started = False
    receipt = None

    def command(args, check=True, timeout=60):
        try:
            result = subprocess.run(args, cwd=ROOT, env=env, text=True, capture_output=True, timeout=timeout)
        except subprocess.TimeoutExpired as error:
            raise RuntimeError(f"owned Swarm command exceeded {timeout}s: {' '.join(args[:5])}") from error
        if check and result.returncode:
            detail = result.stdout + "\n" + result.stderr
            for token in tokens.values():
                detail = detail.replace(token, "[redacted]")
            raise RuntimeError(f"{' '.join(args[:5])}: {detail}")
        return result

    def host_state():
        swarm = json.loads(command(["docker", "info", "--format", "{{json .Swarm}}"], timeout=15).stdout)
        return {key: swarm.get(key) for key in ("LocalNodeState", "NodeID", "ControlAvailable")}

    before = host_state()
    if before["LocalNodeState"] != "inactive":
        raise RuntimeError("this rehearsal requires an inactive host Swarm; host state was not modified")
    image_id = command(["docker", "image", "inspect", "--format", "{{.Id}}", image]).stdout.strip()
    command(["docker", "pull", DIND], timeout=180)
    dind_info = json.loads(command(["docker", "image", "inspect", DIND]).stdout)[0]

    def inner(*args, check=True, timeout=60, variables=None):
        prefix = ["docker", "exec", "-e", "DOCKER_HOST=unix:///var/run/docker.sock"]
        for key, value in (variables or {}).items():
            prefix += ["-e", f"{key}={value}"]
        return command(prefix + [name, "docker", *args], check=check, timeout=timeout)

    def cli(*args):
        return inner("run", "--rm", "--network", "none", "-v", "brainwire-swarm-data:/data",
                     "-e", "BRAINWIRE_MEMORY_CACHE=64MiB", "-e", "BRAINWIRE_MEMORY_MEMTABLES=16MiB",
                     "-e", "BRAINWIRE_MEMORY_QUERY=64MiB", image, *args, timeout=90).stdout

    def service_container():
        ids = inner("ps", "-q", "--filter", f"label=com.docker.swarm.service.name={name}_brainwire").stdout.split()
        return ids[0] if len(ids) == 1 else None

    def ready(previous=None):
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline:
            identifier = service_container()
            if identifier and identifier != previous:
                probe = inner("exec", identifier, "brainwire", "probe", "--mode", "ready", "--timeout-ms", "500", check=False, timeout=8)
                if probe.returncode == 0:
                    data = json.loads(inner("inspect", identifier).stdout)[0]
                    if data["State"].get("Health", {}).get("Status") == "healthy":
                        return identifier, data
            time.sleep(0.5)
        logs = inner("service", "logs", "--tail", "40", name + "_brainwire", check=False).stdout
        raise RuntimeError("owned Swarm service did not become healthy: " + logs)

    def wait_stopped():
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            ids = inner("ps", "-q", "--filter", f"label=com.docker.swarm.service.name={name}_brainwire").stdout.strip()
            if not ids:
                return
            time.sleep(0.5)
        raise RuntimeError("service owner did not stop before offline audit")

    try:
        for volume in volumes:
            owned_volumes.append(volume)
            command(["docker", "volume", "create", "--label", f"brainwire.acceptance={name}", volume])
        started = True
        command(outer_plan(name, *volumes), timeout=90)
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            if inner("info", check=False, timeout=10).returncode == 0:
                break
            time.sleep(0.5)
        else:
            raise RuntimeError("owned Docker-in-Docker daemon failed readiness")
        port = json.loads(command(["docker", "inspect", name]).stdout)[0]["NetworkSettings"]["Ports"][f"{INNER_PORT}/tcp"][0]
        assert port["HostIp"] == "127.0.0.1"
        address = "bolt://127.0.0.1:" + port["HostPort"]
        with tempfile.TemporaryDirectory(prefix=name + "-") as directory:
            temporary = Path(directory)
            credentials = create(temporary / "auth", DATABASE)
            tokens.update({principal: json.loads((credentials / (principal + ".json")).read_text())["credentials"] for principal in ("reader", "writer", "health")})
            archive = temporary / "candidate.tar"
            command(["docker", "save", "--output", str(archive), image], timeout=180)
            command(["docker", "exec", name, "mkdir", "-p", "/work"])
            command(["docker", "cp", str(archive), name + ":/work/candidate.tar"], timeout=180)
            inner("load", "--input", "/work/candidate.tar", timeout=180)
            assert inner("image", "inspect", "--format", "{{.Id}}", image).stdout.strip() == image_id
            for source, target in ((ROOT / "deploy/swarm.yaml", "swarm.yaml"), (ROOT / "deploy/swarm.auth.yaml", "swarm.auth.yaml"),
                                   (credentials / "policy.json", "policy.json"), (credentials / "health.json", "health.json")):
                command(["docker", "cp", str(source), name + ":/work/" + target])
            overlay = temporary / "override.json"
            overlay.write_text(json.dumps(override(image)))
            command(["docker", "cp", str(overlay), name + ":/work/override.json"])
            inner("swarm", "init", "--advertise-addr", "eth0")
            node_id = inner("info", "--format", "{{.Swarm.NodeID}}").stdout.strip()
            assert node_id
            inner("volume", "create", "brainwire-swarm-data")
            inner("secret", "create", "brainwire-auth-policy", "/work/policy.json")
            inner("secret", "create", "brainwire-health-auth", "/work/health.json")
            cli("db", "create", DATABASE, "--ontology", "/opt/brainwire/example-schema.json")
            inner("stack", "deploy", "--resolve-image", "never", "-c", "/work/swarm.yaml", "-c", "/work/swarm.auth.yaml",
                  "-c", "/work/override.json", name, variables={"BRAINWIRE_IMAGE": image, "BRAINWIRE_SWARM_NODE_ID": node_id})
            first_id, first = ready()
            spec = json.loads(inner("service", "inspect", name + "_brainwire").stdout)[0]["Spec"]
            assert spec["Mode"]["Replicated"]["Replicas"] == 1
            assert spec["TaskTemplate"]["Placement"]["Constraints"] == ["node.id == " + node_id]
            assert spec["UpdateConfig"]["Order"] == "stop-first"
            assert first["Image"] == image_id
            assert next(m for m in first["Mounts"] if m["Destination"] == "/data")["Name"] == "brainwire-swarm-data"
            assert all(secret["File"]["Mode"] == 256 for secret in spec["TaskTemplate"]["ContainerSpec"]["Secrets"])

            def driver(principal):
                return GraphDatabase.driver(address, auth=(principal, tokens[principal]), connection_timeout=3,
                    connection_acquisition_timeout=5, max_transaction_retry_time=0)

            def query(principal, text, **params):
                with driver(principal) as client:
                    with client.session(database=DATABASE) as session:
                        return session.run(text, params).data()

            try:
                query("reader", "CREATE (c:Customer {entity_id:'reader-denied'})")
            except Neo4jError as error:
                assert error.code == "Neo.ClientError.Security.Forbidden"
            else:
                raise AssertionError("read principal acquired write permission")
            for offset in range(0, rows, 250):
                batch = [{"id": f"swarm-{i}", "email": f"swarm-{i}@example.test"} for i in range(offset, min(offset + 250, rows))]
                query("writer", "UNWIND $rows AS row MERGE (c:Customer {entity_id:row.id}) SET c.email=row.email", rows=batch)
            target = "swarm-0"
            pending = driver("writer")
            session = pending.session(database=DATABASE)
            transaction = session.begin_transaction()
            try:
                transaction.run("CREATE (c:Customer {entity_id:'pending',email:'pending@example.test'})").consume()
                transaction.run("MATCH (c:Customer {entity_id:$id}) SET c.email='pending-update@example.test'", id=target).consume()
                assert query("reader", "MATCH (c:Customer) RETURN count(*) AS n") == [{"n": rows}]
                inner("kill", "--signal", "KILL", first_id)
            finally:
                for resource in (transaction, session, pending):
                    try:
                        resource.close()
                    except (OSError, Neo4jError, ServiceUnavailable, SessionExpired):
                        pass
            second_id, second = ready(previous=first_id)
            assert second_id != first_id and second["Image"] == image_id
            assert next(m for m in second["Mounts"] if m["Destination"] == "/data")["Name"] == "brainwire-swarm-data"
            assert query("reader", "MATCH (c:Customer) RETURN count(*) AS n") == [{"n": rows}]
            assert query("reader", "MATCH (c:Customer {entity_id:'pending'}) RETURN c.entity_id AS id") == []
            assert query("reader", "MATCH (c:Customer {email:$email}) RETURN c.entity_id AS id", email="swarm-0@example.test") == [{"id": target}]
            assert query("reader", "MATCH (c:Customer {email:'pending-update@example.test'}) RETURN c.entity_id AS id") == []
            inner("service", "scale", name + "_brainwire=0")
            wait_stopped()
            audit = json.loads(cli("db", "check", DATABASE, "--json"))
            assert audit["status"] == "ok"
            checkpoint = json.loads(cli("db", "checkpoint", DATABASE, "--destination", "/data/restored"))
            assert checkpoint["status"] == "checkpoint_created"
            restored_audit = json.loads(cli("db", "check", "restored", "--json"))
            assert restored_audit["status"] == "ok"
            restored_rows = json.loads(cli("cypher", "--database", "/data/restored", "--query", "MATCH (c:Customer) RETURN count(*) AS n"))
            assert restored_rows["rows"] == [[rows]]
            inner("service", "scale", name + "_brainwire=1")
            ready(previous=second_id)
            assert query("reader", "MATCH (c:Customer) RETURN count(*) AS n") == [{"n": rows}]
            receipt = {"result": "PASS", "image": image, "imageId": image_id, "driverVersion": driver_version,
                "dindImage": DIND, "dindImageId": dind_info["Id"], "dindRepoDigests": dind_info.get("RepoDigests", []),
                "outerDaemon": name, "hostSwarmBefore": before, "rows": rows, "innerNodeId": node_id,
                "serviceTaskContainers": [first_id, second_id], "volume": "brainwire-swarm-data",
                "audit": audit, "restore_audit": restored_audit, "checkpoint": checkpoint,
                "manifest_sha256": {path.name: hashlib.sha256(path.read_bytes()).hexdigest() for path in (ROOT / "deploy/swarm.yaml", ROOT / "deploy/swarm.auth.yaml")},
                "checks": ["actual retained-owner Swarm manifests with one replica and protected external secrets",
                    "bounded writer commits indexed rows; reader writes denied",
                    "SIGKILL reschedules one task on the same volume; pending CREATE/index overwrite rolled back",
                    "scale to zero before offline audit; checkpoint restores into a fresh graph and passes queries/audit",
                    "original protected service resumes with acknowledged rows intact"],
                "scope": "isolated privileged DinD, one Swarm engine and retained local volume; no replication, machine-loss or distributed failover qualification"}
    finally:
        cleanup_errors = []
        if started:
            exists = command(["docker", "ps", "-aq", "--filter", f"name=^/{name}$"], check=False).stdout.strip()
            if exists:
                owner = command(["docker", "inspect", "--format", '{{index .Config.Labels "brainwire.acceptance"}}', name]).stdout.strip()
                if owner != name:
                    cleanup_errors.append("refused removal: outer daemon ownership label differs")
                else:
                    removal = command(["docker", "rm", "-f", "-v", name], check=False, timeout=90)
                    if removal.returncode:
                        cleanup_errors.append("owned outer daemon removal failed")
        for volume in owned_volumes:
            inspected = command(["docker", "volume", "inspect", "--format", '{{index .Labels "brainwire.acceptance"}}', volume], check=False)
            if inspected.returncode:
                continue
            if inspected.stdout.strip() != name:
                cleanup_errors.append("refused removal: outer volume ownership label differs")
                continue
            removal = command(["docker", "volume", "rm", volume], check=False, timeout=90)
            if removal.returncode:
                cleanup_errors.append("owned outer volume removal failed: " + volume)
        after = host_state()
        if after != before:
            cleanup_errors.append("host Swarm state changed")
        if cleanup_errors:
            raise RuntimeError("; ".join(cleanup_errors))
        if receipt is not None:
            receipt.update(hostSwarmAfter=after, hostSwarmUnchanged=True, ownedResourcesRemoved=True)
    return receipt


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", help="already-built frozen Brainwire image")
    parser.add_argument("--rows", type=int, default=1000)
    parser.add_argument("--self-check", action="store_true")
    args = parser.parse_args()
    if not args.self_check and not args.image:
        parser.error("--image is required; this harness never builds or chooses a candidate")
    print(json.dumps(self_check() if args.self_check else run(args.image, args.rows), indent=2))
