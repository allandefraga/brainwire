"""Opt-in synthetic mixed-load diagnostic in a new 4-GiB Docker cgroup.

Never attaches an existing graph/volume. See bench_v08_container.md before running.
"""
import argparse
from collections import Counter
import hashlib
import json
import math
import os
from pathlib import Path
import secrets
import subprocess
import sys
import threading
import time
import uuid

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
from scripts.bench_v07_bolt import host_receipt

LIMIT = 4 * 1024**3
LABEL = 'io.brainwire.benchmark'
UPSERT = ('UNWIND $rows AS row MERGE (n:Bench {entity_id:row.id}) '
          'SET n.payload=row.payload,n.sequence=row.sequence')
EDGES = ("UNWIND $rows AS row MATCH (a:Bench {entity_id:row.id}), "
         "(b:Bench {entity_id:'n000000000000'}) MERGE (a)-[:LINK]->(b)")
CGROUP = """set -eu
for f in memory.current memory.peak memory.max memory.events memory.stat; do
  printf '\\n@@%s\\n' "$f"
  cat "/sys/fs/cgroup/$f"
done
printf '\\n@@process_status\\n'
cat /proc/1/status
"""


def docker(*args, timeout=60):
    result = subprocess.run(['docker', *args], capture_output=True, text=True, timeout=timeout)
    if result.returncode:
        # Docker errors may contain command/config details; do not include secrets.
        raise RuntimeError('docker command failed: ' + args[0])
    return result.stdout + (result.stderr if args[0] == 'logs' else '')


def parse_sample(raw):
    sections = {}
    current = None
    for line in raw.splitlines():
        if line.startswith('@@'):
            current = line[2:]
            sections[current] = []
        elif current is not None and line.strip():
            sections[current].append(line)
    def scalar(name):
        value = '\n'.join(sections[name])
        return None if value == 'max' else int(value)
    def pairs(name):
        return {key: int(value) for key, value in (line.split() for line in sections[name])}
    status = {}
    for line in sections['process_status']:
        parts = line.split()
        if parts[0] in ('VmRSS:', 'RssAnon:', 'RssFile:', 'RssShmem:'):
            if len(parts) != 3 or parts[2] != 'kB':
                raise ValueError('unexpected /proc status units')
            status[parts[0][:-1] + '_bytes'] = int(parts[1]) * 1024
    if 'VmRSS_bytes' not in status:
        raise ValueError('process RSS is missing')
    return dict(current_bytes=scalar('memory.current'), peak_bytes=scalar('memory.peak'),
                max_bytes=scalar('memory.max'), events=pairs('memory.events'),
                stat=pairs('memory.stat'), process=status)


def percentile(hist):
    total = sum(hist.values())
    result = {'requests': total}
    for name, fraction in [('p50', .5), ('p95', .95), ('p99', .99)]:
        position = 0
        for micros, count in sorted(hist.items()):
            position += count
            if position >= math.ceil(total * fraction):
                result[name + '_ms'] = micros / 1000
                break
    return result


def identifier(i):
    return f'n{i:012d}'


def batches(rows, batch_size, worker, workers):
    # Disjoint contiguous batches: no writer shares a node or edge with another.
    for start in range(1 + worker * batch_size, rows, workers * batch_size):
        yield [{'id': identifier(i), 'sequence': i, 'payload': hashlib.sha256(str(i).encode()).hexdigest() * 8}
               for i in range(start, min(start + batch_size, rows))]


def owned_cleanup(name, volume, run_id):
    # Only exact resources created by this invocation, guarded by random ownership labels.
    for kind, resource in [('container', name + '-offline'), ('container', name), ('volume', volume)]:
        try:
            objects = json.loads(docker(kind, 'inspect', resource))
        except RuntimeError:
            continue
        labels = objects[0].get('Config', {}).get('Labels', {}) if kind == 'container' else objects[0].get('Labels', {})
        if labels.get(LABEL) != run_id:
            raise RuntimeError('refusing cleanup: resource ownership mismatch')
        docker(kind, 'rm', *(['-f'] if kind == 'container' else []), resource)


def evaluate_memory(samples):
    if not samples:
        raise AssertionError('no cgroup samples')
    for sample in samples:
        if sample['max_bytes'] != LIMIT:
            raise AssertionError('cgroup hard cap differs from 4 GiB')
        if sample['events'].get('oom', 0) or sample['events'].get('oom_kill', 0):
            raise AssertionError('cgroup reported OOM')
        if sample['current_bytes'] > LIMIT or sample['peak_bytes'] > LIMIT:
            raise AssertionError('cgroup exceeded cap')


