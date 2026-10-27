import unittest
import os
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch

from scripts import release
from scripts.release import Version, choose_bump, next_version, updated_files


class SemVerTests(unittest.TestCase):
    def test_breaking_changes_outweigh_feature_and_patch(self):
        self.assertEqual(
            choose_bump(["fix: repair one path", "feat: add an index", "feat!: change protocol"]),
            "major",
        )
        self.assertEqual(choose_bump(["feat: add an index", "fix: repair a path"]), "minor")
        self.assertEqual(choose_bump(["fix: repair a path"]), "patch")
        self.assertEqual(choose_bump(["docs: clarify migration", "BREAKING CHANGE: remove legacy field"]), "major")
        self.assertEqual(choose_bump(["feat(index): add vector search"]), "minor")

    def test_non_feature_delivery_still_gets_a_patch(self):
        self.assertEqual(choose_bump(["docs: clarify install steps", "ci: trim Rust build artifacts"]), "patch")
        with self.assertRaisesRegex(ValueError, "no commits since"):
            choose_bump([])

    def test_major_minor_and_patch(self):
        current = Version.parse("0.1.1")
        self.assertEqual(str(next_version(current, "patch")), "0.1.2")
        self.assertEqual(str(next_version(current, "minor")), "0.2.0")
        self.assertEqual(str(next_version(current, "major")), "1.0.0")

    def test_beta_releases_increment_or_start_a_new_line(self):
        self.assertEqual(str(next_version(Version.parse("0.1.2-beta.1"), "patch", "beta")), "0.1.2-beta.2")
        self.assertEqual(str(next_version(Version.parse("0.1.2-beta.1"), "minor", "beta")), "0.2.0-beta.1")
        self.assertEqual(str(next_version(Version.parse("0.1.1"), "patch", "beta")), "0.1.2-beta.1")

    def test_rejects_invalid_semver_numeric_identifiers(self):
        for value in ("01.2.3", "1.2.3-beta.01", "1.2", "1.2.3+build"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                Version.parse(value)


class ReleaseFileTests(unittest.TestCase):
    def test_managed_versions_advance_but_historical_license_tag_stays(self):
        current = release.workspace_version()
        next_candidate = next_version(current, "patch", "beta")
        files = updated_files(current, next_candidate)
        readme = files[Path("README.md")]
        self.assertIn(
            f"docker pull ghcr.io/allandefraga/brainwire:v{next_candidate} # brainwire-release-version",
            readme,
        )
        self.assertIn("tree/v0.1.1", readme)  # Historical tag that predates the license.
        self.assertIn(
            f"newTag: v{next_candidate}",
            files[Path("deploy/kubernetes-release/kustomization.yaml")],
        )
        self.assertIn(
            f"newTag: v{next_candidate}",
            files[Path("deploy/kubernetes-public-release/kustomization.yaml")],
        )
        self.assertIn(
            f"DOCUMENTATION / v{next_candidate} <!-- brainwire-release-version -->",
            files[Path("docs-site/index.html")],
        )
        self.assertIn(
            f"v{next_candidate} · evolving <!-- brainwire-release-version -->",
            files[Path("docs-site/index.html")],
        )
        lock = files[Path("Cargo.lock")]
        self.assertEqual(lock.count(f'version = "{next_candidate}"'), 7)

    def test_can_advance_versions_after_a_previous_release_bump(self):
        initial = release.workspace_version()
        first_candidate = next_version(initial, "patch", "beta")
        second_candidate = next_version(first_candidate, "patch", "beta")

        with TemporaryDirectory() as temp_dir:
            simulated_root = Path(temp_dir)
            first_update = updated_files(initial, first_candidate)
            for relative, content in first_update.items():
                destination = simulated_root / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_text(content, encoding="utf-8")

            with patch.object(release, "ROOT", simulated_root):
                second_update = updated_files(first_candidate, second_candidate)

        readme = second_update[Path("README.md")]
        self.assertIn(
            f"docker pull ghcr.io/allandefraga/brainwire:v{second_candidate} # brainwire-release-version",
            readme,
        )
        self.assertIn(
            f"newTag: v{second_candidate}",
            second_update[Path("deploy/kubernetes-release/kustomization.yaml")],
        )

    def test_local_acceptance_uses_the_configured_cargo_target_directory(self):
        with patch.dict(os.environ, {"CARGO_TARGET_DIR": "/tmp/brainwire-target"}):
            with patch.object(release.subprocess, "run") as run:
                release.run_local_release_checks()
        acceptance = run.call_args_list[6].args[0]
        self.assertEqual(acceptance[acceptance.index("--binary") + 1],
                         "/tmp/brainwire-target/debug/brainwire")


if __name__ == "__main__":
    unittest.main()
