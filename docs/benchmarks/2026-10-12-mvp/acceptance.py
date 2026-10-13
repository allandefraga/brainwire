"""Disposable DuckDB -> immutable snapshot -> protected Bolt -> restart acceptance.

Run with optional duckdb==1.5.5 and neo4j==6.3.0. Synthetic data only;
no external source, credentials or existing server is touched.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time

import duckdb
from neo4j import GraphDatabase, __version__ as driver_version
from neo4j.exceptions import ClientError

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))
from examples.client.export_snapshot import export_snapshot
from examples.python.create_service_tokens import create
from examples.client.generate_fixture import generate
from examples.client.ingest_snapshot import identity, ingest_snapshot


def run(binary, tickets, captures, ducklake=False):
    report = dict(synthetic=True, duckdb=duckdb.__version__, driver=driver_version,
                  binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
                  limits=dict(encoded_message_mib=8, decoded_message_mib=16, io_timeout_ms=300000,
                              cache_mib=64, memtables_mib=16, query_mib=64))
    captures.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="brainwire-mvp-bridge-") as folder:
        path = Path(folder).resolve()
        fixture, snapshot = path / "fixture.jsonl", path / "snapshot.jsonl"
        report["fixture"] = generate(fixture, tickets, 2, 1024)
        if ducklake:
            from examples.client.export_ducklake_snapshot import export_ducklake_snapshot, sql_literal
            catalog, data = path / "metadata.sqlite", path / "lake-data"
            data.mkdir()
            with duckdb.connect(config={"autoinstall_known_extensions": "false",
                                        "autoload_known_extensions": "false"}) as connection:
                connection.execute("LOAD ducklake")
                connection.execute("LOAD sqlite")
                connection.execute(f"ATTACH {sql_literal('ducklake:sqlite:' + str(catalog))} AS lake (DATA_PATH {sql_literal(str(data) + '/')})")
                connection.execute("CALL lake.set_option('data_inlining_row_limit', 0)")
                connection.execute("CREATE TABLE lake.source AS SELECT * FROM read_json_auto(?)", [str(fixture)])
                connection.execute("UPDATE lake.source SET subject=NULL WHERE record_type='ticket' AND tenant_id='tenant-0' AND external_id='0'")
                version = connection.execute("SELECT max(snapshot_id) FROM lake.snapshots()").fetchone()[0]
                connection.execute("UPDATE lake.source SET subject='newer source',status='changed' WHERE record_type='ticket' AND tenant_id='tenant-0' AND external_id='0'")
                connection.execute("DELETE FROM lake.source WHERE record_type='ticket' AND tenant_id='tenant-1' AND external_id='0'")
                assert connection.execute("SELECT count(*) FROM lake.source").fetchone()[0] == 3 * tickets - 1
            began = time.perf_counter()
            report["export"] = export_ducklake_snapshot(catalog, data, version,
                "SELECT * FROM source_lake.main.source ORDER BY CASE record_type WHEN 'ticket' THEN 0 ELSE 1 END,tenant_id,connector_id,external_id",
                snapshot, fetch_rows=127)
            report["source_mode"] = "version-pinned read-only local SQLite DuckLake"
        else:
            with duckdb.connect(str(path / "source.duckdb")) as connection:
                connection.execute("CREATE TABLE source AS SELECT * FROM read_json_auto(?)", [str(fixture)])
                # Exercise authoritative null/list values through SQL, JSONL and Bolt.
                connection.execute("UPDATE source SET subject=NULL WHERE record_type='ticket' AND tenant_id='tenant-0' AND external_id='0'")
            began = time.perf_counter()
            with duckdb.connect(str(path / "source.duckdb"), read_only=True) as connection:
                report["export"] = export_snapshot(connection,
                    "SELECT * FROM source ORDER BY CASE record_type WHEN 'ticket' THEN 0 ELSE 1 END, rowid",
                    snapshot, fetch_rows=127)
            report["source_mode"] = "read-only DuckDB file"
        report["export_seconds"] = time.perf_counter() - began
        assert report["export"]["tickets"] == tickets
        assert report["export"]["comments"] == 2 * tickets
        assert report["export"]["sha256"] == hashlib.sha256(snapshot.read_bytes()).hexdigest()
        auth_dir = create(path / "auth", "shadow", include_observer=True)
        tokens = {name: json.loads((auth_dir / (name + ".json")).read_text())["credentials"]
                  for name in ["observer", "writer", "reader"]}
        policy, auth = auth_dir / "policy.json", auth_dir / "observer.json"
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            address = "%s:%s" % sock.getsockname()
        env = {k: v for k, v in os.environ.items() if not k.startswith("BRAINWIRE_")}
        env.update(BRAINWIRE_SERVER_DATA_DIR=str(path / "data"),
            BRAINWIRE_SERVER_DEFAULT_DATABASE="shadow", BRAINWIRE_SERVER_LISTEN=address,
            BRAINWIRE_SERVER_AUTH_FILE=str(policy), BRAINWIRE_BOLT_AUTH_FILE=str(auth),
            BRAINWIRE_MEMORY_CACHE="64MiB", BRAINWIRE_MEMORY_MEMTABLES="16MiB",
            BRAINWIRE_MEMORY_QUERY="64MiB", BRAINWIRE_SERVER_MAX_MESSAGE="8MiB",
            BRAINWIRE_SERVER_MAX_DECODED_MESSAGE="16MiB", BRAINWIRE_SERVER_IO_TIMEOUT_MS="300000")
        server = None
        log = (path / "server.log").open("w+")

        def cli(*args):
            return subprocess.run([str(binary), *args], env=env, cwd=ROOT,
                capture_output=True, text=True, timeout=60, check=True).stdout

        def start():
            nonlocal server
            server = subprocess.Popen([str(binary), "serve"], env=env, cwd=ROOT,
                stdout=log, stderr=log)
            for _ in range(200):
                if server.poll() is not None:
                    log.seek(0)
                    raise AssertionError(log.read())
                try:
                    cli("probe", "--address", address, "--timeout-ms", "100")
                    return
                except subprocess.CalledProcessError:
                    time.sleep(.03)
            raise AssertionError("server readiness timeout")

        def stop():
            nonlocal server
            if server is not None:
                if server.poll() is None:
                    server.kill()
                server.wait(timeout=10)
                server = None

        def verify(driver):
            with driver.session(database="shadow") as session:
                for label, expected in [("Ticket", tickets), ("TicketComment", 2 * tickets)]:
                    assert session.run(f"MATCH (n:{label}) RETURN count(*) AS n").single(strict=True)["n"] == expected
                assert session.run("MATCH (t:Ticket)-[:HAS_COMMENT]->(c:TicketComment) RETURN count(*) AS n").single(strict=True)["n"] == 2 * tickets
                for status, expected in report["fixture"]["status_counts"].items():
                    assert session.run("MATCH (t:Ticket {status:$status}) RETURN count(*) AS n", status=status).single(strict=True)["n"] == expected
                row = session.run("MATCH (t:Ticket {entity_id:$id}) RETURN t.subject AS subject,t.tags AS tags",
                    id=identity("tenant-0", "zendesk-main", "ticket", "0")).single(strict=True)
                assert row["subject"] is None and row["tags"] == ["synthetic", "bucket-0"]
                checkpoint = session.run("MATCH (c:ImportCheckpoint {entity_id:'duckdb-fixture-v1'}) RETURN c.next_offset AS offset,c.sha256 AS sha").single(strict=True)
                assert checkpoint["offset"] == report["export"]["bytes"]
                assert checkpoint["sha"] == report["export"]["sha256"]
                sequence = session.run("CALL db.commitState() YIELD sequence RETURN sequence").single(strict=True)["sequence"]
                indexes = [dict(row) for row in session.run("CALL db.indexes() YIELD label,property,state RETURN label,property,state")]
                assert indexes and all(row["state"] == "scalar_equality_ready" for row in indexes)
                return sequence, indexes

        try:
            cli("db", "create", "shadow", "--ontology", str(ROOT / "examples/client/ontology.json"))
            start()
            with GraphDatabase.driver("bolt://" + address, auth=("writer", tokens["writer"])) as driver:
                began = time.perf_counter()
                assert ingest_snapshot(driver, "shadow", snapshot, "duckdb-fixture-v1") == snapshot.stat().st_size
                report["import_seconds"] = time.perf_counter() - began
                report["input_rows_per_second"] = 3 * tickets / report["import_seconds"]
                with driver.session(database="shadow") as session:
                    case = session.run("RETURN toLower('OPEN') AS lower,toUpper('straße') AS upper,toLower(null) AS missing").single(strict=True)
                    assert dict(case) == dict(lower="open", upper="STRASSE", missing=None)
                    identifier = identity("tenant-0", "zendesk-main", "ticket", "0")
                    session.run("MATCH (t:Ticket {entity_id:$id}) SET t.status=toUpper(t.status)", id=identifier).consume()
                    assert session.run("MATCH (t:Ticket {status:'OPEN'}) RETURN count(*) AS n").single(strict=True)["n"] == 1
                    assert session.run("MATCH (t:Ticket {status:'open'}) RETURN count(*) AS n").single(strict=True)["n"] == report["fixture"]["status_counts"]["open"] - 1
                    session.run("MATCH (t:Ticket {entity_id:$id}) SET t.status=toLower(t.status)", id=identifier).consume()
                    assert session.run("MATCH (t:Ticket {status:'OPEN'}) RETURN count(*) AS n").single(strict=True)["n"] == 0
                sequence, report["indexes"] = verify(driver)
                ingest_snapshot(driver, "shadow", snapshot, "duckdb-fixture-v1")
                assert verify(driver)[0] == sequence
            for lang in ["en", "pt-BR"]:
                for view in ["ontology", "memory", "indexes"]:
                    text = cli("shell", "--connect", address, "--graph", "shadow", "--view", view,
                        "--snapshot", "--lang", lang, "--width", "120", "--height", "30")
                    assert "BRAINWIRE" in text
                    (captures / f"{view}-{lang}-120x30.txt").write_text(text)
            with GraphDatabase.driver("bolt://" + address, auth=("reader", tokens["reader"])) as driver:
                verify(driver)
                with driver.session(database="shadow") as session:
                    try:
                        session.run("CALL db.memory() YIELD poolLimitBytes RETURN poolLimitBytes").consume()
                    except ClientError as error:
                        assert error.code == "Neo.ClientError.Security.Forbidden", error.code
                    else:
                        raise AssertionError("reader acquired instance memory telemetry")
            stop()
            start()
            with GraphDatabase.driver("bolt://" + address, auth=("writer", tokens["writer"])) as driver:
                assert verify(driver)[0] == sequence
                ingest_snapshot(driver, "shadow", snapshot, "duckdb-fixture-v1")
                assert verify(driver)[0] == sequence
            stop()
            report["audit"] = json.loads(cli("db", "check", "shadow", "--json"))
            assert report["audit"]["status"] == "ok"
            report["commit_sequence"] = sequence
            report["checks"] = ["SQL export", "null/list roundtrip", "protected import", "indexed counts", "Unicode case functions and indexed SET",
                "final source cursor consistency", "completed replay no writes", "reader telemetry denied",
                "EN/PT connected ontology/memory/indexes", "SIGKILL reopen", "replay after restart", "offline integrity audit"]
            if ducklake:
                report["checks"].append("pinned DuckLake snapshot survives later source UPDATE/DELETE")
        finally:
            stop()
            log.close()
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/brainwire")
    parser.add_argument("--tickets", type=int, default=10000)
    parser.add_argument("--ducklake", action="store_true", help="Test version-pinned SQLite DuckLake; cached ducklake/sqlite extensions required")
    parser.add_argument("--captures", type=Path, default=ROOT / "docs/design/assets/2026-10-12-mvp")
    args = parser.parse_args()
    print(json.dumps(run(args.binary.resolve(), args.tickets, args.captures, args.ducklake), indent=2))
