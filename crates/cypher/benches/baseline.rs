//! Reproducible engine benchmark for the read/write baseline (PERF.md).
//!
//!   cargo bench -p brainwire-cypher --bench baseline -- load DIR [rows] [threads]
//!   cargo bench -p brainwire-cypher --bench baseline -- read DIR
//!   cargo bench -p brainwire-cypher --bench baseline -- write DIR [rows] [sessions...]
//!   cargo bench -p brainwire-cypher --bench baseline -- check DIR
//!
//! `load` builds four synthetic labels (Usuario, Carro, Endereco, Casa) with
//! `rows` nodes each and four relationship types with `rows` edges each,
//! through Cypher batches of 1000 on `threads` concurrent transactions.
//! `read` runs each query once to warm up, then reports the median of three.
//! `write` upserts disjoint synthetic rows (read state, MERGE, stamp ACL in one
//! transaction, as an ingestion writer does) and reports rows/s and retries.
//! Every result is one JSON line on stdout.
use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{ExecError, ExecutionLimits, Out, QueryTransaction};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const BATCH: usize = 1000;
const BRANDS: [&str; 4] = ["MarcaA", "MarcaB", "MarcaC", "MarcaD"];
const STATES: [&str; 6] = ["SP", "RJ", "MG", "RS", "PR", "BA"];

fn limits() -> ExecutionLimits {
    ExecutionLimits {
        memory_bytes: 2 << 30,
        timeout: None,
    }
}

fn open(path: &Path) -> Database {
    let resources = Resources::new(StoreConfig {
        cache_bytes: 1 << 30,
        memtable_bytes: 256 << 20,
        id_block: 1024,
        conflict_history_keys: 1_000_000,
    });
    Database::open(path, &resources).expect("open database")
}

fn s(text: impl Into<String>) -> Value {
    Value::Str(text.into())
}

fn map(entries: Vec<(&str, Value)>) -> Value {
    Value::Map(entries.into_iter().map(|(k, v)| (k.into(), v)).collect())
}

fn params(entries: Vec<(&str, Value)>) -> BTreeMap<String, Value> {
    entries.into_iter().map(|(k, v)| (k.into(), v)).collect()
}

/// Run statements in one transaction, retrying the whole transaction on conflict.
fn transact(db: &Database, statements: &[(&str, BTreeMap<String, Value>)], retries: &AtomicU64) {
    loop {
        let mut tx = QueryTransaction::begin_with_limits(db, limits()).expect("begin");
        let result = statements
            .iter()
            .try_for_each(|(query, params)| tx.run(query, params).map(drop))
            .and_then(|()| tx.commit().map(drop));
        match result {
            Ok(()) => return,
            Err(ExecError::Conflict) => {
                retries.fetch_add(1, Ordering::Relaxed);
            }
            Err(error) => panic!("{error}"),
        }
    }
}

fn ontology() -> GraphMapping {
    serde_json::from_value(serde_json::json!({
        "version": "1",
        "nodes": [
            {"label": "Usuario", "properties": ["usuario_id", "cpf_teste", "nome_teste", "email_teste", "updated_at"],
             "indexed_fields": ["cpf_teste", "email_teste"]},
            {"label": "Carro", "properties": ["carro_id", "usuario_id", "codigo_teste", "marca_teste", "ano", "updated_at"],
             "indexed_fields": ["marca_teste", "ano"]},
            {"label": "Endereco", "properties": ["endereco_id", "usuario_id", "logradouro_teste", "numero_teste",
             "bairro_teste", "cidade_teste", "uf_teste"], "indexed_fields": ["cidade_teste", "uf_teste"]},
            {"label": "Casa", "properties": ["casa_id", "usuario_id", "endereco_id", "codigo_teste", "tipo_teste",
             "area_m2", "updated_at"], "indexed_fields": ["tipo_teste", "area_m2"]},
            {"label": "Pessoa", "properties": ["pessoa_id", "nome", "email", "updated_at"], "indexed_fields": ["email"]}
        ],
        "edges": [
            {"type": "TEM_CARRO", "from": {"label": "Usuario"}, "to": {"label": "Carro"}},
            {"type": "TEM_ENDERECO", "from": {"label": "Usuario"}, "to": {"label": "Endereco"}},
            {"type": "TEM_CASA", "from": {"label": "Usuario"}, "to": {"label": "Casa"}},
            {"type": "CASA_FICA_EM", "from": {"label": "Casa"}, "to": {"label": "Endereco"}}
        ]
    }))
    .expect("ontology")
}

