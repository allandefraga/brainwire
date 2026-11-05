"""Safety checks for the opt-in Kubernetes CSI diagnostic; no cluster contacted."""
import argparse
import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import sys
import types
import unittest
from unittest.mock import MagicMock, patch

spec = importlib.util.spec_from_file_location('handoff', Path(__file__).with_name('kubernetes_csi_handoff.py'))
handoff = importlib.util.module_from_spec(spec)
spec.loader.exec_module(handoff)


def node(name, identity):
    return {'metadata': {'name': name}, 'spec': {},
            'status': {'nodeInfo': {'operatingSystem': 'linux', 'systemUUID': identity},
                       'conditions': [{'type': 'Ready', 'status': 'True'}]}}


class FakeCluster:
    def __init__(self):
        self.calls = []
        self.namespace = None
        self.pods = {}
        self.applied = []
        self.secret_error = False
        self.lose_pod_ownership = False
        self.policy = 'Delete'
        self.insecure = False
        self.claim = None
        self.fail_initialization = False
        self.writer_events = []

    def command(self, words, **kwargs):
        self.calls.append(words)
        args = words[5:]
        if args[:1] == ['--namespace']:
            args = args[2:]
        value = json.loads(kwargs['input']) if kwargs.get('input') else None
        output = ''
        if args[:2] == ['config', 'view']:
            output = {'clusters': [{'cluster': {'server': 'https://api.example.test', 'insecure-skip-tls-verify': self.insecure}}]}
        elif args[:2] == ['get', 'nodes']:
            output = {'items': [node('worker-a', 'machine-a'), node('worker-b', 'machine-b')]}
        elif args[:2] == ['get', 'storageclass']:
            output = {'provisioner': 'example.csi', 'reclaimPolicy': self.policy}
        elif args[0] == 'create':
            self.namespace = value
            self.namespace['metadata']['uid'] = 'namespace-uid'
            output = self.namespace
        elif args[0] == 'apply':
            self.applied.append(value)
            if value['kind'] == 'Secret' and self.secret_error:
                # A webhook is capable of echoing an offending Secret in stderr.
                return argparse.Namespace(returncode=1, stdout='', stderr=json.dumps(value))
            if value['kind'] == 'PersistentVolumeClaim':
                self.claim = {'metadata': {'uid': 'claim-uid'}, 'spec': {'volumeName': 'owned-pv'}}
            if value['kind'] == 'Pod':
                name = value['metadata']['name']
                if name in self.pods:
                    raise AssertionError('replacement applied before the previous pod stopped')
                value['spec']['nodeName'] = value['spec']['affinity']['nodeAffinity']['requiredDuringSchedulingIgnoredDuringExecution']['nodeSelectorTerms'][0]['matchFields'][0]['values'][0]
                if name == 'brainwire':
                    self.writer_events.append(('apply', value['spec']['nodeName']))
                value['status'] = {'phase': ('Failed' if self.fail_initialization else 'Succeeded') if name == 'initialize' else 'Running'}
                self.pods[name] = value
        elif args[:2] == ['get', 'namespace']:
            output = self.namespace
        elif args[:2] == ['get', 'pvc/data']:
            output = self.claim or ''
        elif args[:2] == ['get', 'pv']:
            output = {'metadata': {'uid': 'volume-uid'}, 'spec': {'persistentVolumeReclaimPolicy': 'Delete', 'csi': {'driver': 'example.csi', 'volumeHandle': 'volume-handle'}}}
        elif args[:2] == ['get', 'pods']:
            output = {'items': list(self.pods.values())}
        elif args[:1] == ['get'] and args[1].startswith('pod/'):
            output = copy.deepcopy(self.pods.get(args[1].split('/')[1], ''))
            if output and self.lose_pod_ownership:
                output['metadata']['labels'][handoff.OWNER] = 'another-owner'
        elif args[:2] == ['delete', 'namespace']:
            self.namespace = None
        elif args[:1] == ['delete'] and args[1].startswith('pod/'):
            removed = self.pods.pop(args[1].split('/')[1], None)
            if removed and args[1] == 'pod/brainwire':
                self.writer_events.append(('delete', removed['spec']['nodeName']))
        elif args[:1] == ['logs']:
            output = {'status': 'ok'}
        elif args[:1] == ['wait'] and '--for=condition=Ready' in args:
            self.writer_events.append(('ready', self.pods['brainwire']['spec']['nodeName']))
        elif args[:1] != ['wait']:
            raise AssertionError(f'unhandled kubectl operation: {args[:3]}')
        return argparse.Namespace(returncode=0, stdout=json.dumps(output) if not isinstance(output, str) else output, stderr='')


class SafetyTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        config = Path(self.temp.name) / 'kubeconfig'
        config.write_text('{}')
        self.args = argparse.Namespace(kubeconfig=config, context='explicit-test',
            nodes=['worker-a', 'worker-b'], storage_class='test.csi',
            image='example.test/brainwire@sha256:' + 'a' * 64,
            rows=100, timeout_seconds=30, run_disposable_csi_handoff=True)

    def test_explicit_confirmation_config_context_nodes_digest_and_bounds_required(self):
        handoff.validate(self.args)
        for key, bad in [('run_disposable_csi_handoff', False), ('context', ' '),
                         ('kubeconfig', Path(self.temp.name) / 'absent'),
                         ('nodes', ['worker-a', 'worker-a']), ('nodes', ['worker-a', '-invalid']),
                         ('storage_class', '-all'), ('image', 'example.test/brainwire:latest'),
                         ('rows', 10001), ('timeout_seconds', 601)]:
            with self.subTest(key=key), self.assertRaises(ValueError):
                handoff.validate(argparse.Namespace(**(vars(self.args) | {key: bad})))

    def test_inventory_refuses_same_machine_unready_unschedulable_or_missing_nodes(self):
        good = [node('worker-a', 'machine-a'), node('worker-b', 'machine-b')]
        handoff.check_nodes(good, self.args.nodes)
        cases = [good[:1], [good[0], node('worker-a', 'other')],
                 [good[0], node('worker-b', 'machine-a')], [good[0], node('worker-b', '')]]
        for mutation in ('unschedulable', 'unready', 'wrong-os'):
            case = copy.deepcopy(good)
            if mutation == 'unschedulable': case[1]['spec']['unschedulable'] = True
            elif mutation == 'unready': case[1]['status']['conditions'][0]['status'] = 'False'
            else: case[1]['status']['nodeInfo']['operatingSystem'] = 'windows'
            cases.append(case)
        for case in cases:
            with self.subTest(case=case), self.assertRaises(ValueError):
                handoff.check_nodes(case, self.args.nodes)

    def test_pod_uses_scheduler_affinity_and_one_claim_with_bounded_secure_runtime(self):
        pod = handoff.pod('owned', 'identity', self.args.image, 'worker-b')
        spec = pod['spec']
        self.assertNotIn('nodeName', spec)  # Do not bypass CSI scheduler topology.
        self.assertEqual(spec['affinity']['nodeAffinity']['requiredDuringSchedulingIgnoredDuringExecution']
                         ['nodeSelectorTerms'][0]['matchFields'][0]['values'], ['worker-b'])
        self.assertFalse(spec['automountServiceAccountToken'])
        self.assertEqual(spec['volumes'][0]['persistentVolumeClaim']['claimName'], 'data')
        self.assertEqual(spec['containers'][0]['resources']['limits']['memory'], '1Gi')
        self.assertEqual(spec['containers'][0]['args'], ['serve'])
        self.assertNotIn('allow-unauthenticated', json.dumps(pod))
        self.assertTrue(spec['containers'][0]['securityContext']['readOnlyRootFilesystem'])
        self.assertEqual(handoff.pod('owned', 'identity', self.args.image, 'worker-a', True)
                         ['spec']['containers'][0]['args'][0:2], ['db', 'apply-ontology'])

    def test_namespace_cleanup_identity_is_bound_to_label_and_uid(self):
        value = {'metadata': {'uid': 'uid-a', 'labels': {handoff.OWNER: 'owner-a'}}}
        self.assertTrue(handoff.owned_namespace(value, 'owner-a', 'uid-a'))
        self.assertFalse(handoff.owned_namespace(value, 'owner-b', 'uid-a'))
        self.assertFalse(handoff.owned_namespace(value, 'owner-a', 'uid-b'))
        self.assertFalse(handoff.owned_namespace({}, 'owner-a', 'uid-a'))

    def test_validation_failure_never_runs_kubectl(self):
        self.args.run_disposable_csi_handoff = False
        with patch.object(handoff.subprocess, 'run') as command, self.assertRaises(ValueError):
            handoff.run(self.args)
        command.assert_not_called()

    def test_local_path_storage_refused_before_namespace_or_writes(self):
        calls = []
        def command(words, **kwargs):
            calls.append(words)
            if 'config' in words:
                result = {'clusters': [{'cluster': {'server': 'https://api.example.test'}}]}
            elif 'nodes' in words:
                result = {'items': [node('worker-a', 'a'), node('worker-b', 'b')]}
            else:
                self.assertIn('storageclass', words)
                result = {'provisioner': 'rancher.io/local-path', 'reclaimPolicy': 'Delete'}
            return argparse.Namespace(returncode=0, stdout=json.dumps(result), stderr='')
        with patch.object(handoff.subprocess, 'run', side_effect=command):
            receipt = handoff.run(self.args)
        self.assertEqual(receipt['result'], 'FAIL')
        self.assertTrue(receipt['cleanup_complete'])
        self.assertEqual(len(calls), 3)
        for words in calls:
            self.assertEqual(words[0:5], ['kubectl', '--kubeconfig', str(self.args.kubeconfig.resolve()), '--context', 'explicit-test'])
            self.assertNotIn('create', words)
            self.assertNotIn('apply', words)

    def test_unknown_namespace_creation_result_preserves_resources_and_reports_incomplete_cleanup(self):
        calls = []
        def command(words, **kwargs):
            calls.append(words)
            if 'config' in words:
                result = {'clusters': [{'cluster': {'server': 'https://api.example.test'}}]}
            elif 'nodes' in words:
                result = {'items': [node('worker-a', 'a'), node('worker-b', 'b')]}
            elif 'storageclass' in words:
                result = {'provisioner': 'test.csi', 'reclaimPolicy': 'Delete'}
            else:
                self.assertIn('create', words)
                raise handoff.subprocess.TimeoutExpired(words, 30)
            return argparse.Namespace(returncode=0, stdout=json.dumps(result), stderr='')
        with patch.object(handoff.subprocess, 'run', side_effect=command):
            receipt = handoff.run(self.args)
        self.assertEqual(receipt['result'], 'FAIL')
        self.assertFalse(receipt['cleanup_complete'])
        self.assertIn('outcome unknown', receipt['cleanup_error'])
        self.assertFalse(any('delete' in words for words in calls))


    def exercise(self, cluster):
        client = MagicMock()
        client.__enter__.return_value = client
        client.session.return_value.__enter__.return_value = client
        def query(text, **values):
            result = MagicMock()
            result.single.return_value = {'n': self.args.rows} if 'count(*)' in text else {'id': 'handoff-0'}
            return result
        client.run.side_effect = query
        graph_database = MagicMock()
        graph_database.driver.return_value = client
        connection = MagicMock()
        connection.__enter__.return_value.getsockname.return_value = ('127.0.0.1', 37687)
        process = MagicMock()
        process.poll.return_value = None
        with patch.object(handoff.subprocess, 'run', side_effect=cluster.command), \
             patch.object(handoff.subprocess, 'Popen', return_value=process), \
             patch.object(handoff.socket, 'socket', return_value=connection), \
             patch.object(handoff.socket, 'create_connection', return_value=MagicMock()), \
             patch.dict(sys.modules, {'neo4j': types.SimpleNamespace(GraphDatabase=graph_database)}):
            receipt = handoff.run(self.args)
        return receipt, client, process

    def test_full_handoff_orders_writer_stop_before_replacement_and_waits_for_volume_cleanup(self):
        cluster = FakeCluster()
        receipt, client, process = self.exercise(cluster)
        self.assertEqual(receipt['result'], 'PASS', receipt)
        self.assertTrue(receipt['cleanup_complete'])
        self.assertIsNone(cluster.namespace)
        self.assertEqual(cluster.writer_events, [('apply', 'worker-a'), ('ready', 'worker-a'),
            ('delete', 'worker-a'), ('apply', 'worker-b'), ('ready', 'worker-b'), ('delete', 'worker-b')])
        workloads = [p for p in cluster.applied if p['kind'] == 'Pod']
        self.assertEqual([p['spec']['nodeName'] for p in workloads], ['worker-a', 'worker-a', 'worker-b', 'worker-b'])
        calls = [words[5:] for words in cluster.calls]
        self.assertTrue(any('delete' in words and 'pod/brainwire' in words for words in calls))
        self.assertIn(['wait', '--for=delete', 'pv/owned-pv', '--timeout=30s'], calls)
        self.assertTrue(any('indexed lookup' not in str(call) and 'handoff-0@example.test' in str(call) for call in client.run.call_args_list))
        process.terminate.assert_called()
        for words in cluster.calls:
            self.assertEqual(words[:5], ['kubectl', '--kubeconfig', str(self.args.kubeconfig.resolve()), '--context', self.args.context])
            self.assertNotIn('--force', words)
            self.assertNotIn('cordon', words)

    def test_failed_initialization_discovers_bound_pv_for_cleanup(self):
        cluster = FakeCluster()
        cluster.fail_initialization = True
        receipt, _, _ = self.exercise(cluster)
        self.assertEqual(receipt['result'], 'FAIL')
        self.assertTrue(receipt['cleanup_complete'])
        self.assertEqual(receipt['pv_cleanup'], 'API object deletion verified')
        self.assertTrue(any('--for=delete' in words and 'pv/owned-pv' in words for words in cluster.calls))
        self.assertEqual(cluster.writer_events, [])

    def test_foreign_pod_preserves_namespace_and_pvc(self):
        cluster = FakeCluster()
        cluster.lose_pod_ownership = True
        receipt, _, _ = self.exercise(cluster)
        self.assertEqual(receipt['result'], 'FAIL')
        self.assertFalse(receipt['cleanup_complete'])
        self.assertIsNotNone(cluster.namespace)
        self.assertIn('ownership changed', receipt['cleanup_error'])
        self.assertFalse(any('delete' in words for words in cluster.calls))

    def test_secret_error_never_echoes_private_credentials(self):
        cluster = FakeCluster()
        cluster.secret_error = True
        receipt, _, _ = self.exercise(cluster)
        secret = next(p for p in cluster.applied if p['kind'] == 'Secret')
        token = json.loads(secret['stringData']['health.json'])['credentials']
        self.assertEqual(receipt['result'], 'FAIL')
        self.assertNotIn(token, json.dumps(receipt))
        self.assertEqual(receipt['error'], 'kubectl failed while applying private credentials')
        self.assertTrue(receipt['cleanup_complete'])

    def test_retain_and_insecure_api_refused_before_namespace_creation(self):
        for field, value in [('policy', 'Retain'), ('insecure', True)]:
            with self.subTest(field=field):
                cluster = FakeCluster()
                setattr(cluster, field, value)
                receipt, _, _ = self.exercise(cluster)
                self.assertEqual(receipt['result'], 'FAIL')
                self.assertTrue(receipt['cleanup_complete'])
                self.assertFalse(any('create' in words for words in cluster.calls))


if __name__ == '__main__':
    unittest.main()