def idle_samples(sample, phase, seconds, interval):
    until = time.monotonic() + seconds
    while time.monotonic() < until:
        sample(phase)
        time.sleep(min(interval, max(0, until - time.monotonic())))
    sample(phase + '-end')


def reconnect_driver(factory, name, token, clients, previous=None):
    if previous is not None:
        previous.close()
    inspect = json.loads(docker('inspect', name))[0]
    port = inspect['NetworkSettings']['Ports']['7687/tcp'][0]['HostPort']
    uri = 'bolt://127.0.0.1:' + port
    return factory(uri, auth=('bench', token), connection_timeout=10,
                   connection_acquisition_timeout=30, max_connection_pool_size=clients + 2), uri


def restart_after_idle(name, sample, reconnect, ready, seconds, interval, stage):
    stage('post-load-idle')
    idle_samples(sample, 'post-load-idle', seconds, interval)
    stage('crash-restart')
    docker('kill', '--signal', 'KILL', name)
    state = json.loads(docker('inspect', name))[0]['State']
    if state.get('OOMKilled'):
        raise AssertionError('container was OOM-killed')
    docker('start', name)
    # Docker may assign a different ephemeral host port after start.
    reconnect()
    stage('recovery-readiness')
    ready()
    sample('recovered')
    return state


def summarize_memory(samples):
    summary = {}
    for item in samples:
        value = summary.setdefault(item['phase'], dict(samples=0, current_peak_bytes=0, rss_peak_bytes=0))
        value['samples'] += 1
        value['current_peak_bytes'] = max(value['current_peak_bytes'], item['current_bytes'])
        value['rss_peak_bytes'] = max(value['rss_peak_bytes'], item['process']['VmRSS_bytes'])
        value['last_current_bytes'] = item['current_bytes']
        value['last_rss_bytes'] = item['process']['VmRSS_bytes']
        value['last_anon_bytes'] = item['process'].get('RssAnon_bytes')
    return summary


def measure_query(execute, session, text, **params):
    started = time.perf_counter()
    rows = execute(session, text, **params)
    return rows, time.perf_counter() - started


def walk_cursor(session, expected_rows, timeout, max_seconds, page_size=1000):
    """Verify every synthetic ID without requesting delegated visibility."""
    text = ('MATCH (n:Bench) WHERE n.entity_id > $after '
            f'RETURN n.entity_id AS id ORDER BY n.entity_id LIMIT {page_size}')
    result = dict(query=text, page_size=page_size, pages=[], rows_seen=0,
                  expected_rows=expected_rows, complete=False, query_seconds=0.0)
    after = ''
    started = time.monotonic()
    while (len(result['pages']) < math.ceil(expected_rows / page_size) + 1
           and time.monotonic() - started < max_seconds):
        began = time.perf_counter()
        with session.begin_transaction(timeout=timeout) as tx:
            rows = tx.run(text, after=after).data()
            tx.commit()
        elapsed = time.perf_counter() - began
        expected = [{'id': identifier(i)} for i in
                    range(result['rows_seen'], min(result['rows_seen'] + page_size, expected_rows))]
        if rows != expected:
            raise AssertionError('keyset page differs from expected synthetic IDs')
        result['query_seconds'] += elapsed
        result['rows_seen'] += len(rows)
        result['pages'].append(dict(after=after, rows=len(rows), seconds=elapsed))
        if not rows:
            result['complete'] = result['rows_seen'] == expected_rows
            break
        after = rows[-1]['id']
    result['seconds'] = time.monotonic() - started
    result['status'] = 'ok' if result['complete'] else 'incomplete'
    return result


