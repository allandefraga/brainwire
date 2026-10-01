"""Manual Bolt driver acceptance across server restart.

Prepare a fresh database with the fixture ontology, then start Brainwire and
run `python mvp_demo.py bolt://127.0.0.1:7687 write`. Kill/restart the server
and run the same command with `read`. Requires neo4j Python driver 5.x or 6.x.
"""

import argparse
from neo4j import GraphDatabase


def write(driver, rows):
    with driver.session(database="client_memory") as session:
        tx = session.begin_transaction()
        tx.run("CREATE (c:Customer {entity_id: 'discarded'})").consume()
        tx.rollback()

        def create_graph(tx):
            tx.run(
                "CREATE (c:Customer {entity_id: $id, name: $name})",
                id="driver-1", name="Ana",
            ).consume()
            tx.run(
                "CREATE (c:Customer {entity_id: $id, name: $name})",
                id="driver-2", name="Ben",
            ).consume()
            tx.run(
                "CREATE (c:Company {entity_id: $id, name: $name})",
                id="company-1", name="Acme",
            ).consume()
            tx.run(
                "MATCH (c:Customer {entity_id: 'driver-1'}), "
                "(k:Company {entity_id: 'company-1'}) "
                "CREATE (c)-[:WORKS_AT]->(k)"
            ).consume()

        session.execute_write(create_graph)
        batch = [{"id": f"bulk-{i}", "name": f"Customer {i}"} for i in range(rows)]
        session.run(
            "UNWIND $rows AS row CREATE (c:Customer {entity_id: row.id, name: row.name})",
            rows=batch,
        ).consume()
    print(f"write: committed {rows + 2} customers and one company")


def read(driver, rows):
    with driver.session(database="client_memory") as session:
        count = session.run("MATCH (c:Customer) RETURN count(*) AS n").single()["n"]
        assert count == rows + 2, count
        assert session.run(
            "MATCH (c:Customer {entity_id: 'discarded'}) RETURN count(*) AS n"
        ).single()["n"] == 0
        node = session.run(
            "MATCH (c:Customer {entity_id: 'driver-1'}) RETURN c"
        ).single()["c"]
        assert node.labels == {"Customer"}, node.labels
        assert node["name"] == "Ana", node
        company = session.run(
            "MATCH (c:Customer {entity_id: 'driver-1'})-[:WORKS_AT]->(k:Company) "
            "RETURN k.name AS name"
        ).single()["name"]
        assert company == "Acme", company
    print("mvp demo: ok")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("uri")
    parser.add_argument("mode", choices=("write", "read"))
    parser.add_argument("--rows", type=int, default=10_000)
    args = parser.parse_args()
    with GraphDatabase.driver(args.uri, auth=None) as driver:
        driver.verify_connectivity()
        if args.mode == "write":
            write(driver, args.rows)
        else:
            read(driver, args.rows)
