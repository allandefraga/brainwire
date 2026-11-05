//! Online catalog changes and the bounded background index jobs they leave.
//!
//! Each change stages one catalog rewrite in the caller's transaction. Every
//! Cypher transaction reads the catalog for update at begin, so writers that
//! were already running conflict once and retry against the new catalog.
//!
//! Index builds and drops are jobs under `index-build/` and `index-drop/` meta
//! keys. `Database::index_step` runs one bounded batch per transaction and
//! keeps its progress in the job key, never in the catalog, so a long build
//! does not make writers conflict. Only the last build batch rewrites the
//! catalog, to move the index from populating to online.

use std::collections::BTreeMap;

use brainwire_core::ids::{NodeId, TypeId};
use brainwire_core::keys::{get_bytes, get_u16};
use brainwire_core::schema::{
    Catalog, EdgeSpec, EndpointSpec, GraphMapping, NodeSpec, SchemaError,
};

use crate::db::{Database, StoreError};
use crate::graph::{
    catalog_error, decode_node, index_field_prefix, index_key, node_id_value, property_value,
    GraphError, CATALOG_KEY, INDEX_READY_KEY, MAX_ATOMIC_BACKFILL_KEYS,
};
use crate::txn::{Cf, Txn, TxnError};

const BUILD_JOB: &[u8] = b"index-build/";
const DROP_JOB: &[u8] = b"index-drop/";
/// Nodes (or index keys, when dropping) handled per background transaction.
pub const INDEX_BATCH: usize = 1024;

/// How `drop_index` names its index.
pub enum IndexTarget<'a> {
    Name(&'a str),
    Property { label: &'a str, property: &'a str },
}

fn rejected(message: impl Into<String>) -> GraphError {
    GraphError::Catalog(message.into())
}

fn schema_error(error: SchemaError) -> GraphError {
    match error {
        SchemaError::UnknownLabel(label) => GraphError::UnknownLabel(label),
        error => rejected(error.to_string()),
    }
}

fn job_key(tag: &[u8], prefix: &[u8]) -> Vec<u8> {
    [tag, prefix].concat()
}

fn load(txn: &mut Txn) -> Result<Catalog, GraphError> {
    match txn.get_for_update(Cf::Meta, CATALOG_KEY)? {
        Some(bytes) => Ok(serde_json::from_slice(&bytes).map_err(catalog_error)?),
        None => {
            // A first catalog has nothing to backfill, like a first ontology.
            txn.put(Cf::Meta, INDEX_READY_KEY.to_vec(), vec![2]);
            Ok(Catalog {
                version: 0,
                node_types: vec![],
                edge_types: vec![],
                streams: vec![],
            })
        }
    }
}

fn store(txn: &mut Txn, catalog: &Catalog) -> Result<(), GraphError> {
    let bytes = serde_json::to_vec(catalog).map_err(catalog_error)?;
    txn.put(Cf::Meta, CATALOG_KEY.to_vec(), bytes);
    Ok(())
}

fn store_next(txn: &mut Txn, catalog: &mut Catalog) -> Result<(), GraphError> {
    catalog.version = catalog
        .version
        .checked_add(1)
        .ok_or_else(|| rejected("no ids left for catalog version"))?;
    store(txn, catalog)
}

fn mapping(nodes: Vec<NodeSpec>, edges: Vec<EdgeSpec>) -> GraphMapping {
    GraphMapping {
        version: String::new(),
        nodes,
        edges,
        timeline: vec![],
    }
}

fn find_index(catalog: &Catalog, target: &IndexTarget) -> Option<(TypeId, String)> {
    catalog.node_types.iter().find_map(|ty| {
        ty.indexed
            .iter()
            .find(|field| match target {
                IndexTarget::Name(name) => ty.index_name(field) == *name,
                IndexTarget::Property { label, property } => {
                    ty.label == *label && field == property
                }
            })
            .map(|field| (ty.id, field.clone()))
    })
}

enum Stop {
    Full,
    Store(StoreError),
}
impl From<StoreError> for Stop {
    fn from(error: StoreError) -> Self {
        Stop::Store(error)
    }
}

