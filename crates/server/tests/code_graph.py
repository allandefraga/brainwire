"""Real Bolt acceptance for immutable Python AST snapshots and evidence."""
import argparse, json, os, socket, subprocess, sys, tempfile, time
from pathlib import Path
ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))
from brainwire import connect
from brainwire.code_graph import ontology, load_python, load_python_directory
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/brainwire')
binary = parser.parse_args().binary.resolve()
env={k:v for k,v in os.environ.items() if not k.startswith('BRAINWIRE_')}
with tempfile.TemporaryDirectory(prefix='brainwire-code-smoke-') as temp:
    root=Path(temp)
    with socket.socket() as sock:
        sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
    config=root/'brainwire.toml'
    config.write_text(f'[server]\nlisten="127.0.0.1:{port}"\ndata_dir="{root / "data"}"\ndefault_database="code"\n[memory]\ncache="64MiB"\nmemtables="4MiB"\nquery="32MiB"\n')
    catalog=root/'ontology.json';catalog.write_text(json.dumps(ontology()))
    subprocess.run([str(binary),'db','create','code','--ontology',str(catalog),'--config',str(config)],env=env,check=True,capture_output=True)
    with (root/'server.log').open('w') as log:
        process=subprocess.Popen([str(binary),'serve','--config',str(config)],env=env,stdout=log,stderr=log)
        try:
            for _ in range(200):
                if process.poll() is not None: raise RuntimeError((root/'server.log').read_text())
                try:
                    with socket.create_connection(('127.0.0.1',port),timeout=.1):break
                except OSError:time.sleep(.025)
            else:raise RuntimeError('startup timeout')
            with connect(f'bolt://127.0.0.1:{port}','code',auth=None) as graph:
                source='import os\nclass App:\n    def run(self): pass\n'
                for _ in range(2):
                    assert load_python(graph,source,source_path='app.py',namespace='synthetic')['nodes']==4
                assert graph.run_cypher('MATCH (n:CodeEntity) RETURN count(*) AS count')==[{'count':4}]
                assert graph.run_cypher('MATCH (a:CodeEntity)-[:DEFINES]->(b:CodeEntity) RETURN count(*) AS count')==[{'count':2}]
                assert graph.run_cypher('MATCH (a:CodeEntity)-[:IMPORTS]->(b:CodeEntity) RETURN count(*) AS count')==[{'count':1}]
                evidence=graph.run_cypher('MATCH (a:CodeEntity)-[r:DEFINES]->(b:CodeEntity) RETURN r.evidence AS evidence, r.source_path AS path')
                assert all(row=={'evidence':'EXTRACTED','path':'app.py'} for row in evidence),evidence
                source_dir = root / 'sources'; source_dir.mkdir()
                source_file = source_dir / 'app.py'; source_file.write_text(source)
                for _ in range(2):
                    receipt = load_python_directory(graph, source_dir, namespace='directory')
                    assert receipt['files'] == 1 and receipt['nodes'] == 4
                assert graph.run_cypher('MATCH (n:CodeEntity) RETURN count(*) AS count') == [{'count':8}]
                source_file.write_text(source.replace('import os', 'import sys'))
                load_python_directory(graph, source_dir, namespace='directory')
                load_python_directory(graph, source_dir, namespace='other')
                assert graph.run_cypher('MATCH (n:CodeEntity) RETURN count(*) AS count') == [{'count':16}]
                for relation in ('DEFINES', 'IMPORTS'):
                    for field in ('namespace', 'source_hash'):
                        query = (f'MATCH (a:CodeEntity)-[:{relation}]->(b:CodeEntity) '
                                 f'WHERE a.{field} <> b.{field} RETURN count(*) AS count')
                        assert graph.run_cypher(query) == [{'count':0}]
                (source_dir / 'service.py').write_text('import helper\n')
                helper = source_dir / 'helper.py'; helper.write_text('def run(): pass\n')
                first = load_python_directory(graph, source_dir, namespace='directory', resolve_imports=True)
                replay = load_python_directory(graph, source_dir, namespace='directory', resolve_imports=True)
                assert first == replay and first['nodes'] == 8
                assert graph.run_cypher('MATCH (n:CodeEntity) RETURN count(*) AS count') == [{'count':24}]
                candidate_query = ('MATCH (a:CodeEntity)-[r:LOCAL_MODULE_CANDIDATE]->(b:CodeEntity) '
                                   'WHERE r.generation_id=$generation RETURN count(*) AS count')
                assert graph.run_cypher(candidate_query, {'generation': first['generation_id']}) == [{'count':1}]
                helper.write_text('def changed(): pass\n')
                changed = load_python_directory(graph, source_dir, namespace='directory', resolve_imports=True)
                assert changed['generation_id'] != first['generation_id']
                assert graph.run_cypher('MATCH (n:CodeEntity) RETURN count(*) AS count') == [{'count':32}]
                load_python_directory(graph, source_dir, namespace='other', resolve_imports=True)
                assert graph.run_cypher('MATCH (n:CodeEntity) RETURN count(*) AS count') == [{'count':40}]
                helper.unlink()
                removed = load_python_directory(graph, source_dir, namespace='directory', resolve_imports=True)
                assert graph.run_cypher(candidate_query, {'generation': removed['generation_id']}) == [{'count':0}]
                assert graph.run_cypher('MATCH (n:CodeEntity) RETURN count(*) AS count') == [{'count':46}]
                # Old-generation edges remain, but no edge can connect generations.
                for relation in ('DEFINES', 'IMPORTS', 'LOCAL_MODULE_CANDIDATE'):
                    query = (f'MATCH (a:CodeEntity)-[r:{relation}]->(b:CodeEntity) '
                             'WHERE a.generation_id <> b.generation_id RETURN count(*) AS count')
                    assert graph.run_cypher(query) == [{'count':0}]
                    query = (f'MATCH (a:CodeEntity)-[r:{relation}]->(b:CodeEntity) '
                             'WHERE a.namespace <> b.namespace RETURN count(*) AS count')
                    assert graph.run_cypher(query) == [{'count':0}]
                (source_dir / 'service.py').write_text('import helper as h\ndef execute(): h.run(); h.run()\n')
                helper.write_text('def run(): pass\n')
                calls = load_python_directory(graph, source_dir, namespace='calls', resolve_calls=True)
                assert calls['nodes'] == 11
                assert calls == load_python_directory(graph, source_dir, namespace='calls', resolve_calls=True)
                assert graph.run_cypher('MATCH (n:CodeEntity) RETURN count(*) AS count') == [{'count':57}]
                calls_query = ('MATCH (a:CodeEntity)-[r:STATIC_CALL_CANDIDATE]->(b:CodeEntity) '
                               'WHERE r.generation_id=$generation RETURN count(*) AS count')
                assert graph.run_cypher(calls_query, {'generation': calls['generation_id']}) == [{'count':2}]
                rows = graph.run_cypher(
                    'MATCH (a:CodeEntity)-[r:STATIC_CALL_CANDIDATE]->(b:CodeEntity) '
                    'WHERE r.generation_id=$generation '
                    'RETURN a.start_line AS line,a.start_column AS col,r.evidence AS evidence,b.name AS target',
                    {'generation': calls['generation_id']})
                assert len({row['col'] for row in rows}) == 2
                assert all(row['line']==2 and row['evidence']=='INFERRED' and row['target']=='run' for row in rows)
                helper.write_text('def run(): return 1\n')
                changed_calls=load_python_directory(graph, source_dir, namespace='calls', resolve_calls=True)
                assert changed_calls['generation_id'] != calls['generation_id']
                assert graph.run_cypher('MATCH (n:CodeEntity) RETURN count(*) AS count') == [{'count':68}]
                helper.unlink()
                removed_calls=load_python_directory(graph, source_dir, namespace='calls', resolve_calls=True)
                assert removed_calls['nodes']==7
                assert graph.run_cypher(calls_query, {'generation': removed_calls['generation_id']}) == [{'count':0}]
                assert graph.run_cypher('MATCH (n:CodeEntity) RETURN count(*) AS count') == [{'count':75}]
                for relation in ('HAS_CALL','STATIC_CALL_CANDIDATE'):
                    query=(f'MATCH (a:CodeEntity)-[r:{relation}]->(b:CodeEntity) '
                           'WHERE a.generation_id <> b.generation_id RETURN count(*) AS count')
                    assert graph.run_cypher(query)==[{'count':0}]
                print('PASS: real Bolt AST/import/call snapshots, replay, target changes/removal and generation isolation')
        finally:
            process.terminate()
            try:process.wait(timeout=5)
            except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)
