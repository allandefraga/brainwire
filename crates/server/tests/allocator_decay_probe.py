#!/usr/bin/env python3
"""Compare short-load VmHWM and idle RSS using one prebuilt Linux image.

Run two separate invocations with --decay-ms 10000 and --decay-ms 1000, the
same image ID and fixture, after builds and unrelated workloads have stopped.
This diagnostic does not reproduce an external benchmark's unknown fixture or
qualify a process RSS ceiling. --self-check starts no Docker resources.
"""
from __future__ import annotations

import argparse
import contextlib
import json
import os
import re
from pathlib import Path
import subprocess
import tempfile
import time
import uuid

import cgroup_external_pressure as pressure


class Inconclusive(pressure.Inconclusive):
    pass


def allocator_conf(decay_ms):
    if not isinstance(decay_ms, int) or not 0 <= decay_ms <= 60000:
        raise ValueError('decay must be between 0 and 60000 milliseconds')
    return f'confirm_conf:true,abort_conf:true,dirty_decay_ms:{decay_ms},muzzy_decay_ms:{decay_ms}'


def confirmed(log, decay_ms):
    values = {}
    for line in log.splitlines():
        match = re.fullmatch(
            r'(?:<jemalloc>:\s*)?Set conf value:\s*(dirty_decay_ms|muzzy_decay_ms):(-?\d+)\s*',
            line.strip())
        if match:
            values[match[1]] = int(match[2])
    return all(values.get(key) == decay_ms for key in ('dirty_decay_ms', 'muzzy_decay_ms'))


def oom_observed(report):
    if report.get('oom_observed') or any(item.get('action') == 'oom' for item in report.get('docker_events', [])):
        return True
    for key in ('cleanup_state', 'state_before_stop', 'stopped_state'):
        if report.get(key, {}).get('OOMKilled'):
            return True
    baseline = (report.get('before') or {}).get('memory.events', {})
    points = [report.get('before'), report.get('after_load'), report.get('cleanup_sample'),
              *report.get('idle_samples', [])]
    return any(point is not None and isinstance(point.get('memory.events'), dict)
               and not pressure.no_new_oom(baseline, point['memory.events']) for point in points)


def failure_observed(report):
    if oom_observed(report):
        return True
    for key in ('cleanup_state', 'state_before_stop', 'stopped_state'):
        state = report.get(key, {})
        if not state.get('Running', False) and state.get('ExitCode', 0) != 0:
            return True
    return any(report.get(key, 0) != 0 for key in
               ('initial_restart_count', 'restart_count_before_stop', 'cleanup_restart_count'))


