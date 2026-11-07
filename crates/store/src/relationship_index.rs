//! Secondary relationship properties stored separately from node indexes.
use brainwire_core::codec::{decode_row_field, estimate_row_field_allocation, Row};
use brainwire_core::ids::{EdgeId, EdgeTypeId, NodeId};
use brainwire_core::keys::{get_bytes, get_u16, put_bytes, put_u16};
use brainwire_core::schema::{
    Catalog, EdgeType, RelationshipIndexState, RelationshipPropertyIndex,
};
use brainwire_core::value::Value;

use crate::catalog::{self, job_key, take_keys};
use crate::db::StoreError;
use crate::graph::{decode_edge_key, index_value, row_property, GraphError};
use crate::txn::{Cf, Txn};

pub(crate) const BUILD_JOB: &[u8] = b"relationship-index-build/";
pub(crate) const DROP_JOB: &[u8] = b"relationship-index-drop/";
// Index keys are retained in the write overlay and conflict history.
const MAX_VALUE_BYTES: usize = 64 * 1024;
const MAX_BATCH_BYTES: usize = 4 * 1024 * 1024;
const EDGE_KEY_BYTES: usize = 26;

fn prefix(ty: EdgeTypeId, property: &str) -> Vec<u8> {
    let mut key = Vec::new();
    put_u16(&mut key, ty.0);
    put_bytes(&mut key, property.as_bytes());
    key
}

// Truncating to a common byte prefix is monotone, so inclusive range bounds
// still select a superset. The executor rechecks every candidate exactly.
fn encoded_value(value: &Value) -> Option<Vec<u8>> {
    match value {
        Value::Str(text) => {
            let mut encoded = vec![b's'];
            put_bytes(
                &mut encoded,
                &text.as_bytes()[..text.len().min(MAX_VALUE_BYTES)],
            );
            Some(encoded)
        }
        Value::Int(number) => numeric_key(*number as f64),
        Value::Float(number) => numeric_key(*number),
        _ => index_value(value),
    }
}

// Converting integer bounds to f64 is monotone. Distinct large integers may
// share a key, so only inclusive byte bounds are safe. Residual Cypher
// comparisons remain exact.
fn numeric_key(number: f64) -> Option<Vec<u8>> {
    if number.is_nan() {
        return None;
    }
    let bits = if number == 0.0 { 0 } else { number.to_bits() };
    let sign = 1 << 63;
    let ordered = if bits & sign == 0 { bits ^ sign } else { !bits };
    Some([b"n".as_slice(), &ordered.to_be_bytes()].concat())
}

fn property_key(prefix: &[u8], value: &Value, edge: &[u8]) -> Option<Vec<u8>> {
    encoded_value(value).map(|encoded| [prefix, b"f", &encoded, edge].concat())
}

fn definition_key(ty: EdgeTypeId) -> Vec<u8> {
    [b"relationship-index-def/".as_slice(), &ty.0.to_be_bytes()].concat()
}

fn active_fields(ty: &EdgeType) -> Vec<u8> {
    let mut fields = Vec::new();
    for index in &ty.property_indexes {
        if index.state != RelationshipIndexState::Failed {
            put_bytes(&mut fields, index.property.as_bytes());
        }
    }
    fields
}

fn publish_definition(txn: &mut Txn, ty: &EdgeType) {
    let key = definition_key(ty.id);
    let fields = active_fields(ty);
    if fields.is_empty() {
        txn.delete(Cf::Meta, key);
    } else {
        txn.put(Cf::Meta, key, fields);
    }
}

pub(crate) fn track_definition(txn: &mut Txn, ty: &EdgeType) -> Result<(), StoreError> {
    let key = definition_key(ty.id);
    txn.depend(Cf::Meta, &key);
    txn.with_get(Cf::Meta, &key, |actual| {
        if actual.unwrap_or_default() != active_fields(ty) {
            return Err(StoreError::Catalog(
                "relationship index definition changed; reload catalog".into(),
            ));
        }
        Ok(())
    })
}

