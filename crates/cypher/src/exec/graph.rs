//! Node and relationship access for the executor: visibility, labels,
//! single-property reads, full property maps and output conversion.
use super::val::Val;
use super::*;
use brainwire_store::graph::*;

pub(super) fn acl_property(name: &str) -> bool {
    name.get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("acl_"))
}

pub(super) fn visible(ctx: &Context<'_, '_>, id: NodeId) -> Result<bool, ExecError> {
    let _scope = ctx.budget.scope();
    match ctx.visibility {
        None => Ok(true),
        Some(policy) => {
            node_visible_with_control::<ExecError>(ctx.txn, ctx.catalog, id, policy, |bytes| {
                ctx.budget.charge(bytes)
            })
        }
    }
}

pub(super) fn node_type<'a>(
    ctx: &'a Context<'_, '_>,
    id: NodeId,
) -> Result<&'a brainwire_core::schema::NodeType, ExecError> {
    ctx.catalog
        .node_types
        .iter()
        .find(|ty| ty.id == id.type_id())
        .ok_or_else(|| ExecError::Semantic("node type absent from catalog".into()))
}

pub(super) fn label_of<'a>(ctx: &'a Context<'_, '_>, id: NodeId) -> Result<&'a str, ExecError> {
    Ok(&node_type(ctx, id)?.label)
}

pub(super) fn type_of<'a>(
    ctx: &'a Context<'_, '_>,
    rel: &Relationship,
) -> Result<&'a str, ExecError> {
    ctx.catalog
        .edge_types
        .iter()
        .find(|ty| ty.id == rel.edge_type)
        .map(|ty| ty.name.as_str())
        .ok_or_else(|| ExecError::Semantic("relationship type absent from catalog".into()))
}

fn node_record(ctx: &Context<'_, '_>, id: NodeId) -> Result<NodeRecord, ExecError> {
    if let Some(policy) = ctx.visibility {
        let (result, node_charge) = {
            let _visibility_scope = ctx.budget.scope();
            let result = get_node_with_visibility_and_control::<ExecError>(
                ctx.txn,
                ctx.catalog,
                id,
                policy,
                |bytes| ctx.budget.charge(bytes),
            )?;
            let charge = result.as_ref().map_or(0, |(_, _, charge)| *charge);
            (result, charge)
        };
        return match result {
            Some((record, true, _)) => {
                ctx.budget.charge(node_charge)?;
                Ok(record)
            }
            _ => Err(ExecError::Forbidden("node is not readable")),
        };
    }
    Ok(
        get_node_with_control::<ExecError>(ctx.txn, id, |bytes| ctx.budget.charge(bytes))?
            .ok_or(GraphError::NodeNotFound(id))?,
    )
}

pub(super) fn node_out(ctx: &Context<'_, '_>, id: NodeId) -> Result<NodeOut, ExecError> {
    let mut record = node_record(ctx, id)?;
    let ty = node_type(ctx, id)?;
    let columns = ty
        .columns
        .iter()
        .filter(|column| record.row.columns.contains_key(&column.id));
    let (count, names) = columns.fold((1usize, "entity_id".len()), |(count, names), column| {
        (
            count.saturating_add(1),
            names.saturating_add(column.name.len()),
        )
    });
    ctx.budget.charge(
        count
            .saturating_add(record.row.extra.len())
            .saturating_mul(128)
            .saturating_add(names)
            .saturating_add(ty.label.len()),
    )?;
    let mut props = BTreeMap::new();
    for column in &ty.columns {
        if let Some(value) = record.row.columns.remove(&column.id) {
            props.insert(column.name.clone(), value);
        }
    }
    props.extend(record.row.extra);
    props.retain(|name, _| !acl_property(name));
    props.insert("entity_id".into(), Value::Str(record.entity_id));
    Ok(NodeOut {
        id,
        label: ty.label.clone(),
        props,
    })
}

