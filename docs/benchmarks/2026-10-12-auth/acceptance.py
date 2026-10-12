"""Protected real Bolt/native-client acceptance. Run from the repository root.

uv run --no-project --with neo4j==5.28.6 python this_file.py
All tokens, records, stores and server processes are temporary and synthetic.
"""
import argparse
from contextlib import suppress
import hashlib
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import sys
import tempfile
import time

from neo4j import GraphDatabase, __version__
from neo4j.exceptions import Neo4jError

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))
from examples.python.resumable_customers import ingest
from examples.client.ingest_snapshot import identity


def run(binary):
    report = {'driver': __version__, 'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'scope': 'synthetic localhost service-token auth, not TLS or distributed acceptance'}
    with tempfile.TemporaryDirectory(prefix='brainwire-protected-') as folder:
        temporary = Path(folder)
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0)); address = '%s:%s' % sock.getsockname()
        env = {k:v for k,v in os.environ.items() if not k.startswith('BRAINWIRE_')}
        env.update(BRAINWIRE_SERVER_DATA_DIR=str(temporary/'data'),
                   BRAINWIRE_SERVER_LISTEN=address, BRAINWIRE_SERVER_DEFAULT_DATABASE='private',
                   BRAINWIRE_MEMORY_CACHE='64MiB', BRAINWIRE_MEMORY_MEMTABLES='16MiB', BRAINWIRE_MEMORY_QUERY='64MiB')
        tokens = {name: secrets.token_hex(32) for name in ('reader','writer','admin','monitor')}
        client_files = {}
        for name, token in tokens.items():
            file = temporary/(name+'.json')
            file.write_text(json.dumps(dict(principal=name, credentials=token)))
            file.chmod(0o600); client_files[name] = str(file)
        policy = temporary/'policy.json'
        policy.write_text(json.dumps({'principals': [
            dict(id=name, token_sha256=hashlib.sha256(token.encode()).hexdigest(),
                 graphs={'private' if name=='monitor' else 'allowed': 'write' if name=='writer' else 'read'},
                 instance_admin=name=='admin') for name,token in tokens.items()]}))
        policy.chmod(0o600)
        env['BRAINWIRE_SERVER_AUTH_FILE'] = str(policy)
        env['BRAINWIRE_BOLT_AUTH_FILE'] = client_files['monitor']
        server = None
        log = (temporary/'server.log').open('w+')
        def cli(*args, check=True, credentials='monitor'):
            child_env = dict(env, BRAINWIRE_BOLT_AUTH_FILE=client_files[credentials])
            result = subprocess.run([str(binary),*args],cwd=ROOT,env=child_env,capture_output=True,text=True,timeout=30)
            if check and result.returncode:
                raise AssertionError(result.stderr)
            return result
        def stop(kill=False):
            nonlocal server
            if server is not None:
                server.kill() if kill else server.terminate()
                try: server.wait(timeout=5)
                except subprocess.TimeoutExpired: server.kill(); server.wait(timeout=5)
                server = None
        def start():
            nonlocal server
            server = subprocess.Popen([str(binary),'serve'],cwd=ROOT,env=env,stdout=log,stderr=log)
            for _ in range(100):
                if server.poll() is not None: raise AssertionError('protected server exited')
                if cli('probe','--address',address,'--mode','live','--timeout-ms','100',check=False).returncode==0: return
                time.sleep(.03)
            raise AssertionError('protected server readiness timeout')
        def denied(auth, database, query='RETURN 1', explicit=False, code='Forbidden'):
            with GraphDatabase.driver('bolt://'+address,auth=auth) as driver:
                try:
                    with driver.session(database=database) as session:
                        if explicit:
                            with session.begin_transaction() as tx: tx.run(query).consume()
                        else: session.run(query).consume()
                except Neo4jError as error:
                    assert error.code.endswith(code), error.code
                    assert all(token not in str(error) for token in tokens.values())
                else: raise AssertionError('denied request was accepted')
        try:
            cli('db','create','allowed','--ontology',str(ROOT/'examples/python/resumable_customers_ontology.json'))
            cli('db','create','private')
            cli('db','create','tickets','--ontology',str(ROOT/'examples/client/ontology.json'))
            cli('db','create','knowledge','--ontology',str(ROOT/'examples/knowledge/schema.json'))
            # Extend writer and reader grants for the separate snapshot and ranking graphs.
            mapping=json.loads(policy.read_text())
            for principal in mapping['principals']:
                if principal['id'] in ('writer','reader'):
                    for graph in ('tickets','knowledge'):
                        principal['graphs'][graph]='write' if principal['id']=='writer' else 'read'
            policy.write_text(json.dumps(mapping))
            # Bad policies fail before listener/store initialization; no fallback to dev.
            valid=policy.read_text()
            for malformed in ('', '{"principals":[]}', '{"principals":"secret-marker"}'):
                policy.write_text(malformed)
                result=cli('serve',check=False)
                assert result.returncode and 'secret-marker' not in result.stderr
                with socket.socket() as sock: assert sock.connect_ex(('127.0.0.1',int(address.split(':')[1])))!=0
            policy.write_text(valid)
            shown=cli('config','show').stdout
            assert 'server.auth_file' in shown and all(token not in shown for token in tokens.values())
            start()
            for auth in (None,('reader','f'*64),('missing',tokens['reader'])):
                denied(auth,'allowed',code='Unauthorized')
            for database in ('private','missing',None):
                for explicit in (False,True): denied(('reader',tokens['reader']),database,explicit=explicit)
            for query in ("UNWIND [1] AS x MERGE (c:Customer {entity_id:'denied'})", "CREATE (c:Customer {entity_id:'denied'})", "MATCH (c:Customer) SET c.active=false", "MATCH (c:Customer) DETACH DELETE c", "CALL db.memory() YIELD poolLimitBytes RETURN poolLimitBytes"):
                denied(('reader',tokens['reader']),'allowed',query)
            denied(('writer',tokens['writer']),'allowed',"CALL db.memory() YIELD poolLimitBytes RETURN poolLimitBytes")
            source=temporary/'customers.jsonl'
            source.write_text(''.join(json.dumps(dict(entity_id=f'customer-{i}',email=f'synthetic-{i}@example.test',active=True,tags=['synthetic'],embedding=[.1,.2]))+'\n' for i in range(1000)))
            with GraphDatabase.driver('bolt://'+address,auth=('writer',tokens['writer'])) as writer:
                assert ingest(writer,'allowed',source,'source')==source.stat().st_size
                # Verify both module and direct-script credential imports in public CLIs.
                for invocation in (
                    ['-m','examples.python.ingest_customers'],
                    [str(ROOT/'examples/python/ingest_customers.py')],
                    ['-m','examples.python.resumable_customers'],
                    [str(ROOT/'examples/python/resumable_customers.py')],
                ):
                    arguments=[sys.executable,*invocation,str(source),'--uri','bolt://'+address,'--database','allowed']
                    if 'resumable_customers' in invocation[-1]: arguments+=['--source-id','source']
                    result=subprocess.run(arguments,cwd=ROOT,env=dict(env,BRAINWIRE_BOLT_AUTH_FILE=client_files['writer']),capture_output=True,text=True,timeout=30)
                    assert result.returncode==0,result.stderr
                    assert all(token not in result.stdout+result.stderr for token in tokens.values())

                with writer.session(database='allowed') as session:
                    with session.begin_transaction() as tx:
                        tx.run("MERGE (c:Customer {entity_id:'overwrite'}) SET c.email='old@example.test'").consume()
                        tx.run("MATCH (c:Customer {entity_id:'overwrite'}) SET c.email='new@example.test'").consume()
                    result=session.run("RETURN coalesce(null,'normal',$missing),coalesce(false,size(42))").single()
                    assert list(result)==['normal',False]
                    tx=session.begin_transaction()
                    tx.run("CREATE (c:Customer {entity_id:'rollback-admin'})").consume()
                    try: tx.run("CALL db.memory() YIELD poolLimitBytes RETURN poolLimitBytes").consume()
                    except Neo4jError as error: assert error.code.endswith('Forbidden')
                    else: raise AssertionError('non-admin telemetry accepted')
                    with suppress(Exception): tx.close()
                with writer.session(database='knowledge') as session:
                    session.run("CREATE (s:KnowledgeSource {entity_id:'approved', term:'revenue', domain:'finance', authority:1, authorRelevance:1, certified:true, freshness:1}), (p:KnowledgeSource {entity_id:'popular', term:'revenue', domain:'finance', usage:1})").consume()
                # Exercise the authenticated public snapshot module CLI and shared credential loader.
                snapshot=temporary/'tickets.jsonl'
                snapshot.write_text(json.dumps(dict(record_type='ticket',tenant_id='synthetic',connector_id='z',external_id='1',subject=None,status='open',priority=None,tags=[]))+'\n'+json.dumps(dict(record_type='comment',tenant_id='synthetic',connector_id='z',external_id='c',ticket_id='1',body='synthetic',is_public=True))+'\n')
                result=subprocess.run([sys.executable,'-m','examples.client.ingest_snapshot',str(snapshot),'--uri','bolt://'+address,'--database','tickets','--source-id','tickets-source'],cwd=ROOT,env=dict(env,BRAINWIRE_BOLT_AUTH_FILE=client_files['writer']),capture_output=True,text=True,timeout=30)
                assert result.returncode==0,result.stderr
            with GraphDatabase.driver('bolt://'+address,auth=('reader',tokens['reader'])) as reader:
                with reader.session(database='allowed') as session:
                    assert session.run('MATCH (c:Customer) RETURN count(*) AS n').single()['n']==1001
                    assert session.run("MATCH (c:Customer {email:'new@example.test'}) RETURN count(*) AS n").single()['n']==1
                    assert session.run("MATCH (c:Customer {entity_id:'rollback-admin'}) RETURN count(*) AS n").single()['n']==0
                with reader.session(database='tickets') as session:
                    assert session.run('MATCH (t:Ticket)-[:HAS_COMMENT]->(c:TicketComment) RETURN count(*) AS n').single()['n']==1
                    projection=session.run("CALL graph.project({relationship:'HAS_COMMENT'}) YIELD nodeCount,relationshipCount RETURN nodeCount,relationshipCount").single(strict=True)
                    assert projection['nodeCount']==3 and projection['relationshipCount']==1
                    scores=list(session.run("CALL graph.pagerank({relationship:'HAS_COMMENT'}) YIELD node,score,converged RETURN node.entity_id AS id,score,converged"))
                    assert len(scores)==3 and all(item['converged'] for item in scores)
                    assert abs(sum(item['score'] for item in scores)-1)<1e-8
                    assert scores[0]['id']==identity('synthetic','z','comment','c')
                    options=dict(source=identity('synthetic','z','ticket','1'),sourceLabel='Ticket',relationship='HAS_COMMENT',maxDepth=2)
                    reachable=list(session.run('CALL graph.reachable($options) YIELD node,depth RETURN node.entity_id AS id,depth',options=options))
                    assert sorted(item['depth'] for item in reachable)==[0,1]
                    options.update(target=identity('synthetic','z','comment','c'),targetLabel='TicketComment')
                    path=list(session.run('CALL graph.shortestPath($options) YIELD node,depth RETURN node.entity_id AS id,depth',options=options))
                    assert [item['depth'] for item in path]==[0,1]
                    degrees=list(session.run("CALL graph.degree({relationship:'HAS_COMMENT'}) YIELD outDegree,inDegree RETURN outDegree,inDegree"))
                    assert sum(item['outDegree'] for item in degrees)==sum(item['inDegree'] for item in degrees)==1
                with reader.session(database='knowledge') as session:
                    ranked=list(session.run("CALL knowledge.rank({label:'KnowledgeSource',term:'revenue',domain:'finance'}) YIELD node,score,contributions RETURN node.entity_id AS id,score,contributions"))
                    assert [item['id'] for item in ranked]==['approved','popular']
                    assert abs(ranked[0]['score']-.9)<1e-8 and abs(ranked[1]['score']-.1)<1e-8
                    assert all(abs(sum(item['contributions'].values())-item['score'])<1e-8 for item in ranked)

            with GraphDatabase.driver('bolt://'+address,auth=('admin',tokens['admin'])) as admin:
                with admin.session(database='allowed') as session:
                    assert session.run('CALL db.memory() YIELD poolLimitBytes RETURN poolLimitBytes').single()['poolLimitBytes']==64<<20
            assert cli('probe','--address',address,'--mode','live',credentials='reader').returncode==0
            assert cli('probe','--address',address,'--mode','ready',credentials='reader',check=False).returncode!=0
            assert cli('probe','--address',address,'--mode','ready').returncode==0
            frame=cli('shell','--connect',address,'--graph','allowed','--view','ontology','--snapshot',credentials='reader').stdout
            assert 'Customer' in frame, frame
            (Path(__file__).parent/'ontology-authenticated.txt').write_text(frame)
            stop(kill=True); start()
            with GraphDatabase.driver('bolt://'+address,auth=('reader',tokens['reader'])) as reader:
                with reader.session(database='allowed') as session: assert session.run('MATCH (c:Customer) RETURN count(*) AS n').single()['n']==1001
            # Revocation is restart-based: old credentials fail after loading replacement verifier.
            stop(); mapping=json.loads(policy.read_text())
            for principal in mapping['principals']:
                if principal['id']=='reader': principal['token_sha256']=hashlib.sha256(secrets.token_hex(32).encode()).hexdigest()
            policy.write_text(json.dumps(mapping)); start()
            denied(('reader',tokens['reader']),'allowed',code='Unauthorized')
            stop()
            report['audit']=json.loads(cli('db','check','allowed','--json').stdout)
            assert report['audit']['status']=='ok'
            log.flush(); logged=(temporary/'server.log').read_text()
            assert all(token not in logged for token in tokens.values())
            report['checks']=['invalid policy fails before bind','diagnostic redaction','missing/wrong credentials','default/explicit graph grants','implicit and explicit transactions','readonly mutation denial','instance admin telemetry','1000-row indexed importer','authenticated module and direct-script Customer CLI connections','atomic multi-RUN overwrite and rollback','coalesce lazy evaluation','authenticated Python snapshot CLI','readonly sparse analytics and explainable ranking','native probe and connected ontology','SIGKILL retained data','restart token revocation','offline graph/index audit']
            report['result']='PASS'
            print(json.dumps(report,indent=2))
        finally:
            stop(); log.close()


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=Path,default=ROOT/'target/release/brainwire')
    run(parser.parse_args().binary.resolve())