pub fn create(
    txn: &mut Txn,
    relationship: &str,
    property: &str,
    name: Option<&str>,
    if_not_exists: bool,
) -> Result<(), GraphError> {
    if !txn.has_relationship_index_format() {
        return Err(GraphError::Catalog(
            "enable relationship index storage before declaring an index".into(),
        ));
    }
    if property.to_ascii_lowercase().starts_with("acl_") {
        return Err(GraphError::ReservedProperty(property.into()));
    }
    if property.is_empty() || property.len() > 128 {
        return Err(GraphError::Catalog(
            "relationship index property must have 1..128 bytes".into(),
        ));
    }
    let mut catalog = catalog::load(txn)?;
    let ty = catalog
        .edge_type(relationship)
        .ok_or_else(|| GraphError::UnknownEdgeType(relationship.into()))?;
    let name = name.map_or_else(
        || format!("index_rel_{relationship}_{property}"),
        str::to_string,
    );
    if name.is_empty() {
        return Err(GraphError::Catalog("index name must not be empty".into()));
    }
    if catalog::index_name_exists(&catalog, &name)
        || ty
            .property_indexes
            .iter()
            .any(|index| index.property == property)
    {
        return if if_not_exists {
            Ok(())
        } else {
            Err(GraphError::Catalog(
                "relationship index already exists".into(),
            ))
        };
    }
    if ty.property_indexes.len() >= 8 {
        return Err(GraphError::Catalog(
            "at most 8 property indexes per relationship type".into(),
        ));
    }
    let ty_id = ty.id;
    let prefix = prefix(ty_id, property);
    if txn.get(Cf::Meta, &job_key(DROP_JOB, &prefix))?.is_some() {
        return Err(GraphError::Catalog(
            "dropped relationship index is still being removed".into(),
        ));
    }
    let ty = catalog
        .edge_types
        .iter_mut()
        .find(|ty| ty.id == ty_id)
        .expect("validated type");
    ty.property_indexes.push(RelationshipPropertyIndex {
        property: property.into(),
        name,
        state: RelationshipIndexState::Populating,
    });
    publish_definition(txn, ty);
    catalog::store_next(txn, &mut catalog)?;
    txn.put(Cf::Meta, job_key(BUILD_JOB, &prefix), Vec::new());
    Ok(())
}

pub(crate) fn drop(txn: &mut Txn, catalog: &mut Catalog, name: &str) -> Result<bool, GraphError> {
    let Some((ty, index)) = catalog.edge_types.iter_mut().find_map(|ty| {
        let index = ty
            .property_indexes
            .iter()
            .position(|index| index.name == name)?;
        Some((ty, index))
    }) else {
        return Ok(false);
    };
    let prefix = prefix(ty.id, &ty.property_indexes.remove(index).property);
    publish_definition(txn, ty);
    catalog::store_next(txn, catalog)?;
    txn.delete(Cf::Meta, job_key(BUILD_JOB, &prefix));
    txn.put(Cf::Meta, job_key(DROP_JOB, &prefix), Vec::new());
    Ok(true)
}

