//! Atomic, eager execution of the supported Cypher subset.
//!
//! Each call owns one snapshot and write overlay. All output is evaluated before
//! commit, so an error in a late clause drops every graph write in the call.
//! Scans, bindings and output are materialized; query memory budgets are not yet
//! enforced and this is not a streaming executor.
mod eval;
mod graph;
mod validate;

use crate::{ast::*, parse, ParseError};
use brainwire_core::{ids::NodeId, schema::Catalog, value::Value};
use brainwire_store::{
    db::{Database, StoreError},
    graph::GraphError,
    txn::{Cf, Txn, TxnError},
};
use eval::{eval, scalar};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq)]
pub struct QueryOutput {
    pub fields: Vec<String>,
    pub rows: Vec<Vec<Out>>,
}
#[derive(Debug, Clone, PartialEq)]
pub enum Out {
    Value(Value),
    Node(NodeOut),
}
#[derive(Debug, Clone, PartialEq)]
pub struct NodeOut {
    pub id: NodeId,
    pub label: String,
    pub props: BTreeMap<String, Value>,
}
#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error(transparent)]
    Syntax(#[from] ParseError),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("semantic error: {0}")]
    Semantic(String),
    #[error("missing parameter: {0}")]
    ParameterMissing(String),
    #[error("constraint violation: {0}")]
    ConstraintViolation(String),
    #[error("transaction conflict")]
    Conflict,
    #[error(transparent)]
    Graph(GraphError),
}
impl From<GraphError> for ExecError {
    fn from(e: GraphError) -> Self {
        match e {
            GraphError::Txn(TxnError::Conflict) => Self::Conflict,
            GraphError::UnknownLabel(s) | GraphError::UnknownEdgeType(s) => Self::Semantic(s),
            e => Self::Graph(e),
        }
    }
}
impl From<TxnError> for ExecError {
    fn from(e: TxnError) -> Self {
        Self::from(GraphError::Txn(e))
    }
}
impl From<StoreError> for ExecError {
    fn from(e: StoreError) -> Self {
        Self::Graph(GraphError::Store(e))
    }
}
#[derive(Debug, Clone, PartialEq)]
enum Binding {
    Node(NodeId),
    Value(Value),
}
type Bindings = BTreeMap<String, Binding>;
struct Context<'a, 'db> {
    txn: &'a Txn<'db>,
    catalog: &'a Catalog,
    params: &'a BTreeMap<String, Value>,
}

pub fn execute(
    db: &Database,
    query: &str,
    params: &BTreeMap<String, Value>,
) -> Result<QueryOutput, ExecError> {
    let query = parse(query)?;
    let mut txn = db.begin();
    // This dependency also rejects a concurrent catalog migration when writing.
    let catalog = match txn.get_for_update(Cf::Meta, b"catalog")? {
        Some(bytes) => {
            serde_json::from_slice(&bytes).map_err(|e| StoreError::Catalog(e.to_string()))?
        }
        None => Catalog {
            version: 0,
            node_types: vec![],
            edge_types: vec![],
            streams: vec![],
        },
    };
    validate::query(
        &query,
        &Context {
            txn: &txn,
            catalog: &catalog,
            params,
        },
    )?;
    let mut rows = vec![Bindings::new()];
    let mut output = QueryOutput {
        fields: vec![],
        rows: vec![],
    };
    let mut wrote = false;
    for clause in &query.clauses {
        let ctx = Context {
            txn: &txn,
            catalog: &catalog,
            params,
        };
        match clause {
            Clause::Unwind { expr, var } => {
                let mut next = vec![];
                for row in rows {
                    match scalar(eval(expr, &row, &ctx)?)? {
                        Value::Null => {}
                        Value::List(values) => {
                            for value in values {
                                let mut row = row.clone();
                                row.insert(var.clone(), Binding::Value(value));
                                next.push(row);
                            }
                        }
                        _ => return Err(ExecError::Semantic("UNWIND needs a list or null".into())),
                    }
                }
                rows = next;
            }
            Clause::Match { patterns, where_ } => {
                for pattern in patterns {
                    rows = graph::match_pattern(&ctx, rows, pattern)?;
                }
                if let Some(expr) = where_ {
                    let mut next = vec![];
                    for row in rows {
                        if eval::boolean(eval(expr, &row, &ctx)?)? == Some(true) {
                            next.push(row);
                        }
                    }
                    rows = next;
                }
            }
            Clause::Merge(pattern) => {
                for row in &mut rows {
                    graph::write_pattern(&mut txn, &catalog, params, row, pattern, false)?;
                    wrote = true;
                }
            }
            Clause::Create(patterns) => {
                for row in &mut rows {
                    for pattern in patterns {
                        graph::write_pattern(&mut txn, &catalog, params, row, pattern, true)?;
                        wrote = true;
                    }
                }
            }
            Clause::Set(items) => {
                // Evaluate one row's assignments together, then update each node once.
                for row in &rows {
                    let ctx = Context {
                        txn: &txn,
                        catalog: &catalog,
                        params,
                    };
                    let mut changes: BTreeMap<NodeId, BTreeMap<String, Value>> = BTreeMap::new();
                    for item in items {
                        let id = graph::bound_node(row, &item.var)?;
                        changes
                            .entry(id)
                            .or_default()
                            .insert(item.prop.clone(), scalar(eval(&item.value, row, &ctx)?)?);
                    }
                    for (id, props) in changes {
                        let node = graph::node_out(
                            &Context {
                                txn: &txn,
                                catalog: &catalog,
                                params,
                            },
                            id,
                        )?;
                        let Value::Str(entity_id) = &node.props["entity_id"] else {
                            unreachable!()
                        };
                        brainwire_store::graph::merge_node(
                            &mut txn,
                            &catalog,
                            &node.label,
                            entity_id,
                            &props,
                        )?;
                        wrote = true;
                    }
                }
            }
            Clause::DetachDelete(vars) => {
                let mut ids = BTreeSet::new();
                for row in &rows {
                    for var in vars {
                        ids.insert(graph::bound_node(row, var)?);
                    }
                }
                for id in ids {
                    brainwire_store::graph::detach_delete(&mut txn, id)?;
                    wrote = true;
                }
            }
            Clause::Return { items, limit } => {
                output = eval::project(&ctx, &rows, items, limit.as_ref())?;
            }
        }
    }
    if wrote {
        txn.commit()?;
    }
    Ok(output)
}
