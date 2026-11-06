#!/usr/bin/env python3
"""Owned Linux cgroup-v2 admission smoke. Synthetic; not an RSS/OOM guarantee.

Run with a Python environment containing Neo4j 5.28.8, after the image build
has stopped. Every container/volume has a UUID ownership label; existing data
is never opened. Filling tmpfs deliberately approaches the selected container limit.
"""
from __future__ import annotations
import argparse
import contextlib
import json
import math
import os
import re
import select
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
from brainwire.client import MemoryPressureError

LABEL = 'brainwire.pressure-smoke'
MIB = 1 << 20
DEFAULT_MEMORY_MIB = 512
MEMORY_MIB_CHOICES = (DEFAULT_MEMORY_MIB, 2048, 3072)
LIMIT = DEFAULT_MEMORY_MIB * MIB
PRESSURE_CODE = 'Neo.TransientError.General.MemoryPressure'
INJECTION_PERCENT = 96
SAFETY_PERCENT = 98
OBSERVER_MAX_BYTES = 64 << 10
OBSERVER_MAX_SAMPLES = 256
OBSERVER_MAX_WINDOW_BYTES = 256 << 10
OBSERVER_TIMEOUT = 2.0


class SmokeInconclusive(RuntimeError):
    pass


def parse_observer_snapshot(text):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError('duplicate field')
            result[key] = value
        return result
    def integer(value):
        if (not isinstance(value, str) or not value.isascii() or not value.isdigit()
                or len(value) > 20 or int(value) > (1 << 64) - 1):
            raise ValueError('invalid unsigned reading')
        return int(value)
    try:
        if len(text) > OBSERVER_MAX_BYTES:
            raise ValueError('oversized observer snapshot')
        raw = json.loads(text, object_pairs_hook=unique)
        required = {'uptime', 'memory.current', 'memory.max', 'memory.peak',
                    'memory.swap.max', 'memory.high', 'inactive_file_values', 'events'}
        if not isinstance(raw, dict) or set(raw) != required:
            raise ValueError('missing observer field')
        if not isinstance(raw['uptime'], str) or not re.fullmatch(r'[0-9]{1,20}(\.[0-9]{1,9})?', raw['uptime']):
            raise ValueError('invalid uptime')
        values = {key: integer(raw[key]) for key in
                  ('memory.current', 'memory.max', 'memory.peak', 'memory.swap.max')}
        values['memory.high'] = 'max' if raw['memory.high'] == 'max' else integer(raw['memory.high'])
        inactive = raw['inactive_file_values']
        if not isinstance(inactive, list) or len(inactive) != 1:
            raise ValueError('inactive_file missing or duplicated')
        inactive = integer(inactive[0])
        events = {}
        if not isinstance(raw['events'], list) or len(raw['events']) > 64:
            raise ValueError('invalid event readings')
        for item in raw['events']:
            if (not isinstance(item, list) or len(item) != 2 or not isinstance(item[0], str)
                    or not re.fullmatch(r'[a-z_]{1,64}', item[0]) or item[0] in events):
                raise ValueError('invalid or duplicate event')
            events[item[0]] = integer(item[1])
        if not {'oom', 'oom_kill', 'oom_group_kill'} <= set(events):
            raise ValueError('missing OOM events')
        values.update({'memory.events': events, 'memory.stat.inactive_file': inactive,
                       'observer_uptime_seconds': raw['uptime'],
                       'diagnostic_working_set': {
                           'bytes': max(0, values['memory.current'] - inactive), 'unit': 'bytes',
                           'source': 'memory.current minus inactive_file from persistent shell builtins',
                           'scope': 'sequential readings; no per-sample exec; not an atomic runtime sample'}})
        return values
    except (ValueError, TypeError, KeyError, RecursionError) as error:
        raise SmokeInconclusive('persistent observer returned an invalid snapshot') from error


