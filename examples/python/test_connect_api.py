"""Connection ownership and fail-fast Python onboarding."""
import unittest
from unittest.mock import MagicMock, patch

from brainwire import connect


class ConnectionTests(unittest.TestCase):
    def driver(self):
        driver = MagicMock()
        driver.__enter__.return_value = driver
        return driver

    def test_connect_verifies_and_closes_owned_driver(self):
        driver = self.driver()
        with patch("neo4j.GraphDatabase.driver", return_value=driver) as factory:
            with connect("bolt://localhost:7687", "demo", auth=None,
                         batch_size=50, connection_timeout=3) as graph:
                self.assertIs(graph.driver, driver)
                self.assertEqual(graph.database, "demo")
                self.assertEqual(graph.batch_size, 50)
                driver.verify_connectivity.assert_called_once_with()
                driver.__exit__.assert_not_called()
            factory.assert_called_once_with("bolt://localhost:7687", auth=None,
                                            connection_timeout=3)
        driver.__exit__.assert_called_once()

    def test_connect_closes_on_connection_or_body_failure(self):
        for connection_failure in (True, False):
            driver = self.driver()
            if connection_failure:
                driver.verify_connectivity.side_effect = RuntimeError("unavailable")
            with patch("neo4j.GraphDatabase.driver", return_value=driver):
                with self.assertRaises(RuntimeError):
                    with connect("bolt://localhost:7687", "demo", auth=None):
                        self.assertFalse(connection_failure)
                        raise RuntimeError("body failed")
            driver.__exit__.assert_called_once()

    def test_invalid_batch_bounds_fail_before_network_verification(self):
        driver = self.driver()
        with patch("neo4j.GraphDatabase.driver", return_value=driver):
            with self.assertRaises(ValueError):
                with connect("bolt://localhost:7687", "demo", auth=None, batch_size=0):
                    self.fail("invalid configuration reached caller")
        driver.verify_connectivity.assert_not_called()
        driver.__exit__.assert_called_once()


if __name__ == "__main__":
    unittest.main()
