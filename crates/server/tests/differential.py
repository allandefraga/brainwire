"""Differential test: the same synthetic graph and read queries on Brainwire and Neo4j.

Starts a private Brainwire server from --binary, loads an identical seeded dataset into
it and into an EMPTY, throwaway Neo4j given by --neo4j-uri (the script refuses a
non-empty Neo4j unless --reset-neo4j is passed, and then deletes everything in it),
runs every query in QUERIES on both and compares results as multisets; queries marked
ordered must also match row order. Nodes compare by label and entity_id, relationships
by type and properties, floats to 6 decimals.

    python crates/server/tests/differential.py --binary target/release/brainwire \\
        --neo4j-uri bolt://127.0.0.1:7687 --reset-neo4j

Exit code 1 on any difference.
"""
import argparse
from collections import Counter
import json
import os
from pathlib import Path
import random
import socket
import subprocess
import sys
import tempfile
import time

from neo4j import GraphDatabase
from neo4j.graph import Node, Path as GraphPath, Relationship

GRAPH = "differential"
ONTOLOGY = {"version": "1", "nodes": [
    {"label": "Person", "properties": ["name", "age", "score", "city", "tags", "active"],
     "indexed_fields": ["name", "age"]},
    {"label": "Company", "properties": ["name", "sector"], "indexed_fields": ["sector"]},
    {"label": "Car", "properties": ["brand", "year", "price"], "indexed_fields": ["brand", "year"]},
    {"label": "City", "properties": ["name", "uf"]}],
    "edges": [
    {"type": "KNOWS", "from": {"label": "Person"}, "to": {"label": "Person"}, "properties": ["since"]},
    {"type": "WORKS_AT", "from": {"label": "Person"}, "to": {"label": "Company"}, "properties": ["role"]},
    {"type": "OWNS", "from": {"label": "Person"}, "to": {"label": "Car"}},
    {"type": "LIVES_IN", "from": {"label": "Person"}, "to": {"label": "City"}}]}

