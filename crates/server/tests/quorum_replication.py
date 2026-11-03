"""Synthetic three-process quorum/partition/restart acceptance on owned loopback ports.

No host orchestration, shared store, external application data or remote deployment is touched.
Consensus snapshots remain unsupported; the final check is offline audit/digest equality.
"""
import argparse
from contextlib import ExitStack
import hashlib
import json
import os
from pathlib import Path
import secrets
import socket
import socketserver
import struct
import sys
import subprocess
import tempfile
import threading
import time
import uuid

MAX_FRAME = 8 << 20
MAX_LOG_BYTES = 1 << 30
SCHEMA = {"version": "1", "nodes": [
    {"label": "Page", "properties": ["title"], "indexed_fields": ["title"]},
    {"label": "ImportCheckpoint", "properties": ["next_offset", "sha256"]}],
    "edges": [{"type": "LINK", "from": {"label": "Page"}, "to": {"label": "Page"}, "properties": ["weight"]}]}


def read_exact(stream, count):
    data = bytearray()
    while len(data) < count:
        part = stream.recv(count - len(data))
        if not part:
            raise EOFError("owned proxy peer closed")
        data.extend(part)
    return bytes(data)


def read_frame(stream):
    count = struct.unpack("!I", read_exact(stream, 4))[0]
    if not 1 <= count <= MAX_FRAME:
        raise ValueError("owned proxy frame exceeds its bound")
    return read_exact(stream, count)


def write_frame(stream, body):
    if not 1 <= len(body) <= MAX_FRAME:
        raise ValueError("owned proxy reply exceeds its bound")
    stream.sendall(struct.pack("!I", len(body)) + body)


class Faults:
    def __init__(self):
        self.lock = threading.Lock()
        self.isolated = set()
        self.status_only = set()

    def set(self, members):
        with self.lock:
            self.isolated = set(members)
            self.status_only = set()

    def set_status_only(self, members):
        with self.lock:
            self.isolated = set()
            self.status_only = set(members)

    def blocked(self, sender, recipient, method=None):
        with self.lock:
            if sender in self.isolated or recipient in self.isolated:
                return True
            return (sender in self.status_only or recipient in self.status_only) and method != "Status"


class Proxy:
    def __init__(self, member, backend, faults):
        permits = threading.BoundedSemaphore(64)

        class Handler(socketserver.BaseRequestHandler):
            def handle(handler):
                if not permits.acquire(blocking=False):
                    return
                upstream = None
                try:
                    # Peers reuse connections, so forward frames until either side
                    # closes; a blocked frame drops both connections, as a partition.
                    handler.request.settimeout(35)
                    while True:
                        body = read_frame(handler.request)
                        envelope = json.loads(body)
                        payload = envelope.get("payload")
                        if isinstance(payload, str):
                            method = payload
                        elif isinstance(payload, dict):
                            method = next(iter(payload), None)
                        else:
                            method = None
                        if envelope.get("recipient") != member or faults.blocked(envelope.get("sender"), member, method):
                            return
                        if upstream is None:
                            upstream = socket.create_connection(backend, timeout=2)
                            upstream.settimeout(6)
                        write_frame(upstream, body)
                        reply = read_frame(upstream)
                        if faults.blocked(envelope.get("sender"), member, method):
                            return
                        write_frame(handler.request, reply)
                except (OSError, EOFError, ValueError, TypeError, AttributeError):
                    pass  # Never print envelopes: they contain the private peer token.
                finally:
                    if upstream is not None:
                        upstream.close()
                    permits.release()

        class Server(socketserver.ThreadingTCPServer):
            allow_reuse_address = False
            daemon_threads = False
            block_on_close = True

        self.server = Server(("127.0.0.1", 0), Handler)
        self.address = "%s:%s" % self.server.server_address
        self.closed = False
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def close(self):
        if self.closed:
            return
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)
        if self.thread.is_alive():
            raise RuntimeError("owned fault proxy did not stop")
        self.closed = True


def port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def check_capacity(status, max_log_bytes=MAX_LOG_BYTES):
    assert status["max_log_bytes"] == max_log_bytes
    assert type(status["log_bytes"]) is int and 0 <= status["log_bytes"] <= max_log_bytes
    assert type(status["log_remaining_bytes"]) is int
    assert status["log_remaining_bytes"] == max_log_bytes - status["log_bytes"]


def check_inspection(sample, unavailable=None, max_log_bytes=MAX_LOG_BYTES):
    assert sample["kind"] == "diagnostic_sample"
    assert sample["scope"] == "independent status samples; not a quorum or write/read readiness proof"
    assert sample["configured_max_log_bytes"] == max_log_bytes
    complete = unavailable is None
    assert sample["complete"] is complete and sample["all_initialized"] is complete
    for key in ("retained_log_cap_agreement", "configured_log_cap_agreement"):
        assert sample[key] is (True if complete else None)
    members = sample["members"]
    assert len(members) == 3 and {item["node_id"] for item in members} == {1, 2, 3}
    indexes = []
    for item in members:
        assert type(item["response_time_ms"]) in (int, float) and item["response_time_ms"] >= 0
        if item["node_id"] == unavailable:
            assert item["diagnostic"] == "unavailable" and item["status"] is None
        else:
            assert item["diagnostic"] == "sampled" and item["initialized"] is True
            assert item["status"]["node_id"] == item["node_id"]
            check_capacity(item["status"], max_log_bytes)
            indexes.append((item["status"]["committed_index"], item["status"]["last_applied"]))
    progress = sample["replication_progress"]
    if unavailable is not None or any(index is None for pair in indexes for index in pair):
        assert progress is None
    else:
        assert progress["basis"].startswith("concurrent independent samples")
        for name, values in (("committed_index", [pair[0] for pair in indexes]),
                             ("applied_index", [pair[1] for pair in indexes])):
            assert progress[name] == {"min": min(values), "max": max(values),
                                      "spread": max(values) - min(values)}
        for item in members:
            assert item["committed_index_lag"] == progress["committed_index"]["max"] - item["status"]["committed_index"]
            assert item["applied_index_lag"] == progress["applied_index"]["max"] - item["status"]["last_applied"]


