use std::collections::BTreeMap;

use brainwire_core::acl::NodeAcl;
use brainwire_core::schema::GraphMapping;
use brainwire_core::value::Value;
use brainwire_store::db::{Database, Resources, StoreConfig};
use brainwire_store::graph::{check_invariants, merge_edge, merge_node, set_node_acl};
use brainwire_store::replicated_apply::{
    applied_state, apply, ApplyError, ApplyLimits, Batch, Mutation,
};
use brainwire_store::txn::Cf;

const FAMILIES: [Cf; 11] = [
    Cf::Meta,
    Cf::Node,
    Cf::Pk,
    Cf::Out,
    Cf::In,
    Cf::Lbl,
    Cf::Idx,
    Cf::Ident,
    Cf::Event,
    Cf::Acl,
    Cf::Note,
];
fn mutation(cf: &str, key: &[u8], value: Option<&[u8]>) -> Mutation {
    Mutation {
        cf: cf.into(),
        key: key.to_vec(),
        value: value.map(<[u8]>::to_vec),
    }
}
fn batch(index: u64) -> Batch {
    Batch {
        version: 1,
        index,
        mutations: vec![
            mutation("note", b"a", Some(b"one")),
            mutation("note", b"b", Some(b"two")),
        ],
    }
}
fn open() -> (tempfile::TempDir, Resources, Database) {
    let dir = tempfile::tempdir().unwrap();
    let resources = Resources::new(StoreConfig::for_tests());
    let database = Database::open(dir.path(), &resources).unwrap();
    (dir, resources, database)
}
fn logical(database: &Database) -> Vec<Mutation> {
    let tx = database.begin();
    let mut rows = Vec::new();
    for cf in FAMILIES {
        for (key, value) in tx.scan_prefix(cf, &[]).unwrap() {
            if cf == Cf::Meta
                && (key == b"commit/sequence" || key.starts_with(b"replicated-apply/"))
            {
                continue;
            }
            rows.push(Mutation {
                cf: cf.name().into(),
                key,
                value: Some(value),
            });
        }
    }
    rows.sort_by(|a, b| (&a.cf, &a.key).cmp(&(&b.cf, &b.key)));
    rows
}

#[test]
fn canonical_roundtrip_sorting_digest_and_empty_delete_distinction() {
    let first = batch(1);
    let mut reversed = first.clone();
    reversed.mutations.reverse();
    let limits = ApplyLimits::default();
    assert_eq!(
        first.canonical_bytes(limits).unwrap(),
        reversed.canonical_bytes(limits).unwrap()
    );
    assert_eq!(
        first.digest(limits).unwrap(),
        reversed.digest(limits).unwrap()
    );
    assert_eq!(
        Batch::from_canonical_bytes(&first.canonical_bytes(limits).unwrap(), limits).unwrap(),
        first
    );
    let empty = Batch {
        version: 1,
        index: 1,
        mutations: vec![mutation("note", b"a", Some(b""))],
    };
    let deletion = Batch {
        version: 1,
        index: 1,
        mutations: vec![mutation("note", b"a", None)],
    };
    assert_ne!(
        empty.digest(limits).unwrap(),
        deletion.digest(limits).unwrap()
    );
    let bytes = first.canonical_bytes(limits).unwrap();
    let golden_digest: String = first
        .digest(limits)
        .unwrap()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(
        golden_digest,
        "47eabdc65fb10bce06d1c56061776c094c7091208710963619153fe1145f1def"
    );
    for n in 0..bytes.len() {
        assert!(Batch::from_canonical_bytes(&bytes[..n], limits).is_err());
    }
    assert_eq!(&bytes[..17], b"BWAP\x01\0\0\0\0\0\0\0\x01\0\0\0\x02");
    let mut noncanonical = bytes[..17].to_vec();
    noncanonical.extend_from_slice(&bytes[35..]);
    noncanonical.extend_from_slice(&bytes[17..35]);
    assert!(matches!(
        Batch::from_canonical_bytes(&noncanonical, limits),
        Err(ApplyError::Invalid("noncanonical entry order"))
    ));
    let mut key_announcement = bytes.clone();
    key_announcement[22..26].copy_from_slice(&1000u32.to_be_bytes());
    assert!(matches!(
        Batch::from_canonical_bytes(
            &key_announcement,
            ApplyLimits {
                max_key_bytes: 1,
                ..limits
            }
        ),
        Err(ApplyError::Bounds("max_key_bytes"))
    ));
    let mut value_announcement = bytes.clone();
    value_announcement[28..32].copy_from_slice(&1000u32.to_be_bytes());
    assert!(matches!(
        Batch::from_canonical_bytes(
            &value_announcement,
            ApplyLimits {
                max_value_bytes: 3,
                ..limits
            }
        ),
        Err(ApplyError::Bounds("max_value_bytes"))
    ));
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(Batch::from_canonical_bytes(&trailing, limits).is_err());
    let mut announcements = bytes[..17].to_vec();
    announcements[13..17].copy_from_slice(&100000u32.to_be_bytes());
    assert!(Batch::from_canonical_bytes(&announcements, limits).is_err());
}

