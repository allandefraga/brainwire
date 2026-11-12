"""Bounded read-only evidence retrieval over a caller-authenticated GraphClient."""
import hashlib
import json
import math
import re

from .client import identifier


class ContextLimitError(ValueError):
    """Requested context exceeds its output bound; no partial result is returned."""


_JSON_COLON = object()
_JSON_ESCAPE = re.compile(r'["\\\x00-\x1f]')
_JSON_SURROGATE = re.compile(r'[\ud800-\udfff]')


def _check_json_size(value, maximum, message):
    lower_bound_exceeds = _json_size_lower_bound_exceeds((value,), maximum)
    if lower_bound_exceeds is None:
        raise ContextLimitError('context exceeds maximum nesting depth of 256')
    if lower_bound_exceeds:
        raise ContextLimitError(message)
    size = 0
    for chunk in json.JSONEncoder(ensure_ascii=False, allow_nan=False).iterencode(value):
        size += len(chunk.encode('utf-8'))
        if size > maximum:
            raise ContextLimitError(message)
    return size


def _json_children(value):
    if isinstance(value, dict):
        for key, child in value.items():
            yield key, True
            yield _JSON_COLON, False
            yield child, False
    else:
        for child in value:
            yield child, False


def _json_string_size(value, maximum):
    # Mirror ensure_ascii=False escaping without constructing the encoded chunk.
    if value.isascii():
        if len(value) + 2 > maximum:
            return None
        if _JSON_ESCAPE.search(value) is None:
            return len(value) + 2
    else:
        if _JSON_SURROGATE.search(value):
            return False
        if len(value) + 2 > maximum:
            return None
    size = 2
    for start in range(0, len(value), 4096):
        encoded = json.dumps(value[start:start + 4096], ensure_ascii=False).encode('utf-8')
        size += len(encoded) - 2  # Each encoded chunk has its own quotes.
        if size > maximum:
            return None
    return size


def _json_size_lower_bound_exceeds(values, remaining):
    # Count a lower bound so broad/deep valid containers stop at the budget.
    pending = [(iter((value, False) for value in values), None)]
    active = set()
    minimum = 0
    while pending:
        iterator, identity = pending[-1]
        try:
            value, is_key = next(iterator)
        except StopIteration:
            pending.pop()
            if identity is not None:
                active.remove(identity)
                minimum += 1
                if minimum > remaining:
                    return True
            continue
        if isinstance(value, str):
            string_size = _json_string_size(value, remaining - minimum)
            if string_size is False:
                return False
            if string_size is None:
                return True
            minimum += string_size
        elif is_key:
            if value is None or isinstance(value, (int, float)):
                if isinstance(value, float) and not math.isfinite(value):
                    return False
                minimum += 1
            else:
                # Let JSONEncoder report malformed dictionary keys.
                return False
        elif isinstance(value, (dict, list, tuple)):
            # Bound both traversal and later JSON encoding for nested input.
            if len(pending) > 256:
                return None
            container_id = id(value)
            if container_id in active:
                return False
            minimum += 1
            active.add(container_id)
            pending.append((iter(_json_children(value)), container_id))
        elif value is _JSON_COLON:
            minimum += 1
        elif value is None or isinstance(value, (int, float)):
            if isinstance(value, float) and not math.isfinite(value):
                return False
            minimum += 1
        else:
            # Let JSONEncoder report unsupported values.
            return False
        if minimum > remaining:
            return True
    return False


def _oversized_string(values, remaining):
    # Preserve the early top-level string refusal used by bounded context reads.
    return any(isinstance(value, str) and len(value) > remaining for value in values)


