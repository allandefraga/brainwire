"""Run with unittest discover in examples/python; synthetic credentials only."""
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import connection


class ConnectionTests(unittest.TestCase):
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
