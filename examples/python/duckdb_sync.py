"""Synchronize one DuckDB table from a read-only snapshot into a standalone graph.

The SQLite journal contains source rows. Keep it on a private local volume, and
use one source ID and journal per mapping. The mapping must own its target
identities exclusively. Rerun the same command after failure to complete the
frozen snapshot. Schedule another run to read the next source snapshot.
Deletions require --authoritative and apply only to identities from the previous
confirmed snapshot. Delete related edges before deleting their nodes.
"""
import argparse
import contextlib
from dataclasses import dataclass
import fcntl
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import stat
import sys
import uuid
from typing import Optional

from brainwire.client import GraphClient, _acl_options, _entity, _properties, identifier
from examples.python.connection import auth_from_env
from examples.python.ducklake_sync import (
    Mapping, _contract_fingerprint, _driver, _name, _validate_edge_identity, _validate_source_id, plain,
)


_APPLICATION_ID = 0x42574453
_ROW_BYTES = 512 * 1024
_BATCH_BYTES = 1024 * 1024
_BATCH_PLAN_VERSION = 1
_MAX_ROWS = 2_000_000
_MAX_SNAPSHOT_BYTES = 2 * 1024**3
_HEAD = "MATCH (c:ImportCheckpoint {entity_id:$id})"


@dataclass(frozen=True)
class _Pending:
    expected_snapshot: Optional[str]
    target_snapshot: str
    digest: str
    batch_size: int
    claimed: bool = False


def _json(value):
    return json.dumps(value, sort_keys=True, ensure_ascii=False, allow_nan=False, separators=(',', ':'))


@contextlib.contextmanager
def _journal(path):
    directory = path.parent.stat()
    if directory.st_mode & 0o077 or directory.st_uid != os.geteuid():
        raise ValueError('DuckDB sync journal needs a private directory owned by this user (chmod 700)')
    lock_path = path.with_name(path.name + '.lock')
    with _private_file(lock_path) as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise RuntimeError('DuckDB sync journal is locked by another process') from error
        with _private_file(path), contextlib.closing(sqlite3.connect(path)) as db:
            db.execute('PRAGMA synchronous=FULL')
            app_id = db.execute('PRAGMA application_id').fetchone()[0]
            if app_id == 0 and not db.execute('SELECT name FROM sqlite_master LIMIT 1').fetchone():
                db.executescript(f'''BEGIN;
                    PRAGMA application_id={_APPLICATION_ID};
                    CREATE TABLE state (id INTEGER PRIMARY KEY CHECK(id=1), contract TEXT NOT NULL,
                        baseline TEXT, baseline_digest TEXT, target TEXT, target_digest TEXT, batch_size INTEGER,
                        claimed INTEGER NOT NULL DEFAULT 0 CHECK(claimed IN (0,1)));
                    CREATE TABLE baseline (key TEXT PRIMARY KEY, payload TEXT NOT NULL) WITHOUT ROWID;
                    CREATE TABLE pending (key TEXT PRIMARY KEY, payload TEXT NOT NULL) WITHOUT ROWID;
                    COMMIT;''')
            elif app_id != _APPLICATION_ID:
                raise ValueError('invalid DuckDB sync journal')
            yield db


@contextlib.contextmanager
def _private_file(path):
    with os.fdopen(os.open(path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600), 'rb') as file:
        metadata = os.fstat(file.fileno())
        if (not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1 or
                metadata.st_uid != os.geteuid() or metadata.st_mode & 0o077):
            raise ValueError('DuckDB sync journal files must be private regular files (chmod 600)')
        yield file


def _state(db):
    return db.execute('SELECT contract,baseline,baseline_digest,target,target_digest,batch_size,claimed FROM state').fetchone()


def _pending(db):
    state = _state(db)
    return _Pending(state[1], *state[3:6], bool(state[6])) if state and state[3] else None


def _digest(db, table):
    digest = hashlib.sha256()
    for key, payload in db.execute(f'SELECT key,payload FROM {table} ORDER BY key'):
        digest.update((key + '\n' + payload + '\n').encode('utf-8'))
    return digest.hexdigest()


def _valid_snapshot(db, table, token, digest):
    return (isinstance(token, str) and isinstance(digest, str) and token.endswith(':' + digest)
            and _digest(db, table) == digest)


