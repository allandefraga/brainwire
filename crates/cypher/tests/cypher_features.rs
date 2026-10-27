//! Cypher that agents write: multi-hop and variable-length patterns, untyped
//! and unlabeled matches, OPTIONAL MATCH, WITH, aggregates, functions, CASE,
//! comprehensions, operators, UNION, relationship and path values.
use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{execute, Out};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::collections::BTreeMap;

fn setup() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping = serde_json::from_value(serde_json::json!({
        "version": "1",
        "nodes": [
            {"label": "Person", "properties": ["name", "age", "city"], "indexed_fields": ["name", "age"]},
            {"label": "Company", "properties": ["name"]},
            {"label": "Car", "properties": ["brand", "year"], "indexed_fields": ["brand"]}
        ],
        "edges": [
            {"type": "KNOWS", "from": {"label": "Person"}, "to": {"label": "Person"}, "properties": ["since"]},
            {"type": "WORKS_AT", "from": {"label": "Person"}, "to": {"label": "Company"}},
            {"type": "OWNS", "from": {"label": "Person"}, "to": {"label": "Car"}}
        ]
    }))
    .unwrap();
    db.apply_ontology(&mapping).unwrap();
    run(&db, "CREATE (:Person {entity_id:'ana', name:'Ana', age:31, city:'SP'}), (:Person {entity_id:'bia', name:'Bia', age:25, city:'RJ'}), (:Person {entity_id:'caio', name:'Caio', age:40, city:'SP'}), (:Person {entity_id:'dani', name:'Dani', age:25})");
    run(&db, "CREATE (:Company {entity_id:'acme', name:'Acme'}), (:Company {entity_id:'beta', name:'Beta'})");
    run(&db, "CREATE (:Car {entity_id:'c1', brand:'Fiat', year:2020}), (:Car {entity_id:'c2', brand:'Ford', year:2018}), (:Car {entity_id:'c3', brand:'Fiat', year:2022})");
    for (a, b, since) in [
        ("ana", "bia", 2010),
        ("bia", "caio", 2015),
        ("caio", "dani", 2020),
    ] {
        run(&db, &format!("MATCH (a:Person {{entity_id:'{a}'}}), (b:Person {{entity_id:'{b}'}}) MERGE (a)-[:KNOWS {{since: {since}}}]->(b)"));
    }
    for (p, c) in [("ana", "acme"), ("bia", "acme"), ("caio", "beta")] {
        run(&db, &format!("MATCH (p:Person {{entity_id:'{p}'}}), (c:Company {{entity_id:'{c}'}}) MERGE (p)-[:WORKS_AT]->(c)"));
    }
    for (p, c) in [("ana", "c1"), ("ana", "c2"), ("caio", "c3")] {
        run(&db, &format!("MATCH (p:Person {{entity_id:'{p}'}}), (c:Car {{entity_id:'{c}'}}) MERGE (p)-[:OWNS]->(c)"));
    }
    (dir, db)
}

fn run(db: &Database, query: &str) -> Vec<Vec<Out>> {
    execute(db, query, &BTreeMap::new())
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .rows
}

fn v(value: impl Into<Json>) -> Out {
    Out::Value(value.into().0)
}

struct Json(Value);
impl From<i64> for Json {
    fn from(n: i64) -> Self {
        Json(Value::Int(n))
    }
}
impl From<&str> for Json {
    fn from(s: &str) -> Self {
        Json(Value::Str(s.into()))
    }
}
impl From<f64> for Json {
    fn from(f: f64) -> Self {
        Json(Value::Float(f))
    }
}
impl From<bool> for Json {
    fn from(b: bool) -> Self {
        Json(Value::Bool(b))
    }
}
impl From<()> for Json {
    fn from(_: ()) -> Self {
        Json(Value::Null)
    }
}
fn list(items: Vec<Json>) -> Out {
    Out::Value(Value::List(items.into_iter().map(|j| j.0).collect()))
}

