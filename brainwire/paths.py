"""Bounded directed instance-path traces through the caller's graph visibility."""
import json
import math
import time

from .client import identifier
from .context import ContextLimitError, _entity


def _endpoint(value):
    if not isinstance(value, tuple) or len(value) != 2:
        raise ValueError('endpoints must be (label, entity_id) tuples')
    return {'label': identifier(value[0]), 'entity_id': _entity(value[1])}


def _bounded_output(value, max_bytes):
    size = 0
    for chunk in json.JSONEncoder(ensure_ascii=False, allow_nan=False).iterencode(value):
        size += len(chunk.encode('utf-8'))
        if size > max_bytes:
            raise ContextLimitError('path context exceeds max_bytes')


def trace_paths(graph, source, target, *, relationship=None, max_hops=3,
                limit=3, max_bytes=65536, timeout=10):
    """Return a few visible directed instance paths with one read-only query.

    Endpoints are (label, entity_id) tuples. Every result includes only ordered
    node identities and relationship type names, without arbitrary properties.
    Paths contain 1..max_hops relationships; a cycle may connect equal endpoints.
    Parallel relationships may produce the same node/type sequence. This is
    neither a shortest-path query nor a promise of stable result ordering.

    The server's normal graph grants, node visibility, query budget and deadline
    apply, including to intermediate nodes. Missing, hidden or disconnected
    endpoints have the same empty result shape; timing and query errors are not
    an absence proof or a non-interference guarantee. Paths are current records,
    not ontology suggestions or historical revisions. Different calls do not
    share a snapshot.

    LIMIT bounds returned paths, not traversal work: high fanout can still be
    expensive. max_bytes bounds the encoded result after driver decoding, not
    process RSS. timeout is checked cooperatively between operations and cannot
    preempt a blocked Bolt request; configure server/driver timeouts separately.
    """
    source, target = _endpoint(source), _endpoint(target)
    if relationship is not None:
        relationship = identifier(relationship)
    for name, value, upper in (('max_hops', max_hops, 4), ('limit', limit, 5),
                               ('max_bytes', max_bytes, 1 << 20)):
        if type(value) is not int or not 1 <= value <= upper:
            raise ValueError(f'{name} must be an integer from 1 to {upper}')
    if type(timeout) not in (int, float) or not math.isfinite(timeout) or not 0 < timeout <= 300:
        raise ValueError('timeout must be finite and greater than 0 through 300 seconds')
    database = _entity(graph.database)
    result = {'graph': database, 'schema_only': False, 'source': source, 'target': target,
              'relationship': relationship, 'max_hops': max_hops, 'limit': limit,
              'paths': [], 'truncated': False}
    _bounded_output(result, max_bytes)
    deadline = time.monotonic() + timeout

    def check_deadline():
        if time.monotonic() >= deadline:
            raise TimeoutError('path context deadline exceeded')

    pattern = ':' + relationship if relationship is not None else ''
    query = (f'MATCH p = (s:{source["label"]} {{entity_id:$source_id}})'
             f'-[{pattern}*1..{max_hops}]->'
             f'(t:{target["label"]} {{entity_id:$target_id}}) '
             'RETURN [n IN nodes(p) | {label:head(labels(n)), entity_id:n.entity_id}] AS nodes, '
             '[r IN relationships(p) | type(r)] AS relationships LIMIT $limit')
    parameters = {'source_id': source['entity_id'], 'target_id': target['entity_id'], 'limit': limit + 1}
    check_deadline()
    count = 0
    with graph.stream_cypher(query, parameters, fetch_size=1) as rows:
        for row in rows:
            check_deadline()
            count += 1
            if count > limit + 1:
                raise ContextLimitError('path query returned more than the requested row bound')
            if not isinstance(row, dict) or set(row) != {'nodes', 'relationships'}:
                raise ValueError('invalid path query response')
            nodes, relationships = row['nodes'], row['relationships']
            if not isinstance(nodes, list) or not isinstance(relationships, list) or \
                    not 1 <= len(relationships) <= max_hops or len(nodes) != len(relationships) + 1:
                raise ValueError('path response exceeds hop bound or has invalid shape')
            identities = []
            for node in nodes:
                if not isinstance(node, dict) or set(node) != {'label', 'entity_id'}:
                    raise ValueError('invalid path node identity')
                identities.append({'label': identifier(node['label']), 'entity_id': _entity(node['entity_id'])})
            types = [identifier(value) for value in relationships]
            if identities[0] != source or identities[-1] != target or \
                    relationship is not None and any(value != relationship for value in types):
                raise ValueError('path response does not match the requested endpoints or relationship')
            if count > limit:
                result['truncated'] = True
            else:
                result['paths'].append({'nodes': identities, 'relationships': types})
            # A later lookahead changes false to true, shrinking the JSON by one byte.
            _bounded_output(result, max_bytes + 1)
    _bounded_output(result, max_bytes)
    check_deadline()
    return result
