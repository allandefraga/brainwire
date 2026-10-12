"""Real listener admission/negotiation and request framing acceptance.

Creates its own disposable unprotected loopback server. No driver dependency.
Uses the production 10-second deadline, not the shorter unit-test injection.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[3]
HANDSHAKE = bytes.fromhex("6060b01700000405000000000000000000000000")


def exact(stream, count):
    result = b""
    while len(result) < count:
        part = stream.recv(count - len(result))
        if not part:
            raise EOFError("connection closed")
        result += part
    return result


def message(stream):
    payload = b""
    while size := int.from_bytes(exact(stream, 2), "big"):
        payload += exact(stream, size)
    return payload


def frame(payload):
    return len(payload).to_bytes(2, "big") + payload + b"\0\0"


def run(binary):
    with tempfile.TemporaryDirectory(prefix="brainwire-transport-") as folder:
        with socket.socket() as reserved:
            reserved.bind(("127.0.0.1", 0))
            address = reserved.getsockname()
        env = {k: v for k, v in os.environ.items() if not k.startswith("BRAINWIRE_")}
        env.update(BRAINWIRE_SERVER_DATA_DIR=folder + "/data",
            BRAINWIRE_SERVER_LISTEN="%s:%s" % address, BRAINWIRE_SERVER_MAX_MESSAGE="32",
            BRAINWIRE_MEMORY_CACHE="64MiB", BRAINWIRE_MEMORY_MEMTABLES="16MiB",
            BRAINWIRE_MEMORY_QUERY="16MiB")
        log = tempfile.TemporaryFile(mode="w+")
        server = subprocess.Popen([str(binary), "serve"], env=env, cwd=ROOT, stdout=log, stderr=log)
        sockets = []
        try:
            # Readiness is one complete negotiated socket; it also frees its slot.
            for _ in range(100):
                if server.poll() is not None:
                    log.seek(0)
                    raise AssertionError("server exited before readiness: " + log.read())
                try:
                    with socket.create_connection(address, timeout=.2) as client:
                        client.sendall(HANDSHAKE)
                        assert exact(client, 4) == bytes.fromhex("00000405")
                    break
                except OSError:
                    time.sleep(.03)
            else:
                raise AssertionError("listener not ready")
            time.sleep(.05)
            for _ in range(64):
                sockets.append(socket.create_connection(address, timeout=2))
            time.sleep(.1)
            with socket.create_connection(address, timeout=2) as excess:
                excess.sendall(HANDSHAKE)
                try:
                    selected = exact(excess, 4)
                except (EOFError, ConnectionResetError):
                    selected = None
                assert selected is None, "65th connection negotiated while all permits held"
            began = time.monotonic()
            for client in sockets:
                client.settimeout(12)
                assert client.recv(1) == b"", "silent handshake socket was not closed"
            elapsed = time.monotonic() - began
            assert elapsed <= 12, elapsed
            for client in sockets:
                client.close()
            sockets.clear()
            # Negotiation alone must not retain permits: HELLO without LOGON expires too.
            for _ in range(64):
                client = socket.create_connection(address, timeout=2)
                sockets.append(client)
                client.sendall(HANDSHAKE)
                assert exact(client, 4) == bytes.fromhex("00000405")
                client.sendall(frame(bytes.fromhex("b101a0")))
                assert message(client)[:2] == bytes.fromhex("b170")
            with socket.create_connection(address, timeout=2) as excess:
                excess.sendall(HANDSHAKE)
                try:
                    selected = exact(excess, 4)
                except (EOFError, ConnectionResetError):
                    selected = None
                assert selected is None, "65th connection admitted while all authentication permits held"
            began_auth = time.monotonic()
            for client in sockets:
                client.settimeout(12)
                assert client.recv(1) == b"", "HELLO without LOGON retained a slot"
                client.close()
            auth_elapsed = time.monotonic() - began_auth
            assert auth_elapsed <= 12, auth_elapsed
            sockets.clear()
            # Independent per-message cap despite coalesced TCP delivery.
            with socket.create_connection(address, timeout=2) as client:
                client.sendall(HANDSHAKE)
                assert exact(client, 4) == bytes.fromhex("00000405")
                hello = bytes.fromhex("b101a0")
                logon = bytes.fromhex("b16aa0")
                # NOOP framing makes this write exceed 32 bytes; messages remain tiny.
                client.sendall(b"\0\0" * 20 + frame(hello) + frame(logon) + frame(bytes.fromhex("b002")))
                assert message(client)[:2] == bytes.fromhex("b170")
                assert message(client)[:2] == bytes.fromhex("b170")
                assert client.recv(1) == b""
            for payload in [b"\0\x21", frame(bytes.fromhex("b101a000"))]:
                with socket.create_connection(address, timeout=2) as client:
                    client.sendall(HANDSHAKE)
                    assert exact(client, 4) == bytes.fromhex("00000405")
                    client.sendall(payload)
                    assert message(client)[:2] == bytes.fromhex("b17f")
                    assert client.recv(1) == b""
            return dict(result="PASS", binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
                production_handshake_timeout_seconds=10, silent_sockets=64,
                observed_close_wait_seconds=elapsed, request_payload_cap_bytes=32,
                production_authentication_timeout_seconds=10, unauthenticated_sockets=64,
                observed_auth_close_wait_seconds=auth_elapsed,
                checks=["65th connection rejected", "64 silent sockets expire", "permits reusable",
                    "64 HELLO-without-LOGON sockets expire; permits reusable",
                    "coalesced requests and NOOPs accepted", "announced oversize rejected before body",
                    "trailing PackStream byte rejected"], scope="native isolated loopback; no authenticated idle/drain test")
        finally:
            for client in sockets:
                client.close()
            if server.poll() is None:
                server.kill()
            server.wait(timeout=10)
            log.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/brainwire")
    print(json.dumps(run(parser.parse_args().binary.resolve()), indent=2))
