"""Exercise the native TLS Compose profile with a prebuilt local image.

Requires Docker Compose, openssl and the Neo4j Python driver. Uses a disposable
project and synthetic credentials. The host Docker daemon must mount the fixture
directory and forward published loopback ports to this process.
"""

import argparse
import json
import os
from pathlib import Path
import socket
import ssl
import subprocess
import tempfile
import uuid

from compose_auth import DATABASE, fixture
from native_tls_compose_config import ROOT, SECRETS


HOST = "127.0.0.1"


def generate_certificates(directory):
    def openssl(*args):
        result = subprocess.run(["openssl", *args], cwd=directory, capture_output=True,
                                text=True, timeout=15)
        if result.returncode:
            raise RuntimeError(result.stderr)

    (directory / "ca.cnf").write_text(
        "[req]\ndistinguished_name=dn\nx509_extensions=ca\n[dn]\n"
        "[ca]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\n"
    )
    for name in ("ca", "wrong-ca"):
        openssl("req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
                "-config", "ca.cnf", "-subj", f"/CN=Synthetic {name}",
                "-keyout", f"{name}.key", "-out", f"{name}.pem")
    openssl("req", "-newkey", "rsa:2048", "-nodes", "-subj", f"/CN={HOST}",
            "-keyout", "server.key", "-out", "server.csr")
    (directory / "leaf.cnf").write_text(
        f"subjectAltName=IP:{HOST}\n"
        "basicConstraints=critical,CA:FALSE\nextendedKeyUsage=serverAuth\n"
    )
    openssl("x509", "-req", "-in", "server.csr", "-CA", "ca.pem", "-CAkey", "ca.key",
            "-CAcreateserial", "-days", "1", "-extfile", "leaf.cnf", "-out", "server.pem")


