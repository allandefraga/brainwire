"""Multi-source lake workload against a protected server, end to end.

Simulates a platform that keeps an operational graph current from a layered
DuckLake: a `clean` schema with companies, people and support tickets, each row
carrying tenant ACL columns. Three node tables and two foreign-key edge types
are synchronized with examples/python/ducklake_sync.py, several tables at once
with several worker processes each. While the change cycles run (updates,
moves between companies, ACL changes, ticket closures, deletes and inserts),
tenant-restricted readers query the graph and check they never see the other
tenant's rows. Finally the graph is compared with the lake row by row, ACL
visibility is counted per tenant and the store is audited offline.

    python3 crates/server/tests/lake_workload.py --binary target/debug/brainwire --scale 10000

Needs duckdb with installed ducklake/sqlite extensions and the neo4j driver.
Everything is synthetic and temporary; the server is always stopped.
"""
import argparse
import hashlib
import json
import os
import random
import secrets
import socket
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import duckdb
from neo4j import GraphDatabase

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))
from examples.python.ducklake_sync import Mapping, plain, sync  # noqa: E402

ACL = {"allow": "acl_allow", "connectorIds": "acl_connector_ids", "sourceTable": "acl_source_table"}
NODES = {
    "Company": Mapping.from_json({"table": "clean.companies", "label": "Company", "id": "external_id",
                                  "properties": ["name", "segment", "revenue", "updated_at"], "acl": ACL}),
    "Person": Mapping.from_json({"table": "clean.people", "label": "Person", "id": "external_id",
                                 "properties": ["name", "email", "company_id", "updated_at"], "acl": ACL}),
    "Ticket": Mapping.from_json({"table": "clean.tickets", "label": "Ticket", "id": "external_id",
                                 "properties": ["person_id", "status", "priority", "opened_at"], "acl": ACL}),
}
EDGES = {
    "WORKS_AT": Mapping.from_json({"table": "clean.people", "type": "WORKS_AT",
                                   "source": {"label": "Person", "column": "external_id"},
                                   "target": {"label": "Company", "column": "company_id"}}),
    "OPENED": Mapping.from_json({"table": "clean.tickets", "type": "OPENED",
                                 "source": {"label": "Person", "column": "person_id"},
                                 "target": {"label": "Ticket", "column": "external_id"}}),
}
ONTOLOGY = {"version": "1", "nodes": [
    {"label": "Company", "properties": ["name", "segment", "revenue", "updated_at"], "indexed_fields": ["segment"]},
    {"label": "Person", "properties": ["name", "email", "company_id", "updated_at"], "indexed_fields": ["email"]},
    {"label": "Ticket", "properties": ["person_id", "status", "priority", "opened_at"],
     "indexed_fields": ["status"]},
    {"label": "ImportCheckpoint", "properties": ["snapshot", "lease"], "indexed_fields": []}],
    "edges": [{"type": "WORKS_AT", "from": {"label": "Person"}, "to": {"label": "Company"}},
              {"type": "OPENED", "from": {"label": "Person"}, "to": {"label": "Ticket"}}]}
TENANTS = ("a", "b")


def acl_sql(tenant_expr, table):
    return (f"['group:tenant-' || {tenant_expr}], ['crm-' || {tenant_expr}], 'clean/{table}'")


def build_lake(writer, scale):
    writer.execute("CREATE SCHEMA w.clean")
    acl_columns = "acl_allow VARCHAR[], acl_connector_ids VARCHAR[], acl_source_table VARCHAR"
    writer.execute(f"CREATE TABLE w.clean.companies(external_id VARCHAR, name VARCHAR, segment VARCHAR, "
                   f"revenue DECIMAL(18,2), updated_at TIMESTAMP, {acl_columns})")
    writer.execute(f"CREATE TABLE w.clean.people(external_id VARCHAR, name VARCHAR, email VARCHAR, "
                   f"company_id VARCHAR, updated_at TIMESTAMP, {acl_columns})")
    writer.execute(f"CREATE TABLE w.clean.tickets(external_id VARCHAR, person_id VARCHAR, status VARCHAR, "
                   f"priority BIGINT, opened_at TIMESTAMP, {acl_columns})")
    # Tenant is encoded in every ID so a reader can recognize a foreign row.
    tenant = "CASE WHEN i % 2 = 0 THEN 'a' ELSE 'b' END"
    writer.execute(f"""INSERT INTO w.clean.companies SELECT {tenant} || '-co-' || i, 'Company ' || i,
        'segment-' || (i % 5), (i * 37 % 100000) / 100.0, TIMESTAMP '2026-01-01' + to_seconds(i),
        {acl_sql(tenant, 'companies')} FROM range({scale}) t(i)""")
    writer.execute(f"""INSERT INTO w.clean.people SELECT {tenant} || '-p-' || i, 'Person ' || i,
        'p' || i || '@example.test', {tenant} || '-co-' || ((i % {scale // 2}) * 2 + i % 2),
        TIMESTAMP '2026-01-01' + to_seconds(i), {acl_sql(tenant, 'people')} FROM range({scale * 3}) t(i)""")
    writer.execute(f"""INSERT INTO w.clean.tickets SELECT {tenant} || '-t-' || i,
        {tenant} || '-p-' || ((i % {scale * 3 // 2}) * 2 + i % 2), 'open', i % 4,
        TIMESTAMP '2026-02-01' + to_seconds(i), {acl_sql(tenant, 'tickets')} FROM range({scale * 6}) t(i)""")


