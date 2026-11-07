"""Owned synthetic acceptance for directed instance paths and intermediate ACLs."""
import argparse
import hashlib
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
from neo4j.exceptions import Neo4jError

from brainwire import ContextLimitError, GraphClient
from brainwire.paths import trace_paths
from examples.python.create_service_tokens import create


def node_ids(result):
    return [[node['entity_id'] for node in path['nodes']] for path in result['paths']]


def run(binary):
    with tempfile.TemporaryDirectory(prefix='brainwire-context-paths-') as temporary:
        root = Path(temporary)
        create(root / 'auth', 'workspace', acl_writer=True)
        policy_path = root / 'auth/policy.json'
        policy = json.loads(policy_path.read_text())
        for principal in policy['principals']:
            if principal['id'] in ('reader', 'health'):
                principal['node_visibility'] = {'principals': [principal['id']]}
        policy_path.write_text(json.dumps(policy))
        credentials = {role: json.loads((root / f'auth/{role}.json').read_text())
                       for role in ('writer', 'reader', 'health')}
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]
        config = root / 'brainwire.toml'
        config.write_text(f'''[server]
listen = "127.0.0.1:{port}"
data_dir = "{root / 'data'}"
default_database = "workspace"
auth_file = "{policy_path}"
[memory]
cache = "64MiB"
memtables = "4MiB"
query = "32MiB"
limit = "2GiB"
''')
        ontology = root / 'ontology.json'
        ontology.write_text(json.dumps({'version': '1', 'nodes': [{'label': 'Item'}],
            'edges': [{'type': name, 'from': {'label': 'Item'}, 'to': {'label': 'Item'}}
                      for name in ('LINK', 'OTHER')]}))
        env = {key: value for key, value in os.environ.items() if not key.startswith('BRAINWIRE_')}
        subprocess.run([str(binary), 'db', 'create', 'workspace', '--ontology', str(ontology),
                        '--config', str(config)], env=env, check=True, capture_output=True)

        def connect(role):
            auth = credentials[role]
            return GraphDatabase.driver(f'bolt://127.0.0.1:{port}',
                auth=(auth['principal'], auth['credentials']), connection_timeout=1,
                max_transaction_retry_time=0)

        with (root / 'server.log').open('w') as log:
            process = subprocess.Popen([str(binary), 'serve', '--config', str(config)],
                                       env=env, stdout=log, stderr=log)
            try:
                for _ in range(200):
                    if process.poll() is not None:
                        raise RuntimeError('temporary server stopped during startup')
                    try:
                        with socket.create_connection(('127.0.0.1', port), timeout=.1):
                            break
                    except OSError:
                        time.sleep(.025)
                else:
                    raise RuntimeError('temporary server did not start')
                with connect('writer') as driver, connect('reader') as read_driver, connect('health') as other_driver:
                    writer = GraphClient(driver, 'workspace')
                    reader = GraphClient(read_driver, 'workspace')
                    other = GraphClient(other_driver, 'workspace')
                    for identity, allow in [('a', ['reader', 'health']), ('b', ['reader', 'health']),
                                            ('hidden', ['health']), ('visible', ['reader']),
                                            ('alternate', ['reader'])]:
                        writer.upsert_node_with_acl('Item', identity, {}, {'allow': allow})
                    for source, target in [('a', 'hidden'), ('hidden', 'b')]:
                        writer.upsert_edge(('Item', source), 'LINK', ('Item', target))
                    options = {'relationship': 'LINK', 'max_hops': 3}
                    assert trace_paths(reader, ('Item', 'a'), ('Item', 'b'), **options)['paths'] == []
                    assert node_ids(trace_paths(other, ('Item', 'a'), ('Item', 'b'), **options)) == [['a', 'hidden', 'b']]
                    for target in ('hidden', 'missing'):
                        assert trace_paths(reader, ('Item', 'a'), ('Item', target), **options)['paths'] == []
                    for source in ('hidden', 'missing'):
                        assert trace_paths(reader, ('Item', source), ('Item', 'b'), **options)['paths'] == []
                    for middle in ('visible', 'alternate'):
                        writer.upsert_edge(('Item', 'a'), 'LINK', ('Item', middle))
                        writer.upsert_edge(('Item', middle), 'LINK', ('Item', 'b'))
                        if middle == 'visible':
                            single = trace_paths(reader, ('Item', 'a'), ('Item', 'b'), limit=1, **options)
                            assert node_ids(single) == [['a', 'visible', 'b']]
                            assert single['truncated'] is False, 'hidden alternative influenced lookahead'
                            assert 'hidden' not in json.dumps(single)
                    result = trace_paths(reader, ('Item', 'a'), ('Item', 'b'), **options)
                    assert set(map(tuple, node_ids(result))) == {('a', 'visible', 'b'), ('a', 'alternate', 'b')}
                    assert 'hidden' not in json.dumps(result)
                    assert all(path['relationships'] == ['LINK', 'LINK'] for path in result['paths'])
                    limited = trace_paths(reader, ('Item', 'a'), ('Item', 'b'), limit=1, **options)
                    assert len(limited['paths']) == 1 and limited['truncated']
                    assert trace_paths(reader, ('Item', 'a'), ('Item', 'b'), relationship='LINK', max_hops=1)['paths'] == []
                    assert trace_paths(reader, ('Item', 'b'), ('Item', 'a'), **options)['paths'] == []
                    writer.upsert_edge(('Item', 'visible'), 'LINK', ('Item', 'a'))
                    assert ['a', 'visible', 'a'] in node_ids(trace_paths(reader, ('Item', 'a'), ('Item', 'a'), **options))
                    writer.upsert_edge(('Item', 'a'), 'OTHER', ('Item', 'b'))
                    assert ['a', 'b'] in node_ids(trace_paths(reader, ('Item', 'a'), ('Item', 'b'), max_hops=1))
                    try:
                        trace_paths(reader, ('Item', 'a'), ('Item', 'b'), max_bytes=300, **options)
                    except ContextLimitError:
                        pass
                    else:
                        raise AssertionError('path output budget was ignored')
                    try:
                        reader.run_cypher("CREATE (:Item {entity_id:'forbidden'})")
                    except Neo4jError:
                        pass
                    else:
                        raise AssertionError('read-only principal could write')
                return {'result': 'PASS', 'synthetic': True,
                    'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
                    'checks': ['hidden intermediate nodes prevent traversal', 'hidden endpoints and missing endpoints return no paths',
                               'visible multi-hop paths and relationship types', 'direction, cycles and hop bounds',
                               'hidden alternatives do not influence truncation',
                               'visible lookahead and output byte bound', 'read grants reject writes'],
                    'limits': ['unordered path samples, not shortest or exhaustive paths',
                               'query work and memory depend on topology', 'small fixture, no timing non-interference guarantee']}
            finally:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/brainwire')
    args = parser.parse_args()
    print(json.dumps(run(args.binary.resolve()), indent=2))