/// One property, decoded on its own. Reserved `acl_*` names read as null.
pub(super) fn node_property(
    ctx: &Context<'_, '_>,
    id: NodeId,
    name: &str,
) -> Result<Value, ExecError> {
    if acl_property(name) {
        return Ok(Value::Null);
    }
    if ctx.visibility.is_some() && !visible(ctx, id)? {
        return Err(ExecError::Forbidden("node is not readable"));
    }
    let column = node_type(ctx, id)?.column(name);
    if let Some(cache) = ctx.cache {
        if let Some((cached, bytes)) = cache.0.borrow().as_ref() {
            if *cached == id {
                let value = {
                    let _scope = ctx.budget.scope();
                    decode_node_field_with_control::<ExecError>(bytes, column, name, |size| {
                        ctx.budget.charge(size)
                    })?
                };
                ctx.budget
                    .charge(budget::value_bytes(&value).saturating_mul(2))?;
                return Ok(value);
            }
        }
    }
    let value = {
        let _scope = ctx.budget.scope();
        node_field_with_control::<ExecError>(ctx.txn, id, column, name, |bytes| {
            ctx.budget.charge(bytes)
        })?
        .ok_or(GraphError::NodeNotFound(id))?
    };
    // Only the extracted value survives; cover retained container capacity as well as contents.
    ctx.budget
        .charge(budget::value_bytes(&value).saturating_mul(2))?;
    Ok(value)
}

pub(super) fn edge_property(
    ctx: &Context<'_, '_>,
    edge: &Relationship,
    name: &str,
) -> Result<Value, ExecError> {
    if acl_property(name) {
        return Ok(Value::Null);
    }
    let value = {
        let _scope = ctx.budget.scope();
        let key = (edge.source, edge.edge_type, edge.target, edge.id);
        let row =
            edge_row_with_control::<ExecError>(ctx.txn, key, |bytes| ctx.budget.charge(bytes))?;
        row.and_then(|mut row| {
            row.extra.remove(name).or_else(|| {
                let ty = ctx
                    .catalog
                    .edge_types
                    .iter()
                    .find(|t| t.id == edge.edge_type)?;
                let column = ty.columns.iter().find(|column| column.name == name)?;
                row.columns.remove(&column.id)
            })
        })
        .unwrap_or(Value::Null)
    };
    ctx.budget
        .charge(budget::value_bytes(&value).saturating_mul(2))?;
    Ok(value)
}

pub(super) fn edge_props(
    ctx: &Context<'_, '_>,
    edge: &Relationship,
) -> Result<BTreeMap<String, Value>, ExecError> {
    let key = (edge.source, edge.edge_type, edge.target, edge.id);
    let Some(mut row) =
        edge_row_with_control::<ExecError>(ctx.txn, key, |bytes| ctx.budget.charge(bytes))?
    else {
        return Ok(BTreeMap::new());
    };
    ctx.budget.charge(row.extra.len().saturating_mul(128))?;
    let mut props = BTreeMap::new();
    if let Some(ty) = ctx
        .catalog
        .edge_types
        .iter()
        .find(|t| t.id == edge.edge_type)
    {
        let columns = ty
            .columns
            .iter()
            .filter(|column| row.columns.contains_key(&column.id));
        let (count, names) = columns.fold((0usize, 0usize), |(count, names), column| {
            (
                count.saturating_add(1),
                names.saturating_add(column.name.len()),
            )
        });
        ctx.budget
            .charge(count.saturating_mul(128).saturating_add(names))?;
        for column in &ty.columns {
            if let Some(value) = row.columns.remove(&column.id) {
                props.insert(column.name.clone(), value);
            }
        }
    }
    props.extend(row.extra);
    props.retain(|name, _| !acl_property(name));
    Ok(props)
}

pub(super) fn rel_out(
    ctx: &Context<'_, '_>,
    rel: &Relationship,
) -> Result<RelationshipOut, ExecError> {
    // Type and element identity are cloned; property payloads are moved below.
    ctx.budget
        .charge(type_of(ctx, rel)?.len().saturating_add(128))?;
    Ok(RelationshipOut {
        id: rel.legacy_id(),
        element_id: rel.element_id(),
        start: rel.source,
        end: rel.target,
        rel_type: type_of(ctx, rel)?.to_string(),
        props: edge_props(ctx, rel)?,
    })
}

