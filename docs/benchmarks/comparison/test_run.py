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
    'neo4j': types.SimpleNamespace(GraphDatabase=Mock()),
    'redis': types.SimpleNamespace(Redis=Mock()),
}):
    spec = importlib.util.spec_from_file_location('comparison_runner', pathlib.Path(__file__).with_name('run.py'))
    runner = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(runner)


class RunnerTest(unittest.TestCase):
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

    def test_brainwire_fixture_is_explicit_and_cleanup_stays_scoped(self):
        calls = []
        def docker(*args):
            calls.append(args)
            if args[0] == 'port':
                raise RuntimeError('container exited')
            return 'fixture-id'
        with tempfile.TemporaryDirectory() as workdir:
            args = types.SimpleNamespace(workdir=workdir)
            with patch.object(runner, 'docker', side_effect=docker), \
                 patch.object(runner, 'container_logs', return_value='stdout:\nstarted\nstderr:\nlistener refused'), \
                 patch.object(runner.subprocess, 'run') as cleanup:
                with self.assertRaises(RuntimeError) as caught:
                    runner.run_system('brainwire', args, 'unit-unique')
            self.assertEqual(list(pathlib.Path(workdir).iterdir()), [])
        runs = [call for call in calls if call[0] == 'run']
        self.assertEqual(len(runs), 2)
        self.assertTrue(all('--pull=never' in call for call in runs))
        self.assertEqual(runs[1][-2:], ('serve', '--allow-unauthenticated-remote'))
        self.assertIn('127.0.0.1::7687', runs[1])
        self.assertIn('BRAINWIRE_SERVER_LISTEN=0.0.0.0:7687', runs[1])
        self.assertIn('listener refused', str(caught.exception))
        self.assertIn('started', str(caught.exception))
        self.assertEqual([call.args[0] for call in cleanup.call_args_list], [
            ['docker', 'rm', '-f', 'brainwire-comparison-unit-unique-brainwire'],
            ['docker', 'volume', 'rm', 'bw-compare-unit-unique-brainwire'],
        ])


if __name__ == '__main__':
    unittest.main()
