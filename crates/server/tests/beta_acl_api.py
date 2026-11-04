"""Disposable protected native qualification: Python mutations, node ACL and TUI.

uv run --no-project --with neo4j==6.3.0 python crates/server/tests/beta_acl_api.py
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import secrets

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))
from neo4j import GraphDatabase, Query, __version__ as driver_version
from neo4j.exceptions import Neo4jError, ServiceUnavailable
from examples.python.client import GraphClient
from examples.python.checkpoint_backup import MANIFEST, copy_checkpoint, verify_copy
from examples.python.create_service_tokens import create, write_private


def run(binary, count):
    binary = Path(binary).resolve()
    if not 10 <= count <= 100000:
        raise ValueError("rows must be between 10 and 100000")
    with tempfile.TemporaryDirectory(prefix="brainwire-beta-acl-") as folder:
        folder = Path(folder)
        create(folder / "auth", "pages")
        policy = json.loads((folder / "auth/policy.json").read_text())
        for principal in policy["principals"]:
            if principal["id"] in ("writer","reader"):
                principal["graphs"]["restored"] = principal["graphs"]["pages"]
            if principal["id"] == "writer":
                principal["instance_admin"] = True
            if principal["id"] == "reader":
                principal["node_visibility"] = {"principals":["user:tenant:alice"]}
        delegated_token = secrets.token_hex(32)
        policy["principals"].append({
            "id": "delegated-reader",
            "token_sha256": hashlib.sha256(delegated_token.encode()).hexdigest(),
            "graphs": {"pages": "read"},
            "instance_admin": False,
            "visibility_delegate": True,
        })
        write_private(folder / "auth/delegated-reader.json", {
            "principal": "delegated-reader", "credentials": delegated_token,
        })
        (folder / "auth/policy.json").write_text(json.dumps(policy))
        schema = {"version":"1", "nodes":[{"label":"Page", "properties":["title"], "indexed_fields":["title"]}],
                  "edges":[{"type":"LINK","from":{"label":"Page"},"to":{"label":"Page"},"properties":["weight"]}]}
        (folder / "schema.json").write_text(json.dumps(schema))
        with socket.socket() as probe:
            probe.bind(("127.0.0.1",0))
            port = probe.getsockname()[1]
        env = {key:value for key,value in os.environ.items() if not key.startswith("BRAINWIRE_")}
        env.update(BRAINWIRE_SERVER_DATA_DIR=str(folder / "data"), BRAINWIRE_SERVER_DEFAULT_DATABASE="pages",
                   BRAINWIRE_SERVER_LISTEN=f"127.0.0.1:{port}", BRAINWIRE_SERVER_AUTH_FILE=str(folder / "auth/policy.json"),
                   BRAINWIRE_MEMORY_CACHE="64MiB", BRAINWIRE_MEMORY_MEMTABLES="16MiB", BRAINWIRE_MEMORY_QUERY="128MiB")
        subprocess.run([str(binary),"db","create","pages","--ontology",str(folder / "schema.json")],env=env,check=True,stdout=subprocess.DEVNULL)
        credentials = {}
        for role in ("writer","reader"):
            value = json.loads((folder / f"auth/{role}.json").read_text())
            credentials[role] = value["principal"], value["credentials"]
        elapsed = None
        for phase in ("initial","reopened","restored"):
            database = "restored" if phase == "restored" else "pages"
            with (folder / "server.log").open("a") as log:
                process = subprocess.Popen([str(binary),"serve"],env=env,stdout=log,stderr=log)
                try:
                    with GraphDatabase.driver(f"bolt://127.0.0.1:{port}",auth=credentials["writer"],connection_timeout=1,max_transaction_retry_time=0) as writer:
                        for attempt in range(100):
                            try:
                                writer.verify_connectivity()
                                break
                            except ServiceUnavailable:
                                if process.poll() is not None or attempt == 99:
                                    raise
                                time.sleep(0.05)
                        if phase == "initial":
                            graph = GraphClient(writer,"pages",batch_size=250)
                            start = time.monotonic()
                            assert graph.load_nodes("Page",({"entity_id":f"p-{i}","title":f"title-{i}","old":"remove-me","status":"open"} for i in range(count))) == count
                            elapsed = time.monotonic() - start
                            assert graph.update_node("Page","p-0",{"title":"changed","tags":["x","y"],"details":{"n":1}},remove=["old"])
                            assert not graph.update_node("Page","absent",{"title":"never-created"})
                            assert graph.upsert_edge(("Page","p-0"),"LINK",("Page","p-1"),{"weight":0,"old":"edge"})
                            assert graph.upsert_edge(("Page","p-0"),"LINK",("Page","p-2"),{"weight":1})
                            with writer.session(database="pages") as session:
                                assert session.run("CALL graph.project({relationship:'LINK',weightProperty:'weight'}) YIELD nonZeroCount RETURN nonZeroCount").single()[0] == 1
                            assert graph.update_edge(("Page","p-0"),"LINK",("Page","p-1"),{"weight":3},remove=["old"])
                            assert not graph.update_edge(("Page","p-1"),"LINK",("Page","absent"),{"weight":3})
                            with writer.session(database="pages") as session:
                                assert session.run("CALL graph.project({relationship:'LINK',weightProperty:'weight'}) YIELD nonZeroCount RETURN nonZeroCount").single()[0] == 2
                                for entity in ("p-0","p-1"):
                                    assert graph.set_node_acl("Page",entity,{"allow":["user:tenant:alice"]})
                                assert session.run("CALL db.setNodeAcl({label:'Page',entityId:'p-2',allow:['user:tenant:alice'],deny:['user:tenant:alice']}) YIELD updated RETURN updated").single()[0]
                                try:
                                    session.execute_read(lambda tx: tx.run("CALL db.setNodeAcl({label:'Page',entityId:'p-2',allow:['user:tenant:alice']}) YIELD updated RETURN updated").consume())
                                except Neo4jError as error:
                                    assert error.code == "Neo.ClientError.Security.Forbidden"
                                else:
                                    raise AssertionError("READ transaction allowed ACL mutation")
                        with writer.session(database=database) as session:
                            assert session.run("MATCH (p:Page) RETURN count(*)").single()[0] == count
                            node = session.run("MATCH (p:Page {entity_id:'p-0'}) RETURN p").single()[0]
                            assert node["title"] == "changed" and "old" not in node and node["status"] == "open"
                            assert node["tags"] == ["x","y"] and node["details"] == {"n":1}
                            assert session.run("MATCH (p:Page {title:'title-0'}) RETURN count(*)").single()[0] == 0
                            assert session.run("MATCH (p:Page {title:'changed'}) RETURN count(*)").single()[0] == 1
                    with GraphDatabase.driver(f"bolt://127.0.0.1:{port}",auth=credentials["reader"],max_transaction_retry_time=0) as reader:
                        with reader.session(database=database) as session:
                            assert session.run("MATCH (p:Page) RETURN count(*)").single()[0] == 2
                            assert session.run("MATCH (p:Page {entity_id:'p-2'}) RETURN count(*)").single()[0] == 0
                            assert session.run("MATCH (p:Page {entity_id:'p-0'})-[:LINK]->(q:Page) RETURN q.entity_id").single()[0] == "p-1"
                            assert session.run("CALL graph.wcc({relationship:'LINK'}) YIELD node,size RETURN node.entity_id,size").values() == [["p-0",2],["p-1",2]]
                            assert session.run("CALL graph.scc({relationship:'LINK'}) YIELD node,size RETURN node.entity_id,size").values() == [["p-0",1],["p-1",1]]
                            for query in ("CALL graph.project() YIELD scannedRelationshipCount RETURN scannedRelationshipCount", "CALL db.setNodeAcl({label:'Page',entityId:'p-2',allow:['user:tenant:alice']}) YIELD updated RETURN updated", "MERGE (p:Page {entity_id:'reader-write'}) RETURN p"):
                                try:
                                    session.run(query).consume()
                                except Neo4jError:
                                    pass
                                else:
                                    raise AssertionError("restricted query unexpectedly succeeded")
                        if phase == "initial":
                            delegated_auth = ("delegated-reader", delegated_token)
                            claims = {"principals": ["user:tenant:alice"]}
                            with GraphDatabase.driver(f"bolt://127.0.0.1:{port}",auth=delegated_auth,max_transaction_retry_time=0) as delegated:
                                with delegated.session(database=database) as session:
                                    try:
                                        session.run("MATCH (p:Page) RETURN count(*)").consume()
                                    except Neo4jError as error:
                                        assert error.code == "Neo.ClientError.Security.Forbidden"
                                    else:
                                        raise AssertionError("delegated autocommit read without metadata was allowed")
                                with delegated.session(database=database) as session:
                                    result = session.run(Query(
                                        "MATCH (p:Page) RETURN count(*)",
                                        metadata={"brainwire_visibility": claims},
                                    ))
                                    assert result.single()[0] == 2
                                with delegated.session(database=database) as session:
                                    try:
                                        with session.begin_transaction() as tx:
                                            tx.run("MATCH (p:Page) RETURN count(*)").consume()
                                    except Neo4jError as error:
                                        assert error.code == "Neo.ClientError.Security.Forbidden"
                                    else:
                                        raise AssertionError("delegated explicit read without metadata was allowed")
                                with delegated.session(database=database) as session:
                                    with session.begin_transaction(
                                        metadata={"brainwire_visibility": claims}
                                    ) as tx:
                                        assert tx.run("MATCH (p:Page) RETURN count(*)").single()[0] == 2
                    snapshot_env = dict(env,BRAINWIRE_BOLT_AUTH_FILE=str(folder / "auth/reader.json"))
                    frame = subprocess.run([str(binary),"shell","--connect",f"127.0.0.1:{port}","--graph",database,"--view","graph","--node-label","Page","--entity-id","p-0","--edge-type","LINK","--target-label","Page","--snapshot","--width","120","--height","30"],env=snapshot_env,check=True,text=True,capture_output=True,timeout=30).stdout
                    assert "p-1" in frame and "p-2" not in frame
                finally:
                    process.terminate()
                    try:
                        process.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=10)
            if phase == "reopened":
                checkpoint_path = folder / "checkpoint"
                checkpoint = json.loads(subprocess.run([str(binary),"db","checkpoint","pages","--destination",str(checkpoint_path)],env=env,check=True,text=True,capture_output=True).stdout)
                assert checkpoint["status"] == "checkpoint_created"
                copy_path = folder / "independent-copy"
                copy_checkpoint(checkpoint_path, copy_path, {"name": checkpoint_path.name})
                assert verify_copy(copy_path) == []
                shutil.copytree(copy_path, folder / "data/restored")
                (folder / "data/restored" / MANIFEST).unlink()
                restored_audit = json.loads(subprocess.run([str(binary),"db","check","restored","--json"],env=env,check=True,text=True,capture_output=True).stdout)
                assert restored_audit["status"] == "ok"
        audit = json.loads(subprocess.run([str(binary),"db","check","pages","--json"],env=env,check=True,text=True,capture_output=True).stdout)
        assert audit["status"] == "ok"
        return {"status":"PASS","nodes":count,"nodeLoadSeconds":elapsed,"nodeLoadRowsPerSecond":count/elapsed,
                "serverSha256":hashlib.sha256(binary.read_bytes()).hexdigest(),"driver":f"neo4j=={driver_version}",
                "checks":["bounded Python batches","node partial update/remove","edge upsert/update/remove","index maintenance","private ACL stamp","ACL count/PK/traversal/WCC/SCC","restricted mutation denial","hidden scan count denial","delegated autocommit and explicit transaction metadata required and enforced","TUI filtered graph","restart","checkpoint copy, manifest verification and private ACL restore","offline audit"],
                "scope":"synthetic protected single-node localhost; no live-source or replication claim"}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary",default=str(ROOT / "target/release/brainwire"))
    parser.add_argument("--rows",type=int,default=1000)
    args = parser.parse_args()
    print(json.dumps(run(args.binary,args.rows),indent=2))