#[test]
fn multi_hop_and_variable_length_patterns() {
    let (_dir, db) = setup();
    assert_eq!(
        run(
            &db,
            "MATCH (a:Person {name:'Ana'})-[:KNOWS]->(b)-[:KNOWS]->(c) RETURN b.name, c.name"
        ),
        vec![vec![v("Bia"), v("Caio")]]
    );
    assert_eq!(
        run(
            &db,
            "MATCH (a:Person {name:'Ana'})-[:KNOWS*1..3]->(x) RETURN x.name ORDER BY x.name"
        ),
        vec![vec![v("Bia")], vec![v("Caio")], vec![v("Dani")]]
    );
    assert_eq!(
        run(
            &db,
            "MATCH (a:Person {name:'Ana'})-[r:KNOWS*2]->(x) RETURN x.name, size(r)"
        ),
        vec![vec![v("Caio"), v(2)]]
    );
    // Anchored on the indexed end, expanded leftwards.
    assert_eq!(
        run(&db, "MATCH (c:Company)<-[:WORKS_AT]-(p:Person)-[:OWNS]->(car:Car {brand:'Ford'}) RETURN c.name, p.name"),
        vec![vec![v("Acme"), v("Ana")]]
    );
    assert_eq!(
        run(
            &db,
            "MATCH (x:Person {name:'Dani'})<-[:KNOWS*0..]-(y) RETURN count(y)"
        ),
        vec![vec![v(4)]]
    );
}

#[test]
fn untyped_undirected_and_unlabeled_patterns() {
    let (_dir, db) = setup();
    assert_eq!(
        run(
            &db,
            "MATCH (a:Person {name:'Ana'})-[r]->(x) RETURN type(r), count(*) ORDER BY type(r)"
        ),
        vec![
            vec![v("KNOWS"), v(1)],
            vec![v("OWNS"), v(2)],
            vec![v("WORKS_AT"), v(1)]
        ]
    );
    assert_eq!(
        run(
            &db,
            "MATCH (b:Person {name:'Bia'})-[:KNOWS]-(x) RETURN x.name ORDER BY x.name"
        ),
        vec![vec![v("Ana")], vec![v("Caio")]]
    );
    assert_eq!(run(&db, "MATCH (n) RETURN count(n)"), vec![vec![v(9)]]);
    assert_eq!(
        run(&db, "MATCH (n) WHERE n:Car AND n.year > 2019 RETURN labels(n), n.entity_id ORDER BY n.entity_id"),
        vec![
            vec![list(vec!["Car".into()]), v("c1")],
            vec![list(vec!["Car".into()]), v("c3")]
        ]
    );
    // Relationship uniqueness inside one MATCH.
    assert_eq!(
        run(
            &db,
            "MATCH (a:Person {name:'Bia'})-[:KNOWS]-(b)-[:KNOWS]-(c) RETURN c.name ORDER BY c.name"
        ),
        vec![vec![v("Dani")]]
    );
}

#[test]
fn optional_match_with_and_aggregation() {
    let (_dir, db) = setup();
    assert_eq!(
        run(&db, "MATCH (p:Person) OPTIONAL MATCH (p)-[:OWNS]->(c:Car) RETURN p.name, count(c) AS cars ORDER BY cars DESC, p.name"),
        vec![
            vec![v("Ana"), v(2)],
            vec![v("Caio"), v(1)],
            vec![v("Bia"), v(0)],
            vec![v("Dani"), v(0)]
        ]
    );
    assert_eq!(
        run(&db, "MATCH (p:Person) OPTIONAL MATCH (p)-[:WORKS_AT]->(c) RETURN p.name, c.name ORDER BY p.name"),
        vec![
            vec![v("Ana"), v("Acme")],
            vec![v("Bia"), v("Acme")],
            vec![v("Caio"), v("Beta")],
            vec![v("Dani"), v(())]
        ]
    );
    assert_eq!(
        run(&db, "MATCH (p:Person)-[:WORKS_AT]->(c:Company) WITH c, count(p) AS staff WHERE staff > 1 MATCH (c)<-[:WORKS_AT]-(e) RETURN c.name, staff, collect(e.name) AS names"),
        vec![vec![v("Acme"), v(2), list(vec!["Ana".into(), "Bia".into()])]]
    );
    assert_eq!(
        run(&db, "MATCH (p:Person) RETURN sum(p.age), avg(p.age), min(p.age), max(p.name), count(DISTINCT p.age), collect(DISTINCT p.city)"),
        vec![vec![v(121), v(30.25), v(25), v("Dani"), v(3), list(vec!["SP".into(), "RJ".into()])]]
    );
    assert_eq!(
        run(
            &db,
            "MATCH (p:Person) WITH p ORDER BY p.age DESC LIMIT 2 RETURN collect(p.name)"
        ),
        vec![vec![list(vec!["Caio".into(), "Ana".into()])]]
    );
    assert_eq!(
        run(&db, "MATCH (p:Person) RETURN DISTINCT p.age ORDER BY p.age"),
        vec![vec![v(25)], vec![v(31)], vec![v(40)]]
    );
    assert_eq!(
        run(
            &db,
            "MATCH (p:Person {name:'Nobody'}) RETURN count(p), sum(p.age), collect(p)"
        ),
        vec![vec![v(0), v(0), list(vec![])]]
    );
}

