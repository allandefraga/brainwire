"""Exercise TLS-only Bolt with a synthetic CA, policy and disposable store."""
import argparse
from contextlib import ExitStack
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time

from compose_auth import fixture

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))


def run(binary):
    from neo4j import TrustCustomCAs
    from neo4j.exceptions import Neo4jError, ServiceUnavailable

    from brainwire import connect
    from examples.python.agent_memory import remember

    tokens, policy, health = fixture()
    for principal in policy["principals"]:
        principal["graphs"]["company_demo"] = principal["graphs"]["default"]
    with tempfile.TemporaryDirectory(prefix="bw-native-tls-") as directory:
        root = Path(directory)
        env = {key: value for key, value in os.environ.items() if not key.startswith("BRAINWIRE_")}
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        address = f"127.0.0.1:{port}"
        env.update(BRAINWIRE_SERVER_LISTEN=address, BRAINWIRE_SERVER_DATA_DIR=str(root / "store"))
        (root / "policy.json").write_text(json.dumps(policy))
        (root / "auth.json").write_text(json.dumps(health))
        (root / "auth.json").chmod(0o600)
        env["BRAINWIRE_SERVER_AUTH_FILE"] = str(root / "policy.json")

        def openssl(*args):
            subprocess.run(["openssl", *args], cwd=root, check=True, capture_output=True, timeout=10)

        (root / "ca.cnf").write_text(
            "[req]\ndistinguished_name=dn\nx509_extensions=ca\n[dn]\n"
            "[ca]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\n"
        )
        for name in ("ca", "wrong-ca"):
            openssl("req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
                    "-config", "ca.cnf", "-subj", f"/CN=Synthetic {name}",
                    "-keyout", f"{name}.key", "-out", f"{name}.crt")
        openssl("req", "-newkey", "rsa:2048", "-nodes", "-subj", "/CN=127.0.0.1",
                "-keyout", "server.key", "-out", "server.csr")
        for leaf, san in (("server", "IP:127.0.0.1"), ("wrong-san", "IP:127.0.0.2")):
            (root / "leaf.cnf").write_text(
                f"subjectAltName={san}\n"
                "basicConstraints=critical,CA:FALSE\nextendedKeyUsage=serverAuth\n"
            )
            openssl("x509", "-req", "-in", "server.csr", "-CA", "ca.crt",
                    "-CAkey", "ca.key", "-CAcreateserial", "-days", "1",
                    "-extfile", "leaf.cnf", "-out", f"{leaf}.crt")
        for path in root.glob("*.key"):
            path.chmod(0o600)
        cert = str(root / "server.crt")
        key = str(root / "server.key")
        for settings, expected in [
            ({"BRAINWIRE_SERVER_TLS_CERT_FILE": cert}, "must be configured together"),
            ({"BRAINWIRE_SERVER_TLS_CERT_FILE": cert,
              "BRAINWIRE_SERVER_TLS_KEY_FILE": str(root / "wrong-ca.key")}, "mismatched private key"),
            ({"BRAINWIRE_SERVER_TLS_CERT_FILE": str(root / "policy.json"),
              "BRAINWIRE_SERVER_TLS_KEY_FILE": key}, "certificate chain"),
        ]:
            result = subprocess.run([binary, "serve"], env=env | settings, cwd=root,
                                    timeout=4, capture_output=True, text=True)
            assert result.returncode != 0 and expected in result.stderr, result.stderr
            assert not (root / "store").exists(), "invalid TLS config opened the store"

        tls_env = env | {"BRAINWIRE_SERVER_TLS_CERT_FILE": cert,
                         "BRAINWIRE_SERVER_TLS_KEY_FILE": key}
        start = subprocess.run([binary, "db", "create", "default", "--ontology",
                                str(ROOT / "crates/core/tests/fixtures/graph_mapping.json")],
                               env=env, cwd=root, check=True, capture_output=True, timeout=10)
        assert start.returncode == 0
        subprocess.run([binary, "db", "create", "company_demo", "--ontology",
                        str(ROOT / "examples/company_brain/ontology.json")],
                       env=env, cwd=root, check=True, capture_output=True, timeout=10)
        def command(settings, *arguments):
            return subprocess.run(
                [binary, *arguments], env=env | settings, cwd=root, capture_output=True,
                text=True, timeout=4,
            )

        def probe(mode, settings, timeout=1000):
            return command(settings, "probe", "--address", address, "--mode", mode,
                           "--timeout-ms", str(timeout))

        def start_server(settings):
            return subprocess.Popen(
                [binary, "serve"], env=settings, cwd=root, stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )

        def wait_for_handshake(server, settings, timeout=8):
            deadline = time.monotonic() + timeout
            while server.poll() is None and time.monotonic() < deadline:
                if probe("handshake", settings).returncode == 0:
                    return
                time.sleep(.05)
            raise RuntimeError("Bolt probe did not become ready")

        def stop_server(server):
            if server.poll() is None:
                server.terminate()
                try:
                    assert server.wait(timeout=3) == 0
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait(timeout=3)
                    raise

        client_tls = {"BRAINWIRE_BOLT_TLS_CA_FILE": str(root / "ca.crt")}
        read_client_tls = client_tls | {"BRAINWIRE_BOLT_AUTH_FILE": str(root / "auth.json")}
        server = start_server(tls_env)
        try:
            wait_for_handshake(server, client_tls)

            assert probe("handshake", {}).returncode != 0
            assert probe("handshake", client_tls | {
                "BRAINWIRE_BOLT_AUTH_FILE": str(root / "absent.json"),
            }).returncode == 0
            for mode in ("live", "ready"):
                result = probe(mode, read_client_tls)
                assert result.returncode == 0, result.stderr
            result = command(
                read_client_tls, "shell", "--connect", address, "--graph", "default", "--view",
                "ontology", "--snapshot",
            )
            assert result.returncode == 0 and "Customer" in result.stdout, result
            result = probe("ready", {"BRAINWIRE_BOLT_TLS_CA_FILE": str(root / "wrong-ca.crt")})
            assert result.returncode != 0 and "TLS" in result.stderr, result.stderr

            def open_python(ca="ca.crt", principal="reader", database="default"):
                return connect(
                    f"bolt://{address}", database, auth=(principal, tokens[principal]),
                    encrypted=True, trusted_certificates=TrustCustomCAs(str(root / ca)),
                    connection_timeout=2, max_transaction_retry_time=0,
                )

            with open_python() as graph:
                assert graph.run_cypher("RETURN 1 AS ok") == [{"ok": 1}]
                try:
                    graph.run_cypher("CREATE (:Customer {entity_id: 'denied'})")
                except Neo4jError as error:
                    assert error.code.startswith("Neo.ClientError.Security."), error.code
                else:
                    raise AssertionError("TLS bypassed the graph write grant")
            def expect_python_certificate_error(ca="ca.crt"):
                try:
                    with open_python(ca):
                        raise AssertionError("unexpected acceptance of wrong CA or identity")
                except ServiceUnavailable as error:
                    assert "SSLCertVerificationError" in str(error), str(error)

            expect_python_certificate_error("wrong-ca.crt")
            with open_python(principal="writer", database="company_demo") as graph:
                graph.upsert_node("Project", "tls-project", {"name": "TLS project"})
                remember(graph, "tls-project", "Keep backups", "Keep one daily backup", "Ops review",
                         "https://example.com/synthetic-review")

            def recall_cli(ca):
                return subprocess.run(
                    [sys.executable, "-m", "examples.python.agent_memory", "--uri", f"bolt://{address}",
                     "--tls-ca", str(root / ca), "recall", "--project", "tls-project"],
                    env=env | {"BRAINWIRE_BOLT_AUTH_FILE": str(root / "auth.json")},
                    cwd=ROOT, capture_output=True, text=True, timeout=8,
                )

            recalled = recall_cli("ca.crt")
            assert recalled.returncode == 0, recalled.stderr
            records = json.loads(recalled.stdout)["records"]
            assert len(records) == 1 and records[0]["title"] == "Keep backups", records
            assert records[0]["source_url"] == "https://example.com/synthetic-review", records
            rejected = recall_cli("wrong-ca.crt")
            assert rejected.returncode != 0 and not rejected.stdout, rejected
            assert "SSLCertVerificationError" in rejected.stderr, rejected.stderr
            assert all(token not in rejected.stderr for token in tokens.values())

            with open_python(principal="writer") as graph:
                with graph.driver.session(database="default") as session:
                    with session.begin_transaction() as transaction:
                        transaction.run(
                            "CREATE (:Customer {entity_id: 'tls-committed', name: 'TLS canary'})"
                        ).consume()
                        transaction.commit()
            with ExitStack() as connections:
                for _ in range(64):
                    idle = socket.create_connection(("127.0.0.1", port), timeout=2)
                    connections.enter_context(idle)
                assert probe("handshake", client_tls, timeout=400).returncode != 0, (
                    "incomplete TLS handshakes did not count against the connection limit"
                )
                # The raw clients stay open, so a permit can return only through a server timeout.
                wait_for_handshake(server, client_tls, timeout=15)

            with socket.create_connection(("127.0.0.1", port), timeout=2) as idle:
                time.sleep(.1)
                before = time.monotonic()
                server.terminate()
                assert server.wait(timeout=3) == 0, "TLS handshake did not drain"
                assert time.monotonic() - before < 3
                # A server-initiated close may report a reset instead of EOF.
                try:
                    assert idle.recv(1) == b""
                except ConnectionResetError:
                    pass
        finally:
            stop_server(server)

        server = start_server(tls_env)
        try:
            wait_for_handshake(server, client_tls)
            with open_python() as graph:
                result = graph.run_cypher(
                    "MATCH (c:Customer {entity_id: 'tls-committed'}) RETURN c.name AS name"
                )
                assert result == [{"name": "TLS canary"}], result
        finally:
            stop_server(server)

        wrong_san_env = tls_env | {"BRAINWIRE_SERVER_TLS_CERT_FILE": str(root / "wrong-san.crt")}
        server = start_server(wrong_san_env)
        try:
            # A certificate signed by the trusted CA must still fail on the wrong identity.
            deadline = time.monotonic() + 8
            result = probe("handshake", client_tls)
            while server.poll() is None and time.monotonic() < deadline:
                if "NotValidForName" in result.stderr or "not valid for name" in result.stderr:
                    break
                time.sleep(.05)
                result = probe("handshake", client_tls)
            else:
                raise AssertionError(f"native client did not reject SAN mismatch: {result}")
            expect_python_certificate_error()
        finally:
            stop_server(server)

        server = start_server(env)
        try:
            wait_for_handshake(server, {})
            assert probe("handshake", client_tls).returncode != 0
        finally:
            stop_server(server)
    print("PASS: TLS-only Bolt, Python/agent CLI/probe/TUI verification, auth, restart, connection cap and drain")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    run(str(parser.parse_args().binary.resolve()))
