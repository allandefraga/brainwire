"""Bounded offline inspection of explicitly authorized Brainwire code artifacts."""
import argparse
from collections import deque
import json
import os
import re
import stat
import sys

from .code_graph import _text, _RELATIONS

_MAX_INPUT = 8 << 20
_EXTRACTORS = ('brainwire-python-ast-v1', 'brainwire-python-directory-v1',
               'brainwire-python-directory-imports-v1', 'brainwire-python-directory-calls-v1')


class InspectionLimitError(ValueError):
    """Inspection exceeded a declared limit; no partial result is returned."""


def _integer(value, name, maximum, minimum=1):
    if type(value) is not int or not minimum <= value <= maximum:
        raise ValueError(f'{name} is outside its integer bounds')
    return value


def _hash(value, name):
    if not isinstance(value, str) or re.fullmatch('[0-9a-f]{64}', value) is None:
        raise ValueError(f'{name} must be a SHA-256 hexadecimal value')
    return value


def _validated(artifact):
    if not isinstance(artifact, dict) or artifact.get('schema_version') != '1' \
            or artifact.get('extractor') not in _EXTRACTORS:
        raise ValueError('unsupported code artifact contract')
    _hash(artifact.get('source_hash'), 'source_hash')
    generation = artifact.get('generation_id')
    if artifact['extractor'] in ('brainwire-python-directory-imports-v1', 'brainwire-python-directory-calls-v1'):
        _hash(generation, 'generation_id')
    elif generation is not None:
        raise ValueError('unexpected code artifact generation')
    nodes, edges = artifact.get('nodes'), artifact.get('edges')
    if not isinstance(nodes, list) or not isinstance(edges, list) \
            or len(nodes) > 10000 or len(edges) > 20000:
        raise InspectionLimitError('code artifact exceeds node/edge limits')
    normalized = {}
    namespace = None
    for raw in nodes:
        if not isinstance(raw, dict):
            raise ValueError('code entities must be maps')
        identity = _text(raw.get('entity_id'), 'entity ID', 256)
        if re.fullmatch('code:[0-9a-f]{64}', identity) is None or identity in normalized:
            raise ValueError('code entity IDs must be valid and unique')
        kind = raw.get('kind')
        if kind not in ('file', 'class', 'function', 'import', 'call') or raw.get('evidence') != 'EXTRACTED':
            raise ValueError('invalid code entity kind or evidence')
        node = {'entity_id': identity, 'kind': kind, 'evidence': 'EXTRACTED'}
        for key, maximum in (('namespace', 256), ('name', 4096), ('qualified_name', 4096), ('source_path', 1024)):
            node[key] = _text(raw.get(key), key, maximum)
        node['source_hash'] = _hash(raw.get('source_hash'), 'entity source_hash')
        node['start_line'] = _integer(raw.get('start_line'), 'start_line', 1048577)
        node['end_line'] = _integer(raw.get('end_line'), 'end_line', 1048577, node['start_line'])
        if kind == 'call':
            if artifact['extractor'] != 'brainwire-python-directory-calls-v1':
                raise ValueError('call sites require the static-call artifact contract')
            node['start_column'] = _integer(raw.get('start_column'), 'start_column', 1048576, 0)
            node['end_column'] = _integer(raw.get('end_column'), 'end_column', 1048576,
                node['start_column'] if node['start_line'] == node['end_line'] else 0)
            node['call_ordinal'] = _integer(raw.get('call_ordinal'), 'call_ordinal', 9999, 0)
        if raw.get('alias') is not None:
            node['alias'] = _text(raw['alias'], 'alias', 4096)
        if raw.get('generation_id') != generation:
            raise ValueError('code entities cross artifact generations')
        if generation is not None:
            node['generation_id'] = generation
        if namespace is not None and node['namespace'] != namespace:
            raise ValueError('code entities cross artifact namespaces')
        namespace = node['namespace']
        normalized[identity] = node
    distinct = {}
    for raw in edges:
        if not isinstance(raw, dict):
            raise ValueError('code relationships must be maps')
        source = _text(raw.get('source_id'), 'source ID', 256)
        target = _text(raw.get('target_id'), 'target ID', 256)
        relation = raw.get('relationship')
        if source not in normalized or target not in normalized or relation not in _RELATIONS:
            raise ValueError('invalid code relationship endpoints/type')
        inferred = relation in ('LOCAL_MODULE_CANDIDATE', 'STATIC_CALL_CANDIDATE')
        evidence = 'INFERRED' if inferred else 'EXTRACTED'
        if raw.get('evidence') != evidence or raw.get('generation_id') != generation:
            raise ValueError('invalid code relationship evidence or generation')
        left, right = normalized[source], normalized[target]
        owner = left if inferred else right
        if inferred:
            expected = ('import', 'file', 'STATIC_LOCAL_PATH') if relation == 'LOCAL_MODULE_CANDIDATE' else \
                ('call', 'function', 'STATIC_IMPORT_BINDING')
            if generation is None or left['kind'] != expected[0] or right['kind'] != expected[1] \
                    or raw.get('resolution') != expected[2]:
                raise ValueError('invalid static candidate evidence')
        else:
            parents, children = (('function',), ('call',)) if relation == 'HAS_CALL' else \
                (('file', 'class', 'function'), ('class', 'function') if relation == 'DEFINES' else ('import',))
            if left['kind'] not in parents or right['kind'] not in children \
                    or left['source_path'] != right['source_path'] or left['source_hash'] != right['source_hash']:
                raise ValueError('invalid extracted syntax relationship')
        edge = {'source_id': source, 'target_id': target, 'relationship': relation, 'evidence': evidence}
        for key in ('source_path', 'source_hash', 'start_line', 'end_line'):
            if raw.get(key) != owner[key]:
                raise ValueError('code relationship provenance does not match its source occurrence')
            edge[key] = owner[key]
        if inferred:
            edge['resolution'] = raw['resolution']
        if generation is not None:
            edge['generation_id'] = generation
        key = source, relation, target
        if key in distinct and distinct[key] != edge:
            raise ValueError('duplicate code relationships disagree on evidence')
        distinct[key] = edge
    ordered = sorted(distinct.values(), key=lambda edge: (edge['relationship'], edge['source_id'], edge['target_id']))
    return normalized, ordered