# (ordered, query, params). Ordered queries have unique sort keys.
QUERIES = [
    (False, "MATCH (p:Person) RETURN count(p) AS n", {}),
    (False, "MATCH (n) RETURN count(n) AS n", {}),
    (False, "MATCH (n) RETURN labels(n) AS l, count(*) AS c", {}),
    (False, "MATCH ()-[r]->() RETURN type(r) AS t, count(*) AS c", {}),
    (True, "MATCH (p:Person) RETURN p.entity_id AS id, p.name AS name, p.age AS age ORDER BY id LIMIT 25", {}),
    (True, "MATCH (p:Person) WHERE p.entity_id > $after RETURN p.entity_id AS id ORDER BY id LIMIT 10", {"after": "p0100"}),
    (False, "MATCH (p:Person {name: $name}) RETURN p.entity_id, p.age", {"name": "Ana 7"}),
    (False, "MATCH (p:Person) WHERE p.entity_id IN $ids RETURN p.name", {"ids": ["p0001", "p0002", "p0400", "nope"]}),
    (False, "MATCH (p:Person) WHERE p.name IN $names RETURN p.entity_id", {"names": ["Ana 7", "Bruno 9", "x"]}),
    (False, "MATCH (p:Person) WHERE p.age >= 30 AND p.age < 35 RETURN p.entity_id", {}),
    (False, "MATCH (p:Person) WHERE p.age > 60.5 RETURN p.entity_id, p.age", {}),
    (False, "MATCH (p:Person) WHERE p.name STARTS WITH 'Jo' RETURN p.entity_id", {}),
    (False, "MATCH (p:Person) WHERE p.name ENDS WITH '3' AND p.city CONTAINS 'o' RETURN p.entity_id", {}),
    (False, "MATCH (p:Person) WHERE p.age IS NULL RETURN count(*) AS c", {}),
    (False, "MATCH (p:Person) WHERE p.city IS NOT NULL AND NOT p.active RETURN count(*) AS c", {}),
    (False, "MATCH (p:Person) WHERE p.age > 40 XOR p.active RETURN count(*) AS c", {}),
    (False, "MATCH (c:Car) RETURN c.brand AS brand, count(*) AS n, min(c.year) AS first, max(c.year) AS last, sum(c.year) AS s", {}),
    (False, "MATCH (c:Car) RETURN c.brand AS brand, round(avg(c.price), 2) AS avg_price", {}),
    (True, "MATCH (:Person)-[:OWNS]->(c:Car) RETURN c.brand AS brand, count(*) AS n ORDER BY n DESC, brand LIMIT 3", {}),
    (True, "MATCH (p:Person)-[:LIVES_IN]->(c:City) RETURN c.uf AS uf, count(p) AS people ORDER BY people DESC, uf LIMIT 5", {}),
    (False, "MATCH (u:Person)-[:OWNS]->(c:Car) RETURN count(*) AS pairs", {}),
    (False, "MATCH (p:Person {entity_id: 'p0005'})-[:KNOWS]->(f) RETURN f.entity_id", {}),
    (False, "MATCH (p:Person {entity_id: 'p0005'})-[:KNOWS]->()-[:KNOWS]->(fof) RETURN DISTINCT fof.entity_id", {}),
    (False, "MATCH (p:Person {entity_id: 'p0005'})-[:KNOWS*1..2]->(x) RETURN DISTINCT x.entity_id", {}),
    (False, "MATCH (p:Person {entity_id: 'p0005'})-[r:KNOWS*2]->(x) RETURN x.entity_id, size(r) AS hops", {}),
    (False, "MATCH (p:Person {entity_id: 'p0005'})-[:KNOWS]-(x) RETURN x.entity_id", {}),
    (False, "MATCH (p:Person {entity_id: 'p0005'})-[r]->(x) RETURN type(r) AS t, labels(x) AS l, count(*) AS c", {}),
    (False, "MATCH (p:Person {entity_id: 'p0005'})--(x) RETURN count(x) AS c", {}),
    (False, "MATCH (a:Person)-[:WORKS_AT]->(c:Company)<-[:WORKS_AT]-(b:Person) WHERE a.entity_id = 'p0003' RETURN b.entity_id", {}),
    (False, "MATCH (p:Person) OPTIONAL MATCH (p)-[:WORKS_AT]->(c:Company) RETURN c.sector AS sector, count(p) AS people", {}),
    (False, "MATCH (p:Person) WHERE p.entity_id < 'p0020' OPTIONAL MATCH (p)-[:OWNS]->(c) RETURN p.entity_id, count(c) AS cars", {}),
    (False, "MATCH (p:Person) WHERE p.entity_id < 'p0020' OPTIONAL MATCH (p)-[w:WORKS_AT]->(c) RETURN p.entity_id, c.name, w.role", {}),
    (False, "MATCH (p:Person)-[:WORKS_AT]->(c:Company) WITH c, count(p) AS staff WHERE staff >= 10 RETURN c.entity_id, staff", {}),
    (True, "MATCH (p:Person)-[:OWNS]->(car) WITH p, collect(car.brand) AS brands WHERE size(brands) > 1 RETURN p.entity_id AS id, size(brands) AS n ORDER BY id LIMIT 10", {}),
    (False, "MATCH (p:Person) WITH p.city AS city, avg(p.age) AS a RETURN city, round(a, 3) AS a", {}),
    (False, "MATCH (c:Company) WITH c ORDER BY c.entity_id LIMIT 5 MATCH (c)<-[:WORKS_AT]-(p) RETURN c.entity_id, count(p) AS n", {}),
    (False, "UNWIND $ids AS id MATCH (p:Person {entity_id: id}) RETURN id, p.name", {"ids": ["p0010", "p0011", "zz"]}),
    (False, "UNWIND [1, 2, 3] AS x UNWIND [x, x * 10] AS y RETURN x, y", {}),
    (False, "MATCH (p:Person) WHERE p.entity_id < 'p0010' RETURN p.entity_id, CASE WHEN p.age >= 50 THEN 'senior' WHEN p.age >= 30 THEN 'adult' ELSE 'young' END AS band", {}),
    (False, "MATCH (p:Person) WHERE p.entity_id < 'p0010' RETURN p.entity_id, CASE p.city WHEN 'São Paulo' THEN 1 WHEN 'Rio' THEN 2 ELSE 0 END AS code", {}),
    (False, "MATCH (p:Person) WHERE p.entity_id < 'p0010' RETURN p.entity_id, [t IN p.tags WHERE t STARTS WITH 't' | toUpper(t)] AS tags", {}),
    (False, "MATCH (p:Person) WHERE p.entity_id < 'p0010' RETURN p.entity_id, p.name + ' / ' + coalesce(p.city, '?') AS label, size(p.name) AS len", {}),
    (False, "MATCH (p:Person) WHERE p.entity_id < 'p0010' RETURN p.entity_id, p.age * 2 + 1 AS a, p.age / 7 AS b, p.age % 7 AS c, toFloat(p.age) / 4 AS d", {}),
    (False, "MATCH (p:Person) WHERE p.entity_id < 'p0005' RETURN p {.name, .age, kind: 'person'} AS m", {}),
    (False, "MATCH (p:Person) WHERE p.entity_id < 'p0005' RETURN size(keys(p)) AS k, 'name' IN keys(p) AS has_name, properties(p).name AS n", {}),
    (False, "MATCH (p:Person) WHERE p.entity_id < 'p0005' RETURN toLower(p.name) AS a, toString(p.age) AS b, toInteger('12') AS c, trim('  x ') AS d, replace(p.name, ' ', '_') AS e, substring(p.name, 0, 2) AS f, split('a,b', ',') AS g", {}),
    (False, "MATCH (p:Person) WHERE p.entity_id < 'p0005' RETURN head(p.tags) AS h, last(p.tags) AS l, size(p.tags) AS s, p.tags[0] AS first, p.tags[1..] AS rest", {}),
    (False, "MATCH (p:Person) WHERE any(t IN p.tags WHERE t = 'gold') RETURN count(p) AS c", {}),
    (False, "MATCH (p:Person) WHERE p.entity_id < 'p0050' AND all(t IN p.tags WHERE size(t) > 3) RETURN count(p) AS c", {}),
    (False, "RETURN reduce(s = 0, x IN range(1, 10) | s + x) AS total, range(0, 10, 3) AS r", {}),
    (False, "MATCH (p:Person) WHERE (p)-[:OWNS]->(:Car {brand: 'Fiat'}) RETURN count(p) AS c", {}),
    (False, "MATCH (p:Person) WHERE NOT (p)-[:WORKS_AT]->() RETURN count(p) AS c", {}),
    (False, "MATCH (p:Person) WHERE exists((p)<-[:KNOWS]-()) RETURN count(p) AS c", {}),
    (False, "MATCH (p:Person) RETURN count(DISTINCT p.city) AS cities, collect(DISTINCT p.city)[0..0] AS none", {}),
    (True, "MATCH (p:Person) RETURN DISTINCT p.city AS city ORDER BY city", {}),
    (True, "MATCH (p:Person) RETURN p.entity_id AS id ORDER BY p.score DESC, id SKIP 5 LIMIT 5", {}),
    (False, "MATCH (p:Person) WHERE p.entity_id < 'p0003' RETURN p", {}),
    (False, "MATCH (p:Person {entity_id: 'p0005'})-[r:KNOWS]->(f) RETURN r", {}),
    (False, "MATCH path = (p:Person {entity_id: 'p0005'})-[:KNOWS]->(f) RETURN path", {}),
    (False, "MATCH path = (p:Person {entity_id: 'p0005'})-[:KNOWS*2]->(f) RETURN length(path) AS l, [n IN nodes(path) | n.entity_id] AS ids", {}),
    (False, "MATCH (p:Person {entity_id: 'p0005'})-[:OWNS]->(c) RETURN collect(c) AS cars", {}),
    (False, "MATCH (p:Person {entity_id: 'p0005'}) MATCH (p)-[r:KNOWS]->() RETURN startNode(r) = p AS starts, endNode(r) <> p AS other", {}),
    (False, "MATCH (p:Person) WHERE p.age > 70 RETURN p.entity_id AS id UNION MATCH (c:Car) WHERE c.year < 2001 RETURN c.entity_id AS id", {}),
    (False, "MATCH (p:Person) WHERE p.entity_id < 'p0003' RETURN p.entity_id AS id UNION ALL MATCH (p:Person) WHERE p.entity_id < 'p0002' RETURN p.entity_id AS id", {}),
    (False, "MATCH (c:Car) WHERE c.brand IN ['Fiat', 'Ford'] AND c.year >= 2015 RETURN c.brand, count(*) AS n", {}),
    (False, "MATCH (p:Person)-[k:KNOWS]->(f:Person) WHERE k.since > 2018 RETURN count(*) AS c, min(k.since) AS first", {}),
    (False, "MATCH (p:Person)-[k:KNOWS {since: 2020}]->(f) RETURN count(*) AS c", {}),
    (False, "MATCH (n:Person) WHERE n:Person AND NOT n:Car RETURN count(n) AS c", {}),
    (False, "MATCH (p:Person) RETURN p.active AS active, count(*) AS c, sum(CASE WHEN p.age > 40 THEN 1 ELSE 0 END) AS older", {}),
    (False, "RETURN 1 + 2 * 3 AS a, 2 ^ 10 AS b, 7 / 2 AS c, 7.0 / 2 AS d, -3 % 2 AS e, 'x' + 1 AS f, [1] + [2, 3] AS g, 1 = 1.0 AS h, null = null AS i, 'a' < 'b' AS j", {}),
    (False, "RETURN toInteger(3.9) AS a, toFloat('2.5') AS b, abs(-4) AS c, ceil(1.2) AS d, floor(-1.2) AS e, sqrt(16) AS f, sign(-3) AS g, coalesce(null, null) AS h", {}),
    # Shapes the platform's own reads use (neighborhoods, summaries, dumps).
    (False, "MATCH (e:Person {entity_id: 'p0007'}) WITH e MATCH (e)-[r]-(neighbor) RETURN e, r, neighbor", {}),
    (False, "MATCH (e:Person {entity_id: 'p0007'}) WITH e OPTIONAL MATCH (e)-[r:KNOWS|WORKS_AT]-(neighbor) WHERE NOT neighbor:City RETURN e AS a, r AS r, neighbor AS b", {}),
    (False, "MATCH (e:Person {entity_id: 'p0007'}) WITH e OPTIONAL MATCH (e)-[r]-(neighbor) RETURN e AS a, r AS r, neighbor AS b UNION MATCH (e:Person {entity_id: 'p0007'}) MATCH (e)-[]-(mid)-[r2]-(neighbor) WHERE NOT mid:City AND neighbor <> e RETURN mid AS a, r2 AS r, neighbor AS b", {}),
    (False, "MATCH (e:Person {entity_id: 'p0007'}) WITH e RETURN e AS a, null AS r, null AS b", {}),
    (True, "MATCH (e:Person {entity_id: 'p0007'}) WITH e MATCH (e)-[r]-(n) WHERE NOT n:City RETURN type(r) AS rel, labels(n)[0] AS label, count(*) AS c ORDER BY c DESC, rel, label", {}),
    (True, "MATCH (e:Person {entity_id: 'p0007'})-[]-(m)-[r2]-(n) WHERE NOT m:City AND NOT n:City AND n <> e RETURN labels(m)[0] AS via, type(r2) AS rel, labels(n)[0] AS label, count(*) AS c ORDER BY c DESC, via, rel, label", {}),
    (False, "MATCH (n) WHERE any(lbl IN labels(n) WHERE lbl IN $types) AND n.missing IS NULL RETURN count(n) AS c", {"types": ["Car", "City"]}),
    (False, "MATCH (n) WHERE NOT 'Person' IN labels(n) RETURN count(n) AS c", {}),
    (False, "MATCH (n:Company)-[r]->(m) RETURN count(r) AS c", {}),
    (False, "MATCH (n:Person)-[r:OWNS]->(m) RETURN r, n.entity_id AS src, m.entity_id AS dst", {}),
    (False, "MATCH (a:Person)-[r:KNOWS]->(b) WHERE a.entity_id = 'p0001' RETURN labels(a) AS la, labels(b) AS lb, keys(r) AS k", {}),
    (False, "MATCH (:Person)-[:WORKS_AT]->(:Company) RETURN 1 AS ok LIMIT 1", {}),
    (False, "MATCH (p:Person {entity_id: 'p0009'})-[:OWNS]->(:Car)<-[:OWNS]-(other:Person) RETURN DISTINCT other.entity_id", {}),
    (False, "MATCH path = (p:Person {entity_id: 'p0009'})-[*1..2]->(x) RETURN labels(x)[0] AS label, length(path) AS hops, count(*) AS c", {}),
    (True, "UNWIND range(1, 600, 37) AS i MATCH (p:Person {entity_id: 'p' + substring(toString(10000 + i), 1)}) RETURN p.entity_id AS id ORDER BY id", {}),
    (False, "MATCH (c:Car) WHERE c.year >= 2010 AND c.year <= 2012 RETURN count(c) AS c", {}),
    (False, "MATCH (p:Person) WHERE p.age > 30 AND p.score >= 50 RETURN count(p) AS c, percentileDisc(p.age, 0.5) AS median, stdev(p.score) > 0 AS spread", {}),
    (False, "MATCH (p:Person) RETURN *", {}),
    (False, "MATCH (n:Person:Car) RETURN count(n) AS c", {}),
    (False, "MATCH (n:Person) WHERE n.entity_id = 'p0001' RETURN {a: n.name, b: [n.age]} AS m", {}),
    (False, "MATCH (p:Person) WHERE p.entity_id IN ['p0001', 'p0002'] SET p.touched = true RETURN count(p) AS c", {}),
]


