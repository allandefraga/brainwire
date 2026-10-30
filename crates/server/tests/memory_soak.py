#!/usr/bin/env python3
"""Synthetic sustained-load qualification on a prebuilt Linux cgroup-v2 image.

Run separately for --memory-gib 2, 4 and 8 on an isolated Docker host. Default:
8 writers, 4 readers, 30 minutes; --rows-per-writer 500000 adds a 4M fixture.
Short runs are diagnostic and cannot produce a qualified receipt. No builds or
pulls occur. Only UUID-labelled resources created here are removed.
Requires the Neo4j Python driver. --self-check starts no Docker resources.
"""
from __future__ import annotations
import argparse
import hashlib
import json
import math
import platform
from pathlib import Path
import re
import subprocess
import tempfile
import threading
import time
import uuid
import allocator_decay_probe as probe
import cgroup_external_pressure as pressure

WRITE = ('UNWIND $rows AS r MERGE (n:Customer {entity_id:r.id}) '
         'SET n.name=r.payload')
READ = 'MATCH (n:Customer) RETURN n'



def read_power_profile():
    try:
        value = pressure.command('powerprofilesctl', 'get', timeout=5, check=False)
    except (OSError, subprocess.TimeoutExpired):
        return None
    profile = value.stdout.strip()
    return profile if value.returncode == 0 and profile in (
        'performance', 'balanced', 'power-saver') else None


def linux_power_evidence(supply_root=Path('/sys/class/power_supply'),
                         policy_root=Path('/sys/devices/system/cpu/cpufreq')):
    def read(path):
        try:
            return path.read_text().strip()
        except OSError:
            return None
    try:
        supplies = [dict(name=path.name, **{key: read(path / key)
                    for key in ('type', 'present', 'online', 'status')})
                    for path in sorted(supply_root.iterdir())]
        supply_scan_complete = all(value['type'] is not None for value in supplies)
    except OSError:
        supplies, supply_scan_complete = [], False
    batteries = [value for value in supplies if value['type'] in ('Battery', 'UPS')
                 and value['present'] != '0']
    external_online = any(value['type'] == 'Mains' and value['online'] == '1'
                          for value in supplies)
    source_eligible = supply_scan_complete and external_online
    profile = read_power_profile()
    try:
        policies = [dict(name=path.name,
            preference=read(path / 'energy_performance_preference'),
            governor=read(path / 'scaling_governor'))
            for path in sorted(policy_root.glob('policy*'))]
    except OSError:
        policies = []
    explicit_low_power = any(value['preference'] in ('power', 'balance_power')
                             for value in policies)
    all_performance = bool(policies) and all(
        value['preference'] in ('performance', 'balance_performance') or
        (value['preference'] is None and value['governor'] == 'performance')
        for value in policies)
    if profile == 'power-saver' or explicit_low_power:
        normal_mode = False
    elif profile in ('performance', 'balanced') or all_performance:
        normal_mode = True
    else:
        normal_mode = None
    return {'eligible': source_eligible and normal_mode is True,
        'supply_scan_complete': supply_scan_complete, 'supplies': supplies,
        'battery_detected': bool(batteries) if supply_scan_complete else None,
        'power_source': ('external_online' if external_online else
                         'battery_or_unconfirmed_external' if batteries else
                         'no_battery_detected' if supply_scan_complete else 'unknown'),
        'ac_confirmed': external_online,
        'profile': profile, 'cpu_policies': policies, 'normal_power_mode': normal_mode,
        'scope': ('OS-visible supplies and power policy; no inference about a VM host, '
                  'firmware or physical AC from missing batteries; a virtual Mains '
                  'device does not prove the physical VM host power source')}


def power_receipt():
    receipt = pressure.power_receipt()
    receipt.pop('power_verification', None)
    receipt['power_evidence'] = linux_power_evidence()
    return receipt


def power_eligible(receipt):
    evidence = receipt.get('power_evidence', {})
    return (evidence.get('eligible') is True
            and evidence.get('ac_confirmed') is True
            and evidence.get('supply_scan_complete') is True
            and evidence.get('normal_power_mode') is True)


def require_power(receipt):
    if not power_eligible(receipt):
        raise pressure.Inconclusive('AC/source or normal power mode cannot be verified')


def require_same_power(before, current):
    require_power(before)
    require_power(current)
    if before['power_evidence'] != current['power_evidence']:
        raise pressure.Inconclusive('observed power supply or policy changed during workload')

