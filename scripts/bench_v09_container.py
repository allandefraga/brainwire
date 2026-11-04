"""Opt-in four-label mixed-load diagnostic; see bench_v09_container.md."""
import argparse
from collections import Counter
import hashlib
import json
import math
from pathlib import Path
import secrets
import subprocess
import sys
import threading
import time
import uuid

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
from scripts import bench_v08_container as base

LABELS = {'Usuario': 'usr', 'Carro': 'car', 'Endereco': 'end', 'Casa': 'cas'}
EDGES = [('TEM_CARRO', 'Usuario', 'Carro'), ('TEM_ENDERECO', 'Usuario', 'Endereco'),
         ('TEM_CASA', 'Usuario', 'Casa'), ('CASA_FICA_EM', 'Casa', 'Endereco')]
PROPS = {
    'Usuario': ['usuario_id', 'cpf_teste', 'nome_teste', 'email_teste', 'updated_at'],
    'Carro': ['carro_id', 'usuario_id', 'codigo_teste', 'marca_teste', 'ano', 'updated_at'],
    'Endereco': ['endereco_id', 'usuario_id', 'logradouro_teste', 'numero_teste', 'bairro_teste', 'cidade_teste', 'uf_teste'],
    'Casa': ['casa_id', 'usuario_id', 'endereco_id', 'codigo_teste', 'tipo_teste', 'area_m2', 'updated_at']}
INDEXES = {'Usuario': ['cpf_teste', 'email_teste'], 'Carro': ['marca_teste', 'ano'],
           'Endereco': ['cidade_teste', 'uf_teste'], 'Casa': ['tipo_teste', 'area_m2']}
PRINCIPAL = 'user:benchmark:reader'
VISIBILITY = {'principals': [PRINCIPAL]}
MEMORY_CODES = {'Neo.TransientError.General.QueryMemoryPoolExhausted',
                'Neo.ClientError.Statement.MemoryLimitExceeded'}


def identifier(label, i):
    return f'{LABELS[label]}-{i:08d}'



def auth_policy(writer_token, reader_token, delegated):
    writer=dict(id='bench',token_sha256=hashlib.sha256(writer_token.encode()).hexdigest(),
                graphs={'bench':'write'},instance_admin=True)
    reader=dict(id='bench-read',token_sha256=hashlib.sha256(reader_token.encode()).hexdigest(),
                graphs={'bench':'read'},instance_admin=False,visibility_delegate=delegated)
    return {'principals':[writer,reader]}


def connect_drivers(factory,name,writer_token,reader_token,writers):
    inspect=json.loads(base.docker('inspect',name))[0]
    port=inspect['NetworkSettings']['Ports']['7687/tcp'][0]['HostPort']
    uri='bolt://127.0.0.1:'+port
    options=dict(connection_timeout=10,connection_acquisition_timeout=30)
    writer=factory(uri,auth=('bench',writer_token),max_connection_pool_size=writers+2,**options)
    try:
        reader=factory(uri,auth=('bench-read',reader_token),max_connection_pool_size=6,**options)
    except Exception:
        writer.close();raise
    return writer,reader,uri


def row(label, i):
    value = {'id': identifier(label, i), 'usuario_id': i}
    if label == 'Usuario':
        value.update(cpf_teste=f'{i:011d}', nome_teste=f'Nome {i}', email_teste=f'usuario{i}@example.test')
    elif label == 'Carro':
        value.update(carro_id=i, codigo_teste=f'CAR{i}', marca_teste=['MarcaA','MarcaB','MarcaC','MarcaD'][i % 4], ano=2000+i % 25)
    elif label == 'Endereco':
        value.update(endereco_id=i, logradouro_teste=f'Rua {i}', numero_teste=i % 1000,
                     bairro_teste=f'Bairro {i % 500}', cidade_teste=f'Cidade {i % 100}', uf_teste=['SP','RJ','MG','RS','PR','BA'][i % 6])
    else:
        value.update(casa_id=i, endereco_id=i, codigo_teste=f'CASA{i}', tipo_teste=['casa','apto','sobrado'][i % 3], area_m2=40+i % 300)
    if 'updated_at' in PROPS[label]:
        value['updated_at'] = '2026-10-01T00:00:00Z'
    return value


