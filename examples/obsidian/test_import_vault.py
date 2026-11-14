import contextlib
import io
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from examples.obsidian.import_vault import MAX_NOTE_BYTES, main, prepare


class VaultTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.vault = self.root / "vault"
        self.vault.mkdir()

    def write(self, path, body, *, root=None):
        file = (root or self.vault) / path
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(body, encoding="utf-8")

    def test_link_scope_and_supported_syntax(self):
        self.write("Intro.md", "[[notes/Decision.md#Details|Read it]] [[Decision]]\n"
                   "`[[inline]]` ![[embed]] \\[[escape]] [[#heading]]\n"
                   "````md\n[[fenced]]\n```\n[[short-closing-fence]]\n````\n"
                   "~~~\n[[tilde]]\n~~~\n")
        self.write("notes/Decision.md", "[[Intro]]\n")
        self.write(".obsidian/private.md", "[[hidden]]")
        self.write("attachment.txt", "[[attachment]]")
        notes, links = prepare(self.vault)
        self.assertEqual([note["path"] for note in notes], ["Intro.md", "notes/Decision.md"])
        self.assertEqual(links, [{"source": "Intro.md", "target": "notes/Decision.md"},
                                 {"source": "notes/Decision.md", "target": "Intro.md"}])

    def test_invalid_targets_fail_before_connection(self):
        self.write("one/Decision.md", "first")
        self.write("two/Decision.md", "second")
        for target, error in [("Decision", "ambiguous"), ("missing", "unresolved"),
                              ("../Decision", "unsupported"), ("/Decision", "unsupported"),
                              ("a\\Decision", "unsupported")]:
            with self.subTest(target=target):
                self.write("Intro.md", f"[[{target}]]")
                with patch("examples.obsidian.import_vault.connect") as connect, \
                        contextlib.redirect_stderr(io.StringIO()) as stderr, \
                        self.assertRaises(SystemExit) as result:
                    main([str(self.vault), "--database", "dedicated"])
                self.assertEqual(result.exception.code, 2)
                self.assertIn(error, stderr.getvalue())
                connect.assert_not_called()

    def test_refuses_symlink_files_and_ancestors(self):
        self.write("external.md", "outside", root=self.root)
        (self.vault / "shortcut.md").symlink_to(self.root / "external.md")
        with self.assertRaisesRegex(ValueError, "symlink"):
            prepare(self.vault)
        (self.vault / "shortcut.md").unlink()
        self.write("Intro.md", "inside")
        (self.vault / "external").symlink_to(self.root, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "symlink"):
            prepare(self.vault)
        (self.vault / "external").unlink()
        (self.root / "shortcut").symlink_to(self.root)
        with self.assertRaises(OSError):
            prepare(self.root / "shortcut" / "vault")

    def test_refuses_hard_links_and_oversize_input(self):
        self.write("external.md", "outside", root=self.root)
        os.link(self.root / "external.md", self.vault / "linked.md")
        with self.assertRaisesRegex(ValueError, "one hard link"):
            prepare(self.vault)
        (self.vault / "linked.md").unlink()
        self.write("size.md", "é" * (MAX_NOTE_BYTES // 2 + 1))
        with self.assertRaisesRegex(ValueError, "byte limit"):
            prepare(self.vault)

    def test_refuses_total_byte_and_reference_limits(self):
        with patch("examples.obsidian.import_vault.MAX_TOTAL_BYTES", 10):
            self.write("a.md", "a" * 6)
            self.write("b.md", "a" * 6)
            with self.assertRaisesRegex(ValueError, "byte limit"):
                prepare(self.vault)
        with patch("examples.obsidian.import_vault.MAX_REFERENCES", 2):
            self.write("a.md", "[[b]] [[b]] [[b]]")
            with self.assertRaisesRegex(ValueError, "wikilink references"):
                prepare(self.vault)


if __name__ == "__main__":
    unittest.main()
