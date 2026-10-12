import hashlib
import json
from pathlib import Path
import stat
import tempfile
import unittest

from create_service_tokens import create


class TokenFilesTests(unittest.TestCase):
    def test_observer_is_explicit_and_cannot_write_or_expand_graph_scope(self):
        with tempfile.TemporaryDirectory() as folder:
            directory = Path(folder)/'auth'
            create(directory, 'tickets', include_observer=True)
            entries = json.loads((directory/'policy.json').read_text())['principals']
            self.assertEqual({entry['id'] for entry in entries}, {'writer', 'reader', 'health', 'observer'})
            for entry in entries:
                self.assertEqual(entry['graphs'], {'tickets': 'write' if entry['id'] == 'writer' else 'read'})
                self.assertEqual(entry['instance_admin'], entry['id'] == 'observer')
                file = directory/(entry['id']+'.json')
                token = json.loads(file.read_text())['credentials']
                self.assertEqual(hashlib.sha256(token.encode()).hexdigest(), entry['token_sha256'])
                self.assertEqual(stat.S_IMODE(file.stat().st_mode) & 0o077, 0)

    def test_private_unique_tokens_match_policy_and_existing_directory_is_preserved(self):
        with tempfile.TemporaryDirectory() as folder:
            directory = Path(folder)/'auth'
            create(directory, 'tickets')
            policy = json.loads((directory/'policy.json').read_text())
            tokens = []
            for entry in policy['principals']:
                file = directory/(entry['id']+'.json')
                token = json.loads(file.read_text())['credentials']
                tokens.append(token)
                self.assertEqual(len(token), 64)
                self.assertEqual(hashlib.sha256(token.encode()).hexdigest(), entry['token_sha256'])
                self.assertFalse(entry['instance_admin'])
                self.assertEqual(stat.S_IMODE(file.stat().st_mode) & 0o077, 0)
                self.assertNotIn(token, (directory/'policy.json').read_text())
            self.assertEqual(len(set(tokens)), 3)
            before = (directory/'policy.json').read_bytes()
            with self.assertRaises(FileExistsError): create(directory, 'tickets')
            self.assertEqual((directory/'policy.json').read_bytes(), before)
            invalid = Path(folder)/'invalid'
            with self.assertRaises(ValueError): create(invalid, '../escape')
            self.assertFalse(invalid.exists())


if __name__ == '__main__':
    unittest.main()
