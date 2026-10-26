//! Recursive descent parser for the Cypher subset (spec 3.1). Turns a token
//! stream from `lexer::tokenize` into the AST in `ast`. Grammar (keywords
//! case-insensitive):
//!
//! ```text
//! query     := clause+ [';']
//! clause    := MATCH pattern (',' pattern)* [WHERE expr]
//!            | MERGE pattern
//!            | CREATE pattern (',' pattern)*
//!            | SET var '.' key '=' expr (',' var '.' key '=' expr)*
//!            | DELETE var (',' var)*
//!            | DETACH DELETE var (',' var)*
//!            | UNWIND expr AS var
//!            | CALL name ('.' name)* '(' [expr (',' expr)*] ')'
//!              YIELD name [AS name] (',' name [AS name])* [WHERE expr]
//!            | RETURN item (',' item)* [LIMIT expr]
//! item      := expr [AS name]
//! pattern   := node [rel node]
//! node      := '(' [var] [':' label] [map] ')'
//! rel       := '-' '[' [var] ':' type [map] ']' '->'
//!            | '<-' '[' [var] ':' type [map] ']' '-'
//! map       := '{' key ':' expr (',' key ':' expr)* '}'
//! expr      := or;  or := and (OR and)*;  and := not (AND not)*;  not := [NOT] cmp
//! cmp       := primary [('=' | '<>' | '<' | '<=' | '>' | '>=') primary]
//! primary   := literal | '$' name | var ['.' key] | count '(' ('*' | expr) ')'
//!            | size '(' expr ')' | coalesce '(' expr (',' expr)* ')'
//!            | toLower '(' expr ')' | toUpper '(' expr ')'
//!            | '[' [expr (',' expr)*] ']' | map | '(' expr ')'
//! literal   := integer | float | string | TRUE | FALSE | NULL | '-' number
//! ```
//!
//! Anything else (`OPTIONAL MATCH`, `WITH`, `ORDER BY`, `SKIP`,
//! variable-length relationships, several labels, several hops, functions
//! other than `count`, `size`, `coalesce`, `toLower` and `toUpper`) is rejected with a
//! `ParseError` whose message starts with `not supported yet: `.
//!
//! Nesting through parentheses, lists and maps (including function
//! arguments) is capped at [`MAX_NESTING`] levels so a malicious, deeply
//! nested query cannot recurse the parser into a stack overflow; the
//! tokenizer itself never recurses regardless of input length (see
//! `lexer`).

use brainwire_core::value::Value;

use crate::ast::*;
use crate::lexer::{tokenize, Spanned, Tok};

/// Parentheses, lists, maps and function arguments may nest this
/// many levels deep before parsing gives up; see the module docs.
const MAX_NESTING: usize = 64;

/// Bound on the depth of each completed expression subtree. A leaf has depth
/// one; each operator, wrapper and parenthesized group adds one level.
/// This also bounds recursion when the derived `Drop` traverses the AST.
pub const MAX_EXPR_DEPTH: usize = 1024;

struct ParsedExpr {
    expr: Expr,
    depth: usize,
}

impl ParsedExpr {
    fn leaf(expr: Expr) -> Self {
        Self { expr, depth: 1 }
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{message} (at byte {offset})")]
pub struct ParseError {
    pub message: String,
    pub offset: usize,
}

/// Parses one Cypher query (the exact subset described in the module docs)
/// into a [`Query`]. Never panics: any input that is not valid in the
/// subset, including truncated or non-ASCII text, comes back as `Err` with
/// the byte offset where parsing stopped.
pub fn parse(query: &str) -> Result<Query, ParseError> {
    let toks = tokenize(query)?;
    let mut p = Parser {
        src: query,
        toks,
        pos: 0,
        depth: 0,
    };

    let mut clauses = Vec::new();
    loop {
        if p.is_eof() {
            break;
        }
        if p.check(&Tok::Semicolon) {
            p.advance();
            break;
        }
        clauses.push(p.parse_clause()?);
    }
    if !p.is_eof() {
        return Err(p.error_here("unexpected token after the query"));
    }
    if clauses.is_empty() {
        return Err(p.error_here("empty query"));
    }
    Ok(Query { clauses })
}

struct Parser<'a> {
    src: &'a str,
    toks: Vec<Spanned>,
    pos: usize,
    /// Bracket-only nesting depth (parens/list/map/count-arg), capped at
    /// `MAX_NESTING`. Bounds the parser's own recursion.
    depth: usize,
}

impl<'a> Parser<'a> {
    // -- token cursor -----------------------------------------------------

    fn peek(&self) -> &Tok {
        &self.toks[self.pos].tok
    }

    fn peek_at(&self, ahead: usize) -> Option<&Tok> {
        self.toks.get(self.pos + ahead).map(|s| &s.tok)
    }

    fn current_span(&self) -> (usize, usize) {
        let s = &self.toks[self.pos];
        (s.start, s.end)
    }

    /// End offset of the token just consumed; used to capture a return
    /// item's source text once its expression has been parsed.
    fn prev_span_end(&self) -> usize {
        if self.pos == 0 {
            0
        } else {
            self.toks[self.pos - 1].end
        }
    }

    fn is_eof(&self) -> bool {
        matches!(self.peek(), Tok::Eof)
    }

    fn advance(&mut self) {
        if self.pos + 1 < self.toks.len() {
            self.pos += 1;
        }
    }

    fn check(&self, want: &Tok) -> bool {
        self.peek() == want
    }

    fn check_keyword(&self, kw: &str) -> bool {
        matches!(self.peek(), Tok::Ident(s) if s.eq_ignore_ascii_case(kw))
    }

    fn error_here(&self, message: impl Into<String>) -> ParseError {
        ParseError {
            message: message.into(),
            offset: self.current_span().0,
        }
    }

    fn error_at(&self, offset: usize, message: impl Into<String>) -> ParseError {
        ParseError {
            message: message.into(),
            offset,
        }
    }

    fn expect(&mut self, want: Tok, what: &str) -> Result<(), ParseError> {
        if self.peek() == &want {
            self.advance();
            Ok(())
        } else {
            let found = tok_desc(self.peek());
            Err(self.error_here(format!("expected {what}, found {found}")))
        }
    }

    fn expect_ident(&mut self, what: &str) -> Result<String, ParseError> {
        match self.peek().clone() {
            Tok::Ident(s) | Tok::QuotedIdent(s) => {
                self.advance();
                Ok(s)
            }
            other => {
                let found = tok_desc(&other);
                Err(self.error_here(format!("expected {what}, found {found}")))
            }
        }
    }

