//! AST for the supported Cypher (spec 3.1). Produced by `parser::parse`,
//! consumed by the planner and executor. Every type derives `Clone` and
//! `PartialEq` so the executor and tests can compare and copy values freely.

use brainwire_core::value::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    pub clauses: Vec<Clause>,
    /// `UNION` / `UNION ALL` branches after the first, in source order; `true` keeps duplicates.
    pub unions: Vec<(bool, Vec<Clause>)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Clause {
    Call {
        name: String,
        args: Vec<Expr>,
        yields: Vec<YieldItem>,
        where_: Option<Expr>,
    },
    Match {
        optional: bool,
        patterns: Vec<Pattern>,
        where_: Option<Expr>,
    },
    Merge {
        pattern: Pattern,
        on_create: Vec<SetItem>,
        on_match: Vec<SetItem>,
    },
    Create(Vec<Pattern>),
    Set(Vec<SetItem>),
    Remove(Vec<SetItem>),
    /// `DELETE` (relationships, or nodes without relationships) or `DETACH DELETE`.
    Delete {
        detach: bool,
        targets: Vec<Expr>,
    },
    Unwind {
        expr: Expr,
        var: String,
    },
    With(Projection),
    Return(Projection),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Projection {
    pub distinct: bool,
    /// `*`: keep every variable in scope, followed by `items`.
    pub star: bool,
    pub items: Vec<ReturnItem>,
    pub order: Vec<SortItem>,
    pub skip: Option<Expr>,
    pub limit: Option<Expr>,
    /// `WITH ... WHERE`; always `None` for `RETURN`.
    pub where_: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SortItem {
    pub expr: Expr,
    pub descending: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct YieldItem {
    pub field: String,
    pub alias: String,
}

/// `[p =] (start)-[rel]->(node)...`; a chain of zero or more hops.
#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    pub path: Option<String>,
    pub start: NodePat,
    pub hops: Vec<(RelPat, NodePat)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NodePat {
    pub var: Option<String>,
    /// Every label must hold; a node has exactly one label.
    pub labels: Vec<String>,
    pub props: Vec<(String, Expr)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RelPat {
    pub var: Option<String>,
    /// Alternatives (`:A|B`); empty matches any type.
    pub types: Vec<String>,
    pub direction: Direction,
    pub props: Vec<(String, Expr)>,
    /// Variable length `*min..max`; `None` is exactly one hop.
    pub length: Option<(u32, Option<u32>)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Out,
    In,
    Both,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SetItem {
    /// `x.prop = value` (`REMOVE x.prop` is the null assignment).
    Prop {
        var: String,
        prop: String,
        value: Expr,
    },
    /// `x = map`: replace every property.
    Replace { var: String, value: Expr },
    /// `x += map`: merge properties.
    Merge { var: String, value: Expr },
    /// `x:Label`: labels are fixed at creation; accepted only when already true.
    Labels { var: String, labels: Vec<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReturnItem {
    pub expr: Expr,
    pub alias: Option<String>,
    pub text: String,
}

impl ReturnItem {
    pub fn name(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.text)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Literal(Value),
    Param(String),
    Var(String),
    Prop(Box<Expr>, String),
    /// Function or aggregate call; `name` is lowercased.
    Call {
        name: String,
        distinct: bool,
        args: Vec<Expr>,
    },
    CountStar,
    List(Vec<Expr>),
    Map(Vec<(String, Expr)>),
    Cmp(Box<Expr>, CmpOp, Box<Expr>),
    Arith(Box<Expr>, ArithOp, Box<Expr>),
    Neg(Box<Expr>),
    /// `STARTS WITH`, `ENDS WITH`, `CONTAINS`.
    Text(Box<Expr>, TextOp, Box<Expr>),
    /// `expr IS NULL` (`false`) or `expr IS NOT NULL` (`true`); never null itself.
    IsNull(Box<Expr>, bool),
    /// `value IN list`: three-valued list membership.
    In(Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Xor(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    /// `n:Label[:Other]`.
    HasLabels(Box<Expr>, Vec<String>),
    Index(Box<Expr>, Box<Expr>),
    Slice(Box<Expr>, Option<Box<Expr>>, Option<Box<Expr>>),
    Case {
        operand: Option<Box<Expr>>,
        whens: Vec<(Expr, Expr)>,
        otherwise: Option<Box<Expr>>,
    },
    /// `[var IN list WHERE filter | map]`.
    ListComp {
        var: String,
        list: Box<Expr>,
        filter: Option<Box<Expr>>,
        map: Option<Box<Expr>>,
    },
    /// `all/any/none/single(var IN list WHERE predicate)`.
    Quantifier {
        kind: Quantifier,
        var: String,
        list: Box<Expr>,
        predicate: Box<Expr>,
    },
    /// `reduce(acc = init, var IN list | expr)`.
    Reduce {
        acc: String,
        init: Box<Expr>,
        var: String,
        list: Box<Expr>,
        expr: Box<Expr>,
    },
    /// `var {.a, b: expr, .*}`.
    MapProjection {
        var: String,
        items: Vec<MapItem>,
    },
    /// `exists(pattern)` or a bare pattern used as a predicate.
    Exists(Box<Pattern>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum MapItem {
    Prop(String),
    AllProps,
    Entry(String, Expr),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quantifier {
    All,
    Any,
    None,
    Single,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextOp {
    StartsWith,
    EndsWith,
    Contains,
}

/// Aggregate function names recognized in projections.
pub const AGGREGATES: [&str; 10] = [
    "count",
    "sum",
    "avg",
    "min",
    "max",
    "collect",
    "stdev",
    "stdevp",
    "percentilecont",
    "percentiledisc",
];

impl Expr {
    /// Whether this expression contains an aggregate (outside nested list scopes).
    pub fn has_aggregate(&self) -> bool {
        match self {
            Expr::CountStar => true,
            Expr::Call { name, args, .. } => {
                AGGREGATES.contains(&name.as_str()) || args.iter().any(Expr::has_aggregate)
            }
            other => other.children().into_iter().any(Expr::has_aggregate),
        }
    }

    /// Direct subexpressions (patterns inside `Exists` are not included).
    pub fn children(&self) -> Vec<&Expr> {
        match self {
            Expr::Prop(e, _) | Expr::Neg(e) | Expr::Not(e) | Expr::IsNull(e, _) => vec![&**e],
            Expr::HasLabels(e, _) => vec![&**e],
            Expr::Cmp(a, _, b)
            | Expr::Arith(a, _, b)
            | Expr::Text(a, _, b)
            | Expr::In(a, b)
            | Expr::And(a, b)
            | Expr::Or(a, b)
            | Expr::Xor(a, b)
            | Expr::Index(a, b) => vec![&**a, &**b],
            Expr::Slice(a, b, c) => {
                let mut out = vec![a.as_ref()];
                out.extend(b.as_deref());
                out.extend(c.as_deref());
                out
            }
            Expr::Call { args, .. } | Expr::List(args) => args.iter().collect(),
            Expr::Map(items) => items.iter().map(|(_, e)| e).collect(),
            Expr::Case {
                operand,
                whens,
                otherwise,
            } => {
                let mut out: Vec<&Expr> = operand.as_deref().into_iter().collect();
                for (when, then) in whens {
                    out.push(when);
                    out.push(then);
                }
                out.extend(otherwise.as_deref());
                out
            }
            Expr::ListComp {
                list, filter, map, ..
            } => {
                let mut out = vec![list.as_ref()];
                out.extend(filter.as_deref());
                out.extend(map.as_deref());
                out
            }
            Expr::Quantifier {
                list, predicate, ..
            } => vec![&**list, &**predicate],
            Expr::Reduce {
                init, list, expr, ..
            } => vec![&**init, &**list, &**expr],
            Expr::MapProjection { items, .. } => items
                .iter()
                .filter_map(|item| match item {
                    MapItem::Entry(_, e) => Some(e),
                    _ => None,
                })
                .collect(),
            Expr::Literal(_)
            | Expr::Param(_)
            | Expr::Var(_)
            | Expr::CountStar
            | Expr::Exists(_) => vec![],
        }
    }
}
