"""Owned synthetic Bolt acceptance for context reranking and two ACL identities."""
import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))
from neo4j import GraphDatabase
from brainwire import GraphClient
from brainwire.rerank import rerank_context
from examples.python.create_service_tokens import create


def run(binary):
    with tempfile.TemporaryDirectory(prefix='brainwire-context-rerank-') as temporary:
        root = Path(temporary)
        create(root/'auth', 'company', acl_writer=True)
        policy_path = root/'auth/policy.json'
        policy = json.loads(policy_path.read_text())
        for principal in policy['principals']:
            if principal['id'] in ('reader', 'health'):
                principal['node_visibility'] = {'principals': [principal['id']]}
        policy_path.write_text(json.dumps(policy))
        credentials = {role: json.loads((root/f'auth/{role}.json').read_text())
                       for role in ('writer','reader','health')}
        with socket.socket() as sock:
            sock.bind(('127.0.0.1',0))
            port = sock.getsockname()[1]
        config=root/'brainwire.toml'
        config.write_text(f'''[server]
listen = "127.0.0.1:{port}"
data_dir = "{root/'data'}"
default_database = "company"
auth_file = "{policy_path}"
[memory]
cache = "64MiB"
memtables = "4MiB"
query = "32MiB"
limit = "2GiB"
''')
        ontology=root/'ontology.json'
        ontology.write_text(json.dumps({'version':'1','nodes':[
            {'label':'Document','properties':['body','embedding','source_url']}], 'edges':[]}))
        env={key:value for key,value in os.environ.items() if not key.startswith('BRAINWIRE_')}
        subprocess.run([str(binary),'db','create','company','--ontology',str(ontology),
                        '--config',str(config)],env=env,check=True,capture_output=True)
        def connect(role):
            auth=credentials[role]
            return GraphDatabase.driver(f'bolt://127.0.0.1:{port}',
                auth=(auth['principal'],auth['credentials']),connection_timeout=1,
                max_transaction_retry_time=0)
        with (root/'server.log').open('w') as log:
            process=subprocess.Popen([str(binary),'serve','--config',str(config)],
                                     env=env,stdout=log,stderr=log)
            try:
                for _ in range(200):
                    if process.poll() is not None:
                        raise RuntimeError((root/'server.log').read_text())
                    try:
                        with socket.create_connection(('127.0.0.1',port),timeout=.1): break
                    except OSError: time.sleep(.025)
                else: raise RuntimeError('server did not start')
                with connect('writer') as driver:
                    graph=GraphClient(driver,'company')
                    for identity, allow, vector in [('a','reader',[0.,1.]),
                                                    ('b','health',[1.,0.]),
                                                    ('invalid','health',[1.])]:
                        graph.upsert_node_with_acl('Document',identity,
                            {'body':'Release decision','embedding':vector,
                             'source_url':f'source://{identity}'}, {'allow':[allow]})
                for role, expected in [('reader','a'),('health','b')]:
                    with connect(role) as driver:
                        graph=GraphClient(driver,'company')
                        result=rerank_context(graph,'Document',['a','b'],'release',[1.,0.],
                            text_property='body',vector_property='embedding',properties=['source_url'])
                        assert [row['entity_id'] for row in result['records']]==[expected], result
                        assert result['records'][0]['properties']=={'source_url':f'source://{expected}'}
                        if role=='reader':
                            visible_only=rerank_context(graph,'Document',['a'],'release',[1.,0.],
                                text_property='body',vector_property='embedding',properties=['source_url'])
                            assert result == visible_only, 'hidden highest-score candidate changed visible ranking'
                            result=rerank_context(graph,'Document',['a','invalid'],'release',[1.,0.],
                                text_property='body',vector_property='embedding')
                            assert [row['entity_id'] for row in result['records']]==['a'], result
                        else:
                            try:
                                rerank_context(graph,'Document',['invalid'],'release',[1.,0.],
                                    text_property='body',vector_property='embedding')
                            except ValueError: pass
                            else: raise AssertionError('visible dimension mismatch accepted')
                return {'result':'PASS','two_static_acl_identities':True,
                        'hidden_malformed_vector_excluded':True,
                        'hidden_candidate_score_invariance':True,'synthetic':True}
            finally:
                process.terminate()
                try: process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill(); process.wait(timeout=5)


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=Path,default=ROOT/'target/debug/brainwire')
    args=parser.parse_args()
    print(json.dumps(run(args.binary.resolve()),indent=2))
