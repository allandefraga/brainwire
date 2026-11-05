"""Opt-in CSI volume handoff on two explicitly selected Linux Kubernetes nodes.

Creates only a unique owned namespace. Stops the first writer before scheduling
its replacement on the second node; never cords, drains or force-deletes a node.
This checks cross-node storage handoff, not lost-machine fencing or Raft failover.
Requires kubectl, a CSI StorageClass supporting ReadWriteOncePod, and neo4j.
"""
import argparse
from contextlib import contextmanager, suppress
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import socket
import subprocess
import tempfile
import time
import uuid

OWNER = "brainwire.dev/csi-handoff"
DNS = re.compile(r"[a-z0-9](?:[a-z0-9.-]{0,251}[a-z0-9])?")
IMAGE = re.compile(r"[^\s@]+@sha256:[a-f0-9]{64}")


def validate(args):
    if not args.run_disposable_csi_handoff:
        raise ValueError("explicit --run-disposable-csi-handoff is required")
    if not args.kubeconfig.is_file() or not args.context.strip():
        raise ValueError("an explicit kubeconfig file and context are required")
    if len(args.nodes) != 2 or args.nodes[0] == args.nodes[1]:
        raise ValueError("select two distinct nodes")
    if not all(DNS.fullmatch(name) for name in (*args.nodes, args.storage_class)):
        raise ValueError("invalid Kubernetes node or StorageClass name")
    if not IMAGE.fullmatch(args.image):
        raise ValueError("pin --image by its sha256 digest")
    if not 1 <= args.rows <= 10000 or not 30 <= args.timeout_seconds <= 600:
        raise ValueError("rows or timeout exceed safety bounds")


def check_nodes(items, names):
    if len(items) != 2 or {item['metadata']['name'] for item in items} != set(names):
        raise ValueError("node inventory does not match the two requested nodes")
    identities = []
    for node in items:
        info = node.get('status', {}).get('nodeInfo', {})
        if node.get('spec', {}).get('unschedulable') or info.get('operatingSystem') != 'linux':
            raise ValueError("both selected nodes must be schedulable Linux nodes")
        if not any(c.get('type') == 'Ready' and c.get('status') == 'True'
                   for c in node.get('status', {}).get('conditions', [])):
            raise ValueError("both selected nodes must be Ready")
        identity = info.get('systemUUID', '').strip()
        if not identity:
            raise ValueError("node systemUUID is required for the diagnostic")
        identities.append(identity)
    if identities[0] == identities[1]:
        raise ValueError("selected nodes report the same systemUUID")


def pod(namespace, identity, image, node, initialize=False):
    container = {
        'name': 'brainwire', 'image': image, 'imagePullPolicy': 'IfNotPresent',
        'args': ['db', 'apply-ontology', 'default', '/opt/brainwire/example-schema.json']
                if initialize else ['serve'],
        'env': [{'name': key, 'value': value} for key, value in {
            'BRAINWIRE_SERVER_DATA_DIR': '/data', 'BRAINWIRE_SERVER_LISTEN': '0.0.0.0:7687',
            'BRAINWIRE_MEMORY_CACHE': '64MiB', 'BRAINWIRE_MEMORY_MEMTABLES': '16MiB',
            'BRAINWIRE_MEMORY_QUERY': '64MiB',
            'BRAINWIRE_SERVER_AUTH_FILE': '/auth/policy.json',
            'BRAINWIRE_BOLT_AUTH_FILE': '/auth/health.json'}.items()],
        'resources': {'requests': {'cpu': '100m', 'memory': '256Mi'},
                      'limits': {'cpu': '1', 'memory': '1Gi'}},
        'securityContext': {'allowPrivilegeEscalation': False, 'readOnlyRootFilesystem': True,
                            'capabilities': {'drop': ['ALL']}},
        'volumeMounts': [{'name': 'data', 'mountPath': '/data'},
                         {'name': 'auth', 'mountPath': '/auth', 'readOnly': True}]}
    if not initialize:
        container['readinessProbe'] = {'exec': {'command': ['brainwire', 'probe', '--mode', 'ready', '--timeout-ms', '500']},
                                       'periodSeconds': 2, 'timeoutSeconds': 5, 'failureThreshold': 30}
    return {'apiVersion': 'v1', 'kind': 'Pod',
            'metadata': {'name': 'initialize' if initialize else 'brainwire', 'namespace': namespace,
                         'labels': {OWNER: identity}},
            'spec': {'restartPolicy': 'Never', 'automountServiceAccountToken': False,
                     'terminationGracePeriodSeconds': 60,
                     'securityContext': {'runAsNonRoot': True, 'runAsUser': 10001, 'runAsGroup': 10001,
                                         'fsGroup': 10001, 'seccompProfile': {'type': 'RuntimeDefault'}},
                     'affinity': {'nodeAffinity': {'requiredDuringSchedulingIgnoredDuringExecution': {
                         'nodeSelectorTerms': [{'matchFields': [{'key': 'metadata.name', 'operator': 'In', 'values': [node]}]}]}}},
                     'containers': [container], 'volumes': [
                         {'name': 'data', 'persistentVolumeClaim': {'claimName': 'data'}},
                         {'name': 'auth', 'secret': {'secretName': 'auth', 'defaultMode': 0o440}}]}}


