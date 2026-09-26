//! Graph operations on a transaction (spec 1.2): nodes by primary key, edges
//! mirrored in `out` and `in`, detach delete, and the invariant check used by
//! the crash test.

use std::collections::{BTreeMap, HashSet};

use brainwire_core::codec::{decode_row, encode_row, Row};
use brainwire_core::ids::{EdgeId, EdgeTypeId, NodeId, TypeId};
use brainwire_core::keys::{get_bytes, get_u16, get_u64, put_bytes, put_u16, put_u64};
use brainwire_core::schema::{Catalog, Column, GraphMapping};
use brainwire_core::value::Value;

use crate::db::{Database, StoreError};
use crate::txn::{Cf, Txn, TxnError};

const CATALOG_KEY: &[u8] = b"catalog";

#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("unknown label {0}")]
    UnknownLabel(String),
    #[error("unknown edge type {0}")]
    UnknownEdgeType(String),
    #[error("edge type {0} does not connect these labels")]
    BadSignature(String),
    #[error("property {0} is reserved")]
    ReservedProperty(String),
    #[error("node {0:?} not found")]
    NodeNotFound(NodeId),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Txn(#[from] TxnError),
}

pub struct NodeRecord {
    pub id: NodeId,
    pub entity_id: String,
    pub row: Row,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EdgeRef {
    pub edge_type: EdgeTypeId,
    pub other: NodeId,
    pub id: EdgeId,
}

fn catalog_error(e: impl std::fmt::Display) -> StoreError {
    StoreError::Catalog(e.to_string())
}

impl Database {
    pub fn catalog(&self) -> Result<Option<Catalog>, StoreError> {
        match self.begin().get(Cf::Meta, CATALOG_KEY)? {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(catalog_error),
            None => Ok(None),
        }
    }

