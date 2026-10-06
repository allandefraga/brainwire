//! Graph operations on a transaction (spec 1.2): nodes by primary key, edges
//! mirrored in `out` and `in`, detach delete, and the invariant check used by
//! the crash test.

use std::collections::{BTreeMap, HashSet};

use brainwire_core::codec::{decode_row, encode_row, Row};
use brainwire_core::ids::{EdgeId, EdgeTypeId, IdError, NodeId, TypeId};
use brainwire_core::keys::{get_bytes, get_u16, get_u64, put_bytes, put_u16, put_u64};
use brainwire_core::schema::{Catalog, Column, GraphMapping, NodeType};
use brainwire_core::sparse::{SparseMatrix, SparseMatrixError};
use brainwire_core::value::Value;

use crate::db::{Database, StoreError};
use crate::txn::{Cf, Txn, TxnError};

const CATALOG_KEY: &[u8] = b"catalog";
const INDEX_READY_KEY: &[u8] = b"scalar-index-v2-ready";
const MAX_ATOMIC_BACKFILL_KEYS: usize = 100_000;

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
    #[error(
        "scalar index backfill exceeds {MAX_ATOMIC_BACKFILL_KEYS} keys; use a bounded migration"
    )]
    IndexBackfillTooLarge,
    #[error(transparent)]
    Id(#[from] IdError),
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

/// Lightweight cardinality information for observability and the backoffice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphStats {
    pub nodes: u64,
    pub edges: u64,
}

/// A rebuildable CSR view of one graph (or one relationship type).
#[derive(Debug, Clone, PartialEq)]
pub struct SparseGraphProjection {
    pub nodes: Vec<NodeId>,
    pub matrix: SparseMatrix,
    pub relationship_count: usize,
    pub scanned_relationship_count: usize,
    pub out_degrees: Vec<u64>,
    pub in_degrees: Vec<u64>,
}

impl SparseGraphProjection {
    /// Returns `(NodeId, score)` pairs in the same stable order as `nodes`.
    pub fn pagerank(
        &self,
        iterations: usize,
        damping: f32,
    ) -> Result<Vec<(NodeId, f32)>, SparseMatrixError> {
        if self.nodes.len() != self.matrix.shape().0 {
            return Err(SparseMatrixError::VectorLength {
                expected: self.matrix.shape().0,
                actual: self.nodes.len(),
            });
        }
        Ok(self
            .nodes
            .iter()
            .copied()
            .zip(self.matrix.pagerank(iterations, damping)?)
            .collect())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProjectionError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Matrix(#[from] SparseMatrixError),
    #[error("edge references a node missing from the node column family")]
    MissingNode,
    #[error("projection exceeds {resource} limit ({limit})")]
    Limit {
        resource: &'static str,
        limit: usize,
    },
    #[error(
        "projection allocation guard exceeded: attempted {attempted} bytes, limit {limit} bytes"
    )]
    MemoryLimit { limit: usize, attempted: usize },
    #[error("invalid projection: {0}")]
    Invalid(String),
}

/// Limits bound cardinality, not total process memory. Every node is included;
/// max_relationships bounds all outgoing records scanned, before type filtering.
#[derive(Debug, Clone)]
pub struct ProjectionOptions {
    pub relationship_type: Option<EdgeTypeId>,
    pub weight_property: Option<String>,
    pub max_nodes: usize,
    pub max_relationships: usize,
    /// Conservative construction allocation guard, not process RSS.
    pub max_bytes: usize,
}

impl Default for ProjectionOptions {
    fn default() -> Self {
        Self {
            relationship_type: None,
            weight_property: None,
            max_nodes: 100_000,
            max_relationships: 1_000_000,
            max_bytes: usize::MAX,
        }
    }
}

