"""Local synthetic Ticket/Comment ingestion, replay and recovery acceptance.

uv run --no-project --with neo4j==5.28.6 python this_file.py --tickets 10000
Always creates disposable storage and stops its own server. No production data.
"""
import argparse
from contextlib import suppress
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time

from neo4j import GraphDatabase, __version__ as driver_version
from neo4j.exceptions import ClientError

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))
from examples.python import resumable_customers as base
from examples.client import ingest_snapshot as adapter
from examples.client.generate_fixture import generate

STATE = 'CALL db.commitState() YIELD sequence RETURN sequence'


def count(session, label):
    return session.run(f'MATCH (n:{label}) RETURN count(*) AS n').single(strict=True)['n']


def verify(session, manifest, source_bytes):
    assert count(session, 'Ticket') == manifest['tickets']
    assert count(session, 'TicketComment') == manifest['comments']
    assert session.run('MATCH (t:Ticket)-[:HAS_COMMENT]->(c:TicketComment) RETURN count(*) AS n').single(strict=True)['n'] == manifest['relationships']
    for status, expected in manifest['status_counts'].items():
        assert session.run('MATCH (t:Ticket {status:$status}) RETURN count(*) AS n', status=status).single(strict=True)['n'] == expected
    for priority, expected in manifest['priority_counts'].items():
        assert session.run('MATCH (t:Ticket {priority:$priority}) RETURN count(*) AS n', priority=priority).single(strict=True)['n'] == expected
    cursor = session.run(base.CHECKPOINT, source_id='fixture').single(strict=True)
    assert cursor['next_offset'] == source_bytes and cursor['contract_id'] == adapter.CONTRACT_ID


