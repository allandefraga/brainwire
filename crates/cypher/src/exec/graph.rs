use super::*;
use brainwire_store::graph::*;

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
fn acl_property(name: &str) -> bool {
    name.get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("acl_"))
}
fn node_with_visibility(
    ctx: &Context<'_, '_>,
    id: NodeId,
) -> Result<Option<(NodeRecord, bool)>, ExecError> {
    let Some(policy) = ctx.visibility else {
        return Ok(get_node_with_control::<ExecError>(ctx.txn, id, |bytes| {
            ctx.budget.charge(bytes)
        })?
        .map(|record| (record, true)));
    };
    // The private ACL is needed only while evaluating visibility. Keep its
    // reservation temporary while retaining the decoded node for the caller.
    let (result, node_charge) = {
        let _visibility_scope = ctx.budget.scope();
        let result = get_node_with_visibility_and_control::<ExecError>(
            ctx.txn,
            ctx.catalog,
            id,
            policy,
            |bytes| ctx.budget.charge(bytes),
        )?;
        let node_charge = result
            .as_ref()
            .map_or(0, |(_, _, node_charge)| *node_charge);
        (result, node_charge)
    };
    let Some((record, visible, _)) = result else {
        return Ok(None);
    };
    ctx.budget.charge(node_charge)?;
    Ok(Some((record, visible)))
}
fn node_record(ctx: &Context<'_, '_>, id: NodeId) -> Result<NodeRecord, ExecError> {
    if ctx.visibility.is_some() {
        let Some((record, visible)) = node_with_visibility(ctx, id)? else {
            return Err(ExecError::Forbidden("node is not readable"));
        };
        return if visible {
            Ok(record)
        } else {
            Err(ExecError::Forbidden("node is not readable"))
        };
    }
    Ok(
        get_node_with_control::<ExecError>(ctx.txn, id, |bytes| ctx.budget.charge(bytes))?
            .ok_or(GraphError::NodeNotFound(id))?,
    )
}
fn node_type<'a>(
    ctx: &'a Context<'_, '_>,
    id: NodeId,
) -> Result<&'a brainwire_core::schema::NodeType, ExecError> {
    ctx.catalog
        .node_types
        .iter()
        .find(|ty| ty.id == id.type_id())
        .ok_or_else(|| ExecError::Semantic("node type absent from catalog".into()))
}