def batches(rows, size, worker, workers):
    for start in range(worker * size, rows, workers * size):
        yield range(start, min(start + size, rows))


def ontology():
    return {'version': '1', 'nodes': [dict(label=label, properties=PROPS[label], indexed_fields=INDEXES[label]) for label in LABELS],
            'edges': [dict(type=kind, **{'from': {'label': source}, 'to': {'label': target}}, properties=[]) for kind, source, target in EDGES]}


def node_query(label):
    return f'UNWIND $rows AS r MERGE (n:{label} {{entity_id:r.id}}) SET ' + ','.join(f'n.{p}=r.{p}' for p in PROPS[label])


def edge_query(kind, source, target):
    return f'UNWIND $rows AS r MATCH (a:{source} {{entity_id:r.s}}),(b:{target} {{entity_id:r.t}}) MERGE (a)-[:{kind}]->(b)'


def reader_query(role, after, width, page):
    if role == 'scan':
        return 'MATCH (u:Usuario) RETURN u.usuario_id AS id', {}
    if role == 'aggregation':
        return 'MATCH (:Usuario)-[:TEM_CARRO]->(c:Carro) RETURN c.marca_teste AS brand,count(*) AS count ORDER BY brand', {}
    if role == 'neighborhood':
        return ('MATCH (u:Usuario)-[:TEM_CARRO]->(c:Carro) WHERE u.usuario_id < $width RETURN u,c', {'width': width})
    return (f'MATCH (u:Usuario) WHERE u.entity_id > $after RETURN u.entity_id AS id ORDER BY u.entity_id LIMIT {page}', {'after': after})



def verify_reader_rows(role, values, after, rows, width):
    if role == 'scan':
        ids=[value['id'] for value in values]
        if len(ids)!=len(set(ids)) or any(type(i) is not int or not 0<=i<rows for i in ids):
            raise AssertionError('scan identity mismatch')
    elif role == 'aggregation':
        brands=[value['brand'] for value in values]
        if len(brands)!=len(set(brands)) or any(brand not in ['MarcaA','MarcaB','MarcaC','MarcaD'] for brand in brands):
            raise AssertionError('aggregation brand mismatch')
        if any(type(v['count']) is not int or not 0<v['count']<=rows for v in values):
            raise AssertionError('aggregation count mismatch')
    elif role == 'neighborhood':
        seen=set()
        for value in values:
            user,car=value['u'],value['c'];i=user['usuario_id']
            if type(i) is not int or not 0<=i<min(width,rows) or i in seen:
                raise AssertionError('neighborhood identity mismatch')
            seen.add(i)
            if user['entity_id']!=identifier('Usuario',i) or car['entity_id']!=identifier('Carro',i) or car['usuario_id']!=i:
                raise AssertionError('neighborhood relationship mismatch')
    else:
        ids=[value['id'] for value in values]
        if ids!=sorted(set(ids)) or any(value<=after or not value.startswith('usr-') or not value[4:].isdigit() or not 0<=int(value[4:])<rows for value in ids):
            raise AssertionError('cursor ordering or identity mismatch')



def execute_query(session,text,params,timeout,visibility=None):
    options={'timeout':timeout}
    if visibility is not None:options['metadata']={'brainwire_visibility':visibility}
    with session.begin_transaction(**options) as tx:
        result=tx.run(text,{} if params is None else params).data()
        tx.commit()
        return result


def read_operation(session, role, after, args):
    text,params=reader_query(role,after,args.neighborhood_rows,args.page_size)
    metadata={'brainwire_visibility':VISIBILITY} if args.stamp_acl else {}
    with session.begin_transaction(timeout=args.query_timeout,metadata=metadata) as tx:
        values=tx.run(text,params).data()
        verify_reader_rows(role,values,after,args.rows,args.neighborhood_rows)
        # Verify cardinality in the same snapshot; no racing second transaction.
        checks={'scan':'MATCH (u:Usuario) RETURN count(u) AS count',
                'aggregation':'MATCH (:Usuario)-[:TEM_CARRO]->(:Carro) RETURN count(*) AS count',
                'neighborhood':'MATCH (u:Usuario)-[:TEM_CARRO]->(c:Carro) WHERE u.usuario_id < $width RETURN count(*) AS count'}
        if role in checks:
            expected=tx.run(checks[role],params).data()[0]['count']
            actual=sum(v['count'] for v in values) if role=='aggregation' else len(values)
            if actual!=expected:raise AssertionError('same-snapshot reader cardinality mismatch')
        tx.commit()
    return values


