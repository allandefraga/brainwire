"""Readiness diagnostics without starting Docker resources."""
import unittest
import ssl
from unittest.mock import Mock, patch

from neo4j.exceptions import ServiceUnavailable

import compose_tls


class ReadinessDiagnostics(unittest.TestCase):
    def test_explicit_ca_uses_encryption_and_hostname_verification(self):
        scheme, options = compose_tls.driver_tls_options(True, True, '/tmp/test-ca.pem')
        self.assertEqual(scheme, 'bolt')
        self.assertIs(options['encrypted'], True)
        self.assertEqual(options['trusted_certificates'].certs, ('/tmp/test-ca.pem',))
        self.assertEqual(compose_tls.driver_tls_options(True, False, '/tmp/test-ca.pem'), ('bolt+s', {}))
        self.assertEqual(compose_tls.driver_tls_options(False, True, '/tmp/test-ca.pem'), ('bolt', {}))

    def test_legacy_cipher_setup_supports_libressl(self):
        context = Mock()
        context.set_ciphers.side_effect = [ssl.SSLError('unsupported security level'), None]
        compose_tls.configure_legacy_ciphers(context)
        self.assertEqual([call.args[0] for call in context.set_ciphers.call_args_list],
                         ['DEFAULT:@SECLEVEL=0', 'DEFAULT'])

    def test_timeout_chains_latest_tls_connection_error(self):
        first = ServiceUnavailable("first TLS failure")
        latest = ServiceUnavailable("latest certificate failure")
        read = Mock(side_effect=[first, latest])
        with patch.object(compose_tls.time, "monotonic", side_effect=[0, 0, 1, 90]), \
                patch.object(compose_tls.time, "sleep"):
            with self.assertRaisesRegex(RuntimeError, "backend logs") as caught:
                compose_tls.wait_ready(lambda: True, read, lambda: "backend logs")
        self.assertIs(caught.exception.__cause__, latest)
        self.assertEqual(read.call_count, 2)

    def test_retry_can_succeed_without_reading_failure_logs(self):
        read = Mock(side_effect=[ServiceUnavailable("not ready"), [{"ok": 1}]])
        logs = Mock()
        with patch.object(compose_tls.time, "monotonic", side_effect=[0, 0, 1]), \
                patch.object(compose_tls.time, "sleep"):
            compose_tls.wait_ready(lambda: True, read, logs)
        logs.assert_not_called()

    def test_backend_unready_timeout_has_no_synthetic_tls_error(self):
        read = Mock()
        with patch.object(compose_tls.time, "monotonic", side_effect=[0, 0, 90]), \
                patch.object(compose_tls.time, "sleep"):
            with self.assertRaisesRegex(RuntimeError, "backend unready") as caught:
                compose_tls.wait_ready(lambda: False, read, lambda: "backend unready")
        read.assert_not_called()
        self.assertIsNone(caught.exception.__cause__)


if __name__ == "__main__":
    unittest.main()
