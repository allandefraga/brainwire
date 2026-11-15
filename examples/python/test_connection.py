"""Run with unittest discover in examples/python; synthetic credentials only."""
from contextlib import redirect_stderr
from io import StringIO
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

from examples.company_brain import demo
from examples.obsidian import import_vault
from examples.python import agent_context_demo, agent_memory, connection


class ConnectionTests(unittest.TestCase):
    def test_bolt_transport_accepts_loopback_and_verified_tls(self):
        for uri, expected in (
            ('bolt://localhost:17687', 'bolt://127.0.0.1:17687'),
            ('bolt://127.0.0.2', 'bolt://127.0.0.2:7687'),
            ('bolt://[::1]:7688', 'bolt://[::1]:7688'),
            ('bolt+s://db.example:7687', 'bolt+s://db.example:7687'),
        ):
            with self.subTest(uri=uri):
                self.assertEqual(connection.bolt_connection_options(uri), (expected, {}))

        from neo4j import TrustCustomCAs

        uri = 'bolt://db.example:7687'
        actual, config = connection.bolt_connection_options(uri, 'ca.pem')
        self.assertEqual(actual, uri)
        self.assertIs(config['encrypted'], True)
        self.assertIsInstance(config['trusted_certificates'], TrustCustomCAs)
        self.assertEqual(config['trusted_certificates'].certs, ('ca.pem',))

    def test_bolt_transport_rejects_unverified_and_invalid_uris(self):
        for uri, ca in (
            ('bolt://db.example:7687', None),
            ('bolt://192.0.2.10:7687', None),
            ('bolt+ssc://127.0.0.1', None),
            ('neo4j://localhost', None),
            ('bolt+s://user:secret@db.example', None),
            ('bolt://localhost:invalid', None),
            ('bolt://localhost:0', None),
            ('bolt://localhost?host=db.example', None),
            ('bolt+s://db.example', 'ca.pem'),
            ('bolt://db.example', ''),
        ):
            with self.subTest(uri=uri, ca=ca), self.assertRaises(ValueError) as failure:
                connection.bolt_connection_options(uri, ca)
            self.assertNotIn('secret', str(failure.exception))

    def test_cli_refuses_remote_plaintext_before_loading_auth_or_connecting(self):
        with tempfile.TemporaryDirectory() as temporary:
            vault = Path(temporary).resolve()
            (vault / 'Example.md').write_text('# Example', encoding='utf-8')
            for module, arguments in (
                (demo, []),
                (agent_context_demo, []),
                (agent_memory, ['recall', '--project', 'synthetic']),
                (import_vault, [str(vault), '--database', 'synthetic']),
            ):
                with self.subTest(module=module.__name__), \
                        patch.object(module, 'auth_from_env') as auth, \
                        patch.object(module, 'connect') as connect, \
                        redirect_stderr(StringIO()) as stderr, \
                        self.assertRaises(SystemExit) as result:
                    module.main(['--uri', 'bolt://192.0.2.10:7687', *arguments])
                self.assertEqual(result.exception.code, 2)
                self.assertIn('use verified TLS', stderr.getvalue())
                auth.assert_not_called()
                connect.assert_not_called()

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
