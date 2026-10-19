//! Isolated warm-cache comparison of DETACH DELETE's adjacency collection helpers.
//! cargo run --release --locked --offline -p brainwire-store --example detach_delete -- --degree 10000 --payload-bytes 256 --repeats 9 --full-delete
use std::{collections::BTreeMap, error::Error, hint::black_box, time::Instant};

use brainwire_core::{
    codec::{encode_row, Row},
    ids::{EdgeId, EdgeTypeId, NodeId},
    keys::{get_u16, get_u64},
    schema::GraphMapping,
    value::Value,
};
use brainwire_store::{
    db::{Database, Resources, StoreConfig, StoreError, COLUMN_FAMILIES},
    graph::{check_invariants, detach_delete, get_node, merge_node, nodes_of_type, EdgeRef},
    txn::{Cf, Txn},
};

struct Args {
    degree: usize,
    payload_bytes: usize,
    repeats: usize,
    full_delete: bool,
}

fn arguments() -> Result<Option<Args>, Box<dyn Error>> {
    let mut options = Args {
        degree: 10_000,
        payload_bytes: 256,
        repeats: 9,
        full_delete: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        if flag == "--help" || flag == "-h" {
            println!("detach_delete [--degree N] [--payload-bytes N] [--repeats N] [--full-delete]\n\
                degree: outgoing AND incoming hub relationships (1..=100000); \
                payload-bytes: repeated ASCII bytes per outgoing row (0..=65536); \
                repeats: alternating paired warm-cache scans (3..=101).\n\
                Combined encoded adjacency data is capped at 512 MiB. \
                Uses a temporary database, prints JSON, and removes the fixture on exit.\n\
                By default measures adjacency collection only, not delete staging, WAL sync, RSS, or end-to-end Cypher. \
                --full-delete additionally measures the production DETACH DELETE helper through durable commit, then audits invariants.");
            return Ok(None);
        }
        if flag == "--full-delete" {
            options.full_delete = true;
            continue;
        }
        if !matches!(flag.as_str(), "--degree" | "--payload-bytes" | "--repeats") {
            return Err(format!("unknown argument {flag}; use --help").into());
        }
        let value: usize = args.next().ok_or("missing option value")?.parse()?;
        match flag.as_str() {
            "--degree" => options.degree = value,
            "--payload-bytes" => options.payload_bytes = value,
            "--repeats" => options.repeats = value,
            _ => unreachable!(),
        }
    }
    if !(1..=100_000).contains(&options.degree)
        || options.payload_bytes > 65_536
        || !(3..=101).contains(&options.repeats)
    {
        return Err("arguments outside supported bounds; use --help".into());
    }
    // Include both mirrors and conservative row overhead in the fixture bound.
    if options.degree as u64 * 2 * (options.payload_bytes as u64 + 128) > 512 << 20 {
        return Err("fixture exceeds 512 MiB encoded adjacency cap".into());
    }
    Ok(Some(options))
}

fn edge_key(node: NodeId, ty: EdgeTypeId, other: NodeId, id: EdgeId) -> Vec<u8> {
    let mut key = Vec::with_capacity(26);
    key.extend_from_slice(&node.0.to_be_bytes());
    key.extend_from_slice(&ty.0.to_be_bytes());
    key.extend_from_slice(&other.0.to_be_bytes());
    key.extend_from_slice(&id.0.to_be_bytes());
    key
}

fn decode(key: &[u8]) -> Result<EdgeRef, StoreError> {
    if key.len() != 26 {
        return Err(StoreError::Corrupt("expected 26-byte adjacency key".into()));
    }
    let mut input = key;
    let _node = get_u64(&mut input)?;
    Ok(EdgeRef {
        edge_type: EdgeTypeId(get_u16(&mut input)?),
        other: NodeId(get_u64(&mut input)?),
        id: EdgeId(get_u64(&mut input)?),
    })
}

fn collect(txn: &mut Txn<'_>, hub: NodeId, streaming: bool) -> Result<Vec<EdgeRef>, StoreError> {
    let mut found = Vec::new();
    for cf in [Cf::Out, Cf::In] {
        let prefix = hub.0.to_be_bytes();
        if streaming {
            txn.visit_prefix_for_update::<StoreError>(cf, &prefix, |key, _| {
                found.push(decode(key)?);
                Ok(())
            })?;
        } else {
            for (key, _) in txn.scan_prefix_for_update(cf, &prefix)? {
                found.push(decode(&key)?);
            }
        }
    }
    Ok(found)
}