def image_identity(value):
    identity = value.get('Id', '')
    if not re.fullmatch(r'sha256:[0-9a-f]{64}', identity):
        raise ValueError('image must resolve to an immutable Docker image ID')
    return identity



def host_boot_id():
    if platform.system() != 'Linux':
        raise pressure.Inconclusive('kernel evidence requires a native Linux host')
    try:
        value = Path('/proc/sys/kernel/random/boot_id').read_text().strip()
    except OSError as error:
        raise pressure.Inconclusive('host boot identity is unavailable') from error
    if not re.fullmatch(r'[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}', value):
        raise pressure.Inconclusive('host boot identity is invalid')
    return value


def kernel_snapshot():
    boot_id = host_boot_id()
    result = pressure.command('dmesg', '--raw', '--color=never', check=False, timeout=10)
    lines = result.stdout.splitlines()
    if result.returncode or not lines:
        raise pressure.Inconclusive('readable nonempty host kernel log required')
    if host_boot_id() != boot_id:
        raise pressure.Inconclusive('host boot identity changed while reading kernel log')
    return {'boot_id': boot_id, 'lines': lines}


def verify_kernel_host(name, snapshot):
    result = pressure.command('docker', 'exec', name, 'cat',
        '/proc/sys/kernel/random/boot_id', check=False, timeout=10)
    if (result.returncode or result.stdout.strip() != snapshot['boot_id']
            or host_boot_id() != snapshot['boot_id']):
        raise pressure.Inconclusive('Docker daemon and observer kernel identities differ')


def kernel_increment(before, after):
    if before['boot_id'] != after['boot_id']:
        raise pressure.Inconclusive('host boot identity changed during workload')
    baseline, final = before['lines'], after['lines']
    # Exact prefix prevents a ring-buffer rollover/clear from hiding OOM lines.
    if not baseline or final[:len(baseline)] != baseline:
        raise pressure.Inconclusive('kernel log rolled over, was cleared or changed')
    added = final[len(baseline):]
    matched = [line for line in added if any(term in line.lower() for term in
        ('out of memory', 'killed process', 'oom-kill', 'oom_reaper', 'memory cgroup'))]
    def summary(value):
        encoded = '\n'.join(value['lines']).encode()
        return {'boot_id': value['boot_id'], 'line_count': len(value['lines']),
                'sha256': hashlib.sha256(encoded).hexdigest()}
    # Full boot logs can contain command-line secrets; retain hashes and OOM
    # evidence only. Raw snapshots are used in memory for the exact-prefix check.
    return {'available': True, 'identity_verified': True, 'continuous_prefix': True,
            'scope': 'new records from the verified host kernel during this run',
            'before': summary(before), 'after': summary(after),
            'new_line_count': len(added), 'matched_lines': matched}

def validate_sample(point, limit):
    if point.get('memory.max') != limit or point.get('memory.swap.max') != 0:
        raise pressure.Inconclusive('exact memory limit and disabled swap required')
    if point.get('pid1.cgroup') != point.get('exec.cgroup'):
        raise pressure.Inconclusive('PID1 and sampler cgroups differ')
    if not {'oom', 'oom_kill'} <= point.get('memory.events', {}).keys():
        raise pressure.Inconclusive('OOM event counters unavailable')
    if not {'VmRSS', 'VmHWM'} <= point.get('pid1_memory_kib', {}).keys():
        raise pressure.Inconclusive('process RSS/high-water unavailable')
    if not all(isinstance(point.get(key), int) and point[key] >= 0
               for key in ('memory.current', 'memory.peak')):
        raise pressure.Inconclusive('cgroup memory measurements unavailable')


def validate_state(state, image_id):
    if state is None or state.get('Image') != image_id:
        raise AssertionError('owned server is absent or image identity changed')
    if (not state['State'].get('Running') or state['State'].get('OOMKilled')
            or state.get('RestartCount') != 0):
        raise AssertionError('owned server died, restarted or reported OOMKilled')