def run(args):
    from neo4j import GraphDatabase, __version__
    output = args.output.resolve()
    output.mkdir(mode=0o700)  # Refuse existing directories, even if empty.
    run_id = uuid.uuid4().hex
    name, volume = 'brainwire-v08-' + run_id, 'brainwire-v08-data-' + run_id
    report = dict(status='failed', synthetic=True, run_id=run_id, image=args.image,
                  rows=args.rows, batch_size=args.batch_size, writers=args.writers,
                  readers=args.readers, driver=__version__, host=host_receipt(),
                  image_identity=None, samples=[],
                  gaps=['No distributed replication or power-loss simulation.',
                        'Keyset cursor uses separate transactions, not a snapshot cursor.',
                        'Timeout is a Bolt transaction timeout request; enforcement is not qualified by this harness.',
                        'Idle RSS is measured, not asserted to equal cache: allocator and graph metadata also consume RAM.',
                        'Docker Desktop VM memory/CPU may constrain results; host power receipt is informational.'])
    policy = output / 'policy.json'
    ontology = output / 'ontology.json'
    token = secrets.token_hex(32)
    policy.write_text(json.dumps({'principals': [dict(id='bench', token_sha256=hashlib.sha256(token.encode()).hexdigest(),
                                                      graphs={'bench': 'write'}, instance_admin=True)]}))
    ontology.write_text(json.dumps(dict(version='1', nodes=[dict(label='Bench', properties=['payload', 'sequence'], indexed_fields=[])],
                                        edges=[dict(type='LINK', **{'from': {'label': 'Bench'}, 'to': {'label': 'Bench'}}, properties=[])])))
    # Host output directory is private; only these individual files are mounted read-only.
    policy.chmod(0o444)
    ontology.chmod(0o444)
    common = ['--mount', f'type=volume,src={volume},dst=/data', '--mount', f'type=bind,src={policy},dst=/bench-policy.json,readonly',
              '--mount', f'type=bind,src={ontology},dst=/bench-ontology.json,readonly',
              '-e', 'BRAINWIRE_SERVER_DATA_DIR=/data', '-e', 'BRAINWIRE_SERVER_DEFAULT_DATABASE=bench',
              '-e', 'BRAINWIRE_SERVER_AUTH_FILE=/bench-policy.json', '-e', 'BRAINWIRE_SERVER_LISTEN=0.0.0.0:7687']
    if args.explicit_budgets:
        common += ['-e', 'BRAINWIRE_MEMORY_CACHE=512MiB', '-e', 'BRAINWIRE_MEMORY_MEMTABLES=64MiB',
                   '-e', 'BRAINWIRE_MEMORY_QUERY=256MiB']
    report['memory_profile'] = 'explicit-512-64-256-MiB' if args.explicit_budgets else 'image-defaults'
    stop, workers_done = threading.Event(), threading.Event()
    threads, results = [], []
    samples_lock = threading.Lock()
    driver = None
    def stage(value):
        report['stage'] = value
    def reconnect():
        nonlocal driver
        driver, uri = reconnect_driver(GraphDatabase.driver, name, token, args.writers + args.readers, driver)
        report.setdefault('endpoints', []).append(dict(uri=uri, monotonic_seconds=time.monotonic()))
    def sample(phase):
        item = parse_sample(docker('exec', name, 'sh', '-c', CGROUP, timeout=10))
        item.update(phase=phase, monotonic_seconds=time.monotonic())
        with samples_lock:
            report['samples'].append(item)
        return item
    def query(session, text, **params):
        # Explicit transaction, no automatic retry: acknowledged writes have one attempt.
        with session.begin_transaction(timeout=args.query_timeout) as tx:
            data = tx.run(text, params).data()
            tx.commit()
            return data
    def ready():
        deadline = time.monotonic() + 120
        attempts = 0
        while time.monotonic() < deadline:
            try:
                attempts += 1
                with driver.session(database='bench') as session:
                    query(session, 'MATCH (n:Bench) RETURN count(*) AS count')
                report['readiness'] = dict(stage=report.get('stage'), attempts=attempts, status='ready')
                return
            except Exception as error:
                report['readiness'] = dict(stage=report.get('stage'), attempts=attempts,
                                           last_error_type=type(error).__name__)
                code = getattr(error, 'code', None)
                if isinstance(code, str) and len(code) <= 160 and all(c.isalnum() or c in '._' for c in code):
                    report['readiness']['last_error_code'] = code
                if not json.loads(docker('inspect', name))[0]['State']['Running']:
                    raise RuntimeError('server stopped during readiness')
                time.sleep(.5)
        raise TimeoutError('server readiness deadline')
    try:
        stage('image-inspection')
        report['image_identity'] = docker_image_revision(args.image)
        stage('isolated-setup')
        docker('volume', 'create', '--label', f'{LABEL}={run_id}', volume)
        # Run setup under the same hard limit, and never against an existing volume.
        docker('run', '--pull=never', '--rm', '--name', name + '-offline', '--label', f'{LABEL}={run_id}', '--memory', str(LIMIT), '--memory-swap', str(LIMIT), *common,
               args.image, 'db', 'create', 'bench', '--ontology', '/bench-ontology.json', timeout=120)
        docker('run', '--pull=never', '-d', '--name', name, '--label', f'{LABEL}={run_id}', '--memory', str(LIMIT),
               '--memory-swap', str(LIMIT), '-p', '127.0.0.1::7687', *common, args.image, 'serve')
        inspect = json.loads(docker('inspect', name))[0]
        report['container'] = {key: inspect.get(key) for key in ('Id', 'Image', 'Created')}
        report['container']['memory'] = inspect['HostConfig']['Memory']
        report['container']['memory_swap'] = inspect['HostConfig']['MemorySwap']
        if inspect['HostConfig']['Memory'] != LIMIT or inspect['HostConfig']['MemorySwap'] != LIMIT:
            raise AssertionError('Docker did not apply memory/swap cap')
        stage('initial-readiness')
        reconnect()
        ready()
        sample('ready')  # Fails closed if cgroup-v2 files are unavailable.
        with driver.session(database='bench') as session:
            query(session, UPSERT, rows=[{'id': identifier(0), 'sequence': 0, 'payload': 'seed'}])
        def client(index, writer):
            result = dict(index=index, role='writer' if writer else 'reader', ack_rows=0, histogram=Counter())
            results.append(result)
            try:
                with driver.session(database='bench') as session:
                    if writer:
                        for batch in batches(args.rows, args.batch_size, index, args.writers):
                            if stop.is_set():
                                break
                            began = time.perf_counter()
                            with session.begin_transaction(timeout=args.query_timeout) as tx:
                                tx.run(UPSERT, rows=batch).consume()
                                tx.run(EDGES, rows=batch).consume()
                                tx.commit()
                            result['ack_rows'] += len(batch)
                            result['histogram'][math.ceil((time.perf_counter() - began) * 1e6)] += 1
                    else:
                        while not stop.is_set() and not workers_done.is_set():
                            began = time.perf_counter()
                            rows = query(session, 'MATCH (n:Bench {entity_id:$id}) RETURN n.entity_id AS id', id=identifier(0))
                            if rows != [{'id': identifier(0)}]:
                                raise AssertionError('concurrent point read mismatch')
                            result['histogram'][math.ceil((time.perf_counter() - began) * 1e6)] += 1
                            stop.wait(.01)
            except Exception as error:
                result['error'] = type(error).__name__
                stop.set()
        stage('mixed-load')
        began = time.monotonic()
        for i in range(args.readers):
            thread = threading.Thread(target=client, args=(i, False), daemon=True)
            threads.append(thread)
            thread.start()
        writer_threads = []
        for i in range(args.writers):
            thread = threading.Thread(target=client, args=(i, True), daemon=True)
            threads.append(thread)
            writer_threads.append(thread)
            thread.start()
        while any(t.is_alive() for t in writer_threads):
            sample('mixed-load')
            if stop.is_set() or time.monotonic() - began > args.max_load_seconds:
                stop.set()
                break
            stop.wait(args.sample_seconds)
        workers_done.set()
        for thread in threads:
            thread.join(timeout=30)
        if any(t.is_alive() for t in threads):
            raise TimeoutError('client threads did not stop')
        if any('error' in r for r in results) or stop.is_set():
            raise AssertionError('mixed-load failed or exceeded deadline')
        report['load_seconds'] = time.monotonic() - began
        report['acknowledged_nodes'] = 1 + sum(r['ack_rows'] for r in results)
        if report['acknowledged_nodes'] != args.rows:
            raise AssertionError('not all requested nodes committed')
        if not all(r['histogram'] for r in results):
            raise AssertionError('each concurrent client must complete at least one operation')
        sample('loaded')
        # Observe retention in the original process before allocator state is discarded.
        report['after_kill'] = restart_after_idle(name, sample, reconnect, ready,
                                                   args.idle_seconds, args.sample_seconds, stage)
        stage('recovered-verification')
        with driver.session(database='bench') as session:
            stage('label-count')
            nodes, label_seconds = measure_query(query, session, 'MATCH (n:Bench) RETURN count(*) AS count')
            stage('edge-count')
            edges, edge_seconds = measure_query(query, session, 'MATCH (a:Bench)-[:LINK]->(b:Bench) RETURN count(*) AS count')
            report['counts'] = dict(nodes=nodes[0]['count'], edges=edges[0]['count'])
            report['query_timings_seconds'] = dict(label_count=label_seconds, edge_count=edge_seconds)
            if report['counts'] != {'nodes': args.rows, 'edges': args.rows - 1}:
                raise AssertionError('acknowledged counts did not survive crash')
            stage('unlabelled-limit')
            bounded, limit_seconds = measure_query(query, session, 'MATCH (n) RETURN n.entity_id AS id LIMIT 37')
            report['query_timings_seconds']['unlabelled_limit'] = limit_seconds
            if len(bounded) != min(37, args.rows) or len({r['id'] for r in bounded}) != len(bounded):
                raise AssertionError('unlabelled LIMIT result mismatch')
            report['unlabelled_limit_rows'] = len(bounded)
            verified = 0
            for i in sorted({1, args.rows - 1, *(worker * args.batch_size + 1 for worker in range(args.writers))}):
                if i >= args.rows:
                    continue
                expected = dict(sequence=i, payload=hashlib.sha256(str(i).encode()).hexdigest() * 8)
                values = query(session, 'MATCH (n:Bench {entity_id:$id}) RETURN n.sequence AS sequence,n.payload AS payload', id=identifier(i))
                if values != [expected]:
                    raise AssertionError('recovered property sample mismatch')
                verified += 1
            report['recovered_property_samples'] = verified
            stage('keyset-cursor')
            report['cursor'] = walk_cursor(session, args.rows, args.query_timeout, args.cursor_seconds)
            if not report['cursor']['complete']:
                raise AssertionError('keyset traversal incomplete')
        stage('recovered-idle')
        idle_samples(sample, 'recovered-idle', args.idle_seconds, args.sample_seconds)
        evaluate_memory(report['samples'])
        stage('offline-integrity')
        docker('stop', '--time', '30', name)
        report['integrity'] = json.loads(docker('run', '--pull=never', '--rm', '--name', name + '-offline', '--label', f'{LABEL}={run_id}', '--memory', str(LIMIT), '--memory-swap', str(LIMIT),
                                                *common, args.image, 'db', 'check', 'bench', '--json', timeout=300))
        if report['integrity'].get('status') != 'ok':
            raise AssertionError('offline integrity check failed')
        report['status'] = 'passed-diagnostic'
    except Exception as error:
        report['failure_stage'] = report.get('stage')
        report['error_type'] = type(error).__name__
    finally:
        stop.set()
        workers_done.set()
        for thread in threads:
            thread.join(timeout=2)
        if driver is not None and not any(t.is_alive() for t in threads):
            driver.close()
        report['memory_by_phase'] = summarize_memory(report['samples'])
        report['clients'] = [{**r, 'histogram': percentile(r['histogram'])} for r in results]
        try:
            (output / 'server.log').write_text(docker('logs', name))
            report['final_state'] = json.loads(docker('inspect', name))[0]['State']
            if report['final_state'].get('OOMKilled'):
                report['status'] = 'failed'
                report['error_type'] = 'OOMKilled'
        except RuntimeError:
            pass
        try:
            owned_cleanup(name, volume, run_id)
        except Exception as error:
            report['cleanup_error_type'] = type(error).__name__
            report['status'] = 'failed'
        policy.unlink(missing_ok=True)
        (output / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({'status': report['status'], 'report': str(output / 'report.json')}))
    return 0 if report['status'] == 'passed-diagnostic' else 1


