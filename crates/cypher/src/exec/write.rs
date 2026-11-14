//! Write operators. Each runs over every row of its segment: expressions are
//! evaluated against the transaction first, then the mutation is staged.
use super::eval::eval;
use super::plan::*;
use super::run::{Env, Flow, Row};
use super::val::Val;
use super::*;
use brainwire_store::graph as store;

fn semantic(message: impl Into<String>) -> ExecError {
    ExecError::Semantic(message.into())
}

/// Charge the overlay growth of staged writes to the query guard.
struct Overlay {
    bytes: usize,
}
impl Overlay {
    fn charge(&mut self, txn: &Txn<'_>, env: &Env<'_>) -> Result<(), ExecError> {
        let now = txn.estimated_memory_bytes();
        env.budget.charge(now.saturating_sub(self.bytes))?;
        self.bytes = now;
        Ok(())
    }
}

pub(super) fn apply(
    txn: &mut Txn<'_>,
    env: &Env<'_>,
    op: &Op,
    rows: Vec<Row>,
    wrote: &mut bool,
) -> Result<Vec<Row>, ExecError> {
    let mut overlay = Overlay {
        bytes: txn.estimated_memory_bytes(),
    };
    match op {
        Op::Create(patterns) => {
            let mut out = vec![];
            for mut row in rows {
                for pattern in patterns {
                    write_pattern(txn, env, pattern, &mut row, true, true, &mut overlay)?;
                    *wrote = true;
                }
                out.push(row);
            }
            Ok(out)
        }
        Op::Merge {
            pattern,
            on_create,
            on_match,
        } => {
            let mut out = vec![];
            for mut row in rows {
                let track = !on_create.is_empty() || !on_match.is_empty();
                let created =
                    write_pattern(txn, env, pattern, &mut row, false, track, &mut overlay)?;
                *wrote = true;
                let items = if created { on_create } else { on_match };
                if !items.is_empty() {
                    set_items(txn, env, items, &row, &mut overlay)?;
                }
                out.push(row);
            }
            Ok(out)
        }
        Op::Set(items) => {
            for row in &rows {
                *wrote |= set_items(txn, env, items, row, &mut overlay)?;
            }
            Ok(rows)
        }
        Op::Delete { detach, targets } => {
            delete(txn, env, *detach, targets, &rows, wrote, &mut overlay)?;
            Ok(rows)
        }
        Op::Call {
            name, args, slots, ..
        } if name == ASSERT_NODE_STATE => {
            let (label, entity_id, expected) =
                procedures::node_state_arguments(args, &env_ctx(env, txn))?;
            let ty = env
                .catalog
                .node_type(&label)
                .ok_or(ExecError::PreconditionFailed)?;
            // Cover both dependency keys and transient lookup key copies.
            env.budget.charge(384 + 3 * entity_id.len())?;
            if !store::guard_node_state::<ExecError>(txn, ty, &entity_id, &expected, |bytes| {
                env.budget.check_deadline()?;
                env.budget.charge(bytes)
            })? {
                return Err(ExecError::PreconditionFailed);
            }
            let mut rows = rows;
            for row in &mut rows {
                row[slots[0]] = Val::Bool(true);
            }
            Ok(rows)
        }
        Op::Call {
            name,
            args,
            yields,
            slots,
            ..
        } if name == "db.setNodeAcl" || name == "db.setNodeAcls" => {
            let mut out = vec![];
            for row in rows {
                let (stamps, produced) = {
                    let ctx = env_ctx(env, txn);
                    if name == "db.setNodeAcl" {
                        let (id, acl, produced) = procedures::prepare_node_acl(args, yields, &ctx)?;
                        (vec![(id, acl)], produced)
                    } else {
                        procedures::prepare_node_acls(args, yields, &ctx)?
                    }
                };
                for (id, acl) in &stamps {
                    store::set_node_acl(txn, *id, acl)?;
                }
                overlay.charge(txn, env)?;
                *wrote = true;
                for produced in produced {
                    let mut next = row.clone();
                    for (item, slot) in yields.iter().zip(slots) {
                        next[*slot] = produced.get(&item.alias).cloned().unwrap_or(Val::Null);
                    }
                    out.push(next);
                }
            }
            Ok(out)
        }
        Op::Call { name, args, .. } => {
            let values = args
                .iter()
                .map(|arg| super::eval::constant(arg, &env_ctx(env, txn)))
                .collect::<Result<Vec<_>, _>>()?;
            procedures::admin(txn, name, &values)?;
            *wrote = true;
            Ok(vec![])
        }
        _ => unreachable!("only write operators are applied"),
    }
}

