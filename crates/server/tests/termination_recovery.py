"""Protected native SIGTERM drain and recovery with owned synthetic stores.

SIGTERM starts a bounded drain. This checks refused admission, an explicit
transaction committed during the drain, rollback of a transaction abandoned at
the grace deadline, uncertain unacknowledged RUN atomicity, a stalled reader
bounded by the deadline and prompt exit with only idle sessions.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import socket
import struct
import subprocess
import tempfile
import time

from compose_auth import DATABASE, fixture

ROOT = Path(__file__).resolve().parents[3]
HANDSHAKE = bytes.fromhex("6060b01700000405000000000000000000000000")
ROWS = 1000
UNCERTAIN_ROWS = 200
GRACE_MS = 3000


def pack(value):
    if isinstance(value, str):
        data = value.encode("utf-8")
        if len(data) < 16:
            return bytes([0x80 + len(data)]) + data
        if len(data) <= 255:
            return b"\xd0" + bytes([len(data)]) + data
        if len(data) <= 65535:
            return b"\xd1" + len(data).to_bytes(2, "big") + data
        raise ValueError("fixture string exceeds 65535 bytes")
    if isinstance(value, int):
        if -16 <= value < 128:
            return bytes([value & 255])
        return b"\xc9" + struct.pack(">h", value)
    if isinstance(value, dict):
        assert len(value) < 16
        return bytes([0xa0 + len(value)]) + b"".join(pack(k) + pack(v) for k, v in value.items())
    if isinstance(value, list):
        assert len(value) <= 255
        header = bytes([0x90 + len(value)]) if len(value) < 16 else b"\xd4" + bytes([len(value)])
        return header + b"".join(pack(item) for item in value)
    raise TypeError(type(value).__name__)


def request(stream, signature, *fields):
    payload = bytes([0xb0 + len(fields), signature]) + b"".join(pack(field) for field in fields)
    assert len(payload) <= 65535
    stream.sendall(len(payload).to_bytes(2, "big") + payload + b"\0\0")


def exact(stream, count):
    result = bytearray()
    while len(result) < count:
        part = stream.recv(count - len(result))
        if not part:
            raise EOFError("owned connection closed")
        result.extend(part)
    return bytes(result)


def message(stream):
    payload = bytearray()
    while size := int.from_bytes(exact(stream, 2), "big"):
        if len(payload) + size > 8 * 1024 * 1024:
            raise ValueError("fixture response exceeds 8 MiB")
        payload.extend(exact(stream, size))
    return bytes(payload)


def self_check():
    assert pack(-1) == b"\xff" and pack("n") == b"\x81n"
    assert pack({"n": -1}) == bytes.fromhex("a1816eff")
    assert pack(["a", "b"]) == bytes.fromhex("9281618162")
    assert pack("x" * 16)[:2] == b"\xd0\x10"
    assert pack(200) == bytes.fromhex("c900c8")
    return {"fixture_check": "PASS", "runtime_exercised": False}


def run(binary):
    from neo4j import GraphDatabase, __version__ as driver_version
    from neo4j.exceptions import ServiceUnavailable, SessionExpired

    report = {"binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(), "driver": driver_version,
              "os": platform.platform(), "signal": "SIGTERM", "synthetic": True, "baseline_rows": ROWS,
              "uncertain_rows": UNCERTAIN_ROWS, "shutdown_grace_ms": GRACE_MS, "termination": [], "checks": []}
    with tempfile.TemporaryDirectory(prefix="brainwire-termination-") as folder:
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
                   BRAINWIRE_SERVER_MAX_MESSAGE="8MiB", BRAINWIRE_SERVER_SHUTDOWN_GRACE_MS=str(GRACE_MS))
        server = None
        raw = None
        owned_sockets = []
        log = (path / "server.log").open("w")

        def cli(*args):
            return subprocess.run([str(binary), *args], env=env, cwd=ROOT, capture_output=True,
                                  text=True, check=True, timeout=30).stdout

        def start():
            nonlocal server
            began = time.monotonic()
            server = subprocess.Popen([str(binary), "serve"], env=env, cwd=ROOT, stdout=log, stderr=log)
            for _ in range(150):
                assert server.poll() is None, "owned server exited during readiness"
                try:
                    cli("probe", "--address", endpoint, "--timeout-ms", "100")
                    return round(time.monotonic() - began, 6)
                except subprocess.CalledProcessError:
                    time.sleep(0.03)
            raise AssertionError("owned protected server readiness timeout")

        stop_began = None

        def signal_stop():
            nonlocal stop_began
            stop_began = time.monotonic()
            server.terminate()

        def wait_stop(case, drained):
            nonlocal server
            code = server.wait(timeout=GRACE_MS / 1000 + 5)
            elapsed = time.monotonic() - stop_began
            report["termination"].append({"case": case, "exit_code": code, "exit_seconds": round(elapsed, 6)})
            server = None
            # 0 means every session closed; 1 means the grace deadline abandoned some.
            if drained is None:
                drained = code == 0
            assert code == (0 if drained else 1), (case, code)
            if drained:
                assert elapsed < GRACE_MS / 1000, (case, elapsed)
            else:
                assert GRACE_MS / 1000 <= elapsed < GRACE_MS / 1000 + 3, (case, elapsed)

        def terminate(case, drained):
            signal_stop()
            wait_stop(case, drained)

        def admission_refused():
            began = time.monotonic()
            while time.monotonic() - began < 2:
                try:
                    socket.create_connection(address, timeout=0.2).close()
                except ConnectionRefusedError:
                    return round(time.monotonic() - began, 6)
                except ConnectionResetError:
                    pass  # queued in the backlog as the listener closed; retry
                time.sleep(0.02)
            raise AssertionError("listener still accepting during drain")

        def driver(name):
            return GraphDatabase.driver("bolt://" + endpoint, auth=(name, tokens[name]), connection_timeout=3,
                                       connection_acquisition_timeout=5, max_transaction_retry_time=0)

        def query(text, params=None, name="reader"):
            with driver(name) as connection:
                with connection.session(database=DATABASE) as session:
                    return session.run(text, params or {}).data()

        def baseline():
            assert query("MATCH (c:Customer) WHERE c.active=true RETURN count(*) AS n") == [{"n": ROWS}]
            assert query("MATCH (c:Customer {entity_id:'term-500'}) RETURN c.email AS email") == [{"email": "term-500@example.test"}]
            assert query("MATCH (c:Customer) WHERE c.email='term-500@example.test' RETURN c.entity_id AS id") == [{"id": "term-500"}]
            assert query("MATCH (c:Customer) WHERE c.email='pending-overwrite@example.test' RETURN c.entity_id AS id") == []
            assert query("MATCH (c:Customer {entity_id:'pending-term'}) RETURN c.entity_id") == []
            assert query("MATCH (c:Customer) WHERE c.email='pending-create@example.test' RETURN c.entity_id") == []

        def raw_reader(name):
            connection = socket.socket()
            owned_sockets.append(connection)
            connection.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 1024)
            connection.settimeout(5)
            connection.connect(address)
            connection.sendall(HANDSHAKE)
            assert exact(connection, 4) == bytes.fromhex("00000405")
            request(connection, 0x01, {"user_agent": "brainwire-owned-recovery"})
            assert message(connection)[:2] == bytes.fromhex("b170")
            request(connection, 0x6a, {"scheme": "basic", "principal": name, "credentials": tokens[name]})
            assert message(connection)[:2] == bytes.fromhex("b170")
            return connection

        try:
            cli("db", "create", DATABASE, "--ontology", str(ROOT / "examples/python/schema.json"))
            report["initial_ready_seconds"] = start()
            rows = [{"id": f"term-{i}", "email": f"term-{i}@example.test", "payload": "x" * 4096} for i in range(ROWS)]
            query("UNWIND $rows AS row MERGE (c:Customer {entity_id:row.id}) SET c.email=row.email, c.active=true, c.tags=[row.payload]",
                  {"rows": rows}, "writer")
            baseline()
            with driver("writer") as connection:
                with connection.session(database=DATABASE) as session:
                    transaction = session.begin_transaction()
                    transaction.run("CREATE (c:Customer {entity_id:'drain-commit', email:'drain-create@example.test', active:true})").consume()
                    signal_stop()
                    report["drain_admission_refused_seconds"] = admission_refused()
                    assert server.poll() is None, "an open transaction holds the drain"
                    transaction.run("MATCH (c:Customer {entity_id:'term-999'}) SET c.email='drain-overwrite@example.test'").consume()
                    transaction.commit()
            wait_stop("explicit transaction committed during drain", drained=True)
            report["drain_commit_recovery_ready_seconds"] = start()
            assert query("MATCH (c:Customer) WHERE c.email='drain-create@example.test' RETURN c.entity_id AS id") == [{"id": "drain-commit"}]
            assert query("MATCH (c:Customer) WHERE c.email='drain-overwrite@example.test' RETURN c.entity_id AS id") == [{"id": "term-999"}]
            assert query("MATCH (c:Customer) WHERE c.email='term-999@example.test' RETURN c.entity_id AS id") == []
            assert query("MATCH (c:Customer) WHERE c.active=true RETURN count(*) AS n") == [{"n": ROWS + 1}]
            query("MATCH (c:Customer {entity_id:'drain-commit'}) DETACH DELETE c", name="writer")
            query("MATCH (c:Customer {entity_id:'term-999'}) SET c.email='term-999@example.test'", name="writer")
            baseline()
            report["checks"].append("SIGTERM refuses new connections; an open transaction runs and commits within the grace; its CREATE and indexed overwrite survive restart; exit 0")
            with driver("writer") as connection:
                with connection.session(database=DATABASE) as session:
                    transaction = session.begin_transaction()
                    try:
                        transaction.run("CREATE (c:Customer {entity_id:'pending-term', email:'pending-create@example.test'})").consume()
                        transaction.run("MATCH (c:Customer {entity_id:'term-500'}) SET c.email='pending-overwrite@example.test'").consume()
                        assert transaction.run("MATCH (c:Customer {entity_id:'term-500'}) RETURN c.email AS email").single()["email"] == "pending-overwrite@example.test"
                        baseline()
                        terminate("explicit transaction abandoned at the grace deadline", drained=False)
                    finally:
                        try:
                            transaction.close()
                        except (ServiceUnavailable, SessionExpired, OSError):
                            pass
            report["pending_recovery_ready_seconds"] = start()
            baseline()
            report["checks"].append("acknowledged 1000 indexed writes survive; a transaction left open past the grace exits 1 and its CREATE and indexed overwrite disappear")
            raw = raw_reader("writer")
            uncertain = [{"id": f"uncertain-{i}", "email": f"uncertain-{i}@example.test"} for i in range(UNCERTAIN_ROWS)]
            request(raw, 0x10, "UNWIND $rows AS row CREATE (c:Customer {entity_id:row.id, email:row.email, active:false})",
                    {"rows": uncertain}, {"db": DATABASE})
            # The request is submitted, but no RUN/SUCCESS or commit acknowledgement is read.
            # The server may refuse it as a drain arrives, or run it and wait for PULL.
            terminate("RUN submitted without acknowledgement", drained=None)
            raw.close()
            raw = None
            report["uncertain_recovery_ready_seconds"] = start()
            baseline()
            count = query("MATCH (c:Customer) WHERE c.active=false RETURN count(*) AS n")[0]["n"]
            assert count in (0, UNCERTAIN_ROWS), count
            for row in uncertain:
                expected = [{"id": row["id"]}] if count else []
                assert query("MATCH (c:Customer) WHERE c.email=$email RETURN c.entity_id AS id", {"email": row["email"]}) == expected
                assert query("MATCH (c:Customer {entity_id:$id}) RETURN c.entity_id AS id", {"id": row["id"]}) == expected
            report["unacknowledged_outcome"] = "all mutations committed" if count else "no mutations committed"
            report["checks"].append("unacknowledged RUN produces zero or all 200 nodes with matching scalar indexes")
            raw = raw_reader("reader")
            request(raw, 0x10, "MATCH (c:Customer) WHERE c.active=true RETURN c.tags AS payload", {}, {"db": DATABASE})
            assert message(raw)[:2] == bytes.fromhex("b170")
            request(raw, 0x3f, {"n": -1})
            first = message(raw)
            assert first[:2] == bytes.fromhex("b171") and len(first) >= 4096
            report["slow_reader_first_record_bytes"] = len(first)
            report["slow_reader_requested_socket_receive_buffer"] = 1024
            report["slow_reader_socket_receive_buffer"] = raw.getsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF)
            # First RECORD observed; stop reading. No claim of an exact blocked-write phase.
            terminate("PULL first RECORD observed then reader stopped", drained=False)
            raw.close()
            raw = None
            report["slow_reader_recovery_ready_seconds"] = start()
            baseline()
            assert query("MATCH (c:Customer) RETURN count(*) AS n") == [{"n": ROWS + count}]
            report["checks"].append("a stalled reader holds the drain only until the grace deadline; restart preserves committed graph")
            with driver("reader") as idle:
                idle.verify_connectivity()
                terminate("idle pooled driver connection only", drained=True)
            report["checks"].append("idle authenticated sessions close at once and the drained process exits 0")
            report["audit"] = json.loads(cli("db", "check", DATABASE, "--json"))
            assert report["audit"]["status"] == "ok"
            report["result"] = "PASS"
            report["scope"] = "native single-node SIGTERM drain and recovery; no interruption of running engine calls, no exact commit-phase kill hook"
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
    options = parser.parse_args()
    print(json.dumps(self_check() if options.self_check else run(options.binary.resolve()), indent=2))