#[test]
fn functions_case_comprehensions_and_operators() {
    let (_dir, db) = setup();
    assert_eq!(
        run(&db, "MATCH (p:Person {name:'Ana'}) RETURN p.name + ' (' + p.age + ')', toUpper(p.city), p.age * 2 - 1, p.age / 2, p.age % 7, 2 ^ 3, -p.age"),
        vec![vec![v("Ana (31)"), v("SP"), v(61), v(15), v(3), v(8.0), v(-31)]]
    );
    assert_eq!(
        run(&db, "MATCH (p:Person) WHERE p.name STARTS WITH 'C' OR p.name ENDS WITH 'ia' OR p.name CONTAINS 'an' RETURN p.name ORDER BY p.name"),
        vec![vec![v("Bia")], vec![v("Caio")], vec![v("Dani")]]
    );
    assert_eq!(
        run(&db, "MATCH (p:Person) RETURN p.name, CASE WHEN p.age >= 30 THEN 'senior' ELSE 'junior' END AS band, CASE p.city WHEN 'SP' THEN 1 WHEN 'RJ' THEN 2 END ORDER BY p.name"),
        vec![
            vec![v("Ana"), v("senior"), v(1)],
            vec![v("Bia"), v("junior"), v(2)],
            vec![v("Caio"), v("senior"), v(1)],
            vec![v("Dani"), v("junior"), v(())]
        ]
    );
    assert_eq!(
        run(&db, "RETURN [x IN range(1, 6) WHERE x % 2 = 0 | x * 10], [1, 2] + [3], size('abc'), coalesce(null, 'd'), toLower('ÁB'), reduce(s = 0, x IN [1, 2, 3] | s + x), any(x IN [1, 2] WHERE x > 1), all(x IN [] WHERE x > 1), head([7, 8]), last([7, 8]), [1, 2, 3][1..], split('a,b', ','), toInteger('42'), toString(1.5), trim('  x ')"),
        vec![vec![
            list(vec![20.into(), 40.into(), 60.into()]),
            list(vec![1.into(), 2.into(), 3.into()]),
            v(3), v("d"), v("áb"), v(6), v(true), v(true), v(7), v(8),
            list(vec![2.into(), 3.into()]),
            list(vec!["a".into(), "b".into()]),
            v(42), v("1.5"), v("x")
        ]]
    );
    assert_eq!(
        run(&db, "MATCH (p:Person {name:'Ana'}) RETURN p {.name, .age, kind: 'person'}, keys(p), properties(p).city"),
        vec![vec![
            Out::Value(Value::Map(BTreeMap::from([
                ("name".into(), Value::Str("Ana".into())),
                ("age".into(), Value::Int(31)),
                ("kind".into(), Value::Str("person".into())),
            ]))),
            list(vec!["age".into(), "city".into(), "entity_id".into(), "name".into()]),
            v("SP")
        ]]
    );
    assert_eq!(
        run(&db, "MATCH (p:Person) WHERE (p)-[:OWNS]->(:Car {brand:'Fiat'}) RETURN p.name ORDER BY p.name"),
        vec![vec![v("Ana")], vec![v("Caio")]]
    );
    assert_eq!(
        run(
            &db,
            "MATCH (p:Person) WHERE NOT exists((p)-[:WORKS_AT]->()) RETURN p.name"
        ),
        vec![vec![v("Dani")]]
    );
}

