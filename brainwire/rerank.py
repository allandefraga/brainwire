"""Experimental exact reranking over explicit, authorized graph candidates."""
import json
import math
import re

from .client import identifier
from .context import ContextClient, ContextLimitError, _options


def _text(value, maximum, name):
    if not isinstance(value, str) or len(value) > maximum or len(value.encode('utf-8')) > maximum:
        raise ValueError(f'{name} must be a string of at most {maximum} UTF-8 bytes')
    return value


def _vector(value, dimension=None):
    if not isinstance(value, list) or not 1 <= len(value) <= 4096:
        raise ValueError('vectors must be lists of 1..4096 finite numbers')
    if dimension is not None and len(value) != dimension:
        raise ValueError('candidate vector dimension differs from query vector')
    converted = []
    for item in value:
        if type(item) not in (int, float):
            raise ValueError('vectors must contain finite numbers, excluding bool')
        try:
            number = float(item)
        except (OverflowError, ValueError):
            raise ValueError('vectors must contain finite numbers') from None
        if not math.isfinite(number):
            raise ValueError('vectors must contain finite numbers')
        converted.append(number)
    scale = max(map(abs, converted))
    if scale == 0:
        raise ValueError('vectors must have a positive norm')
    scaled = [number / scale for number in converted]
    norm = math.sqrt(math.fsum(number * number for number in scaled))
    return [number / norm for number in scaled]


def rerank_context(graph, label, entity_ids, query_text, query_vector, *,
                   text_property, vector_property, properties=(), limit=10, max_bytes=65536):
    """Fuse token-overlap and exact-cosine ranks with RRF(k=60).

    Uses one caller-authenticated read of explicit candidate IDs. Intermediate
    output is capped at 1 MiB; this is not a driver-buffer or process RSS cap.
    """
    label, text_property, vector_property = map(identifier,
        (label, text_property, vector_property))
    names = _options(properties, limit, max_bytes)
    query_text = _text(query_text, 1024, 'query_text')
    query = _vector(query_vector)
    if not isinstance(entity_ids, (list, tuple)) or len(entity_ids) > 100:
        raise ValueError('entity_ids must be a list or tuple of at most 100 IDs')
    # All text length caps precede tokenization. Case folding may expand Unicode.
    terms = set(re.findall(r'\w+', query_text.casefold()))
    if not 1 <= len(terms) <= 16:
        raise ValueError('query_text must contain 1..16 distinct word tokens')
    fields = list(dict.fromkeys([*names, text_property, vector_property]))
    if len(fields) > 16:
        raise ValueError('requested properties plus text/vector fields must fit 16 names')
    rows = ContextClient(graph).nodes(label, entity_ids, properties=fields,
                                     limit=100, max_bytes=1 << 20)['records']
    total_text = total_components = 0
    candidates = []
    seen = set()
    for row in rows:
        identity = row['entity_id']
        if identity in seen:
            raise ValueError('candidate identities must be unique')
        seen.add(identity)
        props = row['properties']
        text = _text(props[text_property], 16384, 'candidate text')
        total_text += len(text.encode('utf-8'))
        vector = props[vector_property]
        if not isinstance(vector, list):
            raise ValueError('candidate vector must be a list')
        total_components += len(vector)
        if total_text > 65536 or total_components > 65536:
            raise ContextLimitError('candidate text or vector component budget exceeded')
        candidates.append((row, text, vector))
    scored = []
    for row, text, vector in candidates:
        normalized = _vector(vector, len(query))
        matched = sorted(terms.intersection(re.findall(r'\w+', text.casefold())))
        cosine = max(-1.0, min(1.0, math.fsum(a * b for a, b in zip(query, normalized))))
        scored.append({'label': label, 'entity_id': row['entity_id'],
                       'properties': {name: row['properties'][name] for name in names},
                       'matched_terms': matched, 'lexical_score': len(matched) / len(terms),
                       'cosine_similarity': cosine})
    lexical = sorted((row for row in scored if row['matched_terms']),
                     key=lambda row: (-row['lexical_score'], row['entity_id']))
    vector = sorted(scored, key=lambda row: (-row['cosine_similarity'], row['entity_id']))
    lexical_ranks = {row['entity_id']: rank for rank, row in enumerate(lexical, 1)}
    vector_ranks = {row['entity_id']: rank for rank, row in enumerate(vector, 1)}
    for row in scored:
        rank = lexical_ranks.get(row['entity_id'])
        row['lexical_rank'] = rank
        row['vector_rank'] = vector_ranks[row['entity_id']]
        row['score'] = 1 / (60 + row['vector_rank']) + (1 / (60 + rank) if rank else 0)
    scored.sort(key=lambda row: (-row['score'], row['entity_id']))
    result = {'graph': graph.database, 'method': 'candidate_rrf_v1', 'rrf_k': 60,
              'limit': limit, 'records': scored[:limit]}
    if len(json.dumps(result, ensure_ascii=False, allow_nan=False).encode('utf-8')) > max_bytes:
        raise ContextLimitError('reranked context exceeds max_bytes')
    return result
