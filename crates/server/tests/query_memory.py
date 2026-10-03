"""Concurrent Python/Bolt acceptance for streaming reads and shared reservations.

Uses temporary synthetic graphs, a 256 KiB query budget and the installed Neo4j
5.x/6.x driver. No server, database or listening port is left behind.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time

from neo4j import GraphDatabase
from neo4j.exceptions import Neo4jError

MEMORY = "CALL db.memory() YIELD poolLimitBytes,poolReservedBytes,poolPeakBytes,poolRejections RETURN poolLimitBytes,poolReservedBytes,poolPeakBytes,poolRejections"


def mixed_workload(driver):
    """Short concurrent reads/overwrites, disjoint writer keys and indexed values."""
    barrier = threading.Barrier(8, timeout=30)
    def worker(index):
        timings = []
        with driver.session(database="first") as session:
            barrier.wait()
            for operation in range(100):
                started = time.perf_counter()
                if index < 4:
                    # Each writer owns one node. Reusing the index value exercises
                    # removal/reinsertion without intentional lost-update races.
                    value = f"writer-{index}-{operation % 2}"
                    session.run("MATCH (p:Page {entity_id:$id}) SET p.text=$text",
                                id=f"p-{index}", text=value).consume()
                else:
                    identifier = f"p-{(operation * 17 + index) % 2000}"
                    row = session.run("MATCH (p:Page {entity_id:$id}) RETURN p.entity_id AS id",
                                      id=identifier).single()
                    assert row["id"] == identifier
                timings.append((time.perf_counter() - started) * 1000)
        return ("write" if index < 4 else "read"), timings
    started = time.perf_counter()
    with ThreadPoolExecutor(max_workers=8) as executor:
        runs = list(executor.map(worker, range(8)))
    elapsed = time.perf_counter() - started
    measurements = {}
    for kind in ("read", "write"):
        values = sorted(value for label, values in runs if label == kind for value in values)
        measurements[kind] = {"operations": len(values), "p50Ms": values[len(values)//2],
                              "p95Ms": values[(len(values)*95+99)//100-1]}
    with driver.session(database="first") as session:
        for index in range(4):
            current = f"writer-{index}-1"
            stale = f"writer-{index}-0"
            row = session.run("MATCH (p:Page) WHERE p.text=$text RETURN p.entity_id AS id", text=current).single()
            assert row["id"] == f"p-{index}"
            assert session.run("MATCH (p:Page) WHERE p.text=$text RETURN count(*) AS n", text=stale).single()["n"] == 0
        assert session.run("MATCH (p:Page) WHERE p.text='same' RETURN count(*) AS n").single()["n"] == 1996
        assert session.run("MATCH (p:Page) RETURN count(*) AS n").single()["n"] == 2000
    return {"connections": 8, "operations": 800, "seconds": elapsed,
            "operationsPerSecond": 800/elapsed, "latency": measurements,
            "indexOverwriteCheck": "PASS", "scope": "native debug Bolt; 2K nodes; disjoint writer keys"}


def hot_key_conflicts(driver):
    """Four snapshots stage distinct index replacements of one existing node."""
    barrier = threading.Barrier(4, timeout=30)
    def worker(index):
        with driver.session(database="first") as session:
            with session.begin_transaction() as tx:
                tx.run("MATCH (p:Page {entity_id:'p-10'}) SET p.text=$value", value=f"hot-{index}").consume()
                barrier.wait()
                try:
                    tx.commit()
                    return index, "committed"
                except Neo4jError as error:
                    assert error.code == "Neo.TransientError.Transaction.Outdated", error
                    assert error.is_retryable(), error
                    return index, "conflict"
    with ThreadPoolExecutor(max_workers=4) as executor:
        outcomes = list(executor.map(worker, range(4)))
    winners = [index for index, outcome in outcomes if outcome == "committed"]
    assert len(winners) == 1, outcomes
    with driver.session(database="first") as session:
        for index, outcome in outcomes:
            rows = list(session.run("MATCH (p:Page) WHERE p.text=$value RETURN p.entity_id AS id", value=f"hot-{index}"))
            assert [row["id"] for row in rows] == (["p-10"] if outcome == "committed" else [])
        assert session.run("MATCH (p:Page) WHERE p.text='same' RETURN count(*) AS n").single()["n"] == 1995
        assert session.run("MATCH (p:Page) RETURN count(*) AS n").single()["n"] == 2000
    return {"writers": 4, "committed": 1, "retryableConflicts": 3, "indexCheck": "PASS"}


def run(binary):
    with tempfile.TemporaryDirectory(prefix="brainwire-query-memory-") as temporary:
        root = Path(temporary)
        env = {k: v for k, v in os.environ.items() if not k.startswith("BRAINWIRE_")}
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        config = root / "brainwire.toml"
        config.write_text(f'''[server]
listen = "127.0.0.1:{port}"
data_dir = "{root / 'data'}"
default_database = "first"
query_timeout_ms = 1000
[memory]
cache = "64MiB"
memtables = "4MiB"
query = "256KiB"
''')
        ontology = root / "schema.json"
        ontology.write_text(json.dumps({"version":"1", "nodes":[{"label":"Page", "properties":["text"], "indexed_fields":["text"]}]}))
        for name in ("first", "second"):
            subprocess.run([str(binary),"db","create",name,"--ontology",str(ontology),"--config",str(config)],env=env,check=True,capture_output=True)
        with (root / "server.log").open("w") as log:
            process = subprocess.Popen([str(binary),"serve","--config",str(config)],env=env,stdout=log,stderr=log)
            try:
                for _ in range(200):
                    if process.poll() is not None:
                        raise RuntimeError((root / "server.log").read_text())
                    try:
                        with socket.create_connection(("127.0.0.1",port),timeout=.1):
                            break
                    except OSError:
                        time.sleep(.025)
                else:
                    raise RuntimeError("server failed to start")
                with GraphDatabase.driver(f"bolt://127.0.0.1:{port}",auth=None,max_connection_pool_size=20,max_transaction_retry_time=0) as driver:
                    with driver.session(database="first") as session:
                        for first in range(0,2000,25):
                            session.run("UNWIND $rows AS id CREATE (p:Page {entity_id:id,text:'same'})",rows=[f"p-{i}" for i in range(first,first+25)]).consume()
                        assert session.run("MATCH (p:Page) RETURN count(*) AS n").single()["n"] == 2000
                        assert len(list(session.run("MATCH (p:Page) WHERE p.text='same' RETURN p LIMIT 1"))) == 1
                        assert session.run("UNWIND $v AS a UNWIND $v AS b RETURN count(*) AS n",v=list(range(100))).single()["n"] == 10000
                        assert len(list(session.run("UNWIND $v AS a UNWIND $v AS b UNWIND $v AS c RETURN a,b,c LIMIT 1",v=list(range(100))))) == 1
                    with driver.session(database="first") as session:
                        try:
                            with session.begin_transaction() as tx:
                                tx.run("CREATE (p:Page {entity_id:'timeout-pending',text:'timeout-pending'})").consume()
                                tx.run("UNWIND $v AS a UNWIND $v AS b UNWIND $v AS c UNWIND $v AS d RETURN count(*)",v=list(range(200))).consume()
                        except Neo4jError as error:
                            assert error.code == "Neo.ClientError.Statement.ExecutionTimedOut",error
                        else:
                            raise AssertionError("low-memory fanout must hit its cooperative deadline")
                        assert session.run("MATCH (p:Page) WHERE p.text='timeout-pending' RETURN count(*) AS n").single()["n"] == 0
                    barrier = threading.Barrier(17,timeout=30)
                    outcomes = []
                    lock = threading.Lock()
                    def worker(index):
                        with driver.session(database="first" if index%2==0 else "second") as session:
                            tx = session.begin_transaction()
                            barrier.wait()
                            try:
                                tx.run("CREATE (p:Page {entity_id:$id,text:'pending'})",id=f"pending-{index}").consume()
                                tx.run("RETURN $text",text="x"*16000).consume()
                                outcome = "held"
                            except Neo4jError as error:
                                assert error.code == "Neo.TransientError.General.QueryMemoryPoolExhausted",error
                                assert error.is_retryable(),error
                                outcome = "rejected"
                            finally:
                                with lock:
                                    outcomes.append(locals().get("outcome","failed"))
                                barrier.wait()
                                barrier.wait()
                                tx.close()
                    with ThreadPoolExecutor(max_workers=16) as executor:
                        futures = [executor.submit(worker,i) for i in range(16)]
                        barrier.wait()
                        barrier.wait()
                        try:
                            assert "held" in outcomes and "rejected" in outcomes,outcomes
                            with driver.session(database="second") as session:
                                try:
                                    during = dict(session.run(MEMORY).single())
                                except Neo4jError as error:
                                    # Telemetry is an ordinary query and also respects a full pool.
                                    assert error.code == "Neo.TransientError.General.QueryMemoryPoolExhausted",error
                                    during = {"status":"pool_exhausted"}
                                else:
                                    assert 0 < during["poolReservedBytes"] <= 256*1024
                                    assert during["poolPeakBytes"] <= 256*1024
                        finally:
                            barrier.wait()
                        for future in futures:
                            future.result()
                    for name, expected in (("first",2000),("second",0)):
                        with driver.session(database=name) as session:
                            assert session.run("MATCH (p:Page) RETURN count(*) AS n").single()["n"] == expected
                            assert session.run("MATCH (p:Page) WHERE p.text='pending' RETURN count(*) AS n").single()["n"] == 0
                    with driver.session(database="second") as session:
                        after = dict(session.run(MEMORY).single())
                        assert after["poolReservedBytes"] < 8192,after # This observer query has its own reservation.
                        assert after["poolPeakBytes"] <= 256*1024
                        assert after["poolRejections"] >= outcomes.count("rejected")
                    mixed = mixed_workload(driver)
                    hot = hot_key_conflicts(driver)
                    print(json.dumps({"hotKey":hot,"mixedWorkload":mixed,"result":"PASS","rows":2000,"concurrentConnections":16,"outcomes":outcomes,"during":during,"after":after,"deadlineRollback":"PASS","scope":"synthetic native Bolt; shared accounting, not RSS"},indent=2))
            finally:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary",type=Path,default=Path("target/debug/brainwire"))
    run(parser.parse_args().binary.resolve())
