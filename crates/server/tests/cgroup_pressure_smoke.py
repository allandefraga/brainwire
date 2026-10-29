#!/usr/bin/env python3
"""Owned Linux cgroup-v2 admission smoke. Synthetic; not an RSS/OOM guarantee.

Run with a Python environment containing Neo4j 5.28.8, after the image build
has stopped. Every container/volume has a UUID ownership label; existing data
is never opened. Filling tmpfs deliberately approaches the 512 MiB limit.
"""
from __future__ import annotations
import argparse
import contextlib
import json
import math
import os
from pathlib import Path
import subprocess
import tempfile
import time
import uuid

LABEL = 'brainwire.pressure-smoke'
MIB = 1 << 20
LIMIT = 512 * MIB
PRESSURE_CODE = 'Neo.TransientError.General.MemoryPressure'
INJECTION_PERCENT = 96
SAFETY_PERCENT = 98


class SmokeInconclusive(RuntimeError):
    pass


def command(*args, timeout=60, check=True):
    result = subprocess.run(args, text=True, capture_output=True, timeout=timeout)
    if check and result.returncode:
        raise RuntimeError(f'{args[0]} command failed ({result.returncode}): {result.stderr[-1500:]}')
    return result


def docker(*args, **kwargs):
    return command('docker', *args, **kwargs).stdout.strip()


def inspect_owned(kind, name, run_id):
    result = command('docker', kind, 'inspect', name, check=False)
    if result.returncode:
        missing = result.stderr.lower()
        if any(marker in missing for marker in ('no such container', 'no such volume', 'no such object')):
            return None
        raise RuntimeError(f'cannot verify {kind} ownership or absence: {result.stderr[-1000:]}')
    value = json.loads(result.stdout)[0]
    labels = value.get('Labels', {}) if kind == 'volume' else value.get('Config', {}).get('Labels', {})
    if (labels or {}).get(LABEL) != run_id:
        raise RuntimeError(f'refusing to operate on unowned {kind}')
    return value


def cgroup(name, current_first=False):
    base = '/sys/fs/cgroup/'
    values = {}
    keys = ('memory.max', 'memory.peak', 'memory.events', 'memory.swap.max')
    keys = ('memory.current', *keys) if current_first else (*keys, 'memory.current')
    for key in keys:
        result = command('docker', 'exec', name, 'cat', base + key, check=False, timeout=10)
        if result.returncode:
            values[key] = None
        elif key == 'memory.events':
            try:
                values[key] = {line.split()[0]: int(line.split()[1]) for line in result.stdout.splitlines()}
            except (ValueError, IndexError):
                values[key] = None
        else:
            text = result.stdout.strip()
            values[key] = int(text) if text.isdigit() else text
    return values


