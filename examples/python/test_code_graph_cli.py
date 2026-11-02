"""CLI acceptance with local synthetic sources; no database or model needed."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


@unittest.skipUnless(os.name == 'posix', 'directory traversal requires POSIX')
class CodeGraphCliTests(unittest.TestCase):
    def run_cli(self, root, *args):
        return subprocess.run([sys.executable, '-m', 'brainwire.code_graph', str(root),
                               '--namespace', 'cli-demo', *args], cwd=ROOT,
                              capture_output=True, text=True, timeout=15)

    def test_default_summary_is_small_and_does_not_execute_source(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            marker = root / 'must-not-exist'
            source = f'from pathlib import Path\nPath({str(marker)!r}).write_text("executed")\ndef run(): pass\n'
            (root / 'app.py').write_text(source)
            result = self.run_cli(root)
            self.assertEqual(result.returncode, 0, result.stderr)
            summary = json.loads(result.stdout)
            self.assertEqual(summary['files'], 1)
            self.assertEqual(summary['entities'], 3)
            self.assertEqual(summary['relationships'], 2)
            self.assertEqual(summary['evidence'], 'EXTRACTED')
            self.assertLess(len(result.stdout.encode()), 1024)
            self.assertNotIn('nodes', summary)
            self.assertFalse(marker.exists())
            self.assertEqual(result.stderr, '')

    def test_json_artifact_is_explicit_and_reproducible(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / 'app.py').write_text('import os\ndef run(): pass\n')
            first = self.run_cli(root, '--json')
            second = self.run_cli(root, '--json')
            self.assertEqual(first.returncode, 0, first.stderr)
            self.assertEqual(first.stdout, second.stdout)
            artifact = json.loads(first.stdout)
            self.assertEqual(len(artifact['nodes']), 3)
            self.assertEqual(len(artifact['edges']), 2)
            self.assertEqual(artifact['files'][0]['path'], 'app.py')

    def test_invalid_input_has_no_artifact_or_source_excerpt(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / 'broken.py').write_text('secret = "DO_NOT_PRINT_THIS_SOURCE"\ndef (')
            result = self.run_cli(root, '--json')
            self.assertEqual(result.returncode, 2)
            self.assertEqual(result.stdout, '')
            self.assertIn('syntax', result.stderr)
            self.assertNotIn('DO_NOT_PRINT_THIS_SOURCE', result.stderr)
            (root / 'broken.py').unlink()
            (root / 'a.py').write_text('')
            (root / 'b.py').write_text('')
            result = self.run_cli(root, '--max-files', '1')
            self.assertEqual(result.returncode, 2)
            self.assertEqual(result.stdout, '')
            self.assertIn('max_files', result.stderr)

    def test_degree_summary_is_explicit_and_bounded(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / 'app.py').write_text('import os\nclass App:\n    def run(self): pass\n')
            result = self.run_cli(root, '--top-degree', '2')
            self.assertEqual(result.returncode, 0, result.stderr)
            summary = json.loads(result.stdout)
            self.assertEqual(len(summary['top_degree']), 2)
            self.assertEqual([row['degree'] for row in summary['top_degree']], [2,2])
            self.assertNotIn('nodes', summary)
            self.assertLess(len(result.stdout.encode()), 8192)
            invalid = self.run_cli(root, '--top-degree', '21')
            self.assertEqual(invalid.returncode, 2)
            self.assertEqual(invalid.stdout, '')

    def test_local_import_mode_is_explicit_and_marks_inference(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / 'service.py').write_text('import helper\n')
            (root / 'helper.py').write_text('def run(): pass\n')
            default = self.run_cli(root, '--json')
            self.assertFalse(any(edge['evidence']=='INFERRED' for edge in json.loads(default.stdout)['edges']))
            resolved = self.run_cli(root, '--resolve-imports', '--json')
            self.assertEqual(resolved.returncode, 0, resolved.stderr)
            artifact = json.loads(resolved.stdout)
            self.assertTrue(any(edge['relationship']=='LOCAL_MODULE_CANDIDATE' for edge in artifact['edges']))
            summary = self.run_cli(root, '--resolve-imports')
            self.assertEqual(json.loads(summary.stdout)['evidence'], 'EXTRACTED_AND_INFERRED')
            self.assertEqual(json.loads(summary.stdout)['generation_id'], artifact['generation_id'])

    def test_static_calls_require_explicit_mode(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp)
            (root/'service.py').write_text('import helper as h\ndef execute(): h.run()\n')
            (root/'helper.py').write_text('def run(): pass\n')
            default=self.run_cli(root,'--resolve-imports','--json')
            self.assertFalse(any(node['kind']=='call' for node in json.loads(default.stdout)['nodes']))
            calls=self.run_cli(root,'--resolve-calls','--json')
            self.assertEqual(calls.returncode,0,calls.stderr)
            artifact=json.loads(calls.stdout)
            self.assertEqual(artifact['extractor'],'brainwire-python-directory-calls-v1')
            self.assertEqual(len([node for node in artifact['nodes'] if node['kind']=='call']),1)
