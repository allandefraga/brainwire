"""Parallel, incremental DuckLake table -> Brainwire graph synchronization.

One mapping file maps one DuckLake table to a node label (``id`` column) or to
an edge type (``source``/``target`` columns). The first run loads the table at a
pinned snapshot; later runs read ``ducklake_table_changes`` from the snapshot
after the stored cursor up to the target snapshot, collapse every key to its
final state (upsert or delete) and apply only that. Rerunning a range is
idempotent, so a failed run is retried by running again.

Work is split into ``--workers`` processes by ``hash(key) % workers``; each
opens its own read-only DuckLake connection and Bolt driver, so one key is
always written by one process and processes never contend on the same node.
Each batch commits atomically; write conflicts are transient and the Neo4j
driver retries them. The cursor (``ImportCheckpoint {entity_id: source_id}``
with ``snapshot`` and ``lease`` properties, which the ontology must declare)
advances only after every partition has committed.

    uv run --no-project --with duckdb==1.5.5 --with neo4j==5.28.6 \\
      python -m examples.python.ducklake_sync customers.json \\
      --lake ducklake:sqlite:/data/catalog.sqlite --database crm \\
      --source-id crm-customers --workers 10
"""
import argparse
import contextlib
import datetime
import decimal
import json
import os
import re
import sys
import tempfile
import time
import uuid
from concurrent.futures import ProcessPoolExecutor
from dataclasses import dataclass

NAME = r"[A-Za-z_][A-Za-z0-9_]{0,127}"
RESERVED_COLUMNS = {"snapshot_id", "rowid", "change_type", "brainwire_rank"}  # added by the change feed
PRESENT = "change_type IN ('insert', 'update_postimage')"


def _name(value, what):
    if not isinstance(value, str) or not re.fullmatch(NAME, value):
        raise ValueError(f"{what} must be an ASCII identifier")
    return value


@dataclass(frozen=True)
class Mapping:
    schema: str
    table: str
    keys: tuple          # source columns forming the identity, in output order
    key_names: tuple     # graph names for those columns (entity_id or source_id/target_id)
    properties: tuple    # (graph property, source column) pairs
    label: str = None
    type: str = None
    source_label: str = None
    target_label: str = None

    @classmethod
    def from_json(cls, value):
        schema, _, table = value.get("table", "").rpartition(".")
        schema = _name(schema or "main", "table schema")
        table = _name(table, "table")
        props = value.get("properties", [])
        columns = [value.get("id")] + [value.get(end, {}).get("column") for end in ("source", "target")]
        columns += list(props.values()) if isinstance(props, dict) else list(props)
        if RESERVED_COLUMNS.intersection(columns):
            raise ValueError(f"columns named {sorted(RESERVED_COLUMNS)} clash with the DuckLake change feed")
        pairs = tuple(props.items()) if isinstance(props, dict) else tuple((p, p) for p in props)
        for prop, column in pairs:
            _name(prop, "property")
            _name(column, "column")
            if prop in ("entity_id", "source_id", "target_id"):
                raise ValueError(f"{prop} is reserved for the mapped identity")
        if ("label" in value) == ("type" in value):
            raise ValueError("a mapping has either label (nodes) or type (edges)")
        if "label" in value:
            return cls(schema, table, (_name(value["id"], "id column"),), ("entity_id",), pairs,
                       label=_name(value["label"], "label"))
        source, target = value["source"], value["target"]
        return cls(schema, table, (_name(source["column"], "source column"), _name(target["column"], "target column")),
                   ("source_id", "target_id"), pairs, type=_name(value["type"], "type"),
                   source_label=_name(source["label"], "label"), target_label=_name(target["label"], "label"))

    def columns(self):
        keys = [f'CAST("{column}" AS VARCHAR)' for column in self.keys]
        return ", ".join(keys + [f'"{column}"' for _, column in self.properties])

    def key_expr(self):
        return ", ".join(f'"{column}"' for column in self.keys)


@contextlib.contextmanager
def lake(target, *, load=("ducklake", "sqlite"), init_sql=None):
    """Attach a DuckLake catalog read-only as ``lake``.

    ``init_sql`` runs first (for example a CREATE SECRET for object storage);
    keep credentials in the environment or secret providers, not in the file.
    """
    import duckdb
    con = duckdb.connect()
    try:
        for extension in load:
            con.execute(f"LOAD {_name(extension, 'extension')}")
        if init_sql:
            con.execute(init_sql)
        literal = "'" + target.replace("'", "''") + "'"
        con.execute(f"ATTACH {literal} AS lake (READ_ONLY)")
        yield con
    finally:
        con.close()


