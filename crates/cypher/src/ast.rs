//! AST for the Cypher subset (spec 3.1). Produced by `parser::parse`,
//! consumed by the executor (Task 4). Every type here derives `Clone` and
//! `PartialEq` so the executor and tests can compare and copy values freely.

use brainwire_core::value::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    pub clauses: Vec<Clause>,
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
        patterns: Vec<Pattern>,
        where_: Option<Expr>,
    },
    Merge(Pattern),
    Create(Vec<Pattern>),
    Set(Vec<SetItem>),
    DetachDelete(Vec<String>),
    Unwind {
        expr: Expr,
        var: String,
    },
    Return {
        items: Vec<ReturnItem>,
        limit: Option<Expr>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct YieldItem {
    pub field: String,
    pub alias: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    pub start: NodePat,
    pub hop: Option<(RelPat, NodePat)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NodePat {
    pub var: Option<String>,
    pub label: Option<String>,
    pub props: Vec<(String, Expr)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RelPat {
    pub var: Option<String>,
    pub rel_type: String,
    pub direction: Direction,
    pub props: Vec<(String, Expr)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Direction {
    Out,
    In,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SetItem {
    pub var: String,
    pub prop: String,
    pub value: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReturnItem {
    pub expr: Expr,
    pub alias: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Literal(Value),
    Param(String),
    Var(String),
    Prop(Box<Expr>, String),
    CountStar,
    Count(Box<Expr>),
    Size(Box<Expr>),
    ToLower(Box<Expr>),
    ToUpper(Box<Expr>),
    /// Lazily choose the first non-null argument; an all-null list returns null.
    Coalesce(Vec<Expr>),
    List(Vec<Expr>),
    Map(Vec<(String, Expr)>),
    Cmp(Box<Expr>, CmpOp, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}
