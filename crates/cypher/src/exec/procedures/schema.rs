//! Bounded catalog inspection; never visits node or relationship instance data.
use super::*;

const MAX_NODE_TYPES: usize = 4096;
const MAX_SIGNATURES: usize = 4096;
const MAX_STRING_BYTES: usize = 1 << 20;
const MAX_ENTRIES: usize = 65536;

fn property(name: &str, node: bool) -> bool {
    !name
        .as_bytes()
        .get(..4)
        .is_some_and(|p| p.eq_ignore_ascii_case(b"acl_"))
        && (!node || name != "entity_id")
}

fn endpoint<'a>(
    ctx: &Context<'_, '_>,
    labels: &BTreeMap<brainwire_core::ids::TypeId, &'a str>,
    id: brainwire_core::ids::TypeId,
) -> Result<&'a str, ExecError> {
    ctx.budget.check_deadline()?;
    labels.get(&id).copied().ok_or_else(|| {
        StoreError::Corrupt("schema found a relationship signature with a missing endpoint".into())
            .into()
    })
}

#[derive(Default)]
struct Size {
    strings: usize,
    entries: usize,
}
impl Size {
    fn add(&mut self, strings: &[&str], entries: usize) -> Result<(), ExecError> {
        self.strings = strings
            .iter()
            .fold(self.strings, |n, s| n.saturating_add(s.len()));
        self.entries = self.entries.saturating_add(entries);
        if self.strings > MAX_STRING_BYTES {
            return Err(semantic("db.schema exceeds the 1 MiB schema string limit"));
        }
        if self.entries > MAX_ENTRIES {
            return Err(semantic(
                "db.schema exceeds the 65536 collection entry limit",
            ));
        }
        Ok(())
    }
}

pub(super) fn values(ctx: &Context<'_, '_>, yield_count: usize) -> Result<Vec<Binding>, ExecError> {
    ctx.budget.check_deadline()?;
    if ctx.catalog.node_types.len() > MAX_NODE_TYPES {
        return Err(semantic("db.schema exceeds the 4096 node type limit"));
    }
    let mut labels = BTreeMap::new();
    for ty in &ctx.catalog.edge_types {
        ctx.budget.check_deadline()?;
        if !ty.signatures.is_empty() {
            // Cover temporary tree nodes before allocation; labels remain borrowed.
            ctx.budget.charge(
                1024usize.saturating_add(ctx.catalog.node_types.len().saturating_mul(128)),
            )?;
            for node in &ctx.catalog.node_types {
                ctx.budget.check_deadline()?;
                // Match the former linear lookup for malformed duplicate IDs.
                labels.entry(node.id).or_insert(node.label.as_str());
            }
            break;
        }
    }
    let mut size = Size::default();
    size.add(&["version", "nodes", "relationships", "indexesReady"], 4)?;
    for ty in &ctx.catalog.node_types {
        ctx.budget.check_deadline()?;
        size.add(
            &[
                "label",
                "properties",
                "indexedFields",
                &ty.label,
                "entity_id",
            ],
            5,
        )?;
        if let Some(description) = &ty.description {
            size.add(&["description", description], 1)?;
        }
        for column in &ty.columns {
            ctx.budget.check_deadline()?;
            if property(&column.name, true) {
                size.add(&[&column.name], 1)?;
            }
        }
        for name in &ty.indexed {
            ctx.budget.check_deadline()?;
            size.add(&[name], 1)?;
        }
    }
    let mut signatures = 0usize;
    for ty in &ctx.catalog.edge_types {
        ctx.budget.check_deadline()?;
        signatures = signatures.saturating_add(ty.signatures.len());
        if signatures > MAX_SIGNATURES {
            return Err(semantic(
                "db.schema exceeds the 4096 relationship signature limit",
            ));
        }
        for &(source, target) in &ty.signatures {
            ctx.budget.check_deadline()?;
            size.add(
                &[
                    "name",
                    "source",
                    "target",
                    "properties",
                    &ty.name,
                    endpoint(ctx, &labels, source)?,
                    endpoint(ctx, &labels, target)?,
                ],
                5,
            )?;
            if let Some(description) = &ty.description {
                size.add(&["description", description], 1)?;
            }
            for column in &ty.columns {
                ctx.budget.check_deadline()?;
                if property(&column.name, false) {
                    size.add(&[&column.name], 1)?;
                }
            }
        }
    }
    // Cover collection/map overhead and string data, including the copies made
    // when YIELD aliases bind these values. Normal binding/output accounting
    // remains in force. This conservative reservation is not an RSS bound.
    let bytes = 4096usize
        .saturating_add(size.entries.saturating_mul(256))
        .saturating_add(size.strings.saturating_mul(2))
        .saturating_mul(yield_count.saturating_add(1));
    ctx.budget.charge(bytes)?;

    let mut nodes = Vec::with_capacity(ctx.catalog.node_types.len());
    for ty in &ctx.catalog.node_types {
        ctx.budget.check_deadline()?;
        let mut props = vec![Value::Str("entity_id".into())];
        for column in &ty.columns {
            ctx.budget.check_deadline()?;
            if property(&column.name, true) {
                props.push(Value::Str(column.name.clone()));
            }
        }
        let mut indexed = Vec::with_capacity(ty.indexed.len());
        for name in &ty.indexed {
            ctx.budget.check_deadline()?;
            indexed.push(Value::Str(name.clone()));
        }
        let mut fields = BTreeMap::from([
            ("label".into(), Value::Str(ty.label.clone())),
            ("properties".into(), Value::List(props)),
            ("indexedFields".into(), Value::List(indexed)),
        ]);
        if let Some(description) = &ty.description {
            fields.insert("description".into(), Value::Str(description.clone()));
        }
        nodes.push(Value::Map(fields));
    }
    let mut relationships = Vec::with_capacity(signatures);
    for ty in &ctx.catalog.edge_types {
        ctx.budget.check_deadline()?;
        for &(source, target) in &ty.signatures {
            ctx.budget.check_deadline()?;
            let mut props = Vec::new();
            for column in &ty.columns {
                ctx.budget.check_deadline()?;
                if property(&column.name, false) {
                    props.push(Value::Str(column.name.clone()));
                }
            }
            let mut fields = BTreeMap::from([
                ("name".into(), Value::Str(ty.name.clone())),
                (
                    "source".into(),
                    Value::Str(endpoint(ctx, &labels, source)?.into()),
                ),
                (
                    "target".into(),
                    Value::Str(endpoint(ctx, &labels, target)?.into()),
                ),
                ("properties".into(), Value::List(props)),
            ]);
            if let Some(description) = &ty.description {
                fields.insert("description".into(), Value::Str(description.clone()));
            }
            relationships.push(Value::Map(fields));
        }
    }
    let ready = brainwire_store::graph::scalar_indexes_ready(ctx.txn)?;
    Ok(vec![
        value(Value::Int(ctx.catalog.version.into())),
        value(Value::List(nodes)),
        value(Value::List(relationships)),
        value(Value::Bool(ready)),
    ])
}