def validate_sample(sample, limit):
    if sample['max_bytes'] != limit:
        raise AssertionError('wrong cgroup memory.max')
    if sample['events'].get('oom', 0) or sample['events'].get('oom_kill', 0):
        raise AssertionError('cgroup OOM event')
    if sample['current_bytes'] > limit or sample['peak_bytes'] > limit:
        raise AssertionError('cgroup exceeded memory cap')


def host_environment():
    value = base.host_receipt()
    try:
        result = subprocess.run(['pmset', '-g', 'therm'], capture_output=True, text=True, timeout=5)
        value['thermal'] = {'available': result.returncode == 0, 'receipt': result.stdout if result.returncode == 0 else None}
    except (OSError, subprocess.TimeoutExpired):
        value['thermal'] = {'available': False}
    return value


def inspect_state(name):
    value = json.loads(base.docker('inspect', name))[0]
    return {'state': value['State'], 'restart_count': value.get('RestartCount'), 'image': value['Image']}


def oom_evidence(samples, state, events):
    cgroup = any(s['events'].get('oom', 0) or s['events'].get('oom_kill', 0) for s in samples)
    docker_oom = any(e.get('Action') == 'oom' or e.get('status') == 'oom' for e in events)
    return {'observed': cgroup or docker_oom or state.get('state', {}).get('OOMKilled', False),
            'cgroup_oom': cgroup, 'docker_oom_event': docker_oom,
            'docker_oom_killed': state.get('state', {}).get('OOMKilled'),
            'restart_count': state.get('restart_count'),
            'kernel_log_verified': False}