fn env_ctx<'t, 'db>(env: &'t Env<'_>, txn: &'t Txn<'db>) -> Context<'t, 'db> {
    Context {
        txn,
        catalog: env.catalog,
        params: env.params,
        budget: env.budget,
        visibility: env.visibility,
        checkpoint_dir: env.checkpoint_dir,
        vars: env.vars,
        cache: None,
        param_vals: Some(env.param_vals),
        partition: None,
    }
}

fn props(
    env: &Env<'_>,
    txn: &Txn<'_>,
    row: &[Val],
    items: &[(String, Expr)],
) -> Result<BTreeMap<String, Value>, ExecError> {
    let ctx = env_ctx(env, txn);
    items
        .iter()
        .map(|(name, e)| Ok((name.clone(), eval(e, row, &ctx)?.into_value()?)))
        .collect()
}

/// A stable entity id for a node merged without one: its label-local property map.
fn derived_entity_id(props: &BTreeMap<String, Value>) -> Result<String, ExecError> {
    fn json(value: &Value) -> serde_json::Value {
        match value {
            Value::Null => serde_json::Value::Null,
            Value::Bool(b) => (*b).into(),
            Value::Int(i) => (*i).into(),
            Value::Float(f) => serde_json::Number::from_f64(*f)
                .map(serde_json::Value::Number)
                .unwrap_or_else(|| f.to_string().into()),
            Value::Str(s) => s.clone().into(),
            Value::List(items) => items.iter().map(json).collect(),
            Value::Map(map) => map.iter().map(|(k, v)| (k.clone(), json(v))).collect(),
        }
    }
    if props.is_empty() {
        return Err(semantic(
            "MERGE of a node without entity_id needs at least one property",
        ));
    }
    let text = serde_json::to_string(&json(&Value::Map(props.clone())))
        .map_err(|e| semantic(e.to_string()))?;
    if text.len() > 1000 {
        return Err(semantic(
            "MERGE without entity_id: properties too large to identify the node",
        ));
    }
    Ok(format!("~{text}"))
}

fn generated_entity_id() -> String {
    format!("~{}", uuid::Uuid::new_v4())
}

/// Create or merge one pattern; returns whether anything was created.
fn write_pattern(
    txn: &mut Txn<'_>,
    env: &Env<'_>,
    pattern: &WritePattern,
    row: &mut Row,
    create: bool,
    track_created: bool,
    overlay: &mut Overlay,
) -> Result<bool, ExecError> {
    let mut created = false;
    let mut ids = vec![];
    for node in &pattern.nodes {
        if node.bound {
            match &row[node.slot] {
                Val::Node(id) => ids.push(*id),
                Val::Null => {
                    return Err(semantic("cannot write a relationship to a null node"));
                }
                other => {
                    return Err(semantic(format!(
                        "expected a node, found {}",
                        other.type_name()
                    )));
                }
            }
            continue;
        }
        let label = node.label.as_deref().expect("planned label");
        let ty = env.catalog.node_type(label).expect("validated label");
        let mut values = props(env, txn, row, &node.props)?;
        let entity_id = match values.remove("entity_id") {
            Some(Value::Str(entity_id)) => entity_id,
            Some(_) => return Err(semantic("entity_id needs a string")),
            None if create => generated_entity_id(),
            None => derived_entity_id(&values)?,
        };
        // A plain MERGE needs no existence probe: merge_node upserts.
        let exists =
            track_created && !store::find_by_entity_id(txn, &entity_id, Some(ty.id))?.is_empty();
        if create && exists {
            return Err(ExecError::ConstraintViolation(format!(
                "{label} entity_id {entity_id} already exists"
            )));
        }
        created |= !exists;
        let id = store::merge_node(txn, env.catalog, label, &entity_id, &values)?;
        overlay.charge(txn, env)?;
        row[node.slot] = Val::Node(id);
        ids.push(id);
    }
    for (index, rel) in pattern.rels.iter().enumerate() {
        let (a, b) = (ids[index], ids[index + 1]);
        let (source, target) = match rel.direction {
            Direction::In => (b, a),
            _ => (a, b),
        };
        let values = props(env, txn, row, &rel.props)?;
        let ty = env
            .catalog
            .edge_type(&rel.rel_type)
            .expect("validated type");
        if !ty.merge_key_fields.is_empty() {
            if create {
                return Err(semantic(
                    "CREATE is not supported for keyed relationship types; use MERGE",
                ));
            }
            for field in &ty.merge_key_fields {
                if !matches!(values.get(field), Some(Value::Str(value)) if !value.is_empty()) {
                    return Err(semantic(format!(
                        "relationship merge key {field} requires a nonempty string"
                    )));
                }
            }
            let (source, target, id, was_created) = {
                let _scope = env.budget.scope();
                let mut decode_peak = 0usize;
                store::merge_keyed_edge::<ExecError>(
                    txn,
                    env.catalog,
                    source,
                    &rel.rel_type,
                    target,
                    rel.direction == Direction::Both,
                    &values,
                    |bytes| {
                        env.budget.tick()?;
                        let bytes = bytes.saturating_mul(2);
                        env.budget.charge(bytes.saturating_sub(decode_peak))?;
                        decode_peak = decode_peak.max(bytes);
                        Ok(())
                    },
                )?
            };
            created |= was_created;
            overlay.charge(txn, env)?;
            row[rel.slot] = Val::Rel(Relationship {
                source,
                target,
                edge_type: ty.id,
                id,
            });
            continue;
        }
        let key = (source, ty.id, target, brainwire_core::ids::EdgeId::MERGED);
        let exists = store::edge_exists(txn, key)?
            || (rel.direction == Direction::Both
                && store::edge_exists(
                    txn,
                    (target, ty.id, source, brainwire_core::ids::EdgeId::MERGED),
                )?);
        let (source, target) = if !exists || store::edge_exists(txn, key)? {
            (source, target)
        } else {
            (target, source)
        };
        created |= !exists;
        store::merge_edge(txn, env.catalog, source, &rel.rel_type, target, &values)?;
        overlay.charge(txn, env)?;
        row[rel.slot] = Val::Rel(Relationship {
            source,
            target,
            edge_type: ty.id,
            id: brainwire_core::ids::EdgeId::MERGED,
        });
    }
    if let Some(slot) = pattern.path {
        let rels: Vec<Val> = pattern.rels.iter().map(|r| row[r.slot].clone()).collect();
        row[slot] = eval::path(&ids, &rels)?;
    }
    Ok(created)
}

