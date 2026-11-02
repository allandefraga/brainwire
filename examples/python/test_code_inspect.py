"""Bounded offline artifact inspection and provenance-preserving path tests."""
import copy
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from brainwire.code_graph import extract_python, extract_python_directory
from brainwire.code_inspect import explain_node, find_path, load_artifact, InspectionLimitError

ROOT = Path(__file__).resolve().parents[2]


class InspectionTests(unittest.TestCase):
    def test_explain_is_bounded_and_ambiguity_is_explicit(self):
        artifact = extract_python('class App:\n    def run(self): pass\n', source_path='app.py', namespace='demo')
        out = explain_node(artifact, 'App')
        self.assertEqual(out['node']['kind'], 'class')
        self.assertEqual(out['node']['source_path'], 'app.py')
        self.assertEqual(out['incident_count'], 2)
        self.assertEqual(len(out['edges']), 2)
        limited = explain_node(artifact, 'App', max_edges=1)
        self.assertEqual(len(limited['edges']), 1)
        self.assertTrue(limited['truncated'])
        with self.assertRaises(InspectionLimitError): explain_node(artifact, 'App', max_bytes=1)
        duplicates = extract_python('def f(): pass\ndef f(): pass\n', source_path='a.py', namespace='demo')
        with self.assertRaises(ValueError): explain_node(duplicates, 'f')
        self.assertEqual(explain_node(duplicates, duplicates['nodes'][1]['entity_id'])['node']['name'], 'f')

    def test_path_preserves_directions_evidence_and_determinism(self):
        artifact = extract_python('class App:\n    def run(self): pass\n', source_path='app.py', namespace='demo')
        out = find_path(artifact, 'App.run', 'app.py')
        self.assertEqual(out['status'], 'found')
        self.assertEqual(out['hops'], 2)
        self.assertEqual([edge['direction'] for edge in out['edges']], ['incoming','incoming'])
        self.assertEqual(out['evidence'], 'EXTRACTED')
        self.assertEqual(out, find_path(artifact, 'App.run', 'app.py'))
        self.assertEqual(find_path(artifact, 'App.run', 'app.py', directed=True)['status'], 'not_found_within_bounds')
        self.assertEqual(find_path(artifact, 'App.run', 'app.py', max_hops=1)['status'], 'not_found_within_bounds')
        with self.assertRaises(InspectionLimitError): find_path(artifact, 'App.run', 'app.py', max_visited=1)
        self.assertEqual(find_path(artifact, 'App', 'App')['hops'], 0)

    @unittest.skipUnless(os.name == 'posix', 'directory extraction requires POSIX')
    def test_inferred_cross_file_path_is_labelled(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp)
            (root/'service.py').write_text('import helper\n')
            (root/'helper.py').write_text('def run(): pass\n')
            artifact=extract_python_directory(root, namespace='demo', resolve_imports=True)
            out=find_path(artifact, 'service.py', 'run')
            self.assertEqual(out['hops'],3)
            self.assertEqual(out['evidence'],'INFERRED')
            self.assertEqual([edge['relationship'] for edge in out['edges']], ['IMPORTS','LOCAL_MODULE_CANDIDATE','DEFINES'])
            self.assertTrue(all(node['generation_id']==artifact['generation_id'] for node in out['nodes']))

    @unittest.skipUnless(os.name == 'posix', 'directory extraction requires POSIX')
    def test_path_ties_are_independent_of_artifact_order_and_generations_are_checked(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp)
            (root/'a.py').write_text('import b\nimport b\n')
            (root/'b.py').write_text('def run(): pass\n')
            artifact=extract_python_directory(root,namespace='demo',resolve_imports=True)
            first=find_path(artifact,'a.py','run')
            reordered=copy.deepcopy(artifact)
            reordered['nodes'].reverse();reordered['edges'].reverse()
            self.assertEqual(first,find_path(reordered,'a.py','run'))
            self.assertEqual(first['hops'],3)
            bad=copy.deepcopy(artifact);bad['nodes'][0]['generation_id']='0'*64
            with self.assertRaises(ValueError):find_path(bad,'a.py','run')

    def test_explain_does_not_echo_unknown_properties(self):
        artifact=extract_python('def f(): pass',source_path='a.py',namespace='demo')
        artifact['nodes'][1]['arbitrary_secret']='NEVER_ECHO_EXTRA_PROPERTY'
        artifact['edges'][0]['arbitrary_secret']='NEVER_ECHO_EXTRA_PROPERTY'
        self.assertNotIn('NEVER_ECHO_EXTRA_PROPERTY',json.dumps(explain_node(artifact,'f')))
        artifact['nodes'][1]['start_line']=True
        with self.assertRaises(ValueError):explain_node(artifact,'f')

    def test_found_target_does_not_spend_budget_on_unneeded_siblings(self):
        artifact=extract_python('def first(): pass\ndef second(): pass\n',source_path='a.py',namespace='demo')
        target=min((node for node in artifact['nodes'] if node['kind']=='function'), key=lambda node:node['entity_id'])
        out=find_path(artifact,'a.py',target['entity_id'],max_visited=2)
        self.assertEqual(out['status'],'found')
        self.assertEqual(out['visited'],2)

    def test_malformed_graph_and_cross_generation_are_rejected(self):
        artifact=extract_python('def f(): pass\n', source_path='a.py', namespace='demo')
        bad=copy.deepcopy(artifact);bad['edges'][0]['target_id']='missing'
        with self.assertRaises(ValueError): find_path(bad,'a.py','f')
        bad=copy.deepcopy(artifact);bad['edges'][0]['evidence']='INFERRED'
        with self.assertRaises(ValueError): explain_node(bad,'f')
        bad=copy.deepcopy(artifact);bad['nodes'][1]['namespace']='other'
        with self.assertRaises(ValueError): explain_node(bad,'f')
        with self.assertRaises(ValueError): find_path(artifact,'a.py','f',max_hops=17)

    @unittest.skipUnless(os.name == 'posix', 'artifact loader requires POSIX')
    def test_loader_rejects_duplicate_keys_oversize_and_special_files(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);file=root/'graph.json'
            artifact=extract_python('',source_path='a.py',namespace='demo')
            file.write_text(json.dumps(artifact))
            self.assertEqual(load_artifact(file),artifact)
            file.write_text('{"schema_version":"1","schema_version":"2"}')
            with self.assertRaises(ValueError): load_artifact(file)
            file.write_text(json.dumps({**artifact,'unexpected':float('nan')}))
            with self.assertRaises(ValueError):load_artifact(file)
            file.write_text(json.dumps({**artifact,'unexpected':1.5}))
            with self.assertRaises(ValueError):load_artifact(file)
            file.write_bytes(b' ' * ((8 << 20) + 1))
            with self.assertRaises(InspectionLimitError): load_artifact(file)
            if os.name=='posix':
                file.unlink();os.mkfifo(file)
                with self.assertRaises(ValueError):load_artifact(file)