pub(crate) fn step(txn: &mut Txn, job: &[u8], batch: usize) -> Result<(), GraphError> {
    let cursor = txn.get_for_update(Cf::Meta, job)?.unwrap_or_default();
    if job.starts_with(DROP_JOB) {
        let prefix = &job[DROP_JOB.len()..];
        let keys = take_keys(
            batch.min(MAX_BATCH_BYTES / (2 * MAX_VALUE_BYTES + 1024)),
            |visit| txn.visit_prefix(Cf::RelationshipIdx, prefix, |key, _| visit(key)),
        )?;
        if keys.is_empty() {
            txn.delete(Cf::Meta, job.to_vec());
        }
        for key in keys {
            txn.delete(Cf::RelationshipIdx, key);
        }
        return Ok(());
    }
    let index_prefix = &job[BUILD_JOB.len()..];
    let mut input = index_prefix;
    let ty_id = EdgeTypeId(get_u16(&mut input).map_err(StoreError::from)?);
    let property = String::from_utf8(get_bytes(&mut input).map_err(StoreError::from)?)
        .map_err(|_| StoreError::Corrupt("invalid relationship index property".into()))?;
    let mut catalog = catalog::load(txn)?;
    let Some(ty) = catalog.edge_types.iter_mut().find(|ty| ty.id == ty_id) else {
        return Err(StoreError::Corrupt("relationship index job without type".into()).into());
    };
    let Some(index) = ty.property_indexes.iter().position(|index| {
        index.property == property && index.state == RelationshipIndexState::Populating
    }) else {
        return Err(StoreError::Corrupt(
            "relationship index job without pending declaration".into(),
        )
        .into());
    };
    if !cursor.is_empty() && cursor.len() != EDGE_KEY_BYTES {
        return Err(StoreError::Corrupt("invalid relationship index cursor".into()).into());
    }
    let mut start = cursor;
    if !start.is_empty() {
        start.push(0); // fixed-length edge keys, strictly after the last one
    }
    let keys = take_keys(batch, |visit| {
        txn.visit_range(Cf::Out, &start, &[0xff; EDGE_KEY_BYTES + 1], |key, _| {
            visit(key)
        })
    })?;
    let column = ty.columns.iter().find(|c| c.name == property).map(|c| c.id);
    let mut last = None;
    let mut bytes = 0usize;
    for key in &keys {
        if decode_edge_key(key)?.1 == ty_id {
            let entry = txn.with_get(Cf::Out, key, |raw| -> Result<_, GraphError> {
                let raw = raw.ok_or_else(|| {
                    StoreError::Corrupt("missing relationship index source".into())
                })?;
                if estimate_row_field_allocation(raw, column, &property)
                    .map_err(StoreError::from)?
                    > Some(MAX_BATCH_BYTES)
                {
                    return Ok((false, None));
                }
                let value = decode_row_field(raw, column, &property).map_err(StoreError::from)?;
                Ok((
                    true,
                    value
                        .as_ref()
                        .and_then(|value| property_key(index_prefix, value, key)),
                ))
            })?;
            if !entry.0 {
                ty.property_indexes[index].state = RelationshipIndexState::Failed;
                publish_definition(txn, ty);
                catalog::store(txn, &catalog)?;
                txn.delete(Cf::Meta, job.to_vec());
                return Ok(());
            }
            if let Some(entry) = entry.1 {
                let retained = 2 * entry.len() + 1024;
                if bytes + retained > MAX_BATCH_BYTES {
                    break;
                }
                bytes += retained;
                txn.depend(Cf::Out, key);
                insert(txn, reverse_key(index_prefix, key), entry);
            }
        }
        last = Some(key);
    }
    if keys.len() == batch || last != keys.last() {
        txn.put(
            Cf::Meta,
            job.to_vec(),
            last.expect("first entry fits").clone(),
        );
    } else {
        ty.property_indexes[index].state = RelationshipIndexState::Ready;
        catalog::store(txn, &catalog)?;
        txn.delete(Cf::Meta, job.to_vec());
    }
    Ok(())
}

fn reverse_key(prefix: &[u8], edge: &[u8]) -> Vec<u8> {
    [prefix, b"r", edge].concat()
}

fn insert(txn: &mut Txn, reverse: Vec<u8>, forward: Vec<u8>) {
    txn.put(Cf::RelationshipIdx, forward.clone(), Vec::new());
    txn.put(Cf::RelationshipIdx, reverse, forward);
}

fn verify_forward(prefix: &[u8], edge: &[u8], forward: &[u8]) -> Result<(), StoreError> {
    if forward
        .strip_prefix(prefix)
        .is_none_or(|tail| !tail.starts_with(b"f") || !tail.ends_with(edge))
    {
        return Err(StoreError::Corrupt(
            "invalid relationship index reverse entry".into(),
        ));
    }
    Ok(())
}

pub(crate) fn update(
    txn: &mut Txn,
    ty: &EdgeType,
    edge: &[u8],
    row: &Row,
) -> Result<(), GraphError> {
    let mut changes = Vec::new();
    for index in &ty.property_indexes {
        if index.state == RelationshipIndexState::Failed {
            continue;
        }
        let prefix = prefix(ty.id, &index.property);
        let reverse = reverse_key(&prefix, edge);
        let before = txn.get_for_update(Cf::RelationshipIdx, &reverse)?;
        if let Some(before) = &before {
            verify_forward(&prefix, edge, before)?;
        }
        let after = row_property(row, ty, &index.property)
            .and_then(|value| property_key(&prefix, value, edge));
        if before != after {
            changes.push((reverse, before, after));
        }
    }
    for (reverse, before, after) in changes {
        if let Some(before) = before {
            txn.delete(Cf::RelationshipIdx, before);
        }
        if let Some(after) = after {
            insert(txn, reverse, after);
        } else {
            txn.delete(Cf::RelationshipIdx, reverse);
        }
    }
    Ok(())
}

