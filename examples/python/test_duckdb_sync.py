"""DuckDB snapshots and resumable batch planning, using real local files."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from examples.python import duckdb_sync as sync
from examples.python.ducklake_sync import Mapping


MAPPING = Mapping.from_json({'table': 'items', 'label': 'Item', 'id': 'id', 'properties': ['value']})


@unittest.skipUnless(importlib.util.find_spec('duckdb'), 'DuckDB is optional')
class DuckDBSnapshots(unittest.TestCase):
    def setUp(self):
        import duckdb
        self.duckdb = duckdb
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.source = Path(tmp.name) / 'source.duckdb'
        self.journal = Path(tmp.name) / 'journal.sqlite'
        self.write('CREATE TABLE items(id INTEGER, value VARCHAR)')

    def write(self, sql):
        with self.duckdb.connect(str(self.source)) as db:
            db.execute(sql)

    def test_bounded_snapshot_diff_only_deletes_previously_owned_rows(self):
        self.write("INSERT INTO items VALUES (1, 'old'), (2, 'stable'), (3, 'removed')")
        with sync._journal(self.journal) as db:
            first = sync._stage(db, self.source, MAPPING, 'contract', 2, True, False)
            self.assertEqual(first.expected_snapshot, None)
            self.assertEqual(list(sync._upserts(db)), [
                {'entity_id': '1', 'value': 'old'}, {'entity_id': '2', 'value': 'stable'},
                {'entity_id': '3', 'value': 'removed'},
            ])
            sync._promote(db, first)
            self.write("UPDATE items SET value=NULL WHERE id=1; DELETE FROM items WHERE id=3; INSERT INTO items VALUES (4, 'new')")
            second = sync._stage(db, self.source, MAPPING, 'contract', 2, True, False)
            self.assertEqual(second.expected_snapshot, first.target_snapshot)
            self.assertEqual(list(sync._upserts(db)), [
                {'entity_id': '1', 'value': None}, {'entity_id': '4', 'value': 'new'},
            ])
            self.assertEqual(list(sync._deletes(db)), [('3',)])
            self.write('DELETE FROM items')
            self.assertEqual(list(sync._deletes(db)), [('3',)])
            self.assertEqual(sync._pending(db), second)

    def test_failed_staging_preserves_the_last_good_generation(self):
        self.write("INSERT INTO items VALUES (1, 'old')")
        with sync._journal(self.journal) as db:
            first = sync._stage(db, self.source, MAPPING, 'contract', 2, True, False)
            sync._promote(db, first)
            self.write('INSERT INTO items VALUES (1, NULL)')
            with self.assertRaisesRegex(ValueError, 'duplicate identity'):
                sync._stage(db, self.source, MAPPING, 'contract', 2, True, False)
            self.assertIsNone(sync._pending(db))
            self.write('DELETE FROM items')
            with self.assertRaisesRegex(ValueError, 'empty authoritative snapshot'):
                sync._stage(db, self.source, MAPPING, 'contract', 2, True, False)
            self.assertIsNone(sync._pending(db))
            permitted = sync._stage(db, self.source, MAPPING, 'contract', 2, True, True)
            self.assertEqual(permitted.expected_snapshot, first.target_snapshot)
            self.assertEqual(list(sync._deletes(db)), [('1',)])

    def test_a_second_process_cannot_open_the_same_journal(self):
        import os
        import subprocess
        import sys

        command = ('from pathlib import Path; from examples.python.duckdb_sync import _journal; '
                   'import sys; lock=_journal(Path(sys.argv[1])); lock.__enter__()')
        with sync._journal(self.journal):
            attempt = subprocess.run([sys.executable, '-c', command, str(self.journal)],
                                     env=os.environ, capture_output=True, text=True, timeout=5)
        self.assertNotEqual(attempt.returncode, 0)
        self.assertIn('locked by another process', attempt.stderr)

    def test_rejects_reserved_acl_property_before_opening_the_journal(self):
        mapping = Mapping.from_json({'table': 'items', 'label': 'Item', 'id': 'id', 'properties': ['acl']})
        with self.assertRaisesRegex(ValueError, 'acl is reserved'):
            sync.sync(mapping, source=self.source, journal=self.journal, uri='unreachable', database='test',
                      source_id='example', source_identity='synthetic', exclusive=True)
        self.assertFalse(self.journal.exists())

    def test_snapshot_bounds_roll_back_local_staging(self):
        self.write("INSERT INTO items VALUES (1, 'a'), (2, 'b')")
        with sync._journal(self.journal) as db:
            for options in ({'max_rows': 1}, {'max_snapshot_bytes': 1}):
                with self.assertRaisesRegex(ValueError, 'normalized row or byte limit'):
                    sync._stage(db, self.source, MAPPING, 'contract', 2, False, False, **options)
                self.assertIsNone(sync._pending(db))
                self.assertEqual(list(sync._upserts(db)), [])

    def test_claimed_pending_cannot_start_over_when_graph_checkpoint_disappears(self):
        self.write("INSERT INTO items VALUES (1, 'old')")
        with sync._journal(self.journal) as db:
            pending = sync._stage(db, self.source, MAPPING, 'contract', 2, False, False)
            self.assertIsNone(sync._position(None, pending, 'contract'))
            with db:
                db.execute('UPDATE state SET claimed=1')
        with sync._journal(self.journal) as db:
            pending = sync._pending(db)
            with self.assertRaisesRegex(RuntimeError, 'checkpoint disagree'):
                sync._position(None, pending, 'contract')
            self.assertEqual(sync._position({'contract': 'contract', 'snapshot': None,
                                             'lease': pending.target_snapshot, 'batch': 1},
                                            pending, 'contract'), 1)

    def test_source_id_is_bounded_in_utf8_before_looking_up_checkpoint(self):
        options = dict(source=self.source, journal=self.journal, uri='unreachable', database='test',
                       source_identity='synthetic', exclusive=True)
        for too_large in ('x' * 1025, 'ó' * 512 + 'x'):
            with self.assertRaisesRegex(ValueError, '1024 UTF-8 bytes'):
                sync.sync(MAPPING, source_id=too_large, **options)
        self.assertFalse(self.journal.exists())
        with patch.object(sync, '_journal', side_effect=RuntimeError('journal opened')):
            for allowed in ('x' * 1024, 'ó' * 512):
                with self.assertRaisesRegex(RuntimeError, 'journal opened'):
                    sync.sync(MAPPING, source_id=allowed, **options)

    def test_source_and_wal_are_protected_against_direct_and_aliased_journal_paths(self):
        before = self.source.read_bytes()
        wal = self.source.with_name(self.source.name + '.wal')
        options = dict(source=self.source, uri='unreachable', database='test', source_id='example',
                       source_identity='synthetic', exclusive=True)
        with self.assertRaisesRegex(ValueError, 'WAL and journal files must be separate'):
            sync.sync(MAPPING, journal=wal, **options)
        self.assertFalse(wal.exists())
        wal.write_bytes(b'existing WAL')
        sidecar = self.journal.with_name(self.journal.name + '.lock')
        sidecar.symlink_to(wal)
        with self.assertRaisesRegex(ValueError, 'WAL and journal files must be separate'):
            sync.sync(MAPPING, journal=self.journal, **options)
        self.assertFalse(self.journal.exists())
        self.assertEqual(wal.read_bytes(), b'existing WAL')
        self.assertEqual(self.source.read_bytes(), before)
