"""Checks that the official Neo4j Python driver (5.x or 6.x) talks to brainwire-bolt-dev."""

import sys

from neo4j import GraphDatabase

uri = sys.argv[1] if len(sys.argv) > 1 else "bolt://127.0.0.1:7687"
with GraphDatabase.driver(uri, auth=("neo4j", "dev")) as driver:
    driver.verify_connectivity()
    records, _summary, keys = driver.execute_query("RETURN 1 AS x")
    assert keys == ["x"], keys
    assert records[0]["x"] == 1, records
    with driver.session() as session:
        assert session.run("RETURN 2 AS y").single()["y"] == 2
print("driver smoke: ok")
