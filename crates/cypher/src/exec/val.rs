//! Runtime values: stored property values plus graph entities and paths.
use super::*;
use std::cmp::Ordering;
use std::hash::{Hash, Hasher};

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Val {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Val>),
    Map(BTreeMap<String, Val>),
    Node(NodeId),
    Rel(Relationship),
    Path(Box<PathVal>),
}

/// A path: `rels[i]` joins `nodes[i]` and `nodes[i + 1]`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PathVal {
    pub nodes: Vec<NodeId>,
    pub rels: Vec<Relationship>,
}

impl From<Value> for Val {
    fn from(value: Value) -> Self {
        match value {
            Value::Null => Val::Null,
            Value::Bool(b) => Val::Bool(b),
            Value::Int(i) => Val::Int(i),
            Value::Float(f) => Val::Float(f),
            Value::Str(s) => Val::Str(s),
            Value::List(items) => Val::List(items.into_iter().map(Val::from).collect()),
            Value::Map(map) => Val::Map(map.into_iter().map(|(k, v)| (k, Val::from(v))).collect()),
        }
    }
}

impl From<&Value> for Val {
    fn from(value: &Value) -> Self {
        Val::from(value.clone())
    }
}

impl Val {
    pub fn is_null(&self) -> bool {
        matches!(self, Val::Null)
    }

    /// A storable property value; graph entities cannot be stored.
    pub fn into_value(self) -> Result<Value, ExecError> {
        Ok(match self {
            Val::Null => Value::Null,
            Val::Bool(b) => Value::Bool(b),
            Val::Int(i) => Value::Int(i),
            Val::Float(f) => Value::Float(f),
            Val::Str(s) => Value::Str(s),
            Val::List(items) => Value::List(
                items
                    .into_iter()
                    .map(Val::into_value)
                    .collect::<Result<_, _>>()?,
            ),
            Val::Map(map) => Value::Map(
                map.into_iter()
                    .map(|(k, v)| Ok((k, v.into_value()?)))
                    .collect::<Result<_, ExecError>>()?,
            ),
            Val::Node(_) | Val::Rel(_) | Val::Path(_) => {
                return Err(ExecError::Semantic(
                    "nodes, relationships and paths cannot be used as property values".into(),
                ));
            }
        })
    }

    pub fn has_entity(&self) -> bool {
        match self {
            Val::Node(_) | Val::Rel(_) | Val::Path(_) => true,
            Val::List(items) => items.iter().any(Val::has_entity),
            Val::Map(map) => map.values().any(Val::has_entity),
            _ => false,
        }
    }

    /// Conservative retained size for the query guard.
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<Val>().saturating_add(match self {
            Val::Str(s) => s.len(),
            Val::List(items) => items
                .iter()
                .fold(0usize, |sum, v| sum.saturating_add(v.bytes())),
            Val::Map(map) => map.iter().fold(0usize, |sum, (k, v)| {
                sum.saturating_add(64)
                    .saturating_add(k.len())
                    .saturating_add(v.bytes())
            }),
            Val::Path(path) => path.nodes.len().saturating_mul(8).saturating_add(
                path.rels
                    .len()
                    .saturating_mul(std::mem::size_of::<Relationship>()),
            ),
            _ => 0,
        })
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Val::Null => "null",
            Val::Bool(_) => "boolean",
            Val::Int(_) => "integer",
            Val::Float(_) => "float",
            Val::Str(_) => "string",
            Val::List(_) => "list",
            Val::Map(_) => "map",
            Val::Node(_) => "node",
            Val::Rel(_) => "relationship",
            Val::Path(_) => "path",
        }
    }
}

/// Order of two comparable values, or `None` when Cypher leaves it undefined
/// (different types, or NaN).
pub(crate) fn compare(a: &Val, b: &Val) -> Option<Ordering> {
    match (a, b) {
        (Val::Int(a), Val::Int(b)) => Some(a.cmp(b)),
        (Val::Int(a), Val::Float(b)) => int_float(*a, *b),
        (Val::Float(a), Val::Int(b)) => int_float(*b, *a).map(Ordering::reverse),
        (Val::Float(a), Val::Float(b)) => a.partial_cmp(b),
        (Val::Str(a), Val::Str(b)) => Some(a.cmp(b)),
        (Val::Bool(a), Val::Bool(b)) => Some(a.cmp(b)),
        (Val::Node(a), Val::Node(b)) => Some(a.cmp(b)),
        (Val::Rel(a), Val::Rel(b)) => Some(a.cmp(b)),
        (Val::List(a), Val::List(b)) => {
            for (x, y) in a.iter().zip(b) {
                match compare(x, y)? {
                    Ordering::Equal => continue,
                    other => return Some(other),
                }
            }
            Some(a.len().cmp(&b.len()))
        }
        _ => None,
    }
}

/// Cypher equality: `Some(true/false)`, or `None` (unknown) when null is involved.
pub(crate) fn equals(a: &Val, b: &Val) -> Option<bool> {
    match (a, b) {
        (Val::Null, _) | (_, Val::Null) => None,
        (Val::List(a), Val::List(b)) => {
            if a.len() != b.len() {
                return Some(false);
            }
            let mut unknown = false;
            for (x, y) in a.iter().zip(b) {
                match equals(x, y) {
                    Some(false) => return Some(false),
                    None => unknown = true,
                    Some(true) => {}
                }
            }
            if unknown {
                None
            } else {
                Some(true)
            }
        }
        (Val::Map(a), Val::Map(b)) => {
            if a.len() != b.len() || a.keys().ne(b.keys()) {
                return Some(false);
            }
            let mut unknown = false;
            for (x, y) in a.values().zip(b.values()) {
                match equals(x, y) {
                    Some(false) => return Some(false),
                    None => unknown = true,
                    Some(true) => {}
                }
            }
            if unknown {
                None
            } else {
                Some(true)
            }
        }
        (Val::Path(a), Val::Path(b)) => Some(a == b),
        _ => Some(compare(a, b) == Some(Ordering::Equal)),
    }
}

