"""Pure tests; no Docker, real graph or network."""
import argparse
import unittest
from scripts import bench_v09_container as bench
from scripts.test_bench_v08_container import raw_sample


class FourLabelTests(unittest.TestCase):
    def test_all_batches_cover_each_identity_exactly_once(self):
        for rows,size,workers in [(100,7,8),(1003,31,3)]:
            values=[i for worker in range(workers) for indices in bench.batches(rows,size,worker,workers) for i in indices]
            self.assertEqual(sorted(values),list(range(rows)))
            self.assertEqual(len(set(values)),rows)

    def test_fixture_matches_four_label_edge_contract(self):
        schema=bench.ontology()
        self.assertEqual([n['label'] for n in schema['nodes']],list(bench.LABELS))
        self.assertEqual(len(schema['edges']),4)
        for label in bench.LABELS:
            for i in [0,1,999999]:
                value=bench.row(label,i)
                self.assertEqual(set(value),{'id',*bench.PROPS[label]})
                self.assertEqual(value['id'],bench.identifier(label,i))
                self.assertEqual(value['usuario_id'],i)
        self.assertEqual(bench.row('Endereco',5)['uf_teste'],'BA')
        for edge,source,target in bench.EDGES:
            query=bench.edge_query(edge,source,target)
            self.assertIn(f':{source}',query);self.assertIn(f':{target}',query);self.assertIn(f':{edge}',query)

    def test_reader_roles_have_distinct_queries_and_parameters(self):
        results=[bench.reader_query(role,'usr-00000001',10000,200) for role in ['scan','aggregation','neighborhood','cursor']]
        self.assertEqual(len({q for q,p in results}),4)
        self.assertEqual(results[2][1],{'width':10000})
        self.assertEqual(results[3][1],{'after':'usr-00000001'})
        self.assertIn('LIMIT 200',results[3][0])

    def test_cap_validation_is_parameterized_and_oom_is_failure(self):
        sample=bench.base.parse_sample(raw_sample())
        bench.validate_sample(sample,4*1024**3)
        with self.assertRaises(AssertionError):bench.validate_sample(sample,2*1024**3)
        sample['events']['oom_kill']=1
        with self.assertRaisesRegex(AssertionError,'OOM'):bench.validate_sample(sample,4*1024**3)

    def test_oom_evidence_does_not_trust_false_docker_flag(self):
        state={'state':{'OOMKilled':False},'restart_count':0}
        result=bench.oom_evidence([],state,[{'Action':'oom'}])
        self.assertTrue(result['observed']);self.assertFalse(result['docker_oom_killed'])
        self.assertFalse(result['kernel_log_verified'])
        sample=bench.base.parse_sample(raw_sample());sample['events']['oom']=1
        self.assertTrue(bench.oom_evidence([sample],state,[])['observed'])

    def test_default_contract_and_invalid_values(self):
        parser=bench.parser()
        args=parser.parse_args(['--image','synthetic','--output','/private/tmp/new','--memory-gib','4'])
        bench.validate_args(args)
        self.assertEqual((args.rows,args.writers,args.page_size,args.soak_seconds),(1000000,8,200,1800))
        for field,value in [('soak_seconds',float('nan')),('reader_pause',0),('rows',99),('page_size',0),('writers',0)]:
            changed=argparse.Namespace(**vars(args));setattr(changed,field,value)
            with self.assertRaises(ValueError):bench.validate_args(changed)



class ReaderChecks(unittest.TestCase):
    def test_reader_rejects_duplicate_and_mismatched_rows(self):
        cases=[('scan',[{'id':1},{'id':1}]),('aggregation',[{'brand':'wrong','count':1}]),
               ('cursor',[{'id':'usr-00000001'},{'id':'usr-00000000'}]),
               ('neighborhood',[{'u':{'usuario_id':1,'entity_id':'usr-00000001'},'c':{'usuario_id':2,'entity_id':'car-00000002'}}])]
        for role,values in cases:
            with self.subTest(role=role),self.assertRaises(AssertionError):bench.verify_reader_rows(role,values,'',100,10)

    def test_read_transaction_uses_delegation_and_verifies_same_snapshot(self):
        calls=[]
        class Result:
            def __init__(self,value):self.value=value
            def data(self):return self.value
        class Transaction:
            def __enter__(self):return self
            def __exit__(self,*args):pass
            def run(self,text,params):
                calls.append(text)
                return Result([{'id':1},{'id':2}]) if 'AS id' in text else Result([{'count':2}])
            def commit(self):calls.append('commit')
        class Session:
            def begin_transaction(self,**kwargs):
                self.options=kwargs;return Transaction()
        args=argparse.Namespace(stamp_acl=True,query_timeout=30,rows=100,neighborhood_rows=10,page_size=200)
        session=Session();values=bench.read_operation(session,'scan','',args)
        self.assertEqual(values,[{'id':1},{'id':2}])
        self.assertEqual(session.options['metadata'],{'brainwire_visibility':bench.VISIBILITY})
        self.assertEqual(len(calls),3);self.assertEqual(calls[-1],'commit')


class AuthTests(unittest.TestCase):
    def test_writer_and_reader_have_distinct_nondelegating_write_and_read_permissions(self):
        for delegated in (False,True):
            policy=bench.auth_policy('writer-secret','reader-secret',delegated)['principals']
            writer,reader=policy
            self.assertEqual(writer['id'],'bench');self.assertNotIn('visibility_delegate',writer)
            self.assertEqual(writer['graphs'],{'bench':'write'})
            self.assertEqual(reader['graphs'],{'bench':'read'})
            self.assertEqual(reader['visibility_delegate'],delegated)
            self.assertFalse(reader['instance_admin'])
            self.assertNotEqual(writer['token_sha256'],reader['token_sha256'])

    def test_separate_driver_credentials_and_pools(self):
        from unittest.mock import Mock,patch
        factory=Mock(side_effect=['writer','reader'])
        inspect=[{'NetworkSettings':{'Ports':{'7687/tcp':[{'HostPort':'43123'}]}}}]
        with patch.object(bench.base,'docker',return_value=bench.json.dumps(inspect)):
            writer,reader,uri=bench.connect_drivers(factory,'synthetic','writer-secret','reader-secret',8)
        self.assertEqual((writer,reader,uri),('writer','reader','bolt://127.0.0.1:43123'))
        self.assertEqual(factory.call_args_list[0].kwargs['auth'],('bench','writer-secret'))
        self.assertEqual(factory.call_args_list[1].kwargs['auth'],('bench-read','reader-secret'))

    def test_readiness_ordinary_transaction_does_not_request_delegation(self):
        from unittest.mock import MagicMock
        session=MagicMock();tx=session.begin_transaction.return_value.__enter__.return_value
        tx.run.return_value.data.return_value=[{'ready':1}]
        self.assertEqual(bench.execute_query(session,'RETURN 1 AS ready',None,30),[{'ready':1}])
        session.begin_transaction.assert_called_once_with(timeout=30)
        tx.commit.assert_called_once_with()

if __name__=='__main__':unittest.main()
