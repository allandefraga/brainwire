"""Isolated integration check: schema reads during Python ingestion, then storage audit."""

import concurrent.futures, json, os, socket, subprocess, tempfile, time
from pathlib import Path
from neo4j import GraphDatabase

root = Path(__file__).resolve().parents[4]
binary = str(root / "target/debug/brainwire")
query = "CALL db.schema() YIELD version, nodes, relationships, indexesReady RETURN version, nodes, relationships, indexesReady"
with tempfile.TemporaryDirectory(prefix="brainwire-connected-python-") as tmp:
    env = {k: v for k, v in os.environ.items() if not k.startswith("BRAINWIRE_")}
    env.update(
        BRAINWIRE_SERVER_DATA_DIR=tmp + "/data",
        BRAINWIRE_MEMORY_CACHE="64MiB",
        BRAINWIRE_MEMORY_MEMTABLES="16MiB",
    )

    def run(*args):
        return subprocess.run(
            [binary, *args],
            cwd=root,
            env=env,
            check=True,
            capture_output=True,
            text=True,
        )

    run(
        "db",
        "create",
        "tickets",
        "--ontology",
        str(root / "crates/core/tests/fixtures/graph_mapping.json"),
    )
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        address = "%s:%s" % s.getsockname()
    server = subprocess.Popen(
        [binary, "serve"],
        cwd=root,
        env=dict(env, BRAINWIRE_SERVER_LISTEN=address),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        for _ in range(100):
            if (
                subprocess.run(
                    [binary, "probe", "--address", address, "--timeout-ms", "100"],
                    env=env,
                    capture_output=True,
                ).returncode
                == 0
            ):
                break
            time.sleep(0.03)
        with GraphDatabase.driver("bolt://" + address, auth=None) as driver:

            def writer():
                for batch in range(10):
                    with driver.session(database="tickets") as session:
                        session.run(
                            "UNWIND $rows AS row MERGE (c:Customer {entity_id:row.id}) SET c.name=row.name",
                            rows=[
                                {
                                    "id": f"customer-{batch * 20 + i}",
                                    "name": "synthetic",
                                }
                                for i in range(20)
                            ],
                        ).consume()

            def reader():
                for _ in range(20):
                    with driver.session(database="tickets") as session:
                        row = session.run(query).single(strict=True).data()
                        assert [n["label"] for n in row["nodes"]] == [
                            "Customer",
                            "Company",
                            "Segment",
                        ]
                        assert (
                            len(row["relationships"]) == 3
                            and row["indexesReady"] is True
                        )
                        assert (
                            len(
                                [
                                    e
                                    for e in row["relationships"]
                                    if e["name"] == "IN_SEGMENT"
                                ]
                            )
                            == 2
                        )

            with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
                futures = [
                    pool.submit(writer),
                    pool.submit(reader),
                    pool.submit(reader),
                ]
                for future in futures:
                    future.result()
            with driver.session(database="tickets") as session:
                assert (
                    session.run("MATCH (c:Customer) RETURN count(*) AS n").single()["n"]
                    == 200
                )
            snap = run(
                "shell",
                "--connect",
                address,
                "--graph",
                "tickets",
                "--view",
                "ontology",
                "--snapshot",
            )
            assert "Customer" in snap.stdout and "SERVER SNAPSHOT" in snap.stdout
            print(
                json.dumps(
                    {
                        "driver": "neo4j 5.28.6",
                        "concurrent_schema_queries": 40,
                        "write_batches": 10,
                        "customer_count": 200,
                        "cli_connected_snapshot": "passed",
                        "result": "integration acceptance, not a throughput benchmark",
                    }
                )
            )
    finally:
        server.kill()
        server.wait(timeout=5)
    run("db", "check", "tickets", "--json")
    print("Post-shutdown db check passed; temporary data removed.")