def self_check():
    valid = {"max_log_bytes": MAX_LOG_BYTES, "log_bytes": 123, "log_remaining_bytes": MAX_LOG_BYTES - 123}
    check_capacity(valid)
    check_capacity({"max_log_bytes": 1 << 20, "log_bytes": 123,
                    "log_remaining_bytes": (1 << 20) - 123}, 1 << 20)
    for field, value in (("max_log_bytes", 0), ("log_bytes", -1), ("log_remaining_bytes", MAX_LOG_BYTES), ("log_bytes", True)):
        invalid = {**valid, field: value}
        try:
            check_capacity(invalid)
        except AssertionError:
            pass
        else:
            raise AssertionError("invalid capacity sample accepted")
    inspection = {"kind": "diagnostic_sample", "scope": "independent status samples; not a quorum or write/read readiness proof",
        "configured_max_log_bytes": MAX_LOG_BYTES, "complete": True, "all_initialized": True,
        "retained_log_cap_agreement": True, "configured_log_cap_agreement": True,
        "members": [{"node_id": member, "response_time_ms": 1.0,
                     "diagnostic": "sampled", "initialized": True,
                     "status": {**valid, "node_id": member,
                                "committed_index": 10 + member,
                                "last_applied": 9 + member},
                     "committed_index_lag": 3 - member,
                     "applied_index_lag": 3 - member}
                    for member in (1, 2, 3)],
        "replication_progress": {
            "basis": "concurrent independent samples; not an atomic snapshot or readiness proof",
            "committed_index": {"min": 11, "max": 13, "spread": 2},
            "applied_index": {"min": 10, "max": 12, "spread": 2}}}
    check_inspection(inspection)
    no_indexes = json.loads(json.dumps(inspection))
    no_indexes["replication_progress"] = None
    for member in no_indexes["members"]:
        member["status"]["committed_index"] = None
        member["status"]["last_applied"] = None
        member.pop("committed_index_lag")
        member.pop("applied_index_lag")
    check_inspection(no_indexes)
    incomplete = json.loads(json.dumps(inspection))
    incomplete.update(complete=False, all_initialized=False, retained_log_cap_agreement=None, configured_log_cap_agreement=None)
    incomplete["replication_progress"] = None
    incomplete["members"][1] = {"node_id": 2, "response_time_ms": 5.0,
                                 "diagnostic": "unavailable", "status": None}
    check_inspection(incomplete, unavailable=2)
    for field, value in (("complete", True), ("retained_log_cap_agreement", True)):
        try:
            check_inspection({**incomplete, field: value}, unavailable=2)
        except AssertionError:
            pass
        else:
            raise AssertionError("incomplete inspection claim accepted")
    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    listener.listen()
    listener.settimeout(2)
    calls = []

    def backend():
        with listener.accept()[0] as stream:
            stream.settimeout(2)
            calls.append(json.loads(read_frame(stream))["sender"])
            write_frame(stream, b'{"Status":{"leader":2}}')

    thread = threading.Thread(target=backend)
    thread.start()
    faults = Faults()
    faults.set_status_only({1})
    assert not faults.blocked(2, 1, "Status")
    assert faults.blocked(2, 1, "AppendEntries")
    faults.set(set())
    proxy = Proxy(1, listener.getsockname(), faults)
    try:
        envelope = json.dumps({"sender": 2, "recipient": 1, "token": "private-test-token", "payload": "Status"}).encode()
        address = proxy.server.server_address
        with socket.create_connection(address, timeout=2) as client:
            write_frame(client, envelope)
            assert json.loads(read_frame(client)) == {"Status": {"leader": 2}}
        for isolated in ({1}, {2}):
            faults.set(isolated)
            with socket.create_connection(address, timeout=2) as client:
                write_frame(client, envelope)
                assert client.recv(1) == b""
        faults.set(set())
        with socket.create_connection(address, timeout=2) as client:
            client.sendall(struct.pack("!I", MAX_FRAME + 1))
            assert client.recv(1) == b""
        thread.join(timeout=2)
        assert not thread.is_alive() and calls == [2]
    finally:
        proxy.close()
        listener.close()
    return {"self_check": "PASS", "runtime_exercised": False,
        "checks": ["bounded peer proxy forwards one frame", "isolation blocks sender and recipient", "status-only fault passes diagnostics but blocks consensus traffic", "oversized header refused before body allocation", "capacity sample rejects mismatched limit/remaining and invalid counters", "replication lag is derived only when all initialized voter indexes are present", "incomplete inspect cannot claim completeness or capacity agreement"]}