/// Build an ephemeral projection using the caller's snapshot and write overlay.
/// Missing weights default to one; explicit weights must be nonnegative finite
/// numbers representable as f32. Zero-weight edges count toward degrees, but
/// have no influence in the matrix. No registry or cached state is created.
pub fn sparse_projection(
    txn: &Txn<'_>,
    options: &ProjectionOptions,
) -> Result<SparseGraphProjection, ProjectionError> {
    if options.max_nodes == 0 || options.max_relationships == 0 {
        return Err(ProjectionError::Invalid("limits must be positive".into()));
    }
    let catalog: Option<Catalog> = txn
        .get(Cf::Meta, CATALOG_KEY)?
        .map(|bytes| serde_json::from_slice(&bytes).map_err(catalog_error))
        .transpose()?;
    if options.relationship_type.is_some_and(|id| {
        catalog
            .as_ref()
            .is_none_or(|c| !c.edge_types.iter().any(|t| t.id == id))
    }) {
        return Err(ProjectionError::Invalid("unknown relationship type".into()));
    }
    let mut estimated_bytes = 0usize;
    let mut reserve = |bytes: usize| {
        estimated_bytes = estimated_bytes.saturating_add(bytes);
        if estimated_bytes > options.max_bytes {
            Err(ProjectionError::MemoryLimit {
                limit: options.max_bytes,
                attempted: estimated_bytes,
            })
        } else {
            Ok(())
        }
    };
    let mut nodes = Vec::new();
    txn.visit_prefix::<ProjectionError>(Cf::Node, &[], |key, _| {
        if nodes.len() == options.max_nodes {
            return Err(ProjectionError::Limit {
                resource: "maxNodes",
                limit: options.max_nodes,
            });
        }
        reserve(128)?;
        nodes.push(node_id_value(key)?);
        Ok(())
    })?;
    let mut out_degrees = vec![0; nodes.len()];
    let mut in_degrees = vec![0; nodes.len()];
    let mut entries = Vec::new();
    let mut scanned = 0;
    let mut selected = 0;
    txn.visit_prefix::<ProjectionError>(Cf::Out, &[], |key, value| {
        if scanned == options.max_relationships {
            return Err(ProjectionError::Limit {
                resource: "maxRelationships",
                limit: options.max_relationships,
            });
        }
        scanned += 1;
        if key.len() != 26 {
            return Err(StoreError::Corrupt("edge key length".into()).into());
        }
        let (source, edge_type, target, _) = decode_edge_key(key)?;
        if options
            .relationship_type
            .is_some_and(|expected| expected != edge_type)
        {
            return Ok(());
        }
        let row = nodes
            .binary_search(&source)
            .map_err(|_| ProjectionError::MissingNode)?;
        let col = nodes
            .binary_search(&target)
            .map_err(|_| ProjectionError::MissingNode)?;
        let weight = if let Some(name) = &options.weight_property {
            let edge_schema = catalog
                .as_ref()
                .and_then(|c| c.edge_types.iter().find(|ty| ty.id == edge_type))
                .ok_or_else(|| StoreError::Corrupt("edge type absent from catalog".into()))?;
            reserve(value.len().saturating_mul(4))?;
            let record = decode_row(value).map_err(StoreError::from)?;
            let property = edge_schema
                .columns
                .iter()
                .find(|c| &c.name == name)
                .and_then(|c| record.columns.get(&c.id))
                .or_else(|| record.extra.get(name));
            let number = match property {
                None => 1.0,
                Some(Value::Int(v)) => *v as f64,
                Some(Value::Float(v)) => *v,
                _ => {
                    return Err(ProjectionError::Invalid(format!(
                        "weight property {name} must be numeric"
                    )))
                }
            };
            let weight = number as f32;
            if !number.is_finite()
                || number < 0.0
                || !weight.is_finite()
                || (number > 0.0 && weight == 0.0)
            {
                return Err(ProjectionError::Invalid(format!(
                    "weight property {name} must be finite, non-negative and representable as f32"
                )));
            }
            weight
        } else {
            1.0
        };
        selected += 1;
        out_degrees[row] += 1;
        in_degrees[col] += 1;
        reserve(128)?;
        entries.push((row, col, weight));
        Ok(())
    })?;
    Ok(SparseGraphProjection {
        matrix: SparseMatrix::from_entries(nodes.len(), nodes.len(), entries)?,
        nodes,
        relationship_count: selected,
        scanned_relationship_count: scanned,
        out_degrees,
        in_degrees,
    })
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
    /// Builds an in-memory CSR adjacency projection from the durable graph.
    /// Node order is stable because RocksDB keys are scanned in NodeId order.
    /// The projection is disposable and can be rebuilt after a restart.
    pub fn sparse_projection(
        &self,
        relationship_type: Option<EdgeTypeId>,
    ) -> Result<SparseGraphProjection, ProjectionError> {
        sparse_projection(
            &self.begin(),
            &ProjectionOptions {
                relationship_type,
                ..ProjectionOptions::default()
            },
        )
    }

    /// Count persisted graph entities without decoding every node or edge.
    /// The count runs in a read-only snapshot and therefore does not create a
    /// commit or participate in conflict detection.
    pub fn stats(&self) -> Result<GraphStats, StoreError> {
        let txn = self.begin();
        let mut stats = GraphStats { nodes: 0, edges: 0 };
        txn.visit_prefix::<StoreError>(Cf::Node, &[], |_, _| {
            stats.nodes += 1;
            Ok(())
        })?;
        txn.visit_prefix::<StoreError>(Cf::Out, &[], |_, _| {
            stats.edges += 1;
            Ok(())
        })?;
        Ok(stats)
    }

    pub fn catalog(&self) -> Result<Option<Catalog>, StoreError> {
        match self.begin().get(Cf::Meta, CATALOG_KEY)? {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(catalog_error),
            None => Ok(None),
        }
    }

    pub fn scalar_indexes_ready(&self) -> Result<bool, StoreError> {
        scalar_indexes_ready(&self.begin())
    }

    pub fn apply_ontology(&self, mapping: &GraphMapping) -> Result<Catalog, GraphError> {
        let mut txn = self.begin();
        let next = self.stage_ontology(&mut txn, mapping)?;
        txn.commit()?;
        Ok(next)
    }

    fn stage_ontology(
        &self,
        txn: &mut Txn<'_>,
        mapping: &GraphMapping,
    ) -> Result<Catalog, GraphError> {
        let previous: Option<Catalog> = match txn.get_for_update(Cf::Meta, CATALOG_KEY)? {
            Some(bytes) => Some(serde_json::from_slice(&bytes).map_err(catalog_error)?),
            None => None,
        };
        let next = Catalog::apply(mapping, previous.as_ref()).map_err(catalog_error)?;
        let was_ready = scalar_indexes_ready(txn)?;
        let mut backfilled = 0;
        // A new declaration is backfilled in the same atomic batch as the
        // catalog. Old databases have no marker, so their entire declaration
        // set is rebuilt before a query may trust the index.
        for ty in &next.node_types {
            for field in &ty.indexed {
                let already_indexed = was_ready
                    && previous
                        .as_ref()
                        .and_then(|old| old.node_type(&ty.label))
                        .is_some_and(|old| old.indexed.contains(field));
                if already_indexed {
                    continue;
                }
                let prefix = index_field_prefix(ty.id, field);
                // A declaration may have been removed and later reintroduced.
                // Clear that field's old keys before rebuilding; otherwise a
                // deleted/changed node could reappear as a stale candidate.
                let mut obsolete = Vec::new();
                txn.visit_prefix_for_update::<GraphError>(Cf::Idx, &prefix, |key, _| {
                    if backfilled + obsolete.len() == MAX_ATOMIC_BACKFILL_KEYS {
                        return Err(GraphError::IndexBackfillTooLarge);
                    }
                    obsolete.push(key.to_vec());
                    Ok(())
                })?;
                backfilled += obsolete.len();
                for key in obsolete {
                    txn.delete(Cf::Idx, key);
                }
                let mut keys = Vec::new();
                txn.visit_prefix_for_update::<GraphError>(Cf::Node, &[], |key, bytes| {
                    let id = node_id_value(key)?;
                    if id.type_id() == ty.id {
                        let (_, row) = decode_node(bytes)?;
                        if let Some(index_key) = property_value(&row, ty, field)
                            .and_then(|value| index_key(&prefix, value, id))
                        {
                            if backfilled + keys.len() == MAX_ATOMIC_BACKFILL_KEYS {
                                return Err(GraphError::IndexBackfillTooLarge);
                            }
                            keys.push(index_key);
                        }
                    }
                    Ok(())
                })?;
                backfilled += keys.len();
                for key in keys {
                    txn.put(Cf::Idx, key, Vec::new());
                }
            }
        }
        txn.put(
            Cf::Meta,
            CATALOG_KEY.to_vec(),
            serde_json::to_vec(&next).map_err(catalog_error)?,
        );
        txn.put(Cf::Meta, INDEX_READY_KEY.to_vec(), vec![2]);
        txn.delete(Cf::Meta, b"scalar-index-v1-ready".to_vec());
        Ok(next)
    }
}

fn node_key(id: NodeId) -> Vec<u8> {
    id.0.to_be_bytes().to_vec()
}

fn index_field_prefix(ty: TypeId, field: &str) -> Vec<u8> {
    let mut key = Vec::new();
    put_u16(&mut key, ty.0);
    put_bytes(&mut key, field.as_bytes());
    key
}

fn index_value(value: &Value) -> Option<Vec<u8>> {
    match value {
        // Integral finite floats share the exact i64 key when representable.
        // This matches Cypher's precise int/float comparison, including -0.0.
        Value::Int(value) => {
            let mut key = vec![b'i'];
            key.extend(value.to_be_bytes());
            Some(key)
        }
        Value::Float(value) if value.is_finite() => {
            if value.fract() == 0.0
                && *value >= -9223372036854775808.0
                && *value < 9223372036854775808.0
            {
                index_value(&Value::Int(*value as i64))
            } else {
                let mut key = vec![b'f'];
                key.extend(value.to_bits().to_be_bytes());
                Some(key)
            }
        }
        Value::Bool(value) => Some(vec![b'b', u8::from(*value)]),
        Value::Str(value) => {
            let mut encoded = vec![b's'];
            put_bytes(&mut encoded, value.as_bytes());
            Some(encoded)
        }
        _ => None,
    }
}

