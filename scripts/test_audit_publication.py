import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


SCRIPT = Path(__file__).with_name('audit_publication.py')
spec = importlib.util.spec_from_file_location('audit_publication', SCRIPT)
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)


class PublicationAuditTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.git('init', '-q')
        self.git('config', 'user.name', 'Synthetic Author')
        self.git('config', 'user.email', 'author@example.test')

    def git(self, *args):
        return subprocess.run(['git', '-C', str(self.root), *args], check=True,
                              capture_output=True).stdout

    def commit(self, files):
        for name, value in files.items():
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(value if isinstance(value, bytes) else value.encode())
        self.git('add', '--all')
        self.git('commit', '-qm', 'Synthetic fixture')

    def audit(self, *args):
        result = subprocess.run([sys.executable, str(SCRIPT), '--repo', str(self.root),
                                 *args], text=True, capture_output=True)
        self.assertEqual(result.stderr, '')
        return result.returncode, json.loads(result.stdout), result.stdout

    def test_clean_repository_passes_and_documents_scope(self):
        self.commit({'README.md': 'A small graph project.'})
        code, report, _ = self.audit()
        self.assertEqual(code, 0)
        self.assertEqual(report['result'], 'PASS')
        self.assertEqual(report['files_checked'], 1)
        self.assertEqual(report['findings'], [])
        self.assertTrue(any('historical file contents' in value for value in report['limits']))

    def test_credentials_are_reported_without_values(self):
        token = 'ghp_' + 'A' * 36
        key = '-----BEGIN ' + 'PRIVATE KEY-----'
        self.commit({'config.txt': token + '\n' + key})
        code, report, output = self.audit()
        self.assertEqual(code, 1)
        self.assertEqual({item['category'] for item in report['findings']},
                         {'github_token', 'private_key'})
        self.assertNotIn(token, output)
        self.assertNotIn(key, output)

    def test_terms_and_history_identity_are_case_insensitive_and_redacted(self):
        self.git('config', 'user.email', 'operator@private.example.test')
        self.commit({'docs.md': 'PRIVATE-PROJECT private-project'})
        code, report, output = self.audit('--forbid-term', 'private-project',
                                          '--forbid-email-domain', 'private.example.test')
        self.assertEqual(code, 1)
        self.assertEqual(report['history_email_matches'], 2)
        self.assertEqual(report['findings'][0]['count'], 2)
        self.assertNotIn('private-project', output)
        self.assertNotIn('private.example.test', output)

    def test_email_domain_is_exact_and_exact_email_is_supported(self):
        self.commit({'readme': 'okay'})
        code, report, _ = self.audit('--forbid-email-domain', 'example.test.evil')
        self.assertEqual(code, 0)
        self.assertEqual(report['history_email_matches'], 0)
        code, report, output = self.audit('--forbid-email', 'author@example.test')
        self.assertEqual(code, 1)
        self.assertEqual(report['history_email_matches'], 2)
        self.assertNotIn('author@example.test', output)

    def test_ignored_untracked_and_worktree_changes_are_not_scanned(self):
        self.commit({'tracked.txt': 'safe', '.gitignore': 'ignored.txt\n'})
        secret = 'ghp_' + 'B' * 36
        for name in ('tracked.txt', 'ignored.txt', 'untracked.txt'):
            (self.root / name).write_text(secret)
        code, report, _ = self.audit()
        self.assertEqual(code, 2)
        self.assertEqual(report['result'], 'INCOMPLETE')
        self.assertEqual(report['files_checked'], 2)
        self.assertEqual(report['excluded_changes'], {'unstaged_tracked_paths': 1, 'untracked_paths': 1})
        self.assertEqual(report['findings'], [])

    def test_symlink_does_not_read_outside_repository(self):
        outside = self.root.parent / (self.root.name + '-secret')
        outside.write_text('ghp_' + 'C' * 36)
        self.addCleanup(outside.unlink)
        (self.root / 'link').symlink_to(outside)
        self.git('add', 'link')
        self.git('commit', '-qm', 'Synthetic symlink')
        code, report, _ = self.audit()
        self.assertEqual(code, 0)
        self.assertEqual(report['findings'], [])

    def test_sensitive_filename_is_hashed_and_control_characters_are_hidden(self):
        self.commit({'private-project\nname.txt': 'private-project'})
        code, report, output = self.audit('--forbid-term', 'private-project')
        self.assertEqual(code, 1)
        self.assertTrue(report['findings'][0]['path'].startswith('<redacted-path:'))
        self.assertNotIn('private-project', output)
        self.assertNotIn('name.txt', output)

    def test_filename_only_private_term_is_a_finding_without_leaking_path(self):
        self.commit({'private-project.txt': 'ordinary content'})
        code, report, output = self.audit('--forbid-term', 'private-project')
        self.assertEqual(code, 1)
        finding = report['findings'][0]
        self.assertEqual((finding['category'], finding['location'], finding['count']),
                         ('forbidden_term', 'path', 1))
        self.assertTrue(finding['path'].startswith('<redacted-path:'))
        self.assertNotIn('private-project', output)

    def test_filename_only_token_is_a_finding_without_leaking_path(self):
        token = 'ghp_' + 'G' * 36
        self.commit({token + '.txt': 'ordinary content'})
        code, report, output = self.audit()
        self.assertEqual(code, 1)
        finding = report['findings'][0]
        self.assertEqual((finding['category'], finding['location'], finding['count']),
                         ('github_token', 'path', 1))
        self.assertTrue(finding['path'].startswith('<redacted-path:'))
        self.assertNotIn(token, output)

    def test_large_file_is_incomplete_and_following_blob_is_still_scanned(self):
        secret = 'ghp_' + 'D' * 36
        self.commit({'a-large.bin': b'X' * 1025, 'z-token.txt': secret})
        code, report, output = self.audit('--max-file-bytes', '1024')
        self.assertEqual(code, 2)
        self.assertEqual(report['result'], 'INCOMPLETE')
        self.assertEqual(len(report['unscanned_files']), 1)
        self.assertTrue(any(item['category'] == 'github_token' for item in report['findings']))
        self.assertNotIn(secret, output)

    def test_history_is_head_only_and_index_is_scanned(self):
        self.commit({'tracked': 'safe'})
        self.git('checkout', '-qb', 'other')
        self.git('config', 'user.email', 'hidden@other.example.test')
        self.commit({'other': 'safe'})
        self.git('checkout', '-q', '-')
        token = 'ghp_' + 'E' * 36
        (self.root / 'staged').write_text(token)
        self.git('add', 'staged')
        code, report, _ = self.audit('--forbid-email-domain', 'other.example.test')
        self.assertEqual(code, 1)
        self.assertEqual(report['history_email_matches'], 0)
        self.assertTrue(any(item['category'] == 'github_token' for item in report['findings']))

    def test_history_identity_in_finding_path_is_redacted(self):
        token = 'ghp_' + 'F' * 36
        self.commit({'author@example.test.txt': token})
        code, report, output = self.audit('--forbid-email', 'author@example.test')
        self.assertEqual(code, 1)
        self.assertTrue(report['findings'][0]['path'].startswith('<redacted-path:'))
        self.assertNotIn('author@example.test', output)
        self.assertNotIn(token, output)

    def test_invalid_command_arguments_do_not_echo_values(self):
        private = 'private-option-value'
        result = subprocess.run([sys.executable, str(SCRIPT), '--unknown', private],
                                text=True, capture_output=True)
        self.assertEqual(result.returncode, 2)
        self.assertNotIn(private, result.stdout + result.stderr)

    def test_shallow_history_cannot_pass(self):
        self.commit({'safe': 'safe'})
        with tempfile.TemporaryDirectory() as temporary:
            clone = Path(temporary) / 'clone'
            subprocess.run(['git', 'clone', '-q', '--depth', '1', self.root.as_uri(),
                            str(clone)], check=True, capture_output=True)
            result = subprocess.run([sys.executable, str(SCRIPT), '--repo', str(clone)],
                                    text=True, capture_output=True)
            report = json.loads(result.stdout)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(report['result'], 'INCOMPLETE')
            self.assertTrue(report['history_is_shallow'])

    def assert_change_during_scan_is_incomplete(self, change):
        self.commit({'tracked': 'safe'})
        consume = checker.consume
        changed = False

        def consume_and_change(*args):
            nonlocal changed
            content = consume(*args)
            if not changed:
                change()
                changed = True
            return content

        with mock.patch.object(checker, 'consume', side_effect=consume_and_change):
            report = checker.audit(self.root)
        self.assertEqual(report['result'], 'INCOMPLETE')
        self.assertTrue(report['repository_state_changed'])

    def test_staged_change_during_scan_cannot_pass(self):
        def change():
            (self.root / 'tracked').write_text('changed')
            self.git('add', 'tracked')
        self.assert_change_during_scan_is_incomplete(change)

    def test_head_change_during_scan_cannot_pass(self):
        self.assert_change_during_scan_is_incomplete(
            lambda: self.git('commit', '-q', '--allow-empty', '-m', 'Synthetic update'))

    def test_dirty_status_change_during_scan_cannot_pass(self):
        self.assert_change_during_scan_is_incomplete(
            lambda: (self.root / 'new-untracked').write_text('safe'))

    def test_git_errors_are_redacted(self):
        self.commit({'safe': 'safe'})
        self.git('checkout', '--orphan', 'empty')
        code, report, output = self.audit()
        self.assertEqual(code, 2)
        self.assertEqual(report['result'], 'ERROR')
        self.assertNotIn(str(self.root), output)


if __name__ == '__main__':
    unittest.main()