def node_key(node):
    return ("node", tuple(sorted(node.labels)), node.get("entity_id"))


def norm(value):
    if isinstance(value, Node):
        return node_key(value)
    if isinstance(value, Relationship):
        return ("rel", value.type, tuple(sorted((k, norm(v)) for k, v in value.items())))
    if isinstance(value, GraphPath):
        return ("path", tuple(node_key(n) for n in value.nodes), tuple(norm(r) for r in value.relationships))
    if isinstance(value, float):
        return ("float", round(value, 6))
    if isinstance(value, bool):
        return ("bool", value)
    if isinstance(value, int):
        return ("int", value)
    if isinstance(value, list):
        return ("list", tuple(norm(v) for v in value))
    if isinstance(value, dict):
        return ("map", tuple(sorted((k, norm(v)) for k, v in value.items())))
    return value


def rows(session, query, params):
    result = session.run(query, params)
    return [tuple(norm(record[key]) for key in result.keys()) for record in result]


def dataset(seed=7):
    rng = random.Random(seed)
    first = ["Ana", "Bruno", "Carla", "João", "José", "Lúcia", "Mário", "Nina", "Otávio", "Paula"]
    cities = [("São Paulo", "SP"), ("Rio", "RJ"), ("Belo Horizonte", "MG"), ("Curitiba", "PR"), ("Porto Alegre", "RS")]
    people = []
    for i in range(600):
        people.append({
            "id": f"p{i:04d}", "name": f"{rng.choice(first)} {i % 50}",
            "age": None if i % 17 == 0 else rng.randint(18, 80), "score": round(rng.random() * 100, 3),
            "city": None if i % 13 == 0 else rng.choice(cities)[0],
            "tags": rng.sample(["gold", "silver", "tech", "travel", "music"], rng.randint(0, 3)),
            "active": i % 3 != 0})
    companies = [{"id": f"co{i:03d}", "name": f"Company {i}", "sector": rng.choice(["tech", "retail", "bank"])}
                 for i in range(40)]
    cars = [{"id": f"car{i:04d}", "brand": rng.choice(["Fiat", "Ford", "VW", "Honda", "Toyota"]),
             "year": rng.randint(1998, 2025), "price": round(rng.uniform(20000, 150000), 2)} for i in range(500)]
    city_rows = [{"id": f"city{i}", "name": n, "uf": uf} for i, (n, uf) in enumerate(cities)]
    knows = {(f"p{i:04d}", f"p{j:04d}") for i in range(600) for j in rng.sample(range(600), 3) if i != j}
    knows = [{"s": s, "t": t, "since": rng.randint(2000, 2025)} for s, t in sorted(knows)]
    works = [{"s": f"p{i:04d}", "t": rng.choice(companies)["id"], "role": rng.choice(["dev", "ops", None])}
             for i in range(600) if i % 5]
    owns = [{"s": f"p{rng.randrange(600):04d}", "t": c["id"]} for c in cars]
    lives = [{"s": p["id"], "t": city_rows[i % 5]["id"]} for i, p in enumerate(people) if p["city"]]
    return people, companies, cars, city_rows, knows, works, owns, lives


