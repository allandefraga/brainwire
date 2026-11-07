"""Real Bolt tests for resumable JSONL, including rollback and lost acknowledgements.

uv run --no-project --with neo4j==5.28.6 python this_file.py
All data and server processes are disposable and synthetic.
"""
import argparse
import hashlib
import importlib.util
import json
import os
import platform
import random
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time

from neo4j import GraphDatabase, __version__ as driver_version

ROOT = Path(__file__).resolve().parents[3]
BINARY = ROOT / "target/debug/brainwire"
spec = importlib.util.spec_from_file_location("resumable_customers", ROOT / "examples/python/resumable_customers.py")
importer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(importer)


class InjectedFailure(RuntimeError):
    pass


class InterruptedDriver:
    """Inject at the real transactional boundary; graph operations use actual Bolt."""
    def __init__(self, real, after_commit):
        self.real = real
        self.after_commit = after_commit

    def session(self, **kwargs):
        real_session = self.real.session(**kwargs)
        after_commit = self.after_commit

        class Session:
            def __enter__(self):
                real_session.__enter__()
                return self

            def __exit__(self, *args):
                return real_session.__exit__(*args)

            def execute_write(self, callback):
                with real_session.begin_transaction() as tx:
                    class Transaction:
                        def run(self, query, **params):
                            if not after_commit and query == importer.ADVANCE:
                                raise InjectedFailure("after customer writes, before cursor update")
                            return tx.run(query, **params)
                    result = callback(Transaction())
                    if after_commit:
                        tx.commit()
                    # Before commit: __exit__ rolls back; after commit: lost receipt.
                    raise InjectedFailure("before client receipt")

        return Session()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=BINARY)
    parser.add_argument("--rows", type=int, default=10000)
    parser.add_argument("--dimensions", type=int, default=16)
    parser.add_argument("--varied-vectors", action="store_true")
    args = parser.parse_args()
    binary = args.binary.resolve()
    total = args.rows
    dimensions = args.dimensions
    if total <= 250 or dimensions <= 0:
        parser.error("rows must exceed 250 and dimensions must be positive")
    result = {"host": platform.platform(), "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
              "vector_values": "seeded_random" if args.varied_vectors else "repeated_per_node",
              "driver": driver_version, "profile": "release" if binary.parent.name == "release" else "debug", "rows": total,
              "embedding_dimensions": dimensions, "batch_size": 250}
    with tempfile.TemporaryDirectory(prefix="brainwire-resumable-") as tmp:
        path = Path(tmp)
        data = path / "data"
        data.mkdir()
        source = path / "source.jsonl"
        first_batch_offset = 0
        first_row = last_row = None
        with source.open("w") as stream:
            for i in range(total):
                generator = random.Random(i)
                vector = [generator.random() for _ in range(dimensions)] if args.varied_vectors else [i % 13 / 13] * dimensions
                row = {"entity_id": f"customer-{i}", "email": f"customer-{i}@example.test",
                       "active": i % 2 == 0, "tags": ["synthetic", f"group-{i % 10}"],
                       "embedding": vector}
                line = json.dumps(row) + "\n"
                stream.write(line)
                if i < 250:
                    first_batch_offset += len(line.encode())
                if i == 0:
                    first_row = row
                last_row = row
        first_rows, first_batch_offset = importer.read_batch(source, 0, source.stat().st_size, 250, 1 << 20, 64 << 10)
        first_batch_count = len(first_rows)
        result["first_batch_rows"] = first_batch_count
        result["source_bytes"] = source.stat().st_size
        result["input_limits"] = {"rows": 250, "batch_bytes": 1 << 20, "line_bytes": 64 << 10}
        env = {k: v for k, v in os.environ.items() if not k.startswith("BRAINWIRE_")}
        env.update(BRAINWIRE_SERVER_DATA_DIR=str(data), BRAINWIRE_MEMORY_CACHE="64MiB",
                   BRAINWIRE_MEMORY_MEMTABLES="16MiB", BRAINWIRE_SERVER_DEFAULT_DATABASE="customers")
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            address = "%s:%s" % sock.getsockname()
        env["BRAINWIRE_SERVER_LISTEN"] = address
        server = None
        log = open(path / "server.log", "w+")

        def cli(*args):
            completed = subprocess.run([str(binary), *args], cwd=ROOT, env=env,
                                       capture_output=True, text=True, timeout=30)
            if completed.returncode:
                raise RuntimeError(completed.stderr)
            return completed.stdout

        def start():
            nonlocal server
            server = subprocess.Popen([str(binary), "serve"], cwd=ROOT, env=env, stdout=log, stderr=log)
            for _ in range(100):
                if server.poll() is not None:
                    log.seek(0)
                    raise RuntimeError(log.read())
                try:
                    cli("probe", "--address", address, "--timeout-ms", "100")
                    return
                except RuntimeError:
                    time.sleep(.03)
            raise RuntimeError("server not ready")

        def stop():
            nonlocal server
            if server is not None:
                server.kill()
                server.wait(timeout=5)
                server = None

        def read(driver, query, **params):
            with driver.session(database="customers") as session:
                return session.run(query, **params).single(strict=True).data()

        def cursor(driver):
            return read(driver, importer.CHECKPOINT, source_id="synthetic-source")["next_offset"]

        def count(driver):
            return read(driver, "MATCH (c:Customer) RETURN count(*) AS n")["n"]

        def sequence(driver):
            return read(driver, "CALL db.commitState() YIELD sequence RETURN sequence")["sequence"]

        try:
            cli("db", "create", "customers", "--ontology", str(ROOT / "examples/python/resumable_customers_ontology.json"))
            start()
            with GraphDatabase.driver("bolt://" + address, auth=None) as driver:
                try:
                    importer.ingest(InterruptedDriver(driver, False), "customers", source, "synthetic-source")
                    raise AssertionError("failure not injected")
                except InjectedFailure:
                    pass
                assert count(driver) == 0
                assert read(driver, "MATCH (c:Customer {active:true}) RETURN count(*) AS n")["n"] == 0
                with driver.session(database="customers") as session:
                    assert session.run(importer.CHECKPOINT, source_id="synthetic-source").single() is None
                result["before_commit_customer_and_cursor_rollback"] = True
                try:
                    importer.ingest(InterruptedDriver(driver, True), "customers", source, "synthetic-source")
                    raise AssertionError("failure not injected")
                except InjectedFailure:
                    pass
                assert count(driver) == first_batch_count
                assert cursor(driver) == first_batch_offset
                acknowledged = sequence(driver)
                stop()
            start()
            with GraphDatabase.driver("bolt://" + address, auth=None) as driver:
                assert count(driver) == first_batch_count and cursor(driver) == first_batch_offset
                assert sequence(driver) == acknowledged
                result["lost_ack_cursor_survives_sigkill"] = True
                started = time.perf_counter()
                assert importer.ingest(driver, "customers", source, "synthetic-source") == source.stat().st_size
                result["resume_seconds"] = time.perf_counter() - started
                result["remaining_rows_per_second"] = (total - first_batch_count) / result["resume_seconds"]
                assert count(driver) == total
                assert cursor(driver) == source.stat().st_size
                before_noop = sequence(driver)
                assert importer.ingest(driver, "customers", source, "synthetic-source") == source.stat().st_size
                assert sequence(driver) == before_noop
                result["completed_resume_no_new_commit"] = True
                completed_cli = subprocess.run([
                    sys.executable, str(ROOT / "examples/python/resumable_customers.py"), str(source),
                    "--source-id", "synthetic-source", "--database", "customers",
                    "--uri", "bolt://" + address, "--batch-size", "250",
                    "--batch-bytes", "1048576", "--line-bytes", "65536",
                ], capture_output=True, text=True, timeout=30)
                assert completed_cli.returncode == 0, completed_cli.stderr
                assert f"durable next_offset={source.stat().st_size} bytes" in completed_cli.stdout
                assert sequence(driver) == before_noop
                result["cli_completed_resume_no_new_commit"] = True
                assert read(driver, "MATCH (c:Customer {active:true}) RETURN count(*) AS n")["n"] == (total + 1) // 2
                stored = read(driver, "MATCH (c:Customer {entity_id:$id}) RETURN c.email AS email, c.tags AS tags, c.embedding AS embedding", id=f"customer-{total - 1}")
                assert stored["email"] == last_row["email"]
                assert stored["tags"] == last_row["tags"] and stored["embedding"] == last_row["embedding"]
                result["list_vector_and_scalar_index_roundtrip"] = True
                changed = path / "changed.jsonl"
                changed.write_text(json.dumps({**first_row, "email": "changed@example.test"}) + "\n")
                try:
                    importer.ingest(driver, "customers", changed, "synthetic-source")
                    raise AssertionError("changed source accepted")
                except ValueError as error:
                    assert "different SHA256" in str(error)
                assert sequence(driver) == before_noop and count(driver) == total
                result["changed_source_rejected_without_commit"] = True
                result["final_sequence"] = before_noop
                result["final_offset"] = cursor(driver)
                result["source_sha256"] = importer.fingerprint(source)[0]
            stop()
            assert json.loads(cli("db", "check", "customers", "--json"))["status"] == "ok"
            result["offline_graph_and_index_audit"] = True
        finally:
            stop()
            log.close()
    result["temporary_data_removed"] = True
    result["test_server_stopped"] = True
    print(json.dumps(result, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