#[test]
fn relationship_path_and_identity_values() {
    let (_dir, db) = setup();
    let rows = run(&db, "MATCH p = (a:Person {name:'Ana'})-[r:KNOWS]->(b) RETURN r, p, id(a) = id(a), elementId(b) STARTS WITH 'n:', length(p), [n IN nodes(p) | n.name]");
    let [Out::Relationship(rel), Out::Path(path), same, element, length, names] =
        rows[0].as_slice()
    else {
        panic!("{rows:?}")
    };
    assert_eq!(rel.rel_type, "KNOWS");
    assert_eq!(rel.props.get("since"), Some(&Value::Int(2010)));
    assert_eq!(path.nodes.len(), 2);
    assert_eq!(path.rels.len(), 1);
    assert_eq!((same, element, length), (&v(true), &v(true), &v(1)));
    assert_eq!(names, &list(vec!["Ana".into(), "Bia".into()]));
    let rows = run(
        &db,
        "MATCH (a:Person {name:'Ana'}) RETURN id(a) AS id, elementId(a) AS element",
    );
    let (Out::Value(Value::Int(id)), Out::Value(Value::Str(element))) = (&rows[0][0], &rows[0][1])
    else {
        panic!()
    };
    assert_eq!(
        run(&db, &format!("MATCH (n) WHERE id(n) = {id} RETURN n.name")),
        vec![vec![v("Ana")]]
    );
    assert_eq!(
        run(
            &db,
            &format!("MATCH (n) WHERE elementId(n) = '{element}' RETURN n.name")
        ),
        vec![vec![v("Ana")]]
    );
    let rows = run(
        &db,
        "MATCH (a:Person {name:'Ana'})-[:OWNS]->(c) RETURN collect(c) AS cars",
    );
    let [Out::List(cars)] = rows[0].as_slice() else {
        panic!("{rows:?}")
    };
    assert!(cars
        .iter()
        .all(|c| matches!(c, Out::Node(n) if n.label == "Car")));
}

#[test]
fn union_unwind_and_indexed_lookups() {
    let (_dir, db) = setup();
    assert_eq!(
        run(&db, "MATCH (p:Person) WHERE p.age > 30 RETURN p.name AS name UNION ALL MATCH (c:Company) RETURN c.name AS name UNION ALL RETURN 'Ana' AS name"),
        vec![vec![v("Ana")], vec![v("Caio")], vec![v("Acme")], vec![v("Beta")], vec![v("Ana")]]
    );
    assert!(execute(
        &db,
        "RETURN 1 AS x UNION RETURN 1 AS x UNION ALL RETURN 1 AS x",
        &BTreeMap::new()
    )
    .is_err());
    assert_eq!(
        run(
            &db,
            "MATCH (p:Person) RETURN p.name AS name UNION MATCH (p:Person) RETURN p.name AS name"
        )
        .len(),
        4
    );
    assert_eq!(
        run(&db, "UNWIND ['ana', 'caio', 'zz', 'ana'] AS id MATCH (p:Person {entity_id: id}) RETURN p.name"),
        vec![vec![v("Ana")], vec![v("Caio")], vec![v("Ana")]]
    );
    for query in [
        "MATCH (p:Person) WHERE p.entity_id IN ['ana', 'dani', 'missing'] RETURN p.name ORDER BY p.name",
        "MATCH (p:Person) WHERE p.name IN ['Ana', 'Dani', 'Nobody'] RETURN p.name ORDER BY p.name",
    ] {
        assert_eq!(run(&db, query), vec![vec![v("Ana")], vec![v("Dani")]], "{query}");
    }
    assert_eq!(
        run(
            &db,
            "MATCH (p:Person) WHERE p.age >= 25 AND p.age < 35 RETURN p.name ORDER BY p.name"
        ),
        vec![vec![v("Ana")], vec![v("Bia")], vec![v("Dani")]]
    );
    assert_eq!(
        run(
            &db,
            "MATCH (p:Person) WHERE p.age > 30.5 RETURN p.name ORDER BY p.name"
        ),
        vec![vec![v("Ana")], vec![v("Caio")]]
    );
    assert_eq!(
        run(
            &db,
            "MATCH (p:Person) WHERE p.name STARTS WITH 'Ca' OR p.name = 'x' RETURN p.name"
        ),
        vec![vec![v("Caio")]]
    );
    assert_eq!(
        run(
            &db,
            "MATCH (p:Person) WHERE p.name STARTS WITH 'B' RETURN p.name"
        ),
        vec![vec![v("Bia")]]
    );
    assert_eq!(
        run(&db, "MATCH (p:Person) WHERE p.entity_id > $after RETURN p.entity_id ORDER BY p.entity_id LIMIT 2".replace("$after", "'bia'").as_str()),
        vec![vec![v("caio")], vec![v("dani")]]
    );
}