def run(args):
    from neo4j import GraphDatabase, __version__
    limit = args.memory_gib * 1024**3
    output = args.output.resolve()
    output.mkdir(mode=0o700)
    run_id = uuid.uuid4().hex
    name, volume = 'brainwire-v09-' + run_id, 'brainwire-v09-data-' + run_id
    report = dict(status='failed', synthetic=True, run_id=run_id, image=args.image, rows_per_label=args.rows,
                  expected_nodes=4*args.rows, expected_edges=4*args.rows, memory_limit_bytes=limit,
                  writers=args.writers, readers=['scan','aggregation','neighborhood','cursor'], driver=__version__,
                  workload_settings={'batch_size': args.batch_size, 'neighborhood_rows': args.neighborhood_rows,
                                     'sample_seconds': args.sample_seconds, 'reader_pause': args.reader_pause,
                                     'query_timeout': args.query_timeout},
                  host_start=host_environment(), samples=[], client_results=[],
                  gaps=['Kernel logs may be inaccessible; never infer no OOM from Docker OOMKilled=false.',
                        'No disk loss, power-loss, quorum or partition simulation.',
                        'Process RSS is sampled; cgroup memory.peak includes page cache.',
                        'Atomic four-label batches differ from the older embedded sequential-label loading order.',
                        'Concurrent cursor pages are separate transactions; final quiescent cursor is verified exactly.'])
    if not report['host_start'].get('power_qualified'):
        report['error_type'] = 'HostPowerNotQualified'
        (output/'report.json').write_text(json.dumps(report, indent=2)+'\n')
        return 1
    policy, schema = output/'policy.json', output/'ontology.json'
    token,reader_token = secrets.token_hex(32),secrets.token_hex(32)
    policy.write_text(json.dumps(auth_policy(token,reader_token,args.stamp_acl)))
    schema.write_text(json.dumps(ontology()))
    policy.chmod(0o444); schema.chmod(0o444)
    common = ['--mount', f'type=volume,src={volume},dst=/data', '--mount', f'type=bind,src={policy},dst=/bench-policy.json,readonly',
              '--mount', f'type=bind,src={schema},dst=/bench-ontology.json,readonly',
              '-e','BRAINWIRE_SERVER_DATA_DIR=/data','-e','BRAINWIRE_SERVER_DEFAULT_DATABASE=bench',
              '-e','BRAINWIRE_SERVER_AUTH_FILE=/bench-policy.json','-e','BRAINWIRE_SERVER_LISTEN=0.0.0.0:7687']
    cap = ['--memory',str(limit),'--memory-swap',str(limit)]
    stop = threading.Event(); threads=[]; driver=None; read_driver=None; event_process=None; event_file=None
    lock=threading.Lock(); started=time.monotonic(); phase='setup'
    def sample():
        try:
            item = base.parse_sample(base.docker('exec',name,'sh','-c',base.CGROUP,timeout=10))
            item.update(phase=phase, monotonic_seconds=time.monotonic()-started)
            report['samples'].append(item)
            validate_sample(item,limit)
        except Exception:
            stop.set(); raise
    def query(session,text,params=None,read=False):
        visibility=VISIBILITY if read and args.stamp_acl else None
        return execute_query(session,text,params,args.query_timeout,visibility)
    def reconnect():
        nonlocal driver,read_driver
        if driver is not None:driver.close()
        if read_driver is not None:read_driver.close()
        driver,read_driver,uri=connect_drivers(GraphDatabase.driver,name,token,reader_token,args.writers)
        report.setdefault('endpoints',[]).append(uri)
    def ready():
        until=time.monotonic()+120
        while time.monotonic()<until:
            try:
                with driver.session(database='bench') as session: query(session,'RETURN 1 AS ready')
                return
            except Exception:
                if not inspect_state(name)['state']['Running']: raise RuntimeError('server exited during readiness')
                time.sleep(.5)
        raise TimeoutError('readiness')
    def writer(index):
        result=dict(role='writer',index=index,ack_rows_per_label=0,latency=Counter())
        with lock: report['client_results'].append(result)
        try:
            with driver.session(database='bench') as session:
                for indices in batches(args.rows,args.batch_size,index,args.writers):
                    if stop.is_set(): break
                    began=time.monotonic()
                    with session.begin_transaction(timeout=args.query_timeout) as tx:
                        for label in LABELS: tx.run(node_query(label),rows=[row(label,i) for i in indices]).consume()
                        for kind,source,target in EDGES:
                            tx.run(edge_query(kind,source,target),rows=[{'s':identifier(source,i),'t':identifier(target,i)} for i in indices]).consume()
                        if args.stamp_acl:
                            acl=[dict(label=label,entityId=identifier(label,i),allow=[PRINCIPAL],deny=[],connectorIds=[],sources=['synthetic-v09']) for label in LABELS for i in indices]
                            updated=tx.run('CALL db.setNodeAcls($rows) YIELD updated RETURN updated',rows=acl).data()
                            if updated != [{'updated':len(acl)}]: raise AssertionError('ACL acknowledgement mismatch')
                        tx.commit()
                    result['ack_rows_per_label']+=len(indices)
                    result['latency'][math.ceil((time.monotonic()-began)*1e6)]+=1
        except Exception as error:
            result['error_type']=type(error).__name__; result['error_code']=getattr(error,'code',None); stop.set()
    def reader(role):
        result=dict(role=role,latency=Counter(),denials=Counter(),rows=0)
        with lock: report['client_results'].append(result)
        after=''
        try:
            with read_driver.session(database='bench') as session:
                while not stop.is_set():
                    began=time.monotonic()
                    try:
                        values=read_operation(session,role,after,args)
                        result['rows']+=len(values)
                        result['latency'][math.ceil((time.monotonic()-began)*1e6)]+=1
                        if role=='cursor':
                            ids=[v['id'] for v in values]
                            after=ids[-1] if ids else ''
                    except Exception as error:
                        code=getattr(error,'code',None)
                        if code not in MEMORY_CODES: raise
                        result['denials'][code]+=1
                    stop.wait(args.reader_pause)
        except Exception as error:
            result['error_type']=type(error).__name__; result['error_code']=getattr(error,'code',None); stop.set()
    try:
        report['image_identity']=base.docker_image_revision(args.image)
        base.docker('volume','create','--label',f'{base.LABEL}={run_id}',volume)
        base.docker('run','--pull=never','--rm','--name',name+'-offline','--label',f'{base.LABEL}={run_id}',*cap,*common,args.image,'db','create','bench','--ontology','/bench-ontology.json',timeout=120)
        base.docker('run','--pull=never','-d','--name',name,'--label',f'{base.LABEL}={run_id}',*cap,'-p','127.0.0.1::7687',*common,args.image,'serve')
        report['initial_state']=inspect_state(name)
        event_file=(output/'docker-events.jsonl').open('w')
        event_process=subprocess.Popen(['docker','events','--filter',f'container={name}','--format','{{json .}}'],stdout=event_file,stderr=subprocess.DEVNULL)
        reconnect(); ready(); sample()
        try:
            report['kernel_probe']={'available':True,'log':base.docker('exec',name,'dmesg',timeout=5)}
        except (RuntimeError,subprocess.TimeoutExpired):
            report['kernel_probe']={'available':False,'reason':'unprivileged container cannot read Docker host kernel log'}
        phase='mixed-load'; load_started=time.monotonic()
        for role in ('scan','aggregation','neighborhood','cursor'):
            t=threading.Thread(target=reader,args=(role,),daemon=True);threads.append(t);t.start()
        writers=[]
        for index in range(args.writers):
            t=threading.Thread(target=writer,args=(index,),daemon=True);threads.append(t);writers.append(t);t.start()
        while any(t.is_alive() for t in writers):
            sample()
            if stop.is_set() or time.monotonic()-load_started>args.max_load_seconds: stop.set();break
            stop.wait(args.sample_seconds)
        for t in writers: t.join(timeout=5)
        if any(t.is_alive() for t in writers) or stop.is_set(): raise AssertionError('load failed or deadline exceeded')
        report['load_seconds']=time.monotonic()-load_started
        ack=sum(r.get('ack_rows_per_label',0) for r in report['client_results'])
        if ack!=args.rows: raise AssertionError('acknowledged row count mismatch')
        report['ack_rows_per_label']=ack
        phase='read-soak'; soak_started=time.monotonic(); until=soak_started+args.soak_seconds
        while time.monotonic()<until:
            sample()
            if stop.is_set(): raise AssertionError('read soak failed')
            stop.wait(min(args.sample_seconds,max(0,until-time.monotonic())))
        report['read_soak_seconds']=time.monotonic()-soak_started
        stop.set()
        for t in threads: t.join(timeout=5)
        if any(t.is_alive() for t in threads): raise TimeoutError('client threads still active')
        phase='post-load-idle'; base.idle_samples(lambda _:sample(),phase,args.idle_seconds,args.sample_seconds)
        phase='verification'
        with read_driver.session(database='bench') as session:
            counts={}
            for label in LABELS:
                values,seconds=base.measure_query(lambda s,t,**p:query(s,t,p,read=True),session,f'MATCH (n:{label}) RETURN count(n) AS count')
                counts[label]=values[0]['count'];report.setdefault('timings_seconds',{})['count_'+label]=seconds
            for kind,source,target in EDGES:
                counts[kind]=query(session,f'MATCH (:{source})-[:{kind}]->(:{target}) RETURN count(*) AS count',read=True)[0]['count']
            if any(value!=args.rows for value in counts.values()): raise AssertionError('exact graph counts mismatch')
            report['counts']=counts
            values,seconds=base.measure_query(lambda s,t,**p:query(s,t,p,read=True),session,'MATCH (n) RETURN n.entity_id AS id LIMIT 37')
            if len(values)!=37: raise AssertionError('unlabelled limit mismatch')
            report['timings_seconds']['unlabelled_limit']=seconds
            after='';seen=0;began=time.monotonic()
            while True:
                if time.monotonic()-began>args.cursor_seconds: raise TimeoutError('cursor deadline')
                text,params=reader_query('cursor',after,args.neighborhood_rows,args.page_size)
                values=query(session,text,params,read=True)
                expected=[{'id':identifier('Usuario',i)} for i in range(seen,min(seen+args.page_size,args.rows))]
                if values!=expected: raise AssertionError('exact cursor mismatch')
                seen+=len(values)
                if not values:break
                after=values[-1]['id']
            if seen!=args.rows: raise AssertionError('cursor missing rows')
            report['cursor']={'rows':seen,'page_size':args.page_size,'seconds':time.monotonic()-began,'delegated':args.stamp_acl}
        phase='clean-stop';base.docker('stop','--time','30',name)
        phase='offline-check'
        report['integrity']=json.loads(base.docker('run','--pull=never','--rm','--name',name+'-offline','--label',f'{base.LABEL}={run_id}',*cap,*common,args.image,'db','check','bench','--json',timeout=300))
        if report['integrity'].get('status')!='ok':raise AssertionError('integrity check failed')
        if any('error_type' in r for r in report['client_results']):raise AssertionError('client failure')
        if any(not r['latency'] and not r.get('denials') for r in report['client_results']):raise AssertionError('client performed no operation')
        report['status']='passed-diagnostic'
    except Exception as error:
        report['failure_stage']=phase;report['error_type']=type(error).__name__
    finally:
        stop.set()
        for t in threads:t.join(timeout=1)
        if not any(t.is_alive() for t in threads):
            if driver is not None:driver.close()
            if read_driver is not None:read_driver.close()
        if event_process is not None:
            event_process.terminate()
            try:event_process.wait(timeout=5)
            except subprocess.TimeoutExpired:event_process.kill();event_process.wait()
        if event_file is not None:event_file.close()
        events=[]
        if (output/'docker-events.jsonl').exists():
            for line in (output/'docker-events.jsonl').read_text().splitlines():
                try:events.append(json.loads(line))
                except ValueError:pass
        try:
            report['final_state']=inspect_state(name);(output/'server.log').write_text(base.docker('logs',name))
        except (RuntimeError,subprocess.TimeoutExpired):pass
        report['oom_evidence']=oom_evidence(report['samples'],report.get('final_state',{}),events)
        if report['oom_evidence']['observed']:report['status']='failed'
        report['memory_by_phase']=base.summarize_memory(report['samples'])
        report['host_end']=host_environment()
        if not report['host_end'].get('power_qualified'):
            report['status']='failed';report['host_power_changed']=True
        try:base.owned_cleanup(name,volume,run_id)
        except Exception as error:report['cleanup_error_type']=type(error).__name__;report['status']='failed'
        for t in threads:t.join(timeout=5)
        if any(t.is_alive() for t in threads):
            report['status']='failed';report['client_threads_alive']=True
        for result in report['client_results']:
            result['latency']=base.percentile(result['latency']);result['denials']=dict(result.get('denials',{}))
        policy.unlink(missing_ok=True)
        (output/'report.json').write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps({'status':report['status'],'report':str(output/'report.json')}))
    return 0 if report['status']=='passed-diagnostic' else 1