@unittest.skipUnless(os.name == 'posix', 'artifact CLI requires POSIX')
class InspectionCliTests(unittest.TestCase):
    def test_offline_cli_reads_artifact_only_and_reports_errors_without_partial_json(self):
        with tempfile.TemporaryDirectory() as temp:
            file=Path(temp)/'graph.json'
            file.write_text(json.dumps(extract_python('class App:\n    def run(self): pass\n',source_path='missing-source.py',namespace='demo')))
            def run(*args):
                return subprocess.run([sys.executable,'-m','brainwire.code_inspect',str(file),*args],cwd=ROOT,capture_output=True,text=True,timeout=10)
            explained=run('explain','App')
            self.assertEqual(explained.returncode,0,explained.stderr)
            self.assertEqual(json.loads(explained.stdout)['node']['name'],'App')
            path=run('path','App.run','missing-source.py')
            self.assertEqual(path.returncode,0,path.stderr)
            self.assertEqual(json.loads(path.stdout)['hops'],2)
            denied=run('path','App.run','missing-source.py','--max-visited','1')
            self.assertEqual(denied.returncode,2)
            self.assertEqual(denied.stdout,'')
            file.write_text('not JSON')
            invalid=run('explain','App')
            self.assertEqual(invalid.returncode,2)
            self.assertEqual(invalid.stdout,'')


@unittest.skipUnless(os.name=='posix','directory extraction requires POSIX')
class StaticCallInspectionTests(unittest.TestCase):
    def test_path_and_explanation_preserve_static_call_provenance(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp)
            (root/'service.py').write_text('import helper as h\ndef execute(): h.run()\n')
            (root/'helper.py').write_text('def run(): pass\n')
            artifact=extract_python_directory(root,namespace='demo',resolve_calls=True)
            path=find_path(artifact,'execute','run')
            self.assertEqual(path['hops'],2)
            self.assertEqual(path['evidence'],'INFERRED')
            self.assertEqual([edge['relationship'] for edge in path['edges']],['HAS_CALL','STATIC_CALL_CANDIDATE'])
            site=path['nodes'][1]
            self.assertEqual(site['kind'],'call')
            self.assertEqual(site['start_line'],2)
            self.assertGreater(site['start_column'],0)
            self.assertEqual(explain_node(artifact,site['entity_id'])['incident_count'],2)
            bad=copy.deepcopy(artifact)
            next(edge for edge in bad['edges'] if edge['relationship']=='STATIC_CALL_CANDIDATE')['evidence']='EXTRACTED'
            with self.assertRaises(ValueError):find_path(bad,'execute','run')
