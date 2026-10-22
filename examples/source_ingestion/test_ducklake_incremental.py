"""Synthetic cached DuckLake + native protected Bolt incremental qualification."""
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import unittest
from unittest.mock import patch

from examples.python.create_service_tokens import create
from examples.source_ingestion import ducklake_incremental as delta
from examples.source_ingestion.export_ducklake_snapshot import export_ducklake_snapshot
from examples.source_ingestion.ingest_snapshot import ingest_snapshot
from examples.source_ingestion import test_ducklake_snapshot as snapshot_tests

ROOT = Path(__file__).resolve().parents[2]


class IncrementalTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        snapshot_tests.DuckLakeTests.setUpClass.__func__(cls)

    def setUp(self):
        snapshot_tests.DuckLakeTests.setUp(self)
        self.writer.execute("""INSERT INTO writer.main.source VALUES
          ('ticket','synthetic-tenant','zendesk','gone',NULL,'open',NULL,NULL,NULL,NULL,NULL),
          ('comment','synthetic-tenant','zendesk','gone-comment',NULL,NULL,NULL,NULL,'gone','removed',true)""")
        self.version = self.writer.execute("SELECT max(snapshot_id) FROM ducklake_snapshots('writer')").fetchone()[0]
        self.baseline = self.output.parent / "baseline.jsonl"
        export_ducklake_snapshot(self.catalog, self.data, self.version, self.query, self.baseline)
        self.output = self.output.parent / "changes.jsonl"

    def newer(self):
        self.writer.execute("UPDATE writer.main.source SET status='closed',tags=NULL WHERE external_id='t1'")
        self.writer.execute("DELETE FROM writer.main.source WHERE external_id IN ('c1','gone','gone-comment')")
        self.writer.execute("""INSERT INTO writer.main.source VALUES
          ('ticket','synthetic-tenant','zendesk','t2','new',NULL,NULL,['new'],NULL,NULL,NULL),
          ('comment','synthetic-tenant','zendesk','c2',NULL,NULL,NULL,NULL,'t2',NULL,false)""")
        return self.writer.execute("SELECT max(snapshot_id) FROM ducklake_snapshots('writer')").fetchone()[0]

    def export(self, version=None, **bounds):
        version = self.newer() if version is None else version
        return delta.export_incremental(self.catalog, self.data, self.version, version, self.query, "synthetic-source", self.output, **bounds)

    def unpublished(self):
        self.assertFalse(self.output.exists())
        self.assertEqual(list(self.output.parent.glob('.incremental-*.tmp')), [])

    @contextmanager
    def graph(self, *, with_restart=False):
        try:
            from neo4j import GraphDatabase
        except ImportError:
            self.skipTest('optional Neo4j driver unavailable')
        binary = Path(os.environ.get('BRAINWIRE_INCREMENTAL_BINARY', ROOT / 'target/release/brainwire')).resolve()
        if not binary.exists():
            self.skipTest('native binary unavailable')
        with tempfile.TemporaryDirectory(prefix='brainwire-incremental-test-') as folder:
            root = Path(folder)
            auth = create(root / 'auth', 'shadow')
            writer = json.loads((auth / 'writer.json').read_text())
            with socket.socket() as reserved:
                reserved.bind(('127.0.0.1', 0))
                address = '%s:%s' % reserved.getsockname()
            env = {key: value for key, value in os.environ.items() if not key.startswith('BRAINWIRE_')}
            env.update(BRAINWIRE_SERVER_DATA_DIR=str(root / 'data'), BRAINWIRE_SERVER_LISTEN=address,
                       BRAINWIRE_SERVER_DEFAULT_DATABASE='shadow', BRAINWIRE_SERVER_AUTH_FILE=str(auth / 'policy.json'),
                       BRAINWIRE_BOLT_AUTH_FILE=str(auth / 'health.json'), BRAINWIRE_MEMORY_CACHE='64MiB',
                       BRAINWIRE_MEMORY_MEMTABLES='16MiB', BRAINWIRE_MEMORY_QUERY='64MiB')
            def cli(*args):
                return subprocess.run([str(binary), *args], env=env, cwd=ROOT, capture_output=True, text=True, check=True, timeout=30).stdout
            cli('db', 'create', 'shadow', '--ontology', str(Path(__file__).with_name('ontology.json')))
            with (root / 'server.log').open('w') as log:
                def start_server():
                    process = subprocess.Popen([str(binary), 'serve'], env=env, cwd=ROOT, stdout=log, stderr=log)
                    try:
                        for _ in range(150):
                            self.assertIsNone(process.poll())
                            try:
                                cli('probe', '--address', address, '--timeout-ms', '100')
                                return process
                            except subprocess.CalledProcessError:
                                time.sleep(.03)
                        self.fail('protected server readiness timeout')
                    except BaseException:
                        if process.poll() is None:
                            process.kill()
                        process.wait(timeout=5)
                        raise

                def connect():
                    return GraphDatabase.driver('bolt://' + address, auth=(writer['principal'], writer['credentials']),
                                                liveness_check_timeout=30, max_transaction_retry_time=0)

                server = start_server()
                driver = connect()
                def crash_and_restart():
                    nonlocal server, driver
                    server.kill()
                    self.assertEqual(server.wait(timeout=5), -9)
                    driver.close()
                    server = start_server()
                    driver = connect()
                    return driver

                try:
                    yield (driver, crash_and_restart) if with_restart else driver
                finally:
                    driver.close()
                    if server.poll() is None:
                        server.terminate()
                    try:
                        server.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        server.kill()
                        server.wait(timeout=5)
            audit = json.loads(cli('db', 'check', 'shadow', '--json'))
            self.assertEqual(audit['status'], 'ok', audit)

    def graph_query(self, driver, sql, **params):
        with driver.session(database='shadow') as session:
            return session.run(sql, params).data()

    def bootstrap(self, driver):
        ingest_snapshot(driver, 'shadow', self.baseline, 'baseline', batch_size=1)
        delta.initialize_source(driver, 'shadow', self.output, self.baseline, 'baseline')

    def assert_new_graph(self, driver):
        self.assertEqual(self.graph_query(driver, 'MATCH (t:Ticket) RETURN count(t) AS n'), [{'n': 2}])
        self.assertEqual(self.graph_query(driver, 'MATCH (c:TicketComment) RETURN count(c) AS n'), [{'n': 1}])
        self.assertEqual(self.graph_query(driver, 'MATCH (t:Ticket)-[:HAS_COMMENT]->(c:TicketComment) RETURN c.external_id AS id'), [{'id': 'c2'}])
        self.assertEqual(self.graph_query(driver, "MATCH (t:Ticket) WHERE t.status='open' RETURN t.external_id"), [])
        self.assertEqual(self.graph_query(driver, "MATCH (t:Ticket) WHERE t.status='closed' RETURN t.external_id AS id"), [{'id': 't1'}])
        self.assertEqual(self.graph_query(driver, "MATCH (t:Ticket {entity_id:$id}) RETURN t.tags AS tags", id=delta.identity('synthetic-tenant','zendesk','ticket','t1')), [{'tags': None}])

    def test_diff_order_immutability_and_digest(self):
        receipt = self.export()
        rows = [json.loads(line) for line in self.output.read_bytes().splitlines()]
        self.assertEqual([row['operation'] for row in rows], ['begin','delete','delete','delete','upsert','upsert','upsert','end'])
        self.assertEqual([row['record_type'] for row in rows[1:4]], ['comment','comment','ticket'])
        self.assertEqual(receipt['changes'], 6)
        self.assertEqual(receipt['sha256'], hashlib.sha256(self.output.read_bytes()).hexdigest())
        self.assertEqual(delta.validate_artifact(self.output)[0]['to_version'], receipt['to_version'])
        original = self.output.read_bytes()
        with self.assertRaises(FileExistsError):
            self.export(receipt['to_version'])
        self.assertEqual(self.output.read_bytes(), original)

    def test_invalid_versions_and_bounds_leave_no_final_file(self):
        latest = self.newer()
        for bounds in ({'max_rows':3},{'max_input_bytes':1},{'max_output_bytes':1},{'line_bytes':1}):
            with self.subTest(bounds=bounds), self.assertRaises(ValueError):
                self.export(latest, **bounds)
            self.unpublished()
        for version in (self.version, self.version - 1, True):
            with self.subTest(version=version), self.assertRaises(ValueError):
                self.export(version)
            self.unpublished()

    def test_duplicate_source_identity_refused(self):
        self.writer.execute("INSERT INTO writer.main.source SELECT * FROM writer.main.source WHERE external_id='t1'")
        latest = self.writer.execute("SELECT max(snapshot_id) FROM ducklake_snapshots('writer')").fetchone()[0]
        with self.assertRaisesRegex(ValueError, 'duplicate'):
            self.export(latest)
        self.unpublished()

    def test_reparenting_and_dangling_comments_refused(self):
        self.writer.execute("UPDATE writer.main.source SET ticket_id='gone' WHERE external_id='c1'")
        latest = self.writer.execute("SELECT max(snapshot_id) FROM ducklake_snapshots('writer')").fetchone()[0]
        with self.assertRaisesRegex(ValueError, 'immutable'):
            self.export(latest)
        self.unpublished()
        self.writer.execute("DELETE FROM writer.main.source WHERE external_id='gone'")
        latest = self.writer.execute("SELECT max(snapshot_id) FROM ducklake_snapshots('writer')").fetchone()[0]
        with self.assertRaisesRegex(ValueError, 'existing|snapshot ticket'):
            self.export(latest)
        self.unpublished()

    def test_end_to_end_deletions_mirrors_null_indexes_and_replay(self):
        self.export()
        with self.graph() as driver:
            self.bootstrap(driver)
            offset = delta.ingest_incremental(driver, 'shadow', self.output, batch_size=1)
            self.assertEqual(offset, self.output.stat().st_size)
            self.assert_new_graph(driver)
            self.assertEqual(delta.ingest_incremental(driver, 'shadow', self.output, batch_size=1), offset)
            self.assert_new_graph(driver)

    def test_sigkill_resume_preserves_data_cursor_and_source_head(self):
        receipt = self.export()
        artifact = self.output.read_bytes()
        first_offset = sum(map(len, artifact.splitlines(keepends=True)[:2]))
        checkpoint_id = json.dumps([delta.CONTRACT_ID, 'synthetic-source', self.version,
                                    receipt['to_version'], receipt['sha256']], separators=(',', ':'))
        with self.graph(with_restart=True) as (driver, crash_and_restart):
            self.bootstrap(driver)
            class OneBatch:
                def session(self, **kwargs):
                    inner = driver.session(**kwargs)
                    class Session:
                        def __enter__(self):
                            inner.__enter__()
                            return self
                        def __exit__(self, *args):
                            return inner.__exit__(*args)
                        def execute_write(self, callback):
                            inner.execute_write(callback)
                            raise RuntimeError('synthetic stop after first committed batch')
                    return Session()
            with self.assertRaisesRegex(RuntimeError, 'first committed batch'):
                delta.ingest_incremental(OneBatch(), 'shadow', self.output, batch_size=2)

            driver = crash_and_restart()
            checkpoint_query = "MATCH (s:ImportCheckpoint {entity_id:$id}) RETURN s.next_offset AS offset,s.sha256 AS sha256"
            head_query = "MATCH (s:ImportCheckpoint {entity_id:$id}) RETURN s.current_version AS version,s.snapshot_sha256 AS snapshot,s.active_artifact AS active"
            self.assertEqual(self.graph_query(driver, checkpoint_query, id=checkpoint_id),
                             [{'offset':first_offset, 'sha256':receipt['sha256']}])
            self.assertEqual(self.graph_query(driver, head_query, id=delta.head_id('synthetic-source')),
                             [{'version':self.version, 'snapshot':receipt['from_snapshot_sha256'], 'active':receipt['sha256']}])
            self.assertEqual(self.graph_query(driver, 'MATCH (c:TicketComment) RETURN count(c) AS n'), [{'n':1}])
            self.assertEqual(delta.ingest_incremental(driver, 'shadow', self.output, batch_size=2), len(artifact))
            self.assert_new_graph(driver)
            self.assertEqual(self.graph_query(driver, checkpoint_query, id=checkpoint_id),
                             [{'offset':len(artifact), 'sha256':receipt['sha256']}])
            self.assertEqual(self.graph_query(driver, head_query, id=delta.head_id('synthetic-source')),
                             [{'version':receipt['to_version'], 'snapshot':receipt['to_snapshot_sha256'], 'active':None}])
            self.assertEqual(self.output.read_bytes(), artifact)

    def test_batch_limit_pauses_between_committed_delta_batches(self):
        receipt = self.export()
        artifact = self.output.read_bytes()
        first_offset = sum(map(len, artifact.splitlines(keepends=True)[:2]))
        checkpoint_id = json.dumps([delta.CONTRACT_ID, 'synthetic-source', self.version,
                                    receipt['to_version'], receipt['sha256']], separators=(',', ':'))
        with self.graph() as driver:
            self.bootstrap(driver)
            offset = delta.ingest_incremental(driver, 'shadow', self.output,
                                              batch_size=2, max_batches=1)
            self.assertEqual(offset, first_offset)
            self.assertEqual(self.graph_query(driver,
                "MATCH (s:ImportCheckpoint {entity_id:$id}) RETURN s.next_offset AS offset",
                id=checkpoint_id), [{'offset':first_offset}])
            self.assertEqual(self.graph_query(driver,
                "MATCH (s:ImportCheckpoint {entity_id:$id}) RETURN s.current_version AS version,s.active_artifact AS active",
                id=delta.head_id('synthetic-source')),
                [{'version':self.version, 'active':receipt['sha256']}])
            self.assertEqual(delta.ingest_incremental(driver, 'shadow', self.output,
                                                      batch_size=2), len(artifact))
            self.assert_new_graph(driver)

    def test_baseline_requires_matching_completed_snapshot_checkpoint(self):
        self.export()
        with self.graph() as driver:
            with self.assertRaisesRegex(ValueError, 'completed matching snapshot'):
                delta.initialize_source(driver, 'shadow', self.output, self.baseline, 'baseline')
            ingest_snapshot(driver, 'shadow', self.baseline, 'baseline')
            wrong = self.baseline.with_name('wrong.jsonl')
            wrong.write_bytes(self.baseline.read_bytes().replace(b'old body', b'bad body'))
            with self.assertRaisesRegex(ValueError, 'from snapshot'):
                delta.initialize_source(driver, 'shadow', self.output, wrong, 'baseline')
            self.graph_query(driver, "MATCH (s:ImportCheckpoint {entity_id:'baseline'}) SET s.next_offset=0")
            with self.assertRaisesRegex(ValueError, 'completed matching snapshot'):
                delta.initialize_source(driver, 'shadow', self.output, self.baseline, 'baseline')

    def test_failed_batch_resume_and_competing_artifact(self):
        self.export()
        with self.graph() as driver:
            self.bootstrap(driver)
            original = delta.write_snapshot_batch
            def fail(tx, rows):
                original(tx, rows)
                raise RuntimeError('injected after data staging')
            with patch.object(delta, 'write_snapshot_batch', fail), self.assertRaises(RuntimeError):
                delta.ingest_incremental(driver, 'shadow', self.output, batch_size=1)
            self.assertEqual(self.graph_query(driver, "MATCH (t:Ticket) WHERE t.status='closed' RETURN t.external_id"), [])
            competitor = self.output.with_name('competitor.jsonl')
            competitor.write_bytes(self.output.read_bytes().replace(b'"subject":"new"', b'"subject":"competing"'))
            with self.assertRaisesRegex(ValueError, 'competing'):
                delta.ingest_incremental(driver, 'shadow', competitor, batch_size=1)
            delta.ingest_incremental(driver, 'shadow', self.output, batch_size=1)
            self.assert_new_graph(driver)
            self.graph_query(driver, 'MATCH (s:ImportCheckpoint {entity_id:$id}) SET s.current_version=0', id=delta.head_id('synthetic-source'))
            stale = self.output.with_name('stale.jsonl')
            stale.write_bytes(self.output.read_bytes().replace(b'"subject":"new"', b'"subject":"stale"'))
            with self.assertRaisesRegex(ValueError, 'wrong baseline/version'):
                delta.ingest_incremental(driver, 'shadow', stale)

    def test_empty_diff_advances_version(self):
        self.writer.execute("UPDATE writer.main.source SET subject='temporary' WHERE external_id='t1'")
        self.writer.execute("UPDATE writer.main.source SET subject=NULL WHERE external_id='t1'")
        latest = self.writer.execute("SELECT max(snapshot_id) FROM ducklake_snapshots('writer')").fetchone()[0]
        receipt = self.export(latest)
        self.assertEqual(receipt['changes'], 0)
        with self.graph() as driver:
            self.bootstrap(driver)
            delta.ingest_incremental(driver, 'shadow', self.output, batch_size=1)
            self.assertEqual(self.graph_query(driver, 'MATCH (s:ImportCheckpoint {entity_id:$id}) RETURN s.current_version AS version,s.active_artifact AS active', id=delta.head_id('synthetic-source')), [{'version':latest,'active':None}])

    def test_successive_versions_and_naturally_stale_artifact(self):
        first = self.export()
        self.writer.execute("UPDATE writer.main.source SET body='second version' WHERE external_id='c2'")
        newest = self.writer.execute("SELECT max(snapshot_id) FROM ducklake_snapshots('writer')").fetchone()[0]
        proper = self.output.with_name('next.jsonl')
        stale = self.output.with_name('out-of-order.jsonl')
        delta.export_incremental(self.catalog, self.data, first['to_version'], newest, self.query, 'synthetic-source', proper)
        delta.export_incremental(self.catalog, self.data, self.version, newest, self.query, 'synthetic-source', stale)
        with self.graph() as driver:
            self.bootstrap(driver)
            delta.ingest_incremental(driver, 'shadow', self.output, batch_size=1)
            with self.assertRaisesRegex(ValueError, 'wrong baseline/version'):
                delta.ingest_incremental(driver, 'shadow', stale, batch_size=1)
            delta.ingest_incremental(driver, 'shadow', proper, batch_size=1)
            self.assert_new_graph(driver)
            self.assertEqual(self.graph_query(driver, "MATCH (c:TicketComment) RETURN c.body AS body"), [{'body':'second version'}])
            self.assertEqual(delta.ingest_incremental(driver, 'shadow', self.output), self.output.stat().st_size)
            self.assertEqual(self.graph_query(driver, 'MATCH (s:ImportCheckpoint {entity_id:$id}) RETURN s.current_version AS version', id=delta.head_id('synthetic-source')), [{'version':newest}])

    def test_malformed_artifact_rejected_before_graph_access(self):
        self.export()
        original = self.output.read_bytes()
        for content in (original.splitlines(keepends=True)[0], original.replace(b'"changes":6',b'"changes":7'), original + original.splitlines(keepends=True)[0], original.replace(b'"operation":"delete"',b'"operation":"unknown"',1), original[:-1]):
            malformed = self.output.with_name('malformed.jsonl')
            malformed.write_bytes(content)
            with self.assertRaises(ValueError):
                delta.ingest_incremental(None, 'shadow', malformed)

    def test_lost_acknowledgement_resumes_committed_cursor(self):
        self.export()
        with self.graph() as driver:
            self.bootstrap(driver)
            class LostAck:
                def session(self, **kwargs):
                    inner = driver.session(**kwargs)
                    class Session:
                        calls = 0
                        def __enter__(self):
                            inner.__enter__()
                            return self
                        def __exit__(self, *args):
                            return inner.__exit__(*args)
                        def execute_write(self, callback):
                            result = inner.execute_write(callback)
                            self.calls += 1
                            if self.calls == 2:
                                raise RuntimeError('synthetic lost committed acknowledgement')
                            return result
                    return Session()
            with self.assertRaisesRegex(RuntimeError, 'lost committed'):
                delta.ingest_incremental(LostAck(), 'shadow', self.output, batch_size=1)
            delta.ingest_incremental(driver, 'shadow', self.output, batch_size=1)
            self.assert_new_graph(driver)


if __name__ == '__main__':
    unittest.main()