def finalize(report, identity, name, volume, events_process, events_path):
    """Attempt every owned cleanup; diagnostics never replace the primary error."""
    errors = []

    def attempt(operation, action):
        try:
            return action()
        except Exception as error:
            errors.append({'operation': operation, 'error_type': type(error).__name__,
                           'error': str(error)})
            return None

    report['owned_resources_cleaned'] = False
    attempt('stop event observer', lambda: pressure.stop_events(events_process))
    state = attempt('inspect server', lambda: pressure.inspect_owned('container', name, identity))
    if state is not None:
        report['cleanup_state'] = state['State']
        report['cleanup_restart_count'] = state['RestartCount']
        if state['State'].get('Running'):
            point = attempt('sample server', lambda: pressure.sample(name))
            if point is not None:
                report['cleanup_sample'] = point
    names = (name, name + '-init', name + '-audit')
    for resource in names:
        value = attempt(f'inspect {resource}',
                        lambda resource=resource: pressure.inspect_owned('container', resource, identity))
        if value is not None:
            attempt(f'stop {resource}', lambda resource=resource:
                    pressure.command('docker', 'stop', '--time', '10', resource, check=False, timeout=20))
            attempt(f'remove {resource}', lambda resource=resource:
                    pressure.docker('rm', resource, timeout=20))
    value = attempt('inspect volume', lambda: pressure.inspect_owned('volume', volume, identity))
    if value is not None:
        attempt('remove volume', lambda: pressure.docker('volume', 'rm', volume, timeout=20))
    remaining = []
    for kind, resource in [('container', value) for value in names] + [('volume', volume)]:
        value = attempt(f'verify absence {resource}',
                        lambda kind=kind, resource=resource: pressure.inspect_owned(kind, resource, identity))
        if value is not None:
            remaining.append(resource)
    if remaining:
        errors.append({'operation': 'verify cleanup', 'error_type': 'RuntimeError',
                       'error': 'owned resources remain: ' + ', '.join(remaining)})

    def read_events():
        events = []
        if events_path.exists():
            for line in events_path.read_text().splitlines():
                with contextlib.suppress(json.JSONDecodeError):
                    item = json.loads(line)
                    events.append({'action': item.get('Action', item.get('status')), 'time': item.get('time')})
        return events

    report['docker_events'] = attempt('read events', read_events) or []
    if oom_observed(report):
        report['oom_observed'] = True
    if failure_observed(report) or errors:
        report['result'] = 'FAIL'
    if errors:
        report['cleanup_errors'] = errors
    report['owned_resources_cleaned'] = not errors
    attempt('record final power', lambda: pressure.record_final_power(report))
    if errors:
        report['cleanup_errors'] = errors
        report['owned_resources_cleaned'] = False
        report['result'] = 'FAIL'


def metrics(samples):
    if not samples:
        raise ValueError('no memory samples')
    values = [point['pid1_memory_kib'] for point in samples]
    if any('VmHWM' not in item or 'VmRSS' not in item for item in values):
        raise Inconclusive('PID1 VmHWM or VmRSS unavailable')
    return {'process_vmhwm_bytes': max(item['VmHWM'] for item in values) * 1024,
            'process_sampled_peak_rss_bytes': max(item['VmRSS'] for item in values) * 1024,
            'process_last_rss_bytes': values[-1]['VmRSS'] * 1024,
            'cgroup_peak_bytes': max(point['memory.peak'] for point in samples),
            'scope': 'VmHWM is the kernel process high-water; cgroup peak includes file cache and helper processes'}


def fixture_rows(start, count, payload):
    return [{'id': f'probe-{index:08d}', 'payload': payload} for index in range(start, start + count)]