fn index_key(field_prefix: &[u8], value: &Value, id: NodeId) -> Option<Vec<u8>> {
    let mut key = field_prefix.to_vec();
    key.extend(index_value(value)?);
    put_u64(&mut key, id.0);
    Some(key)
}

fn property_value<'a>(row: &'a Row, ty: &NodeType, field: &str) -> Option<&'a Value> {
    ty.column(field)
        .and_then(|id| row.columns.get(&id))
        .or_else(|| row.extra.get(field))
}

fn update_node_indexes(txn: &mut Txn<'_>, ty: &NodeType, id: NodeId, old: &Row, new: &Row) {
    for field in &ty.indexed {
        let prefix = index_field_prefix(ty.id, field);
        let old_key = property_value(old, ty, field).and_then(|v| index_key(&prefix, v, id));
        let new_key = property_value(new, ty, field).and_then(|v| index_key(&prefix, v, id));
        if old_key == new_key {
            continue;
        }
        if let Some(key) = old_key {
            txn.delete(Cf::Idx, key);
        }
        if let Some(key) = new_key {
            txn.put(Cf::Idx, key, Vec::new());
        }
    }
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
                // A row written before this name was a column may still
                // carry the old value in `extra` (fix round 1, reviewer
                // Important 1): a property that gets promoted to a column
                // must not leave a stale value behind under its old home.
                row.extra.remove(name);
            }
            Some(id) => {
                row.columns.insert(id, value.clone());
                row.extra.remove(name);
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
    let old = row.clone();
    row.schema_version = catalog.version;
    apply_props(&mut row, &ty.columns, props);
    update_node_indexes(txn, ty, id, &old, &row);
    txn.put(Cf::Node, node_key(id), encode_node(entity_id, &row));
    Ok(id)
}

pub fn get_node(txn: &Txn, id: NodeId) -> Result<Option<NodeRecord>, GraphError> {
    Ok(read_node(txn, id)?.map(|(entity_id, row)| NodeRecord { id, entity_id, row }))
}

/// Lists nodes of one type in partition zero by their encoded id order.
pub fn nodes_of_type(txn: &Txn, ty: TypeId) -> Result<Vec<NodeId>, GraphError> {
    let mut nodes = Vec::new();
    visit_nodes_of_type::<GraphError>(txn, ty, |id| {
        nodes.push(id);
        Ok(())
    })?;
    Ok(nodes)
}

/// Stream matching IDs without copying every node's stored properties.
pub fn visit_nodes_of_type<E: From<GraphError> + From<StoreError>>(
    txn: &Txn,
    ty: TypeId,
    mut visit: impl FnMut(NodeId) -> Result<(), E>,
) -> Result<(), E> {
    let (lo, inclusive_hi) = NodeId::type_range(0, ty).map_err(GraphError::from)?;
    let hi = inclusive_hi.0 + 1;
    // Callback errors must propagate in the caller's error type.
    txn.visit_range::<E>(
        Cf::Node,
        &lo.0.to_be_bytes(),
        &hi.to_be_bytes(),
        |key, _| visit(node_id_value(key).map_err(GraphError::from)?),
    )
}

