"""Owned synthetic TLS Compose qualification; requires Docker, OpenSSL and Neo4j.

No existing containers or stores are touched. The external edge uses strict
CA and hostname validation; the single-host proxy/backend connection is plaintext.
"""
import argparse
import json
import os
from pathlib import Path
import platform
import socket
import ssl
import subprocess
import sys
import tempfile
import time
import uuid
import warnings

from compose_auth import DATABASE, ROWS, fixture, self_check

PROXY_IMAGE = "haproxy:3.2.24-alpine@sha256:1aca32ee932b96d2a75f1882909325839dd23c138ba9c6fd26e4d88179f30f13"


def driver_tls_options(encrypted, custom_ca, ca_path):
    if not encrypted:
        return 'bolt', {}
    if custom_ca:
        from neo4j import TrustCustomCAs
        return 'bolt', {'encrypted': True, 'trusted_certificates': TrustCustomCAs(str(ca_path))}
    return 'bolt+s', {}


def configure_legacy_ciphers(context):
    try:
        context.set_ciphers('DEFAULT:@SECLEVEL=0')
    except ssl.SSLError:
        # LibreSSL does not implement the OpenSSL security-level syntax.
        context.set_ciphers('DEFAULT')


def wait_ready(probe, read, logs):
    from neo4j.exceptions import ServiceUnavailable

    deadline = time.monotonic() + 90
    last_error = None
    while time.monotonic() < deadline:
        if probe():
            try:
                assert read() == [{"ok": 1}]
                return
            except ServiceUnavailable as error:
                last_error = error
        time.sleep(0.25)
    raise RuntimeError(logs()) from last_error