/// Collect up to `limit` keys a visit yields, stopping the scan when full.
fn take_keys(
    limit: usize,
    scan: impl FnOnce(&mut dyn FnMut(&[u8]) -> Result<(), Stop>) -> Result<(), Stop>,
) -> Result<Vec<Vec<u8>>, StoreError> {
    let mut keys = Vec::new();
    let result = scan(&mut |key| {
        keys.push(key.to_vec());
        if keys.len() == limit {
            Err(Stop::Full)
        } else {
            Ok(())
        }
    });
    match result {
        Ok(()) | Err(Stop::Full) => Ok(keys),
        Err(Stop::Store(error)) => Err(error),
    }
}

/// Declare an index as populating. Writers maintain it from this commit on;
/// `Database::index_step` backfills older nodes and then marks it online.
pub fn create_index(
    txn: &mut Txn,
    label: &str,
    property: &str,
    name: Option<&str>,
    if_not_exists: bool,
) -> Result<(), GraphError> {
    let mut catalog = load(txn)?;
    if property.to_ascii_lowercase().starts_with("acl_") {
        return Err(GraphError::ReservedProperty(property.into()));
    }
    if property == "entity_id" {
        return Err(rejected("entity_id is always indexed"));
    }
    let ty = catalog
        .node_type(label)
        .ok_or_else(|| GraphError::UnknownLabel(label.into()))?;
    let name = name.map_or_else(|| format!("index_{label}_{property}"), str::to_string);
    if name.is_empty() {
        return Err(rejected("index name must not be empty"));
    }
    if find_index(&catalog, &IndexTarget::Name(&name)).is_some() {
        return match if_not_exists {
            true => Ok(()),
            false => Err(rejected(format!("an index named {name} already exists"))),
        };
    }
    if ty.indexed.iter().any(|field| field == property) {
        return match if_not_exists {
            true => Ok(()),
            false => Err(rejected(format!(
                "index {} already covers {label}.{property}",
                ty.index_name(property)
            ))),
        };
    }
    let prefix = index_field_prefix(ty.id, property);
    if txn.get(Cf::Meta, &job_key(DROP_JOB, &prefix))?.is_some() {
        return Err(rejected(format!(
            "a dropped index on {label}.{property} is still being removed; retry shortly"
        )));
    }
    // Keys left by an index an older ontology removed offline.
    let stale = take_keys(MAX_ATOMIC_BACKFILL_KEYS + 1, |visit| {
        txn.visit_prefix_for_update(Cf::Idx, &prefix, |key, _| visit(key))
    })?;
    if stale.len() > MAX_ATOMIC_BACKFILL_KEYS {
        return Err(GraphError::IndexBackfillTooLarge);
    }
    for key in stale {
        txn.delete(Cf::Idx, key);
    }
    let ty = catalog
        .node_types
        .iter_mut()
        .find(|ty| ty.label == label)
        .expect("checked above");
    ty.indexed.push(property.into());
    ty.populating.push(property.into());
    ty.index_names.insert(property.into(), name);
    store_next(txn, &mut catalog)?;
    txn.put(Cf::Meta, job_key(BUILD_JOB, &prefix), Vec::new());
    Ok(())
}

/// Remove an index from the catalog at once; its keys go in background batches.
pub fn drop_index(txn: &mut Txn, target: IndexTarget, if_exists: bool) -> Result<(), GraphError> {
    let mut catalog = load(txn)?;
    let Some((type_id, field)) = find_index(&catalog, &target) else {
        if if_exists {
            return Ok(());
        }
        return Err(rejected(match target {
            IndexTarget::Name(name) => format!("no index named {name}"),
            IndexTarget::Property { label, property } => format!("no index on {label}.{property}"),
        }));
    };
    let ty = catalog
        .node_types
        .iter_mut()
        .find(|ty| ty.id == type_id)
        .expect("found above");
    ty.indexed.retain(|f| *f != field);
    ty.populating.retain(|f| *f != field);
    ty.index_names.remove(&field);
    store_next(txn, &mut catalog)?;
    let prefix = index_field_prefix(type_id, &field);
    txn.delete(Cf::Meta, job_key(BUILD_JOB, &prefix));
    txn.put(Cf::Meta, job_key(DROP_JOB, &prefix), Vec::new());
    Ok(())
}

