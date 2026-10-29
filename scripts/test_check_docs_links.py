import importlib.util
from pathlib import Path
import tempfile
import unittest


spec = importlib.util.spec_from_file_location("check_docs_links", Path(__file__).with_name("check_docs_links.py"))
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)


class MarkdownLinkTests(unittest.TestCase):
    def test_destinations_decode_spaces_and_parentheses(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "docs").mkdir()
            (root / "docs" / "hello world.md").write_text("# Hello")
            (root / "docs" / "name(test).md").write_text("# Test")
            readme = root / "README.md"
            readme.write_text('[one](docs/hello%20world.md#anything)\n[two](<docs/hello world.md> "title")\n[three](docs/name(test).md)\n[four](docs/name\\(test\\).md)')
            count, errors = checker.check_links(root, [readme])
            self.assertEqual(count, 4)
            self.assertEqual(errors, [])

    def test_missing_target_reports_source_and_line(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            document = root / "README.md"
            document.write_text('# Intro\n\n[Broken](docs/absent.md#section)\n')
            count, errors = checker.check_links(root, [document])
            self.assertEqual(count, 1)
            self.assertEqual(errors, ['README.md:3: missing local target: docs/absent.md#section'])

    def test_external_fragment_and_code_examples_are_ignored(self):
        text = '''[external](https://example.test/missing)
[mail](mailto:reader@example.test)
[anchor](#missing)
```md
[example](absent.md)
```
~~~
[example](absent2.md)
~~~
`[inline](absent3.md)`
[real](exists.md)
'''
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "exists.md").write_text("")
            document = root / "README.md"
            document.write_text(text)
            count, errors = checker.check_links(root, [document])
            self.assertEqual(count, 1)
            self.assertEqual(errors, [])
        self.assertEqual(list(checker.inline_links(text))[-1], (11, "exists.md"))

    def test_image_asset_and_root_relative_link_are_checked(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "docs").mkdir()
            (root / "assets").mkdir()
            (root / "assets" / "image.png").write_bytes(b"preview")
            document = root / "docs" / "guide.md"
            document.write_text('![image](../assets/image.png)\n[root](/assets/image.png)\n![missing](missing.svg)')
            count, errors = checker.check_links(root, [document])
            self.assertEqual(count, 3)
            self.assertEqual(errors, ['docs/guide.md:3: missing local target: missing.svg'])

    def test_parent_escape_is_rejected_even_if_path_exists(self):
        with tempfile.TemporaryDirectory() as temporary:
            parent = Path(temporary)
            root = parent / "repo"
            root.mkdir()
            (parent / "other.md").write_text("")
            document = root / "README.md"
            document.write_text('[outside](../other.md)')
            _, errors = checker.check_links(root, [document])
            self.assertIn('path leaves repository', errors[0])

    def test_missing_or_unreadable_markdown_is_an_error(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            bad = root / "bad.md"
            bad.write_bytes(b"\xff")
            _, errors = checker.check_links(root, [root / "missing.md", bad])
            self.assertEqual(len(errors), 2)
            self.assertIn('cannot read Markdown', errors[0])
            self.assertIn('UnicodeDecodeError', errors[1])


if __name__ == '__main__':
    unittest.main()