pub(super) fn node_out(ctx: &Context<'_, '_>, id: NodeId) -> Result<NodeOut, ExecError> {
    node_out_from_record(ctx, node_record(ctx, id)?, id)
}
fn node_out_from_record(
    ctx: &Context<'_, '_>,
    mut record: NodeRecord,
    id: NodeId,
) -> Result<NodeOut, ExecError> {
    let ty = node_type(ctx, id)?;
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

/// Decode and charge the stored row, but do not build/copy a full property map
/// for a scalar read. Extra fields retain node_out's precedence after promotion.
pub(super) fn node_property(
    ctx: &Context<'_, '_>,
    id: NodeId,
    name: &str,
) -> Result<Value, ExecError> {
    let value = {
        let _scope = ctx.budget.scope();
        let mut record = node_record(ctx, id)?;
        let ty = node_type(ctx, id)?;
        if acl_property(name) {
            Value::Null
        } else if name == "entity_id" {
            Value::Str(record.entity_id)
        } else {
            record
                .row
                .extra
                .remove(name)
                .or_else(|| {
                    ty.columns
                        .iter()
                        .find(|column| column.name == name)
                        .and_then(|column| record.row.columns.remove(&column.id))
                })
                .unwrap_or(Value::Null)
        }
    };
    // Only the extracted value survives; cover retained container capacity as well as contents.
    ctx.budget
        .charge(budget::value_bytes(&value).saturating_mul(2))?;
    Ok(value)
}
pub(super) fn bound_node(row: &Bindings, name: &str) -> Result<NodeId, ExecError> {
    match row.get(name) {
        Some(Binding::Node(id)) => Ok(*id),
        _ => Err(ExecError::Semantic(format!("{name} is not a bound node"))),
    }
}
fn props(
    ctx: &Context<'_, '_>,
    row: &Bindings,
    items: &[(String, Expr)],
) -> Result<BTreeMap<String, Value>, ExecError> {
    items
        .iter()
        .map(|(name, e)| Ok((name.clone(), scalar(eval(e, row, ctx)?)?)))
        .collect()
}
fn bind(row: &Bindings, pat: &NodePat, id: NodeId) -> Option<Bindings> {
    let mut row = row.clone();
    if let Some(var) = &pat.var {
        if let Some(previous) = row.get(var) {
            if previous != &Binding::Node(id) {
                return None;
            }
        }
        row.insert(var.clone(), Binding::Node(id));
    }
    Some(row)
}
fn matches(
    ctx: &Context<'_, '_>,
    row: &Bindings,
    pat: &NodePat,
    id: NodeId,
) -> Result<Option<Bindings>, ExecError> {
    // Stored rows/ACLs used only to test the candidate are dropped before its bindings escape.
    let _scope = ctx.budget.scope();
    let (record, row) = if ctx.visibility.is_some() {
        let record = match node_with_visibility(ctx, id)? {
            None | Some((_, false)) => return Ok(None),
            Some((record, true)) => record,
        };
        let Some(row) = bind(row, pat, id) else {
            return Ok(None);
        };
        (record, row)
    } else {
        let Some(row) = bind(row, pat, id) else {
            return Ok(None);
        };
        let record =
            get_node_with_control::<ExecError>(ctx.txn, id, |bytes| ctx.budget.charge(bytes))?
                .ok_or(GraphError::NodeNotFound(id))?;
        (record, row)
    };
    if pat.props.is_empty() {
        // Validate existence/corruption, without constructing unused properties.
        return Ok((pat.label.as_ref() == Some(&node_type(ctx, id)?.label)).then_some(row));
    }
    let node = node_out_from_record(ctx, record, id)?;
    if pat.label.as_ref() != Some(&node.label) {
        return Ok(None);
    }
    for (name, expected) in props(ctx, &row, &pat.props)? {
        let actual = node.props.get(&name).cloned().unwrap_or(Value::Null);
        if !eval::equal(&Binding::Value(actual), &Binding::Value(expected)) {
            return Ok(None);
        }
    }
    Ok(Some(row))
}

/// Only equality conjuncts with row-independent values can seed a lookup.
/// OR/NOT and variable-dependent comparisons remain residual predicates.
fn lookup_props(pat: &NodePat, predicate: Option<&Expr>) -> Vec<(String, Expr)> {
    fn collect(expr: &Expr, var: Option<&str>, props: &mut Vec<(String, Expr)>) {
        match expr {
            Expr::And(left, right) => {
                collect(left, var, props);
                collect(right, var, props);
            }
            Expr::Cmp(left, CmpOp::Eq, right) => {
                for (property, value) in [
                    (left.as_ref(), right.as_ref()),
                    (right.as_ref(), left.as_ref()),
                ] {
                    if let Expr::Prop(base, field) = property {
                        if matches!(base.as_ref(), Expr::Var(name) if Some(name.as_str()) == var)
                            && matches!(value, Expr::Literal(_) | Expr::Param(_))
                        {
                            props.push((field.clone(), value.clone()));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let mut props = pat.props.clone();
    if let Some(predicate) = predicate {
        collect(predicate, pat.var.as_deref(), &mut props);
    }
    props
}

pub(super) fn match_pattern(
    ctx: &Context<'_, '_>,
    rows: Vec<Bindings>,
    pat: &Pattern,
    predicate: Option<&Expr>,
) -> Result<Vec<Bindings>, ExecError> {
    let mut next = vec![];
    for row in rows {
        visit_pattern::<ExecError>(ctx, &row, pat, predicate, &mut |row| {
            ctx.budget.retain_bindings(&row)?;
            next.push(row);
            Ok(())
        })?;
    }
    Ok(next)
}

/// Shared candidate traversal for eager writes and bounded read pipelines.
/// Callback errors stop the underlying RocksDB iterator immediately.
pub(super) fn visit_pattern<E>(
    ctx: &Context<'_, '_>,
    row: &Bindings,
    pat: &Pattern,
    predicate: Option<&Expr>,
    visit: &mut dyn FnMut(Bindings) -> Result<(), E>,
) -> Result<(), E>
where
    E: From<ExecError> + From<GraphError> + From<StoreError>,
{
    let _lookup_scope = ctx.budget.scope();
    let ty = ctx
        .catalog
        .node_type(pat.start.label.as_deref().expect("validated label"))
        .expect("validated type");
    let mut candidate = |id| -> Result<(), E> {
        let _row_scope = ctx.budget.scope();
        ctx.budget.charge(16)?;
        let Some(row) = matches(ctx, row, &pat.start, id)? else {
            return Ok(());
        };
        ctx.budget.bindings(&row)?;
        if let Some((rel, end)) = &pat.hop {
            let edge_type = ctx
                .catalog
                .edge_type(&rel.rel_type)
                .expect("validated edge type")
                .id;
            let cf = match rel.direction {
                Direction::Out => Cf::Out,
                Direction::In => Cf::In,
            };
            visit_edges_of_type::<E>(ctx.txn, cf, id, edge_type, |edge| {
                let _edge_scope = ctx.budget.scope();
                if let Some(mut row) = matches(ctx, &row, end, edge.other)? {
                    if let Some(var) = &rel.var {
                        let (source, target) = match rel.direction {
                            Direction::Out => (id, edge.other),
                            Direction::In => (edge.other, id),
                        };
                        let binding = Binding::Relationship(Relationship {
                            source,
                            target,
                            edge_type,
                            id: edge.id,
                        });
                        if row.get(var).is_some_and(|previous| previous != &binding) {
                            return Ok(());
                        }
                        row.insert(var.clone(), binding);
                    }
                    ctx.budget.bindings(&row)?;
                    visit(row)?;
                }
                Ok(())
            })?;
        } else {
            visit(row)?;
        }
        Ok(())
    };
    if let Some(existing) = pat.start.var.as_ref().and_then(|v| row.get(v)) {
        return match existing {
            Binding::Node(id) => candidate(*id),
            _ => Err(ExecError::Semantic("MATCH variable must be a node".into()).into()),
        };
    }
    let lookup = lookup_props(&pat.start, predicate);
    if let Some((_, expr)) = lookup.iter().find(|(name, _)| name == "entity_id") {
        if !validate::mentions(expr, pat.start.var.as_deref()) {
            if let Value::Str(value) = scalar(eval(expr, row, ctx)?)? {
                for id in find_by_entity_id(ctx.txn, &value, Some(ty.id))? {
                    candidate(id)?;
                }
            }
            return Ok(());
        }
    } else {
        for (field, expr) in &lookup {
            if validate::mentions(expr, pat.start.var.as_deref()) {
                continue;
            }
            let value = scalar(eval(expr, row, ctx)?)?;
            if visit_by_indexed_property::<E>(ctx.txn, ty, field, &value, &mut candidate)? {
                return Ok(());
            }
        }
    }
    visit_nodes_of_type::<E>(ctx.txn, ty.id, candidate)
}
pub(super) fn write_pattern(
    txn: &mut Txn<'_>,
    catalog: &Catalog,
    params: &BTreeMap<String, Value>,
    row: &mut Bindings,
    pat: &Pattern,
    create: bool,
    budget: &Budget,
) -> Result<(), ExecError> {
    let ctx = Context {
        txn,
        catalog,
        params,
        budget,
        visibility: None,
    };
    if let Some((rel, end)) = &pat.hop {
        let a = bound_node(row, pat.start.var.as_deref().expect("validated endpoint"))?;
        let b = bound_node(row, end.var.as_deref().expect("validated endpoint"))?;
        let props = props(&ctx, row, &rel.props)?;
        let (src, dst) = match rel.direction {
            Direction::Out => (a, b),
            Direction::In => (b, a),
        };
        merge_edge(txn, catalog, src, &rel.rel_type, dst, &props)?;
    } else {
        let label = pat.start.label.as_deref().expect("validated label");
        let mut props = props(&ctx, row, &pat.start.props)?;
        let Some(Value::Str(entity_id)) = props.remove("entity_id") else {
            return Err(ExecError::Semantic("entity_id needs a string".into()));
        };
        let ty = catalog.node_type(label).expect("validated label");
        if create && !find_by_entity_id(txn, &entity_id, Some(ty.id))?.is_empty() {
            return Err(ExecError::ConstraintViolation(format!(
                "{label} entity_id {entity_id} already exists"
            )));
        }
        let id = merge_node(txn, catalog, label, &entity_id, &props)?;
        if let Some(var) = &pat.start.var {
            row.insert(var.clone(), Binding::Node(id));
        }
    }
    Ok(())
}
