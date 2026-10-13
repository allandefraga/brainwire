use std::collections::BTreeMap;

use brainwire_core::schema::GraphMapping;
use brainwire_store::db::{Database, Resources, StoreConfig};
use brainwire_store::graph::{
    check_invariants, merge_edge, merge_node, sparse_projection, sparse_projection_with_control,
    ProjectionError, ProjectionOptions,
};

fn fixture() -> (tempfile::TempDir, Resources, Database, ProjectionOptions) {
    let dir = tempfile::tempdir().unwrap();
    let resources = Resources::new(StoreConfig::for_tests());
    let db = Database::open(dir.path(), &resources).unwrap();
    let mapping: GraphMapping = serde_json::from_str(
        r#"{"version":"1","nodes":[{"label":"A","properties":[]},{"label":"B","properties":[]}],"edges":[{"type":"KEEP","from":{"label":"A"},"to":{"label":"B"}},{"type":"SKIP","from":{"label":"A"},"to":{"label":"B"}}]}"#,
    ).unwrap();
    let catalog = db.apply_ontology(&mapping).unwrap();
    let mut tx = db.begin();
    let source = merge_node(&mut tx, &catalog, "A", "a", &BTreeMap::new()).unwrap();
    let target = merge_node(&mut tx, &catalog, "B", "b", &BTreeMap::new()).unwrap();
    merge_edge(&mut tx, &catalog, source, "SKIP", target, &BTreeMap::new()).unwrap();
    tx.commit().unwrap();
    let options = ProjectionOptions {
        relationship_type: Some(catalog.edge_type("KEEP").unwrap().id),
        ..ProjectionOptions::default()
    };
    (dir, resources, db, options)
}

#[test]
fn interruption_observes_filtered_edge_scan_and_does_not_mutate() {
    let (_dir, _resources, db, options) = fixture();
    let before = db.stats().unwrap();
    let tx = db.begin();
    let sequence = tx.snapshot_commit_sequence();
    let mut checks = 0;
    let mut reserved = 0;
    let result = sparse_projection_with_control(&tx, &options, |bytes| {
        if bytes == 0 {
            checks += 1;
            if checks == 4 {
                return Err(ProjectionError::Interrupted);
            }
        } else {
            reserved += bytes;
        }
        Ok(())
    });
    assert!(matches!(result, Err(ProjectionError::Interrupted)));
    assert_eq!(checks, 4, "start, two nodes, first filtered edge");
    assert_eq!(
        reserved, 256,
        "filtered edge does not reserve selected-edge memory"
    );
    drop(tx);
    assert_eq!(db.stats().unwrap(), before);
    assert_eq!(db.begin().snapshot_commit_sequence(), sequence);
    check_invariants(&db).unwrap();
}

#[test]
fn failed_reservation_prevents_projection_and_preserves_store() {
    let (_dir, _resources, db, options) = fixture();
    let before = db.stats().unwrap();
    let tx = db.begin();
    let mut attempted = Vec::new();
    let result = sparse_projection_with_control(&tx, &options, |bytes| {
        if bytes > 0 {
            attempted.push(bytes);
            return Err(ProjectionError::MemoryLimit {
                limit: 127,
                attempted: bytes,
            });
        }
        Ok(())
    });
    assert!(matches!(
        result,
        Err(ProjectionError::MemoryLimit {
            limit: 127,
            attempted: 128
        })
    ));
    assert_eq!(attempted, [128]);
    drop(tx);
    assert_eq!(db.stats().unwrap(), before);
    check_invariants(&db).unwrap();
}

#[test]
fn wrapper_parity_and_controls_surround_csr_construction() {
    let (_dir, _resources, db, mut options) = fixture();
    options.relationship_type = None;
    let tx = db.begin();
    let expected = sparse_projection(&tx, &options).unwrap();
    let mut controls = Vec::new();
    let actual = sparse_projection_with_control(&tx, &options, |bytes| {
        controls.push(bytes);
        Ok(())
    })
    .unwrap();
    assert_eq!(actual.nodes, expected.nodes);
    assert_eq!(actual.out_degrees, expected.out_degrees);
    assert_eq!(actual.in_degrees, expected.in_degrees);
    assert_eq!(actual.relationship_count, expected.relationship_count);
    assert_eq!(
        actual.scanned_relationship_count,
        expected.scanned_relationship_count
    );
    assert_eq!(
        actual.matrix.matvec(&[1.0, 2.0]).unwrap(),
        expected.matrix.matvec(&[1.0, 2.0]).unwrap()
    );
    assert_eq!(controls, [0, 0, 128, 0, 128, 0, 128, 0, 0]);
    assert!(matches!(
        sparse_projection(
            &tx,
            &ProjectionOptions {
                max_bytes: 127,
                ..options
            }
        ),
        Err(ProjectionError::MemoryLimit { .. })
    ));
}

#[test]
fn weighted_decode_reservation_precedes_selected_edge_reservation() {
    let (_dir, _resources, db, mut options) = fixture();
    options.relationship_type = None;
    options.weight_property = Some("weight".into());
    let tx = db.begin();
    let mut encoded_len = 0;
    tx.visit_prefix::<ProjectionError>(brainwire_store::txn::Cf::Out, &[], |_, value| {
        encoded_len = value.len();
        Ok(())
    })
    .unwrap();
    let mut charged = Vec::new();
    let projection = sparse_projection_with_control(&tx, &options, |bytes| {
        if bytes != 0 {
            charged.push(bytes);
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(charged, [128, 128, encoded_len * 4, 128]);
    assert_eq!(projection.relationship_count, 1);
}