def load(session, data):
    people, companies, cars, cities, knows, works, owns, lives = data
    statements = [
        ("UNWIND $rows AS r MERGE (n:Person {entity_id: r.id}) SET n.name = r.name, n.age = r.age, "
         "n.score = r.score, n.city = r.city, n.tags = r.tags, n.active = r.active", people),
        ("UNWIND $rows AS r MERGE (n:Company {entity_id: r.id}) SET n.name = r.name, n.sector = r.sector", companies),
        ("UNWIND $rows AS r MERGE (n:Car {entity_id: r.id}) SET n.brand = r.brand, n.year = r.year, n.price = r.price", cars),
        ("UNWIND $rows AS r MERGE (n:City {entity_id: r.id}) SET n.name = r.name, n.uf = r.uf", cities),
        ("UNWIND $rows AS r MATCH (a:Person {entity_id: r.s}), (b:Person {entity_id: r.t}) MERGE (a)-[k:KNOWS]->(b) SET k.since = r.since", knows),
        ("UNWIND $rows AS r MATCH (a:Person {entity_id: r.s}), (b:Company {entity_id: r.t}) MERGE (a)-[w:WORKS_AT]->(b) SET w.role = r.role", works),
        ("UNWIND $rows AS r MATCH (a:Person {entity_id: r.s}), (b:Car {entity_id: r.t}) MERGE (a)-[:OWNS]->(b)", owns),
        ("UNWIND $rows AS r MATCH (a:Person {entity_id: r.s}), (b:City {entity_id: r.t}) MERGE (a)-[:LIVES_IN]->(b)", lives),
    ]
    for query, batch in statements:
        for start in range(0, len(batch), 500):
            session.run(query, rows=batch[start:start + 500]).consume()