def owned_namespace(value, identity, uid):
    metadata = value.get('metadata', {})
    return metadata.get('uid') == uid and metadata.get('labels', {}).get(OWNER) == identity


def run(args):
    validate(args)
    identity = uuid.uuid4().hex
    namespace = 'brainwire-csi-' + identity[:12]
    token = secrets.token_hex(32)
    base = ['kubectl', '--kubeconfig', str(args.kubeconfig.resolve()), '--context', args.context]
    env = {k: v for k, v in os.environ.items() if not k.startswith('BRAINWIRE_')}
    report = {'result': 'FAIL', 'namespace': namespace, 'image': args.image, 'rows': args.rows,
              'scope': 'graceful single-writer CSI handoff across two selected nodes; not machine-loss fencing, Raft or production qualification',
              'synthetic': True, 'cleanup_complete': False,
              'node_identity_basis': 'distinct Kubernetes-reported systemUUID values; physical independence is not verified',
              'network_policy_enforcement': 'not verified; requires a trusted enforcing cluster',
              'cleanup_scope': 'owned namespace and PV API objects; backend provider deletion is not independently verified'}
    namespace_uid = None
    creation_attempted = False
    volume_name = None

    def command(words, value=None, timeout=None):
        response = subprocess.run(base + words, input=json.dumps(value) if value is not None else None,
                                  env=env, capture_output=True, text=True,
                                  timeout=timeout or args.timeout_seconds)
        if response.returncode:
            # Input may be a Secret: never include stdin or full invocation in errors.
            if value is not None and value.get('kind') == 'Secret':
                raise RuntimeError('kubectl failed while applying private credentials')
            raise RuntimeError(response.stderr[-2000:] or 'kubectl failed')
        return response.stdout

    def kube(*words, value=None):
        return command(['--namespace', namespace, *words], value)

    def apply(value):
        return kube('apply', '-f', '-', value=value)

    def start(node, initialize=False):
        apply(pod(namespace, identity, args.image, node, initialize))
        if initialize:
            deadline = time.monotonic() + args.timeout_seconds
            while time.monotonic() < deadline:
                phase = json.loads(kube('get', 'pod/initialize', '-o', 'json'))['status'].get('phase')
                if phase == 'Succeeded':
                    break
                if phase == 'Failed':
                    raise RuntimeError('ontology initialization failed')
                time.sleep(1)
            else:
                raise TimeoutError('ontology initialization did not finish')
        else:
            kube('wait', '--for=condition=Ready', 'pod/brainwire', f'--timeout={args.timeout_seconds}s')
            current = json.loads(kube('get', 'pod/brainwire', '-o', 'json'))
            if current['spec']['nodeName'] != node:
                raise RuntimeError('replacement did not run on the requested node')

    def stop(name):
        current = json.loads(command(['get', 'namespace', namespace, '-o', 'json']))
        if not owned_namespace(current, identity, namespace_uid):
            raise RuntimeError('namespace ownership changed; refusing pod deletion')
        found = kube('get', f'pod/{name}', '-o', 'json', '--ignore-not-found=true')
        if not found:
            return
        if json.loads(found).get('metadata', {}).get('labels', {}).get(OWNER) != identity:
            raise RuntimeError('pod ownership changed; refusing pod deletion')
        # Wait for normal termination; never bypass storage fencing with force-delete.
        kube('delete', f'pod/{name}', '--ignore-not-found=true', '--wait=true', '--grace-period=60',
             f'--timeout={args.timeout_seconds}s')

    @contextmanager
    def endpoint():
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            port = listener.getsockname()[1]
        with tempfile.TemporaryFile() as log:
            process = subprocess.Popen(base + ['--namespace', namespace, 'port-forward', '--address', '127.0.0.1',
                                               'pod/brainwire', f'{port}:7687'], env=env, stdout=log, stderr=log)
            try:
                deadline = time.monotonic() + 30
                while time.monotonic() < deadline:
                    if process.poll() is not None:
                        raise RuntimeError('private port-forward exited')
                    try:
                        with socket.create_connection(('127.0.0.1', port), timeout=.2):
                            break
                    except OSError:
                        time.sleep(.1)
                else:
                    raise TimeoutError('private port-forward did not become ready')
                yield f'bolt://127.0.0.1:{port}'
            finally:
                process.terminate()
                with suppress(subprocess.TimeoutExpired):
                    process.wait(timeout=5)
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=5)

    try:
        connection = json.loads(command(['config', 'view', '--minify', '-o', 'json']))
        clusters = connection.get('clusters', [])
        if len(clusters) != 1 or not clusters[0].get('cluster', {}).get('server', '').startswith('https://') or clusters[0]['cluster'].get('insecure-skip-tls-verify'):
            raise ValueError('the selected context must use a verified HTTPS Kubernetes API')
        inventory = json.loads(command(['get', 'nodes', *args.nodes, '-o', 'json']))['items']
        check_nodes(inventory, args.nodes)
        report['selected_nodes'] = args.nodes
        report['node_identity_sha256'] = {node['metadata']['name']: hashlib.sha256(
            node['status']['nodeInfo']['systemUUID'].encode()).hexdigest() for node in inventory}
        storage = json.loads(command(['get', 'storageclass', args.storage_class, '-o', 'json']))
        if storage.get('reclaimPolicy') != 'Delete':
            raise ValueError('diagnostic requires a Delete reclaim policy; Retain may leave provider volumes behind')
        if storage.get('provisioner') in {'kubernetes.io/no-provisioner', 'rancher.io/local-path', 'kubernetes.io/host-path'}:
            raise ValueError('select a CSI StorageClass with cross-node attach support')
        creation_attempted = True
        created = json.loads(command(['create', '-f', '-', '-o', 'json'], {
            'apiVersion': 'v1', 'kind': 'Namespace', 'metadata': {'name': namespace, 'labels': {OWNER: identity}}}))
        namespace_uid = created['metadata']['uid']
        apply({'apiVersion': 'networking.k8s.io/v1', 'kind': 'NetworkPolicy',
               'metadata': {'name': 'deny-network', 'namespace': namespace},
               'spec': {'podSelector': {}, 'policyTypes': ['Ingress', 'Egress'], 'ingress': [], 'egress': []}})
        policy = {'principals': [{'id': 'handoff', 'token_sha256': hashlib.sha256(token.encode()).hexdigest(),
                                  'graphs': {'default': 'write'}, 'instance_admin': False}]}
        apply({'apiVersion': 'v1', 'kind': 'Secret', 'metadata': {'name': 'auth', 'namespace': namespace},
               'stringData': {'policy.json': json.dumps(policy),
                              'health.json': json.dumps({'principal': 'handoff', 'credentials': token})}})
        apply({'apiVersion': 'v1', 'kind': 'PersistentVolumeClaim', 'metadata': {'name': 'data', 'namespace': namespace},
               'spec': {'accessModes': ['ReadWriteOncePod'], 'storageClassName': args.storage_class,
                        'resources': {'requests': {'storage': '1Gi'}}}})
        start(args.nodes[0], initialize=True)
        claim = json.loads(kube('get', 'pvc/data', '-o', 'json'))
        claim_uid = claim['metadata']['uid']
        volume_name = claim['spec']['volumeName']
        volume = json.loads(command(['get', 'pv', volume_name, '-o', 'json']))
        if volume.get('spec', {}).get('persistentVolumeReclaimPolicy') != 'Delete':
            raise RuntimeError('bound volume does not have the requested Delete reclaim policy')
        if not volume.get('spec', {}).get('csi'):
            raise RuntimeError('bound volume is not CSI-backed')
        volume_uid = volume['metadata']['uid']
        volume_handle = volume['spec']['csi']['volumeHandle']
        report['csi_driver'] = volume['spec']['csi']['driver']
        stop('initialize')
        from neo4j import GraphDatabase
        start(args.nodes[0])
        with endpoint() as uri, GraphDatabase.driver(uri, auth=('handoff', token), connection_timeout=5) as driver:
            with driver.session(database='default') as session:
                for offset in range(0, args.rows, 100):
                    rows = [{'id': f'handoff-{i}', 'email': f'handoff-{i}@example.test'}
                            for i in range(offset, min(offset + 100, args.rows))]
                    session.run('UNWIND $rows AS row MERGE (c:Customer {entity_id:row.id}) SET c.email=row.email', rows=rows).consume()
        began = time.monotonic()
        stop('brainwire')
        start(args.nodes[1])
        report['handoff_seconds'] = round(time.monotonic() - began, 3)
        replacement_claim = json.loads(kube('get', 'pvc/data', '-o', 'json'))
        replacement_volume = json.loads(command(['get', 'pv', volume_name, '-o', 'json']))
        if replacement_claim['metadata']['uid'] != claim_uid or replacement_claim['spec']['volumeName'] != volume_name or replacement_volume['metadata']['uid'] != volume_uid or replacement_volume.get('spec', {}).get('csi', {}).get('volumeHandle') != volume_handle:
            raise RuntimeError('replacement used a different PVC or CSI volume')
        with endpoint() as uri, GraphDatabase.driver(uri, auth=('handoff', token), connection_timeout=5) as driver:
            with driver.session(database='default') as session:
                if session.run('MATCH (c:Customer) RETURN count(*) AS n').single()['n'] != args.rows:
                    raise RuntimeError('node count changed during handoff')
                if session.run("MATCH (c:Customer) WHERE c.email='handoff-0@example.test' RETURN c.entity_id AS id").single()['id'] != 'handoff-0':
                    raise RuntimeError('indexed lookup changed during handoff')
        stop('brainwire')
        audit = pod(namespace, identity, args.image, args.nodes[1], initialize=True)
        audit['metadata']['name'] = 'initialize'
        audit['spec']['containers'][0]['args'] = ['db', 'check', 'default', '--json']
        apply(audit)
        kube('wait', '--for=jsonpath={.status.phase}=Succeeded', 'pod/initialize', f'--timeout={args.timeout_seconds}s')
        if json.loads(kube('logs', 'pod/initialize')).get('status') != 'ok':
            raise RuntimeError('offline audit did not pass')
        report['result'] = 'PASS'
    except Exception as error:
        report['error_type'] = type(error).__name__
        report['error'] = str(error).replace(token, '[redacted]')[-2000:]
    finally:
        if namespace_uid is not None:
            try:
                current = json.loads(command(['get', 'namespace', namespace, '-o', 'json']))
                if not owned_namespace(current, identity, namespace_uid):
                    raise RuntimeError('namespace ownership changed; preserving namespace and PVC')
                for name in ('brainwire', 'initialize'):
                    stop(name)
                if json.loads(kube('get', 'pods', '-o', 'json'))['items']:
                    raise RuntimeError('namespace has unexpected pods; preserving namespace and PVC')
                if volume_name is None:
                    found = kube('get', 'pvc/data', '-o', 'json', '--ignore-not-found=true')
                    if found:
                        volume_name = json.loads(found).get('spec', {}).get('volumeName')
                if volume_name is not None:
                    retained = json.loads(command(['get', 'pv', volume_name, '-o', 'json']))
                    if retained.get('spec', {}).get('persistentVolumeReclaimPolicy') != 'Delete':
                        raise RuntimeError('bound PV does not allow verified cleanup; preserving namespace and PVC')
                command(['delete', 'namespace', namespace, '--wait=true', f'--timeout={args.timeout_seconds}s'])
                if volume_name is not None:
                    command(['wait', '--for=delete', f'pv/{volume_name}', f'--timeout={args.timeout_seconds}s'])
                    report['pv_cleanup'] = 'API object deletion verified'
                else:
                    report['pv_cleanup'] = 'no bound PV observed before cleanup; backend volume absence is not verified'
                report['cleanup_complete'] = True
            except Exception as error:
                report['result'] = 'FAIL'
                report['cleanup_error'] = str(error).replace(token, '[redacted]')[-2000:]
        else:
            report['cleanup_complete'] = not creation_attempted
            if creation_attempted:
                report['cleanup_error'] = 'namespace creation outcome unknown; inspect the named namespace before cleanup'
    return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--kubeconfig', type=Path, required=True)
    parser.add_argument('--context', required=True)
    parser.add_argument('--nodes', nargs=2, required=True)
    parser.add_argument('--storage-class', required=True)
    parser.add_argument('--image', required=True)
    parser.add_argument('--rows', type=int, default=100)
    parser.add_argument('--timeout-seconds', type=int, default=180)
    parser.add_argument('--run-disposable-csi-handoff', action='store_true')
    args = parser.parse_args()
    try:
        validate(args)
    except ValueError as error:
        parser.error(str(error))
    receipt = run(args)
    print(json.dumps(receipt, indent=2))
    raise SystemExit(0 if receipt['result'] == 'PASS' else 1)