/// Declare a node label. An existing label is left unchanged.
pub fn create_label(txn: &mut Txn, label: &str) -> Result<(), GraphError> {
    if label.is_empty() {
        return Err(rejected("label must not be empty"));
    }
    let catalog = load(txn)?;
    if catalog.node_type(label).is_some() {
        return Ok(());
    }
    let spec = NodeSpec {
        description: None,
        label: label.into(),
        properties: vec![],
        er_fields: vec![],
        indexed_fields: vec![],
        identifier_columns: BTreeMap::new(),
        display_color: None,
    };
    let next =
        Catalog::apply(&mapping(vec![spec], vec![]), Some(&catalog)).map_err(schema_error)?;
    store(txn, &next)
}

/// Declare a relationship type, or add a signature to an existing one.
pub fn create_relationship_type(
    txn: &mut Txn,
    name: &str,
    from: &str,
    to: &str,
) -> Result<(), GraphError> {
    create_relationship_type_with_keys(txn, name, from, to, None)
}

/// Declare immutable merge identity for a new type. Existing identity cannot change.
pub fn create_relationship_type_with_keys(
    txn: &mut Txn,
    name: &str,
    from: &str,
    to: &str,
    merge_key_fields: Option<Vec<String>>,
) -> Result<(), GraphError> {
    if name.is_empty() {
        return Err(rejected("relationship type must not be empty"));
    }
    let catalog = load(txn)?;
    let id = |label: &str| {
        catalog
            .node_type(label)
            .map(|ty| ty.id)
            .ok_or_else(|| GraphError::UnknownLabel(label.into()))
    };
    let signature = (id(from)?, id(to)?);
    // Validate identity even for an existing signature, before returning a no-op.
    if let Some(keys) = &merge_key_fields {
        if keys.is_empty()
            || catalog
                .edge_type(name)
                .is_some_and(|ty| &ty.merge_key_fields != keys)
        {
            return Err(schema_error(SchemaError::BadMergeKey(name.into())));
        }
    }
    if catalog
        .edge_type(name)
        .is_some_and(|ty| ty.signatures.contains(&signature))
    {
        return Ok(());
    }
    let endpoint = |label: &str| EndpointSpec {
        label: label.into(),
    };
    let spec = EdgeSpec {
        properties: merge_key_fields.clone().unwrap_or_default(),
        merge_key_fields,
        description: None,
        name: name.into(),
        from: endpoint(from),
        to: endpoint(to),
    };
    let next =
        Catalog::apply(&mapping(vec![], vec![spec]), Some(&catalog)).map_err(schema_error)?;
    store(txn, &next)
}

/// Undeclare a node property. Stored values stay under the retired column id,
/// which readers no longer map to a name, and are purged when a node is next
/// written. Writes of the same name afterwards are undeclared extras.
pub fn drop_property(txn: &mut Txn, label: &str, property: &str) -> Result<(), GraphError> {
    let mut catalog = load(txn)?;
    let ty = catalog
        .node_types
        .iter_mut()
        .find(|ty| ty.label == label)
        .ok_or_else(|| GraphError::UnknownLabel(label.into()))?;
    if ty.indexed.iter().any(|field| field == property) {
        return Err(rejected(format!(
            "drop index {} before dropping {label}.{property}",
            ty.index_name(property)
        )));
    }
    let Some(position) = ty.columns.iter().position(|c| c.name == property) else {
        return Err(rejected(format!(
            "{label} has no declared property {property}"
        )));
    };
    let column = ty.columns.remove(position);
    ty.retired_columns.push(column.id);
    store_next(txn, &mut catalog)
}