def _selector(nodes, selector):
    _text(selector, 'selector', 4096)
    if selector in nodes:
        return selector
    matches = [identity for identity, node in nodes.items() if node['qualified_name'] == selector]
    if len(matches) != 1:
        raise ValueError('selector is missing or ambiguous; use a unique entity_id')
    return matches[0]


def _bounded(result, max_bytes):
    _integer(max_bytes, 'max_bytes', 65536)
    if len(json.dumps(result, ensure_ascii=False, separators=(',', ':')).encode('utf-8')) > max_bytes:
        raise InspectionLimitError('inspection exceeds max_bytes; request fewer edges or a shorter path')
    return result


def explain_node(artifact, selector, *, max_edges=50, max_bytes=65536):
    """Explain one explicit entity and bounded incident edges from an authorized artifact."""
    _integer(max_edges, 'max_edges', 100)
    _integer(max_bytes, 'max_bytes', 65536)
    nodes, edges = _validated(artifact)
    identity = _selector(nodes, selector)
    incident = [edge for edge in edges if identity in (edge['source_id'], edge['target_id'])]
    return _bounded({'node': nodes[identity], 'edges': incident[:max_edges],
                     'incident_count': len(incident), 'truncated': len(incident) > max_edges}, max_bytes)


def find_path(artifact, source, target, *, directed=False, max_hops=16,
              max_visited=10000, max_bytes=65536):
    """Find a deterministic bounded BFS path; retain original edge directions/evidence."""
    if type(directed) is not bool:
        raise ValueError('directed must be a boolean')
    _integer(max_hops, 'max_hops', 16, 0)
    _integer(max_visited, 'max_visited', 10000)
    _integer(max_bytes, 'max_bytes', 65536)
    nodes, edges = _validated(artifact)
    source, target = _selector(nodes, source), _selector(nodes, target)
    adjacency = {identity: [] for identity in nodes}
    for edge in edges:
        adjacency[edge['source_id']].append((edge['target_id'], edge, 'outgoing'))
        if not directed and edge['source_id'] != edge['target_id']:
            adjacency[edge['target_id']].append((edge['source_id'], edge, 'incoming'))
    for neighbors in adjacency.values():
        neighbors.sort(key=lambda item: (item[0], item[1]['relationship'], item[1]['source_id'], item[1]['target_id']))
    previous = {source: None}
    def found(identity):
        path_nodes, path_edges = [identity], []
        while previous[identity] is not None:
            parent, edge, direction = previous[identity]
            path_edges.append({**edge, 'direction': direction})
            path_nodes.append(parent)
            identity = parent
        path_nodes.reverse(); path_edges.reverse()
        evidence = 'INFERRED' if any(edge['evidence']=='INFERRED' for edge in path_edges) else 'EXTRACTED'
        return _bounded({'status': 'found', 'hops': len(path_edges),
                         'nodes': [nodes[item] for item in path_nodes], 'edges': path_edges,
                         'evidence': evidence, 'directed': directed, 'visited': len(previous)}, max_bytes)

    if source == target:
        return found(source)
    pending = deque([(source, 0)])
    while pending:
        identity, depth = pending.popleft()
        if depth >= max_hops:
            continue
        for neighbor, edge, direction in adjacency[identity]:
            if neighbor in previous:
                continue
            if len(previous) >= max_visited:
                raise InspectionLimitError('path search exceeds max_visited; no partial path returned')
            previous[neighbor] = identity, edge, direction
            if neighbor == target:
                return found(target)
            pending.append((neighbor, depth + 1))
    return _bounded({'status': 'not_found_within_bounds', 'max_hops': max_hops,
                     'visited': len(previous), 'directed': directed, 'nodes': [], 'edges': []}, max_bytes)


