"""Offline runner regressions; no Docker daemon or client libraries needed."""
import importlib.util
import pathlib
import subprocess
import sys
import tempfile
import types
import unittest
from unittest.mock import Mock, patch

# The tests never instantiate database clients.
with patch.dict(sys.modules, {
    'falkordb': types.SimpleNamespace(FalkorDB=Mock()),
    'neo4j': types.SimpleNamespace(GraphDatabase=Mock(), Query=Mock(side_effect=lambda text, **kw: (text, kw))),
    'redis': types.SimpleNamespace(Redis=Mock()),
}):
    spec = importlib.util.spec_from_file_location('comparison_runner', pathlib.Path(__file__).with_name('run.py'))
    runner = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(runner)


class RunnerTest(unittest.TestCase):
    def test_redis_readiness_bounds_connection_and_reply_wait(self):
        with patch.object(runner, 'Redis') as redis:
            redis.return_value.ping.return_value = True
            self.assertTrue(runner.redis_ready('16379'))
        redis.assert_called_once_with(host='127.0.0.1', port=16379,
                                      socket_connect_timeout=2, socket_timeout=2)
        redis.return_value.ping.assert_called_once_with()

    def test_falkor_runtime_configuration_is_verified_and_recorded(self):
        with patch.object(runner, 'Redis') as redis:
            client = redis.return_value
            values = {'appendonly':'yes', 'appendfsync':'always', 'maxmemory':'3221225472'}
            client.config_get.side_effect = lambda name: {name:values[name]}
            client.info.return_value = {'redis_version':'8.0.0', 'other':'discard'}
            client.module_list.return_value = [{'name':'graph', 'ver':60000, 'path':'discard'}]
            observed = runner.falkor_configuration('16379', 3221225472)
        self.assertEqual(observed, {'config':{**values, 'maxmemory':3221225472},
                         'redis_version':'8.0.0', 'modules':[{'name':'graph','ver':60000}]})
        self.assertEqual([call.args[0] for call in client.config_get.call_args_list],
                         ['appendonly','appendfsync','maxmemory'])
        redis.assert_called_once_with(host='127.0.0.1', port=16379,
            socket_connect_timeout=2, socket_timeout=2, decode_responses=True)
        client.close.assert_called_once_with()

    def test_falkor_wrong_or_missing_runtime_configuration_fails_closed(self):
        correct = {'appendonly':'yes', 'appendfsync':'always', 'maxmemory':'1024'}
        for changed in ({'appendonly':'no'}, {'appendfsync':'everysec'},
                        {'maxmemory':'2048'}, {'maxmemory':'bad'}):
            with self.subTest(changed=changed), patch.object(runner, 'Redis') as redis:
                values = {**correct, **changed}
                redis.return_value.config_get.side_effect = lambda name: {name:values[name]}
                with self.assertRaises(RuntimeError):
                    runner.falkor_configuration('16379', 1024)
                redis.return_value.close.assert_called_once_with()
        with patch.object(runner, 'Redis') as redis:
            redis.return_value.config_get.return_value = {}
            with self.assertRaises(RuntimeError):
                runner.falkor_configuration('16379', 1024)
            redis.return_value.close.assert_called_once_with()

    def test_image_source_labels_are_recorded_from_resolved_id_only(self):
        import json
        image = {'Id':'sha256:'+'a'*64, 'Config':{'Labels':{
            'org.opencontainers.image.revision':'commit', 'org.brainwire.benchmark-source-sha256':'sourcehash',
            'private-label':'must-not-leak'}}}
        with patch.object(runner, 'docker', return_value=json.dumps([image])):
            observed = runner.resolve_image('moving:tag')
        self.assertEqual(observed['source_labels'], {
            'org.opencontainers.image.revision':'commit', 'org.brainwire.benchmark-source-sha256':'sourcehash'})

    def test_logs_include_application_stderr(self):
        result = subprocess.CompletedProcess([], 0, stdout='ready\n', stderr='listener refused\n')
        with patch.object(runner.subprocess, 'run', return_value=result) as execute:
            logs = runner.container_logs('fixture', 25)
        self.assertEqual(logs, 'stdout:\nready\nstderr:\nlistener refused')
        self.assertEqual(execute.call_args.args[0], ['docker', 'logs', '--tail', '25', 'fixture'])

    def test_docker_failure_includes_output_without_echoing_secret_argv(self):
        error = subprocess.CalledProcessError(1, ['docker', 'run', 'secret'], output='partial', stderr='missing image')
        with patch.object(runner.subprocess, 'run', side_effect=error):
            with self.assertRaises(RuntimeError) as caught:
                runner.docker('run', 'secret')
        self.assertIn('partial', str(caught.exception))
        self.assertIn('missing image', str(caught.exception))
        self.assertNotIn('secret', str(caught.exception))

    def fixture_args(self, workdir):
        return types.SimpleNamespace(workdir=workdir, cpus=2, memory_gib=4,
            images={"brainwire": {"id": "sha256:" + "a"*64,
                "reference": "moving:tag", "repo_digests": []}})

    def test_brainwire_fixture_uses_pinned_image_limits_and_owned_cleanup(self):
        calls = []
        def docker(*args, **kwargs):
            calls.append(args)
            if args[0] == 'port':
                raise RuntimeError('container exited')
            return 'container-id'
        with tempfile.TemporaryDirectory() as workdir:
            args = self.fixture_args(workdir)
            with patch.object(runner, 'docker', side_effect=docker), \
                 patch.object(runner, 'inspect_owned', return_value={'Id': 'container-id'}), \
                 patch.object(runner, 'container_logs', return_value='listener refused'), \
                 patch.object(runner, 'cleanup_owned', return_value=[]) as cleanup:
                with self.assertRaisesRegex(RuntimeError, 'container exited'):
                    runner.run_system('brainwire', args, 'unit-unique')
            self.assertEqual(list(pathlib.Path(workdir).iterdir()), [])
        runs = [call for call in calls if call[0] == 'run']
        self.assertEqual(len(runs), 2)
        for call in runs:
            self.assertIn(args.images['brainwire']['id'], call)
            self.assertNotIn('moving:tag', call)
            self.assertIn('--pull=never', call)
            self.assertIn('brainwire.comparison=unit-unique', call)
            self.assertEqual(call[call.index('--memory')+1], '4g')
            self.assertEqual(call[call.index('--memory-swap')+1], '4g')
            self.assertEqual(call[call.index('--cpus')+1], '2')
        self.assertEqual(runs[1][-2:], ('serve', '--allow-unauthenticated-remote'))
        self.assertIn('127.0.0.1::7687', runs[1])
        cleanup.assert_called_once_with(
            ('brainwire-comparison-unit-unique-brainwire',
             'brainwire-comparison-unit-unique-brainwire-init'),
            'bw-compare-unit-unique-brainwire', 'unit-unique')

    def test_existing_fixture_is_never_deleted(self):
        with tempfile.TemporaryDirectory() as workdir:
            fixture = pathlib.Path(workdir) / 'unit-ontology.json'
            fixture.write_text('existing user fixture')
            with patch.object(runner, 'docker', return_value='id'), \
                 patch.object(runner, 'inspect_owned', return_value={}), \
                 patch.object(runner, 'cleanup_owned', return_value=[]):
                with self.assertRaises(RuntimeError):
                    runner.run_system('brainwire', self.fixture_args(workdir), 'unit')
            self.assertEqual(fixture.read_text(), 'existing user fixture')

    def test_volume_create_timeout_still_cleans_owned_resources(self):
        with tempfile.TemporaryDirectory() as workdir:
            with patch.object(runner, 'docker', side_effect=RuntimeError('timeout')), \
                 patch.object(runner, 'cleanup_owned', return_value=[]) as cleanup:
                with self.assertRaisesRegex(RuntimeError, 'timeout'):
                    runner.run_system('brainwire', self.fixture_args(workdir), 'unit')
                self.assertEqual(cleanup.call_count, 1)

    def test_cleanup_refuses_unowned_resources_and_continues(self):
        with patch.object(runner, 'inspect_owned', side_effect=[RuntimeError('unowned'), None]), \
             patch.object(runner, 'docker') as execute:
            errors = runner.cleanup_owned(('shared-name',), 'volume', 'unit')
        self.assertEqual(len(errors), 1)
        execute.assert_not_called()

    def test_cleanup_uses_verified_id_and_accepts_autoremoval(self):
        with patch.object(runner, 'inspect_owned', side_effect=[{'Id':'immutable-id'}, None, None, None]), \
             patch.object(runner, 'docker', side_effect=RuntimeError('already gone')) as execute:
            self.assertEqual(runner.cleanup_owned(('name',), 'volume', 'unit'), [])
        self.assertEqual(execute.call_args.args, ('rm', '-f', 'immutable-id'))

    def test_inspect_requires_correct_uuid_and_distinguishes_unknown_failure(self):
        wrong = subprocess.CompletedProcess([], 0, stdout='[{"Config":{"Labels":{}}}]', stderr='')
        denied = subprocess.CompletedProcess([], 1, stdout='', stderr='permission denied')
        absent = subprocess.CompletedProcess([], 1, stdout='', stderr='No such container: x')
        for outcome in (wrong, denied):
            with patch.object(runner.subprocess, 'run', return_value=outcome):
                with self.assertRaises(RuntimeError):
                    runner.inspect_owned('container', 'x', 'unit')
        with patch.object(runner.subprocess, 'run', return_value=absent):
            self.assertIsNone(runner.inspect_owned('container', 'x', 'unit'))

    def test_image_resolution_requires_local_immutable_id(self):
        identity = 'sha256:' + 'a'*64
        with patch.object(runner, 'docker', return_value='[{"Id":"' + identity + '","RepoDigests":[]}]'):
            self.assertEqual(runner.resolve_image('moving:tag')['id'], identity)
        with patch.object(runner, 'docker', return_value='[{"Id":"moving:tag"}]'):
            with self.assertRaises(RuntimeError):
                runner.resolve_image('moving:tag')

    def test_wrong_image_is_rejected_before_a_client_connects(self):
        with tempfile.TemporaryDirectory() as workdir:
            args = self.fixture_args(workdir)
            with patch.object(runner, 'docker', return_value='container-id'), \
                 patch.object(runner, 'port', return_value=12345), \
                 patch.object(runner, 'inspect_owned', side_effect=[{}, {'Id':'container-id','Image':'wrong'}]), \
                 patch.object(runner, 'cleanup_owned', return_value=[]), \
                 patch.object(runner, 'container_logs', return_value=''), \
                 patch.object(runner, 'Client') as client:
                with self.assertRaisesRegex(RuntimeError, 'image differs'):
                    runner.run_system('brainwire', args, 'unit')
        client.assert_not_called()

    def test_chart_resource_caption_uses_receipt_and_escapes_text(self):
        spec = importlib.util.spec_from_file_location('comparison_plot', pathlib.Path(__file__).with_name('plot.py'))
        plot = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(plot)
        self.assertEqual(plot.resource_caption({'limits_per_db_container':{'cpus':2,'memory':'4g'}}),
                         '2 CPU / 4 GiB cap')
        self.assertEqual(plot.resource_caption({}), '4 CPU / 8 GiB cap')
        self.assertNotIn('<', plot.resource_caption({'limits_per_db_container':{'cpus':'<','memory':'<script>'}}))

    def test_bolt_query_carries_a_cooperative_server_deadline(self):
        client = object.__new__(runner.Client)
        client.graph = None
        client.session = Mock()
        client.session.run.return_value = []
        self.assertEqual(client.run('RETURN $x', {'x': 1}), [])
        self.assertEqual(client.session.run.call_args.args[0], ('RETURN $x', {'timeout': 60}))
        self.assertEqual(client.session.run.call_args.kwargs, {'x': 1})

    def test_timeout_does_not_echo_command_arguments(self):
        with patch.object(runner.subprocess, 'run', side_effect=subprocess.TimeoutExpired(['secret'], 1)):
            with self.assertRaises(RuntimeError) as caught:
                runner.docker('run', 'secret', timeout=1)
        self.assertNotIn('secret', str(caught.exception))


if __name__ == '__main__':
    unittest.main()