    /// Runs `f` one nesting level deeper, failing before recursing further
    /// once [`MAX_NESTING`] levels are already open. Always restores the
    /// depth counter afterwards, whether `f` succeeded or not.
    fn with_nesting<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, ParseError>,
    ) -> Result<T, ParseError> {
        if self.depth >= MAX_NESTING {
            return Err(self.error_here("nested deeper than 64 levels"));
        }
        self.depth += 1;
        let result = f(self);
        self.depth -= 1;
        result
    }

    fn checked_depth(&self, child_depth: usize, offset: usize) -> Result<usize, ParseError> {
        if child_depth >= MAX_EXPR_DEPTH {
            return Err(self.error_at(
                offset,
                format!("expression nested deeper than {MAX_EXPR_DEPTH} levels"),
            ));
        }
        Ok(child_depth + 1)
    }

    // -- clauses ------------------------------------------------------------

    fn parse_clause(&mut self) -> Result<Clause, ParseError> {
        let kw = match self.peek() {
            Tok::Ident(s) => s.clone(),
            _ => return Err(self.error_here(EXPECTED_CLAUSE)),
        };
        match kw.to_ascii_uppercase().as_str() {
            "MATCH" => self.parse_match(),
            "MERGE" => self.parse_merge(),
            "CREATE" => self.parse_create(),
            "SET" => self.parse_set(),
            "DETACH" => self.parse_detach_delete(),
            "UNWIND" => self.parse_unwind(),
            "RETURN" => self.parse_return(),
            "OPTIONAL" => Err(self.error_here("not supported yet: OPTIONAL MATCH")),
            "WITH" => Err(self.error_here("not supported yet: WITH")),
            "CALL" => self.parse_call(),
            "DELETE" => self.parse_delete(),
            _ => Err(self.error_here(EXPECTED_CLAUSE)),
        }
    }

    fn parse_match(&mut self) -> Result<Clause, ParseError> {
        self.advance(); // MATCH
        let mut patterns = vec![self.parse_pattern()?];
        while self.check(&Tok::Comma) {
            self.advance();
            patterns.push(self.parse_pattern()?);
        }
        let where_ = if self.check_keyword("WHERE") {
            self.advance();
            Some(self.parse_expr()?)
        } else {
            None
        };
        Ok(Clause::Match { patterns, where_ })
    }

    fn parse_call(&mut self) -> Result<Clause, ParseError> {
        self.advance();
        let mut parts = vec![self.expect_ident("a procedure name")?];
        while self.check(&Tok::Dot) {
            self.advance();
            parts.push(self.expect_ident("a procedure name segment")?);
        }
        self.expect(Tok::LParen, "'('")?;
        let mut args = vec![];
        if !self.check(&Tok::RParen) {
            loop {
                args.push(self.parse_expr()?);
                if !self.check(&Tok::Comma) {
                    break;
                }
                self.advance();
            }
        }
        self.expect(Tok::RParen, "')'")?;
        if !self.check_keyword("YIELD") {
            return Err(self.error_here("expected YIELD with explicit procedure fields"));
        }
        self.advance();
        let mut yields = vec![];
        loop {
            let field = self.expect_ident("a YIELD field")?;
            let alias = if self.check_keyword("AS") {
                self.advance();
                self.expect_ident("a YIELD alias")?
            } else {
                field.clone()
            };
            yields.push(YieldItem { field, alias });
            if !self.check(&Tok::Comma) {
                break;
            }
            self.advance();
        }
        let where_ = if self.check_keyword("WHERE") {
            self.advance();
            Some(self.parse_expr()?)
        } else {
            None
        };
        Ok(Clause::Call {
            name: parts.join("."),
            args,
            yields,
            where_,
        })
    }

    fn parse_merge(&mut self) -> Result<Clause, ParseError> {
        self.advance(); // MERGE
        Ok(Clause::Merge(self.parse_pattern()?))
    }

    fn parse_create(&mut self) -> Result<Clause, ParseError> {
        self.advance(); // CREATE
        let mut patterns = vec![self.parse_pattern()?];
        while self.check(&Tok::Comma) {
            self.advance();
            patterns.push(self.parse_pattern()?);
        }
        Ok(Clause::Create(patterns))
    }

    fn parse_set(&mut self) -> Result<Clause, ParseError> {
        self.advance(); // SET
        let mut items = vec![self.parse_set_item()?];
        while self.check(&Tok::Comma) {
            self.advance();
            items.push(self.parse_set_item()?);
        }
        Ok(Clause::Set(items))
    }

    fn parse_set_item(&mut self) -> Result<SetItem, ParseError> {
        let var = self.expect_ident("a variable")?;
        self.expect(Tok::Dot, "'.'")?;
        let prop = self.expect_ident("a property key")?;
        self.expect(Tok::Eq, "'='")?;
        let value = self.parse_expr()?;
        Ok(SetItem { var, prop, value })
    }

    fn parse_delete(&mut self) -> Result<Clause, ParseError> {
        self.advance(); // DELETE
        let mut vars = vec![self.expect_ident("a relationship variable")?];
        while self.check(&Tok::Comma) {
            self.advance();
            vars.push(self.expect_ident("a relationship variable")?);
        }
        Ok(Clause::Delete(vars))
    }

    fn parse_detach_delete(&mut self) -> Result<Clause, ParseError> {
        self.advance(); // DETACH
        if !self.check_keyword("DELETE") {
            return Err(self.error_here("expected DELETE after DETACH"));
        }
        self.advance(); // DELETE
        let mut vars = vec![self.expect_ident("a variable")?];
        while self.check(&Tok::Comma) {
            self.advance();
            vars.push(self.expect_ident("a variable")?);
        }
        Ok(Clause::DetachDelete(vars))
    }

    fn parse_unwind(&mut self) -> Result<Clause, ParseError> {
        self.advance(); // UNWIND
        let expr = self.parse_expr()?;
        if !self.check_keyword("AS") {
            return Err(self.error_here("expected AS"));
        }
        self.advance(); // AS
        let var = self.expect_ident("a variable")?;
        Ok(Clause::Unwind { expr, var })
    }

