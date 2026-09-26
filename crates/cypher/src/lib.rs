//! Cypher subset parser. See docs/superpowers/specs/2026-09-24-brainwire-design.md
//! (section 3.1) and docs/superpowers/plans/2026-09-26-brainwire-mvp.md (Task 2).
//!
//! `parser::parse` turns the small Cypher subset the Neo4j driver sends
//! (CREATE, MERGE, MATCH with one hop, SET, DETACH DELETE, UNWIND, RETURN
//! with count and LIMIT) into the AST in `ast`. The executor (Task 4) walks
//! that AST against one atomic store transaction.

pub mod ast;
pub mod lexer;
pub mod parser;

pub use parser::{parse, ParseError};

pub mod exec;
pub use exec::{execute, ExecError, NodeOut, Out, QueryOutput};