enum Target {
    Node(NodeId),
    Rel(Relationship),
}

/// Apply SET/REMOVE items for one row, one store write per entity.
fn set_items(
    txn: &mut Txn<'_>,
    env: &Env<'_>,
    items: &[SetPlan],
    row: &[Val],
    overlay: &mut Overlay,
) -> Result<bool, ExecError> {
    let mut changes: Vec<(Target, BTreeMap<String, Value>)> = vec![];
    {
        let ctx = env_ctx(env, txn);
        for item in items {
            let slot = match item {
                SetPlan::Prop { slot, .. }
                | SetPlan::Replace { slot, .. }
                | SetPlan::Merge { slot, .. }
                | SetPlan::Labels { slot, .. } => *slot,
            };
            let target = match &row[slot] {
                Val::Null => continue,
                Val::Node(id) => Target::Node(*id),
                Val::Rel(rel) => Target::Rel(*rel),
                other => {
                    return Err(semantic(format!(
                        "SET needs a node or relationship, found {}",
                        other.type_name()
                    )));
                }
            };
            let mut values = BTreeMap::new();
            match item {
                SetPlan::Prop { prop, value, .. } => {
                    values.insert(prop.clone(), eval(value, row, &ctx)?.into_value()?);
                }
                SetPlan::Replace { value, .. } | SetPlan::Merge { value, .. } => {
                    let map = match eval(value, row, &ctx)? {
                        Val::Map(map) => map
                            .into_iter()
                            .map(|(k, v)| Ok((k, v.into_value()?)))
                            .collect::<Result<BTreeMap<_, _>, ExecError>>()?,
                        Val::Node(id) => graph::node_out(&ctx, id)?.props,
                        Val::Rel(rel) => graph::edge_props(&ctx, &rel)?,
                        Val::Null => BTreeMap::new(),
                        other => {
                            return Err(semantic(format!(
                                "SET needs a map, found {}",
                                other.type_name()
                            )));
                        }
                    };
                    if matches!(item, SetPlan::Replace { .. }) {
                        let current = match &target {
                            Target::Node(id) => graph::node_out(&ctx, *id)?.props,
                            Target::Rel(rel) => graph::edge_props(&ctx, rel)?,
                        };
                        for key in current.into_keys() {
                            if key != "entity_id" {
                                values.insert(key, Value::Null);
                            }
                        }
                    }
                    values.extend(map);
                    values.remove("entity_id");
                }
                SetPlan::Labels { labels, .. } => {
                    let Target::Node(id) = target else {
                        return Err(semantic("labels belong to nodes"));
                    };
                    let label = graph::label_of(&ctx, id)?;
                    if labels.iter().any(|l| l != label) {
                        return Err(ExecError::Unsupported(
                            "a node's label is fixed when it is created".into(),
                        ));
                    }
                    continue;
                }
            }
            match changes.iter_mut().find(|(t, _)| match (t, &target) {
                (Target::Node(a), Target::Node(b)) => a == b,
                (Target::Rel(a), Target::Rel(b)) => a == b,
                _ => false,
            }) {
                Some((_, existing)) => existing.extend(values),
                None => changes.push((target, values)),
            }
        }
    }
    let wrote = !changes.is_empty();
    for (target, values) in changes {
        match target {
            Target::Node(id) => {
                let (label, entity_id) =
                    {
                        let ctx = env_ctx(env, txn);
                        let label = graph::label_of(&ctx, id)?.to_string();
                        let Some(Value::Str(entity_id)) = store::node_field_with_control::<
                            ExecError,
                        >(
                            txn, id, None, "entity_id", |_| Ok(())
                        )?
                        else {
                            return Err(GraphError::NodeNotFound(id).into());
                        };
                        (label, entity_id)
                    };
                store::merge_node(txn, env.catalog, &label, &entity_id, &values)?;
            }
            Target::Rel(rel) => {
                let keyed = env
                    .catalog
                    .edge_types
                    .iter()
                    .find(|ty| ty.id == rel.edge_type)
                    .is_some_and(|ty| !ty.merge_key_fields.is_empty());
                if !keyed && rel.id != brainwire_core::ids::EdgeId::MERGED {
                    return Err(ExecError::Unsupported(
                        "SET on a relationship created outside MERGE".into(),
                    ));
                }
                let name = {
                    let ctx = env_ctx(env, txn);
                    graph::type_of(&ctx, &rel)?.to_string()
                };
                if !store::edge_exists(txn, (rel.source, rel.edge_type, rel.target, rel.id))? {
                    continue;
                }
                if keyed {
                    store::update_edge_with_id(
                        txn,
                        env.catalog,
                        rel.source,
                        &name,
                        rel.target,
                        rel.id,
                        &values,
                    )?;
                } else {
                    store::merge_edge(txn, env.catalog, rel.source, &name, rel.target, &values)?;
                }
            }
        }
        overlay.charge(txn, env)?;
    }
    Ok(wrote)
}

