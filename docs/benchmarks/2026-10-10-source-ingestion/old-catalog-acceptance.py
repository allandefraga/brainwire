import hashlib,json,os,socket,subprocess,tempfile,time
from pathlib import Path
from neo4j import GraphDatabase
from examples.python.resumable_customers import ingest
root=Path.cwd(); binary=root/'target/release/brainwire'
with tempfile.TemporaryDirectory(prefix='brainwire-old-checkpoint-') as folder:
    path=Path(folder); source=path/'source.jsonl'
    source.write_text(json.dumps(dict(entity_id='legacy-customer',email='synthetic@example.test',active=True,tags=[],embedding=[]))+'\n')
    ontology=json.loads((root/'examples/python/resumable_customers_ontology.json').read_text())
    ontology['nodes'][1]['properties'].remove('contract_id')
    schema=path/'old-ontology.json'; schema.write_text(json.dumps(ontology))
    with socket.socket() as s:
        s.bind(('127.0.0.1',0)); address='%s:%s'%s.getsockname()
    env={k:v for k,v in os.environ.items() if not k.startswith('BRAINWIRE_')}
    env.update(BRAINWIRE_SERVER_DATA_DIR=str(path/'data'),BRAINWIRE_SERVER_DEFAULT_DATABASE='legacy',BRAINWIRE_SERVER_LISTEN=address,BRAINWIRE_MEMORY_CACHE='64MiB',BRAINWIRE_MEMORY_MEMTABLES='16MiB',BRAINWIRE_MEMORY_QUERY='64MiB')
    def cli(*args):
        return subprocess.run([str(binary),*args],env=env,check=True,capture_output=True,text=True,timeout=30).stdout
    cli('db','create','legacy','--ontology',str(schema))
    with (path/'server.log').open('w') as log:
        server=subprocess.Popen([str(binary),'serve'],env=env,stdout=log,stderr=log)
        try:
            for _ in range(100):
                try:
                    cli('probe','--address',address,'--timeout-ms','100'); break
                except subprocess.CalledProcessError:
                    if server.poll() is not None: raise RuntimeError('server exited')
                    time.sleep(.03)
            with GraphDatabase.driver('bolt://'+address,auth=None) as driver:
                with driver.session(database='legacy') as session:
                    session.run('CREATE (s:ImportCheckpoint {entity_id:$id,sha256:$sha,next_offset:0})',id='old-source',sha=hashlib.sha256(source.read_bytes()).hexdigest()).consume()
                assert ingest(driver,'legacy',source,'old-source')==source.stat().st_size
                assert ingest(driver,'legacy',source,'old-source')==source.stat().st_size
                with driver.session(database='legacy') as session:
                    assert session.run('MATCH (c:Customer) RETURN count(*) AS n').single()['n']==1
                assert ingest(driver,'legacy',source,'new-default-source')==source.stat().st_size
        finally:
            server.terminate()
            try: server.wait(timeout=5)
            except subprocess.TimeoutExpired: server.kill(); server.wait(timeout=5)
    audit=json.loads(cli('db','check','legacy','--json')); assert audit['status']=='ok'
    print(json.dumps(dict(result='PASS',scope='actual pre-contract Customer ontology and checkpoint via Bolt',old_catalog_without_contract_id=True,existing_checkpoint_resume=True,completed_replay=True,new_default_source=True,audit=audit,temporary_data_removed_on_exit=True,test_server_stopped=True)))
