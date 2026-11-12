import unittest
import tracemalloc
import json
from brainwire.code_graph import extract_python, load_python, ontology


class CodeGraphSizeTests(unittest.TestCase):
    def test_graph_size_uses_exact_utf8_at_the_boundary(self):
        from brainwire.code_graph import _graph_json_size
        artifact = {'nodes': [{'name': 'é🙂\n'}], 'edges': []}
        size = len(json.dumps(artifact, ensure_ascii=False).encode('utf-8'))
        self.assertEqual(_graph_json_size(artifact, size, 'graph exceeds'), size)
        with self.assertRaisesRegex(ValueError, 'graph exceeds'):
            _graph_json_size(artifact, size - 1, 'graph exceeds')
        for count in (0, 1, 31, 32, 33, 64, 65):
            graph = {'nodes': [{'name': 'é🙂\n', 'n': index} for index in range(count)],
                     'edges': [], 'source_hash': 'abc'}
            expected = len(json.dumps(graph, ensure_ascii=False).encode('utf-8'))
            with self.subTest(count=count):
                self.assertEqual(_graph_json_size(graph, expected, 'graph exceeds'), expected)
                with self.assertRaises(ValueError):
                    _graph_json_size(graph, expected - 1, 'graph exceeds')

    @unittest.skipIf(tracemalloc.is_tracing(), 'requires isolated allocation tracking')
    def test_oversized_graph_is_rejected_without_serialized_copy(self):
        from brainwire.code_graph import _graph_json_size
        artifact = {'nodes': [{'name': 'x' * 4000}] * 1000}
        tracemalloc.start()
        try:
            with self.assertRaisesRegex(ValueError, 'graph exceeds'):
                _graph_json_size(artifact, 1 << 20, 'graph exceeds')
            peak = tracemalloc.get_traced_memory()[1]
        finally:
            tracemalloc.stop()
        self.assertLess(peak, 1 << 20)


class CodeGraphTests(unittest.TestCase):
    def test_source_is_not_executed_and_provenance_is_deterministic(self):
        source = 'raise RuntimeError("must not run")\nimport os as operating\nclass App:\n    async def run(self):\n        from .tools import helper\n'
        graph = extract_python(source, source_path='src/app.py', namespace='demo')
        self.assertEqual(graph, extract_python(source, source_path='src/app.py', namespace='demo'))
        nodes = graph['nodes']
        self.assertEqual([n['kind'] for n in nodes], ['file', 'import', 'class', 'function', 'import'])
        self.assertEqual([e['relationship'] for e in graph['edges']], ['IMPORTS', 'DEFINES', 'DEFINES', 'IMPORTS'])
        self.assertTrue(all(e['evidence'] == 'EXTRACTED' and e['source_path'] == 'src/app.py' for e in graph['edges']))
        self.assertEqual(nodes[-1]['name'], '.tools:helper')
        self.assertEqual(nodes[-1]['start_line'], 5)
        self.assertEqual(nodes[3]['qualified_name'], 'App.run')
        self.assertEqual(len(nodes[0]['source_hash']), 64)

    def test_snapshot_identity_changes_with_content_and_path(self):
        first = extract_python('def f(): pass', source_path='a.py', namespace='n')
        self.assertNotEqual(first['nodes'][0]['entity_id'], extract_python('def f(): return 1', source_path='a.py', namespace='n')['nodes'][0]['entity_id'])
        self.assertNotEqual(first['nodes'][0]['entity_id'], extract_python('def f(): pass', source_path='b.py', namespace='n')['nodes'][0]['entity_id'])

    def test_validation_and_bounds_fail_before_writes(self):
        for path in ('/etc/passwd', '../secret.py', 'a/../b.py', 'a\\b.py', 'bad\x7f.py', 'bad\x9b.py', ''):
            with self.assertRaises(ValueError):
                extract_python('', source_path=path, namespace='n')
        with self.assertRaises(ValueError):
            extract_python('x' * (1048576 + 1), source_path='a.py', namespace='n')
        with self.assertRaises(SyntaxError):
            extract_python('def invalid(', source_path='a.py', namespace='n')
        with self.assertRaises(ValueError):
            extract_python('def f(): pass', source_path='a.py', namespace='n', max_entities=1)
        graph = FakeGraph()
        with self.assertRaises(SyntaxError):
            load_python(graph, 'def invalid(', source_path='a.py', namespace='n')
        self.assertEqual(graph.calls, [])

    def test_loading_nodes_before_edges_and_idempotent_rows(self):
        graph = FakeGraph()
        result = load_python(graph, 'import os\ndef f(): pass', source_path='a.py', namespace='n')
        self.assertEqual(result, {'nodes': 3, 'edges': 2, 'source_hash': extract_python('import os\ndef f(): pass', source_path='a.py', namespace='n')['source_hash']})
        self.assertEqual(graph.calls[0][0], 'nodes')
        self.assertTrue(all(call[-1] is True for call in graph.calls[1:]))
        before = list(graph.calls)
        load_python(graph, 'import os\ndef f(): pass', source_path='a.py', namespace='n')
        self.assertEqual(before, graph.calls[len(before):])
        self.assertEqual(ontology()['nodes'][0]['label'], 'CodeEntity')