def qualification(report):
    """No qualification for short/idle/failed tests or missing kernel evidence."""
    return (report.get('result') == 'PASS'
            and report.get('duration_seconds', 0) >= 1800
            and report.get('elapsed_seconds', 0) >= report['duration_seconds']
            and report.get('writers') == 8 and report.get('readers') == 4
            and len(report.get('workers', [])) == 12
            and all(item.get('operations', 0) > 0 and not item.get('errors')
                    for item in report['workers'])
            and len(report.get('samples', [])) >= 3
            and report.get('audit', {}).get('status') == 'ok'
            and report.get('kernel_log', {}).get('available') is True
            and report['kernel_log'].get('identity_verified') is True
            and report['kernel_log'].get('continuous_prefix') is True
            and not report['kernel_log'].get('matched_lines')
            and report.get('owned_resources_cleaned') is True
            and power_eligible(report.get('host_before', {}))
            and power_eligible(report.get('host_after', {}))
            and report['host_after']['power_evidence'] == report['host_before']['power_evidence']
            and all(power_eligible(point.get('host_power', {}))
                    and point['host_power']['power_evidence'] == report['host_before']['power_evidence']
                    for point in report['samples']))


def rows_for(worker, batch_index, batch, per_worker, payload):
    # Disjoint stable key sets prevent artificial unbounded fixture growth.
    start = batch_index * batch % per_worker
    return [{'id': f'soak-{worker}-{(start + n) % per_worker:08d}',
             'payload': payload} for n in range(batch)]