#[test]
fn merge_hooks_remove_map_set_and_delete() {
    let (_dir, db) = setup();
    run(&db, "MERGE (p:Person {entity_id:'eva'}) ON CREATE SET p.name = 'Eva', p.age = 1 ON MATCH SET p.age = p.age + 1");
    run(&db, "MERGE (p:Person {entity_id:'eva'}) ON CREATE SET p.name = 'Wrong' ON MATCH SET p.age = p.age + 1");
    assert_eq!(
        run(
            &db,
            "MATCH (p:Person {entity_id:'eva'}) RETURN p.name, p.age"
        ),
        vec![vec![v("Eva"), v(2)]]
    );
    run(
        &db,
        "MATCH (p:Person {entity_id:'eva'}) SET p += {city: 'BH', age: 3} REMOVE p.name",
    );
    assert_eq!(
        run(
            &db,
            "MATCH (p:Person {entity_id:'eva'}) RETURN p.name, p.age, p.city"
        ),
        vec![vec![v(()), v(3), v("BH")]]
    );
    run(
        &db,
        "MATCH (p:Person {entity_id:'eva'}) SET p = {name: 'Eva 2'}",
    );
    assert_eq!(
        run(
            &db,
            "MATCH (p:Person {entity_id:'eva'}) RETURN p.name, p.age, p.city"
        ),
        vec![vec![v("Eva 2"), v(()), v(())]]
    );
    // Plain DELETE refuses a node that still has relationships.
    assert!(execute(
        &db,
        "MATCH (p:Person {entity_id:'ana'}) DELETE p",
        &BTreeMap::new()
    )
    .is_err());
    run(&db, "MATCH (p:Person {entity_id:'eva'}) DELETE p");
    run(
        &db,
        "MATCH (:Person {entity_id:'ana'})-[r:OWNS]->(c:Car {entity_id:'c2'}) DELETE r, c",
    );
    assert_eq!(
        run(
            &db,
            "MATCH (p:Person {entity_id:'ana'})-[:OWNS]->(c) RETURN c.entity_id"
        ),
        vec![vec![v("c1")]]
    );
    // Schema marker nodes: MERGE without entity_id is keyed by its properties.
    run(&db, "MERGE (m:Company {__schema_marker: true})");
    run(&db, "MERGE (m:Company {__schema_marker: true})");
    assert_eq!(
        run(
            &db,
            "MATCH (m:Company) WHERE m.__schema_marker = true RETURN count(m)"
        ),
        vec![vec![v(1)]]
    );
    run(
        &db,
        "CREATE (:Company {name: 'NoId'}), (:Company {name: 'NoId'})",
    );
    assert_eq!(
        run(&db, "MATCH (m:Company {name: 'NoId'}) RETURN count(m)"),
        vec![vec![v(2)]]
    );
    // MERGE of a relationship between bound nodes is idempotent.
    run(&db, "MATCH (a:Person {entity_id:'dani'}), (b:Company {entity_id:'beta'}) MERGE (a)-[:WORKS_AT]->(b) MERGE (a)-[:WORKS_AT]->(b)");
    assert_eq!(
        run(
            &db,
            "MATCH (:Person {entity_id:'dani'})-[r:WORKS_AT]->() RETURN count(r)"
        ),
        vec![vec![v(1)]]
    );
}