def run(args, report):
    from neo4j import GraphDatabase, __version__ as driver_version
    identity = uuid.uuid4().hex
    name = f'brainwire-decay-probe-{identity}'
    volume = name + '-data'
    conf = allocator_conf(args.decay_ms)
    info = json.loads(pressure.docker('image', 'inspect', args.image))[0]
    image_id = info['Id']
    if not re.fullmatch(r'sha256:[0-9a-f]{64}', image_id):
        raise RuntimeError('Docker did not resolve the image to an immutable ID')
    report.update(run_id=identity, image=args.image, image_id=image_id,
        image_revision=(info.get('Config', {}).get('Labels') or {}).get('org.opencontainers.image.revision'),
        driver=driver_version, synthetic=True, decay_ms=args.decay_ms, allocator_conf=conf,
        rows=args.rows, batch=args.batch, payload_bytes=args.payload_bytes,
        idle_seconds=args.idle_seconds, memory_limit_bytes=pressure.LIMIT,
        limits=['one synthetic short-load sample', 'same-image environment override, not a code change',
                'not the external 243/545 MiB fixture', 'not an RSS cap or sustained-load qualification'])
    report['host_before'] = pressure.power_receipt()
    common = ['--pull=never', '--label', f'{pressure.LABEL}={identity}',
        '--cpus=2', '--memory=4g', '--memory-swap=4g',
        '--mount', f'type=volume,src={volume},dst=/data',
        '-e', 'BRAINWIRE_SERVER_DEFAULT_DATABASE=demo', '-e', 'BRAINWIRE_MEMORY_CACHE=128MiB',
        '-e', 'BRAINWIRE_MEMORY_MEMTABLES=16MiB', '-e', 'BRAINWIRE_MEMORY_QUERY=128MiB',
        '-e', 'BRAINWIRE_MEMORY_INGRESS=16MiB', '-e', f'MALLOC_CONF={conf}',
        '-e', f'_RJEM_MALLOC_CONF={conf}']
    events_process = None
    with tempfile.TemporaryDirectory(prefix='bw-decay-events-') as temporary:
        events_path = Path(temporary) / 'events.jsonl'
        try:
            pressure.docker('volume', 'create', '--label', f'{pressure.LABEL}={identity}', volume)
            pressure.inspect_owned('volume', volume, identity)
            pressure.docker('run', '--rm', '--name', name + '-init', *common, image_id,
                'db', 'create', 'demo', '--ontology', '/opt/brainwire/example-schema.json', timeout=120)
            with events_path.open('w') as event_file:
                events_process = subprocess.Popen(['docker', 'events', '--filter', f'container={name}',
                    '--format', '{{json .}}'], stdout=event_file, stderr=subprocess.DEVNULL)
                pressure.docker('run', '-d', '--name', name, *common, '-p', '127.0.0.1::7687',
                    image_id, 'serve', '--allow-unauthenticated-remote')
                state = pressure.inspect_owned('container', name, identity)
                if state['Image'] != image_id:
                    raise RuntimeError('owned server image differs from resolved image ID')
                port = state['NetworkSettings']['Ports']['7687/tcp'][0]['HostPort']
                report['initial_restart_count'] = state['RestartCount']
                with GraphDatabase.driver(f'bolt://127.0.0.1:{port}', auth=None,
                    connection_timeout=5, max_transaction_retry_time=0) as driver:
                    deadline = time.monotonic() + 40
                    while True:
                        try:
                            driver.verify_connectivity()
                            break
                        except Exception:
                            if time.monotonic() >= deadline:
                                raise RuntimeError('owned server did not become ready')
                            time.sleep(.1)
                    report['before'] = pressure.sample(name)
                    pressure.require_finite(report['before'])
                    payload = 'x' * args.payload_bytes
                    load_start = time.monotonic()
                    with driver.session(database='demo') as session:
                        for start in range(0, args.rows, args.batch):
                            if time.monotonic() - load_start > 180:
                                raise TimeoutError('short-load deadline exceeded')
                            rows = fixture_rows(start, min(args.batch, args.rows - start), payload)
                            with session.begin_transaction(timeout=30) as tx:
                                tx.run('UNWIND $rows AS r CREATE (:Customer {entity_id:r.id, name:r.payload})', rows=rows).consume()
                                tx.commit()
                        count = session.run('MATCH (n:Customer) RETURN count(n) AS n').single()['n']
                        if count != args.rows:
                            raise AssertionError('loaded row count mismatch')
                    report['load_seconds'] = time.monotonic() - load_start
                    report['after_load'] = pressure.sample(name)
                    report['idle_samples'] = []
                    idle_start = time.monotonic()
                    # No application queries during idle; telemetry uses external cat.
                    while time.monotonic() - idle_start < args.idle_seconds:
                        point = pressure.sample(name)
                        point['idle_seconds'] = time.monotonic() - idle_start
                        report['idle_samples'].append(point)
                        time.sleep(min(1, max(0, args.idle_seconds - (time.monotonic() - idle_start))))
                    points = [report['before'], report['after_load'], *report['idle_samples']]
                    report['metrics'] = metrics(points)
                    report['idle_rss_delta_from_after_load_bytes'] = report['after_load']['pid1_memory_kib']['VmRSS'] * 1024 - report['metrics']['process_last_rss_bytes']
                    if any(not pressure.no_new_oom(report['before']['memory.events'], p['memory.events']) for p in points):
                        raise AssertionError('cgroup recorded an OOM event')
                logs = pressure.command('docker', 'logs', name)
                text = logs.stdout + logs.stderr
                report['allocator_conf_lines'] = [line for line in text.splitlines() if '<jemalloc>' in line][-100:]
                report['allocator_conf_confirmed'] = confirmed(text, args.decay_ms)
                if not report['allocator_conf_confirmed']:
                    raise Inconclusive('allocator did not confirm both requested decay settings')
                state = pressure.inspect_owned('container', name, identity)
                report['state_before_stop'] = state['State']
                report['restart_count_before_stop'] = state['RestartCount']
                if not state['State']['Running'] or state['State'].get('OOMKilled') or state['RestartCount'] != 0:
                    raise AssertionError('owned server died, restarted or reported OOMKilled')
                kernel = pressure.command('docker', 'exec', name, 'dmesg', check=False, timeout=10)
                report['kernel_log'] = {'available': kernel.returncode == 0,
                    'scope': 'container-visible OOM-related lines; may be VM-wide',
                    'matched_lines': [line for line in kernel.stdout.splitlines() if any(term in line.lower()
                        for term in ('out of memory', 'killed process', 'oom-kill', 'memory cgroup'))][-30:],
                    'unavailable_reason': kernel.stderr[-800:] if kernel.returncode else None}
                pressure.docker('stop', '--time', '30', name, timeout=40)
                report['stopped_state'] = pressure.inspect_owned('container', name, identity)['State']
                if report['stopped_state']['ExitCode'] != 0:
                    raise AssertionError('owned server did not exit cleanly')
                report['audit'] = json.loads(pressure.docker('run', '--rm', '--name', name + '-audit',
                    *common, image_id, 'db', 'check', 'demo', '--json', timeout=120))
                if report['audit'].get('status') != 'ok':
                    raise AssertionError('offline audit failed')
                pressure.stop_events(events_process)
                events_process = None
            report['result'] = 'PASS'
        finally:
            finalize(report, identity, name, volume, events_process, events_path)