    fn parse_return(&mut self) -> Result<Clause, ParseError> {
        self.advance(); // RETURN
        let mut items = vec![self.parse_return_item()?];
        while self.check(&Tok::Comma) {
            self.advance();
            items.push(self.parse_return_item()?);
        }
        let mut order = Vec::new();
        if self.check_keyword("ORDER") {
            self.advance();
            if !self.check_keyword("BY") {
                return Err(self.error_here("expected BY after ORDER"));
            }
            self.advance();
            loop {
                order.push(self.parse_sort_item(&items)?);
                if !self.check(&Tok::Comma) {
                    break;
                }
                self.advance();
            }
        }
        let skip = if self.check_keyword("SKIP") {
            self.advance();
            Some(self.parse_expr()?)
        } else {
            None
        };
        let limit = if self.check_keyword("LIMIT") {
            self.advance();
            Some(self.parse_expr()?)
        } else {
            None
        };
        self.reject_order_or_skip()?;
        Ok(Clause::Return {
            items,
            order,
            skip,
            limit,
        })
    }

    /// One `ORDER BY` key. It names a returned column, by alias or by the item's own
    /// text: sorting happens on the projected rows.
    fn parse_sort_item(&mut self, items: &[ReturnItem]) -> Result<(usize, bool), ParseError> {
        let start = self.current_span().0;
        let expr = self.parse_expr()?;
        let text = self.src[start..self.prev_span_end()].trim();
        let column = items.iter().position(|item| {
            matches!(&expr, Expr::Var(name) if item.alias.as_deref() == Some(name.as_str()))
                || (item.alias.is_none() && item.text == text)
        });
        let Some(column) = column else {
            return Err(self.error_at(
                start,
                "not supported yet: ORDER BY an expression that is not returned",
            ));
        };
        let descending = if self.check_keyword("DESC") || self.check_keyword("DESCENDING") {
            self.advance();
            true
        } else {
            if self.check_keyword("ASC") || self.check_keyword("ASCENDING") {
                self.advance();
            }
            false
        };
        Ok((column, descending))
    }

    fn reject_order_or_skip(&self) -> Result<(), ParseError> {
        if self.check_keyword("ORDER") {
            return Err(self.error_here("not supported yet: ORDER BY after SKIP or LIMIT"));
        }
        if self.check_keyword("SKIP") {
            return Err(self.error_here("not supported yet: SKIP after LIMIT"));
        }
        Ok(())
    }

    fn parse_return_item(&mut self) -> Result<ReturnItem, ParseError> {
        let start = self.current_span().0;
        let expr = self.parse_expr()?;
        let end = self.prev_span_end();
        let alias = if self.check_keyword("AS") {
            self.advance();
            Some(self.expect_ident("an alias")?)
        } else {
            None
        };
        let text = self.src[start..end].trim().to_string();
        Ok(ReturnItem { expr, alias, text })
    }

    // -- patterns -------------------------------------------------------

    fn parse_pattern(&mut self) -> Result<Pattern, ParseError> {
        let start = self.parse_node()?;
        let hop = if self.at_rel_start() {
            let rel = self.parse_rel()?;
            let end = self.parse_node()?;
            Some((rel, end))
        } else {
            None
        };
        if hop.is_some() && self.at_rel_start() {
            return Err(self.error_here("not supported yet: multiple hops in a pattern"));
        }
        Ok(Pattern { start, hop })
    }

    fn at_rel_start(&self) -> bool {
        self.check(&Tok::Minus) || self.check(&Tok::LArrow)
    }

    fn parse_node(&mut self) -> Result<NodePat, ParseError> {
        self.expect(Tok::LParen, "'('")?;
        let var = self.try_var();
        let label = if self.check(&Tok::Colon) {
            self.advance();
            let name = self.expect_ident("a label")?;
            if self.check(&Tok::Colon) {
                return Err(self.error_here("not supported yet: multiple labels on a node"));
            }
            Some(name)
        } else {
            None
        };
        let props = self.parse_optional_map()?;
        self.expect(Tok::RParen, "')'")?;
        Ok(NodePat { var, label, props })
    }

    fn parse_rel(&mut self) -> Result<RelPat, ParseError> {
        let direction = if self.check(&Tok::Minus) {
            self.advance();
            Direction::Out
        } else {
            self.advance(); // LArrow, guaranteed by at_rel_start
            Direction::In
        };
        self.expect(Tok::LBracket, "'['")?;
        let var = self.try_var();
        self.expect(Tok::Colon, "':'")?;
        let rel_type = self.expect_ident("a relationship type")?;
        if self.check(&Tok::Star) {
            return Err(self.error_here("not supported yet: variable-length relationship"));
        }
        let props = self.parse_optional_map()?;
        self.expect(Tok::RBracket, "']'")?;
        match direction {
            Direction::Out => self.expect(Tok::Arrow, "'->'")?,
            Direction::In => self.expect(Tok::Minus, "'-'")?,
        }
        Ok(RelPat {
            var,
            rel_type,
            direction,
            props,
        })
    }

    fn try_var(&mut self) -> Option<String> {
        match self.peek().clone() {
            Tok::Ident(name) | Tok::QuotedIdent(name) => {
                self.advance();
                Some(name)
            }
            _ => None,
        }
    }

    fn parse_optional_map(&mut self) -> Result<Vec<(String, Expr)>, ParseError> {
        if self.check(&Tok::LBrace) {
            Ok(self.parse_map()?.0)
        } else {
            Ok(Vec::new())
        }
    }

    fn parse_map(&mut self) -> Result<(Vec<(String, Expr)>, usize), ParseError> {
        self.with_nesting(|p| {
            p.expect(Tok::LBrace, "'{'")?;
            let mut items = Vec::new();
            let mut max_depth = 0;
            if !p.check(&Tok::RBrace) {
                loop {
                    let key = p.expect_ident("a property key")?;
                    p.expect(Tok::Colon, "':'")?;
                    let value = p.parse_expr_depth()?;
                    max_depth = max_depth.max(value.depth);
                    items.push((key, value.expr));
                    if p.check(&Tok::Comma) {
                        p.advance();
                        continue;
                    }
                    break;
                }
            }
            p.expect(Tok::RBrace, "'}'")?;
            Ok((items, max_depth))
        })
    }

    fn parse_list(&mut self) -> Result<(Vec<Expr>, usize), ParseError> {
        self.with_nesting(|p| {
            p.expect(Tok::LBracket, "'['")?;
            let mut items = Vec::new();
            let mut max_depth = 0;
            if !p.check(&Tok::RBracket) {
                loop {
                    let value = p.parse_expr_depth()?;
                    max_depth = max_depth.max(value.depth);
                    items.push(value.expr);
                    if p.check(&Tok::Comma) {
                        p.advance();
                        continue;
                    }
                    break;
                }
            }
            p.expect(Tok::RBracket, "']'")?;
            Ok((items, max_depth))
        })
    }