    pub fn apply_ontology(&self, mapping: &GraphMapping) -> Result<Catalog, GraphError> {
        let mut txn = self.begin();
        let previous: Option<Catalog> = match txn.get_for_update(Cf::Meta, CATALOG_KEY)? {
            Some(bytes) => Some(serde_json::from_slice(&bytes).map_err(catalog_error)?),
            None => None,
        };
        let next = Catalog::apply(mapping, previous.as_ref()).map_err(catalog_error)?;
        txn.put(
            Cf::Meta,
            CATALOG_KEY.to_vec(),
            serde_json::to_vec(&next).map_err(catalog_error)?,
        );
        txn.commit()?;
        Ok(next)
    }
}

fn node_key(id: NodeId) -> Vec<u8> {
    id.0.to_be_bytes().to_vec()
}

fn pk_prefix(entity_id: &str) -> Vec<u8> {
    let mut key = Vec::new();
    put_bytes(&mut key, entity_id.as_bytes());
    key
}

fn pk_key(entity_id: &str, ty: TypeId) -> Vec<u8> {
    let mut key = pk_prefix(entity_id);
    put_u16(&mut key, ty.0);
    key
}

fn edge_key(a: NodeId, ty: EdgeTypeId, b: NodeId, id: EdgeId) -> Vec<u8> {
    let mut key = Vec::with_capacity(26);
    put_u64(&mut key, a.0);
    put_u16(&mut key, ty.0);
    put_u64(&mut key, b.0);
    put_u64(&mut key, id.0);
    key
}

fn decode_edge_key(key: &[u8]) -> Result<(NodeId, EdgeTypeId, NodeId, EdgeId), StoreError> {
    let mut input = key;
    let a = get_u64(&mut input)?;
    let ty = get_u16(&mut input)?;
    let b = get_u64(&mut input)?;
    let id = get_u64(&mut input)?;
    Ok((NodeId(a), EdgeTypeId(ty), NodeId(b), EdgeId(id)))
}

fn node_id_value(bytes: &[u8]) -> Result<NodeId, StoreError> {
    let raw: [u8; 8] = bytes
        .try_into()
        .map_err(|_| StoreError::Corrupt("node id value".into()))?;
    Ok(NodeId(u64::from_be_bytes(raw)))
}

fn encode_node(entity_id: &str, row: &Row) -> Vec<u8> {
    let mut out = (entity_id.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(entity_id.as_bytes());
    out.extend_from_slice(&encode_row(row));
    out
}

fn decode_node(bytes: &[u8]) -> Result<(String, Row), StoreError> {
    let corrupt = || StoreError::Corrupt("node value".into());
    let len: [u8; 4] = bytes
        .get(..4)
        .ok_or_else(corrupt)?
        .try_into()
        .map_err(|_| corrupt())?;
    let n = u32::from_be_bytes(len) as usize;
    let rest = &bytes[4..];
    let raw = rest.get(..n).ok_or_else(corrupt)?;
    let entity_id = String::from_utf8(raw.to_vec()).map_err(|_| corrupt())?;
    Ok((entity_id, decode_row(&rest[n..])?))
}

fn check_props(props: &BTreeMap<String, Value>) -> Result<(), GraphError> {
    match props
        .keys()
        .find(|k| k.to_ascii_lowercase().starts_with("acl_"))
    {
        Some(k) => Err(GraphError::ReservedProperty(k.clone())),
        None => Ok(()),
    }
}

fn apply_props(row: &mut Row, columns: &[Column], props: &BTreeMap<String, Value>) {
    for (name, value) in props {
        match columns.iter().find(|c| &c.name == name).map(|c| c.id) {
            Some(id) if *value == Value::Null => {
                row.columns.remove(&id);
            }
            Some(id) => {
                row.columns.insert(id, value.clone());
            }
            None if *value == Value::Null => {
                row.extra.remove(name);
            }
            None => {
                row.extra.insert(name.clone(), value.clone());
            }
        }
    }
}

/// Plain read: no dependency is recorded, so a later write to this node by
/// another transaction cannot make the caller's commit conflict. Callers that
/// only need the node to still exist *right now* to check a name or format a
/// value, and are not relying on it staying that way until they commit,
/// should use this. `merge_edge`'s existence check must not use this — see
/// `read_node_for_update`.
fn read_node(txn: &Txn, id: NodeId) -> Result<Option<(String, Row)>, GraphError> {
    match txn.get(Cf::Node, &node_key(id))? {
        Some(bytes) => Ok(Some(decode_node(&bytes)?)),
        None => Ok(None),
    }
}

/// Same read, but through `get_for_update`: the node key becomes a tracked
/// dependency of the transaction, so if another transaction writes that key
/// (a `detach_delete` of this same node, in particular) and commits first,
/// the caller's own commit conflicts instead of proceeding as if the node
/// were still there (spec 1.4, first committer wins; rule carried from the
/// Task 7/8 reviews into this task's dispatch). `merge_edge` relies on this
/// for both of its endpoints.
fn read_node_for_update(txn: &mut Txn, id: NodeId) -> Result<Option<(String, Row)>, GraphError> {
    match txn.get_for_update(Cf::Node, &node_key(id))? {
        Some(bytes) => Ok(Some(decode_node(&bytes)?)),
        None => Ok(None),
    }
}

pub fn merge_node(
    txn: &mut Txn,
    catalog: &Catalog,
    label: &str,
    entity_id: &str,
    props: &BTreeMap<String, Value>,
) -> Result<NodeId, GraphError> {
    check_props(props)?;
    let ty = catalog
        .node_type(label)
        .ok_or_else(|| GraphError::UnknownLabel(label.to_string()))?;
    let pk = pk_key(entity_id, ty.id);
    let (id, mut row) = match txn.get_for_update(Cf::Pk, &pk)? {
        Some(bytes) => {
            let id = node_id_value(&bytes)?;
            let (_, row) = read_node(txn, id)?.ok_or(GraphError::NodeNotFound(id))?;
            (id, row)
        }
        None => {
            let id = txn.database().next_node_id(ty.id)?;
            txn.put(Cf::Pk, pk, id.0.to_be_bytes().to_vec());
            (id, Row::default())
        }
    };
    row.schema_version = catalog.version;
    apply_props(&mut row, &ty.columns, props);
    txn.put(Cf::Node, node_key(id), encode_node(entity_id, &row));
    Ok(id)
}

pub fn get_node(txn: &Txn, id: NodeId) -> Result<Option<NodeRecord>, GraphError> {
    Ok(read_node(txn, id)?.map(|(entity_id, row)| NodeRecord { id, entity_id, row }))
}

pub fn find_by_entity_id(
    txn: &Txn,
    entity_id: &str,
    label: Option<TypeId>,
) -> Result<Vec<NodeId>, GraphError> {
    let mut found = Vec::new();
    for (key, value) in txn.scan_prefix(Cf::Pk, &pk_prefix(entity_id))? {
        let mut input = &key[key.len() - 2..];
        let ty = TypeId(get_u16(&mut input).map_err(StoreError::from)?);
        if label.is_none_or(|l| l == ty) {
            found.push(node_id_value(&value)?);
        }
    }
    Ok(found)
}

pub fn merge_edge(
    txn: &mut Txn,
    catalog: &Catalog,
    src: NodeId,
    edge_type: &str,
    dst: NodeId,
    props: &BTreeMap<String, Value>,
) -> Result<(), GraphError> {
    check_props(props)?;
    let et = catalog
        .edge_type(edge_type)
        .ok_or_else(|| GraphError::UnknownEdgeType(edge_type.to_string()))?;
    if !et.signatures.contains(&(src.type_id(), dst.type_id())) {
        return Err(GraphError::BadSignature(edge_type.to_string()));
    }
    // Rule carried from the Task 7/8 reviews (spec 1.4, first committer
    // wins): both endpoints are read `for_update`, not with a plain `get`, so
    // a concurrent `detach_delete` of either endpoint that commits before
    // this transaction makes this commit conflict instead of leaving a
    // dangling edge. See `read_node_for_update`.
    for id in [src, dst] {
        if read_node_for_update(txn, id)?.is_none() {
            return Err(GraphError::NodeNotFound(id));
        }
    }
    let out = edge_key(src, et.id, dst, EdgeId::MERGED);
    let mut row = match txn.get_for_update(Cf::Out, &out)? {
        Some(bytes) => decode_row(&bytes).map_err(StoreError::from)?,
        None => Row::default(),
    };
    row.schema_version = catalog.version;
    apply_props(&mut row, &et.columns, props);
    txn.put(Cf::Out, out, encode_row(&row));
    txn.put(
        Cf::In,
        edge_key(dst, et.id, src, EdgeId::MERGED),
        Vec::new(),
    );
    Ok(())
}

fn edges(txn: &Txn, cf: Cf, node: NodeId) -> Result<Vec<EdgeRef>, GraphError> {
    let mut found = Vec::new();
    for (key, _) in txn.scan_prefix(cf, &node.0.to_be_bytes())? {
        let (_, edge_type, other, id) = decode_edge_key(&key)?;
        found.push(EdgeRef {
            edge_type,
            other,
            id,
        });
    }
    Ok(found)
}

pub fn out_edges(txn: &Txn, src: NodeId) -> Result<Vec<EdgeRef>, GraphError> {
    edges(txn, Cf::Out, src)
}

pub fn in_edges(txn: &Txn, dst: NodeId) -> Result<Vec<EdgeRef>, GraphError> {
    edges(txn, Cf::In, dst)
}

pub fn detach_delete(txn: &mut Txn, id: NodeId) -> Result<(), GraphError> {
    let (entity_id, _) = read_node(txn, id)?.ok_or(GraphError::NodeNotFound(id))?;
    for e in out_edges(txn, id)? {
        txn.delete(Cf::Out, edge_key(id, e.edge_type, e.other, e.id));
        txn.delete(Cf::In, edge_key(e.other, e.edge_type, id, e.id));
    }
    for e in in_edges(txn, id)? {
        txn.delete(Cf::In, edge_key(id, e.edge_type, e.other, e.id));
        txn.delete(Cf::Out, edge_key(e.other, e.edge_type, id, e.id));
    }
    txn.delete(Cf::Pk, pk_key(&entity_id, id.type_id()));
    txn.delete(Cf::Node, node_key(id));
    Ok(())
}

/// Checks the invariants the crash test relies on: pk and node agree both
/// ways, node ids are unique, `out` and `in` mirror each other, and every id
/// counter is above the largest id in use.
pub fn check_invariants(db: &Database) -> Result<(), String> {
    let txn = db.begin();
    let err = |e: &dyn std::fmt::Display| e.to_string();
    let mut seen = HashSet::new();
    for (key, value) in txn.scan_prefix(Cf::Pk, &[]).map_err(|e| err(&e))? {
        let mut input = key.as_slice();
        let entity_id =
            String::from_utf8(get_bytes(&mut input).map_err(|e| err(&e))?).map_err(|e| err(&e))?;
        let ty = TypeId(get_u16(&mut input).map_err(|e| err(&e))?);
        let id = node_id_value(&value).map_err(|e| err(&e))?;
        if !seen.insert(id) {
            return Err(format!("node id {id:?} is used by two pk entries"));
        }
        let (stored, _) = read_node(&txn, id)
            .map_err(|e| err(&e))?
            .ok_or(format!("pk {entity_id} points to missing node"))?;
        if stored != entity_id || id.type_id() != ty {
            return Err(format!("pk {entity_id} and node {id:?} disagree"));
        }
    }
    for (key, value) in txn.scan_prefix(Cf::Node, &[]).map_err(|e| err(&e))? {
        let id = node_id_value(&key).map_err(|e| err(&e))?;
        let (entity_id, _) = decode_node(&value).map_err(|e| err(&e))?;
        let pk = txn
            .get(Cf::Pk, &pk_key(&entity_id, id.type_id()))
            .map_err(|e| err(&e))?;
        if pk.as_deref() != Some(&id.0.to_be_bytes()[..]) {
            return Err(format!("node {id:?} has no matching pk"));
        }
        let counter = txn
            .get(Cf::Meta, &crate::alloc::node_counter_key(id.type_id()))
            .map_err(|e| err(&e))?
            .ok_or(format!("no id counter for type {:?}", id.type_id()))?;
        let end = u64::from_be_bytes(
            counter
                .as_slice()
                .try_into()
                .map_err(|_| "bad counter".to_string())?,
        );
        if id.seq() >= end {
            return Err(format!("node {id:?} is above its counter {end}"));
        }
    }
    for (cf, mirror) in [(Cf::Out, Cf::In), (Cf::In, Cf::Out)] {
        for (key, _) in txn.scan_prefix(cf, &[]).map_err(|e| err(&e))? {
            let (a, ty, b, id) = decode_edge_key(&key).map_err(|e| err(&e))?;
            if txn
                .get(mirror, &edge_key(b, ty, a, id))
                .map_err(|e| err(&e))?
                .is_none()
            {
                return Err(format!(
                    "edge {a:?} {ty:?} {b:?} has no mirror in {}",
                    mirror.name()
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Resources, StoreConfig};

    fn setup() -> (tempfile::TempDir, Resources, Database, Catalog) {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        let db = Database::open(dir.path(), &res).unwrap();
        let mapping: GraphMapping =
            serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json"))
                .unwrap();
        let cat = db.apply_ontology(&mapping).unwrap();
        (dir, res, db, cat)
    }

    fn props(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    /// Creates a Customer and a Company in one committed transaction, for
    /// tests that need two already-existing, already-visible nodes before
    /// opening the concurrent transactions under test.
    fn seed_customer_and_company(db: &Database, cat: &Catalog) -> (NodeId, NodeId) {
        let mut t = db.begin();
        let c = merge_node(&mut t, cat, "Customer", "c:1", &BTreeMap::new()).unwrap();
        let k = merge_node(&mut t, cat, "Company", "k:1", &BTreeMap::new()).unwrap();
        t.commit().unwrap();
        (c, k)
    }

    #[test]
    fn merge_node_is_idempotent_and_null_removes() {
        let (_d, _r, db, cat) = setup();
        let mut t = db.begin();
        let a = merge_node(
            &mut t,
            &cat,
            "Customer",
            "user:7",
            &props(&[("name", Value::Str("Ana".into()))]),
        )
        .unwrap();
        let b = merge_node(
            &mut t,
            &cat,
            "Customer",
            "user:7",
            &props(&[("segment", Value::Str("smb".into())), ("name", Value::Null)]),
        )
        .unwrap();
        t.commit().unwrap();
        assert_eq!(a, b);
        let node = get_node(&db.begin(), a).unwrap().unwrap();
        let customer = cat.node_type("Customer").unwrap();
        assert_eq!(
            node.row.columns.get(&customer.column("segment").unwrap()),
            Some(&Value::Str("smb".into()))
        );
        assert!(!node
            .row
            .columns
            .contains_key(&customer.column("name").unwrap()));
    }

    #[test]
    fn finds_by_entity_id_with_and_without_label() {
        let (_d, _r, db, cat) = setup();
        let mut t = db.begin();
        merge_node(&mut t, &cat, "Customer", "user:7", &BTreeMap::new()).unwrap();
        merge_node(&mut t, &cat, "Company", "user:7", &BTreeMap::new()).unwrap();
        merge_node(&mut t, &cat, "Customer", "user:70", &BTreeMap::new()).unwrap();
        t.commit().unwrap();
        let r = db.begin();
        assert_eq!(find_by_entity_id(&r, "user:7", None).unwrap().len(), 2);
        let customer = cat.node_type("Customer").unwrap().id;
        assert_eq!(
            find_by_entity_id(&r, "user:7", Some(customer))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn merge_edge_never_duplicates_and_mirrors() {
        let (_d, _r, db, cat) = setup();
        let mut t = db.begin();
        let c = merge_node(&mut t, &cat, "Customer", "c:1", &BTreeMap::new()).unwrap();
        let k = merge_node(&mut t, &cat, "Company", "k:1", &BTreeMap::new()).unwrap();
        merge_edge(
            &mut t,
            &cat,
            c,
            "WORKS_AT",
            k,
            &props(&[("role", Value::Str("cto".into()))]),
        )
        .unwrap();
        merge_edge(&mut t, &cat, c, "WORKS_AT", k, &BTreeMap::new()).unwrap();
        t.commit().unwrap();
        let r = db.begin();
        assert_eq!(out_edges(&r, c).unwrap().len(), 1);
        let back = in_edges(&r, k).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].other, c);
    }

    #[test]
    fn a_wrong_signature_is_refused() {
        let (_d, _r, db, cat) = setup();
        let mut t = db.begin();
        let c = merge_node(&mut t, &cat, "Customer", "c:1", &BTreeMap::new()).unwrap();
        let k = merge_node(&mut t, &cat, "Company", "k:1", &BTreeMap::new()).unwrap();
        assert!(matches!(
            merge_edge(&mut t, &cat, k, "WORKS_AT", c, &BTreeMap::new()),
            Err(GraphError::BadSignature(_))
        ));
    }

    #[test]
    fn detach_delete_removes_node_key_and_both_mirrors() {
        let (_d, _r, db, cat) = setup();
        let mut t = db.begin();
        let c = merge_node(&mut t, &cat, "Customer", "c:1", &BTreeMap::new()).unwrap();
        let k = merge_node(&mut t, &cat, "Company", "k:1", &BTreeMap::new()).unwrap();
        merge_edge(&mut t, &cat, c, "WORKS_AT", k, &BTreeMap::new()).unwrap();
        t.commit().unwrap();
        let mut t = db.begin();
        detach_delete(&mut t, c).unwrap();
        t.commit().unwrap();
        let r = db.begin();
        assert!(get_node(&r, c).unwrap().is_none());
        assert!(find_by_entity_id(&r, "c:1", None).unwrap().is_empty());
        assert!(in_edges(&r, k).unwrap().is_empty());
        check_invariants(&db).unwrap();
    }

    #[test]
    fn acl_properties_are_refused_for_now() {
        let (_d, _r, db, cat) = setup();
        let mut t = db.begin();
        let r = merge_node(
            &mut t,
            &cat,
            "Customer",
            "c:1",
            &props(&[("acl_allow", Value::Null)]),
        );
        assert!(matches!(r, Err(GraphError::ReservedProperty(_))));
    }

    #[test]
    fn the_catalog_persists() {
        let (dir, res, db, _cat) = setup();
        let mapping: GraphMapping =
            serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json"))
                .unwrap();
        assert_eq!(db.apply_ontology(&mapping).unwrap().version, 2);
        drop(db);
        let db = Database::open(dir.path(), &res).unwrap();
        assert_eq!(db.catalog().unwrap().unwrap().version, 2);
    }

    // --- Added beyond the brief: the task's carried-over rule requires every
    // public function and every GraphError variant to be exercised by a
    // test. The brief's seven tests above never reach UnknownLabel,
    // UnknownEdgeType, GraphError::Store, or GraphError::Txn, and only reach
    // NodeNotFound implicitly (not at all, in fact - see below). They also
    // never prove that corrupt stored bytes become an error instead of a
    // panic, which the task states as a hard requirement.

    #[test]
    fn merge_node_with_unknown_label_is_an_error() {
        let (_d, _r, db, cat) = setup();
        let mut t = db.begin();
        match merge_node(&mut t, &cat, "Nope", "x:1", &BTreeMap::new()) {
            Err(GraphError::UnknownLabel(l)) => assert_eq!(l, "Nope"),
            other => panic!("expected UnknownLabel, got {other:?}"),
        }
    }

    #[test]
    fn merge_edge_with_unknown_edge_type_is_an_error() {
        let (_d, _r, db, cat) = setup();
        let (c, k) = seed_customer_and_company(&db, &cat);
        let mut t = db.begin();
        match merge_edge(&mut t, &cat, c, "NOPE", k, &BTreeMap::new()) {
            Err(GraphError::UnknownEdgeType(n)) => assert_eq!(n, "NOPE"),
            other => panic!("expected UnknownEdgeType, got {other:?}"),
        }
    }

    #[test]
    fn merge_edge_to_a_nonexistent_node_is_not_found() {
        let (_d, _r, db, cat) = setup();
        let mut t = db.begin();
        let c = merge_node(&mut t, &cat, "Customer", "c:1", &BTreeMap::new()).unwrap();
        let company_ty = cat.node_type("Company").unwrap().id;
        let ghost = NodeId::new(0, company_ty, 999_999).unwrap();
        match merge_edge(&mut t, &cat, c, "WORKS_AT", ghost, &BTreeMap::new()) {
            Err(GraphError::NodeNotFound(id)) => assert_eq!(id, ghost),
            other => panic!("expected NodeNotFound, got {other:?}"),
        }
    }

    #[test]
    fn detach_delete_of_a_nonexistent_node_is_not_found() {
        let (_d, _r, db, cat) = setup();
        let mut t = db.begin();
        let company_ty = cat.node_type("Company").unwrap().id;
        let ghost = NodeId::new(0, company_ty, 999_999).unwrap();
        match detach_delete(&mut t, ghost) {
            Err(GraphError::NodeNotFound(id)) => assert_eq!(id, ghost),
            other => panic!("expected NodeNotFound, got {other:?}"),
        }
    }

    #[test]
    fn merge_node_reports_not_found_when_pk_points_to_a_missing_node() {
        let (_d, _r, db, cat) = setup();
        let customer = cat.node_type("Customer").unwrap();
        let ghost = NodeId::new(0, customer.id, 4_242).unwrap();
        let mut t = db.begin();
        // Corrupt the pk index directly, in the transaction's own overlay: it
        // points at a node id that was never written to `node`. merge_node
        // must surface this as NodeNotFound when it follows the pk, not
        // panic on the missing node.
        t.put(
            Cf::Pk,
            pk_key("ghost:1", customer.id),
            ghost.0.to_be_bytes().to_vec(),
        );
        match merge_node(&mut t, &cat, "Customer", "ghost:1", &BTreeMap::new()) {
            Err(GraphError::NodeNotFound(id)) => assert_eq!(id, ghost),
            other => panic!("expected NodeNotFound, got {other:?}"),
        }
    }

    #[test]
    fn get_node_on_corrupt_bytes_is_a_store_error_not_a_panic() {
        let (_d, _r, db, _cat) = setup();
        let ghost = NodeId::new(0, TypeId(1), 7).unwrap();
        let mut t = db.begin();
        // Fewer than 4 bytes: decode_node cannot even read the entity_id
        // length prefix.
        t.put(Cf::Node, node_key(ghost), vec![0u8, 1, 2]);
        t.commit().unwrap();
        let r = db.begin();
        match get_node(&r, ghost) {
            Err(GraphError::Store(StoreError::Corrupt(_))) => {}
            Ok(_) => panic!("expected a decode error for corrupt node bytes, got Ok"),
            Err(other) => panic!("expected Store(Corrupt), got {other:?}"),
        }
    }

    #[test]
    fn apply_ontology_txn_error_surfaces_as_graph_error() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        // Create every column family, then close the read-write handle so a
        // second, read-only handle can attach to the same path (mirrors
        // txn.rs's a_failed_write_is_a_store_error_not_a_panic and alloc.rs's
        // reserve_propagates_a_store_error_instead_of_panicking).
        drop(Database::open(dir.path(), &res).unwrap());

        let opts = rocksdb::Options::default();
        let cfs = crate::db::COLUMN_FAMILIES
            .iter()
            .map(|name| rocksdb::ColumnFamilyDescriptor::new(*name, rocksdb::Options::default()));
        let ro = rocksdb::DB::open_cf_descriptors_read_only(&opts, dir.path(), cfs, false).unwrap();
        let db = Database::for_test(ro, 1_000_000);

        let mapping: GraphMapping =
            serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json"))
                .unwrap();
        match db.apply_ontology(&mapping) {
            Err(GraphError::Txn(TxnError::Store(_))) => {}
            other => panic!("expected Txn(Store), got {other:?}"),
        }
    }

    // --- Two rules carried from the Task 7/8 reviews into this task's
    // dispatch (spec 1.4, first committer wins). Rule 1: merge_edge must
    // depend on both endpoints' node keys via get_for_update, so a
    // concurrent detach_delete of an endpoint makes one of the two commits
    // conflict instead of leaving a dangling edge (Rule 2 - no write outside
    // a Txn - is a structural property checked by inspection: every write in
    // this file goes through Txn::put/Txn::delete followed by Txn::commit,
    // never through Database's private `db` field).

    #[test]
    fn merge_edge_conflicts_when_a_concurrent_detach_delete_of_the_destination_commits_first() {
        let (_d, _r, db, cat) = setup();
        let (c, k) = seed_customer_and_company(&db, &cat);

        let mut merge_txn = db.begin();
        let mut delete_txn = db.begin();
        merge_edge(&mut merge_txn, &cat, c, "WORKS_AT", k, &BTreeMap::new()).unwrap();
        detach_delete(&mut delete_txn, k).unwrap();
        delete_txn.commit().unwrap();
        assert!(matches!(merge_txn.commit(), Err(TxnError::Conflict)));
    }

    #[test]
    fn merge_edge_conflicts_when_a_concurrent_detach_delete_of_the_source_commits_first() {
        let (_d, _r, db, cat) = setup();
        let (c, k) = seed_customer_and_company(&db, &cat);

        let mut merge_txn = db.begin();
        let mut delete_txn = db.begin();
        merge_edge(&mut merge_txn, &cat, c, "WORKS_AT", k, &BTreeMap::new()).unwrap();
        detach_delete(&mut delete_txn, c).unwrap();
        delete_txn.commit().unwrap();
        assert!(matches!(merge_txn.commit(), Err(TxnError::Conflict)));
    }

    // The reverse commit order: merge_edge reaches commit() FIRST, and the
    // concurrent detach_delete of its destination commits second. Documented
    // here rather than left untested, because it is the asymmetric half of
    // rule 1 that the fix does *not* close (see task-9-report.md, Concerns):
    // detach_delete's own scan for edges to delete (`out_edges`/`in_edges`)
    // is a range read that records no dependency (this transaction model has
    // no phantom-read protection, only exact-key tracking), so when its
    // snapshot predates the new edge, it neither depends on nor overwrites
    // any key merge_edge wrote, and it commits clean, unaware of the edge it
    // just made dangling. Fixing this fully would need detach_delete to
    // depend on something that changes whenever an edge touching the node is
    // added - out of scope for this task (graph.rs only; txn.rs is frozen).
    // This test pins the *current, true* behavior so a future change to
    // either function is forced to update it deliberately instead of
    // silently drifting.
    #[test]
    fn a_detach_delete_that_commits_after_a_concurrent_merge_edge_does_not_see_the_new_edge() {
        let (_d, _r, db, cat) = setup();
        let (c, k) = seed_customer_and_company(&db, &cat);

        let mut merge_txn = db.begin();
        let mut delete_txn = db.begin();
        merge_edge(&mut merge_txn, &cat, c, "WORKS_AT", k, &BTreeMap::new()).unwrap();
        // delete_txn's snapshot predates merge_txn's commit, so its own scan
        // for k's edges (inside detach_delete) finds none yet.
        detach_delete(&mut delete_txn, k).unwrap();
        merge_txn.commit().unwrap();
        delete_txn.commit().unwrap();

        // k is gone, but the edge merge_txn created now points at a deleted
        // node: exactly the dangling edge rule 1 is about, in the one
        // ordering the fix does not cover. check_invariants does not check
        // edge endpoint existence (only pk/node agreement and out/in
        // mirroring, per its own doc comment), so it stays quiet here; the
        // mirror itself is still intact.
        let r = db.begin();
        assert!(get_node(&r, k).unwrap().is_none());
        assert_eq!(out_edges(&r, c).unwrap().len(), 1);
        check_invariants(&db).unwrap();
    }
}
