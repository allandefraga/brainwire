#!/usr/bin/env python3
"""Owned 4 GiB Linux cgroup-v2 smoke with external tmpfs memory pressure.

Requires a locally built Brainwire image and the Neo4j Python driver. Run after
builds/tests have stopped. An external `dd` process in the owned server container
charges a tmpfs file to the same cgroup. This is neither an anonymous-memory
stress test nor sustained workload/RSS qualification. Only UUID-owned resources
are removed. --self-check starts no Docker resources.
"""
from __future__ import annotations

import argparse
import contextlib
import json
import os
from pathlib import Path
import platform
import subprocess
import tempfile
import time
import uuid

from cgroup_pressure_smoke import command, docker, stop_events

MIB = 1 << 20
LIMIT = 4 << 30
TARGET_PERCENT = 96
SAFETY_PERCENT = 97
STEP = 16 * MIB
CODE = 'Neo.TransientError.General.MemoryPressure'
LABEL = 'brainwire.external-pressure'


class Inconclusive(RuntimeError):
    pass


def parse_pairs(text):
    result = {}
    for line in text.splitlines():
        fields = line.split()
        if len(fields) == 2:
            result[fields[0]] = int(fields[1])
    return result


def working_set(raw, stat):
    """Match the server's approximate v2 working set, retaining shmem charges."""
    return max(0, raw - stat.get('inactive_file', 0))


