#!/usr/bin/env python3
"""Verify the official v0.11 binary refuses a new experimental ledger store.

Uses one local immutable release image, an explicitly supplied candidate binary,
and private synthetic directories. Starts only UUID-labelled offline containers;
no ports, pulls, builds or existing graph paths are used. An ordinary-store check
must pass first, so unrelated runtime/config failures cannot pass the downgrade
gate. This verifies format refusal, not supported downgrade or restore semantics.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import uuid

IMAGE = 'ghcr.io/allandefraga/brainwire@sha256:5368f22f991f612e93eb4ca3ee2952347afa04abcee87a268dfa73a5fccc3c02'
SOURCE = '86e1e860c44288b4e4170b785d7ca76e31d4cb4e'
LABEL = 'brainwire.ledger-downgrade'
GRAPH = 'probe'
VERSION = 'brainwire 0.11.0-beta.1'


def command(args, *, env=None, cwd=None, timeout=45):
    return subprocess.run(args, env=env, cwd=cwd, text=True,
                          capture_output=True, timeout=timeout)


def checked(args, **kwargs):
    result = command(args, **kwargs)
    if result.returncode:
        raise RuntimeError(f'owned command failed: {args[0]}: {result.stderr[-2000:]}')
    return result.stdout.strip()


def expected_refusal(result):
    text = (result.stdout + '\n' + result.stderr).lower()
    return (result.returncode != 0 and 'request_ledger_v1' in text
            and 'column famil' in text and ('not opened' in text or 'not open' in text))


def owned(inspect, identity):
    return inspect.get('Config', {}).get('Labels', {}).get(LABEL) == identity


def cleanup(name, identity):
    result = command(['docker', 'inspect', name])
    if result.returncode:
        if 'no such' in result.stderr.lower():
            return
        raise RuntimeError('cannot establish owned container absence')
    value = json.loads(result.stdout)[0]
    if not owned(value, identity):
        raise RuntimeError('refusing to remove an unowned container')
    checked(['docker', 'rm', '--force', name])
    absent = command(['docker', 'inspect', name])
    if absent.returncode == 0 or 'no such' not in absent.stderr.lower():
        raise RuntimeError('owned container cleanup not confirmed')


def image_info():
    value = json.loads(checked(['docker', 'image', 'inspect', IMAGE]))[0]
    if IMAGE not in value.get('RepoDigests', []):
        raise RuntimeError('official immutable image digest not available locally')
    if value.get('Architecture') != 'amd64' or value.get('Os') != 'linux':
        raise RuntimeError('unexpected previous-image architecture or OS')
    revision = value.get('Config', {}).get('Labels', {}).get('org.opencontainers.image.revision')
    if revision != SOURCE:
        raise RuntimeError('previous image source label differs from v0.11 release')
    return {'reference': IMAGE, 'id': value['Id'], 'architecture': value['Architecture'],
            'os': value['Os'], 'source': revision}


def old_command(args, identity, data=None):
    name = f'bw-ledger-downgrade-{identity}-{uuid.uuid4().hex[:6]}'
    invocation = ['docker', 'run', '--pull=never', '--platform=linux/amd64',
                  '--name', name, '--label', f'{LABEL}={identity}',
                  '--network=none', '--read-only', '--cap-drop=ALL',
                  '--security-opt=no-new-privileges', f'--user={os.getuid()}:{os.getgid()}',
                  '--memory=256m', '--memory-swap=256m', '--cpus=1',
                  '--pids-limit=128', '--tmpfs', '/tmp:rw,size=16m']
    if data is not None:
        invocation += ['--mount', f'type=bind,src={data},dst=/data',
                       '--env', 'BRAINWIRE_SERVER_DATA_DIR=/data',
                       '--env', f'BRAINWIRE_SERVER_DEFAULT_DATABASE={GRAPH}',
                       '--env', 'BRAINWIRE_MEMORY_CACHE=64MiB',
                       '--env', 'BRAINWIRE_MEMORY_MEMTABLES=16MiB',
                       '--env', 'BRAINWIRE_MEMORY_QUERY=16MiB']
    invocation += [IMAGE, *args]
    try:
        result = command(invocation, timeout=60)
        state = json.loads(checked(['docker', 'inspect', name]))[0]
        if not owned(state, identity):
            raise RuntimeError('container ownership changed')
        if state['State'].get('Running') or state['State'].get('OOMKilled') or state.get('RestartCount', 0):
            raise RuntimeError('previous image did not complete normally')
        if result.returncode != state['State']['ExitCode']:
            raise RuntimeError('Docker client and previous process disagree on exit status')
        return result
    finally:
        cleanup(name, identity)


def environment():
    env = {k: v for k, v in os.environ.items() if not k.startswith('BRAINWIRE_')}
    env.update(BRAINWIRE_MEMORY_CACHE='64MiB', BRAINWIRE_MEMORY_MEMTABLES='16MiB',
               BRAINWIRE_MEMORY_QUERY='16MiB')
    return env


def candidate_config(root, data, ledger):
    path = root / ('ledger.toml' if ledger else 'ordinary.toml')
    policy = root / 'policy.json'
    if not policy.exists():
        policy.write_text(json.dumps({'principals': [{'id': 'synthetic-admin',
            'token_sha256': hashlib.sha256(b'synthetic-unused-offline-token').hexdigest(),
            'graphs': {GRAPH: 'write'}, 'instance_admin': True}]}))
        policy.chmod(0o600)
    path.write_text('[server]\n' + f'data_dir = {json.dumps(str(data))}\n'
                    + f'default_database = "{GRAPH}"\n'
                    + f'auth_file = {json.dumps(str(policy))}\n'
                    + ('request_ledger = true\n' if ledger else ''))
    path.chmod(0o600)
    return path


def audit(binary, config, root, env):
    result = checked([str(binary), 'db', 'check', GRAPH, '--json', '--config', str(config)],
                     cwd=root, env=env)
    value = json.loads(result)
    if value.get('status') != 'ok':
        raise RuntimeError('candidate invariant audit failed')
    return value


def run(binary):
    binary = binary.resolve(strict=True)
    report = {'candidate_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'candidate_version': checked([str(binary), '--version']),
              'previous_image': image_info(), 'synthetic': True,
              'scope': 'official v0.11 offline format refusal; not supported downgrade/restore/power-loss'}
    identity = uuid.uuid4().hex
    with tempfile.TemporaryDirectory(prefix='bw-ledger-downgrade-') as directory:
        root = Path(directory).resolve()
        env = environment()
        # The prior binary receives only disposable copies of the graph directories.
        root.chmod(0o755)
        ontology = root / 'schema.json'
        ontology.write_text(json.dumps({'version': '1', 'nodes': [
            {'label': 'Probe', 'properties': ['value'], 'indexed_fields': ['value']}], 'edges': []}))
        ordinary, ledger = root / 'ordinary', root / 'ledger'
        ordinary_config = candidate_config(root, ordinary, False)
        ledger_config = candidate_config(root, ledger, True)
        for config in (ordinary_config, ledger_config):
            checked([str(binary), 'db', 'create', GRAPH, '--ontology', str(ontology),
                     '--config', str(config)], cwd=root, env=env)
            audit(binary, config, root, env)
        version = old_command(['--version'], identity)
        if version.returncode or version.stdout.strip() != VERSION:
            raise RuntimeError('previous binary version differs from v0.11 release')
        ordinary_copy, ledger_copy = root / 'ordinary-copy', root / 'ledger-copy'
        shutil.copytree(ordinary, ordinary_copy)
        shutil.copytree(ledger, ledger_copy)
        old_plain = old_command(['db', 'check', GRAPH, '--json'], identity, ordinary_copy)
        if old_plain.returncode or json.loads(old_plain.stdout).get('status') != 'ok':
            raise RuntimeError('ordinary-store control failed on the previous binary')
        sequence_before = checked([str(binary), 'db', 'commit-state', GRAPH, '--json', '--config', str(ledger_config)], cwd=root, env=env)
        refused = old_command(['db', 'check', GRAPH, '--json'], identity, ledger_copy)
        if not expected_refusal(refused):
            raise RuntimeError('previous binary did not refuse specifically for the ledger column family: '
                               + refused.stderr[-1500:])
        # Point only the candidate at the disposable graph the old binary tried.
        reopened_config = root / 'reopened.toml'
        reopened_config.write_text(ledger_config.read_text().replace(str(ledger), str(ledger_copy)))
        reopened_config.chmod(0o600)
        audited = audit(binary, reopened_config, root, env)
        sequence_after = checked([str(binary), 'db', 'commit-state', GRAPH, '--json', '--config', str(reopened_config)], cwd=root, env=env)
        if json.loads(sequence_before) != json.loads(sequence_after):
            raise RuntimeError('previous open attempt changed the tracked sequence')
        report.update(result='PASS', previous_version=version.stdout.strip(),
                      ordinary_control='PASS', ledger_refusal='request_ledger_v1 not opened',
                      candidate_reopen_audit=audited, sequence_unchanged=True,
                      owned_resources_removed=True)
    return report


def self_check():
    for text in ('permission denied', 'unknown configuration key request_ledger', 'OOMKilled'):
        assert not expected_refusal(subprocess.CompletedProcess([], 1, '', text))
    assert not expected_refusal(subprocess.CompletedProcess([], 0, '', 'Column families not opened: request_ledger_v1'))
    assert expected_refusal(subprocess.CompletedProcess([], 1, '', 'Column families not opened: request_ledger_v1'))
    assert owned({'Config': {'Labels': {LABEL: 'x'}}}, 'x')
    assert not owned({'Config': {'Labels': {LABEL: 'other'}}}, 'x')
    assert '@sha256:' in IMAGE and len(SOURCE) == 40
    return {'self_check': 'PASS', 'runtime_exercised': False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path)
    parser.add_argument('--self-check', action='store_true')
    args = parser.parse_args()
    if args.self_check:
        print(json.dumps(self_check(), indent=2))
    elif args.binary is None:
        parser.error('--binary is required unless --self-check is used')
    else:
        print(json.dumps(run(args.binary), indent=2))


if __name__ == '__main__':
    main()
