"""Verify this local synthetic candidate's receipts; not production certification."""
import copy
import hashlib
import json
from pathlib import Path
import sys

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]


def require(condition, reason):
    if not condition:
        raise ValueError(reason)


def validate(candidate, receipts):
    digest = candidate['nativeSha256']
    image = candidate['imageId']
    checks = receipts['checks']
    require(checks['workspaceExit'] == checks['clippyExit'] == checks['fmtExit'] == 0, 'Rust checks')
    require(checks['rustTestsPassed'] > 580 and checks['rustTestsIgnored'] == 0, 'Rust coverage')
    require(checks['pythonUnitExit'] == 0 and checks['pythonUnits'] == 21, 'Python unit checks')
    require(checks['nativeSha256'] == digest, 'test summary candidate')
    api = receipts['python-acl-tui']
    require(api['status'] == 'PASS' and api['serverSha256'] == digest and api['nodes'] == 1000, 'API candidate')
    require('private ACL checkpoint restore' in api['checks'], 'ACL restore coverage')
    lake = receipts['ducklake']
    require(lake['exitCode'] == 0 and lake['testsPassed'] == 11 and lake['skipped'] == 0 and lake['binarySha256'] == digest, 'DuckLake candidate')
    for name in ('compose-auth', 'swarm', 'kubernetes'):
        item = receipts[name]
        require(item['result'] == 'PASS' and item['imageId'] == image and item['rows'] == 1000, name + ' candidate')
        require(item['ownedResourcesRemoved'] is True, name + ' cleanup')
        require(item['audit']['status'] == 'ok', name + ' audit')
    require(receipts['swarm']['hostSwarmUnchanged'] is True, 'host Swarm changed')
    require(receipts['swarm']['restore_audit']['status'] == 'ok', 'Swarm checkpoint restore')
    require(receipts['kubernetes']['checkpointAudit']['status'] == 'ok', 'Kubernetes checkpoint restore')
    load = receipts['mixed-100000-3600']
    require(load['result'] == 'PASS' and load['binary_sha256'] == digest, 'load candidate')
    require(load['rows'] == 100000 and load['requested_seconds'] == 3600 and load['load_seconds'] >= 3600, 'full hour missing')
    require(load['synthetic'] is True and load['hot_point_reads'] is True, 'workload profile')
    require(load['audit']['status'] == 'ok' and load['acknowledged_latest_updates_verified'] == 200, 'load recovery')
    require(len(load['observations']) >= 600, 'insufficient observations')
    require(all(x['poolRejections'] == 0 for x in load['observations']), 'query pool rejections')
    for kind in ('point', 'scalar', 'writes'):
        require(load[kind]['requests'] > 0 and load[kind]['p50_ms'] <= load[kind]['p95_ms'] <= load[kind]['p99_ms'], 'latency receipt')


def self_check():
    # Acceptance checks must reject missing, interrupted and stale receipts.
    c = {'nativeSha256':'abc', 'imageId':'img'}
    r = {'checks':dict(workspaceExit=0,clippyExit=0,fmtExit=0,rustTestsPassed=590,rustTestsIgnored=0,pythonUnitExit=0,pythonUnits=21,nativeSha256='abc'),
         'python-acl-tui':dict(status='PASS',serverSha256='abc',nodes=1000,checks=['private ACL checkpoint restore']),
         'ducklake':dict(exitCode=0,testsPassed=11,skipped=0,binarySha256='abc')}
    for name in ('compose-auth','swarm','kubernetes'):
        r[name]=dict(result='PASS',imageId='img',rows=1000,ownedResourcesRemoved=True,audit={'status':'ok'},hostSwarmUnchanged=True,restore_audit={'status':'ok'},checkpointAudit={'status':'ok'})
    r['mixed-100000-3600']=dict(result='PASS',binary_sha256='abc',rows=100000,requested_seconds=3600,load_seconds=3600,
        synthetic=True,hot_point_reads=True,audit={'status':'ok'},acknowledged_latest_updates_verified=200,
        observations=[{'poolRejections':0}]*600,**{k:dict(requests=1,p50_ms=1,p95_ms=2,p99_ms=3) for k in ('point','scalar','writes')})
    validate(c,r)
    for section,field,value in [('mixed-100000-3600','load_seconds',3599),('compose-auth','imageId','old'),('ducklake','binarySha256','old'),('swarm','ownedResourcesRemoved',False),('checks','nativeSha256','old'),('checks','pythonUnits',20),('swarm','restore_audit',{'status':'failed'}),('kubernetes','checkpointAudit',{'status':'failed'})]:
        bad=copy.deepcopy(r);bad[section][field]=value
        try: validate(c,bad)
        except ValueError: pass
        else: raise AssertionError('accepted '+field)
    for section,field in [('kubernetes',None),('swarm','restore_audit'),('kubernetes','checkpointAudit')]:
        bad=copy.deepcopy(r)
        if field is None: del bad[section]
        else: del bad[section][field]
        try: validate(c,bad)
        except KeyError: pass
        else: raise AssertionError('accepted missing receipt or audit')
    print('self-check PASS: rejects shortened run, stale hashes, missing receipt and failed cleanup')


if __name__ == '__main__':
    if '--self-check' in sys.argv:
        self_check();sys.exit(0)
    candidate=json.loads((HERE/'candidate.json').read_text())
    receipts={name:json.loads((HERE/(name+'.json')).read_text()) for name in
        ('checks','python-acl-tui','ducklake','compose-auth','swarm','kubernetes','mixed-100000-3600')}
    require(hashlib.sha256(Path(candidate['nativeBinary']).read_bytes()).hexdigest()==candidate['nativeSha256'], 'native file changed')
    for name,digest in candidate['sourceSha256'].items():
        require(hashlib.sha256((ROOT/name).read_bytes()).hexdigest()==digest, 'source changed: '+name)
    validate(candidate,receipts)
    print(json.dumps(dict(result='PASS',scope='local synthetic single-owner candidate only',
        nativeSha256=candidate['nativeSha256'],imageId=candidate['imageId'],
        pending=['live external application source','dynamic adapter/atomic ACL ingestion','consensus and machine-loss replication']),indent=2))
