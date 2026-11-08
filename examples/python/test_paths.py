import json
import unittest
from contextlib import contextmanager
from unittest.mock import patch

from brainwire.paths import trace_paths
from brainwire.context import ContextLimitError


def path(*ids):
    return {'nodes': [{'label': 'Item', 'entity_id': value} for value in ids],
            'relationships': ['LINK'] * (len(ids) - 1)}


class Graph:
    database = 'workspace'

    def __init__(self, rows=()):
        self.rows, self.calls, self.closed = rows, [], False

    @contextmanager
    def stream_cypher(self, query, parameters, *, fetch_size):
        self.calls.append((query, parameters, fetch_size))
        try:
            yield iter(self.rows)
        finally:
            self.closed = True


class PathTests(unittest.TestCase):
    def test_query_is_directed_parameterized_and_returns_instance_identities(self):
        graph = Graph([path("start' OR true", 'middle', 'end')])
        result = trace_paths(graph, ('Item', "start' OR true"), ('Item', 'end'),
                             relationship='LINK', max_hops=3, limit=2)
        query, parameters, fetch_size = graph.calls[0]
        self.assertIn('MATCH p = (s:Item {entity_id:$source_id})-[:LINK*1..3]->', query)
        self.assertIn('[n IN nodes(p)', query)
        self.assertNotIn("start' OR true", query)
        self.assertEqual(parameters, {'source_id': "start' OR true", 'target_id': 'end', 'limit': 3})
        self.assertEqual(fetch_size, 1)
        self.assertEqual(result['paths'], [path("start' OR true", 'middle', 'end')])
        self.assertFalse(result['schema_only'])
        self.assertFalse(result['truncated'])
        self.assertTrue(graph.closed)

    def test_optional_relationship_and_visible_lookahead(self):
        graph = Graph([path('a', 'b'), path('a', 'c', 'b')])
        result = trace_paths(graph, ('Item', 'a'), ('Item', 'b'), limit=1)
        self.assertIn('-[*1..3]->', graph.calls[0][0])
        self.assertEqual(result['paths'], [path('a', 'b')])
        self.assertTrue(result['truncated'])

    def test_incoming_and_either_direction_are_explicit_and_parameterized(self):
        for direction, pattern in [('incoming', '<-[:LINK*1..2]-'),
                                   ('both', '-[:LINK*1..2]-')]:
            with self.subTest(direction=direction):
                graph = Graph([path('start', 'middle', 'end')])
                result = trace_paths(graph, ('Item', 'start'), ('Item', 'end'),
                                     relationship='LINK', direction=direction, max_hops=2)
                self.assertIn(pattern, graph.calls[0][0])
                self.assertEqual(result['direction'], direction)
                self.assertEqual(result['paths'], [path('start', 'middle', 'end')])
                self.assertEqual(graph.calls[0][1]['source_id'], 'start')
                self.assertTrue(graph.closed)

    def test_invalid_direction_fails_before_query(self):
        for direction in ['sideways', 'OUTGOING', '-[]->', '', None, [], 1]:
            graph = Graph()
            with self.subTest(direction=direction), self.assertRaises(ValueError):
                trace_paths(graph, ('Item', 'a'), ('Item', 'b'), direction=direction)
            self.assertEqual(graph.calls, [])

    def test_empty_results_have_no_endpoint_existence_signal(self):
        result = trace_paths(Graph(), ('Item', 'missing'), ('Item', 'hidden'))
        self.assertEqual(result['paths'], [])
        self.assertFalse(result['truncated'])
        self.assertNotIn('found', result)

    def test_validation_happens_before_graph_access(self):
        for options in ({'max_hops': 0}, {'max_hops': 5}, {'max_hops': True},
                        {'limit': 0}, {'limit': 6}, {'limit': True},
                        {'max_bytes': 0}, {'max_bytes': (1 << 20) + 1},
                        {'relationship': 'LINK] RETURN 1'}, {'timeout': 0},
                        {'timeout': float('inf')}, {'timeout': True}):
            graph = Graph()
            with self.subTest(options=options), self.assertRaises(ValueError):
                trace_paths(graph, ('Item', 'a'), ('Item', 'b'), **options)
            self.assertEqual(graph.calls, [])
        for source in ('a', ('Item',), ('Item)', 'a'), ('Item', ''), ('Item', 'x' * 4097)):
            graph = Graph()
            with self.subTest(source=source), self.assertRaises(ValueError):
                trace_paths(graph, source, ('Item', 'b'))
            self.assertEqual(graph.calls, [])

    def test_output_bound_counts_utf8_and_closes_stream(self):
        graph = Graph([path('a', 'é' * 200, 'b')])
        with self.assertRaises(ContextLimitError):
            trace_paths(graph, ('Item', 'a'), ('Item', 'b'), max_bytes=400)
        self.assertTrue(graph.closed)
        graph = Graph()
        with self.assertRaises(ContextLimitError):
            trace_paths(graph, ('Item', 'a'), ('Item', 'b'), max_bytes=1)
        self.assertEqual(graph.calls, [])

    def test_output_budget_accepts_exact_encoded_size_and_rejects_one_byte_less(self):
        for rows, limit in [([path('a', 'é中', 'b')], 3),
                            ([path('a', 'é中', 'b'), path('a', 'other', 'b')], 1)]:
            with self.subTest(limit=limit):
                expected = trace_paths(Graph(rows), ('Item', 'a'), ('Item', 'b'), limit=limit)
                size = len(json.dumps(expected, ensure_ascii=False, allow_nan=False).encode('utf-8'))
                exact = Graph(rows)
                self.assertEqual(trace_paths(exact, ('Item', 'a'), ('Item', 'b'),
                                             limit=limit, max_bytes=size), expected)
                self.assertTrue(exact.closed)
                short = Graph(rows)
                with self.assertRaises(ContextLimitError):
                    trace_paths(short, ('Item', 'a'), ('Item', 'b'),
                                limit=limit, max_bytes=size - 1)
                self.assertTrue(short.closed)

    def test_late_stream_error_propagates_without_returning_partial_paths(self):
        failure = RuntimeError('late terminal summary failure')

        class LateGraph(Graph):
            @contextmanager
            def stream_cypher(self, query, parameters, *, fetch_size):
                with super().stream_cypher(query, parameters, fetch_size=fetch_size) as rows:
                    yield rows
                raise failure

        graph = LateGraph([path('a', 'b')])
        marker = object()
        result = marker
        with self.assertRaises(RuntimeError) as caught:
            result = trace_paths(graph, ('Item', 'a'), ('Item', 'b'))
        self.assertIs(caught.exception, failure)
        self.assertIs(result, marker)
        self.assertEqual(len(graph.calls), 1)
        self.assertTrue(graph.closed)

    def test_rejects_invalid_rows_and_oversized_server_results(self):
        invalid = [path('wrong', 'b'), path('a', 'wrong'),
                   {'nodes': [], 'relationships': []},
                   {'nodes': [{'label': 'Item', 'entity_id': 'a'}, {'label': 'Item', 'entity_id': 'b'}],
                    'relationships': ['bad)']},
                   {'nodes': [{'label': 'Item', 'entity_id': 'a'}, {'label': 'Other', 'entity_id': 'b'}],
                    'relationships': ['LINK']},
                   {'nodes': [{'label': 'Item', 'entity_id': 'a'}, {'label': 'Item', 'entity_id': 'b'}],
                    'relationships': ['OTHER']},
                   path('a', 'c', 'd', 'e', 'f', 'b')]
        for row in invalid:
            graph = Graph([row])
            with self.subTest(row=row), self.assertRaises(ValueError):
                trace_paths(graph, ('Item', 'a'), ('Item', 'b'), relationship='LINK')
            self.assertTrue(graph.closed)
        graph = Graph([path('a', 'b')] * 3)
        with self.assertRaises(ContextLimitError):
            trace_paths(graph, ('Item', 'a'), ('Item', 'b'), limit=1)
        self.assertTrue(graph.closed)

    def test_cooperative_timeout_closes_stream(self):
        graph = Graph([path('a', 'b')])
        with patch('brainwire.paths.time.monotonic', side_effect=[0, 0, 11]), \
                self.assertRaises(TimeoutError):
            trace_paths(graph, ('Item', 'a'), ('Item', 'b'), timeout=10)
        self.assertTrue(graph.closed)


if __name__ == '__main__':
    unittest.main()