#[test]
fn strict_indices_and_latest_duplicate_are_durable_without_extra_commit() {
    let (dir, res, db) = open();
    let limits = ApplyLimits::default();
    let first = batch(1);
    assert!(matches!(
        apply(&db, &batch(2), limits),
        Err(ApplyError::Gap {
            expected: 1,
            received: 2
        })
    ));
    assert_eq!(applied_state(&db).unwrap(), None);
    let applied = apply(&db, &first, limits).unwrap();
    assert!(!applied.already_applied);
    let sequence = db.commit_sequence();
    assert!(apply(&db, &first, limits).unwrap().already_applied);
    assert_eq!(db.commit_sequence(), sequence);
    let mut divergent = first.clone();
    divergent.mutations[0].value = Some(b"different".to_vec());
    assert!(matches!(
        apply(&db, &divergent, limits),
        Err(ApplyError::Divergent(1))
    ));
    assert!(matches!(
        apply(&db, &batch(3), limits),
        Err(ApplyError::Gap {
            expected: 2,
            received: 3
        })
    ));
    let second = Batch {
        version: 1,
        index: 2,
        mutations: vec![
            mutation("note", b"a", None),
            mutation("note", b"b", Some(b"new")),
        ],
    };
    apply(&db, &second, limits).unwrap();
    assert!(matches!(
        apply(&db, &first, limits),
        Err(ApplyError::Older {
            applied: 2,
            received: 1
        })
    ));
    let state = applied_state(&db).unwrap();
    drop(db);
    let db = Database::open(dir.path(), &res).unwrap();
    assert_eq!(applied_state(&db).unwrap(), state);
    assert!(apply(&db, &second, limits).unwrap().already_applied);
    assert_eq!(db.begin().get(Cf::Note, b"a").unwrap(), None);
    assert_eq!(
        db.begin().get(Cf::Note, b"b").unwrap(),
        Some(b"new".to_vec())
    );
}

#[test]
fn invalid_entries_and_bounds_fail_without_data_or_progress() {
    let (_dir, _res, db) = open();
    let limits = ApplyLimits::default();
    let invalid = [
        Batch {
            version: 2,
            ..batch(1)
        },
        Batch {
            index: 0,
            ..batch(1)
        },
        Batch {
            index: u64::MAX,
            ..batch(1)
        },
        Batch {
            mutations: vec![mutation("unknown", b"k", Some(b"v"))],
            ..batch(1)
        },
        Batch {
            mutations: vec![mutation("note", b"", Some(b"v"))],
            ..batch(1)
        },
        Batch {
            mutations: vec![mutation("meta", b"replicated-apply/other", Some(b"v"))],
            ..batch(1)
        },
        Batch {
            mutations: vec![mutation("meta", b"commit/sequence", None)],
            ..batch(1)
        },
        Batch {
            mutations: vec![
                mutation("note", b"k", Some(b"v")),
                mutation("note", b"k", None),
            ],
            ..batch(1)
        },
    ];
    for entry in invalid {
        assert!(apply(&db, &entry, limits).is_err());
        assert_eq!(applied_state(&db).unwrap(), None);
    }
    for constrained in [
        ApplyLimits {
            max_entries: 1,
            ..limits
        },
        ApplyLimits {
            max_bytes: 17,
            ..limits
        },
        ApplyLimits {
            max_key_bytes: 0,
            ..limits
        },
        ApplyLimits {
            max_value_bytes: 2,
            ..limits
        },
    ] {
        assert!(apply(&db, &batch(1), constrained).is_err());
        assert_eq!(applied_state(&db).unwrap(), None);
    }
    assert_eq!(db.begin().scan_prefix(Cf::Note, &[]).unwrap(), vec![]);
    assert_eq!(db.commit_sequence(), None);
    let mut tx = db.begin();
    tx.put(Cf::Meta, b"replicated-apply/v1".to_vec(), vec![1]);
    tx.commit().unwrap();
    assert!(matches!(
        apply(&db, &batch(1), limits),
        Err(ApplyError::InvalidLedger)
    ));
    assert_eq!(db.begin().get(Cf::Note, b"a").unwrap(), None);
}