def start_brainwire(binary, root):
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    env = {k: v for k, v in os.environ.items() if not k.startswith("BRAINWIRE_")}
    config = root / "brainwire.toml"
    config.write_text(f'[server]\nlisten = "127.0.0.1:{port}"\ndata_dir = "{root / "data"}"\n'
                      f'default_database = "{GRAPH}"\n')
    (root / "ontology.json").write_text(json.dumps(ONTOLOGY))
    subprocess.check_call([str(binary), "db", "create", GRAPH, "--ontology", str(root / "ontology.json"),
                           "--config", str(config)], env=env, stdout=subprocess.DEVNULL)
    log = (root / "server.log").open("w")
    process = subprocess.Popen([str(binary), "serve", "--config", str(config)], env=env, stdout=log, stderr=log)
    for _ in range(400):
        if process.poll() is not None:
            raise RuntimeError((root / "server.log").read_text())
        try:
            socket.create_connection(("127.0.0.1", port), timeout=0.1).close()
            return process, f"bolt://127.0.0.1:{port}", config, env
        except OSError:
            time.sleep(0.025)
    raise RuntimeError("server did not start")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--neo4j-uri", required=True)
    parser.add_argument("--neo4j-user", default=os.environ.get("NEO4J_USER"))
    parser.add_argument("--neo4j-password-env", default="NEO4J_PASSWORD",
                        help="environment variable holding the Neo4j password (never pass it on the command line)")
    parser.add_argument("--reset-neo4j", action="store_true")
    parser.add_argument("--extra", type=Path, help="JSON file with more [ordered, query, params] entries")
    args = parser.parse_args()
    queries = list(QUERIES)
    if args.extra:
        queries += [tuple(entry) for entry in json.loads(args.extra.read_text())]
    auth = (args.neo4j_user, os.environ.get(args.neo4j_password_env, "")) if args.neo4j_user else None
    data = dataset()
    failures, report = [], {"queries": len(queries)}
    with tempfile.TemporaryDirectory(prefix="brainwire-differential-") as temporary:
        root = Path(temporary)
        process, uri, config, env = start_brainwire(args.binary.resolve(), root)
        try:
            with GraphDatabase.driver(args.neo4j_uri, auth=auth) as neo, GraphDatabase.driver(uri, auth=None) as bw:
                with neo.session() as ns, bw.session(database=GRAPH) as bs:
                    existing = ns.run("MATCH (n) RETURN count(n) AS c").single()["c"]
                    if existing and not args.reset_neo4j:
                        sys.exit(f"refusing a non-empty Neo4j ({existing} nodes); pass --reset-neo4j for a throwaway instance")
                    ns.run("MATCH (n) DETACH DELETE n").consume()
                    for spec in ONTOLOGY["nodes"]:
                        ns.run(f"CREATE INDEX IF NOT EXISTS FOR (n:{spec['label']}) ON (n.entity_id)").consume()
                        for field in spec.get("indexed_fields", []):
                            ns.run(f"CREATE INDEX IF NOT EXISTS FOR (n:{spec['label']}) ON (n.{field})").consume()
                    ns.run("CALL db.awaitIndexes()").consume()
                    started = time.monotonic()
                    load(ns, data)
                    load(bs, data)
                    report["load_seconds"] = round(time.monotonic() - started, 2)
                    for ordered, query, params in queries:
                        try:
                            expected = rows(ns, query, params)
                        except Exception as error:
                            failures.append({"query": query, "neo4j_error": str(error)[:300]})
                            continue
                        try:
                            actual = rows(bs, query, params)
                        except Exception as error:
                            failures.append({"query": query, "brainwire_error": str(error)[:300]})
                            continue
                        same = actual == expected if ordered else Counter(actual) == Counter(expected)
                        if not same:
                            failures.append({"query": query, "neo4j": repr(expected[:5]), "brainwire": repr(actual[:5]),
                                             "neo4j_rows": len(expected), "brainwire_rows": len(actual)})
        finally:
            process.terminate()
            process.wait(timeout=30)
        audit = json.loads(subprocess.check_output([str(args.binary), "db", "check", GRAPH, "--json",
                                                    "--config", str(config)], env=env))
        report["db_check"] = audit["status"]
    report["failures"] = failures
    report["passed"] = len(queries) - len(failures)
    print(json.dumps(report, indent=1, ensure_ascii=False))
    sys.exit(1 if failures or report["db_check"] != "ok" else 0)


if __name__ == "__main__":
    main()