fn delete(
    txn: &mut Txn<'_>,
    env: &Env<'_>,
    detach: bool,
    targets: &[Expr],
    rows: &[Row],
    wrote: &mut bool,
    overlay: &mut Overlay,
) -> Result<(), ExecError> {
    let mut nodes = BTreeSet::new();
    let mut rels = BTreeSet::new();
    {
        let ctx = env_ctx(env, txn);
        fn collect(
            value: Val,
            nodes: &mut BTreeSet<NodeId>,
            rels: &mut BTreeSet<Relationship>,
        ) -> Result<(), ExecError> {
            match value {
                Val::Null => {}
                Val::Node(id) => {
                    nodes.insert(id);
                }
                Val::Rel(rel) => {
                    rels.insert(rel);
                }
                Val::Path(path) => {
                    nodes.extend(path.nodes);
                    rels.extend(path.rels);
                }
                Val::List(items) => {
                    for item in items {
                        collect(item, nodes, rels)?;
                    }
                }
                other => {
                    return Err(ExecError::Semantic(format!(
                        "DELETE needs a node, relationship or path, found {}",
                        other.type_name()
                    )));
                }
            }
            Ok(())
        }
        for row in rows {
            for target in targets {
                env.budget.check_deadline()?;
                collect(eval(target, row, &ctx)?, &mut nodes, &mut rels)?;
            }
        }
    }
    for rel in rels {
        *wrote |= store::delete_edge(txn, rel.source, rel.edge_type, rel.target, rel.id)?;
        overlay.charge(txn, env)?;
    }
    for id in nodes {
        if !store::node_exists(txn, id)? {
            continue;
        }
        if !detach && has_relationship(txn, id)? {
            return Err(ExecError::ConstraintViolation(format!(
                "cannot delete node {} because it still has relationships; use DETACH DELETE",
                id.element_id()
            )));
        }
        {
            let _scope = env.budget.scope();
            store::detach_delete_with_control(txn, env.catalog, id, |bytes| {
                env.budget.charge(bytes)
            })?;
        }
        overlay.charge(txn, env)?;
        *wrote = true;
    }
    Ok(())
}

fn has_relationship(txn: &Txn<'_>, id: NodeId) -> Result<bool, ExecError> {
    for cf in [brainwire_store::txn::Cf::Out, brainwire_store::txn::Cf::In] {
        match store::visit_edges::<Flow>(txn, cf, id, |_| Err(Flow::Stop)) {
            Ok(()) => {}
            Err(Flow::Stop) => return Ok(true),
            Err(Flow::Error(error)) => return Err(error),
        }
    }
    Ok(false)
}