def observer_readline(fd, timeout, cancelled=None):
    deadline, data = time.monotonic() + timeout, bytearray()
    while True:
        if cancelled is not None and cancelled.is_set():
            raise SmokeInconclusive('persistent observer cancelled')
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise SmokeInconclusive('persistent observer timed out')
        if not select.select([fd], [], [], min(remaining, .05))[0]:
            continue
        try:
            chunk = os.read(fd, min(4096, OBSERVER_MAX_BYTES + 1 - len(data)))
        except BlockingIOError:
            continue
        if not chunk:
            raise SmokeInconclusive('persistent observer closed before a full snapshot')
        data.extend(chunk)
        if len(data) > OBSERVER_MAX_BYTES:
            raise SmokeInconclusive('persistent observer snapshot exceeds byte limit')
        if b'\n' in data:
            line, extra = bytes(data).split(b'\n', 1)
            if extra:
                raise SmokeInconclusive('persistent observer returned extra output')
            return line


OBSERVER_SCRIPT = r'''while IFS= read -r command; do
if [ "$command" = sample ]; then
 read -r uptime rest < /proc/uptime || exit 70
 read -r current < /sys/fs/cgroup/memory.current || exit 70
 read -r maximum < /sys/fs/cgroup/memory.max || exit 70
 read -r peak < /sys/fs/cgroup/memory.peak || exit 70
 read -r swap < /sys/fs/cgroup/memory.swap.max || exit 70
 read -r high < /sys/fs/cgroup/memory.high || exit 70
 printf '{"uptime":"%s","memory.current":"%s","memory.max":"%s","memory.peak":"%s","memory.swap.max":"%s","memory.high":"%s","inactive_file_values":[' "$uptime" "$current" "$maximum" "$peak" "$swap" "$high"
 sep=''
 while read -r key value rest; do
  if [ "$key" = inactive_file ]; then printf '%s"%s"' "$sep" "$value"; sep=,; fi
  :
 done < /sys/fs/cgroup/memory.stat || exit 70
 printf '],"events":['
 sep=''
 while read -r key value; do printf '%s["%s","%s"]' "$sep" "$key" "$value"; sep=,; done < /sys/fs/cgroup/memory.events || exit 70
 printf ']}\n'
fi
done'''


