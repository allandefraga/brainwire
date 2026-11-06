"""Exercise relationship property indexing on an isolated synthetic Bolt graph."""

import argparse
from datetime import datetime, timedelta, timezone
import hashlib
import json
import math
from pathlib import Path
import secrets
import signal
import statistics
import subprocess
import tempfile
import time
import uuid

from bench_relationship_types import bounded, cleanup, connect, deadline_exceeded, docker, interrupted

from neo4j import GraphDatabase, Query

BASE = datetime(2026, 1, 1, tzinfo=timezone.utc)
DECLARATION = "CREATE INDEX by_at IF NOT EXISTS FOR ()-[r:EVENT]-() ON (r.at)"
STATUS = (
    "CALL db.indexes() YIELD label, property, state, name, entityType "
    "WHERE label = 'EVENT' AND property = 'at' RETURN state, name, entityType"
)
PATTERN = "MATCH ()-[r:EVENT]->()"
RANGE = PATTERN + " WHERE r.at >= $lo AND r.at < $hi"
EQUALITY = PATTERN + " WHERE r.at = $point"
# The dynamic expression preserves the result for the generated non-null ISO
# timestamps. It prevents matching the current planner's simple property bound.
SCAN_RANGE = PATTERN + " WHERE coalesce(r.at, '') >= $lo AND coalesce(r.at, '') < $hi"
SCAN_EQUALITY = PATTERN + " WHERE coalesce(r.at, '') = $point"


def at(i):
    return (BASE + timedelta(seconds=i)).strftime("%Y-%m-%dT%H:%M:%SZ")


def rows(tx, statement, **params):
    return tx.run(statement, **params).data()


def check_rows(tx, pattern, expected, **params):
    actual = [r["seq"] for r in rows(tx, pattern + " RETURN r.seq AS seq ORDER BY r.seq", **params)]
    if actual != expected:
        raise AssertionError(f"{pattern} got {actual}, expected {expected}")


def check_count(tx, pattern, expected, **params):
    result = tx.run(Query(pattern + " RETURN count(r)", timeout=30), **params).single(strict=True)[0]
    if result != expected:
        raise AssertionError(f"{pattern} counted {result}, expected {expected}")


def check_state(session, expected):
    actual = rows(session, STATUS)
    if actual != expected:
        raise AssertionError(f"unexpected relationship index status {actual}")


def measure(session, probes, samples):
    timings = {name: [] for name in probes}
    for i in range(samples + 2):
        order = list(probes) if i % 2 == 0 else reversed(probes)
        for name in order:
            pattern, expected, params = probes[name]
            start = time.monotonic()
            check_count(session, pattern, expected, **params)
            if i >= 2:
                timings[name].append((time.monotonic() - start) * 1000)
    return {
        name: {
            "p50_ms": statistics.median(values),
            "p95_ms": sorted(values)[math.ceil(.95 * len(values)) - 1],
            "samples_ms": values,
        }
        for name, values in timings.items()
    }