def worker(driver, kind, index, args, stop, ready, go, result):
    ready.set()
    go.wait()
    cycles = math.ceil(args.rows_per_writer / args.batch)
    iteration = 0
    try:
        with driver.session(database='demo', fetch_size=200) as session:
            while not stop.is_set():
                started = time.monotonic()
                with session.begin_transaction(timeout=30) as tx:
                    if kind == 'writer':
                        payload = ('x' if iteration // cycles % 2 else 'y') * args.payload_bytes
                        tx.run(WRITE, rows=rows_for(index, iteration, args.batch,
                            args.rows_per_writer, payload)).consume()
                    else:
                        for _ in tx.run(READ):
                            pass
                    tx.commit()
                result['operations'] += 1
                result['max_operation_seconds'] = max(
                    result['max_operation_seconds'], time.monotonic() - started)
                iteration += 1
    except Exception as error:
        # Arbitrary exception text may contain secrets; retain type/code only.
        result['errors'].append({'type': type(error).__name__,
                                 'code': getattr(error, 'code', None)})
        stop.set()


def join_threads(threads, seconds):
    deadline = time.monotonic() + seconds
    for thread in threads:
        thread.join(max(0, deadline - time.monotonic()))


def run(args, report):
    from neo4j import GraphDatabase, __version__ as driver_version
    if pressure.docker('ps', '-q').strip():
        raise pressure.Inconclusive('isolated Docker host required: other containers are running')
    kernel_before = kernel_snapshot()
    info = json.loads(pressure.docker('image', 'inspect', args.image))[0]
    image_id = image_identity(info)
    identity = uuid.uuid4().hex
    name = 'brainwire-memory-soak-' + identity
    volume = name + '-data'
    limit = args.memory_gib << 30
    report.update(run_id=identity, image=args.image, image_id=image_id,
        image_revision=(info.get('Config', {}).get('Labels') or {}).get('org.opencontainers.image.revision'),
        driver=driver_version, synthetic=True, memory_limit_bytes=limit,
        duration_seconds=args.seconds, writers=args.writers, readers=args.readers,
        initial_rows=args.rows_per_writer * args.writers, batch=args.batch,
        payload_bytes=args.payload_bytes, samples=[], workers=[],
        limitations=['synthetic disjoint-key upserts and full streamed entity reads',
                     'no network partition, power-loss or real-source qualification',
                     'native Linux host with readable dmesg required; remote/Desktop daemons rejected',
                     'sampling cannot observe every transient RSS allocation',
                     'initial fixture load is sequential and excluded from timed soak',
                     '4M initial rows require --writers 8 --rows-per-writer 500000',
                     'cgroup-v2 counters and readable kernel log are mandatory',
                     'OS-visible external power online and normal policy required, including servers/VMs',
                     'power observations at each memory sample cannot prove continuous power conditions',
                     'only Mains online counts as OS-visible AC; USB/UPS never count',
                     'virtual Mains status does not establish physical VM-host AC'])
    report['host_before'] = power_receipt()
    require_power(report['host_before'])
    common = ['--pull=never', '--label', f'{pressure.LABEL}={identity}',
        '--cpus=4', f'--memory={limit}', f'--memory-swap={limit}',
        '--mount', f'type=volume,src={volume},dst=/data',
        '-e', 'BRAINWIRE_SERVER_DEFAULT_DATABASE=demo',
        '-e', 'BRAINWIRE_MEMORY_CACHE=128MiB', '-e', 'BRAINWIRE_MEMORY_MEMTABLES=16MiB',
        '-e', 'BRAINWIRE_MEMORY_QUERY=64MiB', '-e', 'BRAINWIRE_MEMORY_INGRESS=32MiB']
    stop, go = threading.Event(), threading.Event()
    threads, driver, events_process = [], None, None
    with tempfile.TemporaryDirectory(prefix='bw-soak-events-') as temporary:
        events_path = Path(temporary) / 'events.jsonl'
        try:
            pressure.docker('volume', 'create', '--label', f'{pressure.LABEL}={identity}', volume)
            pressure.inspect_owned('volume', volume, identity)
            pressure.docker('run', '--rm', '--name', name + '-init', *common,
                image_id, 'db', 'create', 'demo', '--ontology',
                '/opt/brainwire/example-schema.json', timeout=120)
            with events_path.open('w') as events:
                events_process = subprocess.Popen(['docker', 'events', '--filter',
                    f'container={name}', '--format', '{{json .}}'],
                    stdout=events, stderr=subprocess.DEVNULL)
                pressure.docker('run', '-d', '--name', name, *common,
                    '-p', '127.0.0.1::7687', image_id,
                    'serve', '--allow-unauthenticated-remote')
                state = pressure.inspect_owned('container', name, identity)
                validate_state(state, image_id)
                verify_kernel_host(name, kernel_before)
                port = state['NetworkSettings']['Ports']['7687/tcp'][0]['HostPort']
                driver = GraphDatabase.driver(f'bolt://127.0.0.1:{port}', auth=None,
                    connection_timeout=5, connection_acquisition_timeout=10,
                    max_transaction_retry_time=0,
                    max_connection_pool_size=args.writers + args.readers + 1)
                deadline = time.monotonic() + 40
                while True:
                    try:
                        driver.verify_connectivity()
                        break
                    except Exception:
                        if time.monotonic() >= deadline:
                            raise TimeoutError('owned server did not become ready')
                        time.sleep(.2)
                report['before'] = pressure.sample(name)
                validate_sample(report['before'], limit)
                initial = time.monotonic()
                with driver.session(database='demo') as session:
                    for writer_index in range(args.writers):
                        for start in range(0, args.rows_per_writer, args.batch):
                            if time.monotonic() - initial > args.load_timeout:
                                raise TimeoutError('initial fixture deadline exceeded')
                            count = min(args.batch, args.rows_per_writer - start)
                            rows = [{'id': f'soak-{writer_index}-{n:08d}',
                                     'payload': 'x' * args.payload_bytes}
                                    for n in range(start, start + count)]
                            with session.begin_transaction(timeout=30) as tx:
                                tx.run(WRITE, rows=rows).consume()
                                tx.commit()
                    actual = session.run('MATCH (n:Customer) RETURN count(n) AS n').single()['n']
                    if actual != report['initial_rows']:
                        raise AssertionError('initial row count mismatch')
                report['initial_load_seconds'] = time.monotonic() - initial
                ready_events = []
                for kind, count in [('writer', args.writers), ('reader', args.readers)]:
                    for index in range(count):
                        result = {'kind': kind, 'index': index, 'operations': 0,
                                  'max_operation_seconds': 0, 'errors': []}
                        report['workers'].append(result)
                        ready = threading.Event()
                        thread = threading.Thread(target=worker,
                            args=(driver, kind, index, args, stop, ready, go, result), daemon=True)
                        threads.append(thread)
                        ready_events.append(ready)
                        thread.start()
                if not all(ready.wait(10) for ready in ready_events):
                    raise TimeoutError('workers failed to start')
                started = time.monotonic()
                go.set()
                while True:
                    if events_process.poll() is not None:
                        raise pressure.Inconclusive('Docker event observer exited unexpectedly')
                    point = pressure.sample(name)
                    point['host_power'] = power_receipt()
                    report['samples'].append(point)
                    require_same_power(report['host_before'], point['host_power'])
                    validate_sample(point, limit)
                    if not pressure.no_new_oom(report['before']['memory.events'], point['memory.events']):
                        raise AssertionError('cgroup OOM counters increased')
                    validate_state(pressure.inspect_owned('container', name, identity), image_id)
                    elapsed = time.monotonic() - started
                    report['elapsed_seconds'] = elapsed
                    if stop.is_set():
                        raise AssertionError('workload operation failed')
                    if elapsed >= args.seconds:
                        break
                    stop.wait(min(args.sample_seconds, args.seconds - elapsed))
                stop.set()
                join_threads(threads, 35)
                if any(thread.is_alive() for thread in threads):
                    raise TimeoutError('workload failed to stop within deadline')
                if any(not item['operations'] or item['errors'] for item in report['workers']):
                    raise AssertionError('worker failed or made no progress')
                driver.close()
                driver = None
                report['metrics'] = probe.metrics([report['before'], *report['samples']])
                kernel_after = kernel_snapshot()
                verify_kernel_host(name, kernel_after)
                report['kernel_log'] = kernel_increment(kernel_before, kernel_after)
                if report['kernel_log']['matched_lines']:
                    raise AssertionError('host kernel recorded new OOM evidence')
                state = pressure.inspect_owned('container', name, identity)
                validate_state(state, image_id)
                report['state_before_stop'] = state['State']
                report['restart_count_before_stop'] = state['RestartCount']
                pressure.docker('stop', '--time', '30', name, timeout=40)
                report['stopped_state'] = pressure.inspect_owned('container', name, identity)['State']
                if report['stopped_state']['ExitCode'] != 0:
                    raise AssertionError('owned server did not stop cleanly')
                report['audit'] = json.loads(pressure.docker('run', '--rm', '--name', name + '-audit',
                    *common, image_id, 'db', 'check', 'demo', '--json', timeout=300))
                if report['audit'].get('status') != 'ok':
                    raise AssertionError('offline db check failed')
                report['result'] = 'PASS'
        finally:
            stop.set()
            go.set()
            # Stopping the owned server also unblocks outstanding socket reads.
            probe.finalize(report, identity, name, volume, events_process, events_path)
            try:
                report['host_after'] = power_receipt()
                require_same_power(report['host_before'], report['host_after'])
            except Exception as error:
                report['power_observation_error_type'] = type(error).__name__
                if report.get('result') == 'PASS':
                    report['result'] = 'INCONCLUSIVE'
            join_threads(threads, 5)
            if any(thread.is_alive() for thread in threads):
                report['result'] = 'FAIL'
                report['worker_shutdown_incomplete'] = True
            elif driver is not None:
                try:
                    driver.close()
                except Exception as error:
                    report['result'] = 'FAIL'
                    report['driver_shutdown_error_type'] = type(error).__name__


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--image')
    parser.add_argument('--output', type=Path)
    parser.add_argument('--memory-gib', type=int, choices=(2, 4, 8), default=2)
    parser.add_argument('--seconds', type=int, default=1800)
    parser.add_argument('--writers', type=int, default=8)
    parser.add_argument('--readers', type=int, default=4)
    parser.add_argument('--rows-per-writer', type=int, default=12500)
    parser.add_argument('--batch', type=int, default=100)
    parser.add_argument('--payload-bytes', type=int, default=256)
    parser.add_argument('--sample-seconds', type=int, default=5)
    parser.add_argument('--load-timeout', type=int, default=3600)
    parser.add_argument('--self-check', action='store_true')
    args = parser.parse_args()
    if args.self_check:
        assert rows_for(1, 2, 2, 5, 'x')[0]['id'] == 'soak-1-00000004'
        assert not qualification({'result': 'PASS'})
        print('self-check PASS; no Docker resources started')
        return 0
    if not args.image or not args.output:
        parser.error('--image and --output are required')
    for key in ('seconds', 'writers', 'readers', 'rows_per_writer', 'batch',
                'payload_bytes', 'sample_seconds', 'load_timeout'):
        if getattr(args, key) <= 0:
            parser.error(f'--{key.replace("_", "-")} must be positive')
    if args.writers > 8 or args.readers > 4 or args.rows_per_writer > 500000 \
            or args.batch > 1000 or args.batch > args.rows_per_writer \
            or args.payload_bytes > 4096 or args.seconds > 7200 or args.sample_seconds > 30:
        parser.error('requested workload exceeds harness safety bounds')
    report = {'result': 'FAIL', 'qualified': False}
    try:
        run(args, report)
    except pressure.Inconclusive as error:
        if not probe.failure_observed(report) and not report.get('cleanup_errors'):
            report['result'] = 'INCONCLUSIVE'
        report['error'] = str(error)
    except Exception as error:
        report['result'] = 'FAIL'
        report['error_type'] = type(error).__name__
        report['error_code'] = getattr(error, 'code', None)
    report['qualified'] = qualification(report)
    args.output.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({'result': report['result'], 'qualified': report['qualified'],
                      'receipt': str(args.output)}))
    return 0 if report['qualified'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