class FakeGraph:
    def __init__(self): self.calls = []
    def load_nodes(self, label, rows):
        rows = list(rows); self.calls.append(('nodes', label, rows)); return len(rows)
    def load_edges(self, left, relation, right, rows, *, require_endpoints):
        rows = list(rows); self.calls.append(('edges', left, relation, right, rows, require_endpoints)); return len(rows)


class DirectoryGraphTests(unittest.TestCase):
    def test_directory_snapshot_is_sorted_bounded_and_source_is_not_executed(self):
        from brainwire.code_graph import extract_python_directory
        import tempfile
        from pathlib import Path
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / 'z.py').write_text('raise RuntimeError("must not execute")\n')
            (root / 'pkg').mkdir()
            (root / 'pkg/a.py').write_text('import os\ndef run(): pass\n')
            (root / 'ignored.txt').write_text('not Python')
            (root / '.venv').mkdir()
            (root / '.venv/broken.py').write_text('def invalid(')
            artifact = extract_python_directory(root, namespace='n')
            self.assertEqual([item['path'] for item in artifact['files']], ['pkg/a.py', 'z.py'])
            self.assertEqual(artifact, extract_python_directory(root, namespace='n'))
            self.assertEqual(len(artifact['nodes']), 4)
            with self.assertRaises(ValueError):
                extract_python_directory(root, namespace='n', max_files=1)
            with self.assertRaises(ValueError):
                extract_python_directory(root, namespace='n', max_entities=2)

    def test_symlinks_and_concurrent_replacement_never_escape_root(self):
        from brainwire.code_graph import extract_python_directory
        import os
        import tempfile
        from pathlib import Path
        from unittest.mock import patch
        if os.name != 'posix': self.skipTest('descriptor traversal requires POSIX')
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            root = base / 'root'; root.mkdir()
            external = base / 'external.py'; external.write_text('def secret(): pass')
            link = root / 'linked.py'; link.symlink_to(external)
            with self.assertRaises(ValueError):
                extract_python_directory(root, namespace='n')
            link.unlink()
            root_link = base / 'root_link'; root_link.symlink_to(root, target_is_directory=True)
            with self.assertRaises(ValueError):
                extract_python_directory(root_link, namespace='n')
            file = root / 'a.py'; file.write_text('def safe(): pass')
            real_open = os.open
            def replace_before_open(path, flags, *args, **kwargs):
                if path == 'a.py':
                    file.unlink(); file.symlink_to(external)
                return real_open(path, flags, *args, **kwargs)
            with patch('brainwire.code_graph.os.open', replace_before_open):
                with self.assertRaises(ValueError):
                    extract_python_directory(root, namespace='n')

    def test_directory_validates_all_input_before_writes(self):
        from brainwire.code_graph import load_python_directory
        import tempfile
        from pathlib import Path
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / 'a.py').write_text('def valid(): pass')
            (root / 'z.py').write_text('def invalid(')
            graph = FakeGraph()
            with self.assertRaises(SyntaxError):
                load_python_directory(graph, root, namespace='n')
            self.assertEqual(graph.calls, [])
            (root / 'z.py').write_bytes(b'x' * (1048576 + 1))
            with self.assertRaises(ValueError):
                load_python_directory(graph, root, namespace='n')
            self.assertEqual(graph.calls, [])

    def test_snapshots_and_namespaces_do_not_link_to_each_other(self):
        from brainwire.code_graph import extract_python_directory
        import tempfile
        from pathlib import Path
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            file = root / 'a.py'; file.write_text('import os\ndef f(): pass')
            first = extract_python_directory(root, namespace='one')
            other = extract_python_directory(root, namespace='two')
            file.write_text('import sys\ndef f(): pass')
            changed = extract_python_directory(root, namespace='one')
            original_ids = {node['entity_id'] for node in first['nodes']}
            for artifact in (other, changed):
                ids = {node['entity_id'] for node in artifact['nodes']}
                self.assertTrue(original_ids.isdisjoint(ids))
                self.assertTrue(all(edge['source_id'] in ids and edge['target_id'] in ids for edge in artifact['edges']))

    def test_directory_refuses_special_files_invalid_utf8_and_deep_trees(self):
        from brainwire.code_graph import extract_python_directory
        import os
        import tempfile
        from pathlib import Path
        if os.name != 'posix': self.skipTest('descriptor traversal requires POSIX')
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            fifo = root / 'pipe.py'; os.mkfifo(fifo)
            with self.assertRaises(ValueError):
                extract_python_directory(root, namespace='n')
            fifo.unlink()
            invalid = root / 'invalid.py'; invalid.write_bytes(b'\xff')
            with self.assertRaises(UnicodeDecodeError):
                extract_python_directory(root, namespace='n')
            invalid.unlink()
            child = root
            for _ in range(17):
                child = child / 'nested'; child.mkdir()
            with self.assertRaises(ValueError):
                extract_python_directory(root, namespace='n')

    def test_directory_source_bytes_bound_and_skip_excluded_symlink(self):
        from brainwire.code_graph import extract_python_directory
        import tempfile
        from pathlib import Path
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / '.venv').symlink_to(root / 'missing', target_is_directory=True)
            self.assertEqual(extract_python_directory(root, namespace='n')['files'], [])
            content = '#' + 'x' * (1048576 - 1)
            for index in range(9):
                (root / f'{index}.py').write_text(content)
            with self.assertRaises(ValueError):
                extract_python_directory(root, namespace='n')


