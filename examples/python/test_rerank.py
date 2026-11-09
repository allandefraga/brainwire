import math
import json
import re
import unittest
from contextlib import contextmanager
from brainwire.context import ContextLimitError
from brainwire import rerank_context


class Graph:
    database = 'company'
    def __init__(self, records=()):
        self.records = records
        self.calls = []
        self.closed = False

    @contextmanager
    def stream_cypher(self, query, parameters, *, fetch_size):
        self.calls.append((query, parameters))
        projection = re.findall(r'n\.(\w+) AS (p\d+)', query)
        rows = [{'entity_id': record['id'], **{alias: record.get(field) for field, alias in projection}}
                for record in self.records]
        try:
            yield iter(rows)
        finally:
            self.closed = True


def rank(graph, **options):
    return rerank_context(graph, 'Document', ['a', 'b'], 'release', [1., 0.],
        text_property='body', vector_property='embedding', **options)


class RerankTests(unittest.TestCase):
    def test_output_limit_counts_utf8_exactly(self):
        graph = Graph([{'id': 'a', 'body': 'release', 'embedding': [1., 0.],
                        'title': 'é🙂'}])
        expected = rank(graph, properties=['title'])
        size = len(json.dumps(expected, ensure_ascii=False, allow_nan=False).encode('utf-8'))
        self.assertEqual(rank(graph, properties=['title'], max_bytes=size), expected)
        with self.assertRaises(ContextLimitError):
            rank(graph, properties=['title'], max_bytes=size - 1)

    def test_one_authorized_read_fuses_explainable_rankings(self):
        graph = Graph([{'id':'a','body':'Release notes','embedding':[0.,1.],'title':'Notes'},
                       {'id':'b','body':'Other topic','embedding':[1.,0.],'title':'Other'}])
        result = rank(graph, properties=['title'])
        self.assertEqual(len(graph.calls), 1)
        self.assertEqual(graph.calls[0][1]['ids'], ['a','b'])
        self.assertEqual(result['method'], 'candidate_rrf_v1')
        a, b = result['records']
        self.assertEqual(a['entity_id'], 'a')
        self.assertEqual(a['matched_terms'], ['release'])
        self.assertEqual((a['lexical_rank'], a['vector_rank']), (1,2))
        self.assertAlmostEqual(a['score'], 1/61 + 1/62)
        self.assertIsNone(b['lexical_rank'])
        self.assertEqual(a['properties'], {'title':'Notes'})
        self.assertNotIn('embedding', a['properties'])
        self.assertTrue(graph.closed)

    def test_extreme_finite_values_have_stable_cosine_and_ties(self):
        graph = Graph([{'id':'b','body':'no match','embedding':[1e308,1e308]},
                       {'id':'a','body':'no match','embedding':[1e308,1e308]}])
        result = rerank_context(graph,'Document',['a','b'],'release',[1e308,1e308],
            text_property='body',vector_property='embedding')
        self.assertEqual([row['entity_id'] for row in result['records']], ['a','b'])
        self.assertAlmostEqual(result['records'][0]['cosine_similarity'], 1.)
        self.assertTrue(all(math.isfinite(row['score']) for row in result['records']))

    def test_bad_query_vectors_fail_before_reads(self):
        graph = Graph()
        for vector in [[], [True], [0.], [math.nan], [math.inf], [10**1000], [1.]*4097]:
            with self.subTest(vector_type=type(vector[0]).__name__ if vector else 'empty'):
                with self.assertRaises(ValueError):
                    rerank_context(graph,'Document',['a'],'release',vector,
                        text_property='body', vector_property='embedding')
        self.assertEqual(graph.calls, [])

    def test_invalid_visible_candidates_abort_without_partial_result(self):
        for body, vector in [('x',[1.]), ('x',[0.,0.]), ('x',[True,1.]),
                             ('x',[math.nan,1.]), (None,[1.,0.]), ('x',None)]:
            graph=Graph([{'id':'a','body':body,'embedding':vector}])
            with self.subTest(body=body, vector=vector):
                with self.assertRaises(ValueError):
                    rank(graph)
            self.assertTrue(graph.closed)

    def test_caps_precede_tokenization_and_scores(self):
        graph=Graph([{'id':'a','body':'x'*16385,'embedding':[1.,0.]}])
        with self.assertRaises(ValueError):
            rank(graph)
        graph=Graph([{'id':str(i),'body':'x'*16000,'embedding':[1.,0.]} for i in range(5)])
        with self.assertRaises(ContextLimitError):
            rank(graph)
        graph=Graph()
        with self.assertRaises(ValueError):
            rerank_context(graph,'Document',[],' '.join(str(i) for i in range(17)),[1.],
                text_property='body',vector_property='embedding')
        self.assertEqual(graph.calls, [])

    def test_aggregate_component_cap_boundary(self):
        records = [{'id':str(i),'body':'release','embedding':[1.] * 4096} for i in range(17)]
        def attempt(rows):
            return rerank_context(Graph(rows),'Document',[row['id'] for row in rows],
                'release',[1.] * 4096,text_property='body',vector_property='embedding')
        self.assertEqual(len(attempt(records[:16])['records']), 10)
        with self.assertRaises(ContextLimitError):
            attempt(records)

    def test_query_text_caps_fail_before_io(self):
        graph = Graph()
        for text in ['', '   ', 'x' * 1025, 'é' * 513]:
            with self.subTest(length=len(text)):
                with self.assertRaises(ValueError):
                    rerank_context(graph,'Document',['a'],text,[1.],
                        text_property='body',vector_property='embedding')
        self.assertEqual(graph.calls, [])

    def test_subnormal_and_opposite_vectors_have_finite_cosines(self):
        graph = Graph([{'id':'a','body':'release','embedding':[5e-324,0.]},
                       {'id':'b','body':'release','embedding':[-5e-324,0.]}])
        result = rerank_context(graph,'Document',['a','b'],'release',[5e-324,0.],
            text_property='body',vector_property='embedding')
        scores = {row['entity_id']:row['cosine_similarity'] for row in result['records']}
        self.assertEqual(scores, {'a':1.,'b':-1.})

    def test_empty_visible_candidates_and_output_cap(self):
        self.assertEqual(rank(Graph())['records'], [])
        graph=Graph([{'id':'a','body':'release','embedding':[1.,0.]}])
        with self.assertRaises(ContextLimitError):
            rank(graph,max_bytes=1)


if __name__ == '__main__':
    unittest.main()
