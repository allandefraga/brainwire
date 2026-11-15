"""Compare serial relationship writes on two isolated, local Brainwire binaries.

From the repository root with the Bolt driver installed:
  python scripts/bench_edge_admission.py --baseline /path/to/old --candidate /path/to/new
Each measurement includes Bolt round-trip and autocommit. Each query changes a
small revision property and preserves the existing synthetic payload.
"""

import argparse
from contextlib import ExitStack, contextmanager
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import random
import socket
import statistics
import subprocess
import sys
import tempfile
import time

from neo4j import GraphDatabase, Query, __version__ as driver_version

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from examples.python.create_service_tokens import create
from examples.python.duckdb_acceptance import server

DATABASE = "edge_admission"
PAIR = "MATCH (a:Item {entity_id:$source}), (b:Item {entity_id:'target'}) "
PAYLOAD_SIZES = (64, 256 * 1024)
WORKLOADS = {
    "pair_merge": ("LINK", "MERGE (a)-[:LINK {revision:$revision}]->(b)"),
    "pair_set": ("LINK", "MATCH (a)-[r:LINK]->(b) SET r.revision = $revision"),
    "keyed_merge": ("EVENT {kind:'event'}", "MERGE (a)-[r:EVENT {kind:'event'}]->(b) SET r.revision = $revision"),
    "keyed_set": ("EVENT {kind:'event'}", "MATCH (a)-[r:EVENT]->(b) SET r.revision = $revision"),
}
MEMORY = {
    "BRAINWIRE_MEMORY_CACHE": "64MiB",
    "BRAINWIRE_MEMORY_MEMTABLES": "4MiB",
    "BRAINWIRE_MEMORY_QUERY": "16MiB",
    "BRAINWIRE_MEMORY_LIMIT": "2GiB",
}


@contextmanager
def instance(binary, root, ontology):
    root.mkdir()
    auth = create(root / "auth", DATABASE)
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        address = f"127.0.0.1:{listener.getsockname()[1]}"
    env = {key: value for key, value in os.environ.items() if not key.startswith("BRAINWIRE_")}
    env.update(MEMORY, BRAINWIRE_SERVER_LISTEN=address,
               BRAINWIRE_SERVER_DATA_DIR=str(root / "data"),
               BRAINWIRE_SERVER_DEFAULT_DATABASE=DATABASE,
               BRAINWIRE_SERVER_AUTH_FILE=str(auth / "policy.json"))
    subprocess.run([binary, "db", "create", DATABASE, "--ontology", str(ontology)],
                   env=env, check=True, capture_output=True, timeout=30)
    credential = json.loads((auth / "writer.json").read_text())
    with GraphDatabase.driver(
        f"bolt://{address}", auth=(credential["principal"], credential["credentials"]),
        connection_timeout=2, connection_acquisition_timeout=5, max_transaction_retry_time=0,
    ) as driver, server(binary, root, env, driver), driver.session(database=DATABASE) as session:
        yield session


def latency(values):
    ordered = sorted(values)
    return {"p50_ms": statistics.median(ordered) / 1e6,
            "p95_ms": ordered[math.ceil(len(ordered) * .95) - 1] / 1e6}


def identity(binary):
    version = subprocess.run([binary, "--version"], check=True, capture_output=True,
                             text=True, timeout=5).stdout.strip()
    with binary.open("rb") as stream:
        return {"version": version, "sha256": hashlib.file_digest(stream, "sha256").hexdigest()}


def run(binaries, samples, warmup):
    identities = {name: identity(path) for name, path in binaries.items()}
    results = []
    rng = random.Random(42)
    payloads = {size: rng.randbytes(size // 2).hex() for size in PAYLOAD_SIZES}
    with tempfile.TemporaryDirectory(prefix="brainwire-edge-admission-") as temporary:
        root = Path(temporary)
        ontology = root / "ontology.json"
        ontology.write_text(json.dumps({"version": "1", "nodes": [{"label": "Item"}], "edges": [
            {"type": "LINK", "from": {"label": "Item"}, "to": {"label": "Item"}},
            {"type": "EVENT", "from": {"label": "Item"}, "to": {"label": "Item"},
             "properties": ["kind"], "merge_key_fields": ["kind"]},
        ]}))
        with ExitStack() as stack:
            sessions = {
                name: stack.enter_context(instance(binary, root / name, ontology))
                for name, binary in binaries.items()
            }
            for session in sessions.values():
                session.run(Query("CREATE (:Item {entity_id:'target'})", timeout=5)).consume()
            for size, payload in payloads.items():
                for workload, (pattern, mutation) in WORKLOADS.items():
                    source = f"{workload}-{size}"
                    for session in sessions.values():
                        session.run(Query("CREATE (:Item {entity_id:$source})", timeout=5), source=source).consume()
                        session.run(
                            Query(PAIR + f"MERGE (a)-[r:{pattern}]->(b) SET r.payload = $payload", timeout=5),
                            source=source, payload=payload,
                        ).consume()
                    query = Query(PAIR + mutation, timeout=5)
                    times = {name: [] for name in sessions}
                    for revision in range(1, warmup + samples + 1):
                        order = tuple(sessions) if revision % 2 else tuple(reversed(sessions))
                        for name in order:
                            started = time.perf_counter_ns()
                            sessions[name].run(query, source=source, revision=revision).consume()
                            elapsed = time.perf_counter_ns() - started
                            if revision > warmup:
                                times[name].append(elapsed)
                    for session in sessions.values():
                        record = session.run(
                            Query(PAIR + "MATCH (a)-[r]->(b) RETURN r.revision AS revision, r.payload = $payload AS intact",
                                  timeout=5), source=source, payload=payload,
                        ).single(strict=True)
                        if record["revision"] != warmup + samples or record["intact"] is not True:
                            raise RuntimeError(f"unexpected final relationship state for {workload}, {size} bytes")
                    results.append({"operation": workload, "payload_bytes": size,
                                    **{name: latency(values) for name, values in times.items()}})
    if {name: identity(path) for name, path in binaries.items()} != identities:
        raise RuntimeError("a benchmark binary changed during the measurement")
    return {"binaries": identities,
            "environment": {"system": platform.system(), "release": platform.release(),
                            "machine": platform.machine(), "bolt_driver": driver_version,
                            "memory": MEMORY},
            "method": "Loopback Bolt round-trip and autocommit. Two temporary local stores; "
                      "one request at a time; alternating binary order; workloads run sequentially. "
                      "Nearest-rank p95. One synthetic edge per case; shared host; "
                      "no concurrency or capacity claims.",
            "warmup_per_binary_per_case": warmup, "samples_per_binary_per_case": samples,
            "results": results}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--samples", type=int, default=100)
    parser.add_argument("--warmup", type=int, default=20)
    args = parser.parse_args()
    if not 1 <= args.samples <= 1000 or not 0 <= args.warmup <= 1000:
        parser.error("samples must be 1..1000 and warmup 0..1000")
    binaries = {"baseline": args.baseline.resolve(), "candidate": args.candidate.resolve()}
    for path in binaries.values():
        if not path.is_file() or not os.access(path, os.X_OK):
            parser.error("both binaries must be executable files")
    print(json.dumps(run(binaries, args.samples, args.warmup), indent=2))
