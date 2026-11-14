"""Run DuckDB initial load, snapshot diff and resume against a private Bolt server."""
import argparse
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
import json
import os
from pathlib import Path
import socket
import sqlite3
import subprocess
import tempfile
from threading import Barrier
import time
from unittest.mock import patch

from examples.python import duckdb_sync
from examples.python.ducklake_sync import Mapping


@contextmanager
def server(binary, root, env, driver):
    with (root / 'server.log').open('ab') as log:
        process = subprocess.Popen([binary, 'serve'], env=env, stdout=log, stderr=log)
        try:
            for attempt in range(100):
                if process.poll() is not None:
                    raise RuntimeError('temporary Brainwire server exited during startup')
                try:
                    driver.verify_connectivity()
                    break
                except Exception:
                    if attempt == 99:
                        raise
                    time.sleep(0.05)
            yield
        finally:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=10)


class SimulatedLostAcknowledgement(Exception):
    pass


def run(binary, rows):
    import duckdb
    from neo4j import GraphDatabase
    from neo4j.exceptions import Neo4jError

    with tempfile.TemporaryDirectory(prefix='brainwire-duckdb-sync-') as tmp:
        root = Path(tmp)
        ontology = root / 'ontology.json'
        ontology.write_text(json.dumps({'version': '1', 'nodes': [
            {'label': 'Item', 'properties': ['value']},
            {'label': 'Keep', 'properties': ['value']},
            {'label': 'Race', 'properties': ['value']},
            {'label': 'Claim', 'properties': ['value']},
            {'label': 'ImportCheckpoint', 'properties': ['snapshot', 'lease', 'contract_fingerprint', 'batch']},
        ], 'edges': [{'type': 'LINK', 'from': {'label': 'Item'}, 'to': {'label': 'Item'}, 'properties': ['weight']}]}))
        with socket.socket() as probe:
            probe.bind(('127.0.0.1', 0))
            address = f'127.0.0.1:{probe.getsockname()[1]}'
        env = {k: v for k, v in os.environ.items() if not k.startswith('BRAINWIRE_')}
        env.update(BRAINWIRE_SERVER_DATA_DIR=str(root / 'data'), BRAINWIRE_SERVER_DEFAULT_DATABASE='sync',
                   BRAINWIRE_SERVER_LISTEN=address, BRAINWIRE_MEMORY_CACHE='64MiB', BRAINWIRE_MEMORY_MEMTABLES='16MiB',
                   BRAINWIRE_MEMORY_QUERY='128MiB')
        subprocess.run([binary, 'db', 'create', 'sync', '--ontology', str(ontology)], env=env, check=True,
                       capture_output=True)
        source = root / 'source.duckdb'
        with duckdb.connect(str(source)) as db:
            db.execute('CREATE TABLE items AS SELECT i AS id, CAST(i AS VARCHAR) AS value FROM range(?) r(i)', [rows])
            db.execute('CREATE TABLE links AS SELECT 0 AS src, 1 AS dst, 5 AS weight')
        uri = f'bolt://{address}'
        nodes = Mapping.from_json({'table': 'items', 'label': 'Item', 'id': 'id', 'properties': ['value']})
        edges = Mapping.from_json({'table': 'links', 'type': 'LINK', 'source': {'label': 'Item', 'column': 'src'},
                                  'target': {'label': 'Item', 'column': 'dst'}, 'properties': ['weight']})
        common = dict(source=source, uri=uri, database='sync', source_identity='synthetic', batch_size=100,
                      exclusive=True)

        def load(mapping, source_id, **overrides):
            return duckdb_sync.sync(mapping, **{**common, 'journal': root / (source_id + '.sqlite'),
                                                'source_id': source_id, 'authoritative': True,
                                                **overrides})

        def command(driver, statement, **params):
            with driver.session(database='sync') as session:
                return session.run(statement, **params).values()

        with GraphDatabase.driver(uri, auth=None) as driver, server(binary, root, env, driver), \
                patch.object(duckdb_sync, 'auth_from_env', return_value=None):
            try:
                load(nodes, 'items', exclusive=False)
            except ValueError as error:
                assert '--exclusive' in str(error), error
            else:
                raise AssertionError('accepted an unconfirmed ownership scope')
            assert command(driver, 'MATCH (n:ImportCheckpoint) RETURN count(n)') == [[0]]
            assert not (root / 'items.sqlite').exists()
            assert load(nodes, 'items')['upserted'] == rows
            assert load(nodes, 'items')['upserted'] == 0
            assert load(edges, 'links')['upserted'] == 1
            with duckdb.connect(str(source)) as db:
                db.execute("UPDATE items SET value='changed' WHERE id>=2; DELETE FROM items WHERE id=1")
                db.execute('UPDATE links SET weight=7')
            assert load(edges, 'links')['upserted'] == 1
            assert command(driver, 'MATCH ()-[r:LINK]->() RETURN r.weight') == [[7]]

            super_write = duckdb_sync.GraphClient._write
            writes = 0

            def lose_reply(client, operation):
                nonlocal writes
                value = super_write(client, operation)
                writes += 1
                if writes == 1:
                    raise SimulatedLostAcknowledgement()
                return value

            try:
                with patch.object(duckdb_sync.GraphClient, '_write', lose_reply):
                    load(nodes, 'items')
            except SimulatedLostAcknowledgement:
                pass
            else:
                raise AssertionError('expected an interrupted batch')
            assert command(driver, 'MATCH (s:ImportCheckpoint {entity_id:"items"}) RETURN s.batch') == [[1]]
            with duckdb.connect(str(source)) as db:
                db.execute("UPDATE items SET value='newest' WHERE id>=2")
            try:
                load(nodes, 'items')
            except Neo4jError as error:
                assert 'still has relationships' in str(error), error
            else:
                raise AssertionError('incident relationship was lost')
            assert command(driver, 'MATCH ()-[r:LINK]->() RETURN count(r)') == [[1]]
            with duckdb.connect(str(source)) as db:
                db.execute('DELETE FROM links')
            assert load(edges, 'links', allow_empty_authoritative=True)['deleted'] == 1
            resumed = load(nodes, 'items')
            assert resumed['resumed'] and resumed['deleted'] == 1, resumed
            assert command(driver, 'MATCH (n:Item) RETURN count(n)') == [[rows - 1]]
            assert command(driver, 'MATCH (n:Item {entity_id:"2"}) RETURN n.value') == [['changed']]
            promote = duckdb_sync._promote
            with patch.object(duckdb_sync, '_promote', side_effect=SimulatedLostAcknowledgement):
                try:
                    load(nodes, 'items')
                except SimulatedLostAcknowledgement:
                    pass
                else:
                    raise AssertionError('expected interrupted after graph finalization')
            assert duckdb_sync._promote is promote
            assert command(driver, 'MATCH (n:Item {entity_id:"2"}) RETURN n.value') == [['newest']]
            with duckdb.connect(str(source)) as db:
                db.execute("UPDATE items SET value='after final commit' WHERE id=2")
            recovered = load(nodes, 'items')
            assert recovered == {'resumed': True, 'reconciled': True, 'upserted': 0, 'deleted': 0}, recovered
            assert command(driver, 'MATCH (n:Item {entity_id:"2"}) RETURN n.value') == [['newest']]
            assert load(nodes, 'items')['upserted'] == 1
            try:
                load(nodes, 'items', journal=root / 'other.sqlite')
            except RuntimeError as error:
                assert 'checkpoint disagree' in str(error), error
            else:
                raise AssertionError('accepted a second journal on the same graph')

            try:
                load(nodes, 'items', source_identity='replacement')
            except RuntimeError as error:
                assert 'different contract' in str(error), error
            else:
                raise AssertionError('accepted a changed source identity')

            non_authoritative = Mapping.from_json({'table': 'items', 'label': 'Keep', 'id': 'id', 'properties': ['value']})
            assert load(non_authoritative, 'keep', authoritative=False)['upserted'] == rows - 1
            with duckdb.connect(str(source)) as db:
                db.execute('DELETE FROM items WHERE id=2')
            assert load(non_authoritative, 'keep', authoritative=False)['deleted'] == 0
            assert command(driver, 'MATCH (n:Keep {entity_id:"2"}) RETURN count(n)') == [[1]]

            stage = duckdb_sync._stage
            both_staged = Barrier(2, timeout=10)

            def synchronized_stage(*args, **kwargs):
                result = stage(*args, **kwargs)
                both_staged.wait()
                return result

            racy = Mapping.from_json({'table': 'items', 'label': 'Race', 'id': 'id', 'properties': ['value']})

            def start(index):
                try:
                    result = load(racy, 'race', journal=root / f'race-{index}.sqlite')
                except Neo4jError as error:
                    assert error.code == 'Neo.TransientError.Transaction.Outdated', error
                    return False
                except RuntimeError as error:
                    assert 'checkpoint disagree' in str(error), error
                    return False
                assert result['upserted'] == rows - 2, result
                return True

            with patch.object(duckdb_sync, '_stage', synchronized_stage), ThreadPoolExecutor(2) as pool:
                contenders = list(pool.map(start, (0, 1)))
            assert sum(contenders) == 1, contenders
            assert command(driver, 'MATCH (n:Race) RETURN count(n)') == [[rows - 2]]

            claim_mapping = Mapping.from_json({'table': 'items', 'label': 'Claim', 'id': 'id', 'properties': ['value']})
            from neo4j import Transaction
            original_commit = Transaction.commit

            def lose_claim_ack(tx):
                original_commit(tx)
                raise SimulatedLostAcknowledgement()

            with patch.object(Transaction, 'commit', lose_claim_ack):
                try:
                    load(claim_mapping, 'claim')
                except SimulatedLostAcknowledgement:
                    pass
                else:
                    raise AssertionError('expected lost claim acknowledgement')
            assert command(driver, 'MATCH (n:Claim) RETURN count(n)') == [[0]]
            assert command(driver, 'MATCH (c:ImportCheckpoint {entity_id:"claim"}) RETURN c.batch') == [[0]]
            claimed = load(claim_mapping, 'claim')
            assert claimed['resumed'] and claimed['upserted'] == rows - 2, claimed

            with sqlite3.connect(root / 'claim.sqlite') as state, sqlite3.connect(root / 'old.sqlite') as old:
                state.backup(old)
            with duckdb.connect(str(source)) as db:
                db.execute("UPDATE items SET value='latest' WHERE id=3")
            assert load(claim_mapping, 'claim')['upserted'] == 1
            (root / 'old.sqlite').chmod(0o600)
            try:
                load(claim_mapping, 'claim', journal=root / 'old.sqlite')
            except RuntimeError as error:
                assert 'checkpoint disagree' in str(error), error
            else:
                raise AssertionError('accepted an older journal against a newer graph')

            with sqlite3.connect(root / 'items.sqlite') as state:
                state.execute("UPDATE baseline SET payload='{}' WHERE key='[\"2\"]'")
                # An edited journal cannot replace the digest anchored by the graph token.
                state.execute('UPDATE state SET baseline_digest=?', (duckdb_sync._digest(state, 'baseline'),))
            try:
                load(nodes, 'items')
            except RuntimeError as error:
                assert 'damaged baseline' in str(error), error
            else:
                raise AssertionError('accepted changed contents of the private journal')
            assert command(driver, 'MATCH (n:Item {entity_id:"2"}) RETURN count(n)') == [[1]]

        subprocess.run([binary, 'db', 'check', 'sync', '--json'], env=env, check=True)
    return {'ok': True, 'rows': rows, 'checks': ['initial', 'no-op', 'edge update', 'batch reply lost',
            'frozen resume', 'safe node deletion', 'finalization lost', 'checkpoint mismatch',
            'source identity mismatch', 'explicit ownership and deletions', 'two-journal race',
            'claim acknowledgement lost', 'older journal refused', 'journal integrity', 'offline audit']}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--rows', type=int, default=1000)
    args = parser.parse_args()
    if not 103 <= args.rows <= 1_000_000:
        parser.error('--rows must be 103..1000000')
    print(json.dumps(run(str(args.binary.resolve(strict=True)), args.rows)))


if __name__ == '__main__':
    main()
