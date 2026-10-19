"""Bounded synthetic write diagnostic with three native loopback voters.

Reuses the acceptance harness's framed peer proxies. Sequential Bolt requests
measure end-to-end ACK time with persistent connections; no fault is injected
while timing. Same-volume SIGKILL/restart and offline digest checks follow.
"""
import argparse
from contextlib import ExitStack
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import re
import secrets
import statistics
import subprocess
import sys
import tempfile
import time
import uuid

from quorum_replication import Faults, Proxy, SCHEMA, MAX_LOG_BYTES, port

if not __debug__:
    raise RuntimeError("this diagnostic requires Python assertions; remove -O/PYTHONOPTIMIZE")


def stop_owned(processes, member):
    process = processes.get(member)
    if process is not None:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=15)
        processes.pop(member)


def close_all(callbacks):
    errors = []
    for close in callbacks:
        try:
            close()
        except Exception as error:
            errors.append(error)
    if errors:
        raise RuntimeError("owned resource cleanup failed; all remaining resources were attempted") from errors[0]


def summarize(samples, rows):
    ordered = sorted(samples)
    return {"samples_ms": samples, "sample_count": len(samples),
            "p95_method": "nearest rank; fewer than 20 samples yields the maximum",
            "p50_ms": statistics.median(samples),
            "p95_ms": ordered[math.ceil(len(ordered) * .95) - 1],
            "max_ms": max(samples), "total_ms": sum(samples),
            "rows_per_second": rows * 1000 / sum(samples)}


def tail_log(path, secrets_to_hide, limit=64 * 1024):
    """Last `limit` bytes of a log with secrets redacted; tolerates a missing file."""
    try:
        with open(path, "rb") as log:
            log.seek(0, os.SEEK_END)
            size = log.tell()
            log.seek(max(0, size - limit))
            text = log.read().decode(errors="replace")
    except OSError as error:
        return {"error": str(error)}
    for secret in secrets_to_hide:
        text = text.replace(secret, "[redacted]")
    return {"bytes_total": size, "tail": text}


def lookup_request(command, members, request_id, wait=30, pause=.5):
    """Quorum-authoritative `cluster request` lookup, retried while no leader answers.

    `Lookup: null` means absent only because the CLI succeeded (it performs the quorum
    barrier); an exhausted wait is reported as unavailable, never as absent.
    """
    deadline, attempts, last = time.monotonic() + wait, 0, None
    while True:
        for member in members:
            attempts += 1
            try:
                outcome = command(member, "request", "--id", request_id, timeout=15)
            except Exception as error:
                last = f"voter {member}: {error}"
                continue
            if isinstance(outcome, dict) and "Lookup" in outcome:
                lookup = outcome["Lookup"]
                state = "absent" if lookup is None else "Applied" if "Applied" in lookup else "other"
                return {"state": state, "via_voter": member, "attempts": attempts, "outcome": outcome}
            last = f"voter {member}: unexpected response {outcome!r}"
        if time.monotonic() >= deadline:
            return {"state": "unavailable", "attempts": attempts, "last_error": last}
        time.sleep(pause)


def collect_failure(error, where, members, status, command, log_path, secrets_to_hide, wait=30):
    """Evidence for a failed write, gathered before any scratch state is removed.

    Does not retry the write; the outcome is reconciled by request ID only.
    """
    elapsed_ms = (time.perf_counter_ns() - where["started_ns"]) / 1e6 if "started_ns" in where else None
    message = str(error)
    for secret in secrets_to_hide:
        message = message.replace(secret, "[redacted]")
    found = re.search(r"request ([0-9a-f]{16,})", message)
    request_id = found.group(1) if found else None

    def sample():
        samples = {}
        for member in members:
            try:
                samples[str(member)] = status(member)
            except Exception as problem:
                samples[str(member)] = {"error": str(problem)[:500]}
        return samples

    failure = {"result": "FAIL", "error_type": type(error).__name__, "error_code": getattr(error, "code", None),
               "error": message, "request_id": request_id,
               "repeat": where.get("repeat"), "mode": where.get("mode"), "row_offset": where.get("offset"),
               "rows_in_failed_transaction": where.get("rows"), "failed_call_elapsed_ms": elapsed_ms,
               "status_at_failure": sample()}
    failure["request_lookup"] = (lookup_request(command, members, request_id, wait) if request_id
                                 else {"state": "unavailable", "last_error": "no request id in error message"})
    failure["status_after_lookup"] = sample()
    failure["server_log_tails"] = {str(member): tail_log(log_path(member), secrets_to_hide) for member in members}
    failure["note"] = ("the failed write was not retried; a timeout does not mean rollback. "
                       "request_lookup.state is authoritative only when it is Applied or absent")
    text = json.dumps(failure)
    for secret in secrets_to_hide:
        text = text.replace(secret, "[redacted]")
    return json.loads(text)