def _stage(db, source, mapping, contract, batch_size, authoritative, allow_empty,
           max_rows=_MAX_ROWS, max_snapshot_bytes=_MAX_SNAPSHOT_BYTES):
    import duckdb

    state = _state(db)
    if state and (state[0] != contract or state[3] is not None):
        raise RuntimeError('DuckDB journal has a different contract or an existing pending snapshot')
    # The base-table scan needs no external files or extension autoloading.
    with db, duckdb.connect(str(source), read_only=True, config={
        'memory_limit': '256MiB', 'threads': 2, 'max_temp_directory_size': '0B',
        'enable_external_access': False,
    }) as reader:
        reader.execute('BEGIN TRANSACTION')
        if reader.execute('SELECT table_type FROM information_schema.tables '
                          'WHERE table_catalog=current_database() AND table_schema=? AND table_name=?',
                          [mapping.schema, mapping.table]).fetchall() != [('BASE TABLE',)]:
            raise ValueError('DuckDB sync requires a base table in the source file')
        cursor = reader.execute(f'SELECT {mapping.columns()} FROM "{mapping.schema}"."{mapping.table}"')
        width = len(mapping.keys)
        count = size = 0
        while rows := cursor.fetchmany(min(batch_size, 64)):
            for values in rows:
                keys = tuple(map(_entity, values[:width]))
                attrs = values[width:]
                props = _properties({name: plain(value) for (name, _), value in zip(mapping.properties, attrs)})
                row = {**dict(zip(mapping.key_names, keys)), **props}
                if mapping.acl:
                    acl = {name: plain(value) for (name, _), value in
                           zip(mapping.acl, attrs[len(mapping.properties):]) if value is not None}
                    _acl_options(mapping.label, keys[0], acl)
                    row['acl'] = acl
                payload = _json(row)
                payload_bytes = len(payload.encode('utf-8'))
                if payload_bytes > _ROW_BYTES:
                    raise ValueError('DuckDB source row exceeds the 512 KiB normalized limit')
                key = _json(keys)
                count += 1
                size += len(key.encode('utf-8')) + payload_bytes
                if count > max_rows or size > max_snapshot_bytes:
                    raise ValueError('DuckDB snapshot exceeds the normalized row or byte limit')
                try:
                    db.execute('INSERT INTO pending VALUES (?,?)', (key, payload))
                except sqlite3.IntegrityError as error:
                    raise ValueError('DuckDB source has a duplicate identity after string conversion') from error
        if authoritative and count == 0 and not allow_empty:
            raise ValueError('empty authoritative snapshot requires --allow-empty-authoritative')
        digest = _digest(db, 'pending')
        pending = _Pending(state[1] if state else None, uuid.uuid4().hex + ':' + digest, digest, batch_size)
        db.execute('INSERT OR IGNORE INTO state (id,contract) VALUES (1,?)', (contract,))
        db.execute('UPDATE state SET target=?,target_digest=?,batch_size=?',
                   (pending.target_snapshot, pending.digest, batch_size))
    return pending


def _promote(db, pending):
    with db:
        db.execute('DELETE FROM baseline')
        db.execute('INSERT INTO baseline SELECT * FROM pending')
        _clear(db)
        db.execute('UPDATE state SET baseline=?,baseline_digest=?', (pending.target_snapshot, pending.digest))


def _clear(db):
    db.execute('DELETE FROM pending')
    db.execute('UPDATE state SET target=NULL,target_digest=NULL,batch_size=NULL,claimed=0')


def _upserts(db):
    for (payload,) in db.execute('SELECT p.payload FROM pending p LEFT JOIN baseline b USING (key) '
                                 'WHERE b.key IS NULL OR p.payload<>b.payload ORDER BY p.key'):
        yield json.loads(payload)


def _deletes(db):
    for (key,) in db.execute('SELECT b.key FROM baseline b LEFT JOIN pending p USING (key) '
                             'WHERE p.key IS NULL ORDER BY b.key'):
        yield tuple(json.loads(key))


def _head(tx, source_id):
    row = tx.run(_HEAD + ' RETURN c.snapshot AS snapshot,c.contract_fingerprint AS contract, '
                 'c.lease AS lease,c.batch AS batch', id=source_id).single()
    return None if row is None else row.data()


def _guard(tx, source_id, expected):
    result = tx.run('CALL db.assertNodeState($label,$entity_id,$expected)',
                    label='ImportCheckpoint', entity_id=source_id, expected=expected).single(strict=True)
    if result['matched'] is not True:
        raise RuntimeError('DuckDB checkpoint assertion was not acknowledged')


def _expected(pending, contract, batch):
    return {'snapshot': pending.expected_snapshot, 'contract_fingerprint': contract,
            'lease': pending.target_snapshot, 'batch': batch}


def _position(head, pending, contract):
    if head is None:
        if pending.expected_snapshot is None and not pending.claimed:
            return None
    elif head['contract'] == contract and head['snapshot'] == pending.expected_snapshot:
        if head['lease'] is None and head['batch'] is None and not pending.claimed:
            return None
        if head['lease'] == pending.target_snapshot and type(head['batch']) is int and head['batch'] >= 0:
            return head['batch']
    raise RuntimeError('DuckDB journal and graph checkpoint disagree; reconcile before syncing')