class DegreeSummaryTests(unittest.TestCase):
    def test_directed_distinct_edges_and_stable_ties(self):
        from brainwire.code_graph import degree_summary
        artifact = extract_python('import os\nclass App:\n    def run(self): pass\n', source_path='app.py', namespace='n')
        rows = degree_summary(artifact, limit=4)
        by_id = {row['entity_id']: row for row in rows}
        file = artifact['nodes'][0]['entity_id']
        cls = next(node['entity_id'] for node in artifact['nodes'] if node['kind']=='class')
        self.assertEqual((by_id[file]['in_degree'], by_id[file]['out_degree']), (0, 2))
        self.assertEqual((by_id[cls]['in_degree'], by_id[cls]['out_degree']), (1, 1))
        self.assertEqual(rows, sorted(rows, key=lambda row: (-row['degree'], row['entity_id'])))
        artifact['edges'].append(dict(artifact['edges'][0]))
        self.assertEqual(rows, degree_summary(artifact, limit=4))

    def test_self_loop_and_different_relationship_types_are_counted(self):
        from brainwire.code_graph import degree_summary
        artifact = extract_python('', source_path='app.py', namespace='n')
        identity = artifact['nodes'][0]['entity_id']
        artifact['edges'] = [{'source_id':identity, 'target_id':identity, 'relationship':rel}
                             for rel in ('DEFINES','IMPORTS')]
        row = degree_summary(artifact, limit=1)[0]
        self.assertEqual((row['in_degree'],row['out_degree'],row['degree']), (2,2,4))
        artifact['edges'][0]['target_id'] = 'missing'
        with self.assertRaises(ValueError): degree_summary(artifact)
        with self.assertRaises(ValueError): degree_summary(artifact, limit=21)