pub fn find_by_entity_id(
    txn: &Txn,
    entity_id: &str,
    label: Option<TypeId>,
) -> Result<Vec<NodeId>, GraphError> {
    if let Some(ty) = label {
        return txn
            .get(Cf::Pk, &pk_key(entity_id, ty))?
            .map(|value| {
                node_id_value(&value)
                    .map(|id| vec![id])
                    .map_err(GraphError::from)
            })
            .transpose()
            .map(|found| found.unwrap_or_default());
    }
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

/// Read index readiness from the caller's snapshot, including its write overlay.
pub fn scalar_indexes_ready(txn: &Txn<'_>) -> Result<bool, StoreError> {
    Ok(txn.get(Cf::Meta, INDEX_READY_KEY)?.as_deref() == Some(&[2][..]))
}

/// Returns None when this property cannot safely use the ready scalar index.
/// The caller must then retain the normal label-scan plan.
pub fn find_by_indexed_property(
    txn: &Txn<'_>,
    ty: &NodeType,
    field: &str,
    value: &Value,
) -> Result<Option<Vec<NodeId>>, GraphError> {
    let mut found = Vec::new();
    let ready = visit_by_indexed_property::<GraphError>(txn, ty, field, value, |id| {
        found.push(id);
        Ok(())
    })?;
    Ok(ready.then_some(found))
}

/// Returns false when no ready index supports the value; otherwise streams IDs.
pub fn visit_by_indexed_property<E: From<GraphError> + From<StoreError>>(
    txn: &Txn<'_>,
    ty: &NodeType,
    field: &str,
    value: &Value,
    mut visit: impl FnMut(NodeId) -> Result<(), E>,
) -> Result<bool, E> {
    if !ty.indexed.iter().any(|name| name == field) || !scalar_indexes_ready(txn)? {
        return Ok(false);
    }
    let Some(encoded) = index_value(value) else {
        return Ok(false);
    };
    let mut prefix = index_field_prefix(ty.id, field);
    prefix.extend(encoded);
    txn.visit_prefix::<E>(Cf::Idx, &prefix, |key, _| {
        if key.len() != prefix.len() + 8 {
            return Err(StoreError::Corrupt("scalar index key length".into()).into());
        }
        visit(node_id_value(&key[prefix.len()..]).map_err(GraphError::from)?)
    })?;
    Ok(true)
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

/// Stream directional adjacency without copying stored edge property values.
pub fn visit_edges<E: From<GraphError> + From<StoreError>>(
    txn: &Txn,
    cf: Cf,
    node: NodeId,
    visit: impl FnMut(EdgeRef) -> Result<(), E>,
) -> Result<(), E> {
    visit_edge_prefix(txn, cf, &node.0.to_be_bytes(), visit)
}

/// Stream one relationship type using the exact node/type key prefix.
/// Snapshot visibility, pending writes and callback errors follow `visit_edges`.
pub fn visit_edges_of_type<E: From<GraphError> + From<StoreError>>(
    txn: &Txn,
    cf: Cf,
    node: NodeId,
    edge_type: EdgeTypeId,
    visit: impl FnMut(EdgeRef) -> Result<(), E>,
) -> Result<(), E> {
    let mut prefix = [0; 10];
    prefix[..8].copy_from_slice(&node.0.to_be_bytes());
    prefix[8..].copy_from_slice(&edge_type.0.to_be_bytes());
    visit_edge_prefix(txn, cf, &prefix, visit)
}

fn visit_edge_prefix<E: From<GraphError> + From<StoreError>>(
    txn: &Txn,
    cf: Cf,
    prefix: &[u8],
    mut visit: impl FnMut(EdgeRef) -> Result<(), E>,
) -> Result<(), E> {
    if !matches!(cf, Cf::Out | Cf::In) {
        return Err(
            StoreError::Corrupt("adjacency requires out or in column family".into()).into(),
        );
    }
    txn.visit_prefix::<E>(cf, prefix, |key, _| {
        let (_, edge_type, other, id) = decode_edge_key(key).map_err(GraphError::from)?;
        visit(EdgeRef {
            edge_type,
            other,
            id,
        })
    })
}
fn edges(txn: &Txn, cf: Cf, node: NodeId) -> Result<Vec<EdgeRef>, GraphError> {
    let mut found = Vec::new();
    visit_edges::<GraphError>(txn, cf, node, |edge| {
        found.push(edge);
        Ok(())
    })?;
    Ok(found)
}
pub fn out_edges(txn: &Txn, src: NodeId) -> Result<Vec<EdgeRef>, GraphError> {
    edges(txn, Cf::Out, src)
}
pub fn in_edges(txn: &Txn, dst: NodeId) -> Result<Vec<EdgeRef>, GraphError> {
    edges(txn, Cf::In, dst)
}

/// Same listing as `edges`, but through `scan_prefix_for_update`: the whole
/// (cf, node-prefix) range becomes a tracked dependency, so a concurrent
/// `merge_edge` that adds a *new* edge under this node — one this scan could
/// not have seen — makes whichever of the two transactions commits second
/// conflict, instead of the edge surviving with a since-deleted endpoint
/// (fix round 1, reviewer Critical). `merge_edge`'s own `get_for_update` on
/// the node key (see `read_node_for_update`) only protects the opposite
/// commit order; this closes the other half.
fn edges_for_update(txn: &mut Txn, cf: Cf, node: NodeId) -> Result<Vec<EdgeRef>, GraphError> {
    let mut found = Vec::new();
    for (key, _) in txn.scan_prefix_for_update(cf, &node.0.to_be_bytes())? {
        let (_, edge_type, other, id) = decode_edge_key(&key)?;
        found.push(EdgeRef {
            edge_type,
            other,
            id,
        });
    }
    Ok(found)
}

pub fn detach_delete(txn: &mut Txn, id: NodeId) -> Result<(), GraphError> {
    let (entity_id, row) = read_node(txn, id)?.ok_or(GraphError::NodeNotFound(id))?;
    if let Some(bytes) = txn.get(Cf::Meta, CATALOG_KEY)? {
        let catalog: Catalog = serde_json::from_slice(&bytes).map_err(catalog_error)?;
        if let Some(ty) = catalog.node_types.iter().find(|ty| ty.id == id.type_id()) {
            update_node_indexes(txn, ty, id, &row, &Row::default());
        }
    }
    for e in edges_for_update(txn, Cf::Out, id)? {
        txn.delete(Cf::Out, edge_key(id, e.edge_type, e.other, e.id));
        txn.delete(Cf::In, edge_key(e.other, e.edge_type, id, e.id));
    }
    for e in edges_for_update(txn, Cf::In, id)? {
        txn.delete(Cf::In, edge_key(id, e.edge_type, e.other, e.id));
        txn.delete(Cf::Out, edge_key(e.other, e.edge_type, id, e.id));
    }
    txn.delete(Cf::Pk, pk_key(&entity_id, id.type_id()));
    txn.delete(Cf::Node, node_key(id));
    Ok(())
}

/// Checks the invariants the crash test relies on: pk and node agree both
/// ways, node ids are unique, `out` and `in` mirror each other with both
/// endpoints still existing as nodes, and every id counter is above the
/// largest id in use.
pub fn check_invariants(db: &Database) -> Result<(), String> {
    let txn = db.begin();
    let err = |e: &dyn std::fmt::Display| e.to_string();
    let catalog: Option<Catalog> = txn
        .get(Cf::Meta, CATALOG_KEY)
        .map_err(|e| err(&e))?
        .map(|bytes| serde_json::from_slice(&bytes))
        .transpose()
        .map_err(|e| err(&e))?;
    let ready = scalar_indexes_ready(&txn).map_err(|e| err(&e))?;
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
        let (entity_id, row) = decode_node(&value).map_err(|e| err(&e))?;
        if ready {
            if let Some(ty) = catalog
                .as_ref()
                .and_then(|c| c.node_types.iter().find(|ty| ty.id == id.type_id()))
            {
                for field in &ty.indexed {
                    if let Some(key) = property_value(&row, ty, field)
                        .and_then(|v| index_key(&index_field_prefix(ty.id, field), v, id))
                    {
                        if txn.get(Cf::Idx, &key).map_err(|e| err(&e))?.is_none() {
                            return Err(format!(
                                "missing index entry for {}.{field} node {id:?}",
                                ty.label
                            ));
                        }
                    }
                }
            }
        }
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
    if ready {
        if let Some(catalog) = &catalog {
            for ty in &catalog.node_types {
                for field in &ty.indexed {
                    let prefix = index_field_prefix(ty.id, field);
                    txn.visit_prefix::<StoreError>(Cf::Idx, &prefix, |key, _| {
                        let bad = || {
                            StoreError::Corrupt(format!(
                                "stale index entry for {}.{field}",
                                ty.label
                            ))
                        };
                        let tail = key.get(key.len().saturating_sub(8)..).ok_or_else(bad)?;
                        let id = node_id_value(tail)?;
                        let bytes = txn.get(Cf::Node, &node_key(id))?.ok_or_else(bad)?;
                        let (_, row) = decode_node(&bytes)?;
                        let expected =
                            property_value(&row, ty, field).and_then(|v| index_key(&prefix, v, id));
                        if id.type_id() != ty.id || expected.as_deref() != Some(key) {
                            return Err(bad());
                        }
                        Ok(())
                    })
                    .map_err(|e| err(&e))?;
                }
            }
        }
    }
    for (cf, mirror) in [(Cf::Out, Cf::In), (Cf::In, Cf::Out)] {
        for (key, _) in txn.scan_prefix(cf, &[]).map_err(|e| err(&e))? {
            let (a, ty, b, id) = decode_edge_key(&key).map_err(|e| err(&e))?;
            // `a` is this key's own side (source in Out, target in In): across
            // both directions of this outer loop, every logical edge's two
            // endpoints each get checked here once (fix round 1, reviewer
            // Important 2b — previously a dangling edge, endpoint deleted
            // out from under it, passed this check silently).
            if txn
                .get(Cf::Node, &node_key(a))
                .map_err(|e| err(&e))?
                .is_none()
            {
                return Err(format!(
                    "edge {a:?} {ty:?} {b:?} has a missing endpoint {a:?}"
                ));
            }
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
    use brainwire_core::schema::NodeSpec;

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

    #[test]
    fn typed_adjacency_preserves_snapshot_overlay_direction_and_edge_ids() {
        let (_dir, _res, db, _cat) = setup();
        let node = NodeId(0x0102_0304_0506_0708);
        let neighbor = NodeId(node.0 + 1);
        let selected = EdgeTypeId(0x00ff);
        let adjacent = EdgeTypeId(0x0100);
        let refs = |txn: &Txn<'_>, cf, ty| {
            let mut found = Vec::new();
            visit_edges_of_type::<GraphError>(txn, cf, node, ty, |edge| {
                found.push((edge.edge_type, edge.other, edge.id));
                Ok(())
            })
            .unwrap();
            found
        };
        let mut seed = db.begin();
        for cf in [Cf::Out, Cf::In] {
            for (ty, id) in [(selected, EdgeId(7)), (adjacent, EdgeId(9))] {
                seed.put(cf, edge_key(node, ty, neighbor, id), Vec::new());
            }
            // Neither a neighboring node nor a neighboring type belongs to the prefix.
            seed.put(
                cf,
                edge_key(neighbor, selected, node, EdgeId(11)),
                Vec::new(),
            );
        }
        seed.commit().unwrap();
        let mut snapshot = db.begin();
        let mut writer = db.begin();
        for cf in [Cf::Out, Cf::In] {
            writer.put(
                cf,
                edge_key(node, selected, neighbor, EdgeId(8)),
                Vec::new(),
            );
        }
        writer.commit().unwrap();
        for cf in [Cf::Out, Cf::In] {
            assert_eq!(
                refs(&snapshot, cf, selected),
                [(selected, neighbor, EdgeId(7))]
            );
            assert_eq!(
                refs(&db.begin(), cf, selected),
                [
                    (selected, neighbor, EdgeId(7)),
                    (selected, neighbor, EdgeId(8))
                ]
            );
            assert_eq!(
                refs(&snapshot, cf, adjacent),
                [(adjacent, neighbor, EdgeId(9))]
            );
            assert!(refs(&snapshot, cf, EdgeTypeId(0x00fe)).is_empty());
            snapshot.delete(cf, edge_key(node, selected, neighbor, EdgeId(7)));
            snapshot.put(cf, edge_key(node, selected, node, EdgeId(42)), Vec::new());
            assert_eq!(
                refs(&snapshot, cf, selected),
                [(selected, node, EdgeId(42))]
            );
            let mut all = Vec::new();
            visit_edges::<GraphError>(&snapshot, cf, node, |edge| {
                all.push(edge.edge_type);
                Ok(())
            })
            .unwrap();
            assert_eq!(all, [selected, adjacent]);
        }
    }

    #[test]
    fn typed_adjacency_validates_family_and_stops_on_callback_error() {
        let (_dir, _res, db, _cat) = setup();
        let node = NodeId(1);
        let ty = EdgeTypeId(0x0100);
        let mut txn = db.begin();
        txn.put(
            Cf::Out,
            edge_key(node, ty, NodeId(2), EdgeId(7)),
            Vec::new(),
        );
        txn.put(
            Cf::Out,
            edge_key(node, ty, NodeId(3), EdgeId(8)),
            Vec::new(),
        );
        let mut callbacks = 0;
        let result = visit_edges_of_type::<GraphError>(&txn, Cf::Out, node, ty, |_| {
            callbacks += 1;
            Err(StoreError::Corrupt("callback stopped".into()).into())
        });
        assert_eq!(callbacks, 1);
        assert!(result.unwrap_err().to_string().contains("callback stopped"));
        for cf in [Cf::Node, Cf::Meta, Cf::Pk, Cf::Idx] {
            assert!(visit_edges_of_type::<GraphError>(&txn, cf, node, ty, |_| {
                panic!("invalid family must not invoke callback")
            })
            .unwrap_err()
            .to_string()
            .contains("out or in"));
        }
        let mut corrupt = edge_key(node, ty, NodeId(4), EdgeId(9));
        corrupt.pop();
        txn.put(Cf::Out, corrupt, Vec::new());
        assert!(visit_edges_of_type::<GraphError>(&txn, Cf::Out, node, ty, |_| Ok(())).is_err());
    }

    #[test]
    fn index_backfill_conflicts_if_a_writer_commits_after_its_snapshot() {
        let (_dir, _res, db, cat) = setup();
        let mut mapping: GraphMapping =
            serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json"))
                .unwrap();
        mapping.nodes[0].indexed_fields.push("name".into());
        let mut migration = db.begin();
        db.stage_ontology(&mut migration, &mapping).unwrap();
        let mut writer = db.begin();
        merge_node(
            &mut writer,
            &cat,
            "Customer",
            "new",
            &props(&[("name", Value::Str("Alice".into()))]),
        )
        .unwrap();
        writer.commit().unwrap();
        assert!(matches!(migration.commit(), Err(TxnError::Conflict)));
        assert_eq!(db.catalog().unwrap().unwrap(), cat);
        let next = db.apply_ontology(&mapping).unwrap();
        assert_eq!(
            find_by_indexed_property(
                &db.begin(),
                next.node_type("Customer").unwrap(),
                "name",
                &Value::Str("Alice".into())
            )
            .unwrap()
            .unwrap()
            .len(),
            1
        );
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
    fn nodes_of_type_reads_own_writes_and_deletes_in_id_order() {
        let (_d, _r, db, cat) = setup();
        let customer = cat.node_type("Customer").unwrap().id;
        let company = cat.node_type("Company").unwrap().id;
        let mut seed = db.begin();
        let c1 = merge_node(&mut seed, &cat, "Customer", "c:1", &BTreeMap::new()).unwrap();
        let k1 = merge_node(&mut seed, &cat, "Company", "k:1", &BTreeMap::new()).unwrap();
        let c2 = merge_node(&mut seed, &cat, "Customer", "c:2", &BTreeMap::new()).unwrap();
        seed.commit().unwrap();

        let mut txn = db.begin();
        let k2 = merge_node(&mut txn, &cat, "Company", "k:2", &BTreeMap::new()).unwrap();
        let c3 = merge_node(&mut txn, &cat, "Customer", "c:3", &BTreeMap::new()).unwrap();
        detach_delete(&mut txn, c1).unwrap();
        assert_eq!(nodes_of_type(&txn, customer).unwrap(), vec![c2, c3]);
        assert_eq!(nodes_of_type(&txn, company).unwrap(), vec![k1, k2]);
        assert!(nodes_of_type(&txn, TypeId(100)).unwrap().is_empty());
    }

    #[test]
    fn nodes_of_type_includes_last_sequence_and_isolates_maximum_type() {
        let (_d, _r, db, _cat) = setup();
        let max_seq = (1u64 << 42) - 1;
        let last = NodeId::new(0, TypeId(TypeId::MAX), max_seq).unwrap();
        let first = NodeId::new(0, TypeId(TypeId::MAX), 0).unwrap();
        let previous_type = NodeId::new(0, TypeId(TypeId::MAX - 1), max_seq).unwrap();
        let next_partition = NodeId::new(1, TypeId(TypeId::MAX), 0).unwrap();
        let mut txn = db.begin();
        for id in [last, previous_type, next_partition, first] {
            txn.put(Cf::Node, node_key(id), Vec::new());
        }
        assert_eq!(
            nodes_of_type(&txn, TypeId(TypeId::MAX)).unwrap(),
            vec![first, last]
        );
        assert_eq!(
            nodes_of_type(&txn, TypeId(TypeId::MAX - 1)).unwrap(),
            vec![previous_type]
        );
        assert!(matches!(
            nodes_of_type(&txn, TypeId(TypeId::MAX + 1)),
            Err(GraphError::Id(IdError::TypeOutOfRange))
        ));
    }

    #[test]
    fn nodes_of_type_reports_malformed_node_keys() {
        let (_d, _r, db, _cat) = setup();
        let id = NodeId::new(0, TypeId(7), 1).unwrap();
        let mut malformed = node_key(id);
        malformed.push(0);
        let mut txn = db.begin();
        txn.put(Cf::Node, malformed, Vec::new());
        assert!(matches!(
            nodes_of_type(&txn, TypeId(7)),
            Err(GraphError::Store(StoreError::Corrupt(_)))
        ));
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

    // Fix round 1 (reviewer Critical, on top of this file's original rule 1):
    // the *opposite* commit order from the four tests above — merge_edge
    // reaches commit() FIRST, and a concurrent detach_delete of an endpoint,
    // whose own snapshot predates the merge and so never saw the new edge,
    // commits second. Before `edges_for_update`/`scan_prefix_for_update`,
    // this order left the new edge dangling and check_invariants silent
    // about it (see git history of this file, and task-9-report.md's "Fix
    // round 1" section, for the previous version of this test that pinned
    // that bug as the then-current, true behavior). Now the delete's own
    // range dependency on the node's (Out, In) prefixes catches the write
    // merge_edge lands under them, so it is *delete_txn* — the second
    // committer — that conflicts; k and the edge it never got to touch both
    // survive.

    #[test]
    fn merge_edge_first_then_a_concurrent_detach_delete_of_the_destination_conflicts() {
        let (_d, _r, db, cat) = setup();
        let (c, k) = seed_customer_and_company(&db, &cat);

        let mut merge_txn = db.begin();
        let mut delete_txn = db.begin();
        merge_edge(&mut merge_txn, &cat, c, "WORKS_AT", k, &BTreeMap::new()).unwrap();
        // delete_txn's snapshot predates merge_txn's commit, so its own scan
        // for k's edges (inside detach_delete) finds none yet - and must
        // depend on the range anyway.
        detach_delete(&mut delete_txn, k).unwrap();
        merge_txn.commit().unwrap();
        assert!(matches!(delete_txn.commit(), Err(TxnError::Conflict)));

        let r = db.begin();
        assert!(
            get_node(&r, k).unwrap().is_some(),
            "k must survive: its delete never committed"
        );
        assert_eq!(
            out_edges(&r, c).unwrap().len(),
            1,
            "the edge must survive too"
        );
        check_invariants(&db).unwrap();
    }

    #[test]
    fn merge_edge_first_then_a_concurrent_detach_delete_of_the_source_conflicts() {
        let (_d, _r, db, cat) = setup();
        let (c, k) = seed_customer_and_company(&db, &cat);

        let mut merge_txn = db.begin();
        let mut delete_txn = db.begin();
        merge_edge(&mut merge_txn, &cat, c, "WORKS_AT", k, &BTreeMap::new()).unwrap();
        detach_delete(&mut delete_txn, c).unwrap();
        merge_txn.commit().unwrap();
        assert!(matches!(delete_txn.commit(), Err(TxnError::Conflict)));

        let r = db.begin();
        assert!(get_node(&r, c).unwrap().is_some());
        assert_eq!(in_edges(&r, k).unwrap().len(), 1);
        check_invariants(&db).unwrap();
    }

    // Sanity checks alongside the fix: the new range dependency must not
    // over-constrain ordinary concurrent activity that never touches it.

    #[test]
    fn two_merge_edges_into_the_same_hub_from_different_sources_both_commit() {
        let (_d, _r, db, cat) = setup();
        let mut t = db.begin();
        let c1 = merge_node(&mut t, &cat, "Customer", "c:1", &BTreeMap::new()).unwrap();
        let c2 = merge_node(&mut t, &cat, "Customer", "c:2", &BTreeMap::new()).unwrap();
        let k = merge_node(&mut t, &cat, "Company", "k:1", &BTreeMap::new()).unwrap();
        t.commit().unwrap();

        let mut t1 = db.begin();
        let mut t2 = db.begin();
        merge_edge(&mut t1, &cat, c1, "WORKS_AT", k, &BTreeMap::new()).unwrap();
        merge_edge(&mut t2, &cat, c2, "WORKS_AT", k, &BTreeMap::new()).unwrap();
        t1.commit().unwrap();
        t2.commit().unwrap();

        let r = db.begin();
        assert_eq!(in_edges(&r, k).unwrap().len(), 2);
        check_invariants(&db).unwrap();
    }

    #[test]
    fn detach_delete_of_an_unrelated_node_does_not_conflict_with_a_concurrent_merge_edge() {
        let (_d, _r, db, cat) = setup();
        let mut t = db.begin();
        let c = merge_node(&mut t, &cat, "Customer", "c:1", &BTreeMap::new()).unwrap();
        let k = merge_node(&mut t, &cat, "Company", "k:1", &BTreeMap::new()).unwrap();
        let unrelated = merge_node(&mut t, &cat, "Company", "k:2", &BTreeMap::new()).unwrap();
        t.commit().unwrap();

        let mut merge_txn = db.begin();
        let mut delete_txn = db.begin();
        merge_edge(&mut merge_txn, &cat, c, "WORKS_AT", k, &BTreeMap::new()).unwrap();
        detach_delete(&mut delete_txn, unrelated).unwrap();
        merge_txn.commit().unwrap();
        delete_txn.commit().unwrap();
        check_invariants(&db).unwrap();
    }

    // Fix round 1 (reviewer Important 2a): no existing test ever executes
    // detach_delete's *incoming*-edge loop with a real edge present (the
    // brief's own happy-path test deletes the edge's source, which only
    // exercises the outgoing loop) - emptying that loop's body kept every
    // graph test green. Deleting the destination instead exercises it, and
    // proves the mirrored `out` entry the loop is responsible for
    // (`c`'s edge to `k`) is actually removed too.
    #[test]
    fn detach_delete_of_the_destination_also_removes_the_mirrored_out_edge() {
        let (_d, _r, db, cat) = setup();
        let mut t = db.begin();
        let c = merge_node(&mut t, &cat, "Customer", "c:1", &BTreeMap::new()).unwrap();
        let k = merge_node(&mut t, &cat, "Company", "k:1", &BTreeMap::new()).unwrap();
        merge_edge(&mut t, &cat, c, "WORKS_AT", k, &BTreeMap::new()).unwrap();
        t.commit().unwrap();

        let mut t = db.begin();
        detach_delete(&mut t, k).unwrap();
        t.commit().unwrap();

        let r = db.begin();
        assert!(get_node(&r, k).unwrap().is_none());
        assert!(out_edges(&r, c).unwrap().is_empty());
        check_invariants(&db).unwrap();
    }

    // Fix round 1 (reviewer Important 1): a property that starts out unknown
    // to the catalog lands in `row.extra`; once a later `apply_ontology`
    // promotes its name to a real column, a subsequent SET (including SET
    // NULL) must stop leaving the old value behind under its old, now-stale
    // home. Before the fix, step 2 below left "Al" sitting in `extra` forever.
    fn mapping_with_customer_properties(properties: &[&str]) -> GraphMapping {
        GraphMapping {
            version: "1".into(),
            nodes: vec![NodeSpec {
                label: "Customer".into(),
                properties: properties.iter().map(|s| s.to_string()).collect(),
                er_fields: vec![],
                indexed_fields: vec![],
                identifier_columns: BTreeMap::new(),
                display_color: None,
            }],
            edges: vec![],
            timeline: vec![],
        }
    }

    #[test]
    fn promoting_a_property_to_a_column_clears_the_stale_extra_value() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        let db = Database::open(dir.path(), &res).unwrap();

        // v1: "nickname" is not a column, so it lands in extra.
        let v1 = db
            .apply_ontology(&mapping_with_customer_properties(&["name"]))
            .unwrap();
        let mut t = db.begin();
        let id = merge_node(
            &mut t,
            &v1,
            "Customer",
            "c:1",
            &props(&[("nickname", Value::Str("Al".into()))]),
        )
        .unwrap();
        t.commit().unwrap();
        let node = get_node(&db.begin(), id).unwrap().unwrap();
        assert_eq!(
            node.row.extra.get("nickname"),
            Some(&Value::Str("Al".into()))
        );

        // v2: "nickname" is promoted to a real column.
        let v2 = db
            .apply_ontology(&mapping_with_customer_properties(&["name", "nickname"]))
            .unwrap();
        let nickname_col = v2
            .node_type("Customer")
            .unwrap()
            .column("nickname")
            .unwrap();

        // SET NULL through the now-promoted name must clear the stale extra
        // value, not just no-op on a column that was never set.
        let mut t = db.begin();
        merge_node(
            &mut t,
            &v2,
            "Customer",
            "c:1",
            &props(&[("nickname", Value::Null)]),
        )
        .unwrap();
        t.commit().unwrap();
        let node = get_node(&db.begin(), id).unwrap().unwrap();
        assert!(!node.row.columns.contains_key(&nickname_col));
        assert!(
            !node.row.extra.contains_key("nickname"),
            "stale extra value must be cleared once its name is promoted to a column"
        );

        // A real SET afterward must not leave both a column value and a
        // stale extra value for the same logical property.
        let mut t = db.begin();
        merge_node(
            &mut t,
            &v2,
            "Customer",
            "c:1",
            &props(&[("nickname", Value::Str("Bob".into()))]),
        )
        .unwrap();
        t.commit().unwrap();
        let node = get_node(&db.begin(), id).unwrap().unwrap();
        assert_eq!(
            node.row.columns.get(&nickname_col),
            Some(&Value::Str("Bob".into()))
        );
        assert!(!node.row.extra.contains_key("nickname"));
    }

    #[test]
    fn non_null_set_after_promotion_removes_the_old_extra_value_directly() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        let db = Database::open(dir.path(), &res).unwrap();
        let v1 = db
            .apply_ontology(&mapping_with_customer_properties(&["name"]))
            .unwrap();
        let mut seed = db.begin();
        let id = merge_node(
            &mut seed,
            &v1,
            "Customer",
            "c:1",
            &props(&[("nickname", Value::Str("Al".into()))]),
        )
        .unwrap();
        seed.commit().unwrap();
        let v2 = db
            .apply_ontology(&mapping_with_customer_properties(&["name", "nickname"]))
            .unwrap();
        let nickname_col = v2
            .node_type("Customer")
            .unwrap()
            .column("nickname")
            .unwrap();

        let mut update = db.begin();
        merge_node(
            &mut update,
            &v2,
            "Customer",
            "c:1",
            &props(&[("nickname", Value::Str("Bob".into()))]),
        )
        .unwrap();
        update.commit().unwrap();

        let node = get_node(&db.begin(), id).unwrap().unwrap();
        assert_eq!(
            node.row.columns.get(&nickname_col),
            Some(&Value::Str("Bob".into()))
        );
        assert!(!node.row.extra.contains_key("nickname"));
    }

    // Fix round 1 (reviewer Important 2b): check_invariants had no test for
    // any of its own negative paths - a body of `Ok(())` would have passed
    // every other test in this file. One test per invariant it checks,
    // written by corrupting state directly through a Txn with raw keys
    // (never through the public graph API, which cannot produce these
    // states on its own).

    #[test]
    fn check_invariants_catches_a_pk_pointing_to_a_missing_node() {
        let (_d, _r, db, cat) = setup();
        let customer = cat.node_type("Customer").unwrap();
        let ghost = NodeId::new(0, customer.id, 777).unwrap();
        let mut t = db.begin();
        t.put(
            Cf::Pk,
            pk_key("ghost:1", customer.id),
            ghost.0.to_be_bytes().to_vec(),
        );
        t.commit().unwrap();
        let err = check_invariants(&db).unwrap_err();
        assert!(err.contains("missing node"), "unexpected message: {err}");
    }

    #[test]
    fn check_invariants_catches_a_missing_edge_mirror() {
        let (_d, _r, db, cat) = setup();
        let (c, k) = seed_customer_and_company(&db, &cat);
        let et = cat.edge_type("WORKS_AT").unwrap();
        let mut t = db.begin();
        // Write only the `out` side directly: merge_edge would also write
        // the `in` mirror, but this bypasses it.
        t.put(
            Cf::Out,
            edge_key(c, et.id, k, EdgeId::MERGED),
            encode_row(&Row::default()),
        );
        t.commit().unwrap();
        let err = check_invariants(&db).unwrap_err();
        assert!(err.contains("no mirror"), "unexpected message: {err}");
    }

    #[test]
    fn check_invariants_catches_a_node_above_its_id_counter() {
        let (_d, _r, db, cat) = setup();
        let mut t = db.begin();
        let c = merge_node(&mut t, &cat, "Customer", "c:1", &BTreeMap::new()).unwrap();
        t.commit().unwrap();
        // Corrupt the counter to sit at c's own sequence instead of past it.
        let mut t = db.begin();
        t.put(
            Cf::Meta,
            crate::alloc::node_counter_key(c.type_id()),
            c.seq().to_be_bytes().to_vec(),
        );
        t.commit().unwrap();
        let err = check_invariants(&db).unwrap_err();
        assert!(
            err.contains("above its counter"),
            "unexpected message: {err}"
        );
    }

    // Added beyond the reviewer's exact list: check_invariants' *new*
    // endpoint-existence check (added in this same fix round, see above)
    // is itself new logic and deserves the same treatment as the three
    // pre-existing invariants above, rather than shipping unverified.
    #[test]
    fn check_invariants_catches_an_edge_with_a_missing_endpoint() {
        let (_d, _r, db, cat) = setup();
        let mut t = db.begin();
        let c = merge_node(&mut t, &cat, "Customer", "c:1", &BTreeMap::new()).unwrap();
        t.commit().unwrap();
        let et = cat.edge_type("WORKS_AT").unwrap();
        // Write both sides of an edge directly, but to a target that has no
        // `node` entry at all - merge_edge's own existence check would
        // refuse this; only a raw write can produce it.
        let company_ty = cat.node_type("Company").unwrap().id;
        let ghost = NodeId::new(0, company_ty, 555_555).unwrap();
        let mut t = db.begin();
        t.put(
            Cf::Out,
            edge_key(c, et.id, ghost, EdgeId::MERGED),
            encode_row(&Row::default()),
        );
        t.put(
            Cf::In,
            edge_key(ghost, et.id, c, EdgeId::MERGED),
            Vec::new(),
        );
        t.commit().unwrap();
        let err = check_invariants(&db).unwrap_err();
        assert!(
            err.contains("missing endpoint"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn stats_counts_nodes_and_outgoing_edges() {
        let (_d, _r, db, cat) = setup();
        assert_eq!(db.stats().unwrap(), GraphStats { nodes: 0, edges: 0 });
        let (customer, company) = seed_customer_and_company(&db, &cat);
        let mut txn = db.begin();
        merge_edge(
            &mut txn,
            &cat,
            customer,
            "WORKS_AT",
            company,
            &BTreeMap::new(),
        )
        .unwrap();
        txn.commit().unwrap();
        assert_eq!(db.stats().unwrap(), GraphStats { nodes: 2, edges: 1 });
    }

    #[test]
    fn sparse_projection_maps_persisted_edges_to_csr_rows() {
        let (_d, _r, db, cat) = setup();
        let (customer, company) = seed_customer_and_company(&db, &cat);
        let mut txn = db.begin();
        merge_edge(
            &mut txn,
            &cat,
            customer,
            "WORKS_AT",
            company,
            &BTreeMap::new(),
        )
        .unwrap();
        txn.commit().unwrap();

        let projection = db.sparse_projection(None).unwrap();
        assert_eq!(projection.nodes, vec![customer, company]);
        assert_eq!(projection.matrix.non_zero_count(), 1);
        assert_eq!(projection.matrix.matvec(&[0.0, 2.0]).unwrap(), [2.0, 0.0]);
        assert_eq!(projection.pagerank(10, 0.85).unwrap().len(), 2);
        let works_at = cat.edge_type("WORKS_AT").unwrap().id;
        assert_eq!(
            db.sparse_projection(Some(works_at))
                .unwrap()
                .matrix
                .non_zero_count(),
            1
        );
    }

    #[test]
    fn projection_rejects_corrupt_keys_and_missing_endpoints() {
        let (_d, _r, db, cat) = setup();
        let (customer, company) = seed_customer_and_company(&db, &cat);
        let edge_type = cat.edge_type("WORKS_AT").unwrap().id;
        for key in [
            vec![0; 25],
            vec![0; 27],
            edge_key(customer, edge_type, NodeId(999), EdgeId::MERGED),
        ] {
            let mut tx = db.begin();
            tx.put(Cf::Out, key, encode_row(&Row::default()));
            assert!(sparse_projection(&tx, &ProjectionOptions::default()).is_err());
        }
        let mut tx = db.begin();
        tx.put(Cf::Node, vec![0; 7], vec![]);
        assert!(sparse_projection(&tx, &ProjectionOptions::default()).is_err());
        drop(tx);
        let mut tx = db.begin();
        merge_edge(
            &mut tx,
            &cat,
            customer,
            "WORKS_AT",
            company,
            &BTreeMap::new(),
        )
        .unwrap();
        let projection = sparse_projection(&tx, &ProjectionOptions::default()).unwrap();
        assert_eq!(projection.relationship_count, 1);
        assert_eq!(db.sparse_projection(None).unwrap().relationship_count, 0);
        assert!(matches!(
            sparse_projection(
                &tx,
                &ProjectionOptions {
                    max_nodes: 1,
                    ..ProjectionOptions::default()
                }
            ),
            Err(ProjectionError::Limit { .. })
        ));
    }

    #[test]
    fn projection_filters_types_and_aggregates_parallel_coordinates() {
        let (_d, _r, db, cat) = setup();
        let (customer, company) = seed_customer_and_company(&db, &cat);
        let works_at = cat.edge_type("WORKS_AT").unwrap().id;
        let mut tx = db.begin();
        merge_edge(
            &mut tx,
            &cat,
            customer,
            "WORKS_AT",
            company,
            &BTreeMap::new(),
        )
        .unwrap();
        // Exercise a distinct edge ID at the same source/type/target.
        tx.put(
            Cf::Out,
            edge_key(customer, works_at, company, EdgeId(42)),
            encode_row(&Row::default()),
        );
        let projection = sparse_projection(&tx, &ProjectionOptions::default()).unwrap();
        assert_eq!(projection.relationship_count, 2);
        assert_eq!(projection.matrix.non_zero_count(), 1);
        assert_eq!(projection.matrix.matvec(&[0.0, 3.0]).unwrap(), [6.0, 0.0]);
        assert_eq!(projection.out_degrees, [2, 0]);
        assert_eq!(projection.in_degrees, [0, 2]);
        let options = ProjectionOptions {
            relationship_type: Some(cat.edge_type("IN_SEGMENT").unwrap().id),
            ..ProjectionOptions::default()
        };
        let filtered = sparse_projection(&tx, &options).unwrap();
        assert_eq!(filtered.relationship_count, 0);
        assert_eq!(filtered.scanned_relationship_count, 2);
        assert_eq!(filtered.nodes.len(), 2);
        assert!(sparse_projection(
            &tx,
            &ProjectionOptions {
                max_relationships: 1,
                ..options
            }
        )
        .is_err());
    }
}