def self_check():
    sample = summarize([1.0, 3.0, 2.0], 6)
    assert sample["p50_ms"] == 2 and sample["p95_ms"] == 3
    assert sample["rows_per_second"] == 1000
    class Process:
        def poll(self):
            return None

        def kill(self):
            pass

        def wait(self, timeout):
            raise subprocess.TimeoutExpired("owned-fake", timeout)

    processes = {1: Process()}
    try:
        stop_owned(processes, 1)
    except subprocess.TimeoutExpired:
        assert 1 in processes
    else:
        raise AssertionError("failed stop must retain ownership")
    attempted = []
    def fail():
        raise OSError("synthetic cleanup failure")
    try:
        close_all([fail, lambda: attempted.append(True)])
    except RuntimeError:
        assert attempted == [True]
    else:
        raise AssertionError("cleanup failure must be reported")
    calls = []
    def flaky(member, *arguments, timeout=None):
        calls.append(member)
        if len(calls) < 3:
            raise RuntimeError("no quorum-authoritative request outcome available")
        return {"Lookup": None}
    found = lookup_request(flaky, (1, 2, 3), "ab" * 32, wait=5, pause=0)
    assert found["state"] == "absent" and found["attempts"] == 3
    assert lookup_request(lambda *a, **k: {"Lookup": {"Applied": {}}}, (1,), "ab" * 32)["state"] == "Applied"
    def down(*a, **k):
        raise RuntimeError("no leader")
    assert lookup_request(down, (1, 2), "ab" * 32, wait=0, pause=0)["state"] == "unavailable"
    with tempfile.TemporaryDirectory() as folder:
        log = Path(folder) / "server.log"
        log.write_text("x" * 100 + "SECRET tail")
        shown = tail_log(log, ["SECRET"], limit=20)
        assert shown["tail"] == "xxxxxxxxx[redacted] tail" and shown["bytes_total"] == 111
        assert "error" in tail_log(Path(folder) / "missing.log", [])
        class Driver(Exception):
            code = "Neo.ClientError.Transaction.CommitOutcomeUnknown"
        def status(member):
            if member == 2:
                raise RuntimeError("voter down SECRET")
            return {"state": "Leader"}
        receipt = collect_failure(Driver("commit outcome unknown for request " + "ab" * 32 + "; SECRET"),
            {"repeat": 3, "mode": "single", "offset": 7, "rows": 1, "started_ns": time.perf_counter_ns()},
            (1, 2), status, flaky, lambda member: log, ["SECRET"], wait=1)
        assert receipt["request_id"] == "ab" * 32 and receipt["row_offset"] == 7 and receipt["repeat"] == 3
        assert receipt["error_code"].endswith("CommitOutcomeUnknown") and receipt["request_lookup"]["state"] == "absent"
        assert "SECRET" not in json.dumps(receipt) and "error" in receipt["status_at_failure"]["2"]
        assert receipt["status_at_failure"]["1"] == {"state": "Leader"} and receipt["failed_call_elapsed_ms"] >= 0
        assert collect_failure(RuntimeError("boom"), {}, (1,), status, down, lambda member: log, [])["request_lookup"]["state"] == "unavailable"
    return {"result": "PASS", "checks": ["timing summary", "failed stop retains ownership", "cleanup attempts later resources",
            "request lookup retries then reports absent/Applied/unavailable", "log tail bounded and redacted", "failure evidence from fakes"]}