def _entity(value):
    if not isinstance(value, str) or not value or len(value) > 4096 or len(value.encode('utf-8')) > 4096:
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
                if _oversized_string([record['entity_id'], *record['properties'].values()],
                                     max_bytes - size):
                    raise ContextLimitError(
                        'context exceeds max_bytes; request fewer or smaller properties')
                # Include separators conservatively without repeatedly encoding prior rows.
                record_size = _check_json_size(record, max_bytes - size - 2,
                    'context exceeds max_bytes; request fewer or smaller properties')
                size += record_size + 2
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

    def evidence(self, label, entity_ids, *, source_fields, properties=(),
                 limit=20, max_bytes=65536):
        """Read known records with explicit, reported source fields in one transaction.

        source_fields maps id/title/url/hash/recorded_at to declared properties.
        Citations identify current graph records; they do not authenticate the
        reported source or preserve historical revisions.
        """
        names = _options(properties, limit, max_bytes)
        allowed = {'id', 'title', 'url', 'hash', 'recorded_at'}
        if not isinstance(source_fields, dict) or not source_fields or \
                any(key not in allowed for key in source_fields):
            raise ValueError('source_fields must map id/title/url/hash/recorded_at to properties')
        fields = {key: identifier(source_fields[key]) for key in sorted(source_fields)}
        if len(set(fields.values())) != len(fields):
            raise ValueError('source_fields must map to distinct properties')
        requested = list(dict.fromkeys([*names, *fields.values()]))
        if len(requested) > 16:
            raise ValueError('requested and source properties must fit 16 names')
        label = identifier(label)
        if not isinstance(entity_ids, (list, tuple)) or len(entity_ids) > 100:
            raise ValueError('entity_ids must be a list or tuple of at most 100 IDs')
        ids = [_entity(value) for value in entity_ids]
        result = {'graph': self.graph.database, 'limit': limit, 'records': [], 'sources': []}
        size = len(json.dumps(result, ensure_ascii=False, allow_nan=False).encode('utf-8'))
        if size > max_bytes:
            raise ContextLimitError('evidence context exceeds max_bytes')
        if not ids:
            return result

        query = (f'MATCH (n:{label}) WHERE n.entity_id IN $ids '
                 f'RETURN {self._projection(requested)} LIMIT $limit')
        seen = set()
        intermediate = len(json.dumps({'graph': self.graph.database, 'limit': limit,
                                       'records': []}, ensure_ascii=False).encode('utf-8'))
        # One authorized snapshot; avoid accumulating rows after an output refusal.
        with self.graph.stream_cypher(query, {'ids': ids, 'limit': limit}, fetch_size=1) as rows:
            for row in rows:
                if len(result['records']) >= limit:
                    raise ContextLimitError('context returned more than limit records')
                values = {name: row[f'p{index}'] for index, name in enumerate(requested)}
                identity = _entity(row['entity_id'])
                if identity in seen:
                    raise ValueError('evidence record identities must be unique')
                seen.add(identity)
                original = {'label': label, 'entity_id': identity, 'properties': values}
                if _oversized_string(values.values(), (1 << 20) - intermediate):
                    raise ContextLimitError('context exceeds intermediate limit')
                original_size = _check_json_size(original, (1 << 20) - intermediate - 2,
                                                 'context exceeds intermediate limit')
                intermediate += original_size + 2
                if intermediate > 1 << 20:
                    raise ContextLimitError('context exceeds intermediate limit')
                source = {}
                for key, field in fields.items():
                    value = values[field]
                    if value is None:
                        continue
                    if not isinstance(value, str) or len(value) > 4096 or len(value.encode('utf-8')) > 4096:
                        raise ValueError('reported source values must be strings of at most 4096 UTF-8 bytes')
                    if value.strip():
                        source[key] = value
                citation = None
                source_record = None
                if source:
                    key = json.dumps([self.graph.database, label, identity],
                                     ensure_ascii=False, separators=(',', ':')).encode('utf-8')
                    citation = 'bw:' + hashlib.sha256(key).hexdigest()
                    source_record = {'citation_id': citation,
                        'record': {'label': label, 'entity_id': identity}, 'source': source}
                record = {'label': label, 'entity_id': identity,
                    'properties': {name: values[name] for name in names}, 'citation_id': citation}
                record_size = _check_json_size(record, max_bytes - size -
                    (2 if result['records'] else 0), 'evidence context exceeds max_bytes')
                size += record_size
                if result['records']:
                    size += 2
                if source_record is not None:
                    source_size = _check_json_size(source_record, max_bytes - size -
                        (2 if result['sources'] else 0),
                        'evidence context exceeds max_bytes')
                    size += source_size
                    if result['sources']:
                        size += 2
                if size > max_bytes:
                    raise ContextLimitError('evidence context exceeds max_bytes')
                result['records'].append(record)
                if source_record is not None:
                    result['sources'].append(source_record)
        return result

    def search_text(self, label, text, *, field, entity_ids, properties=(),
                    limit=20, max_bytes=65536):
        """Filter explicit candidates by case-insensitive substring; no relevance score."""
        label, field = identifier(label), identifier(field)
        names = _options(properties, limit, max_bytes)
        if not isinstance(text, str) or len(text) > 4096 or not text.strip() or len(text.encode('utf-8')) > 4096:
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