def next_step(raw, used):
    target = (LIMIT * TARGET_PERCENT + 99) // 100
    safety = LIMIT * SAFETY_PERCENT // 100
    if raw > safety:
        raise Inconclusive('raw usage crossed the injection safety bound')
    if used >= target:
        return 0
    amount = min(STEP, target - used)
    # Exact-byte dd writes would be slow; round up at most one MiB, while
    # requiring safety headroom for the entire next chunk before starting it.
    amount = ((amount + MIB - 1) // MIB) * MIB
    if raw + amount > safety:
        raise Inconclusive('reclaimable file cache leaves insufficient safe injection headroom')
    return amount


def inspect_owned(kind, name, identity):
    result = command('docker', kind, 'inspect', name, check=False)
    if result.returncode:
        if any(x in result.stderr.lower() for x in ('no such container', 'no such volume', 'no such object')):
            return None
        raise RuntimeError(f'cannot inspect owned {kind}: {result.stderr[-800:]}')
    value = json.loads(result.stdout)[0]
    labels = value.get('Labels', {}) if kind == 'volume' else value.get('Config', {}).get('Labels', {})
    if (labels or {}).get(LABEL) != identity:
        raise RuntimeError(f'refusing operation on unowned {kind}')
    return value


def sample(name):
    # One exec reduces, but does not eliminate, time skew among these counters.
    result = command('docker', 'exec', name, 'sh', '-c',
        'for f in memory.current memory.max memory.peak memory.swap.max memory.events memory.stat cgroup.events; '
        'do printf "@@%s\\n" "$f"; cat "/sys/fs/cgroup/$f" || exit 1; done; '
        'printf "@@pid1.status\\n"; cat /proc/1/status; '
        'printf "@@pid1.cgroup\\n"; cat /proc/1/cgroup; '
        'printf "@@exec.cgroup\\n"; cat /proc/self/cgroup', timeout=15, check=False)
    if result.returncode:
        raise Inconclusive('required cgroup-v2 telemetry unavailable')
    parts = {}
    key = None
    for line in result.stdout.splitlines():
        if line.startswith('@@'):
            key = line[2:]
            parts[key] = []
        elif key is not None:
            parts[key].append(line)
    try:
        value = {key: int('\n'.join(parts[key])) for key in
                 ('memory.current', 'memory.max', 'memory.peak', 'memory.swap.max')}
        for key in ('memory.events', 'memory.stat', 'cgroup.events'):
            value[key] = parse_pairs('\n'.join(parts[key]))
        value['pid1.cgroup'] = '\n'.join(parts['pid1.cgroup'])
        value['exec.cgroup'] = '\n'.join(parts['exec.cgroup'])
        value['pid1_memory_kib'] = {}
        for line in parts['pid1.status']:
            fields = line.split()
            if fields and fields[0] in ('VmRSS:', 'VmHWM:', 'RssAnon:', 'RssFile:', 'RssShmem:'):
                value['pid1_memory_kib'][fields[0][:-1]] = int(fields[1])
        value['working_set_bytes'] = working_set(value['memory.current'], value['memory.stat'])
        value['monotonic_seconds'] = time.monotonic()
        return value
    except (KeyError, ValueError) as error:
        raise Inconclusive('incomplete or non-finite cgroup-v2 sample') from error


def require_finite(snapshot):
    if snapshot['memory.max'] != LIMIT or snapshot['memory.swap.max'] != 0:
        raise Inconclusive('expected exact 4 GiB cgroup-v2 limit with swap disabled')
    if snapshot['pid1.cgroup'] != snapshot['exec.cgroup']:
        raise Inconclusive('Docker exec and PID1 report different visible cgroups')
    if not {'oom', 'oom_kill'} <= snapshot['memory.events'].keys():
        raise Inconclusive('required OOM counters unavailable')
    if not {'anon', 'inactive_file', 'shmem'} <= snapshot['memory.stat'].keys():
        raise Inconclusive('required working-set and tmpfs counters unavailable')


def inject(name, fill, samples):
    # At most 256 * 16 MiB is attempted, sampled before every increment.
    for _ in range(256):
        snapshot = sample(name)
        require_finite(snapshot)
        samples.append(snapshot)
        amount = next_step(snapshot['memory.current'], snapshot['working_set_bytes'])
        if amount == 0:
            return snapshot
        docker('exec', name, 'dd', 'if=/dev/zero', f'of={fill}', 'bs=1048576',
               f'count={amount // MIB}', 'oflag=append', 'conv=notrunc', 'status=none', timeout=20)
    raise Inconclusive('injection target not reached within bounded steps')


def refusal(name, operation):
    before = sample(name)
    failure = None
    try:
        operation()
    except Exception as error:
        failure = error
    after = sample(name)
    evidence = {'before': before, 'after': after,
                'scope': 'bracketing samples; no continuous pressure guarantee'}
    if failure is None:
        if min(before['working_set_bytes'], after['working_set_bytes']) * 100 < LIMIT * 95:
            raise Inconclusive('pressure changed around RUN; cannot classify admitted query')
        raise AssertionError('RUN admitted with at least 95% working-set pressure in both samples')
    if getattr(failure, 'code', None) != CODE:
        raise AssertionError(f'wrong refusal code: {getattr(failure, "code", None)}') from failure
    text = str(failure)
    if 'reason=cgroup' not in text or 'rolled_back=true' not in text:
        raise AssertionError('memory refusal did not identify cgroup pressure and confirmed rollback') from failure
    evidence.update(code=failure.code, reason='cgroup', rolled_back=True)
    return evidence


def no_new_oom(before, after):
    return all(after.get(k, 0) == before.get(k, 0) for k in ('oom', 'oom_kill', 'oom_group_kill'))



def oom_observed(report):
    if any(item.get('action') == 'oom' for item in report.get('docker_events', [])):
        return True
    if any(report.get(key, {}).get('OOMKilled') for key in ('cleanup_state', 'state_before_stop', 'stopped_state')):
        return True
    points = [report.get('before'), report.get('after_relief'), report.get('cleanup_sample')]
    points.extend(report.get('injection_samples', []))
    baseline = (report.get('before') or {}).get('memory.events', {})
    return any(point is not None and not no_new_oom(baseline, point['memory.events']) for point in points)


def power_receipt():
    evidence = {'host': platform.platform(), 'load_average': list(os.getloadavg())}
    if platform.system() == 'Darwin':
        battery = command('pmset', '-g', 'batt', check=False)
        settings = command('pmset', '-g', check=False)
        evidence.update(power=battery.stdout, settings=settings.stdout)
        if battery.returncode or settings.returncode or "'AC Power'" not in battery.stdout:
            raise Inconclusive('measurement requires AC power on macOS')
        low_power = [line.split()[-1] for line in settings.stdout.splitlines() if 'lowpowermode' in line]
        if low_power != ['0']:
            raise Inconclusive('low power mode must be explicitly off')
    else:
        evidence['power_verification'] = 'unavailable; no portable AC-power assertion'
    return evidence



def record_final_power(report):
    # Cleanup/measurement diagnostics must not replace the primary test failure.
    try:
        report['host_after'] = power_receipt()
    except Exception as error:
        report['host_after'] = {'measurement_error_type': type(error).__name__,
                                'measurement_conditions_changed': str(error)}
        if report.get('result') == 'PASS':
            report['result'] = 'INCONCLUSIVE'


def run(args, report):
    from neo4j import GraphDatabase, __version__ as driver_version
    identity = uuid.uuid4().hex
    name = f'brainwire-external-pressure-{identity}'
    volume = name + '-data'
    fill = '/pressure/external-fill'
    events_process = None
    report.update(run_id=identity, image=args.image, driver=driver_version,
        memory_limit_bytes=LIMIT, target_percent=TARGET_PERCENT, safety_percent=SAFETY_PERCENT,
        synthetic=True, injection='external dd process creates unswappable tmpfs charge in server cgroup',
        limitations=['not external anonymous-memory pressure', 'not a process RSS ceiling',
                     'not sustained 2/4/8 GiB qualification', 'sampling can miss a short peak',
                     'Docker Desktop VM kernel logs may be inaccessible'])
    report['host_before'] = power_receipt()
    report['image_id'] = json.loads(docker('image', 'inspect', args.image))[0]['Id']
    common = ['--pull=never', '--label', f'{LABEL}={identity}', '--memory=4g', '--memory-swap=4g',
              '--mount', f'type=volume,src={volume},dst=/data',
              '-e', 'BRAINWIRE_SERVER_DEFAULT_DATABASE=demo', '-e', 'BRAINWIRE_MEMORY_CACHE=128MiB',
              '-e', 'BRAINWIRE_MEMORY_MEMTABLES=16MiB', '-e', 'BRAINWIRE_MEMORY_QUERY=64MiB',
              '-e', 'BRAINWIRE_MEMORY_INGRESS=16MiB']
    with tempfile.TemporaryDirectory(prefix='bw-external-pressure-') as temporary:
        events_path = Path(temporary) / 'events.jsonl'
        try:
            docker('volume', 'create', '--label', f'{LABEL}={identity}', volume)
            if inspect_owned('volume', volume, identity) is None:
                raise RuntimeError('owned volume disappeared before initialization')
            docker('run', '--rm', '--name', name + '-init', *common, args.image,
                   'db', 'create', 'demo', '--ontology', '/opt/brainwire/example-schema.json', timeout=120)
            with events_path.open('w') as event_file:
                events_process = subprocess.Popen(['docker', 'events', '--filter', f'container={name}',
                    '--format', '{{json .}}'], stdout=event_file, stderr=subprocess.DEVNULL)
                docker('run', '-d', '--name', name, *common, '--tmpfs', '/pressure:rw,size=4g,mode=1777',
                       '-p', '127.0.0.1::7687', args.image, 'serve', '--allow-unauthenticated-remote')
                info = inspect_owned('container', name, identity)
                port = info['NetworkSettings']['Ports']['7687/tcp'][0]['HostPort']
                report['initial_restart_count'] = info['RestartCount']
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
                    before = sample(name)
                    require_finite(before)
                    report['before'] = before
                    if any(before['memory.events'].get(key, 0) for key in ('oom', 'oom_kill', 'oom_group_kill')):
                        raise AssertionError('fresh server cgroup already recorded an OOM event')
                    if report['initial_restart_count'] != 0:
                        raise AssertionError('owned server restarted before the initial sample')
                    docker('exec', name, 'touch', fill)
                    with driver.session(database='demo') as session:
                        session.run("CREATE (:Customer {entity_id:'acknowledged'})").consume()
                        tx = session.begin_transaction(timeout=180)
                        try:
                            tx.run("CREATE (:Customer {entity_id:'pending'})").consume()
                            report['injection_samples'] = []
                            inject(name, fill, report['injection_samples'])
                            report['pending_transaction_refusal'] = refusal(name,
                                lambda: tx.run("CREATE (:Customer {entity_id:'refused'})").consume())
                        finally:
                            tx.rollback()
                        inject(name, fill, report['injection_samples'])
                        with driver.session(database='demo') as other:
                            report['fresh_session_refusal'] = refusal(name, lambda: other.run('RETURN 1').consume())
                    docker('exec', name, 'rm', '--', fill)
                    deadline = time.monotonic() + 20
                    while True:
                        try:
                            with driver.session(database='demo') as fresh:
                                assert fresh.run('RETURN 1 AS ok').single()['ok'] == 1
                            break
                        except Exception:
                            if time.monotonic() >= deadline:
                                raise AssertionError('normal RUN did not recover after external fill removal')
                            time.sleep(.1)
                    with driver.session(database='demo') as fresh:
                        counts = {key: fresh.run('MATCH (n:Customer {entity_id:$id}) RETURN count(n) AS n',
                            id=key).single()['n'] for key in ('acknowledged', 'pending', 'refused')}
                    assert counts == {'acknowledged': 1, 'pending': 0, 'refused': 0}, counts
                    report['recovered_counts'] = counts
                    report['after_relief'] = sample(name)
                    states = [before, *report['injection_samples'], report['after_relief']]
                    if any(not no_new_oom(before['memory.events'], point['memory.events']) for point in states):
                        raise AssertionError('cgroup reported an OOM event during the test')
                info = inspect_owned('container', name, identity)
                report['state_before_stop'] = info['State']
                report['restart_count_before_stop'] = info['RestartCount']
                if not info['State']['Running'] or info['State'].get('OOMKilled') or info['RestartCount'] != report['initial_restart_count']:
                    raise AssertionError('owned server died, restarted or reported Docker OOMKilled')
                kernel = command('docker', 'exec', name, 'dmesg', check=False, timeout=10)
                report['kernel_log'] = {'available': kernel.returncode == 0,
                    'scope': 'container-visible OOM-related lines; may be VM-wide, not owned-process attribution',
                    'matched_lines': [line for line in kernel.stdout.splitlines()
                        if any(term in line.lower() for term in ('out of memory', 'killed process', 'oom-kill', 'memory cgroup'))][-30:],
                    'unavailable_reason': kernel.stderr[-800:] if kernel.returncode != 0 else None}
                docker('stop', '--time', '30', name, timeout=40)
                stopped = inspect_owned('container', name, identity)
                report['stopped_state'] = stopped['State']
                if stopped['State']['ExitCode'] != 0:
                    raise AssertionError('owned server did not exit cleanly')
                report['audit'] = json.loads(docker('run', '--rm', '--name', name + '-audit', *common,
                    args.image, 'db', 'check', 'demo', '--json', timeout=120))
                if report['audit'].get('status') != 'ok':
                    raise AssertionError('offline integrity audit failed')
                stop_events(events_process)
                events_process = None
            report['result'] = 'PASS'
        finally:
            stop_events(events_process)
            report['owned_resources_cleaned'] = False
            state = inspect_owned('container', name, identity)
            if state is not None:
                report['cleanup_state'] = state['State']
                report['cleanup_restart_count'] = state['RestartCount']
                if state['State'].get('Running'):
                    with contextlib.suppress(Exception):
                        report['cleanup_sample'] = sample(name)
                logs = command('docker', 'logs', name, check=False)
                report['server_log_tail'] = (logs.stdout + logs.stderr)[-12000:]
            for owned in (name, name + '-init', name + '-audit'):
                if inspect_owned('container', owned, identity) is not None:
                    command('docker', 'stop', '--time', '10', owned, check=False, timeout=20)
                    docker('rm', owned, timeout=20)
            if inspect_owned('volume', volume, identity) is not None:
                docker('volume', 'rm', volume, timeout=20)
            if any(inspect_owned('container', owned, identity) is not None for owned in (name, name + '-init', name + '-audit')):
                raise RuntimeError('owned container remains after cleanup')
            if inspect_owned('volume', volume, identity) is not None:
                raise RuntimeError('owned volume remains after cleanup')
            events = []
            if events_path.exists():
                for line in events_path.read_text().splitlines():
                    with contextlib.suppress(json.JSONDecodeError):
                        item = json.loads(line)
                        events.append({'action': item.get('Action', item.get('status')), 'time': item.get('time')})
            report['docker_events'] = events
            if oom_observed(report):
                report['result'] = 'FAIL'
                report['oom_observed'] = True
            report['owned_resources_cleaned'] = True
            record_final_power(report)


def self_check():
    assert parse_pairs('anon 12\ninactive_file 34\n') == {'anon': 12, 'inactive_file': 34}
    assert working_set(100, {'inactive_file': 30, 'shmem': 20}) == 70
    assert working_set(10, {'inactive_file': 30}) == 0
    assert next_step(0, 0) == STEP
    assert next_step(LIMIT * 96 // 100 + 1, LIMIT * 96 // 100 + 1) == 0
    try:
        next_step(LIMIT * 97 // 100 + 1, 0)
    except Inconclusive:
        pass
    else:
        raise AssertionError('injection beyond safety bound accepted')
    assert no_new_oom({'oom': 1, 'oom_kill': 0}, {'oom': 1, 'oom_kill': 0})
    assert not no_new_oom({'oom': 0}, {'oom': 1})
    print('self-check PASS; no Docker resources started')


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
    fd = os.open(args.output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    report = {'result': 'FAIL'}
    with os.fdopen(fd, 'w') as output:
        try:
            run(args, report)
        except Inconclusive as error:
            report['result'] = 'FAIL' if report.get('oom_observed') else 'INCONCLUSIVE'
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