def latest_snapshot(con):
    return con.execute("SELECT max(snapshot_id) FROM lake.snapshots()").fetchone()[0]


def _changes(mapping, start, end):
    return (f"ducklake_table_changes('lake', '{mapping.schema}', '{mapping.table}', "
            f"{int(start) + 1}, {int(end)})")


def read_partition(con, mapping, start, end, workers, worker, fetch=1000):
    """Yield (key tuple, row or None) for this partition; None means delete.

    ``start=None`` reads the whole table at ``end``; otherwise the changes in
    (start, end] collapsed per key: the last snapshot wins and, within one
    snapshot, a post-image/insert wins over a pre-image/delete of the same key.
    """
    partition = f"hash({mapping.key_expr()}) % {int(workers)} = {int(worker)}"
    table = f'lake."{mapping.schema}"."{mapping.table}"'
    if start is None:
        query = f"SELECT true, {mapping.columns()} FROM {table} AT (VERSION => {int(end)}) WHERE {partition}"
    elif start == end:
        return
    else:
        query = (f"SELECT {PRESENT}, {mapping.columns()} FROM ("
                 f"SELECT *, row_number() OVER (PARTITION BY {mapping.key_expr()} "
                 f"ORDER BY snapshot_id DESC, {PRESENT} DESC) AS brainwire_rank "
                 f"FROM {_changes(mapping, start, end)}) WHERE brainwire_rank = 1 AND {partition}")
    cursor = con.execute(query)
    width = len(mapping.keys)
    while rows := cursor.fetchmany(fetch):
        for present, *values in rows:
            key = tuple(values[:width])
            if not present:
                yield key, None
                continue
            row = dict(zip(mapping.key_names, key))
            row.update((prop, plain(value)) for (prop, _), value in zip(mapping.properties, values[width:]))
            yield key, row


def validate(con, mapping, start, end):
    """Refuse null/duplicate identities at ``end`` and a cursor ahead of the source."""
    if start is not None and start > end:
        raise ValueError(f"graph cursor {start} is ahead of source snapshot {end}")
    table = f'lake."{mapping.schema}"."{mapping.table}" AT (VERSION => {int(end)})'
    nulls = " OR ".join(f'"{column}" IS NULL' for column in mapping.keys)
    if con.execute(f"SELECT count(*) FROM {table} WHERE {nulls}").fetchone()[0]:
        raise ValueError(f"{mapping.table}: null identity at snapshot {end}")
    scope = "" if start is None or start == end else (
        f"WHERE ({mapping.key_expr()}) IN (SELECT ({mapping.key_expr()}) FROM {_changes(mapping, start, end)})")
    duplicate = con.execute(f"SELECT {mapping.key_expr()} FROM {table} {scope} "
                            f"GROUP BY ALL HAVING count(*) > 1 LIMIT 1").fetchone()
    if duplicate:
        raise ValueError(f"{mapping.table}: duplicate identity {duplicate} at snapshot {end}")


def stale_keys(con, mapping, end, keys):
    """Return the given graph keys that no source row has at snapshot ``end``.

    Keys are spooled to a temporary JSON-lines file and anti-joined in DuckDB,
    so the graph key list is not held in Python memory.
    """
    names = [f"k{i}" for i in range(len(mapping.keys))]
    with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False) as spool:
        for key in keys:
            spool.write(json.dumps(dict(zip(names, key))) + "\n")
    try:
        columns = "{" + ", ".join(f"'{name}': 'VARCHAR'" for name in names) + "}"
        source = ", ".join(f'CAST("{column}" AS VARCHAR)' for column in mapping.keys)
        literal = "'" + spool.name.replace("'", "''") + "'"
        return con.execute(
            f"SELECT {', '.join(names)} FROM read_json({literal}, format = 'newline_delimited', columns = {columns}) "
            f'EXCEPT SELECT {source} FROM lake."{mapping.schema}"."{mapping.table}" AT (VERSION => {int(end)})'
        ).fetchall()
    finally:
        os.unlink(spool.name)