def run(binary, rows, log_cap_recovery=False):
    from neo4j import GraphDatabase, READ_ACCESS, __version__ as driver_version
    from neo4j.exceptions import Neo4jError, ServiceUnavailable, SessionExpired
    sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
    from examples.python.client import GraphClient, request_key as build_request_key
    from brainwire import CommitOutcomeUnknownError
    if not 4 <= rows <= 500 or rows % 2:
        raise ValueError("rows must be even and between 4 and 500")
    max_log_bytes = (1 << 20) if log_cap_recovery else MAX_LOG_BYTES
    binary = binary.resolve(strict=True)
    report = {"binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(), "driver": driver_version,
              "rows": rows, "synthetic": True, "checks": [], "scope": "three independent native processes on loopback; not a machine-loss, TLS or snapshot qualification"}
    faults = Faults()
    processes = {}
    logs = {}
    proxies = []
    tokens = {name: secrets.token_hex(32) for name in ("writer", "alice", "plain-writer")}
    environment = {key: value for key, value in os.environ.items() if not key.startswith("BRAINWIRE_")}
    cluster_id = "acceptance-" + uuid.uuid4().hex

    with tempfile.TemporaryDirectory(prefix="brainwire-quorum-") as folder, ExitStack() as resources:
        root = Path(folder)
        peer_token = root / "peer-token"
        peer_token.write_text(secrets.token_hex(32) + "\n")
        peer_token.chmod(0o600)
        policy = root / "policy.json"
        policy.write_text(json.dumps({"principals": [
            {"id": "writer", "token_sha256": hashlib.sha256(tokens["writer"].encode()).hexdigest(),
             "graphs": {"pages": "write", "aux": "write"}, "instance_admin": True},
            {"id": "plain-writer", "token_sha256": hashlib.sha256(tokens["plain-writer"].encode()).hexdigest(),
             "graphs": {"pages": "write", "aux": "write"}, "instance_admin": False},
            {"id": "alice", "token_sha256": hashlib.sha256(tokens["alice"].encode()).hexdigest(),
             "graphs": {"pages": "read", "aux": "read"}, "instance_admin": False,
             "node_visibility": {"principals": ["alice"]}}]}))
        policy.chmod(0o600)
        schema_path = root / "schema.json"
        schema_path.write_text(json.dumps(SCHEMA))
        graphs = root / "graphs.json"
        graphs.write_text(json.dumps({"pages": SCHEMA}))
        backend_ports = {member: port() for member in (1, 2, 3)}
        bolt_ports = {member: port() for member in (1, 2, 3)}
        for member in (1, 2, 3):
            proxy = Proxy(member, ("127.0.0.1", backend_ports[member]), faults)
            resources.callback(proxy.close)
            proxies.append(proxy)
        configs = {}
        servers = {}
        for member in (1, 2, 3):
            directory = root / str(member)
            directory.mkdir()
            servers[member] = directory / "server.toml"
            servers[member].write_text(
                f'[server]\nlisten="127.0.0.1:{bolt_ports[member]}"\ndata_dir={json.dumps(str(directory / "data"))}\n'
                f'default_database="pages"\nauth_file={json.dumps(str(policy))}\n'
                'max_message="8MiB"\nmax_decoded_message="16MiB"\nquery_timeout_ms=10000\nio_timeout_ms=300000\n'
                '[memory]\ncache="64MiB"\nmemtables="16MiB"\nquery="64MiB"\n')
            configs[member] = directory / "cluster.toml"
            configs[member].write_text(
                f'node_id={member}\ncluster_id="{cluster_id}"\nlisten="127.0.0.1:{backend_ports[member]}"\n'
                f'token_file={json.dumps(str(peer_token))}\nmax_log_bytes={max_log_bytes}\n' +
                "".join(f'[[peers]]\nid={index}\naddress="{proxy.address}"\n' for index, proxy in enumerate(proxies, 1)))

        def redact(text):
            for token in [*tokens.values(), peer_token.read_text().strip()]:
                text = text.replace(token, "[redacted]")
            return text

        def command(member, *arguments, check=True, timeout=20):
            result = subprocess.run([str(binary), "cluster", "--cluster-config", str(configs[member]), *arguments],
                env=environment, capture_output=True, text=True, timeout=timeout)
            if check and result.returncode:
                raise RuntimeError(redact(result.stderr))
            return result

        def status(member):
            return json.loads(command(member, "status", timeout=8).stdout)["Status"]

        def probe(member, kind, success):
            result = command(member, "probe", "--kind", kind, check=False)
            sample = json.loads(result.stdout)
            assert sample["kind"] == "raft_probe" and sample["node_id"] == member
            assert sample["probe_kind"] == kind and sample["success"] is success
            assert sample["scope"] == "Raft only; does not verify graphs, Bolt authentication, or future writes"
            assert (result.returncode == 0) is success
            return sample

        def start(member):
            log = (root / str(member) / "server.log").open("ab")
            logs.setdefault(member, []).append(log)
            processes[member] = subprocess.Popen([str(binary), "serve", "--config", str(servers[member]),
                "--cluster-config", str(configs[member])], env=environment, stdout=log, stderr=log)
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                if processes[member].poll() is not None:
                    raise RuntimeError(redact((root / str(member) / "server.log").read_text()))
                try:
                    status(member)
                    return
                except (RuntimeError, OSError, ValueError, subprocess.TimeoutExpired):
                    time.sleep(0.1)
            raise RuntimeError("owned replica did not start")

        def stop(member, kill=True):
            process = processes.pop(member, None)
            if process is not None and process.poll() is None:
                process.kill() if kill else process.terminate()
                try:
                    process.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)

        def leader(members=(1, 2, 3), excluded=(), timeout=30):
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                candidates = []
                for member in members:
                    if member in excluded or member not in processes:
                        continue
                    try:
                        info = status(member)
                        candidates.append(info.get("leader"))
                        if info.get("leader") == member and info.get("state") == "Leader":
                            return member
                    except (RuntimeError, OSError, ValueError, subprocess.TimeoutExpired):
                        pass
                time.sleep(0.2)
            raise RuntimeError("majority did not elect an available leader")

        def driver(member, principal="writer"):
            return GraphDatabase.driver(f"bolt://127.0.0.1:{bolt_ports[member]}", auth=(principal, tokens[principal]),
                connection_timeout=2, connection_acquisition_timeout=4, max_transaction_retry_time=0)

        def query(member, text, graph="pages", principal="writer", **parameters):
            with driver(member, principal) as client:
                with client.session(database=graph, default_access_mode=READ_ACCESS if principal == "alice" else "WRITE") as session:
                    return session.run(text, parameters).data()

        def refused(member, text, principal="writer"):
            try:
                query(member, text, principal=principal)
            except (Neo4jError, ServiceUnavailable, SessionExpired, OSError):
                return
            raise AssertionError("isolated/follower operation acknowledged unexpectedly")

        def caught_up(timeout=30):
            snapshots = []
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                try:
                    snapshots = [status(member) for member in (1, 2, 3)]
                    committed = [item.get("committed_index") or 0 for item in snapshots]
                    applied = [item.get("last_applied") or 0 for item in snapshots]
                    if committed[0] and len(set(committed)) == 1 and applied == committed:
                        return snapshots
                except (RuntimeError, OSError, ValueError, subprocess.TimeoutExpired):
                    pass
                time.sleep(0.2)
            raise RuntimeError(f"all owned replicas did not catch up: {snapshots}")

        pending_resources = []
        try:
            for member in (1, 2, 3):
                command(member, "provision", "--config", str(servers[member]))
                start(member)
            report["checks"].append("each fresh owned voter explicitly provisioned once; restarts never provision or replace voter persistence")
            initialized = json.loads(command(1, "init", "--schemas", str(graphs), "--request-id", "bootstrap").stdout)
            report["bootstrap"] = initialized
            current = leader()
            created = json.loads(command(current, "create", "--graph", "aux", "--ontology", str(schema_path), "--request-id", "create-aux").stdout)
            report["create_aux"] = created
            batch = [{"id": f"page-{i}", "title": f"title-{i}"} for i in range(rows)]
            query(current, "UNWIND $rows AS row CREATE (p:Page {entity_id:row.id,title:row.title})", rows=batch)
            for index in range(rows):
                query(current, "CALL db.setNodeAcl({label:'Page',entityId:$id,allow:$allow}) YIELD updated RETURN updated",
                    id=f"page-{index}", allow=["alice" if index % 2 == 0 else "bob"])
            query(current, "MATCH (a:Page {entity_id:'page-0'}) MATCH (b:Page {entity_id:'page-2'}) MERGE (a)-[:LINK]->(b)")
            query(current, "CREATE (c:ImportCheckpoint {entity_id:'source',next_offset:$offset,sha256:'fixture'})", offset=rows)
            query(current, "CREATE (p:Page {entity_id:'aux-1',title:'aux'})", graph="aux")
            query(current, "CALL db.setNodeAcl({label:'Page',entityId:'aux-1',allow:['alice']}) YIELD updated RETURN updated", graph="aux")
            assert query(current, "MATCH (p:Page) RETURN count(*) AS n", principal="alice") == [{"n": rows // 2}]
            assert query(current, "MATCH (p:Page) RETURN count(*) AS n", graph="aux", principal="alice") == [{"n": 1}]
            follower = next(member for member in (1, 2, 3) if member != current)
            refused(follower, "RETURN 1", principal="alice")
            report["local_probes"] = {"leader_readiness": probe(current, "readiness", True),
                "follower_liveness": probe(follower, "liveness", True),
                "follower_readiness": probe(follower, "readiness", False)}
            assert report["local_probes"]["follower_readiness"]["reason"] == "not_local_leader"
            report["checks"].append("local leader readiness passes quorum read barrier; follower liveness passes while follower readiness fails; probes explicitly cover Raft only")
            report["checks"].append("committed bootstrap/create register two graphs; indexed nodes and trusted ACL restrict Alice; follower BEGIN refused")

            faults.set_status_only({follower})
            try:
                time.sleep(0.15)
                for index in range(3):
                    query(current, "MATCH (p:Page {entity_id:'page-0'}) SET p.title=$title",
                          title=f"replication-lag-{index}")
                lag_inspection = json.loads(command(current, "inspect").stdout)
                check_inspection(lag_inspection, max_log_bytes=max_log_bytes)
                lagging_sample = next(item for item in lag_inspection["members"] if item["node_id"] == follower)
                assert lag_inspection["complete"] and lag_inspection["all_initialized"]
                assert max(lagging_sample["committed_index_lag"], lagging_sample["applied_index_lag"]) > 0
                report["inspection_replication_lag"] = lag_inspection
            finally:
                faults.set(set())
            lag_recovered = caught_up()
            assert len({item["committed_index"] for item in lag_recovered}) == 1
            assert len({item["last_applied"] for item in lag_recovered}) == 1
            report["replication_lag_recovered_status"] = lag_recovered
            current = leader()
            query(current, "MATCH (p:Page {entity_id:'page-0'}) SET p.title='title-0'")
            caught_up()
            report["checks"].append("live inspect identifies a reachable follower with blocked replication and reports its sampled committed/applied lag; healing catches it up")

            pending_driver = driver(current)
            pending_resources = [pending_driver]
            pending_session = pending_driver.session(database="pages")
            pending_resources.insert(0, pending_session)
            pending = pending_session.begin_transaction(metadata={"brainwire_request_id": "old-leader-staged"})
            pending_resources.insert(0, pending)
            pending.run("CREATE (p:Page {entity_id:'old-uncommitted',title:'pending'})").consume()
            pending.run("MATCH (p:Page {entity_id:'page-0'}) SET p.title='old-uncommitted-index'").consume()
            old = current
            faults.set({old})
            majority = tuple(member for member in (1, 2, 3) if member != old)
            current = leader(majority, excluded=(old,))
            assert current != old
            query(current, "CREATE (p:Page {entity_id:'majority-ack',title:'majority'})")
            query(current, "CALL db.setNodeAcl({label:'Page',entityId:'majority-ack',allow:['alice']}) YIELD updated RETURN updated")
            query(current, "MATCH (a:Page {entity_id:'majority-ack'}) MATCH (b:Page {entity_id:'page-0'}) MERGE (a)-[:LINK]->(b)")
            try:
                pending.commit()
            except (Neo4jError, ServiceUnavailable, SessionExpired, OSError):
                pass
            else:
                raise AssertionError("partitioned old leader acknowledged staged COMMIT")
            for resource in pending_resources:
                try:
                    resource.close()
                except (Neo4jError, ServiceUnavailable, SessionExpired, OSError):
                    pass
            pending_resources = []
            refused(old, "MATCH (p:Page) RETURN count(*) AS n", principal="alice")
            report["checks"].append("symmetric old-leader partition: majority elects/writes; staged old COMMIT and old read barrier refuse acknowledgement")
            faults.set(set())
            report["post_heal_status"] = caught_up()
            for sample in report["post_heal_status"]:
                check_capacity(sample, max_log_bytes)
            report["inspection_healthy"] = json.loads(command(current, "inspect").stdout)
            check_inspection(report["inspection_healthy"], max_log_bytes=max_log_bytes)
            assert report["inspection_healthy"]["cluster_id"] == cluster_id
            report["checks"].append("healed three-voter status reports configured retained-log cap, exact remaining bytes and sampled committed/applied index lag; independent inspect samples do not imply quorum readiness")
            inspection_victim = next(member for member in (1, 2, 3) if member != current)
            stop(inspection_victim)
            try:
                inspection = command(current, "inspect", check=False)
                assert inspection.returncode != 0, "incomplete inspection exited successfully"
                report["inspection_incomplete"] = json.loads(inspection.stdout)
                report["inspection_incomplete_exit_code"] = inspection.returncode
                check_inspection(report["inspection_incomplete"], unavailable=inspection_victim, max_log_bytes=max_log_bytes)
                assert report["inspection_incomplete"]["cluster_id"] == cluster_id
            finally:
                start(inspection_victim)
            caught_up()
            report["checks"].append("one stopped voter is unavailable in incomplete inspect samples, agreements are unknown and CLI exits nonzero; original voter restarts")
            assert query(current, "MATCH (p:Page {entity_id:'old-uncommitted'}) RETURN p.entity_id AS id") == []
            assert query(current, "MATCH (p:Page {title:'title-0'}) RETURN p.entity_id AS id") == [{"id": "page-0"}]
            stop(current)
            killed = current
            current = leader(tuple(member for member in (1, 2, 3) if member != killed))
            assert query(current, "MATCH (p:Page) RETURN count(*) AS n") == [{"n": rows + 1}]
            assert query(current, "MATCH (p:Page) RETURN count(*) AS n", principal="alice") == [{"n": rows // 2 + 1}]
            assert query(current, "MATCH (c:ImportCheckpoint {entity_id:'source'}) RETURN c.next_offset AS offset") == [{"offset": rows}]
            assert query(current, "MATCH (p:Page) RETURN count(*) AS n", graph="aux") == [{"n": 1}]
            query(current, "CREATE (p:Page {entity_id:'after-promotion',title:'promoted'})")
            start(killed)
            caught_up()
            report["checks"].append("new leader killed after acknowledged writes; promotion preserves nodes/index/ACL/cursor and allocates another unique node")
            stopped = [member for member in (1, 2, 3) if member != current]
            for member in stopped:
                stop(member)
            refused(current, "CREATE (p:Page {entity_id:'no-quorum'})")
            refused(current, "MATCH (p:Page) RETURN count(*) AS n", principal="alice")
            report["local_probes"]["no_quorum_readiness"] = probe(current, "readiness", False)
            for member in stopped:
                start(member)
            current = leader()
            caught_up()
            assert query(current, "MATCH (p:Page {entity_id:'no-quorum'}) RETURN p.entity_id AS id") == []
            report["checks"].append("two voters stopped: remaining writer/read barriers refuse; recovered majority has no acknowledged no-quorum mutation")
            seed = "fixed-lost-ack"
            operation_calls = []

            def write_once(tx):
                operation_calls.append(1)
                tx.run("CREATE (p:Page {entity_id:'idempotent',title:'idempotent'})").consume()
                return "acknowledged"

            with driver(current) as client:
                GraphClient(client, "pages").write_transaction(write_once, request_id=seed)
                # Intentionally ignore the successful ACK; this is not packet-loss injection.
            before_retry = query(current, "MATCH (p:Page) RETURN count(*) AS n")
            with driver(current) as client:
                try:
                    GraphClient(client, "pages").write_transaction(write_once, request_id=seed)
                except Neo4jError as error:
                    assert error.code == "Neo.ClientError.Transaction.RequestAlreadyCommitted", error.code
                else:
                    raise AssertionError("same explicit request ID was accepted again")
            assert operation_calls == [1], "public explicit-write API retried the operation callback"
            assert query(current, "MATCH (p:Page) RETURN count(*) AS n") == before_retry
            request_key = build_request_key(cluster_id, "pages", "writer", seed)
            outcome = json.loads(command(current, "request", "--id", request_key).stdout)
            assert "Applied" in outcome["Lookup"]
            report["request_outcome"] = outcome
            report["checks"].append("public GraphClient explicit COMMIT acknowledgement intentionally ignored; stable seed retry propagates RequestAlreadyCommitted without rerunning callback; request_key resolves durable outcome without duplicate mutation")
            caught_up()
            acknowledged_leader = current
            stop(acknowledged_leader)
            current = leader(tuple(member for member in (1, 2, 3) if member != acknowledged_leader))
            assert current != acknowledged_leader
            promoted_outcome = json.loads(command(current, "request", "--id", request_key).stdout)
            assert promoted_outcome == outcome, "leader failover changed the committed request outcome"
            callbacks_before_failover_retry = len(operation_calls)
            with driver(current) as client:
                try:
                    GraphClient(client, "pages").write_transaction(write_once, request_id=seed)
                except Neo4jError as error:
                    assert error.code == "Neo.ClientError.Transaction.RequestAlreadyCommitted", error.code
                else:
                    raise AssertionError("promoted leader accepted an already committed request")
            assert len(operation_calls) == callbacks_before_failover_retry, "failover retry reran the callback"
            assert query(current, "MATCH (p:Page) RETURN count(*) AS n") == before_retry
            assert query(current, "MATCH (p:Page {entity_id:'idempotent'}) RETURN count(*) AS n") == [{"n": 1}]
            report["request_replay_after_failover"] = {
                "acknowledged_leader": acknowledged_leader, "promoted_leader": current,
                "outcome": promoted_outcome,
                "code": "Neo.ClientError.Transaction.RequestAlreadyCommitted",
                "callback_calls_before": callbacks_before_failover_retry,
                "callback_calls_after": len(operation_calls), "matching_nodes": 1}
            start(acknowledged_leader)
            report["request_failover_recovered_status"] = caught_up()
            report["checks"].append("leader killed after identified COMMIT; promoted majority preserves the exact quorum request outcome and rejects stable-seed replay before callback execution; original voter catches up without duplicate data")
            victim = next(member for member in (1, 2, 3) if member != current)
            data = root / str(victim) / "data"

            def missing_storage_refused(name):
                stop(victim)
                original = data / name
                backup = data / (".acceptance-backup-" + name.lstrip("."))
                assert original.parent == data and backup.parent == data
                assert original.exists() and not original.is_symlink() and not backup.exists()
                original.rename(backup)
                try:
                    attempt = subprocess.run([str(binary), "serve", "--config", str(servers[victim]),
                        "--cluster-config", str(configs[victim])], env=environment,
                        capture_output=True, text=True, timeout=15)
                    assert attempt.returncode != 0, "missing committed storage restarted successfully"
                    assert not original.exists(), "normal restart recreated missing acknowledged storage"
                    report.setdefault("missing_storage_refusals", {})[name] = {
                        "member": victim, "exit_code": attempt.returncode,
                        "diagnostic": redact(attempt.stderr)[-4096:]}
                    # The intact majority remains writable; no new voter is created.
                    query(current, "MATCH (p:Page {entity_id:'page-0'}) SET p.title=$title", title="healthy-majority-" + name)
                finally:
                    assert not original.exists(), "missing acknowledged store was unexpectedly recreated"
                    backup.rename(original)
                start(victim)
                return caught_up(timeout=60)

            report["restored_graph_catchup"] = missing_storage_refused("pages")
            report["checks"].append("missing committed graph refuses startup without recreation; intact majority writes; restored graph with retained votes/log catches up")
            report["restored_raft_catchup"] = missing_storage_refused(".brainwire-raft")
            assert query(current, "MATCH (a:Page)-[:LINK]->(b:Page) RETURN count(*) AS n") == [{"n": 2}]
            report["checks"].append("missing voter Raft directory refuses startup without recreation; restored original persistent votes/log catches up")
            report["restored_registry_catchup"] = missing_storage_refused(".brainwire-graphs.json")
            report["checks"].append("missing initialized committed registry refuses startup; restoring original registry preserves voter history and catches up")
            report["restored_identity_catchup"] = missing_storage_refused(".brainwire-cluster.json")
            report["checks"].append("missing persisted voter identity refuses startup without provisioning; restoring original identity permits restart")
            report["voter_replacement"] = "unsupported: fully wiped voters require new identity and joint membership, not same-ID automatic recreation"
            report["before_full_restart"] = caught_up()
            for member in list(processes):
                stop(member)
            for member in (1, 2, 3):
                start(member)
            current = leader()
            report["after_full_restart"] = caught_up()
            assert query(current, "MATCH (p:Page) RETURN count(*) AS n") == [{"n": rows + 3}]
            assert query(current, "MATCH (p:Page {entity_id:'idempotent'}) RETURN count(*) AS n") == [{"n": 1}]
            assert query(current, "MATCH (a:Page)-[:LINK]->(b:Page) RETURN count(*) AS n") == [{"n": 2}]
            with driver(current) as client:
                with client.session(database="pages") as session:
                    identities = [record["node"].element_id for record in session.run("MATCH (p:Page) RETURN p AS node")]
            assert len(identities) == rows + 3 and len(set(identities)) == rows + 3
            restarted_outcome = json.loads(command(current, "request", "--id", request_key).stdout)
            assert restarted_outcome == report["request_outcome"], "full restart changed durable request outcome"
            callbacks_before_restart_retry = len(operation_calls)
            with driver(current) as client:
                try:
                    GraphClient(client, "pages").write_transaction(write_once, request_id=seed)
                except Neo4jError as error:
                    assert error.code == "Neo.ClientError.Transaction.RequestAlreadyCommitted", error.code
                else:
                    raise AssertionError("full restart lost committed request deduplication")
            assert len(operation_calls) == callbacks_before_restart_retry, "restart retry reran the callback"
            assert query(current, "MATCH (p:Page {entity_id:'idempotent'}) RETURN count(*) AS n") == [{"n": 1}]
            report["request_outcome_after_full_restart"] = restarted_outcome
            report["request_replay_after_full_restart"] = {
                "code": "Neo.ClientError.Transaction.RequestAlreadyCommitted",
                "callback_calls_before": callbacks_before_restart_retry,
                "callback_calls_after": len(operation_calls),
                "matching_nodes": 1}
            report["checks"].append("after all three voters restart, quorum lookup preserves the exact committed request outcome; same-seed replay refuses before invoking the Python callback and retains one node")
            with driver(current) as client:
                assert GraphClient(client, "pages").upsert_node_with_acl("Page", "atomic-visible",
                    {"title": "atomic"}, {"allow": ["alice"]}, request_id="atomic-acl-success") is True
            assert query(current, "MATCH (p:Page {entity_id:'atomic-visible'}) RETURN count(*) AS n", principal="alice") == [{"n": 1}]
            with driver(current, "plain-writer") as client:
                try:
                    GraphClient(client, "pages").upsert_node_with_acl("Page", "atomic-denied",
                        {"title": "denied"}, {"allow": ["alice"]}, request_id="atomic-acl-denied")
                except Neo4jError as error:
                    assert error.code == "Neo.ClientError.Security.Forbidden", error.code
                else:
                    raise AssertionError("non-admin writer stamped trusted ACL")
            assert query(current, "MATCH (p:Page {entity_id:'atomic-denied'}) RETURN count(*) AS n") == [{"n": 0}]
            assert query(current, "MATCH (p:Page) RETURN count(*) AS n") == [{"n": rows + 4}]
            report["checks"].append("public atomic node+ACL helper exposes privileged acknowledged write to Alice; non-admin ACL rejection rolls back earlier MERGE with exact Forbidden error")
            report["post_atomic_status"] = caught_up()
            if log_cap_recovery:
                acknowledged = []
                uncertain = None
                capacity_callbacks = []
                payloads = {}
                capacity_samples = []
                final_payload = "synthetic-log-cap-" + "x" * (16 << 10)

                def capacity_write(seed):
                    def operation(tx):
                        capacity_callbacks.append(seed)
                        tx.run("CREATE (p:Page {entity_id:$id,title:$title})",
                               id=seed, title=payloads[seed]).consume()
                    with driver(current) as client:
                        return GraphClient(client, "pages").write_transaction(operation, request_id=seed)

                for index in range(256):
                    snapshots = [status(member) for member in (1, 2, 3)]
                    for sample in snapshots:
                        check_capacity(sample, max_log_bytes)
                    capacity_samples.append(snapshots)
                    remaining = sorted(item["log_remaining_bytes"] for item in snapshots)
                    cannot_fit_quorum = remaining[1] < len(final_payload.encode())
                    if cannot_fit_quorum:
                        report["log_cap_bound_readiness"] = probe(current, "readiness", True)
                        # A read barrier cannot establish space for a future proposal.
                        after_probe = [status(member) for member in (1, 2, 3)]
                        for sample in after_probe:
                            check_capacity(sample, max_log_bytes)
                        assert sorted(item["log_remaining_bytes"] for item in after_probe)[1] < len(final_payload.encode())
                        report["log_cap_bound_status_after_readiness"] = after_probe
                    seed = f"log-cap-{index}"
                    payloads[seed] = final_payload if cannot_fit_quorum else "x" * min(16 << 10, max(128, remaining[0] // 32))
                    try:
                        capacity_write(seed)
                        acknowledged.append(seed)
                        assert not cannot_fit_quorum, "proposal acknowledged despite insufficient retained-log quorum capacity"
                    except (CommitOutcomeUnknownError, Neo4jError, ServiceUnavailable, SessionExpired, OSError) as error:
                        # A failed response never establishes whether COMMIT became durable.
                        assert cannot_fit_quorum, "write failed before the objective raw-payload capacity bound"
                        uncertain = seed
                        report["log_cap_write_error"] = {"type": type(error).__name__,
                            "code": getattr(error, "code", None), "diagnostic": redact(str(error))[-4096:]}
                        post_error_probe = command(current, "probe", "--kind", "readiness", check=False)
                        report["log_cap_post_error_readiness_observation_only"] = {
                            "exit_code": post_error_probe.returncode,
                            "sample": json.loads(post_error_probe.stdout)}
                        break
                assert acknowledged and uncertain is not None, "bounded synthetic writes did not reach the cap"
                report["log_cap_acknowledged_request_ids"] = acknowledged
                report["log_cap_uncertain_request_id"] = uncertain
                for member in list(processes):
                    stop(member)
                report["log_cap_capacity_samples_before_writes"] = capacity_samples
                report["log_cap_uncertain_raw_payload_bytes"] = len(payloads[uncertain].encode())
                report["log_cap_exhaustion_basis"] = "before the failed proposal, a majority retained fewer free log bytes than its raw UTF-8 property alone; the serialized entry necessarily exceeds that lower bound"
                for member in (1, 2, 3):
                    original = configs[member].read_text()
                    assert original.count(f"max_log_bytes={max_log_bytes}\n") == 1
                    configs[member].write_text(original.replace(
                        f"max_log_bytes={max_log_bytes}\n", f"max_log_bytes={MAX_LOG_BYTES}\n"))
                report["log_cap_recovery_limits"] = {"initial": max_log_bytes, "restarted": MAX_LOG_BYTES}
                for member in (1, 2, 3):
                    start(member)
                current = leader()
                caught_up(timeout=60)
                report["log_cap_recovery_readiness"] = probe(current, "readiness", True)
                outcomes = {}
                for seed in acknowledged + [uncertain]:
                    key = build_request_key(cluster_id, "pages", "writer", seed)
                    outcome = json.loads(command(current, "request", "--id", key).stdout)
                    assert "Lookup" in outcome, "quorum-authoritative request lookup failed"
                    outcomes[seed] = outcome
                    if seed in acknowledged:
                        assert "Applied" in outcome["Lookup"], "acknowledged request was lost"
                # None is authoritative only because the request CLI performed its quorum barrier.
                unresolved = outcomes[uncertain]["Lookup"]
                if unresolved is None:
                    try:
                        capacity_write(uncertain)
                    except Neo4jError as error:
                        assert error.code == "Neo.ClientError.Transaction.RequestAlreadyCommitted", error.code
                else:
                    assert "Applied" in unresolved
                key = build_request_key(cluster_id, "pages", "writer", uncertain)
                resolved = json.loads(command(current, "request", "--id", key).stdout)
                assert "Applied" in resolved["Lookup"]
                report["log_cap_request_outcomes_before_retry"] = outcomes
                report["log_cap_uncertain_resolved"] = resolved
                callbacks_before_retry = len(capacity_callbacks)
                try:
                    capacity_write(acknowledged[0])
                except Neo4jError as error:
                    assert error.code == "Neo.ClientError.Transaction.RequestAlreadyCommitted", error.code
                else:
                    raise AssertionError("committed request replay unexpectedly acknowledged")
                assert len(capacity_callbacks) == callbacks_before_retry
                report["log_cap_committed_replay"] = {"request_id": acknowledged[0],
                    "code": "Neo.ClientError.Transaction.RequestAlreadyCommitted", "callback_rerun": False}
                for seed in acknowledged + [uncertain]:
                    assert query(current, "MATCH (p:Page {entity_id:$id}) RETURN p.title AS title", id=seed) == [{"title": payloads[seed]}]
                assert query(current, "MATCH (p:Page) RETURN count(*) AS n") == [{"n": rows + 5 + len(acknowledged)}]
                report["log_cap_recovery_status"] = caught_up(timeout=60)
                for sample in report["log_cap_recovery_status"]:
                    check_capacity(sample)
                report["checks"].append("opt-in 1 MiB retained-log cap exhausts on bounded explicit writes; same voters and volumes restart with 1 GiB cap; quorum lookup resolves every ACK and uncertain COMMIT; stable request replay preserves each exact payload once and all commit/apply indexes converge")
            report["graceful_stop"] = []
            for member in list(processes):
                process, began = processes[member], time.monotonic()
                stop(member, kill=False)
                report["graceful_stop"].append({"member": member, "exit_code": process.returncode,
                                                "exit_seconds": round(time.monotonic() - began, 6)})
                assert process.returncode == 0, report["graceful_stop"]
            report["checks"].append("SIGTERM drains and closes each voter in turn with exit 0")
            audits = {}
            for member in (1, 2, 3):
                result = command(member, "audit", "--config", str(servers[member]), timeout=60)
                audits[str(member)] = json.loads(result.stdout)
            report["offline_audits"] = audits
            # The parent-owned CLI exposes per-graph audit and logical CF digest.
            digests = []
            for audit in audits.values():
                assert audit["result"] == "PASS"
                assert set(audit["graphs"]) == {"pages", "aux"}
                per_graph = {}
                for graph, proof in audit["graphs"].items():
                    assert proof["status"] == "ok"
                    digest = proof["logical_sha256"]
                    assert len(digest) == 64 and all(c in "0123456789abcdef" for c in digest)
                    per_graph[graph] = digest
                digests.append(per_graph)
            assert digests[0] == digests[1] == digests[2]
            report["logical_graph_digests"] = digests[0]
            report["checks"].append("full process restart retains exactly acknowledged data; quiescent offline audits and all-11-CF logical graph digests agree across all voters")
            report["snapshots"] = "unsupported: no snapshot transfer or compaction acceptance claimed"
            report["result"] = "PASS"
        except Exception:
            print(json.dumps({"partial_checks": report["checks"], "binary_sha256": report["binary_sha256"]}), file=sys.stderr)
            for member in (1, 2, 3):
                log_path = root / str(member) / "server.log"
                if log_path.exists():
                    print(f"Owned replica {member} final log (at most 64 KiB):", file=sys.stderr)
                    with log_path.open("rb") as source:
                        source.seek(max(0, log_path.stat().st_size - (64 << 10)))
                        print(redact(source.read().decode(errors="replace")), file=sys.stderr)
            raise
        finally:
            faults.set(set())
            for resource in pending_resources:
                try:
                    resource.close()
                except Exception:
                    pass
            for member in list(processes):
                stop(member)
            for proxy in proxies:
                proxy.close()
            for entries in logs.values():
                for log in entries:
                    log.close()
        report["childProcessesStopped"] = not processes
        report["faultProxiesStopped"] = all(not proxy.thread.is_alive() for proxy in proxies)
    report["ownedResourcesRemoved"] = not root.exists()
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--rows", type=int, default=200)
    parser.add_argument("--self-check", action="store_true")
    parser.add_argument("--log-cap-recovery", action="store_true",
        help="opt in to 1 MiB log exhaustion/recovery after a four-row acceptance fixture")
    args = parser.parse_args()
    if not args.self_check and not args.binary:
        parser.error("--binary must identify the parent-provided candidate")
    print(json.dumps(self_check() if args.self_check else run(args.binary, 4 if args.log_cap_recovery else args.rows, args.log_cap_recovery), indent=2))