#[test]
fn cursor_pages_follow_entity_id_order_and_stop_early() {
    let (_dir, db) = setup();
    let mut after = String::new();
    let mut seen = vec![];
    loop {
        let page = run(&db, &format!("MATCH (p:Person) WHERE p.entity_id > '{after}' AND p.age < 40 RETURN p.entity_id AS id ORDER BY id LIMIT 2"));
        if page.is_empty() {
            break;
        }
        for row in &page {
            let Out::Value(Value::Str(id)) = &row[0] else {
                panic!()
            };
            seen.push(id.clone());
            after = id.clone();
        }
    }
    assert_eq!(seen, ["ana", "bia", "dani"]);
    assert_eq!(
        run(
            &db,
            "MATCH (n:Car) RETURN n.entity_id ORDER BY n.entity_id SKIP 1 LIMIT 1"
        ),
        vec![vec![v("c2")]]
    );
    assert_eq!(
        run(
            &db,
            "MATCH (n:Car) RETURN n.entity_id ORDER BY n.entity_id DESC LIMIT 1"
        ),
        vec![vec![v("c3")]]
    );
}

#[test]
fn unwind_merge_set_upserts_in_one_write_and_keeps_set_semantics() {
    let (_dir, db) = setup();
    let params = BTreeMap::from([(
        "rows".to_string(),
        Value::List(vec![
            Value::Map(BTreeMap::from([
                ("id".into(), Value::Str("ana".into())),
                ("age".into(), Value::Int(50)),
                ("city".into(), Value::Null),
            ])),
            Value::Map(BTreeMap::from([
                ("id".into(), Value::Str("zoe".into())),
                ("age".into(), Value::Int(9)),
                ("city".into(), Value::Str("BH".into())),
            ])),
        ]),
    )]);
    let query =
        "UNWIND $rows AS r MERGE (n:Person {entity_id: r.id}) SET n.age = r.age, n.city = r.city";
    execute(&db, query, &params).unwrap();
    assert_eq!(
        run(&db, "MATCH (n:Person) WHERE n.entity_id IN ['ana', 'zoe'] RETURN n.entity_id, n.name, n.age, n.city ORDER BY n.entity_id"),
        vec![vec![v("ana"), v("Ana"), v(50), v(())], vec![v("zoe"), v(()), v(9), v("BH")]]
    );
    // A SET that reads the merged node still runs after the MERGE.
    run(
        &db,
        "MERGE (n:Person {entity_id: 'zoe'}) SET n.age = n.age + 1",
    );
    assert_eq!(
        run(&db, "MATCH (n:Person {entity_id: 'zoe'}) RETURN n.age"),
        vec![vec![v(10)]]
    );
}

#[test]
fn large_label_scans_split_across_workers_match_sequential_results() {
    let (_dir, db) = setup();
    // Above the parallel threshold (50k), in batches.
    for start in (0..60_000).step_by(10_000) {
        run(&db, &format!("UNWIND range({start}, {}) AS i CREATE (:Car {{entity_id: 'bulk' + i, brand: ['A', 'B', 'C'][i % 3], year: i % 7}})", start + 9_999));
    }
    run(&db, "MATCH (p:Person {entity_id: 'ana'}), (c:Car) WHERE c.year = 6 AND c.entity_id STARTS WITH 'bulk' MERGE (p)-[:OWNS]->(c)");
    assert_eq!(
        run(&db, "MATCH (c:Car) WHERE c.entity_id STARTS WITH 'bulk' RETURN c.brand AS b, count(*) AS n, sum(c.year) AS s, min(c.year) AS lo, max(c.year) AS hi ORDER BY b"),
        vec![
            vec![v("A"), v(20_000), v(59_997), v(0), v(6)],
            vec![v("B"), v(20_000), v(59_998), v(0), v(6)],
            vec![v("C"), v(20_000), v(59_999), v(0), v(6)]
        ]
    );
    assert_eq!(
        run(&db, "MATCH (c:Car) WHERE c.year < 100 RETURN c.entity_id AS id ORDER BY c.year DESC, id LIMIT 2"),
        vec![vec![v("bulk1000")], vec![v("bulk10002")]]
    );
    // A non-aggregating scan keeps sequential (id) order across chunks.
    let rows = run(&db, "MATCH (c:Car) WHERE c.year = 3 RETURN c.entity_id");
    assert_eq!(rows.len(), 8_571);
    assert_eq!(rows[0], vec![v("bulk3")]);
    assert_eq!(rows.last().unwrap(), &vec![v("bulk59993")]);
    assert_eq!(
        run(
            &db,
            "MATCH (c:Car)<-[:OWNS]-(p:Person) WHERE c.year = 6 RETURN p.name, count(*) AS n"
        ),
        vec![vec![v("Ana"), v(8_571)]]
    );
}
