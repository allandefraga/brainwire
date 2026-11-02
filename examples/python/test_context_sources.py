import json
import unittest
from contextlib import contextmanager
from brainwire.context import ContextClient, ContextLimitError

class Graph:
    database = 'company'
    def __init__(self, rows=()): self.rows, self.calls, self.closed = rows, [], False
    @contextmanager
    def stream_cypher(self, query, parameters, *, fetch_size):
        self.calls.append((query, parameters))
        try: yield iter(self.rows)
        finally: self.closed = True

class SourceTests(unittest.TestCase):
    def test_evidence_uses_one_authorized_read_and_explicit_sources(self):
        graph=Graph([{'entity_id':'t-1','p0':'Blocked','p1':'ticket-42','p2':'source://tickets/42'}])
        result=ContextClient(graph).evidence('Ticket',['t-1'],properties=['title'],
            source_fields={'id':'source_id','url':'source_url'})
        self.assertEqual(len(graph.calls),1)
        self.assertEqual(result['records'][0]['properties'], {'title':'Blocked'})
        citation=result['records'][0]['citation_id']
        self.assertTrue(citation.startswith('bw:'))
        self.assertEqual(result['sources'],[{'citation_id':citation,
            'record':{'label':'Ticket','entity_id':'t-1'},
            'source':{'id':'ticket-42','url':'source://tickets/42'}}])
        self.assertTrue(graph.closed)
        self.assertEqual(result, ContextClient(Graph(graph.rows)).evidence('Ticket',['t-1'],
            properties=['title'],source_fields={'id':'source_id','url':'source_url'}))

    def test_citation_identity_scopes_graph_and_label_but_not_source_revision(self):
        graph = Graph([{'entity_id': 'same-id', 'p0': 'source://first'}])
        def read(label='Ticket'):
            return ContextClient(graph).evidence(label, ['same-id'],
                source_fields={'url': 'source_url'})
        first = read()
        citation = first['records'][0]['citation_id']
        graph.rows = [{'entity_id': 'same-id', 'p0': 'source://updated'}]
        updated = read()
        self.assertEqual(updated['records'][0]['citation_id'], citation)
        self.assertNotEqual(updated['sources'][0]['source'], first['sources'][0]['source'])
        self.assertNotEqual(read('Document')['records'][0]['citation_id'], citation)
        graph.database = 'another-company'
        self.assertNotEqual(read()['records'][0]['citation_id'], citation)

    def test_no_source_is_not_fabricated_and_hidden_records_are_absent(self):
        graph=Graph([{'entity_id':'visible','p0':None}])
        result=ContextClient(graph).evidence('Ticket',['visible','hidden'],source_fields={'url':'source_url'})
        self.assertIsNone(result['records'][0]['citation_id'])
        self.assertEqual(result['sources'],[])
        self.assertEqual([r['entity_id'] for r in result['records']],['visible'])
        self.assertEqual(graph.calls[0][1]['ids'],['visible','hidden'])

    def test_source_mapping_validation_precedes_io(self):
        graph=Graph()
        for mapping in ({}, {'unknown':'source_id'}, {'url':'acl_allow'}, {'url':'source) RETURN 1'},
                        {'url':'source_url','id':'source_url'}, [('url','source_url')]):
            with self.subTest(mapping=mapping), self.assertRaises(ValueError):
                ContextClient(graph).evidence('Ticket',['t'],source_fields=mapping)
        self.assertEqual(graph.calls,[])

    def test_source_values_and_final_envelope_are_bounded(self):
        for value in (['url'], 1, True, 'x'*4097):
            graph=Graph([{'entity_id':'t','p0':value}])
            with self.subTest(value=type(value)), self.assertRaises(ValueError):
                ContextClient(graph).evidence('Ticket',['t'],source_fields={'url':'source_url'})
            self.assertTrue(graph.closed)
        graph=Graph([{'entity_id':'t','p0':'é'*100}])
        with self.assertRaises(ContextLimitError):
            ContextClient(graph).evidence('Ticket',['t'],source_fields={'title':'source_title'}, max_bytes=300)
        self.assertTrue(graph.closed)
        result=ContextClient(Graph()).evidence('Ticket',[],source_fields={'id':'source_id'})
        self.assertEqual(result['records'],[])
        self.assertEqual(result['sources'],[])

if __name__=='__main__': unittest.main()