#[test]
fn three_independent_stores_match_graph_identity_indexes_acl_cursor_and_restore_ledger() {
    let (_source_dir, _source_res, source) = open();
    let mapping: GraphMapping=serde_json::from_str(r#"{"version":"1","nodes":[{"label":"Customer","properties":["email"],"indexed_fields":["email"],"er_fields":["email"],"identifier_columns":{"email":"email"}},{"label":"Company","properties":[]},{"label":"ImportCheckpoint","properties":["sha256","next_offset","contract_id"]}],"edges":[{"type":"WORKS_AT","from":{"label":"Customer"},"to":{"label":"Company"}}]}"#).unwrap();
    let catalog = source.apply_ontology(&mapping).unwrap();
    let mut tx = source.begin();
    let customer = merge_node(
        &mut tx,
        &catalog,
        "Customer",
        "c1",
        &BTreeMap::from([("email".into(), Value::Str("c@example.test".into()))]),
    )
    .unwrap();
    let company = merge_node(&mut tx, &catalog, "Company", "k1", &BTreeMap::new()).unwrap();
    merge_edge(
        &mut tx,
        &catalog,
        customer,
        "WORKS_AT",
        company,
        &BTreeMap::new(),
    )
    .unwrap();
    set_node_acl(
        &mut tx,
        customer,
        &NodeAcl {
            allow: vec!["synthetic-user".into()],
            connector_ids: vec!["synthetic-source".into()],
            ..NodeAcl::default()
        },
    )
    .unwrap();
    merge_node(
        &mut tx,
        &catalog,
        "ImportCheckpoint",
        "source-v1",
        &BTreeMap::from([
            ("sha256".into(), Value::Str("a".repeat(64))),
            ("next_offset".into(), Value::Int(128)),
            ("contract_id".into(), Value::Str("synthetic-v1".into())),
        ]),
    )
    .unwrap();
    // Ident has no serving codec yet; retain explicitly auxiliary identity bytes.
    tx.put(
        Cf::Ident,
        b"synthetic-email:c@example.test".to_vec(),
        customer.0.to_be_bytes().to_vec(),
    );
    tx.commit().unwrap();
    check_invariants(&source).unwrap();
    let expected = logical(&source);
    for name in ["meta", "node", "pk", "out", "in", "idx", "ident", "acl"] {
        assert!(
            expected.iter().any(|m| m.cf == name),
            "fixture lacks {name}"
        );
    }
    let entry = Batch {
        version: 1,
        index: 1,
        mutations: expected.clone(),
    };
    let limits = ApplyLimits::default();
    for _ in 0..3 {
        let (_directory, res, db) = open();
        apply(&db, &entry, limits).unwrap();
        assert_eq!(logical(&db), expected);
        check_invariants(&db).unwrap();
        assert_eq!(db.stats().unwrap().nodes, 3);
        assert_eq!(db.stats().unwrap().edges, 1);
        let checkpoint_directory = tempfile::tempdir().unwrap();
        let checkpoint = checkpoint_directory.path().join("checkpoint");
        db.checkpoint(&checkpoint).unwrap();
        let restored = Database::open(&checkpoint, &res).unwrap();
        assert_eq!(
            applied_state(&restored).unwrap(),
            applied_state(&db).unwrap()
        );
        assert!(apply(&restored, &entry, limits).unwrap().already_applied);
        let next = Batch {
            version: 1,
            index: 2,
            mutations: vec![mutation("note", b"next", Some(b"materialized"))],
        };
        apply(&db, &next, limits).unwrap();
        apply(&restored, &next, limits).unwrap();
        assert_eq!(logical(&restored), logical(&db));
        assert_eq!(
            applied_state(&restored).unwrap(),
            applied_state(&db).unwrap()
        );
        check_invariants(&restored).unwrap();
    }
}