def exercise(args, report, directory, ownership):
    token = secrets.token_hex(32)
    policy = directory / "policy.json"
    policy.write_text(json.dumps({"principals": [{
        "id": "bench", "token_sha256": hashlib.sha256(token.encode()).hexdigest(),
        "graphs": {"bench": "write"}, "instance_admin": True,
    }]}))
    policy.chmod(0o444)
    schema = directory / "schema.json"
    schema.write_text(json.dumps({
        "version": "1", "nodes": [{"label": "Actor"}, {"label": "Matter"}],
        "edges": [{"type": kind, "from": {"label": "Actor"}, "to": {"label": "Matter"},
                   "properties": ["at", "seq"]} for kind in ("EVENT", "AUDIT")],
    }))
    schema.chmod(0o444)
    image = docker("image", "inspect", "--format", "{{.Id}}", args.image).stdout.strip()
    report["image_id"] = image
    container = f"bw-relidx-{report['run']}"
    volume = f"{container}-data"
    if docker("volume", "inspect", volume, check=False).returncode == 0:
        raise RuntimeError("generated volume already exists")
    docker("volume", "create", "--label", ownership, volume)
    owned = docker("volume", "ls", "-q", "--filter", f"label={ownership}").stdout.splitlines()
    if volume not in owned:
        raise RuntimeError("generated volume has a different ownership label")
    common = (
        "--pull", "never", "--label", ownership, "--memory", "3g", "--memory-swap", "3g",
        "--cpus", "2", "--mount", f"type=volume,src={volume},dst=/data",
        "-e", "BRAINWIRE_MEMORY_CACHE=256MiB", "-e", "BRAINWIRE_MEMORY_MEMTABLES=64MiB",
        "-e", "BRAINWIRE_MEMORY_QUERY=256MiB", "-e", "BRAINWIRE_SERVER_DEFAULT_DATABASE=bench",
    )
    opt_in = ("-e", "BRAINWIRE_SERVER_RELATIONSHIP_INDEXES=true")
    docker("run", "--rm", "--network", "none", *common, *opt_in,
           "--mount", f"type=bind,src={schema},dst=/ontology.json,readonly",
           image, "db", "create", "bench", "--ontology", "/ontology.json")
    identifier = docker(
        "run", "-d", "--name", container, *common, *opt_in,
        "--mount", f"type=bind,src={policy},dst=/policy.json,readonly",
        "-e", "BRAINWIRE_SERVER_AUTH_FILE=/policy.json",
        "-p", "127.0.0.1::7687", image,
    ).stdout.strip()
    event = (
        "MATCH (a:Actor {entity_id: $source}), (b:Matter {entity_id: $target}) "
        "CREATE (a)-[:EVENT {at: $at, seq: $seq}]->(b)"
    )
    mid = args.rows // 2
    lo, hi = at(mid), at(mid + 10)
    point = at(mid + 2)
    wanted = list(range(mid, mid + 10))
    probes = {
        "range": (RANGE, 10, {"lo": lo, "hi": hi}),
        "equality": (EQUALITY, 1, {"point": point}),
    }
    ready = [{"state": "scalar_range_ready", "name": "by_at", "entityType": "relationship"}]
    populated = [{"state": "populating", "name": "by_at", "entityType": "relationship"}]

    with connect(GraphDatabase.driver, identifier, token, 60) as session:
        for label in ("Actor", "Matter"):
            rows(session, f"UNWIND $ids AS id CREATE (n:{label} {{entity_id:id}})",
                 ids=[str(i) for i in range(100 if label == "Actor" else (args.rows + 99) // 100)] + ["extra"])
        for start in range(0, args.rows, args.batch):
            session.run(Query(
                "UNWIND $rows AS row MATCH (a:Actor {entity_id: row.source}), "
                "(b:Matter {entity_id: row.target}) CREATE (a)-[:EVENT {at: row.at, seq: row.seq}]->(b)",
                timeout=30,
            ), rows=[
                {"source": str(i % 100), "target": str(i // 100), "at": at(i), "seq": i}
                for i in range(start, min(start + args.batch, args.rows))
            ]).consume()
        rows(session, event.replace(":EVENT", ":AUDIT"), source="0", target="0", at=point, seq=-1)
        check_count(session, PATTERN, args.rows)
        check_state(session, [])
        check_rows(session, RANGE, wanted, lo=lo, hi=hi)
        check_rows(session, EQUALITY, [mid + 2], point=point)
        report["before_ddl"] = measure(session, probes, args.samples)

        # Read the catalog in the same transaction as the DDL. The server's
        # background builder cannot process the declaration before commit.
        started = time.monotonic()
        with session.begin_transaction(timeout=30) as tx:
            tx.run(DECLARATION).consume()
            check_state(tx, populated)
            check_rows(tx, RANGE, wanted, lo=lo, hi=hi)
            check_rows(tx, EQUALITY, [mid + 2], point=point)
            rows(tx, event, source="0", target="extra", at=at(args.rows), seq=args.rows)
            tx.commit()
        report["observed_populating"] = True
        deadline = time.monotonic() + 90
        while True:
            state = rows(session, STATUS)
            if state == ready:
                break
            if state != populated:
                raise RuntimeError(f"unexpected index state during population: {state}")
            if time.monotonic() >= deadline:
                raise TimeoutError("relationship index readiness")
            time.sleep(.05)
        report["ready_after_s"] = time.monotonic() - started
        check_rows(session, EQUALITY, [args.rows], point=at(args.rows))
        check_rows(session, RANGE, wanted, lo=lo, hi=hi)
        check_rows(session, EQUALITY, [mid + 2], point=point)
        report["ready_measurements"] = measure(session, {
            **probes,
            "forced_scan_range": (SCAN_RANGE, 10, {"lo": lo, "hi": hi}),
            "forced_scan_equality": (SCAN_EQUALITY, 1, {"point": point}),
        }, args.samples)

        duplicate = args.rows + 1
        rows(session, event, source="extra", target="extra",
             at=point, seq=duplicate)
        check_rows(session, EQUALITY, [mid + 2, duplicate], point=point)
        check_rows(session, SCAN_EQUALITY, [mid + 2, duplicate], point=point)
        rows(session, EQUALITY + " AND r.seq = $seq SET r.at = $new",
             point=point, seq=mid + 2, new=at(args.rows + 10))
        check_rows(session, EQUALITY, [duplicate], point=point)
        check_rows(session, EQUALITY, [mid + 2], point=at(args.rows + 10))
        rows(session, EQUALITY + " SET r.at = null", point=at(args.rows + 10))
        check_rows(session, EQUALITY, [], point=at(args.rows + 10))
        rows(session, EQUALITY + " DELETE r", point=point)
        check_rows(session, EQUALITY, [], point=point)
        wanted.remove(mid + 2)
        check_rows(session, RANGE, wanted, lo=lo, hi=hi)

        rows(session, "CREATE (:Actor {entity_id:'detach'})")
        detach_point = at(args.rows + 11)
        rows(session, event, source="detach", target="0", at=detach_point, seq=duplicate + 1)
        check_count(session, EQUALITY, 1, point=detach_point)
        rows(session, "MATCH (n:Actor {entity_id:'detach'}) DETACH DELETE n")
        check_count(session, EQUALITY, 0, point=detach_point)

    docker("kill", identifier)
    docker("start", identifier)
    with connect(GraphDatabase.driver, identifier, token, 60) as session:
        check_state(session, ready)
        check_rows(session, RANGE, wanted, lo=lo, hi=hi)
        check_rows(session, EQUALITY, [], point=point)
        check_rows(session, EQUALITY, [args.rows], point=at(args.rows))
        check_count(session, EQUALITY, 0, point=detach_point)
        report["reopen_counts"] = {"range": len(wanted), "deleted_value": 0, "staged_value": 1}

    docker("stop", identifier)

    def offline(command, executable=image, environment=opt_in, check=True):
        return docker(
            "run", "--rm", "--network", "none", *common, *environment,
            executable, "db", command, "bench", "--json", check=check,
        )

    if args.older_image:
        older = docker("image", "inspect", "--format", "{{.Id}}", args.older_image).stdout.strip()
        if older == image:
            raise RuntimeError("the older control image must differ from the candidate")
        before = json.loads(offline("commit-state").stdout)
        rejected = offline("check", executable=older, environment=(), check=False)
        reason = (rejected.stderr + rejected.stdout).strip()
        if rejected.returncode == 0 or "relationship_index_v1" not in reason:
            raise AssertionError(f"older image did not refuse the new column family: {reason}")
        after = json.loads(offline("commit-state").stdout)
        if before != after or before.get("tracked") is not True:
            raise AssertionError("older open changed the candidate's tracked commit state")
        report["older_reopen_refusal"] = {
            "image": args.older_image, "image_id": older, "exit_code": rejected.returncode,
            "reason": reason[-1000:], "sequence_before": before["sequence"],
            "sequence_after": after["sequence"],
        }

    report["integrity"] = json.loads(offline("check").stdout)
    if report["integrity"].get("status") != "ok":
        raise AssertionError("offline integrity check did not pass")
    docker("start", identifier)
    with connect(GraphDatabase.driver, identifier, token, 60) as session:
        check_state(session, ready)
        check_count(session, PATTERN, args.rows + 1)
        check_rows(session, RANGE, wanted, lo=lo, hi=hi)
        check_rows(session, EQUALITY, [], point=point)
        check_rows(session, EQUALITY, [args.rows], point=at(args.rows))
        report["reopen_after_offline_checks"] = True
        rows(session, "DROP INDEX by_at")
        check_state(session, [])
        check_rows(session, RANGE, wanted, lo=lo, hi=hi)
        report["drop_scan_equivalence"] = True


def main(args):
    run = uuid.uuid4().hex
    report = {
        "run": run, "result": "FAIL", "image": args.image, "rows": args.rows, "batch": args.batch,
        "samples": args.samples, "memory_gib": 3, "cpus": 2,
        "scope": "synthetic Bolt queries on one local standalone container",
        "plan_evidence": "populating-state correctness and timing only; EXPLAIN/PROFILE unavailable",
    }
    ownership = f"brainwire.benchmark.run={run}"
    with args.output.open("x", encoding="utf-8") as output:
        with tempfile.TemporaryDirectory(prefix="bw-relidx-", dir=args.output.resolve().parent) as tmp:
            signal.signal(signal.SIGALRM, deadline_exceeded)
            signal.signal(signal.SIGTERM, interrupted)
            signal.alarm(args.deadline)
            try:
                exercise(args, report, Path(tmp), ownership)
                report["result"] = "PASS"
            except (Exception, KeyboardInterrupt) as error:
                report["error"] = type(error).__name__
                report["detail"] = error.stderr[-1000:] if isinstance(error, subprocess.CalledProcessError) else str(error)
                raise
            finally:
                signal.alarm(0)
                try:
                    report["cleanup"] = cleanup(ownership)
                except (OSError, subprocess.SubprocessError):
                    report["cleanup"] = False
                if not report["cleanup"]:
                    report["result"] = "FAIL"
                json.dump(report, output, indent=2)
                output.write("\n")
                print(f"{report['result']} {args.output}", flush=True)
    if not report["cleanup"]:
        raise RuntimeError(f"cleanup incomplete for {ownership}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True)
    parser.add_argument("--older-image", help="locally available image predating the relationship-index format")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--rows", type=bounded(24, 300_000), default=100_000)
    parser.add_argument("--batch", type=bounded(1, 1000), default=500)
    parser.add_argument("--samples", type=bounded(1, 30), default=9)
    parser.add_argument("--deadline", type=bounded(1, 900), default=600)
    main(parser.parse_args())