class PersistentCgroupObserver:
    """One owned shell before baseline; no child process for each snapshot."""
    def __init__(self, name, run_id):
        state = inspect_owned('container', name, run_id)
        if state is None:
            raise SmokeInconclusive('owned server disappeared before observer startup')
        self.name, self.identity = name, state['Id']
        self.lock, self.closed = threading.Lock(), threading.Event()
        self.windows = []
        try:
            self.process = subprocess.Popen(
                ['docker', 'exec', '-i', self.identity, '/bin/sh', '-c', OBSERVER_SCRIPT],
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        except OSError as error:
            raise SmokeInconclusive('cannot start persistent observer') from error
        try:
            os.set_blocking(self.process.stdin.fileno(), False)
            os.set_blocking(self.process.stdout.fileno(), False)
        except OSError as error:
            self.close()
            raise SmokeInconclusive('cannot configure persistent observer') from error

    def sample(self, name, current_first=False, cancelled=None):
        if name not in (self.name, self.identity):
            raise SmokeInconclusive('observer target changed')
        deadline = time.monotonic() + OBSERVER_TIMEOUT
        while not self.lock.acquire(timeout=.05):
            if self.closed.is_set() or (cancelled is not None and cancelled.is_set()):
                raise SmokeInconclusive('persistent observer cancelled')
            if time.monotonic() >= deadline:
                raise SmokeInconclusive('persistent observer lock timed out')
        try:
            if self.closed.is_set() or (cancelled is not None and cancelled.is_set()):
                raise SmokeInconclusive('persistent observer cancelled')
            start, wall = time.monotonic_ns(), time.time_ns()
            try:
                if os.write(self.process.stdin.fileno(), b'sample\n') != 7:
                    raise OSError('incomplete observer request')
                line = observer_readline(self.process.stdout.fileno(), max(0, deadline - time.monotonic()), cancelled)
                value = parse_observer_snapshot(line)
            except (OSError, ValueError) as error:
                raise SmokeInconclusive('persistent observer I/O unavailable') from error
            value.update(observer_monotonic_start_ns=start, observer_monotonic_end_ns=time.monotonic_ns(),
                         observer_wall_start_ns=wall)
            return value
        finally:
            self.lock.release()

    @contextlib.contextmanager
    def window(self, phase=None):
        if len(self.windows) >= 8:
            raise SmokeInconclusive('persistent observer window limit reached')
        window = {'wall_start_ns': time.time_ns(), 'monotonic_start_ns': time.monotonic_ns(), 'samples': []}
        if phase is not None:
            window['phase'] = phase
        self.windows.append(window)
        stop = threading.Event()
        def observe():
            sample_bytes = 0
            for _ in range(OBSERVER_MAX_SAMPLES):
                if stop.is_set():
                    return
                try:
                    sample = self.sample(self.name, cancelled=self.closed)
                    sample_bytes += len(json.dumps(sample, separators=(',', ':')).encode())
                    if sample_bytes > OBSERVER_MAX_WINDOW_BYTES:
                        window['observer_error'] = 'SampleByteLimitReached'
                        return
                    window['samples'].append(sample)
                except Exception as error:
                    window['observer_error'] = type(error).__name__
                    return
                if stop.wait(.005):
                    return
            window['observer_error'] = 'SampleLimitReached'
        thread = threading.Thread(target=observe, daemon=True, name='brainwire-cgroup-observer')
        thread.start()
        window['operation_monotonic_start_ns'] = time.monotonic_ns()
        window['operation_wall_start_ns'] = time.time_ns()
        try:
            yield window
        finally:
            window['operation_monotonic_end_ns'] = time.monotonic_ns()
            window['operation_wall_end_ns'] = time.time_ns()
            stop.set()
            thread.join(OBSERVER_TIMEOUT + 1)
            if thread.is_alive():
                self.close()
                thread.join(OBSERVER_TIMEOUT + 1)
                if thread.is_alive():
                    raise RuntimeError('persistent observer thread did not stop')
            window['monotonic_end_ns'] = time.monotonic_ns()

    def close(self):
        self.closed.set()
        try:
            if self.process.poll() is None:
                self.process.terminate()
                try:
                    self.process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    self.process.wait(timeout=3)
        finally:
            try:
                self.process.stdin.close()
            finally:
                self.process.stdout.close()


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


def inactive_file_bytes(text):
    rows = [line.split() for line in text.splitlines()
            if line.split() and line.split()[0] == 'inactive_file']
    if len(rows) != 1 or len(rows[0]) != 2:
        return None
    raw = rows[0][1]
    if not raw.isascii() or not raw.isdigit() or len(raw) > 20:
        return None
    value = int(raw)
    return value if value <= (1 << 64) - 1 else None


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
    inactive = None
    try:
        result = command('docker', 'exec', name, 'cat', base + 'memory.stat', check=False, timeout=10)
        if result.returncode == 0:
            inactive = inactive_file_bytes(result.stdout)
    except (OSError, subprocess.TimeoutExpired):
        pass  # Optional diagnostics cannot replace the original pressure verdict.
    used = values.get('memory.current')
    values['memory.stat.inactive_file'] = inactive
    values['diagnostic_working_set'] = {
        'bytes': max(0, used - inactive) if type(used) is int and inactive is not None else None,
        'unit': 'bytes',
        'source': '/sys/fs/cgroup/memory.current minus memory.stat inactive_file, clamped at zero',
        'scope': 'separate point-in-time readings; admission-test estimate, not an atomic runtime sample'}
    return values


def pressure_steps(current, limit):
    # Aim above the95% admission boundary so rollback/probe allocations do not
    # immediately make the second assertion meaningless. Keep a2% hard margin.
    target = (limit * INJECTION_PERCENT + 99) // 100
    first = max(0, (target - current - 8 * MIB) // MIB)
    return target, first


def pressured(snapshot):
    used = snapshot.get('diagnostic_working_set', {}).get('bytes')
    cap = snapshot.get('memory.max')
    if type(used) is not int or type(cap) is not int or used < 0 or cap <= 0:
        return None
    return used * 100 >= cap * 95


def fill_to_pressure(name, fill, samples, sample=None, *, limit=LIMIT):
    sample = sample or cgroup
    snapshot = sample(name)
    if snapshot['memory.max'] != limit or not isinstance(snapshot['memory.current'], int):
        raise SmokeInconclusive('finite cgroup-v2 reading disappeared during injection')
    current = snapshot.get('diagnostic_working_set', {}).get('bytes')
    if type(current) is not int:
        raise SmokeInconclusive('working-set estimate unavailable during injection')
    target, first = pressure_steps(current, limit)
    if snapshot['memory.current'] > limit * SAFETY_PERCENT // 100:
        raise SmokeInconclusive('usage already above the injection safety margin')
    if snapshot['memory.current'] + max(0, target - current) > limit * SAFETY_PERCENT // 100:
        raise SmokeInconclusive('cannot reach working-set target within the raw-usage safety margin')
    if first:
        docker('exec', name, 'dd', 'if=/dev/zero', f'of={fill}', 'bs=1048576',
               f'count={first}', 'oflag=append', 'conv=notrunc', 'status=none', timeout=60)
    for _ in range(40):
        snapshot = sample(name)
        samples.append(snapshot)
        raw = snapshot['memory.current']
        current = snapshot.get('diagnostic_working_set', {}).get('bytes')
        if type(raw) is not int or type(current) is not int or snapshot['memory.max'] != limit:
            raise SmokeInconclusive('cgroup reading disappeared or max changed during injection')
        if raw > limit * SAFETY_PERCENT // 100:
            raise SmokeInconclusive('usage exceeded the injection safety margin')
        if current >= target:
            return snapshot
        if raw + MIB > limit * SAFETY_PERCENT // 100:
            raise SmokeInconclusive('refusing another increment with insufficient headroom')
        docker('exec', name, 'dd', 'if=/dev/zero', f'of={fill}', 'bs=1048576',
               'count=1', 'oflag=append', 'conv=notrunc', 'status=none', timeout=10)
    raise SmokeInconclusive('pressure target not reached within bounded fill loop')


def require_pressure_error(name, operation, checks, sample=None, observer=None, phase=None, trace=False, *, limit=LIMIT):
    sample = sample or cgroup
    before = sample(name)
    failure = None
    operation_start_ns = None
    operation_end_ns = None
    with (observer.window(phase) if observer is not None else contextlib.nullcontext(None)) as window:
        try:
            if trace:
                operation_start_ns = time.time_ns()
            operation()
        except Exception as error:
            failure = error
        finally:
            if trace:
                operation_end_ns = time.time_ns()
    observer_error = window and window.get('observer_error')
    after, after_error = {}, None
    if not observer_error:
        try:
            after = sample(name, current_first=True)
        except SmokeInconclusive as error:
            if observer is None:
                raise
            after_error = error
    evidence = {'before': before, 'after': after,
                'pressure_basis': 'memory.current minus inactive_file (matching runtime estimate)',
                'scope': 'point-in-time samples; no continuous usage guarantee'}
    checks.append(evidence)
    if phase is not None:
        evidence['phase'] = phase
    if trace:
        evidence['operation_wall_start_ns'] = operation_start_ns
        evidence['operation_wall_end_ns'] = operation_end_ns
    if window is not None:
        evidence['observer_window'] = window
    if after_error is not None:
        evidence['observer_after_error'] = type(after_error).__name__
    if failure is not None:
        if getattr(failure, 'code', None) != PRESSURE_CODE:
            raise AssertionError(f'expected {PRESSURE_CODE}; got {type(failure).__name__} code={getattr(failure, "code", None)}') from failure
    if observer_error or after_error is not None:
        raise SmokeInconclusive('persistent observer failed during RUN; pressure evidence unavailable')
    if failure is not None:
        parsed = MemoryPressureError(failure)
        evidence.update({'code': failure.code, 'type': type(failure).__name__,
                         'reason': parsed.reason, 'rolled_back': parsed.rolled_back})
        if parsed.reason != 'cgroup' or parsed.rolled_back is not True:
            raise SmokeInconclusive('memory refusal did not confirm cgroup pressure and rollback in a valid server hint') from failure
        return evidence
    if before.get('memory.max') != limit or after.get('memory.max') != limit:
        raise SmokeInconclusive('configured cgroup limit was unavailable or changed around the admitted RUN')
    if pressured(before) is not True or pressured(after) is not True:
        raise SmokeInconclusive('RUN admitted without confirmed 95% working-set pressure in both bracketing samples')
    # External samples can perturb usage and do not establish the server's
    # decision-time reading, even when both sides exceed the threshold.
    raise SmokeInconclusive('RUN admitted with high external bracketing samples; decision-time pressure is unproven')


def stop_events(process):
    if process is not None and process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)


def finalize(report, run_id, name, volume, created, event_process, events_path):
    """Verify UUID ownership before each removal; cleanup never hides the failure."""
    errors = []
    def attempt(operation, action):
        try:
            return action()
        except Exception as error:
            errors.append({'operation': operation, 'error_type': type(error).__name__})
            return None

    report['owned_resources_cleaned'] = False
    attempt('stop observer', lambda: stop_events(event_process))
    state = attempt('inspect server evidence', lambda: inspect_owned('container', name, run_id))
    if state is not None:
        report.setdefault('cleanup_state', state.get('State', {}))
        if state.get('State', {}).get('Running'):
            snapshot = attempt('capture cgroup', lambda: cgroup(state['Id'], current_first=True))
            if snapshot is not None:
                report['cleanup_cgroup'] = snapshot
        def capture_logs():
            logs = command('docker', 'logs', state['Id'])
            lines = (logs.stdout + logs.stderr).splitlines()
            if report.get('admission_trace_enabled'):
                report['cleanup_admission_trace_lines'] = [line for line in lines
                    if 'memory pressure decision: trace ' in line][:64]
            return [line for line in lines
                    if 'memory pressure admission' in line or 'memory pressure decision' in line][-40:]
        captured = attempt('capture logs', capture_logs)
        if captured is not None:
            report['cleanup_pressure_log_lines'] = captured
    names = (name, name + '-init', name + '-audit')
    for resource in names:
        value = attempt('inspect ' + resource,
                        lambda resource=resource: inspect_owned('container', resource, run_id))
        if value is not None:
            identity = value.get('Id')
            if not isinstance(identity, str) or not identity:
                errors.append({'operation': 'identify ' + resource, 'error_type': 'MissingContainerId'})
                continue
            attempt('stop ' + resource, lambda identity=identity:
                    command('docker', 'stop', '--time', '10', identity, check=False, timeout=20))
            def remove(identity=identity):
                try:
                    docker('rm', identity, timeout=20)
                except Exception:
                    # --rm containers can disappear while docker stop is finishing.
                    if inspect_owned('container', identity, run_id) is not None:
                        raise
            attempt('remove ' + resource, remove)
    # A create timeout can occur after Docker creates the volume: only its
    # UUID ownership label and confirmed absence establish cleanup, not the ACK.
    value = attempt('inspect volume', lambda: inspect_owned('volume', volume, run_id))
    if value is not None:
        attempt('remove volume', lambda: docker('volume', 'rm', volume, timeout=20))
    resources = [('container', resource) for resource in names] + [('volume', volume)]
    for kind, resource in resources:
        value = attempt('verify absence ' + resource,
                        lambda kind=kind, resource=resource: inspect_owned(kind, resource, run_id))
        if value is not None:
            errors.append({'operation': 'verify absence ' + resource, 'error_type': 'ResourceRemaining'})
    def capture_events():
        events = []
        if events_path.exists():
            for line in events_path.read_text().splitlines():
                with contextlib.suppress(json.JSONDecodeError):
                    item = json.loads(line)
                    events.append({'action': item.get('Action', item.get('status')), 'time': item.get('time')})
        return events
    report['cleanup_docker_events'] = attempt('capture events', capture_events) or []
    if errors:
        report['cleanup_errors'] = errors
        report['result'] = 'FAIL'
    report['owned_resources_cleaned'] = not errors


def self_check(limit=LIMIT):
    target, first = pressure_steps(40 * MIB, limit)
    assert target == (limit * 96 + 99) // 100
    assert 40 * MIB + first * MIB <= target - 8 * MIB
    assert pressure_steps(limit, limit)[1] == 0
    print('self-check PASS; no Docker resources started')


def run(args, report):
    memory_mib = getattr(args, 'memory_mib', DEFAULT_MEMORY_MIB)
    if type(memory_mib) is not int or memory_mib not in MEMORY_MIB_CHOICES:
        raise ValueError('unsupported memory limit')
    limit = memory_mib * MIB
    from neo4j import GraphDatabase
    import neo4j
    run_id = uuid.uuid4().hex
    name = f'brainwire-pressure-smoke-{run_id}'
    volume = name + '-data'
    fill = f'/pressure/smoke-fill-{run_id}'
    event_process = None
    observer = None
    sample = cgroup
    report.update({'run_id': run_id, 'driver': neo4j.__version__, 'image': args.image,
                   'memory_limit_bytes': limit, 'threshold_percent': 95,
                   'pressure_basis': 'memory.current minus inactive_file; raw usage retained for injection safety',
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
            common = ['--pull=never', '--label', f'{LABEL}={run_id}', f'--memory={memory_mib}m',
                      f'--memory-swap={memory_mib}m',
                      '--mount', f'type=volume,src={volume},dst=/data',
                      '-e', 'BRAINWIRE_SERVER_DEFAULT_DATABASE=demo',
                      '-e', 'BRAINWIRE_MEMORY_CACHE=64MiB', '-e', 'BRAINWIRE_MEMORY_MEMTABLES=8MiB',
                      '-e', 'BRAINWIRE_MEMORY_QUERY=32MiB', '-e', 'BRAINWIRE_MEMORY_INGRESS=16MiB']
            if getattr(args, 'trace_admission', False):
                common.extend(['-e', 'BRAINWIRE_TRACE_CGROUP_ADMISSION=1'])
                report['admission_trace_enabled'] = True
            docker('run', '--rm', '--name', name + '-init', *common, report['image_id'],
                   'db', 'create', 'demo', '--ontology', '/opt/brainwire/example-schema.json', timeout=120)
            with events_path.open('w') as event_file:
                event_process = subprocess.Popen(['docker', 'events', '--filter', f'container={name}',
                                                  '--format', '{{json .}}'], stdout=event_file, stderr=subprocess.DEVNULL)
                docker('run', '-d', '--name', name, *common, '--tmpfs', f'/pressure:rw,size={memory_mib}m,mode=1777',
                       '-p', '127.0.0.1::7687', report['image_id'], 'serve', '--allow-unauthenticated-remote')
                state = inspect_owned('container', name, run_id)
                if state is None or state.get('Image') != report['image_id']:
                    raise RuntimeError('owned server image does not match the inspected local image ID')
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
                    if getattr(args, 'persistent_observer', False):
                        observer = PersistentCgroupObserver(name, run_id)
                        sample = observer.sample
                        report['pressure_sampling_method'] = 'persistent_shell_builtins'
                        report['observer_run_windows'] = observer.windows
                        report['observer_limits'] = {'samples_per_window': OBSERVER_MAX_SAMPLES,
                                                     'bytes_per_snapshot': OBSERVER_MAX_BYTES,
                                                     'bytes_per_window': OBSERVER_MAX_WINDOW_BYTES,
                                                     'max_windows': 8}
                        report['limitations'].append('persistent observer has fixed memory overhead; samples are not decision-time atomic')
                    report['before'] = sample(name)
                    before = report['before']
                    if before['memory.max'] != limit or not isinstance(before['memory.current'], int) or not isinstance(before['memory.events'], dict):
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
                            fill_to_pressure(name, fill, report['pressure_samples'], sample=sample, limit=limit)
                            report['pressure_phase'] = 'transaction_run'
                            report['rejected_transaction_run'] = require_pressure_error(name,
                                lambda: tx.run("CREATE (:Customer {entity_id:'rejected-under-pressure'})").consume(),
                                report.setdefault('run_pressure_checks', []), sample=sample, observer=observer,
                                phase='transaction_run', trace=getattr(args, 'trace_admission', False), limit=limit)
                            tx.rollback()
                        report['after_transaction_rollback'] = sample(name)
                        fill_to_pressure(name, fill, report['pressure_samples'], sample=sample, limit=limit)
                        # An independent session must also be denied. Driver
                        # callbacks are never automatically retried by this smoke.
                        with driver.session(database='demo') as other:
                            report['pressure_phase'] = 'new_session_run'
                            report['rejected_new_run'] = require_pressure_error(name, lambda: other.run('RETURN 1').consume(),
                                                                                report.setdefault('run_pressure_checks', []),
                                                                                sample=sample, observer=observer,
                                                                                phase='new_session_run',
                                                                                trace=getattr(args, 'trace_admission', False), limit=limit)
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
                    report['after_relief'] = sample(name)
                    for point in [report['before'], *report['pressure_samples'], report['after_relief']]:
                        events = point['memory.events']
                        if not isinstance(events, dict) or any(events.get(key, 0) for key in ('oom', 'oom_kill', 'oom_group_kill')):
                            raise AssertionError('missing or nonzero cgroup OOM evidence')
                report['final_state'] = inspect_owned('container', name, run_id)['State']
                if report['final_state'].get('OOMKilled') or not report['final_state'].get('Running'):
                    raise AssertionError('owned server stopped or Docker reports OOMKilled')
                log_result = command('docker', 'logs', name)
                logs = log_result.stdout + log_result.stderr
                report['pressure_log_lines'] = [line for line in logs.splitlines()
                    if 'memory pressure admission' in line or 'memory pressure decision' in line][-30:]
                if report.get('admission_trace_enabled'):
                    report['admission_trace_lines'] = [line for line in logs.splitlines()
                        if 'memory pressure decision: trace ' in line][:64]
                docker('stop', '--time', '30', name, timeout=40)
                report['offline_audit'] = json.loads(docker('run', '--rm', '--name', name + '-audit',
                                                          *common, report['image_id'], 'db', 'check', 'demo', '--json', timeout=120))
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
            observer_error = None
            if observer is not None:
                try:
                    observer.close()
                except Exception as error:
                    observer_error = {'operation': 'stop persistent observer', 'error_type': type(error).__name__}
            try:
                finalize(report, run_id, name, volume, created, event_process, events_path)
            finally:
                if observer_error is not None:
                    report.setdefault('cleanup_errors', []).append(observer_error)
                    report['owned_resources_cleaned'] = False
                    report['result'] = 'FAIL'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--image')
    parser.add_argument('--output', type=Path)
    parser.add_argument('--memory-mib', type=int, choices=MEMORY_MIB_CHOICES, default=DEFAULT_MEMORY_MIB,
                        help='container memory limit in MiB (default: %(default)s)')
    parser.add_argument('--self-check', action='store_true')
    parser.add_argument('--persistent-observer', action='store_true',
                        help='opt-in diagnostic: sample with one owned shell instead of per-sample execs')
    parser.add_argument('--trace-admission', action='store_true',
                        help='opt-in: log a bounded number of server admission decisions')
    args = parser.parse_args()
    if args.self_check:
        self_check(args.memory_mib * MIB)
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
            cleanup_failed = bool(report.get('cleanup_errors')) or report.get('owned_resources_cleaned') is False
            report['result'] = 'FAIL' if observed_oom or cleanup_failed else 'INCONCLUSIVE'
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