    // -- expressions ------------------------------------------------------

    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        Ok(self.parse_expr_depth()?.expr)
    }

    fn parse_expr_depth(&mut self) -> Result<ParsedExpr, ParseError> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<ParsedExpr, ParseError> {
        let mut left = self.parse_and()?;
        while self.check_keyword("OR") {
            let offset = self.current_span().0;
            self.advance();
            let right = self.parse_and()?;
            let depth = self.checked_depth(left.depth.max(right.depth), offset)?;
            left = ParsedExpr {
                expr: Expr::Or(Box::new(left.expr), Box::new(right.expr)),
                depth,
            };
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<ParsedExpr, ParseError> {
        let mut left = self.parse_not()?;
        while self.check_keyword("AND") {
            let offset = self.current_span().0;
            self.advance();
            let right = self.parse_not()?;
            let depth = self.checked_depth(left.depth.max(right.depth), offset)?;
            left = ParsedExpr {
                expr: Expr::And(Box::new(left.expr), Box::new(right.expr)),
                depth,
            };
        }
        Ok(left)
    }

    fn parse_not(&mut self) -> Result<ParsedExpr, ParseError> {
        if self.check_keyword("NOT") {
            let offset = self.current_span().0;
            self.advance();
            let inner = self.parse_cmp()?;
            let depth = self.checked_depth(inner.depth, offset)?;
            Ok(ParsedExpr {
                expr: Expr::Not(Box::new(inner.expr)),
                depth,
            })
        } else {
            self.parse_cmp()
        }
    }

    fn parse_cmp(&mut self) -> Result<ParsedExpr, ParseError> {
        let left = self.parse_primary()?;
        if self.check_keyword("IS") {
            let offset = self.current_span().0;
            self.advance();
            let negated = self.check_keyword("NOT");
            if negated {
                self.advance();
            }
            if !self.check_keyword("NULL") {
                return Err(self.error_here("expected NULL after IS"));
            }
            self.advance();
            let depth = self.checked_depth(left.depth, offset)?;
            return Ok(ParsedExpr {
                expr: Expr::IsNull(Box::new(left.expr), negated),
                depth,
            });
        }
        if self.check_keyword("IN") {
            let offset = self.current_span().0;
            self.advance();
            let right = self.parse_primary()?;
            let depth = self.checked_depth(left.depth.max(right.depth), offset)?;
            return Ok(ParsedExpr {
                expr: Expr::In(Box::new(left.expr), Box::new(right.expr)),
                depth,
            });
        }
        let op = match self.peek() {
            Tok::Eq => CmpOp::Eq,
            Tok::Ne => CmpOp::Ne,
            Tok::Lt => CmpOp::Lt,
            Tok::Le => CmpOp::Le,
            Tok::Gt => CmpOp::Gt,
            Tok::Ge => CmpOp::Ge,
            _ => return Ok(left),
        };
        let offset = self.current_span().0;
        self.advance();
        let right = self.parse_primary()?;
        let depth = self.checked_depth(left.depth.max(right.depth), offset)?;
        Ok(ParsedExpr {
            expr: Expr::Cmp(Box::new(left.expr), op, Box::new(right.expr)),
            depth,
        })
    }

    fn parse_primary(&mut self) -> Result<ParsedExpr, ParseError> {
        let start = self.current_span().0;
        match self.peek().clone() {
            Tok::Minus => {
                self.advance();
                match self.peek().clone() {
                    Tok::MinIntMagnitude => {
                        self.advance();
                        Ok(ParsedExpr::leaf(Expr::Literal(Value::Int(i64::MIN))))
                    }
                    Tok::Int(n) => {
                        self.advance();
                        let neg = n
                            .checked_neg()
                            .ok_or_else(|| self.error_at(start, "integer literal out of range"))?;
                        Ok(ParsedExpr::leaf(Expr::Literal(Value::Int(neg))))
                    }
                    Tok::Float(f) => {
                        self.advance();
                        Ok(ParsedExpr::leaf(Expr::Literal(Value::Float(-f))))
                    }
                    _ => Err(self.error_at(start, "expected a number after '-'")),
                }
            }
            Tok::MinIntMagnitude => Err(self.error_at(start, "integer literal out of range")),
            Tok::Int(n) => {
                self.advance();
                Ok(ParsedExpr::leaf(Expr::Literal(Value::Int(n))))
            }
            Tok::Float(f) => {
                self.advance();
                Ok(ParsedExpr::leaf(Expr::Literal(Value::Float(f))))
            }
            Tok::Str(s) => {
                self.advance();
                Ok(ParsedExpr::leaf(Expr::Literal(Value::Str(s))))
            }
            Tok::Dollar => {
                self.advance();
                let name = self.expect_ident("a parameter name")?;
                Ok(ParsedExpr::leaf(Expr::Param(name)))
            }
            Tok::LBracket => {
                let (items, child_depth) = self.parse_list()?;
                let depth = self.checked_depth(child_depth, start)?;
                Ok(ParsedExpr {
                    expr: Expr::List(items),
                    depth,
                })
            }
            Tok::LBrace => {
                let (items, child_depth) = self.parse_map()?;
                let depth = self.checked_depth(child_depth, start)?;
                Ok(ParsedExpr {
                    expr: Expr::Map(items),
                    depth,
                })
            }
            Tok::LParen => self.with_nesting(|p| {
                p.expect(Tok::LParen, "'('")?;
                let inner = p.parse_expr_depth()?;
                p.expect(Tok::RParen, "')'")?;
                let depth = p.checked_depth(inner.depth, start)?;
                Ok(ParsedExpr {
                    expr: inner.expr,
                    depth,
                })
            }),
            Tok::Ident(name) => self.parse_ident_primary(start, name),
            Tok::QuotedIdent(name) => {
                self.advance();
                self.parse_optional_prop(ParsedExpr::leaf(Expr::Var(name)))
            }
            other => Err(self.error_at(
                start,
                format!("expected an expression, found {}", tok_desc(&other)),
            )),
        }
    }

    fn parse_ident_primary(
        &mut self,
        start: usize,
        name: String,
    ) -> Result<ParsedExpr, ParseError> {
        if name.eq_ignore_ascii_case("true") {
            self.advance();
            return Ok(ParsedExpr::leaf(Expr::Literal(Value::Bool(true))));
        }
        if name.eq_ignore_ascii_case("false") {
            self.advance();
            return Ok(ParsedExpr::leaf(Expr::Literal(Value::Bool(false))));
        }
        if name.eq_ignore_ascii_case("null") {
            self.advance();
            return Ok(ParsedExpr::leaf(Expr::Literal(Value::Null)));
        }
        if self.peek_at(1) == Some(&Tok::LParen) {
            let size = name.eq_ignore_ascii_case("size");
            let coalesce = name.eq_ignore_ascii_case("coalesce");
            let lower = name.eq_ignore_ascii_case("toLower");
            let upper = name.eq_ignore_ascii_case("toUpper");
            if !size && !coalesce && !lower && !upper && !name.eq_ignore_ascii_case("count") {
                return Err(self.error_at(start, format!("not supported yet: function {name}")));
            }
            self.advance(); // function name
            return self.with_nesting(|p| {
                p.expect(Tok::LParen, "'('")?;
                if coalesce {
                    let mut args = Vec::new();
                    let mut max_depth = 0;
                    loop {
                        let argument = p.parse_expr_depth()?;
                        max_depth = max_depth.max(argument.depth);
                        args.push(argument.expr);
                        if !p.check(&Tok::Comma) {
                            break;
                        }
                        p.advance();
                    }
                    p.expect(Tok::RParen, "')'")?;
                    return Ok(ParsedExpr {
                        expr: Expr::Coalesce(args),
                        depth: p.checked_depth(max_depth, start)?,
                    });
                }
                if !size && !lower && !upper && p.check(&Tok::Star) {
                    p.advance();
                    p.expect(Tok::RParen, "')'")?;
                    return Ok(ParsedExpr::leaf(Expr::CountStar));
                }
                let inner = p.parse_expr_depth()?;
                p.expect(Tok::RParen, "')'")?;
                let depth = p.checked_depth(inner.depth, start)?;
                Ok(ParsedExpr {
                    expr: if size {
                        Expr::Size(Box::new(inner.expr))
                    } else if lower {
                        Expr::ToLower(Box::new(inner.expr))
                    } else if upper {
                        Expr::ToUpper(Box::new(inner.expr))
                    } else {
                        Expr::Count(Box::new(inner.expr))
                    },
                    depth,
                })
            });
        }
        self.advance();
        self.parse_optional_prop(ParsedExpr::leaf(Expr::Var(name)))
    }

    fn parse_optional_prop(&mut self, base: ParsedExpr) -> Result<ParsedExpr, ParseError> {
        if self.check(&Tok::Dot) {
            let offset = self.current_span().0;
            self.advance();
            let key = self.expect_ident("a property key")?;
            let depth = self.checked_depth(base.depth, offset)?;
            Ok(ParsedExpr {
                expr: Expr::Prop(Box::new(base.expr), key),
                depth,
            })
        } else {
            Ok(base)
        }
    }
}

const EXPECTED_CLAUSE: &str =
    "expected a clause (MATCH, MERGE, CREATE, SET, DETACH DELETE, UNWIND, CALL or RETURN)";

fn tok_desc(t: &Tok) -> String {
    match t {
        Tok::LParen => "'('".to_string(),
        Tok::RParen => "')'".to_string(),
        Tok::LBracket => "'['".to_string(),
        Tok::RBracket => "']'".to_string(),
        Tok::LBrace => "'{'".to_string(),
        Tok::RBrace => "'}'".to_string(),
        Tok::Colon => "':'".to_string(),
        Tok::Comma => "','".to_string(),
        Tok::Dot => "'.'".to_string(),
        Tok::Dollar => "'$'".to_string(),
        Tok::Semicolon => "';'".to_string(),
        Tok::Star => "'*'".to_string(),
        Tok::Minus => "'-'".to_string(),
        Tok::Eq => "'='".to_string(),
        Tok::Ne => "'<>'".to_string(),
        Tok::Lt => "'<'".to_string(),
        Tok::Le => "'<='".to_string(),
        Tok::Gt => "'>'".to_string(),
        Tok::Ge => "'>='".to_string(),
        Tok::Arrow => "'->'".to_string(),
        Tok::LArrow => "'<-'".to_string(),
        Tok::Ident(s) => format!("identifier '{s}'"),
        Tok::QuotedIdent(s) => format!("identifier `{s}`"),
        Tok::Int(n) => format!("integer {n}"),
        Tok::MinIntMagnitude => "integer magnitude 9223372036854775808".into(),
        Tok::Float(f) => format!("float {f}"),
        Tok::Str(s) => format!("string {s:?}"),
        Tok::Eof => "end of input".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brainwire_core::value::Value;

    fn s(v: &str) -> Expr {
        Expr::Literal(Value::Str(v.into()))
    }

    #[test]
    fn create_with_properties() {
        let q = parse("CREATE (c:Customer {entity_id: 'c1', name: \"Ana\"})").unwrap();
        assert_eq!(
            q.clauses,
            vec![Clause::Create(vec![Pattern {
                start: NodePat {
                    var: Some("c".into()),
                    label: Some("Customer".into()),
                    props: vec![("entity_id".into(), s("c1")), ("name".into(), s("Ana"))]
                },
                hop: None,
            }])]
        );
    }

    #[test]
    fn merge_then_set_with_parameters() {
        let q =
            parse("merge (c:Customer {entity_id: $id}) set c.name = $name, c.age = 30").unwrap();
        assert_eq!(
            q.clauses[1],
            Clause::Set(vec![
                SetItem {
                    var: "c".into(),
                    prop: "name".into(),
                    value: Expr::Param("name".into())
                },
                SetItem {
                    var: "c".into(),
                    prop: "age".into(),
                    value: Expr::Literal(Value::Int(30))
                },
            ])
        );
    }

    #[test]
    fn one_hop_match_with_where_and_return_names() {
        let q = parse(
            "MATCH (c:Customer)-[:WORKS_AT]->(e:Company) WHERE c.name = 'Ana' AND e.name <> 'X' RETURN c.name, e.name AS company LIMIT 5",
        )
        .unwrap();
        let Clause::Match { patterns, where_ } = &q.clauses[0] else {
            panic!()
        };
        let (rel, end) = patterns[0].hop.clone().unwrap();
        assert_eq!(rel.rel_type, "WORKS_AT");
        assert_eq!(rel.direction, Direction::Out);
        assert_eq!(end.label.as_deref(), Some("Company"));
        assert!(matches!(where_, Some(Expr::And(_, _))));
        let Clause::Return { items, limit, .. } = &q.clauses[1] else {
            panic!()
        };
        assert_eq!(items[0].text, "c.name");
        assert_eq!(items[1].alias.as_deref(), Some("company"));
        assert_eq!(limit, &Some(Expr::Literal(Value::Int(5))));
    }

    #[test]
    fn incoming_relationship_and_count() {
        let q = parse("MATCH (e:Company)<-[r:WORKS_AT]-(c:Customer) RETURN count(*)").unwrap();
        let Clause::Match { patterns, .. } = &q.clauses[0] else {
            panic!()
        };
        assert_eq!(patterns[0].hop.as_ref().unwrap().0.direction, Direction::In);
        let Clause::Return { items, .. } = &q.clauses[1] else {
            panic!()
        };
        assert_eq!(items[0].expr, Expr::CountStar);
        assert_eq!(items[0].text, "count(*)");
    }

    #[test]
    fn unwind_rows_merge() {
        let q = parse(
            "UNWIND $rows AS row MERGE (c:Customer {entity_id: row.id}) SET c.name = row.name",
        )
        .unwrap();
        assert_eq!(
            q.clauses[0],
            Clause::Unwind {
                expr: Expr::Param("rows".into()),
                var: "row".into()
            }
        );
    }

    #[test]
    fn detach_delete_and_literals() {
        let q = parse("MATCH (c:Customer {entity_id: 'c1'}) DETACH DELETE c").unwrap();
        assert_eq!(q.clauses[1], Clause::DetachDelete(vec!["c".into()]));
        let q = parse("RETURN -1.5, true, null, [1, 'a'], {k: 2}").unwrap();
        let Clause::Return { items, .. } = &q.clauses[0] else {
            panic!()
        };
        assert_eq!(items[0].expr, Expr::Literal(Value::Float(-1.5)));
        assert_eq!(items[2].expr, Expr::Literal(Value::Null));
    }

    #[test]
    fn match_with_multiple_comma_separated_patterns() {
        let q = parse("MATCH (a:A), (b:B)-[:T]->(c:C) RETURN a, b, c").unwrap();
        let Clause::Match { patterns, .. } = &q.clauses[0] else {
            panic!()
        };
        assert_eq!(patterns.len(), 2);
        assert!(patterns[0].hop.is_none());
        assert!(patterns[1].hop.is_some());
    }

    #[test]
    fn create_with_multiple_comma_separated_patterns() {
        let q = parse("CREATE (a:A {entity_id: '1'}), (b:B {entity_id: '2'})").unwrap();
        let Clause::Create(patterns) = &q.clauses[0] else {
            panic!()
        };
        assert_eq!(patterns.len(), 2);
    }

    #[test]
    fn detach_delete_multiple_variables() {
        let q = parse("MATCH (a), (b) DETACH DELETE a, b, c").unwrap();
        assert_eq!(
            q.clauses[1],
            Clause::DetachDelete(vec!["a".into(), "b".into(), "c".into()])
        );
    }

    #[test]
    fn errors_have_offsets_and_unsupported_is_named() {
        let e = parse("MATCH (c:Customer RETURN c").unwrap_err();
        assert!(e.offset > 0);
        let e = parse("MATCH (c:Customer) WITH c RETURN c").unwrap_err();
        assert!(
            e.message.starts_with("not supported yet: WITH"),
            "{}",
            e.message
        );
        let e = parse("MATCH (a)-[:T*1..3]->(b) RETURN a").unwrap_err();
        assert!(e.message.starts_with("not supported yet:"), "{}", e.message);
        assert!(parse("").is_err());
    }

    // -- additional coverage: every named unsupported construct ------------
    // The brief's grammar names ten constructs that must fail with a message
    // starting "not supported yet: "; the brief's own tests exercise WITH and
    // variable-length relationships. These tests cover the rest.

    #[test]
    fn optional_match_is_unsupported() {
        let e = parse("OPTIONAL MATCH (c:Customer) RETURN c").unwrap_err();
        assert!(
            e.message.starts_with("not supported yet: OPTIONAL MATCH"),
            "{}",
            e.message
        );
    }

    #[test]
    fn order_by_names_a_returned_column() {
        let q = parse("MATCH (c:Customer) RETURN c.name AS n, c.entity_id ORDER BY n DESC, c.entity_id SKIP 2 LIMIT 3").unwrap();
        let Clause::Return {
            order, skip, limit, ..
        } = &q.clauses[1]
        else {
            panic!()
        };
        assert_eq!(order, &vec![(0, true), (1, false)]);
        assert!(skip.is_some() && limit.is_some());
        let e = parse("MATCH (c:Customer) RETURN c.name ORDER BY c.segment").unwrap_err();
        assert!(
            e.message.starts_with("not supported yet: ORDER BY"),
            "{}",
            e.message
        );
    }

    #[test]
    fn skip_after_limit_is_unsupported() {
        let e = parse("RETURN 1 LIMIT 1 SKIP 1").unwrap_err();
        assert!(
            e.message.starts_with("not supported yet: SKIP"),
            "{}",
            e.message
        );
    }

    #[test]
    fn order_by_after_limit_is_also_unsupported() {
        let e = parse("RETURN 1 LIMIT 5 ORDER BY 1").unwrap_err();
        assert!(
            e.message.starts_with("not supported yet: ORDER BY"),
            "{}",
            e.message
        );
    }

    #[test]
    fn multiple_labels_is_unsupported() {
        let e = parse("MATCH (c:Customer:Person) RETURN c").unwrap_err();
        assert!(e.message.starts_with("not supported yet:"), "{}", e.message);
    }

    #[test]
    fn multiple_hops_is_unsupported() {
        let e = parse("MATCH (a)-[:T]->(b)-[:U]->(c) RETURN a").unwrap_err();
        assert!(e.message.starts_with("not supported yet:"), "{}", e.message);
    }

    #[test]
    fn function_other_than_count_is_unsupported() {
        let e = parse("RETURN foo(1)").unwrap_err();
        assert!(
            e.message.starts_with("not supported yet: function foo"),
            "{}",
            e.message
        );
    }

    #[test]
    fn call_requires_explicit_yield() {
        let e = parse("CALL db.labels()").unwrap_err();
        assert!(e.message.starts_with("expected YIELD"), "{}", e.message);
    }

    #[test]
    fn call_parses_names_parameters_aliases_filters_and_utf8() {
        let query = parse("call graph.pagerank($config) yield node AS `nó`, score where score > 0 return `nó`, score limit 10;").unwrap();
        let Clause::Call {
            name,
            args,
            yields,
            where_,
        } = &query.clauses[0]
        else {
            panic!("expected call")
        };
        assert_eq!(name, "graph.pagerank");
        assert_eq!(args, &[Expr::Param("config".into())]);
        assert_eq!(yields[0].alias, "nó");
        assert!(where_.is_some());
        for invalid in [
            "CALL",
            "CALL graph.",
            "CALL graph.project(",
            "CALL graph.project({})",
            "CALL graph.project() YIELD",
            "CALL graph.project() YIELD *",
            "CALL graph.project() YIELD node AS",
        ] {
            assert!(parse(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn relationship_delete_parses_multiple_variables() {
        let q = parse("MATCH (a:Page)-[r:LINK]->(b:Page) DELETE r, s").unwrap();
        assert_eq!(q.clauses[1], Clause::Delete(vec!["r".into(), "s".into()]));
        assert!(parse("DELETE").is_err());
    }

    // -- nesting limit -------------------------------------------------------

    #[test]
    fn nesting_up_to_64_levels_is_ok_and_65_is_an_error() {
        let ok = format!("RETURN {}1{}", "(".repeat(64), ")".repeat(64));
        assert!(parse(&ok).is_ok(), "64 levels of nesting should parse");

        let too_deep = format!("RETURN {}1{}", "(".repeat(65), ")".repeat(65));
        let e = parse(&too_deep).unwrap_err();
        assert_eq!(e.message, "nested deeper than 64 levels");

        // The same cap applies to a `count(...)` argument, not just bare parens.
        let nested_in_count = format!("RETURN count({}1{})", "(".repeat(65), ")".repeat(65));
        let e = parse(&nested_in_count).unwrap_err();
        assert_eq!(e.message, "nested deeper than 64 levels");
    }

    #[test]
    fn nesting_shares_one_counter_across_parens_lists_and_maps() {
        // 30 parens + 30 lists, alternating = 60 levels total: under the cap.
        let under = format!("RETURN {}1{}", "([".repeat(30), "])".repeat(30));
        assert!(
            parse(&under).is_ok(),
            "60 alternating levels should still parse"
        );

        // 34 parens + 34 lists, alternating = 68 levels total: over the cap.
        // If parens and lists were tracked by separate counters this would
        // wrongly succeed (34 of each, neither alone over 64).
        let over = format!("RETURN {}1{}", "([".repeat(34), "])".repeat(34));
        let e = parse(&over).unwrap_err();
        assert_eq!(e.message, "nested deeper than 64 levels");
    }

    // -- expression depth budget (fix round 1: unbounded AND/OR chains) -----
    //
    // parse_and/parse_or build a left-associated Expr::And/Or chain in a
    // flat loop, so the *parser* never recurses deeply for a long chain -
    // but the *resulting Expr tree* does grow one level per extra term, and
    // the compiler-derived Drop/Clone/Debug/PartialEq for that tree recurses
    // once per level. A long enough chain overflows the stack when the
    // value is dropped, not while it is being parsed. MAX_EXPR_DEPTH bounds
    // the tree's depth directly: every extra AND/OR term and every bracket
    // level (parens/list/map/count(..)) draws from this one budget.

    fn and_chain(extra_terms: usize) -> String {
        format!("1{}", " AND 1".repeat(extra_terms))
    }

    fn or_chain(extra_terms: usize) -> String {
        format!("1{}", " OR 1".repeat(extra_terms))
    }

    #[test]
    fn and_chain_up_to_1023_extra_terms_is_ok_and_1024_is_an_error() {
        assert!(
            parse(&format!("RETURN {}", and_chain(1023))).is_ok(),
            "a leaf and 1023 AND nodes have depth 1024"
        );

        let e = parse(&format!("RETURN {}", and_chain(1024))).unwrap_err();
        assert_eq!(e.message, "expression nested deeper than 1024 levels");
    }

    #[test]
    fn or_chain_up_to_1023_extra_terms_is_ok_and_1024_is_an_error() {
        assert!(
            parse(&format!("RETURN {}", or_chain(1023))).is_ok(),
            "a leaf and 1023 OR nodes have depth 1024"
        );

        let e = parse(&format!("RETURN {}", or_chain(1024))).unwrap_err();
        assert_eq!(e.message, "expression nested deeper than 1024 levels");
    }

    #[test]
    fn and_subtree_cannot_hide_under_or_chain() {
        let prefix = format!("RETURN {}", and_chain(1023));
        assert!(parse(&prefix).is_ok());
        let query = format!("{prefix} OR 1");
        let e = parse(&query).unwrap_err();
        assert_eq!(e.message, "expression nested deeper than 1024 levels");
        assert_eq!(e.offset, prefix.len() + 1);
    }

    #[test]
    fn mixed_operators_and_wrappers_share_the_actual_subtree_depth() {
        let accepted = format!("RETURN count([{{k: NOT a.b = 1 AND {}}}])", and_chain(1016));
        assert!(parse(&accepted).is_ok());
        let rejected = format!("RETURN count([{{k: NOT a.b = 1 AND {}}}])", and_chain(1017));
        let e = parse(&rejected).unwrap_err();
        assert_eq!(e.message, "expression nested deeper than 1024 levels");

        // A completed deep term must count when a later OR wraps it.
        let query = format!("RETURN {} OR 1", and_chain(1023));
        let e = parse(&query).unwrap_err();
        assert_eq!(e.message, "expression nested deeper than 1024 levels");
    }

    #[test]
    fn independent_siblings_do_not_accumulate_depth() {
        let chain = and_chain(1023);
        let query = format!("RETURN {chain}, {chain}");
        assert!(parse(&query).is_ok());
    }

    #[test]
    fn very_long_and_or_chains_are_refused_quickly_without_crashing() {
        // 200,000 extra terms is far beyond MAX_EXPR_DEPTH. This must return
        // Err (never panic, never hang) almost immediately - the parser has
        // to give up within about 1024 iterations, long before it would ever
        // build anything close to a 200,000-deep tree, since the crash the
        // reviewer found happens when a deep-enough tree is *dropped*, not
        // while it is being assembled.
        let e = parse(&format!("RETURN {}", and_chain(200_000))).unwrap_err();
        assert_eq!(e.message, "expression nested deeper than 1024 levels");

        let e = parse(&format!("RETURN {}", or_chain(200_000))).unwrap_err();
        assert_eq!(e.message, "expression nested deeper than 1024 levels");

        // The first OR must reject the completed AND subtree before a long
        // continuation can grow a deeper tree.
        let mixed = format!("RETURN {}{}", and_chain(1023), " OR 1".repeat(200_000));
        let e = parse(&mixed).unwrap_err();
        assert_eq!(e.message, "expression nested deeper than 1024 levels");
    }

    #[test]
    fn and_chain_inside_parentheses_shares_the_expression_depth_budget() {
        // 64 levels of parens is fine on its own (a separate, smaller cap -
        // see the bracket-nesting tests above). Wrapped around a chain long
        // enough that 64 + the chain's extra terms exceeds 1024, the whole
        // thing must be refused: parens and AND terms draw from the same
        // 1024-level budget, not two independent ones.
        let opens = "(".repeat(64);
        let closes = ")".repeat(64);

        let short = format!("RETURN {opens}{}{closes}", and_chain(100));
        assert!(
            parse(&short).is_ok(),
            "64 parens + 100 extra AND terms (164 total) should still parse"
        );

        let long = format!("RETURN {opens}{}{closes}", and_chain(1000));
        let e = parse(&long).unwrap_err();
        assert_eq!(
            e.message, "expression nested deeper than 1024 levels",
            "64 parens + 1000 extra AND terms (1064 total) must be refused"
        );
    }

    // -- lexer-level errors, surfaced through parse -------------------------

    #[test]
    fn unterminated_string_is_an_error() {
        assert!(parse("RETURN 'abc").is_err());
        assert!(parse("RETURN \"abc").is_err());
    }

    #[test]
    fn invalid_escape_sequence_is_an_error() {
        let e = parse("RETURN 'a\\qb'").unwrap_err();
        assert!(e.message.contains("escape"), "{}", e.message);
    }

    #[test]
    fn unterminated_backtick_identifier_is_an_error() {
        assert!(parse("MATCH (`abc) RETURN 1").is_err());
    }

    #[test]
    fn unexpected_character_is_an_error() {
        assert!(parse("RETURN @").is_err());
    }

    #[test]
    fn float_literal_boundaries() {
        for source in ["RETURN 1e309", "RETURN -1e309", "RETURN 1.9e308"] {
            assert!(parse(source).is_err(), "{source}");
        }
        let query = parse("RETURN 1e308, -1e308, 0.0, 1e-308").unwrap();
        let Clause::Return { items, .. } = &query.clauses[0] else {
            panic!()
        };
        for item in items {
            let Expr::Literal(Value::Float(f)) = item.expr else {
                panic!()
            };
            assert!(f.is_finite());
        }
    }
    #[test]
    fn signed_integer_literal_boundaries() {
        for source in [
            "RETURN -9223372036854775808",
            "RETURN - 9223372036854775808",
            "RETURN -0009223372036854775808",
        ] {
            let query = parse(source).unwrap();
            let Clause::Return { items, .. } = &query.clauses[0] else {
                panic!()
            };
            assert_eq!(items[0].expr, Expr::Literal(Value::Int(i64::MIN)));
        }
        for source in [
            "RETURN 9223372036854775808",
            "RETURN 0009223372036854775808",
            "RETURN -9223372036854775809",
            "RETURN - 0009223372036854775809",
        ] {
            assert!(parse(source).is_err(), "{source}");
        }
        let query = parse("RETURN 9223372036854775807").unwrap();
        let Clause::Return { items, .. } = &query.clauses[0] else {
            panic!()
        };
        assert_eq!(items[0].expr, Expr::Literal(Value::Int(i64::MAX)));
    }

    #[test]
    fn integer_literal_out_of_range_is_an_error() {
        assert!(parse("RETURN 99999999999999999999").is_err());
    }

    #[test]
    fn backtick_quoted_identifier_is_never_a_keyword() {
        let q = parse("MATCH (`match`:Customer) RETURN `match`.name").unwrap();
        let Clause::Match { patterns, .. } = &q.clauses[0] else {
            panic!()
        };
        assert_eq!(patterns[0].start.var.as_deref(), Some("match"));
    }

    // -- generic syntax errors, not "not supported yet" ---------------------

    #[test]
    fn garbage_top_level_keyword_is_an_error() {
        assert!(parse("FOOBAR").is_err());
    }

    #[test]
    fn detach_without_delete_is_an_error() {
        assert!(parse("MATCH (c) DETACH RETURN c").is_err());
    }

    #[test]
    fn unwind_without_as_is_an_error() {
        assert!(parse("UNWIND [1, 2] row").is_err());
    }

    #[test]
    fn semicolon_followed_by_more_tokens_is_an_error() {
        // query := clause+ [';'] allows exactly one trailing ';', not one per clause.
        assert!(parse("RETURN 1; RETURN 2").is_err());
    }

    #[test]
    fn trailing_semicolon_is_allowed() {
        assert!(parse("RETURN 1;").is_ok());
    }

    #[test]
    fn empty_and_labelless_node_patterns_parse_structurally() {
        // The parser only enforces the grammar; "needs a label" etc. is the
        // executor's job (Task 4), so these are valid syntax here.
        let q = parse("CREATE ()").unwrap();
        assert_eq!(
            q.clauses,
            vec![Clause::Create(vec![Pattern {
                start: NodePat {
                    var: None,
                    label: None,
                    props: vec![]
                },
                hop: None,
            }])]
        );
        assert!(parse("MATCH (c) RETURN c").is_ok());
    }

    // -- fuzz-style: never panic, on any prefix of any query used above -----

    #[test]
    fn never_panics_on_any_prefix_of_every_test_query() {
        let queries = [
            "CREATE (c:Customer {entity_id: 'c1', name: \"Ana\"})",
            "merge (c:Customer {entity_id: $id}) set c.name = $name, c.age = 30",
            "MATCH (c:Customer)-[:WORKS_AT]->(e:Company) WHERE c.name = 'Ana' AND e.name <> 'X' RETURN c.name, e.name AS company LIMIT 5",
            "MATCH (e:Company)<-[r:WORKS_AT]-(c:Customer) RETURN count(*)",
            "UNWIND $rows AS row MERGE (c:Customer {entity_id: row.id}) SET c.name = row.name",
            "MATCH (c:Customer {entity_id: 'c1'}) DETACH DELETE c",
            "RETURN -1.5, true, null, [1, 'a'], {k: 2}",
            "MATCH (a)-[:T*1..3]->(b) RETURN a",
            "OPTIONAL MATCH (c:Customer) RETURN c",
            "MATCH (c:Cliente {entity_id: 'joão'}) RETURN c.nome, `estado-civil`",
            "RETURN 'unterminated",
            "RETURN `unterminated",
            "RETURN 'bad \\escape'",
        ];
        for q in queries {
            let mut boundaries: Vec<usize> = q.char_indices().map(|(i, _)| i).collect();
            boundaries.push(q.len());
            for end in boundaries {
                let prefix = &q[..end];
                let outcome = std::panic::catch_unwind(|| parse(prefix));
                assert!(
                    outcome.is_ok(),
                    "parse panicked on prefix {prefix:?} of {q:?}"
                );
            }
        }
    }
}
