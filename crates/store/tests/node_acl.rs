use brainwire_core::{
    acl::{NodeAcl, ReadVisibility},
    schema::GraphMapping,
    value::Value,
};
use brainwire_store::{
    db::{Database, Resources, StoreConfig},
    graph::{self, ProjectionError, ProjectionOptions},
    txn::Cf,
};
use std::collections::BTreeMap;
fn setup() -> (
    tempfile::TempDir,
    Resources,
    Database,
    brainwire_core::schema::Catalog,
) {
    let dir = tempfile::tempdir().unwrap();
    let resources = Resources::new(StoreConfig::for_tests());
    let db = Database::open(dir.path(), &resources).unwrap();
    let mapping:GraphMapping=serde_json::from_str(r#"{"version":"1","nodes":[{"label":"A","properties":["document_id"]}],"edges":[{"type":"LINK","from":{"label":"A"},"to":{"label":"A"},"properties":["weight"]}]}"#).unwrap();
    let catalog = db.apply_ontology(&mapping).unwrap();
    (dir, resources, db, catalog)
}
fn policy() -> ReadVisibility {
    ReadVisibility {
        principals: vec!["user".into()],
        ..ReadVisibility::default()
    }
}
fn allowed() -> NodeAcl {
    NodeAcl {
        allow: vec!["user".into()],
        ..NodeAcl::default()
    }
}
#[test]
fn acl_overlay_snapshot_conflict_delete_and_reopen_are_atomic() {
    let (dir, resources, db, catalog) = setup();
    let mut tx = db.begin();
    let node = graph::merge_node(&mut tx, &catalog, "A", "one", &BTreeMap::new()).unwrap();
    graph::set_node_acl(&mut tx, node, &allowed()).unwrap();
    assert!(graph::node_visible(&tx, &catalog, node, &policy()).unwrap());
    tx.commit().unwrap();
    let old = db.begin();
    let mut change = db.begin();
    graph::set_node_acl(&mut change, node, &NodeAcl::default()).unwrap();
    assert!(graph::node_visible(&old, &catalog, node, &policy()).unwrap());
    assert!(!graph::node_visible(&change, &catalog, node, &policy()).unwrap());
    change.commit().unwrap();
    assert!(graph::node_visible(&old, &catalog, node, &policy()).unwrap());
    drop(old);
    let mut first = db.begin();
    let mut second = db.begin();
    graph::set_node_acl(&mut first, node, &allowed()).unwrap();
    graph::set_node_acl(&mut second, node, &NodeAcl::default()).unwrap();
    first.commit().unwrap();
    assert!(second.commit().is_err());
    drop(db);
    let db = Database::open(dir.path(), &resources).unwrap();
    assert_eq!(
        graph::get_node_acl(&db.begin(), node).unwrap(),
        Some(allowed())
    );
    let mut delete = db.begin();
    graph::detach_delete(&mut delete, node).unwrap();
    assert!(graph::get_node_acl(&delete, node).unwrap().is_none());
    delete.commit().unwrap();
    assert!(graph::get_node_acl(&db.begin(), node).unwrap().is_none());
}
#[test]
fn actual_document_property_activates_exclusive_grants_and_invalid_stamps_do_not_mutate() {
    let (_dir, _resources, db, catalog) = setup();
    let mut tx = db.begin();
    let node = graph::merge_node(
        &mut tx,
        &catalog,
        "A",
        "doc",
        &BTreeMap::from([("document_id".into(), Value::Str("doc-1".into()))]),
    )
    .unwrap();
    graph::set_node_acl(&mut tx, node, &allowed()).unwrap();
    let restricted = ReadVisibility {
        allow_all: true,
        knowledge_grants: Some(vec![]),
        ..ReadVisibility::default()
    };
    assert!(!graph::node_visible(&tx, &catalog, node, &restricted).unwrap());
    let invalid = NodeAcl {
        allow: vec!["x".into(); 257],
        ..NodeAcl::default()
    };
    assert!(graph::set_node_acl(&mut tx, node, &invalid).is_err());
    assert_eq!(graph::get_node_acl(&tx, node).unwrap(), Some(allowed()));
    tx.commit().unwrap();
    let mut broken = db.begin();
    broken.put(Cf::Acl, node.0.to_be_bytes().to_vec(), vec![1, b'{']);
    broken.commit().unwrap();
    assert!(graph::node_visible(&db.begin(), &catalog, node, &policy()).is_err());
    assert!(graph::check_invariants(&db).is_err());
}
#[test]
fn filtered_projection_excludes_hidden_topology_before_weight_and_keeps_physical_limits() {
    let (_dir, _resources, db, catalog) = setup();
    let mut tx = db.begin();
    let a = graph::merge_node(&mut tx, &catalog, "A", "a", &BTreeMap::new()).unwrap();
    let b = graph::merge_node(&mut tx, &catalog, "A", "b", &BTreeMap::new()).unwrap();
    let hidden = graph::merge_node(&mut tx, &catalog, "A", "hidden", &BTreeMap::new()).unwrap();
    for node in [a, b] {
        graph::set_node_acl(&mut tx, node, &allowed()).unwrap();
    }
    graph::merge_edge(&mut tx, &catalog, a, "LINK", b, &BTreeMap::new()).unwrap();
    graph::merge_edge(
        &mut tx,
        &catalog,
        b,
        "LINK",
        hidden,
        &BTreeMap::from([("weight".into(), Value::Str("invalid-but-hidden".into()))]),
    )
    .unwrap();
    tx.commit().unwrap();
    let opts = ProjectionOptions {
        weight_property: Some("weight".into()),
        ..ProjectionOptions::default()
    };
    let projection = graph::sparse_projection_with_visibility_and_control(
        &db.begin(),
        &opts,
        Some(&policy()),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(projection.nodes, vec![a, b]);
    assert_eq!(projection.relationship_count, 1);
    assert_eq!(projection.scanned_relationship_count, 2);
    assert_eq!(projection.out_degrees, vec![1, 0]);
    for limited in [
        ProjectionOptions {
            max_nodes: 2,
            ..opts.clone()
        },
        ProjectionOptions {
            max_relationships: 1,
            ..opts.clone()
        },
    ] {
        assert!(matches!(
            graph::sparse_projection_with_visibility_and_control(
                &db.begin(),
                &limited,
                Some(&policy()),
                |_| Ok(())
            ),
            Err(ProjectionError::Limit { .. })
        ));
    }
}

#[test]
fn stamping_and_detaching_conflict_in_both_commit_orders_and_orphan_metadata_fails_audit() {
    for delete_first in [false, true] {
        let (_dir, _resources, db, catalog) = setup();
        let mut initial = db.begin();
        let node = graph::merge_node(&mut initial, &catalog, "A", "one", &BTreeMap::new()).unwrap();
        initial.commit().unwrap();
        let mut stamp = db.begin();
        let mut delete = db.begin();
        graph::set_node_acl(&mut stamp, node, &allowed()).unwrap();
        graph::detach_delete(&mut delete, node).unwrap();
        if delete_first {
            delete.commit().unwrap();
            assert!(stamp.commit().is_err());
        } else {
            stamp.commit().unwrap();
            assert!(delete.commit().is_err());
        }
        graph::check_invariants(&db).unwrap();
    }
    let (_dir, _resources, db, _catalog) = setup();
    let mut tx = db.begin();
    tx.put(Cf::Acl, 7u64.to_be_bytes().to_vec(), vec![1, b'{', b'}']);
    tx.commit().unwrap();
    assert!(graph::check_invariants(&db)
        .unwrap_err()
        .contains("missing node"));
}

#[test]
fn visibility_filter_keeps_dangling_corruption_detection_and_control_errors() {
    let (_dir, _resources, db, catalog) = setup();
    let mut tx = db.begin();
    let a = graph::merge_node(&mut tx, &catalog, "A", "a", &BTreeMap::new()).unwrap();
    let b = graph::merge_node(&mut tx, &catalog, "A", "b", &BTreeMap::new()).unwrap();
    graph::merge_edge(&mut tx, &catalog, a, "LINK", b, &BTreeMap::new()).unwrap();
    tx.commit().unwrap();
    let opts = ProjectionOptions::default();
    let deny = ReadVisibility::default();
    let empty = graph::sparse_projection_with_visibility_and_control(
        &db.begin(),
        &opts,
        Some(&deny),
        |_| Ok(()),
    )
    .unwrap();
    assert!(empty.nodes.is_empty());
    assert_eq!(empty.scanned_relationship_count, 1);
    let before = db.stats().unwrap();
    let mut checks = 0;
    assert!(matches!(
        graph::sparse_projection_with_visibility_and_control(
            &db.begin(),
            &opts,
            Some(&deny),
            |_| {
                checks += 1;
                if checks == 4 {
                    Err(ProjectionError::Interrupted)
                } else {
                    Ok(())
                }
            }
        ),
        Err(ProjectionError::Interrupted)
    ));
    assert_eq!(db.stats().unwrap(), before);
    let mut broken = db.begin();
    let (mut key, value) = broken.scan_prefix(Cf::Out, &[]).unwrap().pop().unwrap();
    key[10..18].copy_from_slice(&(b.0 + 1000).to_be_bytes());
    broken.put(Cf::Out, key, value);
    broken.commit().unwrap();
    assert!(matches!(
        graph::sparse_projection_with_visibility_and_control(
            &db.begin(),
            &opts,
            Some(&deny),
            |_| Ok(())
        ),
        Err(ProjectionError::MissingNode)
    ));
}

#[test]
fn visibility_precharges_borrowed_node_and_acl_before_decoding_and_preserves_callback_error() {
    let (_dir, _resources, db, catalog) = setup();
    let mut tx = db.begin();
    let node = graph::merge_node(
        &mut tx,
        &catalog,
        "A",
        "large",
        &BTreeMap::from([("body".into(), Value::Str("x".repeat(200_000)))]),
    )
    .unwrap();
    graph::set_node_acl(&mut tx, node, &allowed()).unwrap();
    tx.commit().unwrap();
    let tx = db.begin();
    let raw_node = tx.get(Cf::Node, &node.0.to_be_bytes()).unwrap().unwrap();
    let raw_len = raw_node.len();
    let identity_len = u32::from_be_bytes(raw_node[..4].try_into().unwrap()) as usize;
    let node_estimate =
        brainwire_core::codec::estimate_row_allocation(&raw_node[4 + identity_len..]).unwrap()
            + identity_len
            + std::mem::size_of::<graph::NodeRecord>()
            + raw_len;
    let mut attempts = Vec::new();
    let error = graph::node_visible_with_control::<graph::GraphError>(
        &tx,
        &catalog,
        node,
        &policy(),
        |bytes| {
            attempts.push(bytes);
            if bytes > 0 {
                Err(graph::GraphError::UnknownLabel("guard sentinel".into()))
            } else {
                Ok(())
            }
        },
    )
    .unwrap_err();
    assert!(matches!(error,graph::GraphError::UnknownLabel(ref value) if value=="guard sentinel"));
    assert_eq!(attempts, vec![0, node_estimate]);
    assert_eq!(
        graph::node_visible_with_control::<graph::GraphError>(
            &tx,
            &catalog,
            node,
            &policy(),
            |_| Ok(())
        )
        .unwrap(),
        graph::node_visible(&tx, &catalog, node, &policy()).unwrap()
    );
    drop(tx);
    let mut broken = db.begin();
    let mut raw_acl = vec![b'{'; 20_000];
    raw_acl[0] = 1;
    broken.put(Cf::Acl, node.0.to_be_bytes().to_vec(), raw_acl.clone());
    broken.commit().unwrap();
    let tx = db.begin();
    let mut positive_calls = 0;
    let mut charged = 0;
    let error = graph::node_visible_with_control::<graph::GraphError>(
        &tx,
        &catalog,
        node,
        &policy(),
        |bytes| {
            charged += bytes;
            if bytes > 0 {
                positive_calls += 1;
            }
            if positive_calls == 2 {
                Err(graph::GraphError::UnknownLabel("ACL guard sentinel".into()))
            } else {
                Ok(())
            }
        },
    )
    .unwrap_err();
    assert!(
        matches!(error,graph::GraphError::UnknownLabel(ref value) if value=="ACL guard sentinel")
    );
    assert_eq!(charged, node_estimate + raw_acl.len() * 32 + 1024);
    assert!(graph::node_visible(&tx, &catalog, node, &policy()).is_err());
}

#[test]
fn null_heavy_node_and_acl_private_error_are_guarded_without_secret_leak() {
    let (_dir, _resources, db, catalog) = setup();
    let mut tx = db.begin();
    let node = graph::merge_node(
        &mut tx,
        &catalog,
        "A",
        "null-heavy",
        &BTreeMap::from([("body".into(), Value::List(vec![Value::Null; 4096]))]),
    )
    .unwrap();
    tx.commit().unwrap();
    let tx = db.begin();
    let raw = tx.get(Cf::Node, &node.0.to_be_bytes()).unwrap().unwrap();
    assert!(raw.len() < 32768);
    let mut largest = 0;
    let guard = |bytes: usize| -> Result<(), graph::GraphError> {
        if bytes > 32768 {
            Err(graph::GraphError::UnknownLabel("preflight guard".into()))
        } else {
            Ok(())
        }
    };
    assert!(matches!(
        graph::get_node_with_control::<graph::GraphError>(&tx, node, guard),
        Err(graph::GraphError::UnknownLabel(_))
    ));
    let error = graph::node_visible_with_control::<graph::GraphError>(
        &tx,
        &catalog,
        node,
        &policy(),
        |bytes| {
            largest = largest.max(bytes);
            guard(bytes)
        },
    )
    .unwrap_err();
    assert!(matches!(error, graph::GraphError::UnknownLabel(_)));
    assert!(largest >= 4096 * std::mem::size_of::<Value>());
    drop(tx);
    let mut broken = db.begin();
    let mut bytes = vec![1];
    bytes.extend_from_slice(br#"{"allow":"private-principal-secret"}"#);
    broken.put(Cf::Acl, node.0.to_be_bytes().to_vec(), bytes);
    broken.commit().unwrap();
    let error = graph::get_node_acl(&db.begin(), node)
        .unwrap_err()
        .to_string();
    assert!(error.contains("invalid private node ACL encoding"));
    assert!(!error.contains("private-principal-secret"));
}

#[test]
fn acl_stamping_validates_borrowed_large_rows_and_pending_overlay_without_owned_decoding() {
    let (_dir, _resources, db, catalog) = setup();
    let mut tx = db.begin();
    let node = graph::merge_node(
        &mut tx,
        &catalog,
        "A",
        "null-heavy",
        &BTreeMap::from([("body".into(), Value::List(vec![Value::Null; 100_000]))]),
    )
    .unwrap();
    tx.commit().unwrap();
    let mut tx = db.begin();
    graph::set_node_acl(&mut tx, node, &allowed()).unwrap();
    assert_eq!(graph::get_node_acl(&tx, node).unwrap(), Some(allowed()));
    graph::detach_delete(&mut tx, node).unwrap();
    assert!(matches!(
        graph::set_node_acl(&mut tx, node, &allowed()),
        Err(graph::GraphError::NodeNotFound(_))
    ));
    assert!(graph::get_node_acl(&tx, node).unwrap().is_none());
    tx.commit().unwrap();
    let mut tx = db.begin();
    let malformed =
        graph::merge_node(&mut tx, &catalog, "A", "malformed", &BTreeMap::new()).unwrap();
    tx.put(
        Cf::Node,
        malformed.0.to_be_bytes().to_vec(),
        vec![0, 0, 0, 2, b'o'],
    );
    assert!(graph::set_node_acl(&mut tx, malformed, &allowed()).is_err());
    assert!(graph::get_node_acl(&tx, malformed).unwrap().is_none());
    // The invalid raw fixture remains only in this discarded overlay.
    drop(tx);
    graph::check_invariants(&db).unwrap();
}

#[test]
fn trusted_acl_replacement_tracks_large_or_corrupt_previous_metadata_without_decoding_it() {
    let (_dir, _resources, db, catalog) = setup();
    let mut tx = db.begin();
    let node = graph::merge_node(&mut tx, &catalog, "A", "repair", &BTreeMap::new()).unwrap();
    tx.put(
        Cf::Acl,
        node.0.to_be_bytes().to_vec(),
        vec![b'x'; 64 * 1024],
    );
    tx.commit().unwrap();
    let mut first = db.begin();
    let mut second = db.begin();
    graph::set_node_acl(&mut first, node, &allowed()).unwrap();
    graph::set_node_acl(&mut second, node, &NodeAcl::default()).unwrap();
    first.commit().unwrap();
    assert!(second.commit().is_err());
    assert_eq!(
        graph::get_node_acl(&db.begin(), node).unwrap(),
        Some(allowed())
    );
    graph::check_invariants(&db).unwrap();
}