def run(image, fixture_parent):
    from neo4j import GraphDatabase, TrustCustomCAs
    from neo4j.exceptions import Neo4jError

    project = f"bw-native-tls-{uuid.uuid4().hex}"
    tokens, policy, health_auth = fixture()
    with socket.socket() as listener:
        listener.bind((HOST, 0))
        address = listener.getsockname()
    uri = f"bolt://{HOST}:{address[1]}"

    with tempfile.TemporaryDirectory(prefix=f"{project}-", dir=fixture_parent) as temporary:
        directory = Path(temporary).resolve()
        (directory / "policy.json").write_text(json.dumps(policy))
        (directory / "health.json").write_text(json.dumps(health_auth))
        generate_certificates(directory)

        env = {key: value for key, value in os.environ.items()
               if not key.startswith(("BRAINWIRE_", "COMPOSE_"))}
        env.update(BRAINWIRE_NATIVE_TLS_IMAGE=image, BRAINWIRE_BIND_PORT=str(address[1]))
        secret_files = {
            "BRAINWIRE_AUTH_POLICY_FILE": "policy.json",
            "BRAINWIRE_HEALTH_AUTH_FILE": "health.json",
            "BRAINWIRE_NATIVE_TLS_CHAIN_FILE": "server.pem",
            "BRAINWIRE_NATIVE_TLS_KEY_FILE": "server.key",
            "BRAINWIRE_NATIVE_TLS_CA_FILE": "ca.pem",
        }
        for variable, filename in secret_files.items():
            path = directory / filename
            # File-backed Compose secrets retain host mode. Only synthetic secrets
            # inside this private fixture directory are made readable by UID 10001.
            path.chmod(0o444)
            env[variable] = str(path)

        base = ["docker", "compose", "--env-file", os.devnull, "-p", project]
        for filename in ("compose.yaml", "compose.auth.yaml", "compose.native-tls.yaml"):
            base.extend(("-f", str(ROOT / "deploy" / filename)))

        def command(args, *, timeout=30):
            result = subprocess.run(args, cwd=ROOT, env=env, capture_output=True,
                                    text=True, timeout=timeout)
            if result.returncode:
                output = f"{result.stdout}\n{result.stderr}"
                for token in tokens.values():
                    output = output.replace(token, "[redacted]")
                raise RuntimeError(f"{' '.join(args)} failed:\n{output}")
            return result.stdout.strip()

        def compose(*args, timeout=30):
            return command([*base, *args], timeout=timeout)

        def assert_project_empty():
            for args in (("ps", "-aq"), ("network", "ls", "-q"), ("volume", "ls", "-q")):
                assert not command(["docker", *args, "--filter",
                                    f"label=com.docker.compose.project={project}"]), args

        def verified_container():
            container_id = compose("ps", "-q", "brainwire")
            assert container_id, "Brainwire container is missing"
            container, = json.loads(command(["docker", "inspect", container_id]))
            assert container["Image"] == image_id, "container used a different image"
            assert container["State"]["Health"]["Status"] == "healthy"
            mounts = {mount["Destination"]: mount for mount in container["Mounts"]}
            data = mounts["/data"]
            assert data["Type"] == "volume" and data["Name"] == f"{project}_brainwire-data"
            for _, _, _, filename in SECRETS:
                assert not mounts[f"/run/secrets/{filename}"]["RW"], filename
            return container["State"]["StartedAt"]

        def query(auth, text):
            with GraphDatabase.driver(
                uri, auth=auth, encrypted=True,
                trusted_certificates=TrustCustomCAs(str(directory / "ca.pem")),
                connection_timeout=3, connection_acquisition_timeout=5,
                max_transaction_retry_time=0,
            ) as driver:
                with driver.session(database=DATABASE) as session:
                    return session.run(text).data()

        def denied(auth, text, code):
            try:
                query(auth, text)
            except Neo4jError as error:
                assert error.code == code, error.code
            else:
                raise AssertionError(f"expected {code}")

        image_id = command(["docker", "image", "inspect", "--format", "{{.Id}}", image])
        assert_project_empty()
        try:
            compose("run", "--pull", "never", "--rm", "--no-deps", "-T", "brainwire", "db",
                    "create", DATABASE, "--ontology", "/opt/brainwire/example-schema.json",
                    timeout=90)
            compose("up", "--no-build", "--pull", "never", "--wait", "--wait-timeout", "90",
                    timeout=100)
            started_at = verified_container()
            reader, writer = (("reader", tokens["reader"]), ("writer", tokens["writer"]))
            assert query(reader, "RETURN 1 AS ok") == [{"ok": 1}]
            denied(None, "RETURN 1", "Neo.ClientError.Security.Unauthorized")
            denied(("reader", "0" * 64), "RETURN 1", "Neo.ClientError.Security.Unauthorized")

            wrong_ca = ssl.create_default_context(cafile=str(directory / "wrong-ca.pem"))
            with socket.create_connection(address, timeout=3) as connection:
                try:
                    with wrong_ca.wrap_socket(connection, server_hostname=HOST):
                        raise AssertionError("TLS handshake accepted an unrelated CA")
                except ssl.SSLCertVerificationError:
                    pass

            with socket.create_connection(address, timeout=3) as connection:
                try:
                    # Bolt magic followed by a Bolt 5.4 offer and three empty offers.
                    connection.sendall(bytes.fromhex("6060b01700000405000000000000000000000000"))
                    # TLS may close the connection or return a TLS alert (0x15).
                    assert connection.recv(1) in (b"", b"\x15"), "server replied to plaintext Bolt"
                except ConnectionResetError:
                    pass

            denied(reader, "CREATE (:Customer {entity_id:'denied'})",
                   "Neo.ClientError.Security.Forbidden")
            query(writer, "CREATE (:Customer {entity_id:'tls-runtime', name:'TLS canary'})")
            read_back = "MATCH (c:Customer {entity_id:'tls-runtime'}) RETURN c.name AS name"
            assert query(reader, read_back) == [{"name": "TLS canary"}]

            compose("restart", "brainwire", timeout=90)
            compose("up", "--no-recreate", "--no-build", "--pull", "never", "--wait",
                    "--wait-timeout", "90", timeout=100)
            assert verified_container() != started_at, "container did not restart"
            assert query(reader, read_back) == [{"name": "TLS canary"}]
        finally:
            compose("down", "--volumes", "--remove-orphans", timeout=90)
            assert_project_empty()

    print(json.dumps({"image": image_id, "health": "healthy before and after restart",
                      "verified_bolt": "passed", "wrong_ca": "rejected", "plaintext": "rejected",
                      "authentication_and_grants": "passed", "restart_persistence": "passed"}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("image", help="prebuilt image available to the local Docker daemon")
    parser.add_argument("--fixture-parent", type=Path,
                        help="parent directory shared with the Docker daemon")
    args = parser.parse_args()
    run(args.image, args.fixture_parent)
