//! Commits a stream of graph writes and acknowledges each completed commit.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use brainwire_core::schema::GraphMapping;
use brainwire_core::value::Value;
use brainwire_store::db::{Database, Resources, StoreConfig};
use brainwire_store::graph::{merge_edge, merge_node};

fn main() {
    let path = std::env::args().nth(1).expect("database path");
    let start: u64 = std::env::args()
        .nth(2)
        .expect("first number")
        .parse()
        .expect("a number");
    let sequence_ack = std::env::args().nth(3).as_deref() == Some("--sequence-ack");
    let res = Resources::new(StoreConfig::for_tests());
    let db = Database::open(Path::new(&path), &res).expect("open");
    let mapping: GraphMapping = serde_json::from_str(include_str!(
        "../../../core/tests/fixtures/graph_mapping.json"
    ))
    .expect("fixture");
    let catalog = db.apply_ontology(&mapping).expect("ontology");
    let mut out = std::io::stdout();

    let mut i = start;
    loop {
        let mut txn = db.begin();
        let mut props = BTreeMap::new();
        props.insert("name".to_string(), Value::Str(format!("customer {i}")));
        if sequence_ack {
            props.insert(
                "segment".to_string(),
                Value::Str(format!("segment {}", i % 3)),
            );
        }
        let customer = merge_node(&mut txn, &catalog, "Customer", &format!("c:{i}"), &props)
            .expect("customer");
        let company = merge_node(
            &mut txn,
            &catalog,
            "Company",
            &format!("k:{}", i % 100),
            &BTreeMap::new(),
        )
        .expect("company");
        merge_edge(
            &mut txn,
            &catalog,
            customer,
            "WORKS_AT",
            company,
            &BTreeMap::new(),
        )
        .expect("edge");
        let sequence = txn.commit().expect("commit");
        if sequence_ack {
            writeln!(out, "{i} {sequence}").expect("stdout");
        } else {
            writeln!(out, "{i}").expect("stdout");
        }
        out.flush().expect("flush");
        i = i.checked_add(1).expect("commit number exhausted");
    }
}