def change_cycle(writer, rng, cycle, scale):
    """One day of source activity, keeping foreign keys consistent."""
    w = "w.clean"
    writer.execute(f"UPDATE {w}.companies SET name = name || ' r{cycle}', revenue = revenue + 1, "
                   f"updated_at = updated_at + INTERVAL 1 DAY WHERE hash(external_id || '{cycle}') % 20 = 0")
    # Move 3% of people to another company of the same tenant.
    writer.execute(f"""UPDATE {w}.people SET company_id = substr(external_id, 1, 1) || '-co-' ||
        ((hash(external_id || '{cycle}') % {scale // 2}) * 2 + CASE WHEN external_id[1] = 'a' THEN 0 ELSE 1 END),
        updated_at = updated_at + INTERVAL 1 DAY WHERE hash(external_id || 'move{cycle}') % 33 = 0""")
    # Share 1% of tenant-a people with tenant-b (ACL change only).
    writer.execute(f"""UPDATE {w}.people SET acl_allow = ['group:tenant-a', 'group:tenant-b']
        WHERE external_id[1] = 'a' AND hash(external_id || 'share{cycle}') % 100 = 0""")
    writer.execute(f"UPDATE {w}.tickets SET status = 'closed' WHERE status = 'open' "
                   f"AND hash(external_id || '{cycle}') % 10 = 0")
    # Delete 1% of people with their tickets, and 2% of other tickets.
    writer.execute(f"""CREATE OR REPLACE TEMP TABLE gone AS SELECT external_id FROM {w}.people
        WHERE hash(external_id || 'gone{cycle}') % 100 = 0""")
    writer.execute(f"DELETE FROM {w}.tickets WHERE person_id IN (SELECT external_id FROM gone) "
                   f"OR hash(external_id || 'drop{cycle}') % 50 = 0")
    writer.execute(f"DELETE FROM {w}.people WHERE external_id IN (SELECT external_id FROM gone)")
    # New people and tickets.
    new = scale // 10
    tenant = "CASE WHEN i % 2 = 0 THEN 'a' ELSE 'b' END"
    writer.execute(f"""INSERT INTO {w}.people SELECT {tenant} || '-p-c{cycle}-' || i, 'New ' || i,
        'new{cycle}-' || i || '@example.test', {tenant} || '-co-' || ((i % {scale // 2}) * 2 + i % 2),
        TIMESTAMP '2026-03-01', {acl_sql(tenant, 'people')} FROM range({new}) t(i)""")
    writer.execute(f"""INSERT INTO {w}.tickets SELECT {tenant} || '-t-c{cycle}-' || i,
        {tenant} || '-p-c{cycle}-' || (i % {new}), 'open', 3, TIMESTAMP '2026-03-01',
        {acl_sql(tenant, 'tickets')} FROM range({new * 2}) t(i)""")


def lake_state(writer):
    state = {}
    for label, mapping in NODES.items():
        columns = ", ".join(column for _, column in mapping.properties)
        rows = writer.execute(f"SELECT external_id, {columns} FROM w.clean.{mapping.table}").fetchall()
        state[label] = {row[0]: tuple(plain(value) for value in row[1:]) for row in rows}
    for edge_type, mapping in EDGES.items():
        source, target = mapping.keys
        state[edge_type] = set(writer.execute(f"SELECT {source}, {target} FROM w.clean.{mapping.table}").fetchall())
    return state


def graph_state(session):
    state = {}
    for label, mapping in NODES.items():
        fields = ", ".join(f"n.{prop} AS {prop}" for prop, _ in mapping.properties)
        state[label] = {r["id"]: tuple(r[prop] for prop, _ in mapping.properties)
                        for r in session.run(f"MATCH (n:{label}) RETURN n.entity_id AS id, {fields}")}
    for edge_type, mapping in EDGES.items():
        state[edge_type] = {(r["s"], r["t"]) for r in session.run(
            f"MATCH (a:{mapping.source_label})-[:{edge_type}]->(b:{mapping.target_label}) "
            "RETURN a.entity_id AS s, b.entity_id AS t")}
    return state


