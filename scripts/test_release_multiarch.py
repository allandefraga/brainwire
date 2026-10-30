"""Exercise the manifest guard embedded in the release workflow."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest

WORKFLOW = Path(__file__).resolve().parents[1] / ".github/workflows/release.yml"


def manifest_guard():
    text = WORKFLOW.read_text()
    section = text.split("      - name: Create the manifest from tested digests", 1)[1]
    source = section.split("          python - <<'PY'\n", 1)[1].split("          PY\n", 1)[0]
    return textwrap.dedent(source)


class MultiarchManifestTests(unittest.TestCase):
    def test_release_branches_run_ci_before_tagging(self):
        ci = WORKFLOW.with_name("ci.yml").read_text()
        self.assertIn('branches: [main, "release/**"]', ci)
        self.assertIn("  workflow_dispatch:", ci)
        self.assertIn("python scripts/check_docs_links.py", ci)
        self.assertIn("python scripts/check_docs_site.py", ci)

    def validate(self, entries):
        with tempfile.TemporaryDirectory(prefix="brainwire-manifest-test-") as temporary:
            root = Path(temporary)
            (root / "image-metadata").mkdir()
            for arch, digit in (("amd64", "a"), ("arm64", "b")):
                (root / "image-metadata" / f"{arch}.digest").write_text("sha256:" + digit * 64 + "\n")
            (root / "manifest.json").write_text(json.dumps({"manifests": entries}))
            return subprocess.run([sys.executable, "-c", manifest_guard()], cwd=root,
                                  capture_output=True, text=True).returncode

    def entries(self):
        return [{"platform": {"os": "linux", "architecture": arch},
                 "digest": "sha256:" + digit * 64}
                for arch, digit in (("amd64", "a"), ("arm64", "b"))]

    def test_publication_follows_native_acceptance_and_uses_commit_identity(self):
        workflow = WORKFLOW.read_text()
        self.assertIn('runner: ubuntu-24.04-arm', workflow)
        self.assertIn('$RELEASE_TAG-$GITHUB_SHA-$ARCH', workflow)
        self.assertLess(workflow.index('name: Verify the exact image and retained-volume recovery'),
                        workflow.index('docker push "$CANDIDATE"'))
        self.assertIn('test "$revision" = "$GITHUB_SHA"', workflow)
        self.assertIn('needs: [verify, image]', workflow)

    def test_accepts_exactly_both_tested_architectures(self):
        self.assertEqual(self.validate(self.entries()), 0)

    def test_missing_architecture_fails(self):
        self.assertNotEqual(self.validate(self.entries()[:1]), 0)

    def test_untested_digest_fails(self):
        entries = self.entries()
        entries[1]["digest"] = "sha256:" + "c" * 64
        self.assertNotEqual(self.validate(entries), 0)

    def test_duplicate_architecture_fails(self):
        entries = self.entries()
        self.assertNotEqual(self.validate(entries + entries[:1]), 0)

    def test_wrong_operating_system_fails(self):
        entries = self.entries()
        entries[1]["platform"]["os"] = "windows"
        self.assertNotEqual(self.validate(entries), 0)


if __name__ == "__main__":
    unittest.main()
