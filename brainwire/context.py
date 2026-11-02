"""Bounded read-only evidence retrieval over a caller-authenticated GraphClient."""
import json

from .client import identifier


class ContextLimitError(ValueError):
    """Requested context exceeds its output bound; no partial result is returned."""


def _entity(value):
    if not isinstance(value, str) or not value or len(value.encode('utf-8')) > 4096:
        raise ValueError('entity IDs must be nonempty strings of at most 4096 UTF-8 bytes')
    return value


def _options(properties, limit, max_bytes):
    if type(limit) is not int or not 1 <= limit <= 100:
        raise ValueError('limit must be an integer from 1 to 100')
    if type(max_bytes) is not int or not 1 <= max_bytes <= 1 << 20:
        raise ValueError('max_bytes must be an integer from 1 to 1048576')
    if not isinstance(properties, (list, tuple)) or len(properties) > 16:
        raise ValueError('properties must be a list or tuple of at most 16 names')
    names = [identifier(name) for name in properties]
    if len(set(names)) != len(names):
        raise ValueError('property names must be unique')
    return names


class ContextClient:
    """Retrieve explicit records and one-hop evidence from one named graph.

    The supplied GraphClient controls credentials and graph identity. Queries use
    its read-only streaming route and existing server authorization. This helper
    does not infer identities, run model-generated queries or perform ranking.
    """

    def __init__(self, graph):
        self.graph = graph

    def _read(self, query, parameters, label, names, limit, max_bytes, **metadata):
        result = {'graph': self.graph.database, **metadata, 'limit': limit, 'records': []}
        size = len(json.dumps(result, ensure_ascii=False, allow_nan=False).encode('utf-8'))
        if size > max_bytes:
            raise ContextLimitError('context envelope exceeds max_bytes')
        with self.graph.stream_cypher(query, parameters, fetch_size=min(limit, 100)) as rows:
            for row in rows:
                if len(result['records']) >= limit:
                    raise ContextLimitError('context returned more than limit records')
                record = {'label': label, 'entity_id': row['entity_id'],
                          'properties': {name: row[f'p{index}'] for index, name in enumerate(names)}}
                # Include separators conservatively without repeatedly encoding prior rows.
                size += len(json.dumps(record, ensure_ascii=False, allow_nan=False).encode('utf-8')) + 2
                if size > max_bytes:
                    raise ContextLimitError('context exceeds max_bytes; request fewer or smaller properties')
                result['records'].append(record)
        return result

    @staticmethod
    def _projection(names):
        return ', '.join(['n.entity_id AS entity_id'] +
                         [f'n.{name} AS p{index}' for index, name in enumerate(names)])

    def nodes(self, label, entity_ids, *, properties=(), limit=20, max_bytes=65536):
        """Fetch up to limit visible nodes by at most 100 explicit IDs."""
        label = identifier(label)
        names = _options(properties, limit, max_bytes)
        if not isinstance(entity_ids, (list, tuple)) or len(entity_ids) > 100:
            raise ValueError('entity_ids must be a list or tuple of at most 100 IDs')
        ids = [_entity(value) for value in entity_ids]
        if not ids:
            result = {'graph': self.graph.database, 'limit': limit, 'records': []}
            if len(json.dumps(result, ensure_ascii=False).encode('utf-8')) > max_bytes:
                raise ContextLimitError('context envelope exceeds max_bytes')
            return result
        query = (f'MATCH (n:{label}) WHERE n.entity_id IN $ids '
                 f'RETURN {self._projection(names)} LIMIT $limit')
        return self._read(query, {'ids': ids, 'limit': limit}, label, names, limit, max_bytes)

    def search_text(self, label, text, *, field, entity_ids, properties=(),
                    limit=20, max_bytes=65536):
        """Filter explicit candidates by case-insensitive substring; no relevance score."""
        label, field = identifier(label), identifier(field)
        names = _options(properties, limit, max_bytes)
        if not isinstance(text, str) or not text.strip() or len(text.encode('utf-8')) > 4096:
            raise ValueError('text must be a nonblank string of at most 4096 UTF-8 bytes')
        if not isinstance(entity_ids, (list, tuple)) or len(entity_ids) > 100:
            raise ValueError('entity_ids must be a list or tuple of at most 100 IDs')
        ids = [_entity(value) for value in entity_ids]
        search = {'field': field, 'mode': 'case_insensitive_substring'}
        if not ids:
            result = {'graph': self.graph.database, 'search': search, 'limit': limit, 'records': []}
            if len(json.dumps(result, ensure_ascii=False).encode('utf-8')) > max_bytes:
                raise ContextLimitError('context envelope exceeds max_bytes')
            return result
        query = (f'MATCH (n:{label}) WHERE n.entity_id IN $ids '
                 f'AND toLower(n.{field}) CONTAINS toLower($text) '
                 f'RETURN {self._projection(names)} LIMIT $limit')
        return self._read(query, {'ids': ids, 'text': text, 'limit': limit}, label,
                          names, limit, max_bytes, search=search)

    def neighbors(self, source_label, source_id, relationship, target_label, *,
                  properties=(), limit=20, max_bytes=65536):
        """Fetch visible outgoing one-hop targets with explicit path metadata."""
        source_label, relationship, target_label = map(identifier,
            (source_label, relationship, target_label))
        source_id = _entity(source_id)
        names = _options(properties, limit, max_bytes)
        query = (f'MATCH (s:{source_label} {{entity_id:$source_id}})-[:{relationship}]->(n:{target_label}) '
                 f'RETURN {self._projection(names)} LIMIT $limit')
        return self._read(query, {'source_id': source_id, 'limit': limit}, target_label,
                          names, limit, max_bytes,
                          source={'label': source_label, 'entity_id': source_id},
                          relationship=relationship)