class LocalImportTests(unittest.TestCase):
    def test_candidate_edges_use_complete_directory_generation_and_preserve_default(self):
        from brainwire.code_graph import extract_python_directory
        import tempfile
        from pathlib import Path
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root/'service.py').write_text('import helper\n')
            (root/'helper.py').write_text('def run(): pass\n')
            default = extract_python_directory(root, namespace='n')
            first = extract_python_directory(root, namespace='n', resolve_imports=True)
            self.assertEqual(first, extract_python_directory(root, namespace='n', resolve_imports=True))
            ids = {node['entity_id'] for node in first['nodes']}
            self.assertTrue(ids.isdisjoint({node['entity_id'] for node in default['nodes']}))
            self.assertTrue(all(node['generation_id']==first['generation_id'] for node in first['nodes']))
            self.assertTrue(all(edge['generation_id']==first['generation_id'] for edge in first['edges']))
            self.assertTrue(all(edge['source_id'] in ids and edge['target_id'] in ids for edge in first['edges']))
            candidates=[edge for edge in first['edges'] if edge['relationship']=='LOCAL_MODULE_CANDIDATE']
            self.assertEqual(len(candidates),1)
            self.assertEqual(candidates[0]['evidence'],'INFERRED')
            (root/'helper.py').write_text('def changed(): pass\n')
            changed=extract_python_directory(root,namespace='n',resolve_imports=True)
            changed_ids={node['entity_id'] for node in changed['nodes']}
            self.assertTrue(ids.isdisjoint(changed_ids))
            self.assertNotEqual(first['generation_id'],changed['generation_id'])
            other=extract_python_directory(root,namespace='other',resolve_imports=True)
            self.assertTrue(changed_ids.isdisjoint({node['entity_id'] for node in other['nodes']}))
            (root/'helper.py').unlink()
            removed=extract_python_directory(root,namespace='n',resolve_imports=True)
            self.assertFalse(any(edge['relationship']=='LOCAL_MODULE_CANDIDATE' for edge in removed['edges']))
            self.assertTrue(changed_ids.isdisjoint({node['entity_id'] for node in removed['nodes']}))

    def test_packages_relatives_and_ambiguous_modules(self):
        from brainwire.code_graph import extract_python_directory
        import tempfile
        from pathlib import Path
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp)
            (root/'pkg').mkdir();(root/'pkg/__init__.py').write_text('')
            (root/'pkg/tools.py').write_text('def run(): pass')
            (root/'pkg/service.py').write_text('from .tools import run\nfrom ..tools import invalid\nfrom . import tools\n')
            (root/'main.py').write_text('import pkg.tools\nfrom pkg import tools\nfrom .pkg import invalid\n')
            artifact=extract_python_directory(root,namespace='n',resolve_imports=True)
            nodes={node['entity_id']:node for node in artifact['nodes']}
            pairs=[(nodes[edge['source_id']]['name'],nodes[edge['target_id']]['source_path'])
                   for edge in artifact['edges'] if edge['relationship']=='LOCAL_MODULE_CANDIDATE']
            self.assertEqual(set(pairs),{('.tools:run','pkg/tools.py'),('.:tools','pkg/__init__.py'),
                                         ('pkg.tools','pkg/tools.py'),('pkg:tools','pkg/__init__.py')})
            (root/'pkg.py').write_text('')
            ambiguous=extract_python_directory(root,namespace='n',resolve_imports=True)
            self.assertFalse(any(edge['relationship']=='LOCAL_MODULE_CANDIDATE' for edge in ambiguous['edges']))


