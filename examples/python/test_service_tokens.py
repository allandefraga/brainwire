import hashlib
import json
from pathlib import Path
import stat
import tempfile
import unittest

from create_service_tokens import create


class TokenFilesTests(unittest.TestCase):
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