def pressure_steps(current, limit):
    # Aim above the95% admission boundary so rollback/probe allocations do not
    # immediately make the second assertion meaningless. Keep a2% hard margin.
    target = (limit * INJECTION_PERCENT + 99) // 100
    first = max(0, (target - current - 8 * MIB) // MIB)
    return target, first


def pressured(snapshot):
    used, cap = snapshot.get('memory.current'), snapshot.get('memory.max')
    return isinstance(used, int) and isinstance(cap, int) and used * 100 >= cap * 95


def fill_to_pressure(name, fill, samples):
    snapshot = cgroup(name)
    if snapshot['memory.max'] != LIMIT or not isinstance(snapshot['memory.current'], int):
        raise SmokeInconclusive('finite cgroup-v2 reading disappeared during injection')
    target, first = pressure_steps(snapshot['memory.current'], LIMIT)
    if snapshot['memory.current'] > LIMIT * SAFETY_PERCENT // 100:
        raise SmokeInconclusive('usage already above the injection safety margin')
    if first:
        docker('exec', name, 'dd', 'if=/dev/zero', f'of={fill}', 'bs=1048576',
               f'count={first}', 'oflag=append', 'conv=notrunc', 'status=none', timeout=60)
    for _ in range(40):
        snapshot = cgroup(name)
        samples.append(snapshot)
        current = snapshot['memory.current']
        if not isinstance(current, int) or snapshot['memory.max'] != LIMIT:
            raise SmokeInconclusive('cgroup reading disappeared or max changed during injection')
        if current > LIMIT * SAFETY_PERCENT // 100:
            raise SmokeInconclusive('usage exceeded the injection safety margin')
        if current >= target:
            return snapshot
        if current + MIB > LIMIT * SAFETY_PERCENT // 100:
            raise SmokeInconclusive('refusing another increment with insufficient headroom')
        docker('exec', name, 'dd', 'if=/dev/zero', f'of={fill}', 'bs=1048576',
               'count=1', 'oflag=append', 'conv=notrunc', 'status=none', timeout=10)
    raise SmokeInconclusive('pressure target not reached within bounded fill loop')


def require_pressure_error(name, operation, checks):
    before = cgroup(name)
    failure = None
    try:
        operation()
    except Exception as error:
        failure = error
    after = cgroup(name, current_first=True)
    evidence = {'before': before, 'after': after,
                'scope': 'point-in-time samples; no continuous usage guarantee'}
    checks.append(evidence)
    if failure is not None:
        if getattr(failure, 'code', None) != PRESSURE_CODE:
            raise AssertionError(f'expected {PRESSURE_CODE}; got {type(failure).__name__} code={getattr(failure, "code", None)}') from failure
        evidence.update({'code': failure.code, 'type': type(failure).__name__})
        return evidence
    if not pressured(before) or not pressured(after):
        raise SmokeInconclusive('RUN was admitted but cgroup usage fell below95% around that RUN')
    raise AssertionError('RUN admitted with pressure in both bracketing samples; further phase-level investigation required')


def stop_events(process):
    if process is not None and process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)


def self_check():
    target, first = pressure_steps(40 * MIB, LIMIT)
    assert target == (LIMIT * 96 + 99) // 100
    assert 40 * MIB + first * MIB <= target - 8 * MIB
    assert pressure_steps(LIMIT, LIMIT)[1] == 0
    print('self-check PASS; no Docker resources started')