def principal(name, graphs, *, admin=False, visibility=None):
    token = secrets.token_hex(32)
    entry = {"id": name, "token_sha256": hashlib.sha256(token.encode()).hexdigest(),
             "graphs": graphs, "instance_admin": admin}
    if visibility is not None:
        entry["node_visibility"] = visibility
    return entry, (name, token)


class Readers:
    """Tenant readers that query continuously and record leaks and latency."""

    def __init__(self, uri, credentials):
        self.uri, self.credentials = uri, credentials
        self.stop = threading.Event()
        self.latencies, self.leaks, self.errors, self.queries = [], [], [], 0
        self.lock = threading.Lock()

    def run(self, tenant):
        rng = random.Random(tenant)
        own = f"{tenant}-"
        queries = [
            ("MATCH (p:Person)-[:WORKS_AT]->(c:Company) RETURN c.entity_id AS id LIMIT 200", {}),
            ("MATCH (p:Person)-[:OPENED]->(t:Ticket) WHERE t.status = 'open' RETURN t.entity_id AS id LIMIT 200", {}),
            ("MATCH (c:Company) WHERE c.segment = 'segment-1' RETURN c.entity_id AS id LIMIT 200", {}),
        ]
        with GraphDatabase.driver(self.uri, auth=self.credentials[tenant]) as driver:
            while not self.stop.is_set():
                query, params = rng.choice(queries)
                started = time.monotonic()
                try:
                    with driver.session(database="lake") as session:
                        ids = [record["id"] for record in session.run(query, params)]
                except Exception as error:  # recorded, asserted at the end
                    with self.lock:
                        self.errors.append(f"{type(error).__name__}: {error}")
                    continue
                with self.lock:
                    self.latencies.append(time.monotonic() - started)
                    self.queries += 1
                    # Companies and tickets are never shared across tenants.
                    self.leaks.extend(i for i in ids if not i.startswith(own))