fn drop_batch(txn: &mut Txn, job: &[u8], batch: usize) -> Result<(), GraphError> {
    txn.get_for_update(Cf::Meta, job)?;
    let prefix = &job[DROP_JOB.len()..];
    let keys = take_keys(batch, |visit| {
        txn.visit_prefix(Cf::Idx, prefix, |key, _| visit(key))
    })?;
    if keys.len() < batch {
        txn.delete(Cf::Meta, job.to_vec());
    }
    for key in keys {
        txn.delete(Cf::Idx, key);
    }
    Ok(())
}

fn build_batch(txn: &mut Txn, job: &[u8], batch: usize) -> Result<(), GraphError> {
    let cursor = txn.get_for_update(Cf::Meta, job)?;
    let prefix = &job[BUILD_JOB.len()..];
    let mut input = prefix;
    let type_id = TypeId(get_u16(&mut input).map_err(StoreError::from)?);
    let field = String::from_utf8(get_bytes(&mut input).map_err(StoreError::from)?)
        .map_err(|_| StoreError::Corrupt("index job field".into()))?;
    let mut catalog = load(txn)?;
    let Some(ty) = catalog
        .node_types
        .iter()
        .find(|ty| ty.id == type_id && ty.populating.contains(&field))
        .cloned()
    else {
        txn.delete(Cf::Meta, job.to_vec());
        return Ok(());
    };
    let (lo, hi) = NodeId::type_range(0, type_id)?;
    let start = match cursor.as_deref() {
        Some(last) if !last.is_empty() => node_id_value(last)?.0 + 1,
        _ => lo.0,
    };
    let keys = take_keys(batch, |visit| {
        txn.visit_range(
            Cf::Node,
            &start.to_be_bytes(),
            &(hi.0 + 1).to_be_bytes(),
            |key, _| visit(key),
        )
    })?;
    for key in &keys {
        // Tracked reads: a writer that changes one of these nodes first makes
        // this batch conflict instead of indexing a value it already replaced.
        let Some(bytes) = txn.get_for_update(Cf::Node, key)? else {
            continue;
        };
        let id = node_id_value(key)?;
        let (_, row) = decode_node(&bytes)?;
        if let Some(entry) =
            property_value(&row, &ty, &field).and_then(|value| index_key(prefix, value, id))
        {
            txn.put(Cf::Idx, entry, Vec::new());
        }
    }
    match keys.last() {
        Some(last) if keys.len() == batch => txn.put(Cf::Meta, job.to_vec(), last.clone()),
        _ => {
            let ty = catalog
                .node_types
                .iter_mut()
                .find(|ty| ty.id == type_id)
                .expect("found above");
            ty.populating.retain(|f| *f != field);
            // Same schema, so the version stays; this is the one catalog write.
            store(txn, &catalog)?;
            txn.delete(Cf::Meta, job.to_vec());
        }
    }
    Ok(())
}

/// Forget pending jobs for an index the offline ontology path rebuilt itself.
pub(crate) fn clear_jobs(txn: &mut Txn, prefix: &[u8]) {
    txn.delete(Cf::Meta, job_key(BUILD_JOB, prefix));
    txn.delete(Cf::Meta, job_key(DROP_JOB, prefix));
}

impl Database {
    /// Run one bounded batch of a pending online index build or drop in its
    /// own transaction. Requested batch size is clamped to 1..=INDEX_BATCH.
    /// Returns false when no job is left. A batch that conflicts with a concurrent
    /// writer is discarded and retried next call. This bounds items, not RSS.
    pub fn index_step(&self, batch: usize) -> Result<bool, GraphError> {
        let batch = batch.clamp(1, INDEX_BATCH);
        let mut txn = self.begin();
        let mut job = None;
        for tag in [DROP_JOB, BUILD_JOB] {
            job = take_keys(1, |visit| {
                txn.visit_prefix(Cf::Meta, tag, |key, _| visit(key))
            })?
            .pop();
            if job.is_some() {
                break;
            }
        }
        let Some(job) = job else {
            return Ok(false);
        };
        if job.starts_with(DROP_JOB) {
            drop_batch(&mut txn, &job, batch)?;
        } else {
            build_batch(&mut txn, &job, batch)?;
        }
        match txn.commit() {
            Ok(_) | Err(TxnError::Conflict) => Ok(true),
            Err(error) => Err(error.into()),
        }
    }