def run(args, report):
    from neo4j import GraphDatabase
    import neo4j
    run_id = uuid.uuid4().hex
    name = f'brainwire-pressure-smoke-{run_id}'
    volume = name + '-data'
    fill = f'/pressure/smoke-fill-{run_id}'
    event_process = None
    report.update({'run_id': run_id, 'driver': neo4j.__version__, 'image': args.image,
                   'memory_limit_bytes': LIMIT, 'threshold_percent': 95,
                   'injection_target_percent': INJECTION_PERCENT, 'injection_safety_percent': SAFETY_PERCENT,
                   'scope': 'synthetic single-container Linux cgroup-v2 RUN admission',
                   'limitations': ['not a global RSS ceiling', 'not sustained 2/4/8 GiB qualification',
                                  'not power-loss or distributed failover', 'raw usage includes tmpfs and file cache',
                                  'kernel logs not independently verified']})
    created = False
    with tempfile.TemporaryDirectory(prefix='bw-pressure-events-') as temporary:
        events_path = Path(temporary) / 'events.jsonl'
        try:
            docker('volume', 'create', '--label', f'{LABEL}={run_id}', volume)
            created = True
            report['image_id'] = json.loads(docker('image', 'inspect', args.image))[0]['Id']
            common = ['--pull=never', '--label', f'{LABEL}={run_id}', '--memory=512m', '--memory-swap=512m',
                      '--mount', f'type=volume,src={volume},dst=/data',
                      '-e', 'BRAINWIRE_SERVER_DEFAULT_DATABASE=demo',
                      '-e', 'BRAINWIRE_MEMORY_CACHE=64MiB', '-e', 'BRAINWIRE_MEMORY_MEMTABLES=8MiB',
                      '-e', 'BRAINWIRE_MEMORY_QUERY=32MiB', '-e', 'BRAINWIRE_MEMORY_INGRESS=16MiB']
            docker('run', '--rm', '--name', name + '-init', *common, args.image,
                   'db', 'create', 'demo', '--ontology', '/opt/brainwire/example-schema.json', timeout=120)
            with events_path.open('w') as event_file:
                event_process = subprocess.Popen(['docker', 'events', '--filter', f'container={name}',
                                                  '--format', '{{json .}}'], stdout=event_file, stderr=subprocess.DEVNULL)
                docker('run', '-d', '--name', name, *common, '--tmpfs', '/pressure:rw,size=512m,mode=1777',
                       '-p', '127.0.0.1::7687', args.image, 'serve', '--allow-unauthenticated-remote')
                state = inspect_owned('container', name, run_id)
                port = state['NetworkSettings']['Ports']['7687/tcp'][0]['HostPort']
                uri = f'bolt://127.0.0.1:{port}'
                with GraphDatabase.driver(uri, auth=None, connection_timeout=5,
                                          max_transaction_retry_time=0) as driver:
                    deadline = time.monotonic() + 30
                    while True:
                        try:
                            driver.verify_connectivity()
                            break
                        except Exception:
                            if time.monotonic() >= deadline:
                                raise RuntimeError('owned server startup did not become ready')
                            time.sleep(.1)
                    report['before'] = cgroup(name)
                    before = report['before']
                    if before['memory.max'] != LIMIT or not isinstance(before['memory.current'], int) or not isinstance(before['memory.events'], dict):
                        report['result'] = 'UNSUPPORTED'
                        report['reason'] = 'required finite cgroup-v2 current/max/events unavailable'
                        return
                    if before['memory.swap.max'] != 0:
                        raise AssertionError('container swap is not disabled')
                    report['server_protocol'] = str(driver.get_server_info().protocol_version)
                    docker('exec', name, 'touch', fill)
                    with driver.session(database='demo') as session:
                        session.run("CREATE (:Customer {entity_id:'acknowledged'})").consume()
                        with session.begin_transaction(timeout=120) as tx:
                            tx.run("CREATE (:Customer {entity_id:'pending-before-pressure'})").consume()
                            report['pressure_samples'] = []
                            fill_to_pressure(name, fill, report['pressure_samples'])
                            report['rejected_transaction_run'] = require_pressure_error(name,
                                lambda: tx.run("CREATE (:Customer {entity_id:'rejected-under-pressure'})").consume(),
                                report.setdefault('run_pressure_checks', []))
                            tx.rollback()
                        report['after_transaction_rollback'] = cgroup(name)
                        fill_to_pressure(name, fill, report['pressure_samples'])
                        # An independent session must also be denied. Driver
                        # callbacks are never automatically retried by this smoke.
                        with driver.session(database='demo') as other:
                            report['rejected_new_run'] = require_pressure_error(name, lambda: other.run('RETURN 1').consume(),
                                                                                report.setdefault('run_pressure_checks', []))
                        docker('exec', name, 'rm', '--', fill)
                        recovered = None
                        deadline = time.monotonic() + 15
                        while time.monotonic() < deadline:
                            with contextlib.suppress(Exception):
                                with driver.session(database='demo') as fresh:
                                    recovered = fresh.run('RETURN 1 AS ok').single()['ok']
                                    if recovered == 1:
                                        break
                            time.sleep(.1)
                        if recovered != 1:
                            raise AssertionError('normal RUN did not recover after own fill file was removed')
                        with driver.session(database='demo') as fresh:
                            counts = {}
                            for identity in ('acknowledged', 'pending-before-pressure', 'rejected-under-pressure'):
                                counts[identity] = fresh.run('MATCH (n:Customer {entity_id:$id}) RETURN count(n) AS n',
                                                             id=identity).single()['n']
                        if counts != {'acknowledged': 1, 'pending-before-pressure': 0, 'rejected-under-pressure': 0}:
                            raise AssertionError(f'transaction rollback/recovery mismatch: {counts}')
                        report['recovered_counts'] = counts
                    report['after_relief'] = cgroup(name)
                    for point in [report['before'], *report['pressure_samples'], report['after_relief']]:
                        events = point['memory.events']
                        if not isinstance(events, dict) or any(events.get(key, 0) for key in ('oom', 'oom_kill', 'oom_group_kill')):
                            raise AssertionError('missing or nonzero cgroup OOM evidence')
                report['final_state'] = inspect_owned('container', name, run_id)['State']
                if report['final_state'].get('OOMKilled') or not report['final_state'].get('Running'):
                    raise AssertionError('owned server stopped or Docker reports OOMKilled')
                log_result = command('docker', 'logs', name)
                logs = log_result.stdout + log_result.stderr
                report['pressure_log_lines'] = [line for line in logs.splitlines() if 'memory pressure admission' in line][-30:]
                docker('stop', '--time', '30', name, timeout=40)
                report['offline_audit'] = json.loads(docker('run', '--rm', '--name', name + '-audit',
                                                          *common, args.image, 'db', 'check', 'demo', '--json', timeout=120))
                if report['offline_audit'].get('status') != 'ok':
                    raise AssertionError('offline integrity audit failed')
                stop_events(event_process)
                event_process = None
            events = []
            for line in events_path.read_text().splitlines():
                with contextlib.suppress(json.JSONDecodeError):
                    events.append(json.loads(line))
            report['docker_events'] = [{'action': item.get('Action', item.get('status')),
                                        'time': item.get('time')} for item in events]
            if any(item['action'] == 'oom' for item in report['docker_events']):
                raise AssertionError('Docker event stream reported OOM')
            report['result'] = 'PASS'
        finally:
            report['owned_resources_cleaned'] = False
            stop_events(event_process)
            state = inspect_owned('container', name, run_id)
            if state is not None:
                report.setdefault('cleanup_state', state['State'])
                # Preserve evidence before stopping/removing owned resources,
                # including on failures or inconclusive pressure transitions.
                if state['State'].get('Running'):
                    try:
                        report['cleanup_cgroup'] = cgroup(name, current_first=True)
                    except Exception as capture_error:
                        report.setdefault('evidence_capture_errors', []).append(type(capture_error).__name__)
                try:
                    logs = command('docker', 'logs', name)
                    report['cleanup_pressure_log_lines'] = [line for line in (logs.stdout + logs.stderr).splitlines()
                                                            if 'memory pressure admission' in line][-40:]
                except Exception as capture_error:
                    report.setdefault('evidence_capture_errors', []).append(type(capture_error).__name__)
                command('docker', 'stop', '--time', '10', name, check=False, timeout=20)
                command('docker', 'rm', name, check=False, timeout=15)
            if created and inspect_owned('volume', volume, run_id) is not None:
                docker('volume', 'rm', volume, timeout=20)
            if inspect_owned('container', name, run_id) is not None:
                raise RuntimeError('owned container still exists after cleanup')
            if created and inspect_owned('volume', volume, run_id) is not None:
                raise RuntimeError('owned volume still exists after cleanup')
            if events_path.exists():
                evidence_events = []
                for line in events_path.read_text().splitlines():
                    with contextlib.suppress(json.JSONDecodeError):
                        item = json.loads(line)
                        evidence_events.append({'action': item.get('Action', item.get('status')), 'time': item.get('time')})
                report['cleanup_docker_events'] = evidence_events
            report['owned_resources_cleaned'] = True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--image')
    parser.add_argument('--output', type=Path)
    parser.add_argument('--self-check', action='store_true')
    args = parser.parse_args()
    if args.self_check:
        self_check()
        return 0
    if not args.image or args.output is None:
        parser.error('--image and --output are required')
    report = {'result': 'FAIL'}
    fd = os.open(args.output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'w') as output:
        try:
            run(args, report)
        except SmokeInconclusive as error:
            cgroup_events = (report.get('cleanup_cgroup') or {}).get('memory.events') or {}
            observed_oom = any(cgroup_events.get(key, 0) for key in ('oom', 'oom_kill', 'oom_group_kill'))
            observed_oom = observed_oom or report.get('cleanup_state', {}).get('OOMKilled', False)
            observed_oom = observed_oom or any(item.get('action') == 'oom' for item in report.get('cleanup_docker_events', []))
            report['result'] = 'FAIL' if observed_oom else 'INCONCLUSIVE'
            report['error_type'] = type(error).__name__
            report['error'] = str(error)
        except Exception as error:
            report['result'] = 'FAIL'
            report['error_type'] = type(error).__name__
            report['error'] = str(error)
        json.dump(report, output, indent=2)
        output.write('\n')
    print(json.dumps({'result': report['result'], 'receipt': str(args.output)}))
    return 0 if report['result'] == 'PASS' else 1


if __name__ == '__main__':
    raise SystemExit(main())