fn median(samples: &[f64]) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    } else {
        sorted[middle]
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let Some(args) = arguments()? else {
        return Ok(());
    };
    let directory = tempfile::tempdir()?;
    let config = StoreConfig {
        cache_bytes: 256 << 20,
        memtable_bytes: 32 << 20,
        id_block: 4096,
        conflict_history_keys: 50_000,
    };
    let resources = Resources::new(config);
    let database = Database::open(directory.path(), &resources)?;
    let mapping: GraphMapping = serde_json::from_str(
        r#"{"version":"1","nodes":[{"label":"Item"}],"edges":[{"type":"LINK","from":{"label":"Item"},"to":{"label":"Item"}}]}"#,
    )?;
    let catalog = database.apply_ontology(&mapping)?;
    let ty = catalog.edge_type("LINK").ok_or("missing edge type")?.id;
    let mut txn = database.begin();
    let hub = merge_node(&mut txn, &catalog, "Item", "hub", &BTreeMap::new())?;
    txn.commit()?;
    let row = encode_row(&Row {
        schema_version: catalog.version,
        extra: BTreeMap::from([("payload".into(), Value::Str("x".repeat(args.payload_bytes)))]),
        ..Row::default()
    });
    for first in (0..args.degree).step_by(500) {
        let mut txn = database.begin();
        for index in first..(first + 500).min(args.degree) {
            let peer = merge_node(
                &mut txn,
                &catalog,
                "Item",
                &format!("peer-{index}"),
                &BTreeMap::new(),
            )?;
            for (src, dst) in [(hub, peer), (peer, hub)] {
                let id = txn.next_edge_id()?;
                txn.put(Cf::Out, edge_key(src, ty, dst, id), row.clone());
                txn.put(Cf::In, edge_key(dst, ty, src, id), Vec::new());
            }
        }
        txn.commit()?;
    }
    drop(database);
    // Replay/flush every family before measuring, so the fixture is read from SSTs.
    let raw = rocksdb::DB::open_cf(
        &rocksdb::Options::default(),
        directory.path(),
        COLUMN_FAMILIES,
    )?;
    for family in COLUMN_FAMILIES {
        raw.flush_cf(raw.cf_handle(family).ok_or("missing family")?)?;
    }
    drop(raw);
    let database = Database::open(directory.path(), &resources)?;
    check_invariants(&database)?;
    let expected = collect(&mut database.begin(), hub, false)?;
    assert_eq!(expected.len(), args.degree * 2);
    assert_eq!(expected, collect(&mut database.begin(), hub, true)?);
    let mut materialized = Vec::new();
    let mut streaming = Vec::new();
    for repetition in 0..args.repeats {
        for method in if repetition % 2 == 0 {
            [false, true]
        } else {
            [true, false]
        } {
            let mut txn = database.begin();
            let started = Instant::now();
            let result = black_box(collect(&mut txn, hub, method)?);
            let milliseconds = started.elapsed().as_secs_f64() * 1000.0;
            // Comparison and vector destruction are deliberately outside timing.
            assert_eq!(result, expected);
            if method {
                &mut streaming
            } else {
                &mut materialized
            }
            .push(milliseconds);
        }
    }
    let full_delete_ms = if args.full_delete {
        let mut txn = database.begin();
        let started = Instant::now();
        detach_delete(&mut txn, hub)?;
        txn.commit()?;
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        check_invariants(&database)?;
        let audit = database.begin();
        assert!(get_node(&audit, hub)?.is_none());
        let remaining_nodes = nodes_of_type(&audit, hub.type_id())?.len();
        let remaining_out = audit.scan_prefix(Cf::Out, &[])?.len();
        let remaining_in = audit.scan_prefix(Cf::In, &[])?.len();
        assert_eq!(remaining_nodes, args.degree);
        assert_eq!(remaining_out, 0);
        assert_eq!(remaining_in, 0);
        Some(elapsed)
    } else {
        None
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "benchmark": "detach_delete_adjacency_collection",
            "scope": "warm-cache Out+In scans including EdgeRef vector construction; excludes transaction creation/drop, vector drop, delete staging and commit",
            "fixture": {
                "nodes": args.degree + 1,
                "outgoing_degree": args.degree,
                "incoming_degree": args.degree,
                "total_hub_degree": args.degree * 2,
                "logical_relationships": args.degree * 2,
                "adjacency_records": args.degree * 4,
                "payload_bytes": args.payload_bytes,
                "encoded_out_row_bytes": row.len(),
                "in_value_bytes": 0,
                "payload_pattern": "repeated ASCII x; compressible synthetic data",
                "sst_flushed_before_reopen": true,
                "invariants": "passed"
            },
            "config": {
                "cache_bytes": config.cache_bytes,
                "memtable_bytes": config.memtable_bytes,
                "id_block": config.id_block,
                "conflict_history_keys": config.conflict_history_keys,
                "repeats": args.repeats,
                "order": "materialized first on even repetitions, streaming first on odd repetitions",
                "warmup": "invariant check plus one full scan per method"
            },
            "materialized": {"median_ms": median(&materialized), "samples_ms": materialized},
            "streaming": {"median_ms": median(&streaming), "samples_ms": streaming},
            "median_speedup": median(&materialized) / median(&streaming),
            "exact_vectors_equal": true,
            "full_detach_delete": full_delete_ms.map(|elapsed_ms| serde_json::json!({
                "elapsed_ms": elapsed_ms,
                "scope": "production detach_delete helper plus transaction commit; includes WAL sync, excludes fixture construction and invariant audit",
                "invariants_after_commit": "passed",
                "hub_absent_after_commit": true,
                "remaining_nodes": args.degree,
                "remaining_out_records": 0,
                "remaining_in_records": 0
            }))
        }))?
    );
    Ok(())
}