def _pairs(items):
    result = {}
    for key, value in items:
        if key in result:
            raise ValueError('duplicate JSON keys are not allowed')
        result[key] = value
    return result


def _json_integer(value):
    if len(value) > 16:
        raise ValueError('JSON integer exceeds code artifact bounds')
    return int(value)


def _no_float(value):
    raise ValueError('floating-point values are not part of the code artifact contract')


def load_artifact(path):
    """Read at most 8 MiB before JSON parsing; never read source paths from the artifact."""
    if os.name != 'posix' or not hasattr(os, 'O_NOFOLLOW'):
        raise ValueError('safe artifact file loading requires POSIX no-follow support')
    try:
        fd = os.open(os.fspath(path), os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(fd, 'rb') as stream:
            if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
                raise ValueError('artifact must be a regular file')
            data = stream.read(_MAX_INPUT + 1)
    except OSError as error:
        raise ValueError('cannot safely read code artifact') from error
    if len(data) > _MAX_INPUT:
        raise InspectionLimitError('code artifact exceeds 8 MiB input bound')
    artifact = json.loads(data.decode('utf-8'), object_pairs_hook=_pairs,
                          parse_int=_json_integer, parse_float=_no_float, parse_constant=_no_float)
    _validated(artifact)
    return artifact


def main(argv=None):
    parser = argparse.ArgumentParser(description='Inspect an authorized code graph JSON artifact offline.')
    parser.add_argument('artifact', help='approved artifact JSON file; no source files are read')
    commands = parser.add_subparsers(dest='command', required=True)
    explain = commands.add_parser('explain', help='inspect an entity and its incident evidence')
    explain.add_argument('selector', help='entity_id or unique qualified_name')
    explain.add_argument('--max-edges', type=int, default=50)
    path = commands.add_parser('path', help='find a bounded BFS path')
    path.add_argument('source', help='entity_id or unique qualified_name')
    path.add_argument('target', help='entity_id or unique qualified_name')
    path.add_argument('--directed', action='store_true')
    path.add_argument('--max-hops', type=int, default=16)
    path.add_argument('--max-visited', type=int, default=10000)
    for command in (explain, path):
        command.add_argument('--max-bytes', type=int, default=65536)
    args = parser.parse_args(argv)
    try:
        artifact = load_artifact(args.artifact)
        if args.command == 'explain':
            result = explain_node(artifact, args.selector, max_edges=args.max_edges, max_bytes=args.max_bytes)
        else:
            result = find_path(artifact, args.source, args.target, directed=args.directed,
                               max_hops=args.max_hops, max_visited=args.max_visited, max_bytes=args.max_bytes)
        print(json.dumps(result, ensure_ascii=False, separators=(',', ':')))
        return 0
    except (ValueError, RecursionError):
        print('error: invalid code artifact, selector or inspection limit; no partial result returned', file=sys.stderr)
        return 2


if __name__ == '__main__':
    raise SystemExit(main())