def run(image, idle_pool=False, custom_ca=False):
    from neo4j import GraphDatabase, __version__ as driver_version
    from neo4j.exceptions import Neo4jError, ServiceUnavailable

    root = Path(__file__).resolve().parents[3]
    sys.path.insert(0, str(root))
    project = f"brainwire-tls-{uuid.uuid4().hex[:12]}"
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    tokens, policy, health = fixture()
    checks = []
    with tempfile.TemporaryDirectory(prefix=f"{project}-") as directory:
        temporary = Path(directory).resolve()
        temporary.chmod(0o700)
        env = {key: value for key, value in os.environ.items() if not key.startswith("BRAINWIRE_")}
        env.update(BRAINWIRE_IMAGE=image, BRAINWIRE_TLS_IMAGE=PROXY_IMAGE, BRAINWIRE_TLS_PORT=str(port),
                   BRAINWIRE_AUTH_POLICY_FILE=str(temporary / "policy.json"),
                   BRAINWIRE_HEALTH_AUTH_FILE=str(temporary / "health.json"),
                   BRAINWIRE_TLS_PEM_FILE=str(temporary / "server.pem"))
        for name, content in (("policy.json", policy), ("health.json", health)):
            path = temporary / name
            path.write_text(json.dumps(content))
            # Random synthetic fixture only: UID 10001 reads file-backed Compose secrets.
            path.chmod(0o444)
        base = ["docker", "compose", "--env-file", os.devnull, "-p", project,
                "-f", str(root / "deploy/compose.yaml"), "-f", str(root / "deploy/compose.auth.yaml"),
                "-f", str(root / "deploy/compose.tls.yaml")]

        def command(args, check=True, timeout=60):
            result = subprocess.run(args, cwd=root, env=env, capture_output=True, text=True, timeout=timeout)
            if check and result.returncode:
                message = result.stdout + result.stderr
                for token in tokens.values():
                    message = message.replace(token, "[redacted]")
                raise RuntimeError(message)
            return result

        def compose(*args, **kwargs):
            return command(base + list(args), **kwargs)

        def openssl(*args):
            return command(["openssl", *args])

        ca_config = temporary / "ca.cnf"
        ca_config.write_text("[req]\ndistinguished_name=dn\nx509_extensions=ca\n[dn]\n[ca]\nbasicConstraints=critical,CA:TRUE,pathlen:0\nkeyUsage=critical,keyCertSign,cRLSign\nsubjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid:always\n")
        for name in ("ca", "wrong-ca"):
            openssl("req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-sha256",
                    "-config", str(ca_config), "-subj", f"/CN=Brainwire synthetic {name}", "-keyout", str(temporary / f"{name}.key"),
                    "-out", str(temporary / f"{name}.crt"))
        openssl("req", "-newkey", "rsa:2048", "-nodes", "-subj", "/CN=localhost",
                "-keyout", str(temporary / "server.key"), "-out", str(temporary / "server.csr"))
        extensions = temporary / "extensions.cnf"
        extensions.write_text("subjectAltName=DNS:localhost\nextendedKeyUsage=serverAuth\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nsubjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid:always,issuer\n")
        openssl("x509", "-req", "-in", str(temporary / "server.csr"), "-CA", str(temporary / "ca.crt"),
                "-CAkey", str(temporary / "ca.key"), "-CAcreateserial", "-days", "1", "-sha256",
                "-extfile", str(extensions), "-out", str(temporary / "server.crt"))
        openssl("verify", "-CAfile", str(temporary / "ca.crt"), "-purpose", "sslserver", str(temporary / "server.crt"))
        pem = temporary / "server.pem"
        pem.write_bytes((temporary / "server.crt").read_bytes() + (temporary / "server.key").read_bytes())
        for path in temporary.glob("*.key"):
            path.chmod(0o600)
        # Synthetic one-day key only; mounted image UID 99 must read it on Linux.
        pem.chmod(0o444)
        ca = temporary / "ca.crt"
        previous_ca = os.environ.get("SSL_CERT_FILE")
        os.environ["SSL_CERT_FILE"] = str(ca)

        def driver(auth, host="localhost", encrypted=True, **extra):
            scheme, tls_options = driver_tls_options(encrypted, custom_ca, os.environ["SSL_CERT_FILE"])
            return GraphDatabase.driver(f"{scheme}://{host}:{port}", auth=auth,
                                       connection_timeout=3, connection_acquisition_timeout=5,
                                       max_transaction_retry_time=0, **tls_options, **extra)

        def query(auth, text, params=None, **kwargs):
            with driver(auth, **kwargs) as connection:
                with connection.session(database=DATABASE) as session:
                    return session.run(text, params or {}).data()

        def denied(auth, code):
            try:
                query(auth, "MERGE (c:Customer {entity_id:'denied'})")
            except Neo4jError as error:
                assert error.code == code, error.code
            else:
                raise AssertionError(f"expected {code}")

        def ready():
            wait_ready(
                lambda: compose("exec", "-T", "brainwire", "brainwire", "probe", "--mode", "ready",
                                "--timeout-ms", "500", check=False, timeout=8).returncode == 0,
                lambda: query(("reader", tokens["reader"]), "RETURN 1 AS ok"),
                lambda: compose("logs", "--tail", "50", check=False).stdout,
            )

        receipt = None
        try:
            rendered = json.loads(compose("config", "--format", "json").stdout)
            assert not rendered["services"]["brainwire"].get("ports"), "plaintext backend published"
            assert rendered["services"]["tls"]["ports"][0]["host_ip"] == "127.0.0.1"
            compose("run", "--rm", "--no-deps", "brainwire", "db", "apply-ontology", DATABASE,
                    "/opt/brainwire/example-schema.json")
            compose("up", "-d", "--no-build", timeout=120)
            ready()
            if custom_ca:
                from brainwire import connect
                from neo4j import TrustCustomCAs
                with connect(f"bolt://localhost:{port}", DATABASE,
                             auth=("reader", tokens["reader"]), encrypted=True,
                             trusted_certificates=TrustCustomCAs(str(ca))) as graph:
                    assert graph.run_cypher("RETURN 1 AS ok") == [{"ok": 1}]
                checks.append("documented Python connect helper accepts an explicitly trusted CA")
            compose("exec", "-T", "tls", "haproxy", "-c", "-f", "/usr/local/etc/haproxy/haproxy.cfg")
            checks.append("strict CA and localhost hostname verification succeeds" +
                          (" with explicit CA" if custom_ca else " with bolt+s"))
            identifiers = {name: compose("ps", "-q", name).stdout.strip() for name in ("brainwire", "tls")}
            assert all(identifiers.values())
            inspections = {name: json.loads(command(["docker", "inspect", identifier]).stdout)[0]
                           for name, identifier in identifiers.items()}
            assert not inspections["brainwire"]["HostConfig"]["PortBindings"]
            for name, destinations in (("tls", ("/run/secrets/brainwire-tls.pem", "/usr/local/etc/haproxy/haproxy.cfg")),
                                       ("brainwire", ("/run/secrets/brainwire-policy.json", "/run/secrets/brainwire-health-auth.json"))):
                for destination in destinations:
                    assert not next(m for m in inspections[name]["Mounts"] if m["Destination"] == destination)["RW"]
            checks.append("backend host port absent; TLS loopback port only; certificates/config/auth mounts read-only")
            for kwargs, trust in (({}, temporary / "wrong-ca.crt"), ({"host": "127.0.0.1"}, ca),
                                  ({"encrypted": False}, ca)):
                os.environ["SSL_CERT_FILE"] = str(trust)
                try:
                    query(("reader", tokens["reader"]), "RETURN 1", **kwargs)
                except ServiceUnavailable:
                    pass
                else:
                    raise AssertionError("wrong CA, wrong hostname or plaintext unexpectedly accepted")
            os.environ["SSL_CERT_FILE"] = str(ca)
            context = ssl.create_default_context(cafile=str(ca))
            with context.wrap_socket(socket.create_connection(("127.0.0.1", port), timeout=3), server_hostname="localhost") as connection:
                tls_version = connection.version()
                assert tls_version in ("TLSv1.2", "TLSv1.3")
            context.maximum_version = ssl.TLSVersion.TLSv1_2
            with context.wrap_socket(socket.create_connection(("127.0.0.1", port), timeout=3), server_hostname="localhost") as connection:
                assert connection.version() == "TLSv1.2"
            with warnings.catch_warnings():
                warnings.simplefilter("ignore", DeprecationWarning)
                legacy = ssl.create_default_context(cafile=str(ca))
                legacy.minimum_version = legacy.maximum_version = ssl.TLSVersion.TLSv1_1
                configure_legacy_ciphers(legacy)
            try:
                with legacy.wrap_socket(socket.create_connection(("127.0.0.1", port), timeout=3), server_hostname="localhost"):
                    raise AssertionError("TLS 1.1 unexpectedly accepted")
            except ssl.SSLError as error:
                assert error.reason == "TLSV1_ALERT_PROTOCOL_VERSION", error
            checks.append(f"default negotiation uses {tls_version}; TLS 1.2 succeeds; TLS 1.1 receives protocol-version rejection")
            checks.append("wrong CA, hostname mismatch and plaintext driver connections rejected")
            denied(None, "Neo.ClientError.Security.Unauthorized")
            denied(("reader", "0" * 64), "Neo.ClientError.Security.Unauthorized")
            reader, writer = ("reader", tokens["reader"]), ("writer", tokens["writer"])
            denied(reader, "Neo.ClientError.Security.Forbidden")
            rows = [{"id": f"tls-{i}", "email": f"tls-{i}@example.test"} for i in range(ROWS)]
            query(writer, "UNWIND $rows AS row MERGE (c:Customer {entity_id:row.id}) SET c.email=row.email", {"rows": rows})
            assert query(reader, "MATCH (c:Customer) RETURN count(*) AS n") == [{"n": ROWS}]
            checks.append("Bolt auth/grants preserved through TLS; writer imports 1000 indexed records")
            idle_pool_receipt = None
            if idle_pool:
                def frontend_connections():
                    tables = compose("exec", "-T", "tls", "cat", "/proc/net/tcp", "/proc/net/tcp6").stdout
                    return {fields[2] for line in tables.splitlines()
                            if len(fields := line.split()) >= 4 and fields[3] == "01"
                            and fields[1].endswith(":1E08")}

                with driver(reader, liveness_check_timeout=30, max_connection_pool_size=1) as pooled:
                    with pooled.session(database=DATABASE) as session:
                        assert session.run("RETURN 1 AS ok").single()["ok"] == 1
                    before = frontend_connections()
                    assert len(before) == 1, before
                    began = time.monotonic()
                    time.sleep(65)
                    assert not before & frontend_connections(), "proxy did not expire the idle TLS socket"
                    with pooled.session(database=DATABASE) as session:
                        assert session.run("RETURN 1 AS ok").single()["ok"] == 1
                    after = frontend_connections()
                    assert len(after) == 1 and before.isdisjoint(after), (before, after)
                    idle_pool_receipt = {"idle_seconds": round(time.monotonic() - began, 3),
                                         "proxy_idle_timeout_seconds": 60, "liveness_check_timeout_seconds": 30,
                                         "first_reuse": "PASS", "new_frontend_peer_observed": True}
                checks.append("65s idle pool crosses 60s proxy expiry; 30s liveness first reuse succeeds on a new TLS connection")
            with driver(writer) as connection:
                with connection.session(database=DATABASE) as session:
                    transaction = session.begin_transaction()
                    transaction.run("CREATE (c:Customer {entity_id:'pending-tls', email:'pending@example.test'})").consume()
                    transaction.rollback()
            assert query(reader, "MATCH (c:Customer {entity_id:'pending-tls'}) RETURN c.entity_id") == []
            compose("kill", "-s", "SIGKILL", "brainwire")
            compose("start", "brainwire")
            ready()
            assert query(reader, "MATCH (c:Customer) RETURN count(*) AS n") == [{"n": ROWS}]
            assert query(reader, "MATCH (c:Customer) WHERE c.email='tls-500@example.test' RETURN c.entity_id AS id") == [{"id": "tls-500"}]
            checks.append("explicit rollback succeeds; committed rows/index survive backend SIGKILL/start and TLS reconnect")
            compose("stop", timeout=90)
            audit = json.loads(compose("run", "--rm", "--no-deps", "brainwire", "db", "check", DATABASE, "--json").stdout)
            assert audit["status"] == "ok"
            receipt = {"result": "PASS", "driverVersion": driver_version, "image": image,
                       "pythonVersion": platform.python_version(), "opensslVersion": ssl.OPENSSL_VERSION,
                       "imageId": inspections["brainwire"]["Image"], "proxyImage": PROXY_IMAGE,
                       "proxyImageId": inspections["tls"]["Image"], "tlsVersion": tls_version,
                       "project": project, "rows": ROWS, "checks": checks, "audit": audit,
                       "idlePool": idle_pool_receipt,
                       "scope": "single-host client/proxy TLS; backend plaintext; no Kubernetes/Swarm qualification"}
        finally:
            if previous_ca is None:
                os.environ.pop("SSL_CERT_FILE", None)
            else:
                os.environ["SSL_CERT_FILE"] = previous_ca
            compose("down", "--volumes", "--remove-orphans", timeout=90)
            for resource, args in (("containers", ["docker", "ps", "-aq"]), ("volumes", ["docker", "volume", "ls", "-q"]),
                                   ("networks", ["docker", "network", "ls", "-q"])):
                assert not command(args + ["--filter", f"label=com.docker.compose.project={project}"]).stdout.strip(), resource
        receipt["ownedResourcesRemoved"] = True
        return receipt


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", default="brainwire:mvp-20261012")
    parser.add_argument("--self-check", action="store_true")
    parser.add_argument("--idle-pool", action="store_true", help="Qualify first reuse after 65 seconds of proxy idle expiry")
    parser.add_argument("--custom-ca", action="store_true", help="Load the synthetic CA explicitly instead of relying on SSL_CERT_FILE")
    options = parser.parse_args()
    print(json.dumps(self_check() if options.self_check else run(options.image, options.idle_pool, options.custom_ca), indent=2))
