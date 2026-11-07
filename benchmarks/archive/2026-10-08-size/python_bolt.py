import json, os, pathlib, socket, subprocess, tempfile, time
from neo4j import GraphDatabase
root=pathlib.Path(__file__).resolve().parents[3]
binary=root/'target/debug/brainwire'
with tempfile.TemporaryDirectory(prefix='brainwire-size-bolt-') as tmp:
    with socket.socket() as sock:
        sock.bind(('127.0.0.1',0)); address='%s:%s'%sock.getsockname()
    env={k:v for k,v in os.environ.items() if not k.startswith('BRAINWIRE_')}
    env.update(BRAINWIRE_SERVER_LISTEN=address,BRAINWIRE_SERVER_DATA_DIR=tmp+'/data',BRAINWIRE_SERVER_DEFAULT_DATABASE='size_test',BRAINWIRE_MEMORY_CACHE='64MiB',BRAINWIRE_MEMORY_MEMTABLES='16MiB')
    subprocess.run([str(binary),'db','create','size_test','--ontology',str(root/'crates/core/tests/fixtures/graph_mapping.json')],env=env,cwd=root,check=True,capture_output=True)
    with open(tmp+'/server.log','w+') as log:
        server=subprocess.Popen([str(binary),'serve'],env=env,cwd=root,stdout=log,stderr=log)
        try:
            for _ in range(100):
                probe=subprocess.run([str(binary),'probe','--address',address,'--timeout-ms','100'],env=env,cwd=root,capture_output=True)
                if probe.returncode==0: break
                assert server.poll() is None
                time.sleep(.03)
            else: raise RuntimeError('server readiness failed')
            with GraphDatabase.driver('bolt://'+address,auth=None) as driver, driver.session(database='size_test') as session:
                row=session.run('RETURN size($text) AS characters, size($items) AS items',text='é🧠',items=[1,None,3]).single(strict=True).data()
                assert row=={'characters':2,'items':3},row
                session.run("CREATE (c:Customer {entity_id:'python-size',name:'é🧠'}) SET c.length=size(c.name)").consume()
                persisted=session.run("MATCH (c:Customer {entity_id:'python-size'}) RETURN c.length AS length, size(c.name) AS characters").single(strict=True).data()
                assert persisted=={'length':2,'characters':2},persisted
                print(json.dumps({'status':'PASS','interface':'Python/Bolt','result':row,'persisted':persisted,'scope':'disposable local server; std size/list/string; no ANN'}))
        finally:
            server.terminate()
            try: server.wait(timeout=5)
            except subprocess.TimeoutExpired: server.kill();server.wait(timeout=5)
