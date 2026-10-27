"""Bolt read benchmark: the baseline queries against any Bolt server (Brainwire or Neo4j).

Expects the dataset built by `cargo bench -p brainwire-cypher --bench baseline -- load`
(labels Usuario, Carro, Endereco, Casa; 1M nodes each). Runs each query once to warm
up, then reports the median of three wall-clock timings measured in the driver,
including Bolt transfer. A cursor walk pages through Usuario in pages of 1000.

    python crates/server/tests/bolt_read_bench.py --uri bolt://127.0.0.1:7687 --database bench
"""
import argparse
import json
import statistics
import time

from neo4j import GraphDatabase

QUERIES = [
    ("count_pairs", "MATCH (u:Usuario)-[:TEM_CARRO]->(c:Carro) RETURN count(*) AS pairs", {}),
    ("top_brands", "MATCH (:Usuario)-[:TEM_CARRO]->(c:Carro) RETURN c.marca_teste AS marca, count(*) AS n "
                   "ORDER BY n DESC, marca LIMIT 5", {}),
    ("houses_per_state", "MATCH (c:Casa)-[:CASA_FICA_EM]->(e:Endereco) RETURN e.uf_teste AS uf, count(c) AS casas "
                         "ORDER BY casas DESC, uf LIMIT 5", {}),
    ("indexed_lookup", "MATCH (u:Usuario) WHERE u.email_teste = 'usuario424242@example.test' RETURN u.usuario_id", {}),
    ("in_100_unindexed", "MATCH (u:Usuario) WHERE u.usuario_id IN $ids RETURN count(u)",
     {"ids": list(range(424200, 424300))}),
    ("in_100_entity_ids", "MATCH (u:Usuario) WHERE u.entity_id IN $ids RETURN count(u)",
     {"ids": [f"usr-{i:08d}" for i in range(424200, 424300)]}),
    ("one_hop_unindexed", "MATCH (u:Usuario {usuario_id: 424242})-[:TEM_CARRO]->(c:Carro) RETURN c.carro_id", {}),
    ("count_label", "MATCH (u:Usuario) RETURN count(u)", {}),
    ("two_hops", "MATCH (u:Usuario {entity_id: 'usr-00424242'})-[:TEM_CASA]->(:Casa)-[:CASA_FICA_EM]->(e:Endereco) "
                 "RETURN e.uf_teste", {}),
]


BRANDS, STATES = ["MarcaA", "MarcaB", "MarcaC", "MarcaD"], ["SP", "RJ", "MG", "RS", "PR", "BA"]


def node_rows(label, start, end):
    for i in range(start, end):
        if label == "Usuario":
            yield {"id": f"usr-{i:08d}", "usuario_id": i, "cpf_teste": f"{i:011d}", "nome_teste": f"Nome {i}",
                   "email_teste": f"usuario{i}@example.test", "updated_at": "2026-10-01T00:00:00Z"}
        elif label == "Carro":
            yield {"id": f"car-{i:08d}", "carro_id": i, "usuario_id": i, "codigo_teste": f"CAR{i}",
                   "marca_teste": BRANDS[i % 4], "ano": 2000 + i % 25, "updated_at": "2026-10-01T00:00:00Z"}
        elif label == "Endereco":
            yield {"id": f"end-{i:08d}", "endereco_id": i, "usuario_id": i, "logradouro_teste": f"Rua {i}",
                   "numero_teste": i % 1000, "bairro_teste": f"Bairro {i % 500}", "cidade_teste": f"Cidade {i % 100}",
                   "uf_teste": STATES[i % 6]}
        else:
            yield {"id": f"cas-{i:08d}", "casa_id": i, "usuario_id": i, "endereco_id": i, "codigo_teste": f"CASA{i}",
                   "tipo_teste": ["casa", "apto", "sobrado"][i % 3], "area_m2": 40 + i % 300,
                   "updated_at": "2026-10-01T00:00:00Z"}


def load(session, rows, batch=10000):
    """Load the benchmark dataset into an empty server (used for Neo4j)."""
    indexes = {"Usuario": ["entity_id", "cpf_teste", "email_teste"], "Carro": ["entity_id", "marca_teste", "ano"],
               "Endereco": ["entity_id", "cidade_teste", "uf_teste"], "Casa": ["entity_id", "tipo_teste", "area_m2"]}
    for label, fields in indexes.items():
        for field in fields:
            session.run(f"CREATE INDEX IF NOT EXISTS FOR (n:{label}) ON (n.{field})").consume()
    session.run("CALL db.awaitIndexes()").consume()
    for label in indexes:
        for start in range(0, rows, batch):
            chunk = list(node_rows(label, start, min(rows, start + batch)))
            props = ", ".join(f"n.{k} = r.{k}" for k in chunk[0] if k != "id")
            session.run(f"UNWIND $rows AS r CREATE (n:{label} {{entity_id: r.id}}) SET {props}", rows=chunk).consume()
    edges = [("TEM_CARRO", "Usuario", "usr", "Carro", "car"), ("TEM_ENDERECO", "Usuario", "usr", "Endereco", "end"),
             ("TEM_CASA", "Usuario", "usr", "Casa", "cas"), ("CASA_FICA_EM", "Casa", "cas", "Endereco", "end")]
    for edge, a, ap, b, bp in edges:
        for start in range(0, rows, batch):
            chunk = [{"s": f"{ap}-{i:08d}", "t": f"{bp}-{i:08d}"} for i in range(start, min(rows, start + batch))]
            session.run(f"UNWIND $rows AS r MATCH (a:{a} {{entity_id: r.s}}), (b:{b} {{entity_id: r.t}}) "
                        f"CREATE (a)-[:{edge}]->(b)", rows=chunk).consume()


def timed(session, query, params):
    started = time.perf_counter()
    rows = session.run(query, params).data()
    return time.perf_counter() - started, rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--uri", required=True)
    parser.add_argument("--database")
    parser.add_argument("--cursor-seconds", type=float, default=120)
    parser.add_argument("--load-rows", type=int, help="first load this many rows per label (empty server only)")
    args = parser.parse_args()
    with GraphDatabase.driver(args.uri, auth=None) as driver, driver.session(database=args.database) as session:
        if args.load_rows:
            if session.run("MATCH (n) RETURN count(n) AS c").single()["c"]:
                raise SystemExit("refusing to load into a non-empty database")
            started = time.perf_counter()
            load(session, args.load_rows)
            print(json.dumps({"name": "load", "seconds": round(time.perf_counter() - started, 1)}))
        for name, query, params in QUERIES:
            _, rows = timed(session, query, params)
            times = sorted(timed(session, query, params)[0] for _ in range(3))
            print(json.dumps({"name": name, "median_s": round(statistics.median(times), 3), "rows": rows[:2]}))
        after, seen, started = "", 0, time.perf_counter()
        page = ("MATCH (n:Usuario) WHERE n.entity_id > $after RETURN n.entity_id AS id "
                "ORDER BY n.entity_id LIMIT 1000")
        while time.perf_counter() - started < args.cursor_seconds:
            rows = session.run(page, after=after).data()
            if not rows:
                break
            seen += len(rows)
            after = rows[-1]["id"]
        elapsed = time.perf_counter() - started
        print(json.dumps({"name": "cursor_walk", "rows_seen": seen, "seconds": round(elapsed, 2),
                          "extrapolated_1m_s": round(elapsed * 1_000_000 / max(seen, 1), 2)}))


if __name__ == "__main__":
    main()