    /// Drive every pending index job to completion. The server does this in
    /// a background worker; embedded callers and tests call it directly.
    pub fn build_pending_indexes(&self) -> Result<(), GraphError> {
        while self.index_step(INDEX_BATCH)? {}
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Resources, StoreConfig};
    use crate::graph::{check_invariants, find_by_indexed_property, get_node, merge_node};
    use brainwire_core::value::Value;

    fn open(dir: &std::path::Path, res: &Resources) -> Database {
        Database::open(dir, res).unwrap()
    }

    fn fixture() -> GraphMapping {
        serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json")).unwrap()
    }

    fn customer(db: &Database, id: &str, name: &str) {
        let catalog = db.catalog().unwrap().unwrap();
        let mut txn = db.begin();
        let props = BTreeMap::from([("name".to_string(), Value::Str(name.into()))]);
        merge_node(&mut txn, &catalog, "Customer", id, &props).unwrap();
        txn.commit().unwrap();
    }

    fn by_name(db: &Database, name: &str) -> Option<usize> {
        let catalog = db.catalog().unwrap().unwrap();
        let ty = catalog.node_type("Customer").unwrap();
        find_by_indexed_property(&db.begin(), ty, "name", &Value::Str(name.into()))
            .unwrap()
            .map(|ids| ids.len())
    }

    fn ddl(db: &Database, change: impl FnOnce(&mut Txn) -> Result<(), GraphError>) {
        let mut txn = db.begin();
        change(&mut txn).unwrap();
        txn.commit().unwrap();
    }

    #[test]
    fn online_index_populates_in_resumable_batches_then_serves_queries() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        let db = open(dir.path(), &res);
        db.apply_ontology(&fixture()).unwrap();
        for i in 0..5 {
            customer(
                &db,
                &format!("c{i}"),
                if i % 2 == 0 { "even" } else { "odd" },
            );
        }
        ddl(&db, |txn| {
            create_index(txn, "Customer", "name", None, false)
        });
        let catalog = db.catalog().unwrap().unwrap();
        let ty = catalog.node_type("Customer").unwrap();
        assert_eq!(ty.populating, vec!["name"]);
        assert_eq!(ty.index_name("name"), "index_Customer_name");
        assert_eq!(
            by_name(&db, "even"),
            None,
            "populating index must not answer"
        );
        check_invariants(&db).unwrap();

