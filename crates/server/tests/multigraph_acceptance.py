"""Owned protected three-graph isolation, concurrent writes and restart acceptance.

All records, credentials, graph directories and processes are disposable.
This qualifies one server's named graphs; it does not exercise replication.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import platform
import socket
import subprocess
import sys
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))
from examples.python.create_service_tokens import create, write_private

GRAPHS = ("graph_a", "graph_b", "graph_c")
BATCH_ROWS = 250
UPSERT = """UNWIND $rows AS row
MERGE (c:Customer {entity_id: row.id})
SET c.email=row.email, c.active=row.active, c.tags=row.tags"""


def rows(graph, start, stop, overwritten):
    return [{"id":f"customer-{index:06}",
             "email":f"{graph}-{'updated' if overwritten else 'initial'}-{index:06}@example.test",
             "active":overwritten,"tags":[graph]} for index in range(start,stop)]


def self_check():
    first=rows("graph_a",0,2,False)
    updated=rows("graph_b",0,2,True)
    assert first[0]["id"]==updated[0]["id"]=="customer-000000"
    assert first[0]["email"]!=updated[0]["email"]
    assert len(rows("graph_a",0,BATCH_ROWS,True))==250
    assert first[0]["active"] is False and updated[0]["active"] is True
    return {"fixture_check":"PASS","runtime_exercised":False,"graphs":list(GRAPHS),"batch_rows":BATCH_ROWS}


def run(binary, row_count):
    from neo4j import GraphDatabase, __version__ as driver_version
    from neo4j.exceptions import ClientError, ServiceUnavailable, SessionExpired
    report={"binary_sha256":hashlib.sha256(binary.read_bytes()).hexdigest(),
            "driver":driver_version,"os":platform.platform(),"rows_per_graph":row_count,
            "batch_rows":BATCH_ROWS,"graphs":list(GRAPHS),"synthetic":True,"checks":[]}
    with tempfile.TemporaryDirectory(prefix="brainwire-multigraph-") as folder:
        path=Path(folder).resolve();path.chmod(0o700)
        credentials={};principals=[]
        for graph in GRAPHS:
            directory=create(path/graph,graph,include_observer=True)
            policy=json.loads((directory/"policy.json").read_text())
            for principal in policy["principals"]:
                role=principal["id"];principal["id"]=f"{graph}-{role}"
                client=json.loads((directory/(role+".json")).read_text())
                client["principal"]=principal["id"]
                # The helper's private client files stay private after renaming IDs.
                (directory/(role+".json")).write_text(json.dumps(client)+"\n")
                credentials[(graph,role)]=client
                principals.append(principal)
        write_private(path/"policy.json",{"principals":principals})
        with socket.socket() as reserved:
            reserved.bind(("127.0.0.1",0));address="%s:%s"%reserved.getsockname()
        env={key:value for key,value in os.environ.items() if not key.startswith("BRAINWIRE_")}
        env.update(BRAINWIRE_SERVER_DATA_DIR=str(path/"data"),BRAINWIRE_SERVER_LISTEN=address,
            BRAINWIRE_SERVER_DEFAULT_DATABASE=GRAPHS[0],BRAINWIRE_SERVER_AUTH_FILE=str(path/"policy.json"),
            BRAINWIRE_BOLT_AUTH_FILE=str(path/GRAPHS[0]/"health.json"),BRAINWIRE_MEMORY_CACHE="64MiB",
            BRAINWIRE_MEMORY_MEMTABLES="16MiB",BRAINWIRE_MEMORY_QUERY="64MiB",
            BRAINWIRE_SERVER_MAX_MESSAGE="1MiB",BRAINWIRE_SERVER_MAX_DECODED_MESSAGE="16MiB",
            BRAINWIRE_SERVER_IO_TIMEOUT_MS="300000")
        server=None;log=(path/"server.log").open("w")

        def cli(*argv,client=None):
            cli_env=env if client is None else dict(env,BRAINWIRE_BOLT_AUTH_FILE=str(path/client[0]/(client[1]+".json")))
            return subprocess.run([str(binary),*argv],cwd=path,env=cli_env,capture_output=True,text=True,check=True,timeout=30).stdout

        def start():
            nonlocal server
            began=time.monotonic()
            server=subprocess.Popen([str(binary),"serve"],cwd=path,env=env,stdout=log,stderr=log)
            for _ in range(150):
                assert server.poll() is None,"owned server exited before readiness"
                try:
                    cli("probe","--address",address,"--timeout-ms","100")
                    return time.monotonic()-began
                except subprocess.CalledProcessError:
                    time.sleep(.03)
            raise AssertionError("owned server readiness timeout")

        def stop(kill=False):
            nonlocal server
            if server is not None:
                if server.poll() is None:
                    server.kill() if kill else server.terminate()
                code=server.wait(timeout=5);server=None
                return code

        def connect(graph,role="reader",auth_override=False,auth=None):
            client=credentials[(graph,role)]
            return GraphDatabase.driver("bolt://"+address,
                auth=auth if auth_override else (client["principal"],client["credentials"]),
                connection_timeout=3,connection_acquisition_timeout=5,
                liveness_check_timeout=30,max_transaction_retry_time=0,max_connection_pool_size=2)

        def query(graph,statement,parameters=None,role="reader",principal_graph=None,auth_override=False,auth=None):
            with connect(principal_graph or graph,role,auth_override,auth) as driver:
                with driver.session(database=graph) as session:
                    return session.run(statement,parameters or {}).data()

        def denied(operation,code):
            try:
                operation()
            except ClientError as error:
                assert error.code==code,(error.code,code)
            else:
                raise AssertionError("restricted operation unexpectedly succeeded")

        def verify(graph):
            with connect(graph) as driver:
                with driver.session(database=graph) as session:
                    assert session.run("MATCH (c:Customer) RETURN count(*) AS n").single()["n"]==row_count
                    assert session.run("MATCH (c:Customer) WHERE c.active=true RETURN count(*) AS n").single()["n"]==row_count
                    for index in [0,row_count//2,row_count-1]:
                        expected=rows(graph,index,index+1,True)[0]
                        assert session.run("MATCH (c:Customer {entity_id:$id}) RETURN c.email AS email, c.tags AS tags",id=expected["id"]).data()==[{"email":expected["email"],"tags":[graph]}]
                        assert session.run("MATCH (c:Customer) WHERE c.email=$email RETURN c.entity_id AS id",email=expected["email"]).data()==[{"id":expected["id"]}]
                        initial=rows(graph,index,index+1,False)[0]["email"]
                        assert session.run("MATCH (c:Customer) WHERE c.email=$email RETURN c.entity_id AS id",email=initial).data()==[]
                    for other in GRAPHS:
                        if other!=graph:
                            foreign=rows(other,0,1,True)[0]["email"]
                            assert session.run("MATCH (c:Customer) WHERE c.email=$email RETURN c.entity_id AS id",email=foreign).data()==[]
                    assert session.run("MATCH (c:Customer {entity_id:'pending-only-a'}) RETURN c.entity_id AS id").data()==[]
                    assert session.run("MATCH (c:Customer) WHERE c.email='pending-only-a@example.test' RETURN c.entity_id AS id").data()==[]
                    return {"nodes":row_count,"all_active":True,"sample_point_and_scalar_index_rows":3,"foreign_graph_email_rows":0}

        done={graph:threading.Event() for graph in GRAPHS}
        barrier=threading.Barrier(6)
        abort=threading.Event()

        def writer(graph):
            batches=0
            try:
                with connect(graph,"writer") as driver:
                    with driver.session(database=graph) as session:
                        barrier.wait(timeout=10)
                        for overwritten in (False,True):
                            for offset in range(0,row_count,BATCH_ROWS):
                                if abort.is_set():raise AssertionError("another graph task failed")
                                values=rows(graph,offset,min(offset+BATCH_ROWS,row_count),overwritten)
                                session.run(UPSERT,rows=values).consume();batches+=1
                return {"acknowledged_batches":batches,"initial_rows":row_count,"overwritten_rows":row_count}
            except BaseException:
                abort.set();raise
            finally:
                done[graph].set()

        def reader(graph):
            reads=0
            try:
                with connect(graph) as driver:
                    with driver.session(database=graph) as session:
                        barrier.wait(timeout=10)
                        while not done[graph].is_set() or reads==0:
                            if abort.is_set():raise AssertionError("another graph task failed")
                            count=session.run("MATCH (c:Customer) RETURN count(*) AS n").single()["n"]
                            assert 0<=count<=row_count,count
                            point=session.run("MATCH (c:Customer {entity_id:'customer-000000'}) RETURN c.email AS email, c.tags AS tags").data()
                            assert point in [[],[{"email":rows(graph,0,1,False)[0]["email"],"tags":[graph]}],[{"email":rows(graph,0,1,True)[0]["email"],"tags":[graph]}]],point
                            for other in GRAPHS:
                                if other!=graph:
                                    foreign=rows(other,0,1,True)[0]["email"]
                                    assert session.run("MATCH (c:Customer) WHERE c.email=$email RETURN c.entity_id AS id",email=foreign).data()==[]
                            reads+=1;time.sleep(.01)
                return {"read_cycles":reads,"foreign_rows":0}
            except BaseException:
                abort.set();raise

        try:
            for graph in GRAPHS:
                cli("db","create",graph,"--ontology",str(ROOT/"examples/python/schema.json"))
            report["initial_ready_seconds"]=start()
            denied(lambda:query(GRAPHS[0],"RETURN 1 AS ok",auth_override=True,auth=None),"Neo.ClientError.Security.Unauthorized")
            wrong=(credentials[(GRAPHS[0],"reader")]["principal"],"0"*64)
            denied(lambda:query(GRAPHS[0],"RETURN 1 AS ok",auth_override=True,auth=wrong),"Neo.ClientError.Security.Unauthorized")
            for index,graph in enumerate(GRAPHS):
                other=GRAPHS[(index+1)%3]
                denied(lambda g=graph:query(g,"CREATE (c:Customer {entity_id:'denied-write'})"),"Neo.ClientError.Security.Forbidden")
                for role in ["reader","writer","observer"]:
                    denied(lambda g=graph,o=other,r=role:query(o,"RETURN 1 AS ok",role=r,principal_graph=g),"Neo.ClientError.Security.Forbidden")
            report["checks"].append("anonymous/wrong tokens denied; per-graph reader write and cross-graph reader/writer/observer denied")
            began=time.monotonic()
            with ThreadPoolExecutor(max_workers=6) as executor:
                writers={graph:executor.submit(writer,graph) for graph in GRAPHS}
                readers={graph:executor.submit(reader,graph) for graph in GRAPHS}
                report["concurrent_writers"]={graph:future.result(timeout=120) for graph,future in writers.items()}
                report["concurrent_readers"]={graph:future.result(timeout=120) for graph,future in readers.items()}
            report["concurrent_import_overwrite_seconds"]=time.monotonic()-began
            report["before_restart"]={graph:verify(graph) for graph in GRAPHS}
            report["checks"].append("three concurrent bounded writers and readers; overlapping IDs, graph-specific properties and scalar indexes stay isolated")
            memory=[]
            for graph in GRAPHS:
                sample=query(graph,"CALL db.memory() YIELD poolLimitBytes, poolReservedBytes, poolPeakBytes, poolRejections RETURN poolLimitBytes, poolReservedBytes, poolPeakBytes, poolRejections",role="observer")
                assert len(sample)==1,sample
                value=sample[0]
                assert all(type(item) is int and item>=0 for item in value.values()),value
                assert value["poolLimitBytes"]==64<<20 and value["poolReservedBytes"]>0 and value["poolPeakBytes"]>=value["poolReservedBytes"],value
                memory.append({"authorized_graph":graph,**value})
            assert len({(sample["poolLimitBytes"], sample["poolPeakBytes"], sample["poolRejections"]) for sample in memory})==1,memory
            report["shared_query_pool_samples"]=memory
            report["checks"].append("each explicitly scoped observer reads the same configured shared query pool; counters are reservations, not RSS")
            snapshots={}
            for graph in GRAPHS:
                ontology=cli("shell","--connect",address,"--graph",graph,"--snapshot","--width","120","--height","30",client=(graph,"reader"))
                indexes=cli("shell","--connect",address,"--graph",graph,"--view","indexes","--snapshot","--width","120","--height","30",client=(graph,"reader"))
                assert "ONTOLOGY" in ontology and "Customer" in ontology and graph in ontology
                assert "INDEXES" in indexes and "READY" in indexes and "email" in indexes and graph in indexes
                snapshots[graph]={"default_workspace_ontology_sha256":hashlib.sha256(ontology.encode()).hexdigest(),"indexes_sha256":hashlib.sha256(indexes.encode()).hexdigest()}
            report["tui_readonly_snapshots"]=snapshots
            report["checks"].append("per-graph native default workspace Ontology and standalone Indexes snapshots use each reader's private credentials")
            with connect(GRAPHS[0],"writer") as driver:
                with driver.session(database=GRAPHS[0]) as session:
                    transaction=session.begin_transaction()
                    try:
                        transaction.run("CREATE (c:Customer {entity_id:'pending-only-a',email:'pending-only-a@example.test',active:true})").consume()
                        transaction.run("MATCH (c:Customer {entity_id:'customer-000000'}) SET c.email='pending-overwrite-a@example.test'").consume()
                        assert transaction.run("MATCH (c:Customer {entity_id:'customer-000000'}) RETURN c.email AS email").single()["email"]=="pending-overwrite-a@example.test"
                        assert query(GRAPHS[0],"MATCH (c:Customer {entity_id:'customer-000000'}) RETURN c.email AS email")==[{"email":rows(GRAPHS[0],0,1,True)[0]["email"]}]
                        report["kill_exit_code"]=stop(kill=True)
                    finally:
                        try:transaction.close()
                        except (ServiceUnavailable,SessionExpired,OSError):pass
            report["restart_ready_seconds"]=start()
            report["after_restart"]={graph:verify(graph) for graph in GRAPHS}
            assert query(GRAPHS[0],"MATCH (c:Customer) WHERE c.email='pending-overwrite-a@example.test' RETURN c.entity_id AS id")==[]
            report["checks"].append("SIGKILL/restart preserves all acknowledged graph A/B/C rows and indexes; pending graph A CREATE/indexed overwrite disappear")
            stop()
            report["audits"]={graph:json.loads(cli("db","check",graph,"--json")) for graph in GRAPHS}
            report["offline_counts"]={graph:json.loads(cli("db","stats",graph,"--json")) for graph in GRAPHS}
            for graph in GRAPHS:
                assert report["audits"][graph]["status"]=="ok"
                assert report["offline_counts"][graph]["nodes"]==row_count and report["offline_counts"][graph]["edges"]==0
            report["result"]="PASS"
            report["scope"]="three named graphs on one protected native server; no replication, distributed failover, query latency SLO or aggregate RSS guarantee"
        finally:
            stop(kill=True);log.close()
    assert not path.exists()
    report["ownedResourcesRemoved"]=True
    report["childServerStopped"]=True
    report["privateCredentialsAndStorageRemoved"]=True
    return report


if __name__=="__main__":
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary",type=Path,default=ROOT/"target/release/brainwire")
    parser.add_argument("--rows-per-graph",type=int,default=10000)
    parser.add_argument("--self-check",action="store_true")
    args=parser.parse_args()
    if not 1<=args.rows_per_graph<=100000:parser.error("--rows-per-graph must be 1..100000")
    print(json.dumps(self_check() if args.self_check else run(args.binary.resolve(strict=True),args.rows_per_graph),indent=2))
