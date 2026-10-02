use super::*;
use brainwire_store::graph::*;

pub(super) fn node_out(ctx: &Context<'_, '_>, id: NodeId) -> Result<NodeOut, ExecError> {
    let record = get_node(ctx.txn, id)?.ok_or(GraphError::NodeNotFound(id))?;
    let ty = ctx
        .catalog
        .node_types
        .iter()
        .find(|t| t.id == id.type_id())
        .ok_or_else(|| ExecError::Semantic("node type absent from catalog".into()))?;
    let mut props = BTreeMap::new();
    for column in &ty.columns {
        if let Some(v) = record.row.columns.get(&column.id) {
            props.insert(column.name.clone(), v.clone());
        }
    }
    props.extend(record.row.extra);
    props.insert("entity_id".into(), Value::Str(record.entity_id));
    Ok(NodeOut {
        id,
        label: ty.label.clone(),
        props,
    })
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
    let Some(row) = bind(row, pat, id) else {
        return Ok(None);
    };
    let node = node_out(ctx, id)?;
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
    let ty = ctx
        .catalog
        .node_type(pat.start.label.as_deref().expect("validated label"))
        .expect("validated type");
    let lookup = lookup_props(&pat.start, predicate);
    for row in rows {
        let candidates = if let Some(existing) = pat.start.var.as_ref().and_then(|v| row.get(v)) {
            match existing {
                Binding::Node(id) => vec![*id],
                _ => return Err(ExecError::Semantic("MATCH variable must be a node".into())),
            }
        } else if let Some((_, e)) = lookup.iter().find(|(name, _)| name == "entity_id") {
            // A reference to the pattern's own variable cannot seed the index lookup.
            if validate::mentions(e, pat.start.var.as_deref()) {
                nodes_of_type(ctx.txn, ty.id)?
            } else {
                match scalar(eval(e, &row, ctx)?)? {
                    Value::Str(s) => find_by_entity_id(ctx.txn, &s, Some(ty.id))?,
                    _ => vec![],
                }
            }
        } else {
            let mut indexed = None;
            for (field, expr) in &lookup {
                if validate::mentions(expr, pat.start.var.as_deref()) {
                    continue;
                }
                let value = scalar(eval(expr, &row, ctx)?)?;
                if let Some(found) = find_by_indexed_property(ctx.txn, ty, field, &value)? {
                    indexed = Some(found);
                    break;
                }
            }
            match indexed {
                Some(found) => found,
                None => nodes_of_type(ctx.txn, ty.id)?,
            }
        };
        for id in candidates {
            let Some(row) = matches(ctx, &row, &pat.start, id)? else {
                continue;
            };
            if let Some((rel, end)) = &pat.hop {
                let edge_type = ctx
                    .catalog
                    .edge_type(&rel.rel_type)
                    .expect("validated edge type")
                    .id;
                let edges = match rel.direction {
                    Direction::Out => out_edges(ctx.txn, id)?,
                    Direction::In => in_edges(ctx.txn, id)?,
                };
                for edge in edges {
                    if edge.edge_type == edge_type {
                        if let Some(row) = matches(ctx, &row, end, edge.other)? {
                            next.push(row);
                        }
                    }
                }
            } else {
                next.push(row);
            }
        }
    }
    Ok(next)
}
pub(super) fn write_pattern(
    txn: &mut Txn<'_>,
    catalog: &Catalog,
    params: &BTreeMap<String, Value>,
    row: &mut Bindings,
    pat: &Pattern,
    create: bool,
) -> Result<(), ExecError> {
    let ctx = Context {
        txn,
        catalog,
        params,
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