fn node_row(label: &str, i: i64) -> Value {
    let updated = s("2026-10-01T00:00:00Z");
    match label {
        "Usuario" => map(vec![
            ("id", s(format!("usr-{i:08}"))),
            ("usuario_id", Value::Int(i)),
            ("cpf_teste", s(format!("{i:011}"))),
            ("nome_teste", s(format!("Nome {i}"))),
            ("email_teste", s(format!("usuario{i}@example.test"))),
            ("updated_at", updated),
        ]),
        "Carro" => map(vec![
            ("id", s(format!("car-{i:08}"))),
            ("carro_id", Value::Int(i)),
            ("usuario_id", Value::Int(i)),
            ("codigo_teste", s(format!("CAR{i}"))),
            ("marca_teste", s(BRANDS[(i % 4) as usize])),
            ("ano", Value::Int(2000 + i % 25)),
            ("updated_at", updated),
        ]),
        "Endereco" => map(vec![
            ("id", s(format!("end-{i:08}"))),
            ("endereco_id", Value::Int(i)),
            ("usuario_id", Value::Int(i)),
            ("logradouro_teste", s(format!("Rua {i}"))),
            ("numero_teste", Value::Int(i % 1000)),
            ("bairro_teste", s(format!("Bairro {}", i % 500))),
            ("cidade_teste", s(format!("Cidade {}", i % 100))),
            ("uf_teste", s(STATES[(i % 6) as usize])),
        ]),
        "Casa" => map(vec![
            ("id", s(format!("cas-{i:08}"))),
            ("casa_id", Value::Int(i)),
            ("usuario_id", Value::Int(i)),
            ("endereco_id", Value::Int(i)),
            ("codigo_teste", s(format!("CASA{i}"))),
            (
                "tipo_teste",
                s(["casa", "apto", "sobrado"][(i % 3) as usize]),
            ),
            ("area_m2", Value::Int(40 + i % 300)),
            ("updated_at", updated),
        ]),
        _ => unreachable!(),
    }
}

fn node_query(label: &str) -> String {
    let props: Vec<&str> = match label {
        "Usuario" => vec![
            "usuario_id",
            "cpf_teste",
            "nome_teste",
            "email_teste",
            "updated_at",
        ],
        "Carro" => vec![
            "carro_id",
            "usuario_id",
            "codigo_teste",
            "marca_teste",
            "ano",
            "updated_at",
        ],
        "Endereco" => vec![
            "endereco_id",
            "usuario_id",
            "logradouro_teste",
            "numero_teste",
            "bairro_teste",
            "cidade_teste",
            "uf_teste",
        ],
        "Casa" => vec![
            "casa_id",
            "usuario_id",
            "endereco_id",
            "codigo_teste",
            "tipo_teste",
            "area_m2",
            "updated_at",
        ],
        _ => unreachable!(),
    };
    let set: Vec<String> = props.iter().map(|p| format!("n.{p} = r.{p}")).collect();
    format!(
        "UNWIND $rows AS r MERGE (n:{label} {{entity_id: r.id}}) SET {}",
        set.join(", ")
    )
}

const EDGES: [(&str, &str, &str, &str, &str); 4] = [
    ("TEM_CARRO", "Usuario", "usr", "Carro", "car"),
    ("TEM_ENDERECO", "Usuario", "usr", "Endereco", "end"),
    ("TEM_CASA", "Usuario", "usr", "Casa", "cas"),
    ("CASA_FICA_EM", "Casa", "cas", "Endereco", "end"),
];

/// Run `work(batch_start)` over [0, rows) in batches on `threads` threads.
fn parallel(rows: i64, threads: usize, work: &(dyn Fn(i64) + Sync)) {
    let next = AtomicU64::new(0);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| loop {
                let start = next.fetch_add(BATCH as u64, Ordering::Relaxed) as i64;
                if start >= rows {
                    break;
                }
                work(start);
            });
        }
    });
}

fn load(path: &Path, rows: i64, threads: usize) {
    assert!(!path.exists(), "refusing to reuse {}", path.display());
    let db = open(path);
    db.apply_ontology(&ontology()).expect("apply ontology");
    let retries = AtomicU64::new(0);
    for label in ["Usuario", "Carro", "Endereco", "Casa"] {
        let query = node_query(label);
        let started = Instant::now();
        parallel(rows, threads, &|start| {
            let batch = (start..(start + BATCH as i64).min(rows))
                .map(|i| node_row(label, i))
                .collect();
            transact(
                &db,
                &[(&query, params(vec![("rows", Value::List(batch))]))],
                &retries,
            );
        });
        report("load_nodes", label, rows, started.elapsed(), &retries);
    }
    for (edge, from, from_prefix, to, to_prefix) in EDGES {
        let query = format!(
            "UNWIND $rows AS r MATCH (a:{from} {{entity_id: r.s}}), (b:{to} {{entity_id: r.t}}) MERGE (a)-[:{edge}]->(b)"
        );
        let started = Instant::now();
        parallel(rows, threads, &|start| {
            let batch = (start..(start + BATCH as i64).min(rows))
                .map(|i| {
                    map(vec![
                        ("s", s(format!("{from_prefix}-{i:08}"))),
                        ("t", s(format!("{to_prefix}-{i:08}"))),
                    ])
                })
                .collect();
            transact(
                &db,
                &[(&query, params(vec![("rows", Value::List(batch))]))],
                &retries,
            );
        });
        report("load_edges", edge, rows, started.elapsed(), &retries);
    }
}