/// Collect fully admitted tombstones before modifying adjacency or index data.
pub(crate) fn removal_keys<E: From<StoreError>>(
    txn: &mut Txn,
    ty: &EdgeType,
    edge: &[u8],
    mut control: impl FnMut(usize) -> Result<(), E>,
    keys: &mut Vec<Vec<u8>>,
) -> Result<(), E> {
    for index in &ty.property_indexes {
        control(256 + index.property.len().saturating_mul(4))?;
        let prefix = prefix(ty.id, &index.property);
        let reverse = reverse_key(&prefix, edge);
        txn.depend(Cf::RelationshipIdx, &reverse);
        let forward = txn.with_get::<E, _>(Cf::RelationshipIdx, &reverse, |forward| {
            if let Some(forward) = forward {
                verify_forward(&prefix, edge, forward)?;
                control(forward.len() + 512)?;
            }
            Ok(forward.map(<[u8]>::to_vec))
        })?;
        if let Some(forward) = forward {
            keys.push(forward);
            keys.push(reverse);
        }
    }
    Ok(())
}

pub(crate) fn check(txn: &Txn, catalog: &Catalog) -> Result<(), StoreError> {
    if !txn.has_relationship_index_format() {
        if catalog
            .edge_types
            .iter()
            .any(|ty| !ty.property_indexes.is_empty())
        {
            return Err(StoreError::Corrupt(
                "relationship index catalog without storage".into(),
            ));
        }
        return Ok(());
    }
    for ty in &catalog.edge_types {
        let definition = txn.get(Cf::Meta, &definition_key(ty.id))?;
        if definition.as_deref().unwrap_or_default() != active_fields(ty) {
            return Err(StoreError::Corrupt(
                "relationship index definition mismatch".into(),
            ));
        }
        for index in &ty.property_indexes {
            let prefix = prefix(ty.id, &index.property);
            for kind in *b"fr" {
                let scan_prefix = [prefix.as_slice(), &[kind]].concat();
                txn.visit_prefix(Cf::RelationshipIdx, &scan_prefix, |key, value| {
                    let edge = &key[key.len().saturating_sub(EDGE_KEY_BYTES)..];
                    if decode_edge_key(edge)?.1 != ty.id {
                        return Err(StoreError::Corrupt(
                            "relationship index type mismatch".into(),
                        ));
                    }
                    let reverse = reverse_key(&prefix, edge);
                    let forward = if kind == b'r' { value } else { key };
                    verify_forward(&prefix, edge, forward)?;
                    if txn.get(Cf::RelationshipIdx, &reverse)?.as_deref() != Some(forward)
                        || txn.get(Cf::RelationshipIdx, forward)?.as_deref() != Some(&[])
                    {
                        return Err(StoreError::Corrupt(
                            "relationship index mirror missing".into(),
                        ));
                    }
                    let column = ty
                        .columns
                        .iter()
                        .find(|c| c.name == index.property)
                        .map(|c| c.id);
                    txn.with_get(Cf::Out, edge, |raw| {
                        let raw = raw.ok_or_else(|| {
                            StoreError::Corrupt("relationship index points to missing edge".into())
                        })?;
                        if index.state != RelationshipIndexState::Failed {
                            let expected = decode_row_field(raw, column, &index.property)?
                                .as_ref()
                                .and_then(|value| property_key(&prefix, value, edge));
                            if expected.as_deref() != Some(forward) {
                                return Err(StoreError::Corrupt(
                                    "relationship index value mismatch".into(),
                                ));
                            }
                        }
                        Ok(())
                    })
                })?;
            }
        }
    }
    let ready: std::collections::BTreeMap<_, Vec<_>> = catalog
        .edge_types
        .iter()
        .filter_map(|ty| {
            let fields: Vec<_> = ty
                .property_indexes
                .iter()
                .filter(|index| index.state == RelationshipIndexState::Ready)
                .map(|index| {
                    let column = ty
                        .columns
                        .iter()
                        .find(|c| c.name == index.property)
                        .map(|c| c.id);
                    (
                        prefix(ty.id, &index.property),
                        column,
                        index.property.as_str(),
                    )
                })
                .collect();
            (!fields.is_empty()).then_some((ty.id, fields))
        })
        .collect();
    if !ready.is_empty() {
        txn.visit_prefix(Cf::Out, &[], |edge, raw| {
            if let Some(fields) = ready.get(&decode_edge_key(edge)?.1) {
                for (prefix, column, field) in fields {
                    if let Some(expected) = decode_row_field(raw, *column, field)?
                        .as_ref()
                        .and_then(|value| property_key(prefix, value, edge))
                    {
                        if txn.get(Cf::RelationshipIdx, &expected)?.as_deref() != Some(&[]) {
                            return Err(StoreError::Corrupt(
                                "ready relationship index omits edge".into(),
                            ));
                        }
                    }
                }
            }
            Ok(())
        })?;
    }
    Ok(())
}

pub type PhysicalRelationship = (NodeId, EdgeTypeId, NodeId, EdgeId);