def run(binary, rows, batch_size, repeats, failure_receipt=None):
    from neo4j import GraphDatabase, __version__ as driver_version
    if not 2 <= rows <= 2000 or not 2 <= batch_size <= rows or rows % batch_size:
        raise ValueError("rows must be 2..2000 and divisible by batch-size (2..rows)")
    # 300 x 2,020 entries stays under the 1 GiB retained-log cap (snapshots are off).
    if not 1 <= repeats <= 300:
        raise ValueError("repeats must be 1..300")
    started_run = time.perf_counter()
    binary = binary.resolve(strict=True)
    report = {"result": "RUNNING", "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
              "driver": driver_version, "platform": platform.platform(),
              "rows_per_round": rows, "batch_size": batch_size, "repeats": repeats,
              "scope": "synthetic single-client three-native-process loopback diagnostic; includes Bolt, Cypher, quorum and durable ACK; not multi-machine throughput",
              "timing": "session.run(...).consume(); persistent authenticated Bolt session; alternating mode order; fixture creation and validation excluded",
              "durability": "candidate defaults retained; no WAL or sync setting changed",
              "queries": {"single": "CREATE (p:Page {entity_id:$id,title:$title})",
                          "batch": "UNWIND $rows AS row CREATE (p:Page {entity_id:row.id,title:row.title})"},
              "latency_unit": "milliseconds per acknowledged transaction; batch contains batch_size rows",
              "rounds": []}
    environment = {key: value for key, value in os.environ.items() if not key.startswith("BRAINWIRE_")}
    processes, logs, proxies = {}, [], []
    faults = Faults()
    token, peer_secret = secrets.token_hex(32), secrets.token_hex(32)
    with tempfile.TemporaryDirectory(prefix="brainwire-quorum-write-") as folder, ExitStack() as resources:
        root = Path(folder)
        peer_token = root / "peer-token"
        peer_token.write_text(peer_secret + "\n")
        peer_token.chmod(0o600)
        policy = root / "policy.json"
        policy.write_text(json.dumps({"principals": [{"id": "writer", "token_sha256": hashlib.sha256(token.encode()).hexdigest(),
            "graphs": {"pages": "write"}, "instance_admin": True}]}))
        policy.chmod(0o600)
        graphs = root / "graphs.json"
        graphs.write_text(json.dumps({"pages": SCHEMA}))
        backend = {member: port() for member in (1, 2, 3)}
        bolt = {member: port() for member in (1, 2, 3)}
        for member in (1, 2, 3):
            proxy = Proxy(member, ("127.0.0.1", backend[member]), faults)
            resources.callback(proxy.close)
            proxies.append(proxy)
        configs, servers = {}, {}
        cluster_id = "write-diagnostic-" + uuid.uuid4().hex
        for member in (1, 2, 3):
            directory = root / str(member)
            directory.mkdir()
            servers[member] = directory / "server.toml"
            servers[member].write_text(
                f'[server]\nlisten="127.0.0.1:{bolt[member]}"\ndata_dir={json.dumps(str(directory / "data"))}\n'
                f'default_database="pages"\nauth_file={json.dumps(str(policy))}\n'
                'max_message="8MiB"\nmax_decoded_message="16MiB"\nquery_timeout_ms=10000\nio_timeout_ms=300000\n'
                '[memory]\ncache="64MiB"\nmemtables="16MiB"\nquery="64MiB"\n')
            configs[member] = directory / "cluster.toml"
            configs[member].write_text(
                f'node_id={member}\ncluster_id="{cluster_id}"\nlisten="127.0.0.1:{backend[member]}"\n'
                f'token_file={json.dumps(str(peer_token))}\nmax_log_bytes={MAX_LOG_BYTES}\n' +
                ''.join(f'[[peers]]\nid={index}\naddress="{proxy.address}"\n' for index, proxy in enumerate(proxies, 1)))

        def redact(text):
            return text.replace(token, "[redacted]").replace(peer_secret, "[redacted]")

        def command(member, *arguments, timeout=30):
            result = subprocess.run([str(binary), "cluster", "--cluster-config", str(configs[member]), *arguments],
                env=environment, capture_output=True, text=True, timeout=timeout)
            if result.returncode:
                raise RuntimeError(redact(result.stderr))
            return json.loads(result.stdout)

        def status(member):
            return command(member, "status", timeout=8)["Status"]

        def start(member):
            log = (root / str(member) / "server.log").open("ab")
            logs.append(log)
            processes[member] = subprocess.Popen([str(binary), "serve", "--config", str(servers[member]),
                "--cluster-config", str(configs[member])], env=environment, stdout=log, stderr=log)
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                if processes[member].poll() is not None:
                    raise RuntimeError(f"owned replica {member} exited; no secret-bearing log emitted")
                try:
                    status(member)
                    return
                except (RuntimeError, OSError, ValueError, subprocess.TimeoutExpired):
                    time.sleep(.1)
            raise RuntimeError("owned replica did not start")

        def stop(member):
            stop_owned(processes, member)

        def leader():
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                for member in (1, 2, 3):
                    info = status(member)
                    if info.get("leader") == member and info.get("state") == "Leader":
                        return member
                time.sleep(.2)
            raise RuntimeError("no leader elected")

        def caught_up():
            deadline = time.monotonic() + 60
            while time.monotonic() < deadline:
                snapshots = [status(member) for member in (1, 2, 3)]
                committed = [item.get("committed_index") or 0 for item in snapshots]
                if committed[0] and len(set(committed)) == 1 and [item.get("last_applied") for item in snapshots] == committed:
                    return snapshots
                time.sleep(.2)
            raise RuntimeError("voters did not converge")

        def driver(member):
            return GraphDatabase.driver(f"bolt://127.0.0.1:{bolt[member]}", auth=("writer", token),
                connection_timeout=2, connection_acquisition_timeout=4, max_transaction_retry_time=0)

        try:
            for member in (1, 2, 3):
                command(member, "provision", "--config", str(servers[member]))
                start(member)
            command(1, "init", "--schemas", str(graphs), "--request-id", "bootstrap")
            current = leader()
            expected, where = {}, {}
            with driver(current) as client, client.session(database="pages") as session:
                session.run("MATCH (p:Page) RETURN count(*) AS n").consume()
                try:
                    for repeat in range(repeats):
                        for mode in (("single", "batch") if repeat % 2 == 0 else ("batch", "single")):
                            fixture = [{"id": f"{repeat}-{mode}-{i}", "title": f"title-{i}"} for i in range(rows)]
                            expected.update({item["id"]: item["title"] for item in fixture})
                            samples = []
                            size = 1 if mode == "single" else batch_size
                            for offset in range(0, rows, size):
                                chunk = fixture[offset:offset + size]
                                where.update(repeat=repeat, mode=mode, offset=offset, rows=len(chunk))
                                started = where["started_ns"] = time.perf_counter_ns()
                                if mode == "single":
                                    session.run("CREATE (p:Page {entity_id:$id,title:$title})", **chunk[0]).consume()
                                else:
                                    session.run("UNWIND $rows AS row CREATE (p:Page {entity_id:row.id,title:row.title})", rows=chunk).consume()
                                samples.append((time.perf_counter_ns() - started) / 1e6)
                            report["rounds"].append({"repeat": repeat, "mode": mode, "transactions": len(samples),
                                "rows": rows, **summarize(samples, rows)})
                except Exception as error:
                    failure = collect_failure(error, where, (1, 2, 3), status, command,
                        lambda member: root / str(member) / "server.log", [token, peer_secret])
                    failure.update({"binary_sha256": report["binary_sha256"], "driver": driver_version, "platform": report["platform"],
                        "rows_per_round": rows, "batch_size": batch_size, "repeats": repeats, "leader_member": current,
                        "completed_rounds": [{key: item[key] for key in ("repeat", "mode", "transactions", "p50_ms", "max_ms")} for item in report["rounds"]]})
                    text = json.dumps(failure, indent=2)
                    if failure_receipt:
                        Path(failure_receipt).write_text(text + "\n")
                    else:
                        print(text, file=sys.stderr)
                    raise
            report["aggregate"] = {}
            for mode in ("single", "batch"):
                selected = [item for item in report["rounds"] if item["mode"] == mode]
                report["aggregate"][mode] = summarize([sample for item in selected for sample in item["samples_ms"]], rows * repeats)
            report["before_restart"] = caught_up()
            for member in list(processes):
                stop(member)
            for member in (1, 2, 3):
                start(member)
            current = leader()
            report["after_restart"] = caught_up()
            # Exact count, then every expected row in bounded blocks: one unbounded
            # read of a large run exceeds the server's query allocation guard.
            with driver(current) as client, client.session(database="pages") as session:
                assert session.run("MATCH (p:Page) RETURN count(*) AS n").single()["n"] == len(expected)
                ids = list(expected)
                for start_at in range(0, len(ids), 2000):
                    block = ids[start_at:start_at + 2000]
                    actual = session.run("UNWIND $ids AS id MATCH (p:Page {entity_id:id}) RETURN p.entity_id AS id,p.title AS title",
                                         ids=block).data()
                    assert {item["id"]: item["title"] for item in actual} == {key: expected[key] for key in block}
            report["verified_rows_after_sigkill_restart"] = len(expected)
            for member in list(processes):
                stop(member)
            audits = {str(member): command(member, "audit", "--config", str(servers[member]), timeout=60) for member in (1, 2, 3)}
            digests = []
            for audit in audits.values():
                assert audit["result"] == "PASS" and set(audit["graphs"]) == {"pages"}
                assert audit["graphs"]["pages"]["status"] == "ok"
                digests.append(audit["graphs"]["pages"]["logical_sha256"])
            assert len(digests[0]) == 64 and len(set(digests)) == 1
            report["offline_audits"] = audits
            report["logical_graph_sha256"] = digests[0]
            report["result"] = "PASS"
        finally:
            close_all([*(lambda member=member: stop(member) for member in list(processes)),
                       *(proxy.close for proxy in proxies), *(log.close for log in logs)])
        report["child_processes_stopped"] = not processes
        report["fault_proxies_stopped"] = all(not proxy.thread.is_alive() for proxy in proxies)
    report["owned_resources_removed"] = not root.exists()
    report["total_elapsed_seconds"] = time.perf_counter() - started_run
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--rows", type=int, default=100)
    parser.add_argument("--batch-size", type=int, default=25)
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--failure-receipt", type=Path, help="write the failure evidence here if a write fails (default: stderr)")
    parser.add_argument("--self-check", action="store_true")
    args = parser.parse_args()
    if not args.self_check and not args.binary:
        parser.error("--binary must identify the candidate")
    print(json.dumps(self_check() if args.self_check else run(args.binary, args.rows, args.batch_size, args.repeats, args.failure_receipt), indent=2))