def parser():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--image',required=True);p.add_argument('--output',type=Path,required=True)
    p.add_argument('--memory-gib',type=int,choices=(2,4,8),required=True)
    p.add_argument('--rows',type=int,default=1_000_000);p.add_argument('--batch-size',type=int,default=250)
    p.add_argument('--writers',type=int,default=8);p.add_argument('--query-timeout',type=float,default=30)
    p.add_argument('--max-load-seconds',type=float,default=3600);p.add_argument('--soak-seconds',type=float,default=1800)
    p.add_argument('--idle-seconds',type=float,default=60);p.add_argument('--sample-seconds',type=float,default=1)
    p.add_argument('--reader-pause',type=float,default=.01);p.add_argument('--page-size',type=int,default=200)
    p.add_argument('--neighborhood-rows',type=int,default=100_000);p.add_argument('--cursor-seconds',type=float,default=600)
    p.add_argument('--stamp-acl',action='store_true',help='Stamp all nodes visible to a synthetic principal; reader uses delegated visibility')
    return p


def validate_args(a):
    if not 100<=a.rows<=10_000_000 or not 1<=a.batch_size<=1000 or not 1<=a.writers<=64 or a.rows< a.writers*a.batch_size:
        raise ValueError('invalid rows/batch/writers')
    if not 1<=a.page_size<=10_000 or not 1<=a.neighborhood_rows<=a.rows:raise ValueError('invalid reader size')
    if any(not math.isfinite(v) or v<=0 for v in (a.query_timeout,a.max_load_seconds,a.soak_seconds,a.idle_seconds,a.sample_seconds,a.reader_pause,a.cursor_seconds)):
        raise ValueError('timings must be positive and finite')


if __name__=='__main__':
    p=parser();a=p.parse_args()
    try:validate_args(a)
    except ValueError as error:p.error(str(error))
    raise SystemExit(run(a))