/// Seek a ready equality index for one directed endpoint pair and track phantoms.
/// The caller validates the current catalog and rechecks the complete merge key.
#[allow(clippy::too_many_arguments)]
pub(crate) fn visit_equality_pair_for_update<E: From<GraphError> + From<StoreError>>(
    txn: &mut Txn,
    ty: &EdgeType,
    field: &str,
    value: &Value,
    source: NodeId,
    target: NodeId,
    mut callback: impl FnMut(&[u8], &[u8]) -> Result<(), E>,
) -> Result<bool, E> {
    if !ty
        .property_indexes
        .iter()
        .any(|index| index.property == field && index.state == RelationshipIndexState::Ready)
    {
        return Ok(false);
    }
    let Some(value) = encoded_value(value) else {
        return Ok(false);
    };
    track_definition(txn, ty)?;
    let edge = crate::graph::edge_key(source, ty.id, target, EdgeId::MERGED);
    let pair = &edge[..EDGE_KEY_BYTES - 8];
    let prefix = [prefix(ty.id, field).as_slice(), b"f", &value, pair].concat();
    txn.depend_prefix(Cf::RelationshipIdx, &prefix);
    txn.visit_prefix(Cf::RelationshipIdx, &prefix, |key, _| {
        if key.len() != prefix.len() + 8 {
            return Err(StoreError::Corrupt("invalid relationship pair index key".into()).into());
        }
        let physical = &key[key.len() - EDGE_KEY_BYTES..];
        txn.with_get(Cf::Out, physical, |raw| {
            let raw = raw.ok_or_else(|| {
                StoreError::Corrupt("relationship index points to missing edge".into())
            })?;
            callback(physical, raw)
        })
    })?;
    Ok(true)
}

/// Visit a conservative candidate set for a ready relationship index.
/// The caller must recheck the predicate and endpoint visibility.
pub fn visit<E: From<GraphError> + From<StoreError>>(
    txn: &Txn,
    ty: &EdgeType,
    field: &str,
    equality: Option<&Value>,
    lower: Option<&Value>,
    upper: Option<&Value>,
    mut callback: impl FnMut(PhysicalRelationship) -> Result<(), E>,
) -> Result<bool, E> {
    if !ty
        .property_indexes
        .iter()
        .any(|index| index.property == field && index.state == RelationshipIndexState::Ready)
    {
        return Ok(false);
    }
    let mut prefix = prefix(ty.id, field);
    prefix.push(b'f');
    let candidate = |key: &[u8]| -> Result<PhysicalRelationship, StoreError> {
        let edge = key
            .get(key.len().saturating_sub(EDGE_KEY_BYTES)..)
            .ok_or_else(|| StoreError::Corrupt("invalid relationship index entry".into()))?;
        if key.len() <= prefix.len() + EDGE_KEY_BYTES {
            return Err(StoreError::Corrupt(
                "invalid relationship index key length".into(),
            ));
        }
        decode_edge_key(edge)
    };
    if let Some(value) = equality {
        let Some(encoded) = encoded_value(value) else {
            return Ok(false);
        };
        txn.visit_prefix(
            Cf::RelationshipIdx,
            &[prefix.as_slice(), &encoded].concat(),
            |key, _| callback(candidate(key)?),
        )?;
        return Ok(true);
    }
    let range_value = |value: &Value| match value {
        Value::Str(_) | Value::Int(_) | Value::Float(_) => encoded_value(value),
        _ => None,
    };
    let lo = lower.and_then(range_value);
    let hi = upper.and_then(range_value);
    if (lower.is_some() && lo.is_none()) || (upper.is_some() && hi.is_none()) {
        return Ok(false);
    }
    let Some(kind) = lo.as_ref().or(hi.as_ref()).map(|key| key[0]) else {
        return Ok(false);
    };
    if hi.as_ref().is_some_and(|key| key[0] != kind) {
        return Ok(false);
    }
    let lo = match lo {
        Some(key) => [prefix.as_slice(), &key].concat(),
        None => [prefix.as_slice(), &[kind]].concat(),
    };
    let hi = match hi {
        Some(key) => [prefix.as_slice(), &key, &[0xff; EDGE_KEY_BYTES + 1]].concat(),
        None => [prefix.as_slice(), &[kind + 1]].concat(),
    };
    txn.visit_range(Cf::RelationshipIdx, &lo, &hi, |key, _| {
        callback(candidate(key)?)
    })?;
    Ok(true)
}