class StaticCallTests(unittest.TestCase):
    def extract(self, root, source, helper='def run(): pass\n', **kwargs):
        from brainwire.code_graph import extract_python_directory
        (root/'service.py').write_text(source)
        (root/'helper.py').write_text(helper)
        return extract_python_directory(root, namespace='demo', resolve_calls=True, **kwargs)

    def test_static_call_candidates_use_same_parse_and_distinct_same_line_sites(self):
        import tempfile
        from pathlib import Path
        from unittest.mock import patch
        import ast
        from brainwire.code_graph import extract_python_directory
        source='import helper as h\nfrom helper import run as go\ndef execute():\n    h.run(); go()\n'
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp)
            (root/'service.py').write_text(source);(root/'helper.py').write_text('def run(): pass\n')
            original_parse=ast.parse
            with patch('brainwire.code_graph.ast.parse', wraps=original_parse) as parse:
                artifact=extract_python_directory(root, namespace='demo', resolve_calls=True)
            self.assertEqual(parse.call_count,2)
            calls=[node for node in artifact['nodes'] if node['kind']=='call']
            self.assertEqual(len(calls),2)
            self.assertNotEqual(calls[0]['start_column'],calls[1]['start_column'])
            self.assertEqual(len({node['entity_id'] for node in calls}),2)
            candidates=[edge for edge in artifact['edges'] if edge['relationship']=='STATIC_CALL_CANDIDATE']
            self.assertEqual(len(candidates),2)
            nodes={node['entity_id']:node for node in artifact['nodes']}
            self.assertTrue(all(nodes[edge['target_id']]['qualified_name']=='run' for edge in candidates))
            self.assertTrue(all(edge['evidence']=='INFERRED' and edge['source_path']=='service.py' for edge in candidates))
            self.assertEqual(artifact,extract_python_directory(root,namespace='demo',resolve_calls=True))
            imports=extract_python_directory(root,namespace='demo',resolve_imports=True)
            self.assertTrue({node['entity_id'] for node in artifact['nodes']}.isdisjoint({node['entity_id'] for node in imports['nodes']}))
            (root/'helper.py').write_text('def run(): return 1\n')
            changed=extract_python_directory(root,namespace='demo',resolve_calls=True)
            self.assertTrue({node['entity_id'] for node in artifact['nodes']}.isdisjoint({node['entity_id'] for node in changed['nodes']}))

    def test_shadowing_and_dynamic_binding_cases_are_conservatively_omitted(self):
        import tempfile
        from pathlib import Path
        sources=[
            'import helper as h\nh = other\ndef execute(): h.run()\n',
            'import helper as h\ndef execute(h): h.run()\n',
            'import helper as h\ndef execute():\n    h.run()\n    h = other\n',
            'import helper as h\ndef execute():\n    for h in []: pass\n    h.run()\n',
            'import helper as h\ndef execute():\n    with other as h: pass\n    h.run()\n',
            'import helper as h\ndef execute():\n    try: pass\n    except Exception as h: pass\n    h.run()\n',
            'import helper as h\ndef execute():\n    (h := other)\n    h.run()\n',
            'import helper as h\ndef execute():\n    del h\n    h.run()\n',
            'import helper as h\ndef execute():\n    global h\n    h.run()\n',
            'import helper as h\ndef execute():\n    nonlocal h\n    h.run()\n',
            'import helper as h\ndef execute():\n    import other as h\n    h.run()\n',
            'import helper as h\ndef execute():\n    def h(): pass\n    h.run()\n',
            'import helper as h\nh.run = other\ndef execute(): h.run()\n',
            'import helper as h\nother=h\nother.run=mutated\ndef execute(): h.run()\n',
            'import helper as h\nmutate(h)\ndef execute(): h.run()\n',
            'import helper as h\nh["run"] = other\ndef execute(): h.run()\n',
            'import helper as h\nother.setattr(h,"run",other)\ndef execute(): h.run()\n',
            'import helper as h\nexec("anything")\ndef execute(): h.run()\n',
            'if True:\n    import helper as h\ndef execute(): h.run()\n',
            'def execute():\n    import helper as h\n    h.run()\n',
            'import helper as h\nclass C:\n    def execute(self): h.run()\n',
            'import helper as h\ndef execute():\n    def nested(): h.run()\n',
        ]
        with tempfile.TemporaryDirectory() as temp:
            for source in sources:
                artifact=self.extract(Path(temp),source)
                self.assertFalse(any(edge['relationship']=='STATIC_CALL_CANDIDATE' for edge in artifact['edges']),source)
            for helper in ('def run(): pass\nrun=other\n','@decorate\ndef run(): pass\n','def run(): pass\ndef run(): pass\n','def run(): pass\nrun.__code__=other\n','def run(): pass\nother=run\nother.__code__=mutated\n'):
                artifact=self.extract(Path(temp),'import helper as h\ndef execute(): h.run()\n',helper)
                self.assertFalse(any(edge['relationship']=='STATIC_CALL_CANDIDATE' for edge in artifact['edges']))

    def test_call_entity_bound_is_checked_before_writes(self):
        from brainwire.code_graph import load_python_directory
        import tempfile
        from pathlib import Path
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp)
            (root/'service.py').write_text('import helper as h\ndef execute(): h.run()\n')
            (root/'helper.py').write_text('def run(): pass\n')
            graph=FakeGraph()
            with self.assertRaises(ValueError):
                load_python_directory(graph,root,namespace='demo',resolve_calls=True,max_entities=5)
            self.assertEqual(graph.calls,[])


    def test_analysis_bound_and_import_binding_forms(self):
        import tempfile
        from pathlib import Path
        from brainwire.code_graph import extract_python_directory
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp)
            source='from helper import run as go\ndef execute(go): go()\n'
            artifact=self.extract(root,source)
            self.assertFalse(any(node['kind']=='call' for node in artifact['nodes']))
            source='import helper as h\nimport helper as h\ndef execute(): h.run()\n'
            artifact=self.extract(root,source)
            self.assertFalse(any(node['kind']=='call' for node in artifact['nodes']))
            source='import helper as h\ndef execute():\n' + '    h.run()\n' * 10001
            (root/'service.py').write_text(source)
            with self.assertRaises(ValueError):
                extract_python_directory(root,namespace='demo',resolve_calls=True)

    @unittest.skipUnless(__import__('sys').version_info >= (3,10), 'pattern syntax requires Python 3.10')
    def test_pattern_bindings_disable_candidates(self):
        import tempfile
        from pathlib import Path
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp)
            for pattern in ('h', '[*h]', '{"key": other, **h}'):
                source='import helper as h\ndef execute(value):\n    match value:\n        case ' + pattern + ': pass\n    h.run()\n'
                artifact=self.extract(root,source)
                self.assertFalse(any(node['kind']=='call' for node in artifact['nodes']))