def plain(value):
    """Convert DuckDB Python values to the Bolt subset Brainwire stores."""
    if value is None or isinstance(value, (bool, int, float, str)):
        return value
    if isinstance(value, decimal.Decimal):
        return float(value)  # ponytail: decimals lose precision as float; map to str if exact cents matter
    if isinstance(value, (datetime.date, datetime.time)):
        return value.isoformat()  # datetime is a date subclass
    if isinstance(value, uuid.UUID):
        return str(value)
    if isinstance(value, (list, tuple)):
        return [plain(item) for item in value]
    if isinstance(value, dict):
        return {str(key): plain(item) for key, item in value.items()}
    raise ValueError(f"unsupported source value type {type(value).__name__}")


def _driver(uri):
    from neo4j import GraphDatabase
    from examples.python.connection import auth_from_env
    return GraphDatabase.driver(uri, auth=auth_from_env())


def _apply(client, mapping, upserts, deletes):
    if mapping.label:
        return (client.load_nodes(mapping.label, upserts) if upserts else 0,
                client.delete_nodes(mapping.label, [key for (key,) in deletes]) if deletes else 0)
    args = (mapping.source_label, mapping.type, mapping.target_label)
    deletes = [{"source_id": s, "target_id": t} for s, t in deletes]
    return (client.load_edges(*args, upserts) if upserts else 0,
            client.delete_edges(*args, deletes) if deletes else 0)


def sync_partition(job):
    """Worker entry point: stream one hash partition into the graph."""
    from examples.python.client import GraphClient
    mapping, start, end = job["mapping"], job["start"], job["end"]
    upserted = deleted = 0
    with lake(job["lake"], load=job["load"], init_sql=job["init_sql"]) as con, _driver(job["uri"]) as driver:
        client = GraphClient(driver, job["database"], batch_size=job["batch_size"])
        upserts, deletes = [], []
        for key, row in read_partition(con, mapping, start, end, job["workers"], job["worker"]):
            (deletes if row is None else upserts).append(key if row is None else row)
            if len(upserts) + len(deletes) >= job["batch_size"] * 8:
                u, d = _apply(client, mapping, upserts, deletes)
                upserted, deleted, upserts, deletes = upserted + u, deleted + d, [], []
        u, d = _apply(client, mapping, upserts, deletes)
    return {"worker": job["worker"], "upserted": upserted + u, "deleted": deleted + d}


def _prune(driver, database, mapping, lake_target, load, init_sql, end, batch_size):
    from examples.python.client import GraphClient
    if mapping.label:
        query = f"MATCH (n:{mapping.label}) RETURN n.entity_id"
    else:
        query = (f"MATCH (a:{mapping.source_label})-[:{mapping.type}]->(b:{mapping.target_label}) "
                 "RETURN a.entity_id, b.entity_id")
    with driver.session(database=database) as session, lake(lake_target, load=load, init_sql=init_sql) as con:
        stale = stale_keys(con, mapping, end, (tuple(record.values()) for record in session.run(query)))
    return _apply(GraphClient(driver, database, batch_size=batch_size), mapping, [], stale)[1]


def _cursor(tx, source_id):
    rows = tx.run("MATCH (s:ImportCheckpoint {entity_id:$id}) RETURN s.snapshot AS snapshot, s.lease AS lease",
                  id=source_id).data()
    return rows[0] if rows else {"snapshot": None, "lease": None}


def claim(session, source_id, run_id, break_lease=False):
    """Take the source lease in one transaction; return the stored snapshot."""
    def work(tx):
        state = _cursor(tx, source_id)
        if state["lease"] not in (None, run_id) and not break_lease:
            raise RuntimeError(f"source {source_id!r} is leased by run {state['lease']}; "
                               "wait for it, or pass --break-lease if that run is dead")
        tx.run("MERGE (s:ImportCheckpoint {entity_id:$id}) SET s.lease = $run", id=source_id, run=run_id)
        return state["snapshot"]
    return session.execute_write(work)


def release(session, source_id, run_id, start, end=None):
    """Drop the lease and, when ``end`` is given, advance the cursor from ``start``."""
    def work(tx):
        state = _cursor(tx, source_id)
        if state["lease"] != run_id or state["snapshot"] != start:
            raise RuntimeError(f"source {source_id!r} cursor/lease changed during the run; rerun to reconcile")
        tx.run("MATCH (s:ImportCheckpoint {entity_id:$id}) SET s.lease = null, s.snapshot = $snapshot",
               id=source_id, snapshot=start if end is None else end)
    session.execute_write(work)


