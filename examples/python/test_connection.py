"""Run with unittest discover in examples/python; synthetic credentials only."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import connection


class ConnectionTests(unittest.TestCase):
    @unittest.skipUnless(os.name == 'posix', 'Unix credential file permissions')
    def test_opened_credentials_reject_unsafe_permissions_and_allow_secret_symlinks(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'auth.json'
            token = 'a' * 64
            path.write_text(json.dumps(dict(principal='reader', credentials=token)))
            link = Path(folder) / 'mounted-secret'
            link.symlink_to(path)
            with patch.dict(os.environ, {'BRAINWIRE_BOLT_AUTH_FILE': str(link)}, clear=True):
                for mode in (0o400, 0o600, 0o440, 0o640, 0o444, 0o644):
                    path.chmod(mode)
                    self.assertEqual(connection.auth_from_env(), ('reader', token))
                for mode in (0o666, 0o620, 0o602, 0o622):
                    path.chmod(mode)
                    with self.assertRaises(ValueError) as failure:
                        connection.auth_from_env()
                    self.assertNotIn(token, str(failure.exception))

    @unittest.skipUnless(hasattr(os, 'mkfifo'), 'Unix FIFO files')
    def test_credential_fifo_is_rejected_without_waiting_for_a_writer(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'credential-fifo'
            os.mkfifo(path, 0o600)
            script = '''
import sys
sys.path.insert(0, sys.argv[1])
import connection
try:
    connection.auth_from_env()
except ValueError:
    raise SystemExit(0)
raise SystemExit('accepted a credential FIFO')
'''
            try:
                result = subprocess.run(
                    [sys.executable, '-c', script, str(Path(connection.__file__).parent)],
                    env={**os.environ, 'BRAINWIRE_BOLT_AUTH_FILE': str(path)},
                    capture_output=True, text=True, timeout=3,
                )
            except subprocess.TimeoutExpired:
                self.fail('credential loader blocked waiting for a FIFO writer')
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_absent_file_keeps_development_auth_and_valid_file_returns_tuple(self):
        with patch.dict(os.environ, {}, clear=True):
            self.assertIsNone(connection.auth_from_env())
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'auth.json'
            token = 'a' * 64
            path.write_text(json.dumps(dict(principal='reader', credentials=token)))
            with patch.dict(os.environ, {'BRAINWIRE_BOLT_AUTH_FILE': str(path)}, clear=True):
                self.assertEqual(connection.auth_from_env(), ('reader', token))

    def test_malformed_missing_oversized_and_duplicate_credentials_fail_without_echo(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'auth.json'
            cases = ['{"principal":"reader","credentials":"secret-marker"}',
                     '{"principal":"reader","principal":"other","credentials":"' + 'a' * 64 + '"}',
                     json.dumps(dict(principal='reader', credentials='a' * 64, extra=True)),
                     'x' * 65537, '[]']
            for content in cases:
                path.write_text(content)
                with patch.dict(os.environ, {'BRAINWIRE_BOLT_AUTH_FILE': str(path)}, clear=True):
                    with self.assertRaises(ValueError) as failure:
                        connection.auth_from_env()
                    self.assertNotIn('secret-marker', str(failure.exception))
            for name in ('', str(path.with_name('missing.json'))):
                with patch.dict(os.environ, {'BRAINWIRE_BOLT_AUTH_FILE': name}, clear=True):
                    with self.assertRaises(ValueError):
                        connection.auth_from_env()


if __name__ == '__main__':
    unittest.main()
