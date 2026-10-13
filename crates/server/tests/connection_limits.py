"""Owned protected native request-decode and authenticated IO qualification."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import socket
import subprocess
import tempfile
import time

from compose_auth import DATABASE, fixture
from termination_recovery import HANDSHAKE, exact, message, pack, request

ROOT = Path(__file__).resolve().parents[3]


def amplified_run():
    query = "CREATE (c:Customer {entity_id:'decode-denied', email:'decode-denied@example.test'})"
    params = b"\xa1" + pack("amplified") + b"\xd5\x20\x00" + b"\xc0" * 8192
    return b"\xb3\x10" + pack(query) + params + pack({"db": DATABASE})


def self_check():
    payload = amplified_run()
    assert len(payload) < 65535 and payload.count(b"\xc0") == 8192
    assert payload[:2] == bytes.fromhex("b310")
    return {"fixture_check": "PASS", "amplified_encoded_bytes": len(payload), "runtime_exercised": False}


def run(binary, default_idle=False):
    from neo4j import GraphDatabase, __version__ as driver_version
    from neo4j.exceptions import ServiceUnavailable, SessionExpired

    report = {"binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(), "driver": driver_version,
              "os": platform.platform(), "default_idle": default_idle, "checks": []}
    with tempfile.TemporaryDirectory(prefix="brainwire-connection-limits-") as folder:
        path = Path(folder).resolve()
        path.chmod(0o700)
        tokens, policy, health = fixture()
        for name, value in (("policy.json", policy), ("health.json", health)):
            target = path / name
            target.write_text(json.dumps(value))
            target.chmod(0o600)
        with socket.socket() as reserved:
            reserved.bind(("127.0.0.1", 0))
            address = reserved.getsockname()
        endpoint = "%s:%s" % address
        env = {key: value for key, value in os.environ.items() if not key.startswith("BRAINWIRE_")}
        env.update(BRAINWIRE_SERVER_DATA_DIR=str(path / "data"), BRAINWIRE_SERVER_LISTEN=endpoint,
                   BRAINWIRE_SERVER_DEFAULT_DATABASE=DATABASE, BRAINWIRE_SERVER_AUTH_FILE=str(path / "policy.json"),
                   BRAINWIRE_BOLT_AUTH_FILE=str(path / "health.json"), BRAINWIRE_MEMORY_CACHE="64MiB",
                   BRAINWIRE_MEMORY_MEMTABLES="16MiB", BRAINWIRE_MEMORY_QUERY="64MiB",
                   BRAINWIRE_SERVER_MAX_MESSAGE="8MiB", BRAINWIRE_SERVER_MAX_DECODED_MESSAGE="64KiB")
        if not default_idle:
            env["BRAINWIRE_SERVER_IO_TIMEOUT_MS"] = "200"
        server = None
        owned_sockets = []
        log = (path / "server.log").open("w")

        def cli(*args):
            return subprocess.run([str(binary), *args], env=env, cwd=ROOT, capture_output=True,
                                  text=True, check=True, timeout=30).stdout

        def start():
            nonlocal server
            server = subprocess.Popen([str(binary), "serve"], env=env, cwd=ROOT, stdout=log, stderr=log)
            for _ in range(150):
                assert server.poll() is None, "owned server exited during readiness"
                try:
                    cli("probe", "--address", endpoint, "--timeout-ms", "100")
                    return
                except subprocess.CalledProcessError:
                    time.sleep(.03)
            raise AssertionError("owned server readiness timeout")

        def connect(name="reader", **kwargs):
            return GraphDatabase.driver("bolt://" + endpoint, auth=(name, tokens[name]), connection_timeout=3,
                                       connection_acquisition_timeout=5, max_transaction_retry_time=0, **kwargs)

        def query(text, name="reader"):
            with connect(name) as driver:
                with driver.session(database=DATABASE) as session:
                    return session.run(text).data()

        def raw(name="reader"):
            connection = socket.create_connection(address, timeout=5)
            owned_sockets.append(connection)
            connection.sendall(HANDSHAKE)
            assert exact(connection, 4) == bytes.fromhex("00000405")
            request(connection, 0x01, {"user_agent": "brainwire-owned-limits"})
            assert message(connection)[:2] == bytes.fromhex("b170")
            request(connection, 0x6a, {"scheme": "basic", "principal": name, "credentials": tokens[name]})
            assert message(connection)[:2] == bytes.fromhex("b170")
            return connection

        def scalar(driver):
            with driver.session(database=DATABASE) as session:
                assert session.run("RETURN 1 AS ok").single()["ok"] == 1

        def pool_wait(seconds):
            outcomes = {}
            for mode, options in (("default", {}), ("liveness", {"liveness_check_timeout": 1})):
                with connect(**options) as driver:
                    scalar(driver)
                    time.sleep(seconds)
                    try:
                        scalar(driver)
                        outcomes[mode] = "first reuse succeeded"
                    except (ServiceUnavailable, SessionExpired):
                        outcomes[mode] = "first reuse failed; explicit new connection succeeded"
                        assert query("RETURN 1 AS ok") == [{"ok": 1}]
                        if mode == "liveness":
                            raise AssertionError("liveness pool failed its first reuse")
            return outcomes

        try:
            cli("db", "create", DATABASE, "--ontology", str(ROOT / "examples/python/schema.json"))
            config = cli("config", "show")
            configured_timeout = re.search(r"server.io_timeout_ms\s*=\s*(\d+)", config)
            assert configured_timeout and int(configured_timeout.group(1)) == (300000 if default_idle else 200), config
            report["effective_connection_config"] = [line for line in config.splitlines()
                if line.startswith(("server.max_message", "server.max_decoded_message", "server.io_timeout_ms"))]
            start()
            query("CREATE (c:Customer {entity_id:'baseline', email:'old@example.test'})", "writer")
            if default_idle:
                # Both pools and raw authenticated socket become idle concurrently.
                ordinary, live = connect(), connect(liveness_check_timeout=1)
                try:
                    scalar(ordinary)
                    scalar(live)
                    connection = raw()
                    connection.settimeout(310)
                    began = time.monotonic()
                    assert connection.recv(1) == b""
                    elapsed = time.monotonic() - began
                    assert 295 <= elapsed <= 310, elapsed
                    report["default_idle_observed_seconds"] = elapsed
                    time.sleep(1)
                    scalar(live)
                    report["liveness_pool"] = "first reuse succeeded after default expiry"
                    try:
                        scalar(ordinary)
                        report["default_pool"] = "first reuse succeeded after default expiry"
                    except (ServiceUnavailable, SessionExpired):
                        report["default_pool"] = "first reuse failed; explicit new connection succeeded"
                        assert query("RETURN 1 AS ok") == [{"ok": 1}]
                finally:
                    ordinary.close()
                    live.close()
                report["checks"].append("unoverridden 300000ms authenticated idle budget expires; liveness pool reconnects")
            else:
                connection = raw("writer")
                payload = amplified_run()
                connection.sendall(len(payload).to_bytes(2, "big") + payload + b"\0\0")
                failure = message(connection)
                assert failure[:2] == bytes.fromhex("b17f")
                assert b"Neo.ClientError.Request.Invalid" in failure, failure
                assert connection.recv(1) == b""
                assert query("MATCH (c:Customer {entity_id:'decode-denied'}) RETURN c.entity_id") == []
                assert query("RETURN 1 AS ok") == [{"ok": 1}]
                report["decoded_cap_bytes"] = 65536
                report["amplified_encoded_bytes"] = len(payload)
                report["checks"].append("8KiB NULL-list RUN exceeds 64KiB decoded cap before mutation; fatal Invalid; healthy subsequent connection")
                for case in ("NOOP", "partial frame"):
                    connection = raw()
                    connection.settimeout(.02)
                    began = time.monotonic()
                    if case == "partial frame":
                        connection.sendall(b"\x04\x00")
                    while True:
                        try:
                            connection.sendall(b"\0\0" if case == "NOOP" else b"\xc0")
                            if connection.recv(1) == b"":
                                break
                        except (BrokenPipeError, ConnectionResetError):
                            break
                        except socket.timeout:
                            pass
                        assert time.monotonic() - began < .8, "drip renewed absolute IO timer"
                        time.sleep(.02)
                    elapsed = time.monotonic() - began
                    assert .12 <= elapsed < .8, elapsed
                    report[case + "_close_seconds"] = elapsed
                    connection.close()
                report["checks"].append("authenticated NOOP and partial-byte drips cannot renew 200ms complete-request IO budget")
                with connect("writer") as driver:
                    with driver.session(database=DATABASE) as session:
                        tx = session.begin_transaction()
                        try:
                            tx.run("CREATE (c:Customer {entity_id:'idle-pending', email:'idle-pending@example.test'})").consume()
                            tx.run("MATCH (c:Customer {entity_id:'baseline'}) SET c.email='pending@example.test'").consume()
                            time.sleep(.3)
                            try:
                                tx.run("RETURN 1").consume()
                            except (ServiceUnavailable, SessionExpired):
                                pass
                            else:
                                raise AssertionError("idle transaction retained connection")
                        finally:
                            try:
                                tx.close()
                            except (ServiceUnavailable, SessionExpired):
                                pass
                assert query("MATCH (c:Customer {entity_id:'idle-pending'}) RETURN c.entity_id") == []
                assert query("MATCH (c:Customer) WHERE c.email='old@example.test' RETURN c.entity_id AS id") == [{"id": "baseline"}]
                assert query("MATCH (c:Customer) WHERE c.email='pending@example.test' RETURN c.entity_id") == []
                report["checks"].append("idle explicit transaction closes and rolls back pending CREATE/index overwrite")
                with connect() as driver:
                    for _ in range(6):
                        scalar(driver)
                        time.sleep(.07)
                report["checks"].append("healthy complete SDK requests renew IO budget across >200ms")
                report["pool_reuse"] = pool_wait(1.2)
            server.terminate()
            server.wait(timeout=5)
            server = None
            report["audit"] = json.loads(cli("db", "check", DATABASE, "--json"))
            assert report["audit"]["status"] == "ok"
            report["result"] = "PASS"
        finally:
            for connection in owned_sockets:
                connection.close()
            if server is not None and server.poll() is None:
                server.kill()
            if server is not None:
                server.wait(timeout=5)
            log.close()
    assert not path.exists()
    report["ownedResourcesRemoved"] = True
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/brainwire")
    parser.add_argument("--self-check", action="store_true")
    parser.add_argument("--default-idle", action="store_true", help="Wait ~301s to qualify unoverridden 300s idle budget")
    options = parser.parse_args()
    print(json.dumps(self_check() if options.self_check else run(options.binary.resolve(), options.default_idle), indent=2))