def sync(mapping, *, lake_target, uri, database, source_id, workers=10, batch_size=500,
         to_snapshot=None, full=False, prune=False, break_lease=False, load=("ducklake", "sqlite"),
         init_sql=None):
    """Synchronize one mapped table; return a summary dict.

    ``prune`` (only with ``full``) then deletes graph keys of this label/type
    that the source snapshot no longer has, for a reconciliation when change
    history is unavailable. It lists every graph key in one query, bounded by
    the server's ``memory.query``.
    """
    if type(workers) is not int or not 1 <= workers <= 64:
        raise ValueError("workers must be 1..64")
    if prune and not full:
        raise ValueError("prune requires full")
    started = time.monotonic()
    run_id = uuid.uuid4().hex
    with _driver(uri) as driver:
        def session():  # a fresh session per step: a long run can outlive an idle connection
            return driver.session(database=database)
        with session() as s:
            stored = claim(s, source_id, run_id, break_lease)
        try:
            with lake(lake_target, load=load, init_sql=init_sql) as con:
                end = latest_snapshot(con) if to_snapshot is None else to_snapshot
                start = None if full else stored
                if stored is not None and end < stored:
                    raise ValueError(f"target snapshot {end} is older than the graph cursor {stored}")
                validate(con, mapping, start, end)
            jobs = [{"mapping": mapping, "start": start, "end": end, "workers": workers, "worker": k,
                     "lake": lake_target, "load": tuple(load), "init_sql": init_sql, "uri": uri,
                     "database": database, "batch_size": batch_size} for k in range(workers)]
            with ProcessPoolExecutor(workers) as pool:
                parts = list(pool.map(sync_partition, jobs))
            pruned = _prune(driver, database, mapping, lake_target, load, init_sql, end, batch_size) if prune else 0
        except BaseException as error:
            try:
                with session() as s:
                    release(s, source_id, run_id, stored)
            except Exception as release_error:  # keep the original failure visible
                error.add_note(f"lease not released ({release_error}); rerun with --break-lease")
            raise
        with session() as s:
            release(s, source_id, run_id, stored, end)
    return {"source_id": source_id, "from": start, "to": end, "mode": "full" if start is None else "changes",
            "workers": workers, "upserted": sum(p["upserted"] for p in parts),
            "deleted": sum(p["deleted"] for p in parts), "pruned": pruned, "seconds": round(time.monotonic() - started, 3)}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("mapping", help="JSON mapping file for one table")
    parser.add_argument("--lake", required=True, help="DuckLake ATTACH target, e.g. ducklake:sqlite:/abs/catalog.sqlite")
    parser.add_argument("--database", required=True)
    parser.add_argument("--source-id", required=True, help="stable cursor name for this table+mapping")
    parser.add_argument("--uri", default="bolt://127.0.0.1:7687")
    parser.add_argument("--workers", type=int, default=10)
    parser.add_argument("--batch-size", type=int, default=500)
    parser.add_argument("--to-snapshot", type=int, help="default: latest source snapshot")
    parser.add_argument("--full", action="store_true", help="reload the whole table (does not delete absent keys)")
    parser.add_argument("--prune", action="store_true", help="with --full: delete graph keys absent from the source")
    parser.add_argument("--break-lease", action="store_true", help="take over a lease left by a dead run")
    parser.add_argument("--load", default="ducklake,sqlite", help="DuckDB extensions to LOAD (already installed)")
    parser.add_argument("--init-sql", help="SQL file run before ATTACH, e.g. CREATE SECRET for object storage")
    args = parser.parse_args(argv)
    with open(args.mapping) as stream:
        mapping = Mapping.from_json(json.load(stream))
    init_sql = open(args.init_sql).read() if args.init_sql else None
    summary = sync(mapping, lake_target=args.lake, uri=args.uri, database=args.database,
                   source_id=args.source_id, workers=args.workers, batch_size=args.batch_size,
                   to_snapshot=args.to_snapshot, full=args.full, prune=args.prune, break_lease=args.break_lease,
                   load=tuple(filter(None, args.load.split(","))), init_sql=init_sql)
    print(json.dumps(summary))
    return 0


if __name__ == "__main__":
    sys.exit(main())