class _Batches(GraphClient):
    def __init__(self, driver, database, source_id, contract, pending, position):
        super().__init__(driver, database, batch_size=pending.batch_size, batch_bytes=_BATCH_BYTES)
        self.source_id = source_id
        self.contract = contract
        self.pending = pending
        self.position = position
        self.number = 0

    def _write(self, operation):
        self.number += 1
        if self.number <= self.position:
            return 0

        def apply(tx):
            _guard(tx, self.source_id, _expected(self.pending, self.contract, self.number - 1))
            result = operation(tx)
            tx.run(_HEAD + ' SET c.batch=$batch', id=self.source_id, batch=self.number).consume()
            return result

        return super()._write(apply)

    def remove_nodes(self, label, keys):
        # Non-detaching DELETE preserves relationships owned by another mapping.
        query = f'UNWIND $ids AS id MATCH (n:{label} {{entity_id:id}}) DELETE n RETURN count(*) AS deleted'
        total, ids, size = 0, [], 0
        for (key,) in keys:
            encoded = len(_json(key).encode('utf-8'))
            if ids and (len(ids) == self.batch_size or size + encoded > self.batch_bytes):
                total += self._write(lambda tx: tx.run(query, ids=ids).single(strict=True)['deleted'])
                ids, size = [], 0
            ids.append(key)
            size += encoded
        if ids:
            total += self._write(lambda tx: tx.run(query, ids=ids).single(strict=True)['deleted'])
        return total


def _sync_graph(driver, database, source_id, contract, pending, journal, mapping, authoritative, resumed):
    with driver.session(database=database) as session, session.begin_transaction() as tx:
        head = _head(tx, source_id)
        if head == {'snapshot': pending.target_snapshot, 'contract': contract, 'lease': None, 'batch': None}:
            _promote(journal, pending)
            return {'upserted': 0, 'deleted': 0, 'resumed': resumed, 'reconciled': True}
        position = _position(head, pending, contract)
        if position is None:
            expected = _expected(pending, contract, 0)
            if head is None:
                tx.run('CREATE (:ImportCheckpoint {entity_id:$id,contract_fingerprint:$contract,'
                       'lease:$lease,batch:0})', id=source_id, contract=contract, lease=expected['lease']).consume()
            else:
                _guard(tx, source_id, {**expected, 'lease': None, 'batch': None})
                tx.run(_HEAD + ' SET c.lease=$lease,c.batch=0', id=source_id, lease=expected['lease']).consume()
            _guard(tx, source_id, expected)
            tx.commit()
            position = 0

    if not pending.claimed:
        with journal:
            journal.execute('UPDATE state SET claimed=1')
    client = _Batches(driver, database, source_id, contract, pending, position)
    if mapping.label:
        upserted = client.load_nodes(mapping.label, _upserts(journal), with_acl=bool(mapping.acl))
        deleted = client.remove_nodes(mapping.label, _deletes(journal)) if authoritative else 0
    else:
        signature = (mapping.source_label, mapping.type, mapping.target_label)
        upserted = client.load_edges(*signature, _upserts(journal))
        deleted = (client.delete_edges(*signature, ({'source_id': s, 'target_id': t} for s, t in _deletes(journal)))
                   if authoritative else 0)
    if client.number < position:
        raise RuntimeError('DuckDB checkpoint is ahead of the frozen batch plan')
    with driver.session(database=database) as session, session.begin_transaction() as tx:
        _guard(tx, source_id, _expected(pending, contract, client.number))
        tx.run(_HEAD + ' SET c.snapshot=$target,c.lease=NULL,c.batch=NULL',
               id=source_id, target=pending.target_snapshot).consume()
        tx.commit()
    _promote(journal, pending)
    return {'upserted': upserted, 'deleted': deleted, 'resumed': resumed, 'reconciled': False}


