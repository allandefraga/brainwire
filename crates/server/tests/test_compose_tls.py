"""Readiness diagnostics without starting Docker resources."""
import unittest
from unittest.mock import Mock, patch

from neo4j.exceptions import ServiceUnavailable

import compose_tls


class ReadinessDiagnostics(unittest.TestCase):
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
