#!/usr/bin/env python3
"""Compare active COMMIT admission on two local images in a Linux cgroup-v2 VM.

Run separately for each already-built image, with the same local Docker socket:
  python crates/server/tests/cgroup_active_commit.py --docker-host unix:///path/docker.sock --image BASELINE --expect commit --output before.json
  python crates/server/tests/cgroup_active_commit.py --docker-host unix:///path/docker.sock --image CANDIDATE --expect rollback --output after.json

The test deliberately fills a private tmpfs within one 512 MiB cgroup. Use an
isolated Docker daemon with free capacity. Only synthetic data is involved.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import sys
import tempfile
import time
import uuid

from cgroup_pressure_smoke import (
    DEFAULT_MEMORY_MIB, LABEL, LIMIT, PRESSURE_CODE, SAFETY_PERCENT,
    PersistentCgroupObserver, SmokeInconclusive, docker, fill_to_pressure,
    finalize, inspect_owned, pressured,
)

DATABASE = 'demo'
ROWS = 32
RSS_ADMISSION_DISABLED = 'BRAINWIRE_MEMORY_LIMIT=0'
ACK_ID = 'acknowledged'
BEFORE, AFTER = 'before@example.test', 'after@example.test'
PAYLOAD = 'x' * 1024
COMMIT_REFUSAL = re.compile(
    r'\[brainwire\.memory reason=process_limit requested_bytes=([1-9][0-9]{0,19}) '
    r'available_bytes=0 rolled_back=true\] \[brainwire\.commit outcome=rolled_back\]\Z'
)
DOCKER_ENDPOINT_OVERRIDES = ('DOCKER_CONTEXT', 'DOCKER_TLS', 'DOCKER_TLS_VERIFY', 'DOCKER_CERT_PATH')


def use_local_socket(endpoint):
    if not endpoint.startswith('unix:///') or endpoint == 'unix:///' or endpoint[8:9] == '/':
        raise ValueError('Docker endpoint must be a local Unix socket (unix:///absolute/path)')
    try:
        path = Path(endpoint[len('unix://'):]).resolve(strict=True)
        if not stat.S_ISSOCK(path.stat().st_mode):
            raise ValueError('Docker endpoint is not a Unix socket')
    except (OSError, RuntimeError) as error:
        raise ValueError('Docker Unix socket is unavailable') from error
    # All subprocesses in the smoke helpers inherit this explicit endpoint.
    for key in DOCKER_ENDPOINT_OVERRIDES:
        os.environ.pop(key, None)
    os.environ['DOCKER_HOST'] = f'unix://{path}'


def refused_growth(error):
    message = getattr(error, 'message', None)
    match = COMMIT_REFUSAL.search(message) if isinstance(message, str) else None
    if getattr(error, 'code', None) != PRESSURE_CODE or not match or int(match[1]) >= 1 << 64:
        raise AssertionError('COMMIT did not report refused query growth and confirmed rollback') from error
    return int(match[1])


def sample(observer, target, under_pressure):
    observed = observer.sample(target)
    if (observed['memory.max'] != LIMIT or observed['memory.swap.max'] != 0
            or observed['memory.high'] not in ('max', LIMIT)):
        raise SmokeInconclusive('required cgroup-v2 memory limit or disabled swap unavailable')
    if any(observed['memory.events'].get(key, 0) for key in ('oom', 'oom_kill', 'oom_group_kill')):
        raise AssertionError('cgroup reported an OOM event')
    if observed['memory.current'] > LIMIT * SAFETY_PERCENT // 100:
        raise SmokeInconclusive('cgroup exceeded the injection safety margin')
    if pressured(observed) is not under_pressure:
        raise SmokeInconclusive('cgroup pressure did not match the current test phase')
    return observed


def stage(tx, ids):
    tx.run('UNWIND $ids AS id CREATE (:Customer {entity_id:id,name:$payload})',
           ids=ids, payload=PAYLOAD).consume()
    tx.run('MATCH (n:Customer {entity_id:$id}) SET n.email=$email', id=ACK_ID, email=AFTER).consume()
    tx.run('UNWIND $ids AS id MATCH (n:Customer {entity_id:id}), (c:Company {entity_id:"root"}) '
           'CREATE (n)-[:WORKS_AT]->(c)', ids=ids).consume()


def verify(session, ids, committed):
    expected = ids if committed else []
    nodes = session.run('MATCH (n:Customer) WHERE n.entity_id IN $ids '
                        'RETURN n.entity_id AS id, n.name AS name ORDER BY id', ids=ids).data()
    edges = session.run('MATCH (n:Customer)-[:WORKS_AT]->(c:Company) WHERE n.entity_id IN $ids '
                        'RETURN n.entity_id AS id, c.entity_id AS target ORDER BY id', ids=ids).data()
    assert nodes == [{'id': item, 'name': PAYLOAD} for item in expected], 'unexpected node state after COMMIT'
    assert edges == [{'id': item, 'target': 'root'} for item in expected], 'unexpected edge state after COMMIT'
    for value in (BEFORE, AFTER):
        found = session.run('MATCH (n:Customer {email:$email}) RETURN n.entity_id AS id', email=value).data()
        expected_id = [{'id': ACK_ID}] if value == (AFTER if committed else BEFORE) else []
        assert found == expected_id, 'unexpected indexed overwrite or lost acknowledged row'


def exercise(args, report, observer, target, fill, driver):
    ids = [f'pending-{index:03d}' for index in range(ROWS)]
    report['healthy'] = sample(observer, target, False)
    with driver.session(database=DATABASE) as session:
        session.run('CREATE (:Customer {entity_id:$id,email:$email})', id=ACK_ID, email=BEFORE).consume()
        session.run('CREATE (:Company {entity_id:"root"})').consume()
        with session.begin_transaction(timeout=120) as tx:
            stage(tx, ids)
            report['staged'] = sample(observer, target, False)
            report['injection'] = []
            docker('exec', target, 'touch', fill, timeout=10)
            fill_to_pressure(target, fill, report['injection'], sample=observer.sample)
            # Leave several sampler periods after reaching the threshold.
            # Bracket COMMIT so it cannot pass on evidence of pressure only before it.
            report['pressure_before_commit'] = []
            for _ in range(4):
                time.sleep(.1)
                report['pressure_before_commit'].append(sample(observer, target, True))
            try:
                tx.commit()
            except Exception as error:
                report['requested_bytes'] = refused_growth(error)
                report['commit'] = 'rolled_back'
            else:
                report['commit'] = 'committed'
            finally:
                try:
                    report['pressure_after_commit'] = sample(observer, target, True)
                finally:
                    docker('exec', target, 'rm', '--', fill, timeout=10)
        report['relief'] = sample(observer, target, False)
        # A false failure is possible immediately after relief as the sampler
        # may still hold the pressured reading. Wait for a new reservation.
        deadline = time.monotonic() + 10
        while True:
            try:
                session.run('RETURN 1 AS ready').consume()
                break
            except Exception as error:
                if getattr(error, 'code', None) != PRESSURE_CODE or time.monotonic() >= deadline:
                    raise
                time.sleep(.1)
        committed = report['commit'] == 'committed'
        verify(session, ids, committed)
        assert committed == (args.expect == 'commit'), 'COMMIT outcome differs from image expectation'
        if not committed:
            with session.begin_transaction(timeout=20) as retry:
                stage(retry, ids)
                retry.commit()
            verify(session, ids, True)
            report['whole_transaction_retry'] = 'PASS'
    report['post_relief'] = sample(observer, target, False)


def run(args, report):
    from neo4j import GraphDatabase, __version__ as driver_version

    run_id = uuid.uuid4().hex
    name = 'brainwire-active-commit-' + run_id
    volume, fill = name + '-data', '/pressure/fill-' + run_id
    metadata = json.loads(docker('image', 'inspect', '--format', '{{json .}}', args.image))
    image_id = metadata.get('Id', '')
    if not re.fullmatch(r'sha256:[0-9a-f]{64}', image_id):
        raise RuntimeError('local image did not resolve to one immutable ID')
    labels = metadata.get('Config', {}).get('Labels') or {}
    revision = labels.get('org.opencontainers.image.revision')
    runner = Path(__file__).resolve()
    report.update(image_id=image_id,
                  image_revision=revision if isinstance(revision, str) and re.fullmatch(r'[0-9a-f]{7,64}', revision) else None,
                  docker_endpoint_kind='local Unix socket', bolt_endpoint_scope='IPv4 loopback',
                  runner_sha256=hashlib.sha256(runner.read_bytes()).hexdigest(),
                  helper_sha256=hashlib.sha256(runner.with_name('cgroup_pressure_smoke.py').read_bytes()).hexdigest(),
                  driver=driver_version, expect=args.expect,
                  memory_limit_bytes=LIMIT, rows=ROWS, payload_bytes=len(PAYLOAD),
                  scope='synthetic single-container cgroup-v2 pressure across an active explicit transaction',
                  limitations=['external cgroup samples bracket COMMIT; exact decision-time usage is unobserved',
                               'one bounded pressure episode; not sustained capacity or an RSS ceiling'],
                  rss_admission_disabled=True)
    common = ['--pull=never', '--label', f'{LABEL}={run_id}', f'--memory={DEFAULT_MEMORY_MIB}m',
              f'--memory-swap={DEFAULT_MEMORY_MIB}m', '--cpus=1', '--no-healthcheck',
              '--mount', f'type=volume,src={volume},dst=/data',
              '-e', f'BRAINWIRE_SERVER_DEFAULT_DATABASE={DATABASE}', '-e', RSS_ADMISSION_DISABLED,
              '-e', 'BRAINWIRE_MEMORY_CACHE=64MiB', '-e', 'BRAINWIRE_MEMORY_MEMTABLES=8MiB',
              '-e', 'BRAINWIRE_MEMORY_QUERY=32MiB', '-e', 'BRAINWIRE_MEMORY_INGRESS=16MiB']
    with tempfile.TemporaryDirectory(prefix='bw-active-commit-') as temporary:
        events_path = Path(temporary) / 'unused-events.jsonl'
        try:
            if inspect_owned('volume', volume, run_id) is not None:
                raise RuntimeError('test volume already exists')
            docker('volume', 'create', '--label', f'{LABEL}={run_id}', volume)
            if inspect_owned('volume', volume, run_id) is None:
                raise RuntimeError('owned volume was not created')
            docker('run', '--rm', '--network=none', '--name', name + '-init', *common, image_id,
                   'db', 'create', DATABASE, '--ontology', '/opt/brainwire/example-schema.json', timeout=120)
            docker('run', '-d', '--name', name, *common,
                   '--tmpfs', f'/pressure:rw,size={DEFAULT_MEMORY_MIB}m,mode=1777',
                   '-p', '127.0.0.1::7687', image_id, 'serve', '--allow-unauthenticated-remote')
            state = inspect_owned('container', name, run_id)
            if state is None or state['Image'] != image_id:
                raise RuntimeError('owned container does not match the requested image')
            target = state['Id']
            ports = state['NetworkSettings']['Ports']['7687/tcp']
            if len(ports) != 1 or ports[0]['HostIp'] != '127.0.0.1':
                raise RuntimeError('owned container must publish Bolt only to IPv4 loopback')
            port = int(ports[0]['HostPort'])
            if not 1 <= port <= 65535:
                raise RuntimeError('owned container did not report a valid Bolt port')
            with GraphDatabase.driver(f'bolt://127.0.0.1:{port}', auth=None, connection_timeout=3,
                                      max_transaction_retry_time=0) as driver:
                deadline = time.monotonic() + 20
                while True:
                    try:
                        driver.verify_connectivity()
                        break
                    except Exception:
                        if time.monotonic() >= deadline:
                            raise
                        time.sleep(.1)
                observer = PersistentCgroupObserver(name, run_id)
                try:
                    exercise(args, report, observer, target, fill, driver)
                finally:
                    observer.close()
            state = inspect_owned('container', target, run_id)['State']
            if state.get('OOMKilled') or not state.get('Running'):
                raise AssertionError('owned server stopped or Docker reports OOMKilled')
            docker('stop', '--time', '20', target, timeout=30)
            report['offline_audit'] = json.loads(docker('run', '--rm', '--network=none',
                '--name', name + '-audit', *common, image_id, 'db', 'check', DATABASE, '--json', timeout=120))
            assert report['offline_audit']['status'] == 'ok', 'offline graph audit failed'
            report['result'] = 'PASS'
        finally:
            finalize(report, run_id, name, volume, False, None, events_path)


def self_check():
    from unittest.mock import patch

    # ByteSize accepts plain byte counts without a suffix.
    assert RSS_ADMISSION_DISABLED.partition('=')[2] == '0'
    conflicting_env = dict.fromkeys(DOCKER_ENDPOINT_OVERRIDES, 'remote')
    conflicting_env['DOCKER_HOST'] = 'ssh://remote'
    with patch.dict(os.environ, conflicting_env), \
            patch.object(Path, 'resolve', return_value=Path('/tmp/docker.sock')), \
            patch.object(Path, 'stat') as path_stat:
        path_stat.return_value.st_mode = stat.S_IFSOCK
        for host in ('ssh://remote', 'tcp://127.0.0.1:2375', 'unix://relative', 'unix:///'):
            try:
                use_local_socket(host)
            except ValueError:
                assert all(os.environ[key] == value for key, value in conflicting_env.items())
            else:
                raise AssertionError('accepted a non-local Docker endpoint')
        use_local_socket('unix:///tmp/docker.sock')
        assert os.environ['DOCKER_HOST'] == 'unix:///tmp/docker.sock'
        assert all(key not in os.environ for key in DOCKER_ENDPOINT_OVERRIDES)
    hint = '[brainwire.memory reason=process_limit requested_bytes=9 available_bytes=0 rolled_back=true]'
    suffix = ' [brainwire.commit outcome=rolled_back]'
    exception = RuntimeError('fixture')
    exception.code = PRESSURE_CODE
    exception.message = hint + suffix
    assert refused_growth(exception) == 9
    for message in (hint, hint + suffix.replace('rolled_back', 'unknown'),
                    (hint + suffix).replace('process_limit', 'ingress'),
                    (hint + suffix).replace('requested_bytes=9', 'requested_bytes=0')):
        exception.message = message
        try:
            refused_growth(exception)
        except AssertionError:
            continue
        raise AssertionError('accepted an ambiguous or unrelated refusal')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--docker-host')
    parser.add_argument('--image')
    parser.add_argument('--expect', choices=('commit', 'rollback'))
    parser.add_argument('--output', type=Path)
    parser.add_argument('--self-check', action='store_true')
    args = parser.parse_args()
    if args.self_check:
        self_check()
        return 0
    if not args.docker_host or not args.image or not args.expect or args.output is None:
        parser.error('--docker-host, --image, --expect and --output are required')
    try:
        use_local_socket(args.docker_host)
    except ValueError as error:
        parser.error(str(error))
    report = {'result': 'FAIL'}
    with args.output.open('x', encoding='utf-8') as output:
        try:
            run(args, report)
        except SmokeInconclusive as error:
            state = report.get('cleanup_state', {})
            events = (report.get('cleanup_cgroup') or {}).get('memory.events') or {}
            oom = state.get('OOMKilled') or any(events.get(key, 0) for key in ('oom', 'oom_kill', 'oom_group_kill'))
            result = 'FAIL' if oom or not report.get('owned_resources_cleaned') else 'INCONCLUSIVE'
            report.update(result=result, error_type=type(error).__name__, error=str(error))
        except Exception as error:
            report.update(result='FAIL', error_type=type(error).__name__)
            print(str(error), file=sys.stderr)
        finally:
            if 'cleanup_state' in report:
                state = report['cleanup_state']
                report['cleanup_state'] = {key: state.get(key) for key in ('Running', 'OOMKilled', 'ExitCode')}
            json.dump(report, output, indent=2)
            output.write('\n')
    print(report['result'])
    return 0 if report['result'] == 'PASS' else 1


if __name__ == '__main__':
    raise SystemExit(main())
