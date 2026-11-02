"""The Python installer must never overwrite an existing environment."""
import contextlib
import importlib.util
import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('setup_python', Path(__file__).with_name('setup_python.py'))


class SetupPythonTests(unittest.TestCase):
    def load(self):
        module = importlib.util.module_from_spec(SPEC)
        SPEC.loader.exec_module(module)
        return module

    def test_existing_destination_is_refused_before_running_any_command(self):
        module = self.load()
        with tempfile.TemporaryDirectory() as temporary:
            target = Path(temporary) / 'existing'
            target.mkdir()
            marker = target / 'keep'
            marker.write_text('user data')
            with patch.object(module.subprocess, 'run') as run:
                with self.assertRaisesRegex(ValueError, 'already exists'):
                    module.install(target)
                run.assert_not_called()
            self.assertEqual(marker.read_text(), 'user data')

    @unittest.skipUnless(hasattr(__import__('os'), 'symlink'), 'symlink support unavailable')
    def test_dangling_symlink_is_never_used_as_environment(self):
        module = self.load()
        with tempfile.TemporaryDirectory() as temporary:
            target = Path(temporary) / 'existing-link'
            try:
                target.symlink_to(Path(temporary) / 'missing')
            except OSError as error:
                if getattr(error, 'winerror', None) == 1314:
                    self.skipTest('Windows account cannot create symbolic links')
                raise
            with patch.object(module.subprocess, 'run') as run:
                with self.assertRaisesRegex(ValueError, 'already exists'):
                    module.install(target)
                run.assert_not_called()
            self.assertTrue(target.is_symlink())

    def test_installs_into_fresh_environment_without_activation(self):
        module = self.load()
        with tempfile.TemporaryDirectory() as temporary:
            target = Path(temporary) / 'new environment'
            with patch.object(module.subprocess, 'run') as run, contextlib.redirect_stdout(io.StringIO()) as output:
                module.install(target)
            python = target / ('Scripts/python.exe' if module.os.name == 'nt' else 'bin/python')
            self.assertEqual(run.call_args_list[0].args[0], [module.sys.executable, '-m', 'venv', str(target)])
            self.assertEqual(run.call_args_list[1].args[0], [str(python), '-m', 'pip', 'install', '--upgrade', 'pip>=23'])
            self.assertEqual(run.call_args_list[2].args[0], [str(python), '-m', 'pip', 'install', str(module.ROOT)])
            self.assertTrue(all(call.kwargs['check'] for call in run.call_args_list))
            self.assertIn(str(python), output.getvalue())

    def test_failed_environment_creation_does_not_attempt_install(self):
        module = self.load()
        with tempfile.TemporaryDirectory() as temporary:
            with patch.object(module.subprocess, 'run', side_effect=module.subprocess.CalledProcessError(1, 'venv')) as run:
                with self.assertRaises(module.subprocess.CalledProcessError):
                    module.install(Path(temporary) / 'new')
                self.assertEqual(run.call_count, 1)


if __name__ == '__main__':
    unittest.main()
