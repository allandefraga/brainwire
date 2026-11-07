"""Bounded protected mixed-load observation; temporary synthetic data only.

Run from root with the Neo4j Python driver. Four readers + one throttled writer;
latencies are end-to-end materialized Bolt requests, bucketed up to 1 microsecond.
"""
import argparse
import base64
from collections import Counter
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import random
import secrets
import socket
import subprocess
import sys
import tempfile
import threading
import time

from neo4j import GraphDatabase, __version__
ROOT=Path(__file__).resolve().parents[3]
sys.path.insert(0,str(ROOT))
from examples.python.resumable_customers import ingest


def percentiles(hist):
    total=sum(hist.values()); result={}
    if not total: return {'requests':0}
    for label,p in [('p50',.5),('p95',.95),('p99',.99)]:
        accumulated=0
        for microseconds,count in sorted(hist.items()):
            accumulated+=count
            if accumulated>=math.ceil(total*p): result[label+'_ms']=microseconds/1000; break
    return dict(requests=total,**result)


def run(binary,rows,seconds,hot_reads=False):
    if not 1000<=rows<=100000 or not 1<=seconds<=3600: raise ValueError('rows 1000..100000, seconds 1..3600')
    report=dict(driver=__version__,binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),host=platform.platform(),rows=rows,requested_seconds=seconds,
                synthetic=True,hot_point_reads=hot_reads,profile='release localhost, 4 readers and 1 writer, no Raft/history/search',limits=dict(cache_mib=64,memtables_mib=16,query_mib=64,message_mib=8,decoded_message_mib=16,io_timeout_ms=300000),latency_bucket_us=1)
    with tempfile.TemporaryDirectory(prefix='brainwire-auth-load-') as folder:
        path=Path(folder); source=path/'source.jsonl'; random_source=random.Random(123)
        with source.open('w') as stream:
            for i in range(rows):
                # Different deterministic entropy per node; 1024 ASCII chars in tags.
                payload=base64.b64encode(random_source.randbytes(768)).decode()
                stream.write(json.dumps(dict(entity_id=f'customer-{i}',email=f'synthetic-{i}@example.test',active=True,tags=[payload],embedding=[.25,.5]))+'\n')
        report['source_bytes']=source.stat().st_size; report['payload_ascii_characters_per_node']=1024
        tokens={name:secrets.token_hex(32) for name in ('reader','writer','observer')}
        policy=path/'policy.json'
        policy.write_text(json.dumps(dict(principals=[dict(id=name,token_sha256=hashlib.sha256(token.encode()).hexdigest(),graphs={'load':'write' if name=='writer' else 'read'},instance_admin=name=='observer') for name,token in tokens.items()])))
        policy.chmod(0o600)
        auth=path/'client.json'; auth.write_text(json.dumps(dict(principal='observer',credentials=tokens['observer']))); auth.chmod(0o600)
        with socket.socket() as sock: sock.bind(('127.0.0.1',0)); address='%s:%s'%sock.getsockname()
        env={k:v for k,v in os.environ.items() if not k.startswith('BRAINWIRE_')}
        env.update(BRAINWIRE_SERVER_DATA_DIR=str(path/'data'),BRAINWIRE_SERVER_LISTEN=address,BRAINWIRE_SERVER_DEFAULT_DATABASE='load',BRAINWIRE_SERVER_AUTH_FILE=str(policy),BRAINWIRE_BOLT_AUTH_FILE=str(auth),BRAINWIRE_MEMORY_CACHE='64MiB',BRAINWIRE_MEMORY_MEMTABLES='16MiB',BRAINWIRE_MEMORY_QUERY='64MiB',BRAINWIRE_SERVER_MAX_MESSAGE='8MiB',BRAINWIRE_SERVER_MAX_DECODED_MESSAGE='16MiB',BRAINWIRE_SERVER_IO_TIMEOUT_MS='300000')
        server=None; log=(path/'server.log').open('w'); stop_event=threading.Event(); threads=[]
        def cli(*args):
            return subprocess.run([str(binary),*args],cwd=ROOT,env=env,capture_output=True,text=True,check=True,timeout=60).stdout
        def start():
            nonlocal server
            server=subprocess.Popen([str(binary),'serve'],cwd=ROOT,env=env,stdout=log,stderr=log)
            for _ in range(200):
                if server.poll() is not None: raise AssertionError('server stopped')
                try: cli('probe','--address',address,'--timeout-ms','100'); return
                except subprocess.CalledProcessError: time.sleep(.03)
            raise AssertionError('server readiness timeout')
        def stop(kill=False):
            nonlocal server
            if server is not None:
                server.kill() if kill else server.terminate()
                try: server.wait(timeout=5)
                except subprocess.TimeoutExpired: server.kill(); server.wait(timeout=5)
                server=None
        results=[dict(point=Counter(),scalar=Counter(),writes=Counter(),latest={},error=None) for _ in range(5)]
        def client(index):
            result=results[index]; generator=random.Random(index)
            try:
                name='writer' if index==4 else 'reader'
                with GraphDatabase.driver('bolt://'+address,auth=(name,tokens[name])) as driver:
                    with driver.session(database='load') as session:
                        sequence=0
                        while not stop_event.is_set():
                            if index==4:
                                identifier=f'customer-{sequence%200}'; email=f'updated-{sequence}@example.test'
                                began=time.perf_counter()
                                session.run('MATCH (c:Customer {entity_id:$id}) SET c.email=$email',id=identifier,email=email).consume()
                                result['writes'][math.ceil((time.perf_counter()-began)*1e6)]+=1
                                result['latest'][identifier]=email; sequence+=1
                                stop_event.wait(.05)
                            else:
                                kind='point' if sequence%2==0 else 'scalar'
                                i=generator.randrange(200) if hot_reads and kind=='point' else generator.randrange(200,rows)
                                query='MATCH (c:Customer {entity_id:$id}) RETURN c.entity_id AS id,c.email AS email' if kind=='point' else 'MATCH (c:Customer {email:$email}) RETURN c.entity_id AS id,c.email AS email'
                                began=time.perf_counter()
                                record=session.run(query,id=f'customer-{i}',email=f'synthetic-{i}@example.test').single(strict=True)
                                assert record['id']==f'customer-{i}'
                                if hot_reads and kind=='point' and record['email']!=f'synthetic-{i}@example.test':
                                    email=record['email']
                                    assert isinstance(email,str) and email.startswith('updated-') and email.endswith('@example.test')
                                    assert int(email[len('updated-'):-len('@example.test')])%200==i
                                else:
                                    assert record['email']==f'synthetic-{i}@example.test'
                                result[kind][math.ceil((time.perf_counter()-began)*1e6)]+=1; sequence+=1
            except Exception as error:
                # Exceptions may contain driver text: retain type only, no credentials.
                result['error']=type(error).__name__; stop_event.set()
        try:
            cli('db','create','load','--ontology',str(ROOT/'examples/python/resumable_customers_ontology.json')); start()
            with GraphDatabase.driver('bolt://'+address,auth=('writer',tokens['writer'])) as driver:
                began=time.perf_counter(); assert ingest(driver,'load',source,'load-source')==source.stat().st_size
                report['import_seconds']=time.perf_counter()-began
                report['input_records_per_second']=rows/report['import_seconds']
            began=time.perf_counter()
            for i in range(5):
                thread=threading.Thread(target=client,args=(i,)); threads.append(thread); thread.start()
            observations=[]
            with GraphDatabase.driver('bolt://'+address,auth=('observer',tokens['observer'])) as observer:
                with observer.session(database='load') as session:
                    while time.perf_counter()-began<seconds and not stop_event.is_set():
                        rss=subprocess.run(['ps','-o','rss=','-p',str(server.pid)],check=True,capture_output=True,text=True).stdout.strip()
                        record=session.run('CALL db.memory() YIELD poolReservedBytes,poolPeakBytes,poolRejections RETURN poolReservedBytes,poolPeakBytes,poolRejections').single(strict=True)
                        observations.append(dict(seconds=round(time.perf_counter()-began,3),rss_kib=int(rss),**record.data()))
                        if len(observations) == 1 or len(observations) % 12 == 0:
                            print(json.dumps(dict(phase='load',elapsed_seconds=round(time.perf_counter()-began,1),rss_kib=int(rss),**record.data())),file=sys.stderr,flush=True)
                        stop_event.wait(min(5,max(.01,seconds-(time.perf_counter()-began))))
            stop_event.set()
            for thread in threads: thread.join(timeout=30); assert not thread.is_alive()
            report['load_seconds']=time.perf_counter()-began
            assert all(result['error'] is None for result in results),[result['error'] for result in results]
            for kind in ('point','scalar','writes'):
                hist=Counter()
                for result in results: hist.update(result[kind])
                report[kind]=percentiles(hist)
            report['sampled_rss_peak_kib']=max(item['rss_kib'] for item in observations)
            report['observations']=observations
            latest=results[4]['latest']; assert latest
            stop(kill=True); start()
            with GraphDatabase.driver('bolt://'+address,auth=('reader',tokens['reader'])) as driver:
                with driver.session(database='load') as session:
                    assert session.run('MATCH (c:Customer) RETURN count(*) AS n').single()['n']==rows
                    for identifier,email in latest.items():
                        record=session.run('MATCH (c:Customer {entity_id:$id}) RETURN c.email AS email',id=identifier).single(strict=True)
                        assert record['email']==email
                        assert session.run('MATCH (c:Customer {email:$email}) RETURN count(*) AS n',email=email).single()['n']==1
            stop(); report['audit']=json.loads(cli('db','check','load','--json')); assert report['audit']['status']=='ok'
            report['acknowledged_latest_updates_verified']=len(latest); report['result']='PASS'
            print(json.dumps(report,indent=2))
        finally:
            stop_event.set()
            for thread in threads: thread.join(timeout=30)
            stop(); log.close()


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=Path,default=ROOT/'target/release/brainwire')
    parser.add_argument('--rows',type=int,default=100000)
    parser.add_argument('--seconds',type=int,default=900)
    parser.add_argument('--hot-reads',action='store_true',help='Point reads overlap the 200 records updated by the writer; scalar reads remain disjoint')
    args=parser.parse_args(); run(args.binary.resolve(),args.rows,args.seconds,args.hot_reads)
