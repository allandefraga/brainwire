"""Experimental search over explicit ranked providers and authorized graph data."""
from collections.abc import Mapping
import math
import re
import time

from .client import identifier
from .context import ContextClient, ContextLimitError, _check_json_size, _entity, _options


class ContextProviderError(RuntimeError):
    """A configured provider failed; its potentially private message is omitted."""


def search_context(graph, label, query_text, *, providers, source_fields=None,
                   properties=(), limit=10, max_bytes=65536, timeout=10, candidate_ids=None):
    """Fuse ranked provider IDs after one authorized graph read.

    Each callback receives query_text and keyword graph_name, label, limit=100,
    timeout=remaining_seconds, and returns a list/tuple of ranked IDs in that
    namespace. Results
    are candidates, never authorization. Hidden/missing IDs are removed before
    ranks are assigned, so they cannot influence visible scores or citations.

    candidate_ids optionally narrows providers to at most 100 explicit IDs.
    It is not an authorization boundary or a shared graph snapshot; provider
    top-k results outside the scope can reduce recall.

    The deadline is cooperative: callbacks must honor their supplied timeout.
    This helper cannot preempt a blocking provider or Bolt operation.
    Configure server and driver timeouts separately. Providers may contact
    external services only according to the application's own configuration.
    """
    label = identifier(label)
    graph_name = _entity(graph.database)
    names = _options(properties, limit, max_bytes)
    if not isinstance(query_text, str) or not query_text.strip() or \
            len(query_text) > 1024 or len(query_text.encode('utf-8')) > 1024:
        raise ValueError('query_text must be nonblank and at most 1024 UTF-8 bytes')
    if type(timeout) not in (int, float) or not math.isfinite(timeout) or not 0 < timeout <= 300:
        raise ValueError('timeout must be finite and greater than 0 through 300 seconds')
    if not isinstance(providers, Mapping) or not 1 <= len(providers) <= 4:
        raise ValueError('providers must map 1..4 names to ranked-ID callbacks')
    for name, callback in providers.items():
        if not isinstance(name, str) or re.fullmatch(r'[A-Za-z][A-Za-z0-9_-]{0,31}', name) is None:
            raise ValueError('provider names must be 1..32 ASCII letters, digits, underscore or dash')
        if not callable(callback):
            raise ValueError('providers must be callable')
    if source_fields is not None:
        allowed = {'id', 'title', 'url', 'hash', 'recorded_at'}
        if not isinstance(source_fields, dict) or not source_fields or any(
                name not in allowed for name in source_fields):
            raise ValueError('source_fields must map id/title/url/hash/recorded_at to properties')
        fields = [identifier(value) for value in source_fields.values()]
        if len(set(fields)) != len(fields) or len(set([*names, *fields])) > 16:
            raise ValueError('source fields must be distinct and fit the 16-property bound')

    scope = None
    if candidate_ids is not None:
        if type(candidate_ids) not in (list, tuple) or len(candidate_ids) > 100:
            raise ValueError('candidate_ids must be a list or tuple of at most 100 IDs')
        scope = set()
        size = 0
        for candidate in candidate_ids:
            identity = _entity(candidate)
            if identity not in scope:
                size += len(identity.encode('utf-8'))
                if size > 256 * 1024:
                    raise ContextLimitError('candidate_ids exceed their ID byte bound')
                scope.add(identity)

    result = {'graph': graph_name, 'method': 'provider_rrf_v1', 'rrf_k': 60,
              'providers': sorted(providers), 'limit': limit, 'records': [], 'sources': []}

    def output_bound():
        _check_json_size(result, max_bytes, 'searched context exceeds max_bytes')

    output_bound()
    if scope is not None and not scope:
        return result
    deadline = time.monotonic() + timeout

    def remaining():
        seconds = deadline - time.monotonic()
        if seconds <= 0:
            raise TimeoutError('context search deadline exceeded')
        return seconds

    rankings = {}
    candidates = {}
    candidate_bytes = 0
    for name in sorted(providers):
        seconds = remaining()
        try:
            values = providers[name](query_text, graph_name=graph_name, label=label,
                                     limit=100, timeout=seconds)
        except Exception:
            raise ContextProviderError(f'context provider {name} failed') from None
        remaining()
        if not isinstance(values, (list, tuple)):
            raise ValueError('a provider must return a list or tuple of ranked entity IDs')
        if len(values) > 100:
            raise ContextLimitError('a provider returned more than 100 entries')
        ordered, seen = [], set()
        for value in values:
            remaining()
            identity = _entity(value)
            if scope is not None and identity not in scope:
                continue
            if identity in seen:
                continue
            seen.add(identity)
            ordered.append(identity)
            if identity not in candidates:
                candidate_bytes += len(identity.encode('utf-8'))
                if len(candidates) >= 100 or candidate_bytes > 256 * 1024:
                    raise ContextLimitError('combined provider candidates exceed their ID bound')
                candidates[identity] = None
        remaining()
        rankings[name] = ordered
    if not candidates:
        return result

    remaining()
    context = ContextClient(graph)
    if source_fields is None:
        evidence = context.nodes(label, list(candidates), properties=names,
                                 limit=100, max_bytes=1 << 20)
    else:
        evidence = context.evidence(label, list(candidates), properties=names,
                                    source_fields=source_fields, limit=100, max_bytes=1 << 20)
    remaining()
    visible = {}
    for row in evidence['records']:
        identity = _entity(row['entity_id'])
        if identity not in candidates or identity in visible:
            raise ValueError('authorized graph records must have unique candidate identities')
        visible[identity] = {**row, 'score': 0.0, 'matches': []}
    for name, ordered in rankings.items():
        rank = 0
        for identity in ordered:
            if identity in visible:
                rank += 1
                visible[identity]['score'] += 1 / (60 + rank)
                visible[identity]['matches'].append({'provider': name, 'rank': rank})
    result['records'] = sorted(visible.values(), key=lambda row: (-row['score'], row['entity_id']))[:limit]
    sources = {source['citation_id']: source for source in evidence.get('sources', ())}
    result['sources'] = [sources[row['citation_id']] for row in result['records']
                         if row.get('citation_id') in sources]
    output_bound()
    remaining()
    return result
