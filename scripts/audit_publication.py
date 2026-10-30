#!/usr/bin/env python3
"""Read-only publication audit of Git index blobs and HEAD email metadata.

This heuristic detects selected credential formats and operator-configured private
terms/identities. It does not prove absence of leaks. Historical file contents,
commit messages, ignored/untracked files and unstaged working-tree changes are
not scanned. Stage intended publication files before running it. No findings
include matched values; filenames containing private terms or control characters
are hashed. Shallow history or repository changes during the scan make its
result incomplete. Exit 0: pass, 1: findings, 2: incomplete scan or operational error.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys


PATTERNS = {
    'private_key': re.compile(r'-----BEGIN (?:RSA |EC |DSA |OPENSSH |ENCRYPTED )?PRIVATE KEY-----'),
    'github_token': re.compile(r'(?<![A-Za-z0-9_])(?:gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{40,})(?![A-Za-z0-9_])'),
    'aws_access_key_id': re.compile(r'(?<![A-Z0-9])(?:AKIA|ASIA)[A-Z0-9]{16}(?![A-Z0-9])'),
    'slack_token': re.compile(r'(?<![A-Za-z0-9_-])xox[baprs]-[A-Za-z0-9-]{20,}(?![A-Za-z0-9_-])'),
    'openai_token': re.compile(r'(?<![A-Za-z0-9_-])sk-(?:proj-|svcacct-)?[A-Za-z0-9_-]{40,}(?![A-Za-z0-9_-])'),
}
LIMITS = [
    'Heuristic credential patterns cannot prove absence of leaks or token validity.',
    'Scans Git index blobs, not ignored/untracked files or unstaged working-tree changes.',
    'Only raw author/committer emails reachable from HEAD are checked for configured identities.',
    'Does not scan historical file contents, commit messages, remote branches or tags.',
    'Oversized blobs, submodules, unstaged tracked changes and untracked paths make the scan incomplete.',
    'Shallow history or detected HEAD/index/status changes make the scan incomplete; this is not an atomic Git snapshot.',
]


class AuditError(RuntimeError):
    pass


def git(root, *args):
    result = subprocess.run(['git', '--no-optional-locks', '-C', str(root), *args], capture_output=True)
    if result.returncode:
        # Git stderr can contain private paths, URLs or configuration values.
        raise AuditError('Git command failed')
    return result.stdout


def safe_path(path, terms):
    text = path.decode('utf-8', errors='replace')
    if (any(ord(char) < 32 or ord(char) == 127 for char in text)
            or '\ufffd' in text
            or any(term in text.casefold() for term in terms)
            or any(pattern.search(text) for pattern in PATTERNS.values())):
        digest = hashlib.sha256(path).hexdigest()[:16]
        return f'<redacted-path:{digest}>'
    return text


def repository_state(root):
    return (git(root, 'rev-parse', 'HEAD').strip(),
            git(root, 'ls-files', '--stage', '-z'),
            git(root, 'status', '--porcelain=v1', '-z', '--untracked-files=all'),
            git(root, 'rev-parse', '--is-shallow-repository').strip())


def text_findings(text, path, terms, location):
    findings = []
    for category, pattern in PATTERNS.items():
        count = sum(1 for _ in pattern.finditer(text))
        if count:
            findings.append({'path': path, 'category': category, 'count': count,
                             'location': location})
    count = sum(text.casefold().count(term) for term in terms)
    if count:
        findings.append({'path': path, 'category': 'forbidden_term', 'count': count,
                         'location': location})
    return findings


def consume(stream, size, keep):
    data = bytearray()
    left = size
    while left:
        chunk = stream.read(min(left, 64 * 1024))
        if not chunk:
            raise AuditError('Truncated Git blob output')
        if keep:
            data.extend(chunk)
        left -= len(chunk)
    if stream.read(1) != b'\n':
        raise AuditError('Malformed Git blob output')
    return bytes(data)


def audit(root, *, terms=(), emails=(), email_domains=(), max_file_bytes=8 * 1024 * 1024):
    terms = tuple(value.casefold() for value in terms)
    emails = frozenset(value.casefold() for value in emails)
    domains = frozenset(value.removeprefix('@').casefold() for value in email_domains)
    if max_file_bytes <= 0 or any(not value for value in (*terms, *emails, *domains)):
        raise AuditError('Invalid audit configuration')
    initial_state = repository_state(root)
    report = {'result': 'PASS', 'scope': 'Git index blobs and HEAD author/committer emails',
              'limits': LIMITS, 'files_checked': 0, 'findings': [],
              'history_email_matches': 0, 'unscanned_files': [],
              'history_is_shallow': initial_state[3] == b'true',
              'repository_state_changed': False}
    report['excluded_changes'] = {
        'unstaged_tracked_paths': len([value for value in git(root, 'diff', '--name-only', '-z').split(b'\0') if value]),
        'untracked_paths': len([value for value in git(root, 'ls-files', '--others', '--exclude-standard', '-z').split(b'\0') if value]),
    }
    metadata = git(root, 'log', initial_state[0].decode('ascii'), '--format=%ae%x00%ce%x00').decode('utf-8', errors='replace')
    for value in metadata.split('\0'):
        email = value.strip().casefold()
        domain = email.rpartition('@')[2] if '@' in email else ''
        if email and (email in emails or domain in domains):
            report['history_email_matches'] += 1
    entries = []
    for entry in initial_state[1].split(b'\0'):
        if not entry:
            continue
        try:
            info, path = entry.split(b'\t', 1)
            mode, oid, stage = info.split(b' ')
        except ValueError as error:
            raise AuditError('Malformed Git index output') from error
        if stage != b'0':
            raise AuditError('Unmerged Git index')
        label = safe_path(path, (*terms, *emails, *domains))
        report['findings'].extend(text_findings(
            path.decode('utf-8', errors='replace'), label, terms, 'path'))
        if mode == b'160000':
            report['unscanned_files'].append({'path': label, 'reason': 'submodule'})
        else:
            entries.append((oid, label))
    process = subprocess.Popen(['git', '--no-optional-locks', '-C', str(root), 'cat-file', '--batch'],
                               stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=subprocess.DEVNULL)
    try:
        for oid, label in entries:
            process.stdin.write(oid + b'\n')
            process.stdin.flush()
            header = process.stdout.readline().split()
            if len(header) != 3 or header[0] != oid or header[1] != b'blob':
                raise AuditError('Git index object is not an available blob')
            try:
                size = int(header[2])
            except ValueError as error:
                raise AuditError('Malformed Git blob size') from error
            if size < 0:
                raise AuditError('Invalid Git blob size')
            content = consume(process.stdout, size, size <= max_file_bytes)
            if size > max_file_bytes:
                report['unscanned_files'].append({'path': label, 'reason': 'size_limit'})
                continue
            report['files_checked'] += 1
            text = content.decode('utf-8', errors='replace')
            report['findings'].extend(text_findings(text, label, terms, 'content'))
        process.stdin.close()
        if process.wait(timeout=10):
            raise AuditError('Git blob reader failed')
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
        process.stdout.close()
        if not process.stdin.closed:
            process.stdin.close()
    report['repository_state_changed'] = repository_state(root) != initial_state
    if (report['unscanned_files'] or any(report['excluded_changes'].values())
            or report['history_is_shallow'] or report['repository_state_changed']):
        report['result'] = 'INCOMPLETE'
    elif report['findings'] or report['history_email_matches']:
        report['result'] = 'FAIL'
    return report


class RedactedParser(argparse.ArgumentParser):
    def error(self, message):
        # Do not echo argument values that might themselves be private.
        self.exit(2, 'Invalid command arguments; use --help.\n')


def main(argv=None):
    parser = RedactedParser(description=__doc__)
    parser.add_argument('--repo', type=Path, default=Path.cwd())
    parser.add_argument('--forbid-term', action='append', default=[], help='Case-insensitive private term; repeatable.')
    parser.add_argument('--forbid-email', action='append', default=[], help='Exact private history email; repeatable.')
    parser.add_argument('--forbid-email-domain', action='append', default=[], help='Exact history email domain; repeatable.')
    parser.add_argument('--max-file-bytes', type=int, default=8 * 1024 * 1024)
    args = parser.parse_args(argv)
    try:
        report = audit(args.repo, terms=args.forbid_term, emails=args.forbid_email,
                       email_domains=args.forbid_email_domain, max_file_bytes=args.max_file_bytes)
    except (AuditError, OSError, subprocess.SubprocessError):
        report = {'result': 'ERROR', 'error': 'Audit could not complete; no private diagnostic details are emitted.',
                  'limits': LIMITS}
    print(json.dumps(report, indent=2, ensure_ascii=True))
    return {'PASS': 0, 'FAIL': 1}.get(report['result'], 2)


if __name__ == '__main__':
    sys.exit(main())