def self_check():
    assert allocator_conf(1000).endswith('dirty_decay_ms:1000,muzzy_decay_ms:1000')
    assert not confirmed('Set conf value: dirty_decay_ms:1000', 1000)
    assert not confirmed('Set conf value: dirty_decay_ms:10000\nSet conf value: muzzy_decay_ms:10000', 1000)
    assert confirmed('Set conf value: dirty_decay_ms:1000\nSet conf value: muzzy_decay_ms:1000', 1000)
    assert fixture_rows(1, 2, 'x') == [{'id': 'probe-00000001', 'payload': 'x'}, {'id': 'probe-00000002', 'payload': 'x'}]
    print('self-check PASS; no Docker resources started')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--image')
    parser.add_argument('--output', type=Path)
    parser.add_argument('--decay-ms', type=int, default=10000)
    parser.add_argument('--rows', type=int, default=10000)
    parser.add_argument('--batch', type=int, default=250)
    parser.add_argument('--payload-bytes', type=int, default=4096)
    parser.add_argument('--idle-seconds', type=int, default=30)
    parser.add_argument('--self-check', action='store_true')
    args = parser.parse_args()
    if args.self_check:
        self_check()
        return 0
    if not args.image or args.output is None:
        parser.error('--image and --output are required')
    if not (1 <= args.rows <= 1000000 and 1 <= args.batch <= 1000 and
            1 <= args.payload_bytes <= 65536 and 15 <= args.idle_seconds <= 120):
        parser.error('fixture or idle settings outside bounded probe ranges')
    allocator_conf(args.decay_ms)
    report = {'result': 'FAIL'}
    fd = os.open(args.output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'w') as output:
        try:
            run(args, report)
        except pressure.Inconclusive as error:
            report['result'] = 'FAIL' if failure_observed(report) or report.get('cleanup_errors') else 'INCONCLUSIVE'
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