/// Admit output-vector storage and decode temporaries before construction. The
/// caller charges the completed row or acquires its owned lease synchronously.
pub(super) fn output_row(ctx: &Context<'_, '_>, row: Vec<Val>) -> Result<Vec<Out>, ExecError> {
    let _scope = ctx.budget.scope();
    ctx.budget
        .charge(row.len().saturating_mul(std::mem::size_of::<Out>()))?;
    let mut output = Vec::with_capacity(row.len());
    for value in row {
        output.push(out(ctx, value)?);
    }
    Ok(output)
}

fn plain_container_bytes(value: &Val) -> usize {
    match value {
        Val::List(items) => items.iter().fold(
            items.len().saturating_mul(std::mem::size_of::<Value>()),
            |sum, item| sum.saturating_add(plain_container_bytes(item)),
        ),
        Val::Map(map) => map
            .values()
            .fold(map.len().saturating_mul(128), |sum, item| {
                sum.saturating_add(plain_container_bytes(item))
            }),
        _ => 0,
    }
}

fn plain_output_value(value: Val) -> Result<Value, ExecError> {
    Ok(match value {
        Val::List(items) => {
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                values.push(plain_output_value(item)?);
            }
            Value::List(values)
        }
        Val::Map(map) => {
            let mut values = BTreeMap::new();
            for (key, item) in map {
                values.insert(key, plain_output_value(item)?);
            }
            Value::Map(values)
        }
        other => other.into_value()?,
    })
}

/// Convert a runtime value for output; plain values stay `Out::Value`.
pub(super) fn out(ctx: &Context<'_, '_>, value: Val) -> Result<Out, ExecError> {
    if !value.has_entity() {
        // Payload strings move, but list/map conversion constructs fresh containers.
        ctx.budget.charge(plain_container_bytes(&value))?;
        return Ok(Out::Value(plain_output_value(value)?));
    }
    Ok(match value {
        Val::Node(id) => Out::Node(node_out(ctx, id)?),
        Val::Rel(rel) => Out::Relationship(rel_out(ctx, &rel)?),
        Val::Path(path) => {
            ctx.budget.charge(
                path.nodes
                    .len()
                    .saturating_mul(std::mem::size_of::<NodeOut>())
                    .saturating_add(
                        path.rels
                            .len()
                            .saturating_mul(std::mem::size_of::<RelationshipOut>()),
                    ),
            )?;
            let mut nodes = Vec::with_capacity(path.nodes.len());
            for id in &path.nodes {
                nodes.push(node_out(ctx, *id)?);
            }
            let mut rels = Vec::with_capacity(path.rels.len());
            for rel in &path.rels {
                rels.push(rel_out(ctx, rel)?);
            }
            Out::Path(PathOut { nodes, rels })
        }
        Val::List(items) => {
            ctx.budget
                .charge(items.len().saturating_mul(std::mem::size_of::<Out>()))?;
            let mut output = Vec::with_capacity(items.len());
            for item in items {
                output.push(out(ctx, item)?);
            }
            Out::List(output)
        }
        Val::Map(map) => {
            ctx.budget.charge(
                map.len()
                    .saturating_mul(128usize.saturating_add(std::mem::size_of::<Out>())),
            )?;
            Out::Map(
                map.into_iter()
                    .map(|(k, v)| Ok((k, out(ctx, v)?)))
                    .collect::<Result<_, ExecError>>()?,
            )
        }
        _ => unreachable!("plain values returned above"),
    })
}

/// Whether a pattern predicate has at least one match for this row.
pub(super) fn exists(
    ctx: &Context<'_, '_>,
    row: &[Val],
    pattern: &Pattern,
) -> Result<bool, ExecError> {
    run::pattern_exists(ctx, row, pattern)
}