def run(binary, scale, workers, cycles, lake_target=None, data_path=None, load=("ducklake", "sqlite"),
        init_sql=None):
    rng = random.Random(11)
    report = {"scale": scale, "workers_per_table": workers}
    with tempfile.TemporaryDirectory(prefix="brainwire-lake-workload-") as temporary:
        root = Path(temporary)
        target = lake_target or f"ducklake:sqlite:{root / 'catalog.sqlite'}"
        writer = duckdb.connect()
        writer.execute("SET enable_progress_bar = false")
        for extension in load:
            writer.execute(f"LOAD {extension}")
        if init_sql:
            writer.execute(init_sql)
        writer.execute(f"ATTACH '{target}' AS w (DATA_PATH '{data_path or str(root / 'lake') + '/'}')")
        build_lake(writer, scale)

        loader, loader_auth = principal("loader", {"lake": "write"}, admin=True)
        policy, credentials = [loader], {}
        for tenant in TENANTS:
            entry, credentials[tenant] = principal(
                f"tenant-{tenant}", {"lake": "read"}, visibility={"principals": [f"group:tenant-{tenant}"]})
            policy.append(entry)
        (root / "policy.json").write_text(json.dumps({"principals": policy}))
        auth_file = root / "loader.json"
        auth_file.write_text(json.dumps({"principal": loader_auth[0], "credentials": loader_auth[1]}))
        os.chmod(root / "policy.json", 0o600)
        os.chmod(auth_file, 0o600)

        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        env = {k: v for k, v in os.environ.items() if not k.startswith("BRAINWIRE_")}
        env.update(BRAINWIRE_SERVER_LISTEN=f"127.0.0.1:{port}", BRAINWIRE_SERVER_DATA_DIR=str(root / "data"),
                   BRAINWIRE_SERVER_DEFAULT_DATABASE="lake", BRAINWIRE_SERVER_AUTH_FILE=str(root / "policy.json"),
                   BRAINWIRE_MEMORY_CACHE="256MiB", BRAINWIRE_MEMORY_MEMTABLES="64MiB",
                   BRAINWIRE_MEMORY_QUERY="512MiB")
        (root / "ontology.json").write_text(json.dumps(ONTOLOGY))
        subprocess.run([str(binary), "db", "create", "lake", "--ontology", str(root / "ontology.json")],
                       env=env, check=True, capture_output=True)
        # Workers read the loader credentials from the same variable as any client.
        os.environ["BRAINWIRE_BOLT_AUTH_FILE"] = str(auth_file)
        uri = f"bolt://127.0.0.1:{port}"
        log = (root / "server.log").open("w")
        process = subprocess.Popen([str(binary), "serve"], env=env, stdout=log, stderr=log)
        readers = Readers(uri, credentials)
        try:
            for _ in range(400):
                if process.poll() is not None:
                    raise RuntimeError((root / "server.log").read_text())
                try:
                    socket.create_connection(("127.0.0.1", port), timeout=0.1).close()
                    break
                except OSError:
                    time.sleep(0.025)

            def sync_all(group, full):
                started = time.monotonic()
                with ThreadPoolExecutor(len(group)) as pool:
                    results = list(pool.map(lambda item: sync(
                        item[1], lake_target=target, uri=uri, database="lake", source_id=item[0],
                        workers=workers, batch_size=500, full=full, load=load, init_sql=init_sql),
                        group.items()))
                return {"seconds": round(time.monotonic() - started, 3),
                        "upserted": sum(r["upserted"] for r in results),
                        "deleted": sum(r["deleted"] for r in results)}

            report["initial_nodes"] = sync_all(NODES, False)
            report["initial_edges"] = sync_all(EDGES, False)
            reader_threads = [threading.Thread(target=readers.run, args=(t,)) for t in TENANTS]
            for thread in reader_threads:
                thread.start()
            report["cycles"] = []
            for cycle in range(cycles):
                change_cycle(writer, rng, cycle, scale)
                report["cycles"].append({"nodes": sync_all(NODES, False), "edges": sync_all(EDGES, False)})
            readers.stop.set()
            for thread in reader_threads:
                thread.join()

            with GraphDatabase.driver(uri, auth=loader_auth) as driver, driver.session(database="lake") as session:
                started = time.monotonic()
                lake, graph = lake_state(writer), graph_state(session)
                for name in lake:
                    assert lake[name] == graph[name], f"{name} differs: {len(lake[name])} lake vs {len(graph[name])}"
                report["compare_seconds"] = round(time.monotonic() - started, 3)
                report["final"] = {name: len(rows) for name, rows in lake.items()}
            for tenant in TENANTS:
                group = f"group:tenant-{tenant}"
                expected = writer.execute("SELECT count(*) FROM w.clean.people WHERE list_contains(acl_allow, ?)",
                                          [group]).fetchone()[0]
                with GraphDatabase.driver(uri, auth=credentials[tenant]) as driver, \
                        driver.session(database="lake") as session:
                    seen = session.run("MATCH (p:Person) RETURN count(*) AS n").single()["n"]
                assert seen == expected, f"tenant {tenant} sees {seen} people, lake grants {expected}"
                report[f"tenant_{tenant}_people_visible"] = seen
            latencies = sorted(readers.latencies)
            report["readers"] = {
                "queries": readers.queries, "errors": len(readers.errors), "leaks": len(readers.leaks),
                "p50_ms": round(statistics.median(latencies) * 1000, 1) if latencies else None,
                "p95_ms": round(latencies[int(len(latencies) * 0.95)] * 1000, 1) if latencies else None}
            assert not readers.leaks, readers.leaks[:5]
            assert not readers.errors, readers.errors[:5]
        finally:
            readers.stop.set()
            process.terminate()
            process.wait(timeout=30)
            log.close()
        audit = json.loads(subprocess.run([str(binary), "db", "check", "lake", "--json"], env=env, check=True,
                                          capture_output=True, text=True).stdout)
        assert audit["status"] == "ok", audit
        report["db_check"] = audit["status"]
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/brainwire")
    parser.add_argument("--scale", type=int, default=2000, help="companies; people = 3x, tickets = 6x")
    parser.add_argument("--workers", type=int, default=4, help="processes per table sync")
    parser.add_argument("--cycles", type=int, default=3)
    parser.add_argument("--lake", help="empty DuckLake ATTACH target to use instead of a temporary SQLite one")
    parser.add_argument("--data-path", help="DATA_PATH for --lake, e.g. s3://bucket/prefix/")
    parser.add_argument("--load", default="ducklake,sqlite", help="installed DuckDB extensions to LOAD")
    parser.add_argument("--init-sql", help="SQL file run before ATTACH (e.g. CREATE SECRET from env)")
    args = parser.parse_args()
    init_sql = Path(args.init_sql).read_text() if args.init_sql else None
    print(json.dumps(run(args.binary.resolve(), args.scale, args.workers, args.cycles, args.lake, args.data_path,
                         tuple(filter(None, args.load.split(","))), init_sql), indent=1))