        // A write during population maintains the index itself.
        customer(&db, "c9", "even");
        assert!(db.index_step(2).unwrap());
        // Progress survives a restart: it lives in the job key.
        drop(db);
        let db = open(dir.path(), &res);
        check_invariants(&db).unwrap();
        let mut steps = 0;
        while db.index_step(2).unwrap() {
            steps += 1;
        }
        assert!(steps >= 2);
        assert!(db
            .catalog()
            .unwrap()
            .unwrap()
            .node_type("Customer")
            .unwrap()
            .populating
            .is_empty());
        assert_eq!(by_name(&db, "even"), Some(4));
        assert_eq!(by_name(&db, "odd"), Some(2));
        check_invariants(&db).unwrap();
    }

    #[test]
    fn oversized_requested_index_batch_preserves_bounded_progress() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        let db = open(dir.path(), &res);
        db.apply_ontology(&fixture()).unwrap();
        let catalog = db.catalog().unwrap().unwrap();
        let mut txn = db.begin();
        let props = BTreeMap::from([("name".into(), Value::Str("same".into()))]);
        for i in 0..=INDEX_BATCH {
            merge_node(&mut txn, &catalog, "Customer", &format!("c{i}"), &props).unwrap();
        }
        txn.commit().unwrap();
        ddl(&db, |txn| {
            create_index(txn, "Customer", "name", None, false)
        });
        assert!(db.index_step(usize::MAX).unwrap());
        assert_eq!(
            by_name(&db, "same"),
            None,
            "a caller cannot bypass the background batch ceiling"
        );
        // A subsequent bounded transaction completes exactly the remaining row.
        assert!(db.index_step(usize::MAX).unwrap());
        assert_eq!(by_name(&db, "same"), Some(INDEX_BATCH + 1));
        assert!(!db.index_step(usize::MAX).unwrap());
        check_invariants(&db).unwrap();

        let prefix = index_field_prefix(catalog.node_type("Customer").unwrap().id, "name");
        ddl(&db, |txn| {
            drop_index(txn, IndexTarget::Name("index_Customer_name"), false)
        });
        assert!(db.index_step(usize::MAX).unwrap());
        assert_eq!(db.begin().scan_prefix(Cf::Idx, &prefix).unwrap().len(), 1);
        assert!(db.index_step(usize::MAX).unwrap());
        assert!(db.begin().scan_prefix(Cf::Idx, &prefix).unwrap().is_empty());
        assert!(!db.index_step(usize::MAX).unwrap());
        check_invariants(&db).unwrap();
    }

    #[test]
    fn create_index_names_and_existence_rules() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        let db = open(dir.path(), &res);
        db.apply_ontology(&fixture()).unwrap();
        let mut txn = db.begin();
        create_index(&mut txn, "Customer", "name", Some("by_name"), false).unwrap();
        create_index(&mut txn, "Customer", "name", Some("by_name"), true).unwrap();
        create_index(&mut txn, "Customer", "name", None, true).unwrap();
        for (label, property, name) in [
            ("Customer", "name", None),
            ("Company", "name", Some("by_name")),
            ("Customer", "email", None),
            ("Nope", "name", None),
            ("Customer", "acl_allow", None),
            ("Customer", "entity_id", None),
        ] {
            assert!(create_index(&mut txn, label, property, name, false).is_err());
        }
        drop_index(&mut txn, IndexTarget::Name("index_Customer_email"), false).unwrap();
        drop_index(&mut txn, IndexTarget::Name("missing"), true).unwrap();
        assert!(drop_index(&mut txn, IndexTarget::Name("missing"), false).is_err());
        drop_index(
            &mut txn,
            IndexTarget::Property {
                label: "Customer",
                property: "name",
            },
            false,
        )
        .unwrap();
        txn.commit().unwrap();
        let ty = db.catalog().unwrap().unwrap().node_types[0].clone();
        assert_eq!(ty.indexed, vec!["phone", "segment"]);
        assert!(ty.index_names.is_empty() && ty.populating.is_empty());
    }

    #[test]
    fn dropped_index_keys_are_removed_in_batches_and_recreation_waits() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        let db = open(dir.path(), &res);
        db.apply_ontology(&fixture()).unwrap();
        ddl(&db, |txn| {
            create_index(txn, "Customer", "name", None, false)
        });
        for i in 0..5 {
            customer(&db, &format!("c{i}"), &format!("n{i}"));
        }
        db.build_pending_indexes().unwrap();
        let prefix = index_field_prefix(TypeId(1), "name");
        let count = |db: &Database| db.begin().scan_prefix(Cf::Idx, &prefix).unwrap().len();
        assert_eq!(count(&db), 5);
        ddl(&db, |txn| {
            drop_index(txn, IndexTarget::Name("index_Customer_name"), false)
        });
        assert_eq!(by_name(&db, "n1"), None);
        let mut txn = db.begin();
        assert!(create_index(&mut txn, "Customer", "name", None, false).is_err());
        drop(txn);
        assert!(db.index_step(2).unwrap());
        assert_eq!(count(&db), 3);
        db.build_pending_indexes().unwrap();
        assert_eq!(count(&db), 0);
        check_invariants(&db).unwrap();
        ddl(&db, |txn| {
            create_index(txn, "Customer", "name", None, false)
        });
        db.build_pending_indexes().unwrap();
        assert_eq!(by_name(&db, "n1"), Some(1));
        check_invariants(&db).unwrap();
    }

    #[test]
    fn labels_and_relationship_types_are_added_online() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        let db = open(dir.path(), &res);
        ddl(&db, |txn| create_label(txn, "Ticket"));
        assert!(db.scalar_indexes_ready().unwrap());
        ddl(&db, |txn| create_label(txn, "Ticket"));
        ddl(&db, |txn| create_label(txn, "Agent"));
        ddl(&db, |txn| {
            create_relationship_type(txn, "ASSIGNED", "Ticket", "Agent")
        });
        ddl(&db, |txn| {
            create_relationship_type(txn, "ASSIGNED", "Agent", "Agent")
        });
        ddl(&db, |txn| {
            create_relationship_type(txn, "ASSIGNED", "Agent", "Agent")
        });
        let mut txn = db.begin();
        assert!(matches!(
            create_relationship_type(&mut txn, "X", "Ticket", "Nope"),
            Err(GraphError::UnknownLabel(_))
        ));
        let catalog = db.catalog().unwrap().unwrap();
        assert_eq!(catalog.version, 4);
        assert_eq!(catalog.edge_type("ASSIGNED").unwrap().signatures.len(), 2);
    }

    #[test]
    fn dropped_property_values_stay_hidden_and_its_id_is_never_reused() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        let db = open(dir.path(), &res);
        db.apply_ontology(&fixture()).unwrap();
        customer(&db, "c1", "Ana");
        let old = db.catalog().unwrap().unwrap();
        let column = old.node_type("Customer").unwrap().column("name").unwrap();
        let mut txn = db.begin();
        assert!(
            drop_property(&mut txn, "Customer", "email").is_err(),
            "indexed"
        );
        assert!(drop_property(&mut txn, "Customer", "missing").is_err());
        drop(txn);
        ddl(&db, |txn| drop_property(txn, "Customer", "name"));
        let catalog = db.catalog().unwrap().unwrap();
        let ty = catalog.node_type("Customer").unwrap();
        assert_eq!(ty.column("name"), None);
        assert_eq!(ty.retired_columns, vec![column]);
        check_invariants(&db).unwrap();

        // The next write of the node purges the hidden value.
        let id = crate::graph::find_by_entity_id(&db.begin(), "c1", Some(ty.id)).unwrap()[0];
        assert!(get_node(&db.begin(), id)
            .unwrap()
            .unwrap()
            .row
            .columns
            .contains_key(&column));
        let mut txn = db.begin();
        merge_node(&mut txn, &catalog, "Customer", "c1", &BTreeMap::new()).unwrap();
        txn.commit().unwrap();
        assert!(!get_node(&db.begin(), id)
            .unwrap()
            .unwrap()
            .row
            .columns
            .contains_key(&column));

        // Re-declaring it offline yields a fresh column id.
        db.apply_ontology(&fixture()).unwrap();
        let readded = db
            .catalog()
            .unwrap()
            .unwrap()
            .node_type("Customer")
            .unwrap()
            .column("name");
        assert!(readded.is_some_and(|id| id > column));
        check_invariants(&db).unwrap();
    }

    #[test]
    fn offline_ontology_keeps_a_populating_index_and_a_legacy_rebuild_finishes_it() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        let db = open(dir.path(), &res);
        db.apply_ontology(&fixture()).unwrap();
        customer(&db, "c1", "Ana");
        ddl(&db, |txn| {
            create_index(txn, "Customer", "name", Some("by_name"), false)
        });
        db.apply_ontology(&fixture()).unwrap();
        let ty = db.catalog().unwrap().unwrap().node_types[0].clone();
        assert_eq!(ty.populating, vec!["name"]);
        assert_eq!(by_name(&db, "Ana"), None);

        // Without the global readiness marker, the offline path rebuilds all
        // declarations at once, so the online job has nothing left to do.
        let mut txn = db.begin();
        txn.delete(Cf::Meta, INDEX_READY_KEY.to_vec());
        txn.commit().unwrap();
        db.apply_ontology(&fixture()).unwrap();
        let ty = db.catalog().unwrap().unwrap().node_types[0].clone();
        assert!(ty.populating.is_empty());
        assert_eq!(ty.index_name("name"), "by_name");
        assert_eq!(by_name(&db, "Ana"), Some(1));
        assert!(!db.index_step(INDEX_BATCH).unwrap());
        check_invariants(&db).unwrap();
    }
}
