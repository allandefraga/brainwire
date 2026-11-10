import unittest
import tracemalloc
from contextlib import contextmanager

from brainwire.context import ContextClient, ContextLimitError


class Graph:
    database = 'company'

    def __init__(self, records=()):
        self.records = records
        self.calls = []
        self.closed = False

    @contextmanager
    def stream_cypher(self, query, parameters, *, fetch_size):
        self.calls.append((query, parameters, fetch_size))
        try:
            yield iter(self.records)
        finally:
            self.closed = True


class ContextTests(unittest.TestCase):
    @unittest.skipIf(tracemalloc.is_tracing(), 'requires isolated allocation tracking')
    def test_large_property_is_rejected_without_copying_it(self):
        graph = Graph([{'entity_id': 't', 'p0': 'x' * (8 << 20)}])
        tracemalloc.start()
        try:
            with self.assertRaises(ContextLimitError):
                ContextClient(graph).nodes('Ticket', ['t'], properties=['body'], max_bytes=1024)
            peak = tracemalloc.get_traced_memory()[1]
        finally:
            tracemalloc.stop()
        self.assertLess(peak, 1 << 20)
        self.assertTrue(graph.closed)

    @unittest.skipIf(tracemalloc.is_tracing(), 'requires isolated allocation tracking')
    def test_evidence_rejects_large_property_without_copying_it(self):
        graph = Graph([{'entity_id': 't', 'p0': 'x' * (8 << 20), 'p1': 'source'}])
        tracemalloc.start()
        try:
            with self.assertRaises(ContextLimitError):
                ContextClient(graph).evidence('Ticket', ['t'], properties=['body'],
                    source_fields={'url': 'url'})
            peak = tracemalloc.get_traced_memory()[1]
        finally:
            tracemalloc.stop()
        self.assertLess(peak, 1 << 20)
        self.assertTrue(graph.closed)

    def test_nodes_parameterize_values_and_return_explicit_evidence(self):
        graph = Graph([{'entity_id': 't-1', 'p0': 'Title', 'p1': 'source://1'}])
        result = ContextClient(graph).nodes('Ticket', ["t-1' OR true"],
            properties=['title', 'source_url'], limit=3)
        query, parameters, page = graph.calls[0]
        self.assertNotIn("t-1' OR true", query)
        self.assertEqual(parameters, {'ids': ["t-1' OR true"], 'limit': 3})
        self.assertEqual(page, 3)
        self.assertEqual(result['records'], [{'label': 'Ticket', 'entity_id': 't-1',
            'properties': {'title': 'Title', 'source_url': 'source://1'}}])
        self.assertEqual(result['graph'], 'company')
        self.assertTrue(graph.closed)

    def test_neighbors_preserve_path_identity(self):
        graph = Graph([{'entity_id': 'p-1', 'p0': 'Release'}])
        result = ContextClient(graph).neighbors('Ticket', 't-1', 'BLOCKS', 'Project',
            properties=['title'])
        self.assertIn('(s:Ticket {entity_id:$source_id})-[:BLOCKS]->(n:Project)', graph.calls[0][0])
        self.assertEqual(graph.calls[0][1]['source_id'], 't-1')
        self.assertEqual(result['source'], {'label': 'Ticket', 'entity_id': 't-1'})
        self.assertEqual(result['relationship'], 'BLOCKS')
        self.assertEqual(result['records'][0]['entity_id'], 'p-1')

    def test_output_limit_fails_and_closes_without_returning_partial_result(self):
        graph = Graph([{'entity_id': 't-1', 'p0': 'x' * 200}])
        with self.assertRaises(ContextLimitError):
            ContextClient(graph).nodes('Ticket', ['t-1'], properties=['body'], max_bytes=100)
        self.assertTrue(graph.closed)

    def test_result_row_limit_defends_against_bad_adapters(self):
        graph = Graph([{'entity_id': 'a'}, {'entity_id': 'b'}])
        with self.assertRaises(ContextLimitError):
            ContextClient(graph).nodes('Ticket', ['a'], limit=1)
        self.assertTrue(graph.closed)

    def test_validation_prevents_unbounded_or_injected_requests(self):
        graph = Graph()
        context = ContextClient(graph)
        bad = [
            lambda: context.nodes('Ticket) RETURN 1', ['a']),
            lambda: context.nodes('Ticket', 'a'),
            lambda: context.nodes('Ticket', ['a'] * 101),
            lambda: context.nodes('Ticket', ['a' * 4097]),
            lambda: context.nodes('Ticket', ['a'], properties=['acl_allow']),
            lambda: context.nodes('Ticket', ['a'], properties=['title'] * 17),
            lambda: context.nodes('Ticket', ['a'], limit=True),
            lambda: context.nodes('Ticket', ['a'], limit=101),
            lambda: context.nodes('Ticket', ['a'], max_bytes=1048577),
            lambda: context.neighbors('Ticket', '', 'BLOCKS', 'Project'),
        ]
        for request in bad:
            with self.subTest(request=request):
                with self.assertRaises(ValueError):
                    request()
        self.assertEqual(graph.calls, [])

    def test_byte_budget_counts_utf8_and_the_envelope(self):
        graph = Graph([{'entity_id': 't', 'p0': 'a' * 50}])
        self.assertEqual(len(ContextClient(graph).nodes('Ticket', ['t'],
            properties=['body'], max_bytes=180)['records']), 1)
        graph = Graph([{'entity_id': 't', 'p0': 'é' * 50}])
        with self.assertRaises(ContextLimitError):
            ContextClient(graph).nodes('Ticket', ['t'], properties=['body'], max_bytes=180)
        self.assertTrue(graph.closed)
        with self.assertRaises(ContextLimitError):
            ContextClient(Graph()).nodes('Ticket', [], max_bytes=1)

    def test_text_search_is_scoped_parameterized_and_unranked(self):
        graph = Graph([{'entity_id': 't-1', 'p0': 'Delayed release'}])
        result = ContextClient(graph).search_text('Ticket', "release' OR true", field='body',
            entity_ids=['t-1'], properties=['body'])
        query, parameters, _ = graph.calls[0]
        self.assertIn('n.entity_id IN $ids', query)
        self.assertIn('toLower(n.body) CONTAINS toLower($text)', query)
        self.assertNotIn("release' OR true", query)
        self.assertEqual(parameters['text'], "release' OR true")
        self.assertEqual(parameters['ids'], ['t-1'])
        self.assertEqual(result['search'], {'field': 'body', 'mode': 'case_insensitive_substring'})
        self.assertNotIn('score', result['records'][0])

    def test_text_search_validates_scope_and_text_before_query(self):
        graph = Graph()
        context = ContextClient(graph)
        for text, field, ids in [('', 'body', ['a']), ('x' * 4097, 'body', ['a']),
                                  ('query', 'acl_allow', ['a']), ('query', 'body', 'a'),
                                  ('query', 'body', ['a'] * 101)]:
            with self.subTest(text=text[:10], field=field):
                with self.assertRaises(ValueError):
                    context.search_text('Ticket', text, field=field, entity_ids=ids)
        self.assertEqual(graph.calls, [])
        self.assertEqual(context.search_text('Ticket', 'query', field='body', entity_ids=[])['records'], [])
        self.assertEqual(graph.calls, [])

    def test_empty_seeds_do_not_query(self):
        graph = Graph()
        self.assertEqual(ContextClient(graph).nodes('Ticket', [])['records'], [])
        self.assertEqual(graph.calls, [])


if __name__ == '__main__':
    unittest.main()