fn report(phase: &str, name: &str, rows: i64, elapsed: Duration, retries: &AtomicU64) {
    let seconds = elapsed.as_secs_f64();
    println!(
        "{}",
        serde_json::json!({"phase": phase, "name": name, "rows": rows, "seconds": round(seconds),
            "rows_per_s": (rows as f64 / seconds).round(), "conflict_retries": retries.swap(0, Ordering::Relaxed)})
    );
}

fn round(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

fn run(db: &Database, query: &str, params: &BTreeMap<String, Value>) -> Vec<Vec<Out>> {
    let mut tx = QueryTransaction::begin_with_limits(db, limits()).expect("begin");
    let rows = tx
        .run(query, params)
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .rows;
    tx.commit().expect("commit");
    rows
}

fn show(rows: &[Vec<Out>]) -> serde_json::Value {
    let value = |out: &Out| match out {
        Out::Value(Value::Int(n)) => serde_json::json!(n),
        Out::Value(Value::Str(text)) => serde_json::json!(text),
        other => serde_json::json!(format!("{other:?}")),
    };
    rows.iter()
        .take(2)
        .map(|row| row.iter().map(value).collect::<Vec<_>>())
        .collect()
}

fn read(path: &Path) {
    let db = open(path);
    let ids = || Value::List((424_200..424_300).map(Value::Int).collect());
    let entity_ids = || {
        Value::List(
            (424_200..424_300)
                .map(|i| s(format!("usr-{i:08}")))
                .collect(),
        )
    };
    let emails = || {
        Value::List(
            (424_200..424_300)
                .map(|i| s(format!("usuario{i}@example.test")))
                .collect(),
        )
    };
    let queries: Vec<(&str, &str, BTreeMap<String, Value>)> = vec![
        ("count_pairs", "MATCH (u:Usuario)-[:TEM_CARRO]->(c:Carro) RETURN count(*) AS pairs", params(vec![])),
        ("top_brands", "MATCH (:Usuario)-[:TEM_CARRO]->(c:Carro) RETURN c.marca_teste AS marca, count(*) AS n ORDER BY n DESC LIMIT 5", params(vec![])),
        ("houses_per_state", "MATCH (c:Casa)-[:CASA_FICA_EM]->(e:Endereco) RETURN e.uf_teste AS uf, count(c) AS casas ORDER BY casas DESC LIMIT 5", params(vec![])),
        ("indexed_lookup", "MATCH (u:Usuario) WHERE u.email_teste = 'usuario424242@example.test' RETURN u.usuario_id", params(vec![])),
        ("in_100_unindexed", "MATCH (u:Usuario) WHERE u.usuario_id IN $ids RETURN count(u)", params(vec![("ids", ids())])),
        ("in_100_entity_ids", "MATCH (u:Usuario) WHERE u.entity_id IN $ids RETURN count(u)", params(vec![("ids", entity_ids())])),
        ("in_100_indexed", "MATCH (u:Usuario) WHERE u.email_teste IN $ids RETURN count(u)", params(vec![("ids", emails())])),
        ("one_hop_unindexed", "MATCH (u:Usuario {usuario_id: 424242})-[:TEM_CARRO]->(c:Carro) RETURN c.carro_id", params(vec![])),
        ("count_label", "MATCH (u:Usuario) RETURN count(u)", params(vec![])),
    ];
    for (name, query, params) in &queries {
        let rows = run(&db, query, params);
        let mut times = vec![];
        for _ in 0..3 {
            let started = Instant::now();
            run(&db, query, params);
            times.push(started.elapsed().as_secs_f64());
        }
        times.sort_by(f64::total_cmp);
        println!(
            "{}",
            serde_json::json!({"phase": "read", "name": name, "median_s": round(times[1]), "rows": show(&rows)})
        );
    }
    // Cursor pagination over the whole label, capped at two minutes (extrapolated).
    let page = "MATCH (n:Usuario) WHERE n.entity_id > $after RETURN n.entity_id AS id ORDER BY id LIMIT 1000";
    let total = run(&db, "MATCH (n:Usuario) RETURN count(n)", &params(vec![]));
    let Out::Value(Value::Int(total)) = total[0][0] else {
        panic!("count")
    };
    let (mut after, mut seen, started) = (String::new(), 0i64, Instant::now());
    while started.elapsed() < Duration::from_secs(120) {
        let rows = run(&db, page, &params(vec![("after", s(after.clone()))]));
        seen += rows.len() as i64;
        match rows.last() {
            Some(row) => match &row[0] {
                Out::Value(Value::Str(id)) => after = id.clone(),
                other => panic!("{other:?}"),
            },
            None => break,
        }
        if rows.len() < 1000 {
            break;
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
    println!(
        "{}",
        serde_json::json!({"phase": "read", "name": "cursor_walk", "rows_seen": seen, "total": total,
            "seconds": round(elapsed), "extrapolated_s": round(elapsed * total as f64 / seen.max(1) as f64)})
    );
}

/// `BENCH_READ_FIRST=1` also reads the batch's current state inside each
/// write transaction, as an ingestion writer that merges with existing rows does.
fn write(path: &Path, rows: i64, sessions: &[usize]) {
    let db = open(path);
    let read_first = std::env::var("BENCH_READ_FIRST").is_ok_and(|v| v == "1");
    let read = "MATCH (n:Pessoa) WHERE n.entity_id IN $ids RETURN n.entity_id, n.updated_at";
    let merge = "UNWIND $rows AS r MERGE (n:Pessoa {entity_id: r.id}) SET n.pessoa_id = r.pessoa_id, n.nome = r.nome, n.email = r.email, n.updated_at = r.updated_at";
    let acl = "CALL db.setNodeAcls($acls) YIELD updated RETURN updated";
    let runs = AtomicU64::new(0);
    for &threads in sessions {
        let retries = AtomicU64::new(0);
        let run_id = runs.fetch_add(1, Ordering::Relaxed);
        let prefix = format!("p{}-{}", std::process::id(), run_id);
        let started = Instant::now();
        parallel(rows, threads, &|start| {
            let range = start..(start + BATCH as i64).min(rows);
            let id = |i: i64| format!("{prefix}-{i:08}");
            let ids = Value::List(range.clone().map(|i| s(id(i))).collect());
            let batch = range
                .clone()
                .map(|i| {
                    map(vec![
                        ("id", s(id(i))),
                        ("pessoa_id", Value::Int(i)),
                        ("nome", s(format!("Pessoa {i}"))),
                        ("email", s(format!("{prefix}-{i}@example.test"))),
                        ("updated_at", s("2026-10-01T00:00:00Z")),
                    ])
                })
                .collect();
            let acls = range
                .map(|i| {
                    map(vec![
                        ("label", s("Pessoa")),
                        ("entityId", s(id(i))),
                        ("allow", Value::List(vec![s(format!("user-{}", i % 50))])),
                        ("connectorIds", Value::List(vec![s("connector-1")])),
                    ])
                })
                .collect();
            let mut statements = vec![
                (merge, params(vec![("rows", Value::List(batch))])),
                (acl, params(vec![("acls", Value::List(acls))])),
            ];
            if read_first {
                statements.insert(0, (read, params(vec![("ids", ids)])));
            }
            transact(&db, &statements, &retries);
        });
        let name = format!(
            "sessions_{threads}{}",
            if read_first { "_read_first" } else { "" }
        );
        report("write", &name, rows, started.elapsed(), &retries);
    }
}

fn main() {
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|arg| arg != "--bench")
        .collect();
    let number = |index: usize, default: i64| -> i64 {
        args.get(index)
            .map_or(default, |n| n.parse().expect("number"))
    };
    match args.first().map(String::as_str) {
        Some("load") => load(
            Path::new(&args[1]),
            number(2, 1_000_000),
            number(3, 8) as usize,
        ),
        Some("read") => read(Path::new(&args[1])),
        Some("write") => {
            let sessions: Vec<usize> = args[3..]
                .iter()
                .map(|n| n.parse().expect("sessions"))
                .collect();
            let sessions = if sessions.is_empty() {
                vec![1, 4, 8, 16]
            } else {
                sessions
            };
            write(Path::new(&args[1]), number(2, 200_000), &sessions)
        }
        Some("check") => {
            let db = open(Path::new(&args[1]));
            let status = brainwire_store::graph::check_invariants(&db);
            println!(
                "{}",
                serde_json::json!({"phase": "check", "status": if status.is_ok() { "ok".to_string() } else { format!("{status:?}") }})
            );
        }
        _ => eprintln!("usage: baseline load|read|write|check DIR [...]"),
    }
}