def sync(mapping, *, source, journal, uri, database, source_id, source_identity,
         batch_size=250, exclusive=False, authoritative=False, allow_empty_authoritative=False,
         max_rows=_MAX_ROWS, max_snapshot_bytes=_MAX_SNAPSHOT_BYTES):
    """Apply changes from one DuckDB file, resuming frozen work before staging new data."""
    if exclusive is not True:
        raise ValueError('--exclusive must confirm this mapping owns all its target identities')
    _validate_source_id(source_id)
    if type(batch_size) is not int or not 1 <= batch_size <= 1000:
        raise ValueError('batch_size must be 1..1000')
    if type(max_rows) is not int or not 1 <= max_rows <= _MAX_ROWS:
        raise ValueError(f'max_rows must be 1..{_MAX_ROWS}')
    if type(max_snapshot_bytes) is not int or not 1 <= max_snapshot_bytes <= _MAX_SNAPSHOT_BYTES:
        raise ValueError(f'max_snapshot_bytes must be 1..{_MAX_SNAPSHOT_BYTES}')
    if type(authoritative) is not bool or type(allow_empty_authoritative) is not bool:
        raise ValueError('authoritative options must be booleans')
    if allow_empty_authoritative and not authoritative:
        raise ValueError('--allow-empty-authoritative requires --authoritative')
    for name in (mapping.schema, mapping.table, *mapping.keys, *(column for _, column in mapping.properties + mapping.acl)):
        _name(name, 'source identifier')
    for name in (mapping.label, mapping.type, mapping.source_label, mapping.target_label):
        if name is not None:
            identifier(name)
            if name == 'ImportCheckpoint':
                raise ValueError('ImportCheckpoint is reserved for synchronization checkpoints')
    if mapping.label and 'acl' in dict(mapping.properties):
        raise ValueError('acl is reserved for dedicated ACL columns')
    journal, source = Path(journal), Path(source).resolve()
    journal = journal.parent.resolve() / journal.name
    source_files = (source, source.with_name(source.name + '.wal'))
    for suffix in ('', '.lock', '-journal', '-wal', '-shm'):
        control_file = journal.with_name(journal.name + suffix)
        for source_file in source_files:
            if control_file == source_file or (control_file.exists() and source_file.exists()
                                               and control_file.samefile(source_file)):
                raise ValueError('DuckDB source, its WAL and journal files must be separate')
    auth = auth_from_env()
    base = _contract_fingerprint(mapping, source_identity, database, auth[0] if auth else None, True)
    contract = hashlib.sha256(_json(['duckdb-snapshot', _BATCH_PLAN_VERSION, base, source_id,
                                    str(source), uri, authoritative]).encode()).hexdigest()
    with _journal(journal) as db, _driver(uri, auth) as driver:
        state = _state(db)
        pending = _pending(db)
        if state and (state[0] != contract or (state[1] is not None and
                                                not _valid_snapshot(db, 'baseline', state[1], state[2]))):
            raise RuntimeError('DuckDB journal has a different contract or damaged baseline')
        _validate_edge_identity(driver, database, mapping)
        if pending:
            if batch_size != pending.batch_size or not _valid_snapshot(db, 'pending', pending.target_snapshot,
                                                                        pending.digest):
                raise RuntimeError('DuckDB frozen snapshot or batch size changed; reconcile before syncing')
            return _sync_graph(driver, database, source_id, contract, pending, db, mapping, authoritative, True)
        check = _Pending(state[1] if state else None, uuid.uuid4().hex, '', batch_size)
        with driver.session(database=database) as session, session.begin_transaction() as tx:
            _position(_head(tx, source_id), check, contract)
        pending = _stage(db, source, mapping, contract, batch_size, authoritative, allow_empty_authoritative,
                         max_rows, max_snapshot_bytes)
        if state and state[1] is not None and state[2] == pending.digest:
            with db:
                _clear(db)
            return {'upserted': 0, 'deleted': 0, 'resumed': False, 'reconciled': False}
        return _sync_graph(driver, database, source_id, contract, pending, db, mapping, authoritative, False)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mapping', type=Path, help='JSON mapping file for one table')
    parser.add_argument('--source', type=Path, required=True, help='DuckDB database file')
    parser.add_argument('--journal', type=Path, required=True, help='private SQLite state file to preserve between runs')
    parser.add_argument('--database', required=True, help='target graph')
    parser.add_argument('--source-id', required=True, help='stable checkpoint name for this mapping')
    parser.add_argument('--source-identity', required=True, help='stable identifier for this source generation')
    parser.add_argument('--uri', default='bolt://127.0.0.1:7687')
    parser.add_argument('--batch-size', type=int, default=250)
    parser.add_argument('--max-rows', type=int, default=_MAX_ROWS, help='maximum rows per source snapshot')
    parser.add_argument('--max-snapshot-bytes', type=int, default=_MAX_SNAPSHOT_BYTES,
                        help='maximum normalized key and row bytes per source snapshot')
    parser.add_argument('--exclusive', action='store_true', help='confirm this mapping owns all target identities')
    parser.add_argument('--authoritative', action='store_true', help='delete previously owned IDs absent from the table')
    parser.add_argument('--allow-empty-authoritative', action='store_true', help='allow an empty authoritative snapshot')
    args = parser.parse_args(argv)
    mapping = Mapping.from_json(json.loads(args.mapping.read_text(encoding='utf-8')))
    options = vars(args)
    del options['mapping']
    print(_json(sync(mapping, **options)))
    return 0


if __name__ == '__main__':
    sys.exit(main())