def docker_image_revision(image):
    value = json.loads(docker('image', 'inspect', image))[0]
    return {'image_id': value['Id'], 'repo_digests': value.get('RepoDigests', []),
            'revision_label': (value.get('Config', {}).get('Labels') or {}).get('org.opencontainers.image.revision'),
            'source_provenance_verified': False}


def parser():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--image', required=True, help='Already-built local image, never pulled or built here')
    p.add_argument('--output', type=Path, required=True, help='New private output directory; must not exist')
    p.add_argument('--rows', type=int, default=1_000_000)
    p.add_argument('--batch-size', type=int, default=250)
    p.add_argument('--writers', type=int, default=8)
    p.add_argument('--readers', type=int, default=4)
    p.add_argument('--query-timeout', type=float, default=30)
    p.add_argument('--max-load-seconds', type=float, default=3600)
    p.add_argument('--cursor-seconds', type=float, default=600)
    p.add_argument('--idle-seconds', type=float, default=60)
    p.add_argument('--sample-seconds', type=float, default=2)
    p.add_argument('--explicit-budgets', action='store_true')
    return p


if __name__ == '__main__':
    p = parser()
    a = p.parse_args()
    if (not 100 <= a.rows <= 100_000_000 or not 1 <= a.batch_size <= 10000
            or not 1 <= a.writers <= 64 or not 1 <= a.readers <= 64
            or a.rows - 1 <= (a.writers - 1) * a.batch_size
            or any(not math.isfinite(v) or v <= 0 for v in
                   (a.query_timeout, a.max_load_seconds, a.cursor_seconds, a.idle_seconds, a.sample_seconds))):
        p.error('invalid row/client/batch count or nonpositive/nonfinite timing')
    raise SystemExit(run(a))