def verify_updates(driver, directory):
    """Use a separate graph to keep bulk counts and overwrite expectations exact."""
    database = 'client-validation'
    ticket = {'record_type': 'ticket', 'tenant_id': 'checks', 'connector_id': 'z',
              'external_id': 'a', 'subject': 'initial', 'status': 'open',
              'priority': 'high', 'tags': ['synthetic']}
    other = dict(ticket, external_id='b')
    comment = {'record_type': 'comment', 'tenant_id': 'checks', 'connector_id': 'z',
               'external_id': 'c', 'ticket_id': 'a', 'body': 'initial', 'is_public': True}
    def run(rows, source):
        path = directory / (source + '.jsonl')
        path.write_text(''.join(json.dumps(row) + '\n' for row in rows))
        return adapter.ingest_snapshot(driver, database, path, source)
    run([ticket, other, comment, comment], 'initial')  # duplicate row, one merged edge
    changed = dict(ticket, subject=None, status='solved', priority=None, tags=None)
    run([changed, dict(comment, body=None, is_public=False)], 'next-snapshot')
    with driver.session(database=database) as session:
        assert count(session, 'Ticket') == 2 and count(session, 'TicketComment') == 1
        result = session.run('MATCH (t:Ticket {entity_id:$id}) RETURN t.status AS status, t.subject AS subject, t.priority AS priority, t.tags AS tags',
                             id=adapter.identity('checks', 'z', 'ticket', 'a')).single(strict=True).data()
        assert result == {'status':'solved', 'subject':None, 'priority':None, 'tags':None}, result
        assert session.run("MATCH (t:Ticket {priority:'high'}) RETURN count(*) AS n").single(strict=True)['n'] == 1
        assert session.run("MATCH (c:TicketComment {is_public:true}) RETURN count(*) AS n").single(strict=True)['n'] == 0
        assert session.run("MATCH (c:TicketComment {is_public:false}) RETURN c.body AS body").single(strict=True)['body'] is None
        assert session.run('MATCH (t:Ticket)-[:HAS_COMMENT]->(c:TicketComment) RETURN count(*) AS n').single(strict=True)['n'] == 1
    # Reparenting, absent endpoints and conflicting duplicate parents must not create progress.
    for source, rows in [
        ('reparent', [dict(comment, ticket_id='b')]),
        ('missing-ticket', [dict(comment, external_id='missing', ticket_id='absent')]),
        ('duplicate-parent', [dict(comment, external_id='duplicate'), dict(comment, external_id='duplicate', ticket_id='b')]),
    ]:
        try:
            run(rows, source)
        except ValueError:
            pass
        else:
            raise AssertionError(source + ' was accepted')
        with driver.session(database=database) as session:
            assert session.run(base.CHECKPOINT, source_id=source).single() is None
            assert count(session, 'TicketComment') == 1
            assert session.run('MATCH (t:Ticket)-[:HAS_COMMENT]->(c:TicketComment) RETURN count(*) AS n').single(strict=True)['n'] == 1
    try:
        run([dict(ticket, status='should-not-commit')], 'initial')
    except ValueError:
        pass
    else:
        raise AssertionError('same-source changed content was accepted')
    with driver.session(database=database) as session:
        assert session.run('MATCH (t:Ticket {entity_id:$id}) RETURN t.status AS status',
                           id=adapter.identity('checks', 'z', 'ticket', 'a')).single(strict=True)['status'] == 'solved'
    # Lose the first batch receipt AFTER a real server commit; resume must use its cursor.
    partial = directory / 'partial.jsonl'
    partial.write_text(''.join(json.dumps(row) + '\n' for row in [dict(ticket, external_id='partial'), dict(comment, external_id='partial-c', ticket_id='partial')]))
    class LostAckSession:
        def __init__(self, session):
            self.session = session
        def __enter__(self):
            self.session.__enter__()
            return self
        def __exit__(self, *args):
            return self.session.__exit__(*args)
        def execute_write(self, callback):
            self.session.execute_write(callback)
            raise RuntimeError('receipt lost after committed first batch')
    class LostAckDriver:
        def session(self, **kwargs):
            return LostAckSession(driver.session(**kwargs))
    try:
        adapter.ingest_snapshot(LostAckDriver(), database, partial, 'partial', batch_size=1)
    except RuntimeError as error:
        assert 'receipt lost' in str(error)
    else:
        raise AssertionError('expected lost acknowledgement')
    with driver.session(database=database) as session:
        checkpoint = session.run(base.CHECKPOINT, source_id='partial').single(strict=True)
        assert 0 < checkpoint['next_offset'] < partial.stat().st_size
        assert count(session, 'Ticket') == 3 and count(session, 'TicketComment') == 1
    adapter.ingest_snapshot(driver, database, partial, 'partial', batch_size=1)
    with driver.session(database=database) as session:
        assert count(session, 'Ticket') == 3 and count(session, 'TicketComment') == 2
        assert session.run('MATCH (t:Ticket)-[:HAS_COMMENT]->(c:TicketComment) RETURN count(*) AS n').single(strict=True)['n'] == 2
    # New snapshot IDs update/upsert; absence does not delete other records.
    with driver.session(database=database) as session:
        assert session.run('MATCH (t:Ticket {entity_id:$id}) RETURN t.status AS status',
                           id=adapter.identity('checks', 'z', 'ticket', 'b')).single(strict=True)['status'] == 'open'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/release/brainwire')
    parser.add_argument('--tickets', type=int, default=10000)
    args = parser.parse_args()
    binary = args.binary.resolve()
    output = {'synthetic': True, 'driver': driver_version, 'profile': binary.parent.name,
              'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'limits': dict(encoded_message_mib=8, decoded_message_mib=16, io_timeout_ms=300000,
                             cache_mib=64, memtables_mib=16, query_mib=64)}
    with tempfile.TemporaryDirectory(prefix='brainwire-client-') as tmp:
        temporary = Path(tmp)
        source = temporary / 'fixture.jsonl'
        manifest = generate(source, args.tickets, 2, 1024)
        output['fixture'] = manifest
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            address = '%s:%s' % sock.getsockname()
        env = {k: v for k, v in os.environ.items() if not k.startswith('BRAINWIRE_')}
        env.update(BRAINWIRE_SERVER_LISTEN=address,
                   BRAINWIRE_SERVER_DATA_DIR=str(temporary / 'data'),
                   BRAINWIRE_SERVER_DEFAULT_DATABASE='client-shadow-synthetic',
                   BRAINWIRE_MEMORY_CACHE='64MiB', BRAINWIRE_MEMORY_MEMTABLES='16MiB',
                   BRAINWIRE_MEMORY_QUERY='64MiB', BRAINWIRE_SERVER_MAX_MESSAGE='8MiB',
                   BRAINWIRE_SERVER_MAX_DECODED_MESSAGE='16MiB', BRAINWIRE_SERVER_IO_TIMEOUT_MS='300000')
        server = None
        log = (temporary / 'server.log').open('w+')

        def cli(*args):
            result = subprocess.run([str(binary), *args], env=env, cwd=ROOT,
                                    capture_output=True, text=True, timeout=60)
            if result.returncode:
                raise RuntimeError(result.stderr)
            return result.stdout

        def start():
            nonlocal server
            server = subprocess.Popen([str(binary), 'serve'], env=env, cwd=ROOT,
                                      stdout=log, stderr=log)
            for _ in range(100):
                if server.poll() is not None:
                    log.seek(0)
                    raise RuntimeError(log.read())
                try:
                    cli('probe', '--address', address, '--timeout-ms', '100')
                    return
                except RuntimeError:
                    time.sleep(.03)
            raise RuntimeError('server readiness timed out')

        def stop(kill=False):
            nonlocal server
            if server is not None:
                server.kill() if kill else server.terminate()
                try:
                    server.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait(timeout=5)
                server = None

        try:
            cli('db', 'create', 'client-shadow-synthetic', '--ontology', str(ROOT / 'examples/client/ontology.json'))
            cli('db', 'create', 'client-validation', '--ontology', str(ROOT / 'examples/client/ontology.json'))
            start()
            with GraphDatabase.driver('bolt://' + address, auth=None) as driver:
                verify_updates(driver, temporary)
                began = time.perf_counter()
                cursor = adapter.ingest_snapshot(driver, 'client-shadow-synthetic', source, 'fixture')
                elapsed = time.perf_counter() - began
                assert cursor == source.stat().st_size
                output['import_seconds'] = elapsed
                output['input_rows_per_second'] = (manifest['tickets'] + manifest['comments']) / elapsed
                with driver.session(database='client-shadow-synthetic') as session:
                    verify(session, manifest, cursor)
                    sequence = session.run(STATE).single(strict=True)['sequence']
                adapter.ingest_snapshot(driver, 'client-shadow-synthetic', source, 'fixture')
                with driver.session(database='client-shadow-synthetic') as session:
                    assert session.run(STATE).single(strict=True)['sequence'] == sequence
                    # Same logical source cannot silently change its mapping, even at EOF.
                    try:
                        base.ingest(driver, 'client-shadow-synthetic', source, 'fixture',
                                    normalize_row=adapter.normalize_row,
                                    write_batch=adapter.write_batch, contract_id='different-contract')
                    except ValueError:
                        pass
                    else:
                        raise AssertionError('mapping mismatch accepted')
                    assert session.run(STATE).single(strict=True)['sequence'] == sequence
                # Real callback error after nodes/edges are staged must roll back progress too.
                small = temporary / 'rollback.jsonl'
                ticket = {'record_type': 'ticket', 'tenant_id': 'rollback', 'connector_id': 'z',
                          'external_id': '1', 'subject': 'test', 'status': 'open', 'priority': None, 'tags': []}
                comment = {'record_type': 'comment', 'tenant_id': 'rollback', 'connector_id': 'z',
                           'external_id': 'c', 'ticket_id': '1', 'body': 'test', 'is_public': True}
                small.write_text('\n'.join(json.dumps(r) for r in [ticket, comment]) + '\n')
                def fail_after_writes(tx, rows):
                    adapter.write_batch(tx, rows)
                    raise RuntimeError('synthetic interruption before cursor')
                try:
                    base.ingest(driver, 'client-shadow-synthetic', small, 'rollback',
                                normalize_row=adapter.normalize_row, write_batch=fail_after_writes,
                                contract_id=adapter.CONTRACT_ID)
                except RuntimeError as error:
                    assert 'synthetic interruption' in str(error)
                else:
                    raise AssertionError('expected injected error')
                with driver.session(database='client-shadow-synthetic') as session:
                    verify(session, manifest, cursor)
                    assert session.run(base.CHECKPOINT, source_id='rollback').single() is None
                    # Allocator reservations can survive a failed user transaction.
                    sequence_after_rollback = session.run(STATE).single(strict=True)['sequence']
                    output['rollback_allocator_counter_advanced'] = sequence_after_rollback != sequence
                    sequence = sequence_after_rollback
                    # The eager mass-write path can exceed the unchanged 64MiB guard.
                    # Record its rejection instead of raising the budget to hide it.
                    tx = session.begin_transaction()
                    try:
                        tx.run("MATCH (t:Ticket {status:'open'}) SET t.status='uncommitted'").consume()
                        output['large_write_guard_rejected'] = False
                    except ClientError as error:
                        assert error.code.endswith('MemoryLimitExceeded'), error
                        output['large_write_guard_rejected'] = True
                    finally:
                        tx.rollback()
                    verify(session, manifest, cursor)
                    assert session.run(STATE).single(strict=True)['sequence'] == sequence
                    tx = session.begin_transaction()
                    tx.run("MATCH (t:Ticket {entity_id:$id}) SET t.status='uncommitted'",
                           id=adapter.identity('tenant-0', 'zendesk-main', 'ticket', '0')).consume()
                    tx.run(base.ADVANCE, source_id='fixture', next_offset=0).consume()
                    stop(kill=True)
                    with suppress(Exception):
                        tx.close()
            start()
            with GraphDatabase.driver('bolt://' + address, auth=None) as driver:
                with driver.session(database='client-shadow-synthetic') as session:
                    verify(session, manifest, cursor)
                    assert session.run(STATE).single(strict=True)['sequence'] == sequence
                adapter.ingest_snapshot(driver, 'client-shadow-synthetic', source, 'fixture')
            stop()
            output['audit'] = json.loads(cli('db', 'check', 'client-shadow-synthetic', '--json'))
            output['validation_audit'] = json.loads(cli('db', 'check', 'client-validation', '--json'))
            destination = temporary / 'data/restored'
            output['checkpoint'] = json.loads(cli('db', 'checkpoint', 'client-shadow-synthetic', '--destination', str(destination)))
            start()
            with GraphDatabase.driver('bolt://' + address, auth=None) as driver:
                with driver.session(database='restored') as session:
                    verify(session, manifest, cursor)
                    assert session.run(STATE).single(strict=True)['sequence'] == sequence
                adapter.ingest_snapshot(driver, 'restored', source, 'fixture')
            stop()
            output['restore_audit'] = json.loads(cli('db', 'check', 'restored', '--json'))
            for key in ('audit', 'validation_audit', 'restore_audit'):
                assert output[key]['status'] == 'ok', (key, output[key])
                assert output[key]['scalar_indexes'] == 'checked_active_declarations', (key, output[key])
            output['checks'] = ['exact node/edge/index counts', 'no-op replay', 'mapping mismatch',
                                'overwrite/null indexes', 'duplicate edge replay', 'immutable/missing parents',
                                'conflicting duplicate parents', 'graph isolation', 'changed source rejected',
                                'lost-acknowledgement partial resume',
                                'callback rollback', 'bounded write/guard rollback', 'SIGKILL pending data/cursor', 'reopen/replay',
                                'independent checkpoint restore/resume/audit']
            output['result'] = 'PASS'
            output['scope'] = 'localhost single-node synthetic snapshot; no CDC, auth, Raft, ANN or production performance certification'
            print(json.dumps(output, indent=2))
        finally:
            stop()
            log.close()


if __name__ == '__main__':
    main()
