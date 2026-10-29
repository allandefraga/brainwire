import re
import unittest
from pathlib import Path

import brainwire
from brainwire.client import GraphClient, identifier, request_key
from examples.python import client as legacy
from scripts.release import python_version, workspace_version


class PackageTests(unittest.TestCase):
    def test_legacy_imports_are_the_packaged_implementation(self):
        self.assertIs(brainwire.GraphClient, GraphClient)
        self.assertIs(legacy.GraphClient, GraphClient)
        self.assertIs(legacy.request_key, request_key)
        self.assertIs(legacy.identifier, identifier)

    def test_package_tracks_workspace_version_and_declares_bolt_driver(self):
        root = Path(__file__).resolve().parents[2]
        manifest = (root / "pyproject.toml").read_text()
        version = re.search(r'^version = "([^"]+)"$', manifest, re.MULTILINE)
        self.assertEqual(version.group(1), python_version(workspace_version()))
        self.assertIn('"neo4j>=5.28,<7"', manifest)


if __name__ == "__main__":
    unittest.main()