// Compare an integer with an IEEE float without rounding the integer to f64.
// The cast is safe only inside these explicit i64 bounds; Rust truncates toward zero.
fn int_float(i: i64, f: f64) -> Option<Ordering> {
    if f.is_nan() {
        return None;
    }
    if f >= 9223372036854775808.0 {
        return Some(Ordering::Less);
    }
    if f < -9223372036854775808.0 {
        return Some(Ordering::Greater);
    }
    let truncated = f as i64;
    Some(i.cmp(&truncated).then_with(|| {
        if f.fract() > 0.0 {
            Ordering::Less
        } else if f.fract() < 0.0 {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    }))
}

/// `ORDER BY` order across types: map < node < relationship < list < path <
/// string < boolean < number, and null last ascending.
fn rank(v: &Val) -> u8 {
    match v {
        Val::Map(_) => 0,
        Val::Node(_) => 1,
        Val::Rel(_) => 2,
        Val::List(_) => 3,
        Val::Path(_) => 4,
        Val::Str(_) => 5,
        Val::Bool(_) => 6,
        Val::Int(_) | Val::Float(_) => 7,
        Val::Null => 8,
    }
}

/// Total order used by `ORDER BY`, `min` and `max`. NaN sorts above every number.
pub(crate) fn sort_cmp(a: &Val, b: &Val) -> Ordering {
    match (a, b) {
        (Val::Float(x), Val::Float(y)) if x.is_nan() || y.is_nan() => x.is_nan().cmp(&y.is_nan()),
        (Val::Float(x), Val::Int(_)) if x.is_nan() => Ordering::Greater,
        (Val::Int(_), Val::Float(y)) if y.is_nan() => Ordering::Less,
        (Val::List(x), Val::List(y)) => {
            for (p, q) in x.iter().zip(y) {
                match sort_cmp(p, q) {
                    Ordering::Equal => continue,
                    other => return other,
                }
            }
            x.len().cmp(&y.len())
        }
        (Val::Map(x), Val::Map(y)) => x.len().cmp(&y.len()).then_with(|| {
            for ((ka, va), (kb, vb)) in x.iter().zip(y) {
                let ord = ka.cmp(kb).then_with(|| sort_cmp(va, vb));
                if ord.is_ne() {
                    return ord;
                }
            }
            Ordering::Equal
        }),
        _ => compare(a, b).unwrap_or_else(|| rank(a).cmp(&rank(b))),
    }
}

/// Grouping/DISTINCT key: equal values (1 and 1.0 included) hash and compare
/// alike; null equals null here, unlike Cypher's `=`.
#[derive(Debug, Clone)]
pub(crate) struct Key(pub Vec<Val>);

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        self.0.len() == other.0.len() && self.0.iter().zip(&other.0).all(|(a, b)| same(a, b))
    }
}
impl Eq for Key {}
impl Hash for Key {
    fn hash<H: Hasher>(&self, state: &mut H) {
        for v in &self.0 {
            hash_val(v, state);
        }
    }
}

pub(crate) fn same(a: &Val, b: &Val) -> bool {
    match (a, b) {
        (Val::Null, Val::Null) => true,
        (Val::Float(x), Val::Float(y)) if x.is_nan() && y.is_nan() => true,
        (Val::List(x), Val::List(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q))
        }
        (Val::Map(x), Val::Map(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y)
                    .all(|((ka, va), (kb, vb))| ka == kb && same(va, vb))
        }
        (Val::Path(x), Val::Path(y)) => x == y,
        _ => compare(a, b) == Some(Ordering::Equal),
    }
}

fn hash_val<H: Hasher>(v: &Val, state: &mut H) {
    match v {
        Val::Null => 0u8.hash(state),
        Val::Bool(b) => (1u8, b).hash(state),
        // Integral floats hash as their integer so 1 and 1.0 group together.
        Val::Int(i) => (2u8, i).hash(state),
        Val::Float(f) if f.fract() == 0.0 && *f >= -9.2e18 && *f <= 9.2e18 => {
            (2u8, *f as i64).hash(state)
        }
        Val::Float(f) => (3u8, f.to_bits()).hash(state),
        Val::Str(s) => (4u8, s).hash(state),
        Val::List(items) => {
            (5u8, items.len()).hash(state);
            for item in items {
                hash_val(item, state);
            }
        }
        Val::Map(map) => {
            (6u8, map.len()).hash(state);
            for (k, item) in map {
                k.hash(state);
                hash_val(item, state);
            }
        }
        Val::Node(id) => (7u8, id.0).hash(state),
        Val::Rel(rel) => (8u8, rel).hash(state),
        Val::Path(path) => {
            9u8.hash(state);
            for id in &path.nodes {
                id.0.hash(state);
            }
            for rel in &path.rels {
                rel.hash(state);
            }
        }
    }
}
