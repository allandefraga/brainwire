//! Recursive descent parser for the supported Cypher (spec 3.1). Turns a
//! token stream from `lexer::tokenize` into the AST in `ast`. Keywords are
//! case-insensitive.
//!
//! ```text
//! query     := part (UNION [ALL] part)* [';']
//! part      := clause+
//! clause    := [OPTIONAL] MATCH pattern (',' pattern)* [WHERE expr]
//!            | MERGE pattern (ON (CREATE | MATCH) SET item (',' item)*)*
//!            | CREATE pattern (',' pattern)*
//!            | SET item (',' item)*       item := var '.' key '=' expr | var ['+']= expr | var labels
//!            | REMOVE var '.' key | var labels (',' ...)*
//!            | [DETACH] DELETE expr (',' expr)*
//!            | UNWIND expr AS var
//!            | WITH projection [WHERE expr] | RETURN projection
//!            | CALL name ('.' name)* '(' [expr (',' expr)*] ')' [YIELD name [AS name] (',' ...)* [WHERE expr]]
//!            | CREATE INDEX [name] [IF NOT EXISTS] FOR (v:Label) ON (v.key)
//!            | DROP INDEX name [IF EXISTS] | DROP INDEX ON :Label(key)
//! projection:= [DISTINCT] ('*' | item) (',' item)* [ORDER BY expr [ASC|DESC] (',' ...)*] [SKIP expr] [LIMIT expr]
//! pattern   := [var '='] node (rel node)*
//! node      := '(' [var] (':' label)* [map] ')'
//! rel       := ('-' | '<-') ['[' [var] [':' type ('|' [':'] type)*] ['*' [n] ['..' [m]]] [map] ']'] ('-' | '->')
//! expr      := OR > XOR > AND > NOT > comparison > + - > * / % > ^ > unary > postfix > atom
//! ```
//!
//! Nesting through parentheses, lists, maps and function arguments is capped
//! at [`MAX_NESTING`] levels and every completed expression subtree at
//! [`MAX_EXPR_DEPTH`], so a malicious query cannot recurse the parser (or the
//! derived `Drop`) into a stack overflow; the tokenizer itself never recurses
//! regardless of input length (see `lexer`).

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

/// Upper bound accepted for a variable-length relationship.
pub const MAX_VAR_LENGTH: u32 = 64;

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

/// Parses one Cypher query into a [`Query`]. Never panics: any input that is
/// not valid comes back as `Err` with the byte offset where parsing stopped.
pub fn parse(query: &str) -> Result<Query, ParseError> {
    let toks = tokenize(query)?;
    let mut p = Parser {
        src: query,
        toks,
        pos: 0,
        depth: 0,
    };
    let clauses = p.parse_part()?;
    let mut unions = Vec::new();
    while p.check_keyword("UNION") {
        p.advance();
        let all = p.check_keyword("ALL");
        if all {
            p.advance();
        }
        unions.push((all, p.parse_part()?));
    }
    if p.check(&Tok::Semicolon) {
        p.advance();
    }
    if !p.is_eof() {
        return Err(p.error_here("unexpected token after the query"));
    }
    Ok(Query { clauses, unions })
}

struct Parser<'a> {
    src: &'a str,
    toks: Vec<Spanned>,
    pos: usize,
    /// Bracket-only nesting depth (parens/list/map/function args), capped at
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

    fn keyword_at(&self, ahead: usize, kw: &str) -> bool {
        matches!(self.peek_at(ahead), Some(Tok::Ident(s)) if s.eq_ignore_ascii_case(kw))
    }

    fn eat_keyword(&mut self, kw: &str) -> bool {
        let found = self.check_keyword(kw);
        if found {
            self.advance();
        }
        found
    }

    fn expect_keyword(&mut self, kw: &str) -> Result<(), ParseError> {
        if self.eat_keyword(kw) {
            Ok(())
        } else {
            Err(self.error_here(format!("expected {kw}")))
        }
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

    fn wrap(
        &self,
        children: &[usize],
        offset: usize,
        expr: Expr,
    ) -> Result<ParsedExpr, ParseError> {
        let depth = self.checked_depth(children.iter().copied().max().unwrap_or(0), offset)?;
        Ok(ParsedExpr { expr, depth })
    }

    // -- clauses ------------------------------------------------------------

    fn parse_part(&mut self) -> Result<Vec<Clause>, ParseError> {
        let mut clauses = Vec::new();
        while !self.is_eof() && !self.check(&Tok::Semicolon) && !self.check_keyword("UNION") {
            clauses.push(self.parse_clause()?);
        }
        if clauses.is_empty() {
            return Err(self.error_here("empty query"));
        }
        Ok(clauses)
    }

    fn parse_clause(&mut self) -> Result<Clause, ParseError> {
        let kw = match self.peek() {
            Tok::Ident(s) => s.clone(),
            _ => return Err(self.error_here(EXPECTED_CLAUSE)),
        };
        match kw.to_ascii_uppercase().as_str() {
            "MATCH" => self.parse_match(false),
            "OPTIONAL" => {
                self.advance();
                if !self.check_keyword("MATCH") {
                    return Err(self.error_here("expected MATCH after OPTIONAL"));
                }
                self.parse_match(true)
            }
            "MERGE" => self.parse_merge(),
            "CREATE" if self.keyword_at(1, "INDEX") => self.parse_create_index(),
            "CREATE" => self.parse_create(),
            "DROP" => self.parse_drop_index(),
            "SET" => {
                self.advance();
                Ok(Clause::Set(self.parse_set_items()?))
            }
            "REMOVE" => self.parse_remove(),
            "DETACH" => {
                self.advance();
                if !self.check_keyword("DELETE") {
                    return Err(self.error_here("expected DELETE after DETACH"));
                }
                self.parse_delete(true)
            }
            "DELETE" => self.parse_delete(false),
            "UNWIND" => self.parse_unwind(),
            "WITH" => {
                self.advance();
                Ok(Clause::With(self.parse_projection(true)?))
            }
            "RETURN" => {
                self.advance();
                Ok(Clause::Return(self.parse_projection(false)?))
            }
            "CALL" => self.parse_call(),
            "FOREACH" | "LOAD" | "USE" | "EXPLAIN" | "PROFILE" => {
                Err(self.error_here(format!("not supported yet: {}", kw.to_ascii_uppercase())))
            }
            _ => Err(self.error_here(EXPECTED_CLAUSE)),
        }
    }

    fn parse_match(&mut self, optional: bool) -> Result<Clause, ParseError> {
        self.advance(); // MATCH
        let patterns = self.parse_patterns()?;
        let where_ = if self.eat_keyword("WHERE") {
            Some(self.parse_expr()?)
        } else {
            None
        };
        Ok(Clause::Match {
            optional,
            patterns,
            where_,
        })
    }

    fn parse_patterns(&mut self) -> Result<Vec<Pattern>, ParseError> {
        let mut patterns = vec![self.parse_pattern()?];
        while self.check(&Tok::Comma) {
            self.advance();
            patterns.push(self.parse_pattern()?);
        }
        Ok(patterns)
    }

    fn parse_call(&mut self) -> Result<Clause, ParseError> {
        self.advance();
        if self.check(&Tok::LBrace) {
            return Err(self.error_here("not supported yet: CALL subqueries"));
        }
        let mut parts = vec![self.expect_ident("a procedure name")?];
        while self.check(&Tok::Dot) {
            self.advance();
            parts.push(self.expect_ident("a procedure name segment")?);
        }
        let mut args = vec![];
        if self.check(&Tok::LParen) {
            self.advance();
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
        }
        let mut yields = vec![];
        let mut where_ = None;
        if self.eat_keyword("YIELD") {
            loop {
                let field = self.expect_ident("a YIELD field")?;
                let alias = if self.eat_keyword("AS") {
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
            if self.eat_keyword("WHERE") {
                where_ = Some(self.parse_expr()?);
            }
        }
        Ok(Clause::Call {
            name: parts.join("."),
            args,
            yields,
            where_,
        })
    }

    fn parse_merge(&mut self) -> Result<Clause, ParseError> {
        self.advance(); // MERGE
        let pattern = self.parse_pattern()?;
        let (mut on_create, mut on_match) = (vec![], vec![]);
        while self.check_keyword("ON") {
            self.advance();
            let create = if self.eat_keyword("CREATE") {
                true
            } else if self.eat_keyword("MATCH") {
                false
            } else {
                return Err(self.error_here("expected CREATE or MATCH after ON"));
            };
            self.expect_keyword("SET")?;
            let items = self.parse_set_items()?;
            if create {
                on_create.extend(items);
            } else {
                on_match.extend(items);
            }
        }
        Ok(Clause::Merge {
            pattern,
            on_create,
            on_match,
        })
    }

    fn parse_create(&mut self) -> Result<Clause, ParseError> {
        self.advance(); // CREATE
        Ok(Clause::Create(self.parse_patterns()?))
    }

    /// `CREATE INDEX [name] [IF NOT EXISTS] FOR (v:Label) ON (v.key)` or the
    /// legacy `CREATE INDEX ON :Label(key)`, as `CALL db.createIndex(label, key, name)`.
    fn parse_create_index(&mut self) -> Result<Clause, ParseError> {
        self.advance(); // CREATE
        self.advance(); // INDEX
        let mut if_not_exists = false;
        let (label, key, name) = if self.eat_keyword("ON") {
            let (label, key) = self.parse_legacy_index_target()?;
            (label, key, None)
        } else {
            let name = if !self.check_keyword("FOR") && !self.check_keyword("IF") {
                Some(self.expect_ident("an index name")?)
            } else {
                None
            };
            if self.eat_keyword("IF") {
                self.expect_keyword("NOT")?;
                self.expect_keyword("EXISTS")?;
                if_not_exists = true;
            }
            self.expect_keyword("FOR")?;
            self.expect(Tok::LParen, "'('")?;
            let var = self.expect_ident("a variable")?;
            self.expect(Tok::Colon, "':'")?;
            let label = self.expect_ident("a label")?;
            self.expect(Tok::RParen, "')'")?;
            self.expect_keyword("ON")?;
            self.expect(Tok::LParen, "'('")?;
            let owner = self.expect_ident("a variable")?;
            if owner != var {
                return Err(self.error_here("index property must belong to the pattern variable"));
            }
            self.expect(Tok::Dot, "'.'")?;
            let key = self.expect_ident("a property key")?;
            if self.check(&Tok::Comma) {
                return Err(self.error_here("not supported yet: composite indexes"));
            }
            self.expect(Tok::RParen, "')'")?;
            (label, key, name)
        };
        if self.check_keyword("OPTIONS") {
            return Err(self.error_here("not supported yet: index OPTIONS"));
        }
        let text = |value: String| Expr::Literal(Value::Str(value));
        Ok(Clause::Call {
            name: "db.createIndex".into(),
            args: vec![
                text(label),
                text(key),
                name.map(text).unwrap_or(Expr::Literal(Value::Null)),
                Expr::Literal(Value::Bool(if_not_exists)),
            ],
            yields: vec![],
            where_: None,
        })
    }

    fn parse_legacy_index_target(&mut self) -> Result<(String, String), ParseError> {
        self.expect(Tok::Colon, "':'")?;
        let label = self.expect_ident("a label")?;
        self.expect(Tok::LParen, "'('")?;
        let key = self.expect_ident("a property key")?;
        self.expect(Tok::RParen, "')'")?;
        Ok((label, key))
    }

    /// `DROP INDEX name [IF EXISTS]` or `DROP INDEX ON :Label(key)`, as `CALL db.dropIndex`.
    fn parse_drop_index(&mut self) -> Result<Clause, ParseError> {
        self.advance(); // DROP
        if !self.eat_keyword("INDEX") {
            return Err(self.error_here("not supported yet: DROP other than DROP INDEX"));
        }
        let text = |value: String| Expr::Literal(Value::Str(value));
        let args = if self.eat_keyword("ON") {
            let (label, key) = self.parse_legacy_index_target()?;
            vec![text(label), text(key)]
        } else {
            let name = self.expect_ident("an index name")?;
            let if_exists = self.eat_keyword("IF");
            if if_exists {
                self.expect_keyword("EXISTS")?;
            }
            vec![text(name), Expr::Literal(Value::Bool(if_exists))]
        };
        Ok(Clause::Call {
            name: "db.dropIndex".into(),
            args,
            yields: vec![],
            where_: None,
        })
    }

    fn parse_set_items(&mut self) -> Result<Vec<SetItem>, ParseError> {
        let mut items = vec![self.parse_set_item()?];
        while self.check(&Tok::Comma) {
            self.advance();
            items.push(self.parse_set_item()?);
        }
        Ok(items)
    }

    fn parse_set_item(&mut self) -> Result<SetItem, ParseError> {
        let var = self.expect_ident("a variable")?;
        match self.peek() {
            Tok::Dot => {
                self.advance();
                let prop = self.expect_ident("a property key")?;
                self.expect(Tok::Eq, "'='")?;
                let value = self.parse_expr()?;
                Ok(SetItem::Prop { var, prop, value })
            }
            Tok::Eq => {
                self.advance();
                Ok(SetItem::Replace {
                    var,
                    value: self.parse_expr()?,
                })
            }
            Tok::PlusEq => {
                self.advance();
                Ok(SetItem::Merge {
                    var,
                    value: self.parse_expr()?,
                })
            }
            Tok::Colon => Ok(SetItem::Labels {
                var,
                labels: self.parse_labels()?,
            }),
            other => {
                let found = tok_desc(other);
                Err(self.error_here(format!("expected '.', '=', '+=' or a label, found {found}")))
            }
        }
    }

    fn parse_labels(&mut self) -> Result<Vec<String>, ParseError> {
        let mut labels = vec![];
        while self.check(&Tok::Colon) {
            self.advance();
            labels.push(self.expect_ident("a label")?);
        }
        Ok(labels)
    }

    fn parse_remove(&mut self) -> Result<Clause, ParseError> {
        self.advance(); // REMOVE
        let mut items = vec![];
        loop {
            let var = self.expect_ident("a variable")?;
            if self.check(&Tok::Colon) {
                items.push(SetItem::Labels {
                    var,
                    labels: self.parse_labels()?,
                });
            } else {
                self.expect(Tok::Dot, "'.'")?;
                let prop = self.expect_ident("a property key")?;
                items.push(SetItem::Prop {
                    var,
                    prop,
                    value: Expr::Literal(Value::Null),
                });
            }
            if !self.check(&Tok::Comma) {
                break;
            }
            self.advance();
        }
        Ok(Clause::Remove(items))
    }

    fn parse_delete(&mut self, detach: bool) -> Result<Clause, ParseError> {
        self.advance(); // DELETE
        let mut targets = vec![self.parse_expr()?];
        while self.check(&Tok::Comma) {
            self.advance();
            targets.push(self.parse_expr()?);
        }
        Ok(Clause::Delete { detach, targets })
    }

    fn parse_unwind(&mut self) -> Result<Clause, ParseError> {
        self.advance(); // UNWIND
        let expr = self.parse_expr()?;
        if !self.eat_keyword("AS") {
            return Err(self.error_here("expected AS"));
        }
        let var = self.expect_ident("a variable")?;
        Ok(Clause::Unwind { expr, var })
    }

    fn parse_projection(&mut self, with: bool) -> Result<Projection, ParseError> {
        let distinct = self.eat_keyword("DISTINCT");
        let mut star = false;
        let mut items = vec![];
        if self.check(&Tok::Star) {
            self.advance();
            star = true;
            while self.check(&Tok::Comma) {
                self.advance();
                items.push(self.parse_return_item()?);
            }
        } else {
            items.push(self.parse_return_item()?);
            while self.check(&Tok::Comma) {
                self.advance();
                items.push(self.parse_return_item()?);
            }
        }
        let mut order = Vec::new();
        if self.eat_keyword("ORDER") {
            self.expect_keyword("BY")?;
            loop {
                let expr = self.parse_expr()?;
                let descending = if self.eat_keyword("DESC") || self.eat_keyword("DESCENDING") {
                    true
                } else {
                    let _ = self.eat_keyword("ASC") || self.eat_keyword("ASCENDING");
                    false
                };
                order.push(SortItem { expr, descending });
                if !self.check(&Tok::Comma) {
                    break;
                }
                self.advance();
            }
        }
        let skip = if self.eat_keyword("SKIP") || self.eat_keyword("OFFSET") {
            Some(self.parse_expr()?)
        } else {
            None
        };
        let limit = if self.eat_keyword("LIMIT") {
            Some(self.parse_expr()?)
        } else {
            None
        };
        if self.check_keyword("ORDER") || self.check_keyword("SKIP") {
            return Err(self.error_here("ORDER BY and SKIP must come before LIMIT"));
        }
        let where_ = if with && self.eat_keyword("WHERE") {
            Some(self.parse_expr()?)
        } else {
            None
        };
        Ok(Projection {
            distinct,
            star,
            items,
            order,
            skip,
            limit,
            where_,
        })
    }

    fn parse_return_item(&mut self) -> Result<ReturnItem, ParseError> {
        let start = self.current_span().0;
        let expr = self.parse_expr()?;
        let end = self.prev_span_end();
        let alias = if self.eat_keyword("AS") {
            Some(self.expect_ident("an alias")?)
        } else {
            None
        };
        let text = self.src[start..end].trim().to_string();
        Ok(ReturnItem { expr, alias, text })
    }

    // -- patterns -------------------------------------------------------

    fn parse_pattern(&mut self) -> Result<Pattern, ParseError> {
        let path = if matches!(self.peek(), Tok::Ident(_) | Tok::QuotedIdent(_))
            && self.peek_at(1) == Some(&Tok::Eq)
        {
            let name = self.expect_ident("a path variable")?;
            self.advance(); // '='
            Some(name)
        } else {
            None
        };
        if self.check_keyword("shortestPath") || self.check_keyword("allShortestPaths") {
            return Err(self.error_here(
                "not supported yet: shortestPath patterns; use CALL graph.shortestPath",
            ));
        }
        let start = self.parse_node()?;
        let mut hops = vec![];
        while self.at_rel_start() {
            let rel = self.parse_rel()?;
            let end = self.parse_node()?;
            hops.push((rel, end));
        }
        Ok(Pattern { path, start, hops })
    }

    fn at_rel_start(&self) -> bool {
        self.check(&Tok::Minus) || self.check(&Tok::LArrow)
    }

    fn parse_node(&mut self) -> Result<NodePat, ParseError> {
        self.with_nesting(|p| {
            p.expect(Tok::LParen, "'('")?;
            let var = p.try_var();
            let labels = p.parse_labels()?;
            let props = p.parse_optional_map()?;
            p.expect(Tok::RParen, "')'")?;
            Ok(NodePat { var, labels, props })
        })
    }

    fn parse_rel(&mut self) -> Result<RelPat, ParseError> {
        let incoming = self.check(&Tok::LArrow);
        self.advance(); // '-' or '<-'
        let mut rel = RelPat {
            var: None,
            types: vec![],
            direction: Direction::Both,
            props: vec![],
            length: None,
        };
        if self.check(&Tok::LBracket) {
            self.with_nesting(|p| {
                p.advance();
                rel.var = p.try_var();
                if p.check(&Tok::Colon) {
                    p.advance();
                    rel.types.push(p.expect_ident("a relationship type")?);
                    while p.check(&Tok::Pipe) {
                        p.advance();
                        if p.check(&Tok::Colon) {
                            p.advance();
                        }
                        rel.types.push(p.expect_ident("a relationship type")?);
                    }
                }
                if p.check(&Tok::Star) {
                    p.advance();
                    rel.length = Some(p.parse_length()?);
                }
                rel.props = p.parse_optional_map()?;
                p.expect(Tok::RBracket, "']'")
            })?;
        }
        // `->` closes an outgoing pattern; `-` an incoming or undirected one.
        let outgoing = match self.peek() {
            Tok::Arrow => true,
            Tok::Minus => false,
            other => {
                let found = tok_desc(other);
                return Err(self.error_here(format!("expected '-' or '->', found {found}")));
            }
        };
        self.advance();
        rel.direction = match (incoming, outgoing) {
            (false, true) => Direction::Out,
            (true, false) => Direction::In,
            _ => Direction::Both,
        };
        Ok(rel)
    }

    fn parse_length(&mut self) -> Result<(u32, Option<u32>), ParseError> {
        let bound = |p: &mut Self| -> Result<Option<u32>, ParseError> {
            match p.peek().clone() {
                Tok::Int(n) => {
                    p.advance();
                    u32::try_from(n)
                        .ok()
                        .filter(|n| *n <= MAX_VAR_LENGTH)
                        .map(Some)
                        .ok_or_else(|| {
                            p.error_here(format!(
                                "variable-length bounds must be between 0 and {MAX_VAR_LENGTH}"
                            ))
                        })
                }
                _ => Ok(None),
            }
        };
        let min = bound(self)?;
        if self.check(&Tok::DotDot) {
            self.advance();
            let max = bound(self)?;
            let min = min.unwrap_or(1);
            if max.is_some_and(|max| max < min) {
                return Err(self.error_here("variable-length maximum is below the minimum"));
            }
            Ok((min, max))
        } else {
            match min {
                Some(n) => Ok((n, Some(n))),
                None => Ok((1, None)),
            }
        }
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
        } else if self.check(&Tok::Dollar) {
            Err(self.error_here(
                "not supported yet: a parameter map in a pattern; use {key: $param.key}",
            ))
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
                    let key = p.expect_key()?;
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

    /// Map keys may also be quoted strings.
    fn expect_key(&mut self) -> Result<String, ParseError> {
        if let Tok::Str(key) = self.peek().clone() {
            self.advance();
            return Ok(key);
        }
        self.expect_ident("a property key")
    }

    // -- expressions ------------------------------------------------------

    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        Ok(self.parse_expr_depth()?.expr)
    }

    fn parse_expr_depth(&mut self) -> Result<ParsedExpr, ParseError> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<ParsedExpr, ParseError> {
        let mut left = self.parse_xor()?;
        while self.check_keyword("OR") {
            let offset = self.current_span().0;
            self.advance();
            let right = self.parse_xor()?;
            left = self.wrap(
                &[left.depth, right.depth],
                offset,
                Expr::Or(Box::new(left.expr), Box::new(right.expr)),
            )?;
        }
        Ok(left)
    }

    fn parse_xor(&mut self) -> Result<ParsedExpr, ParseError> {
        let mut left = self.parse_and()?;
        while self.check_keyword("XOR") {
            let offset = self.current_span().0;
            self.advance();
            let right = self.parse_and()?;
            left = self.wrap(
                &[left.depth, right.depth],
                offset,
                Expr::Xor(Box::new(left.expr), Box::new(right.expr)),
            )?;
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<ParsedExpr, ParseError> {
        let mut left = self.parse_not()?;
        while self.check_keyword("AND") {
            let offset = self.current_span().0;
            self.advance();
            let right = self.parse_not()?;
            left = self.wrap(
                &[left.depth, right.depth],
                offset,
                Expr::And(Box::new(left.expr), Box::new(right.expr)),
            )?;
        }
        Ok(left)
    }

    fn parse_not(&mut self) -> Result<ParsedExpr, ParseError> {
        if self.check_keyword("NOT") {
            let offset = self.current_span().0;
            self.advance();
            let inner = self.parse_not()?;
            self.wrap(&[inner.depth], offset, Expr::Not(Box::new(inner.expr)))
        } else {
            self.parse_cmp()
        }
    }

    fn parse_cmp(&mut self) -> Result<ParsedExpr, ParseError> {
        let mut left = self.parse_additive()?;
        loop {
            let offset = self.current_span().0;
            if self.check_keyword("IS") {
                self.advance();
                let negated = self.eat_keyword("NOT");
                if !self.eat_keyword("NULL") {
                    return Err(self.error_here("expected NULL after IS"));
                }
                left = self.wrap(
                    &[left.depth],
                    offset,
                    Expr::IsNull(Box::new(left.expr), negated),
                )?;
                continue;
            }
            if self.check_keyword("IN") {
                self.advance();
                let right = self.parse_additive()?;
                left = self.wrap(
                    &[left.depth, right.depth],
                    offset,
                    Expr::In(Box::new(left.expr), Box::new(right.expr)),
                )?;
                continue;
            }
            let text = if self.check_keyword("STARTS") && self.keyword_at(1, "WITH") {
                Some(TextOp::StartsWith)
            } else if self.check_keyword("ENDS") && self.keyword_at(1, "WITH") {
                Some(TextOp::EndsWith)
            } else if self.check_keyword("CONTAINS") {
                Some(TextOp::Contains)
            } else {
                None
            };
            if let Some(op) = text {
                self.advance();
                if op != TextOp::Contains {
                    self.advance(); // WITH
                }
                let right = self.parse_additive()?;
                left = self.wrap(
                    &[left.depth, right.depth],
                    offset,
                    Expr::Text(Box::new(left.expr), op, Box::new(right.expr)),
                )?;
                continue;
            }
            if self.check(&Tok::RegexMatch) {
                return Err(self.error_here("not supported yet: regular expressions (=~)"));
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
            self.advance();
            let right = self.parse_additive()?;
            left = self.wrap(
                &[left.depth, right.depth],
                offset,
                Expr::Cmp(Box::new(left.expr), op, Box::new(right.expr)),
            )?;
        }
    }

    fn parse_additive(&mut self) -> Result<ParsedExpr, ParseError> {
        let mut left = self.parse_multiplicative()?;
        loop {
            let op = match self.peek() {
                Tok::Plus => ArithOp::Add,
                Tok::Minus => ArithOp::Sub,
                _ => return Ok(left),
            };
            let offset = self.current_span().0;
            self.advance();
            let right = self.parse_multiplicative()?;
            left = self.wrap(
                &[left.depth, right.depth],
                offset,
                Expr::Arith(Box::new(left.expr), op, Box::new(right.expr)),
            )?;
        }
    }

    fn parse_multiplicative(&mut self) -> Result<ParsedExpr, ParseError> {
        let mut left = self.parse_power()?;
        loop {
            let op = match self.peek() {
                Tok::Star => ArithOp::Mul,
                Tok::Slash => ArithOp::Div,
                Tok::Percent => ArithOp::Mod,
                _ => return Ok(left),
            };
            let offset = self.current_span().0;
            self.advance();
            let right = self.parse_power()?;
            left = self.wrap(
                &[left.depth, right.depth],
                offset,
                Expr::Arith(Box::new(left.expr), op, Box::new(right.expr)),
            )?;
        }
    }

    fn parse_power(&mut self) -> Result<ParsedExpr, ParseError> {
        let mut left = self.parse_unary()?;
        while self.check(&Tok::Caret) {
            let offset = self.current_span().0;
            self.advance();
            let right = self.parse_unary()?;
            left = self.wrap(
                &[left.depth, right.depth],
                offset,
                Expr::Arith(Box::new(left.expr), ArithOp::Pow, Box::new(right.expr)),
            )?;
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<ParsedExpr, ParseError> {
        let start = self.current_span().0;
        match self.peek().clone() {
            Tok::Minus => {
                self.advance();
                match self.peek().clone() {
                    Tok::MinIntMagnitude => {
                        self.advance();
                        self.parse_postfix(ParsedExpr::leaf(Expr::Literal(Value::Int(i64::MIN))))
                    }
                    Tok::Int(n) => {
                        self.advance();
                        let neg = n
                            .checked_neg()
                            .ok_or_else(|| self.error_at(start, "integer literal out of range"))?;
                        self.parse_postfix(ParsedExpr::leaf(Expr::Literal(Value::Int(neg))))
                    }
                    Tok::Float(f) => {
                        self.advance();
                        self.parse_postfix(ParsedExpr::leaf(Expr::Literal(Value::Float(-f))))
                    }
                    _ => {
                        let inner = self.parse_unary()?;
                        self.wrap(&[inner.depth], start, Expr::Neg(Box::new(inner.expr)))
                    }
                }
            }
            Tok::Plus => {
                self.advance();
                self.parse_unary()
            }
            _ => {
                let atom = self.parse_atom()?;
                self.parse_postfix(atom)
            }
        }
    }

    fn parse_postfix(&mut self, mut base: ParsedExpr) -> Result<ParsedExpr, ParseError> {
        loop {
            let offset = self.current_span().0;
            match self.peek() {
                Tok::Dot => {
                    self.advance();
                    let key = self.expect_ident("a property key")?;
                    base =
                        self.wrap(&[base.depth], offset, Expr::Prop(Box::new(base.expr), key))?;
                }
                Tok::LBracket => {
                    base = self.with_nesting(|p| {
                        p.advance();
                        let start = if p.check(&Tok::DotDot) {
                            None
                        } else {
                            Some(p.parse_expr_depth()?)
                        };
                        if p.check(&Tok::DotDot) {
                            p.advance();
                            let end = if p.check(&Tok::RBracket) {
                                None
                            } else {
                                Some(p.parse_expr_depth()?)
                            };
                            p.expect(Tok::RBracket, "']'")?;
                            let depths = [
                                base.depth,
                                start.as_ref().map_or(0, |e| e.depth),
                                end.as_ref().map_or(0, |e| e.depth),
                            ];
                            p.wrap(
                                &depths,
                                offset,
                                Expr::Slice(
                                    Box::new(base.expr),
                                    start.map(|e| Box::new(e.expr)),
                                    end.map(|e| Box::new(e.expr)),
                                ),
                            )
                        } else {
                            let index = start.ok_or_else(|| p.error_here("expected an index"))?;
                            p.expect(Tok::RBracket, "']'")?;
                            p.wrap(
                                &[base.depth, index.depth],
                                offset,
                                Expr::Index(Box::new(base.expr), Box::new(index.expr)),
                            )
                        }
                    })?;
                }
                Tok::Colon if matches!(base.expr, Expr::Var(_)) => {
                    let labels = self.parse_labels()?;
                    base = self.wrap(
                        &[base.depth],
                        offset,
                        Expr::HasLabels(Box::new(base.expr), labels),
                    )?;
                }
                _ => return Ok(base),
            }
        }
    }

    fn parse_atom(&mut self) -> Result<ParsedExpr, ParseError> {
        let start = self.current_span().0;
        match self.peek().clone() {
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
                let name = match self.peek().clone() {
                    Tok::Int(n) => {
                        self.advance();
                        n.to_string()
                    }
                    _ => self.expect_ident("a parameter name")?,
                };
                Ok(ParsedExpr::leaf(Expr::Param(name)))
            }
            Tok::LBracket => self.parse_list_or_comprehension(start),
            Tok::LBrace => {
                let (items, child_depth) = self.parse_map()?;
                self.wrap(&[child_depth], start, Expr::Map(items))
            }
            Tok::LParen => {
                if let Some(pattern) = self.try_pattern_predicate()? {
                    return Ok(ParsedExpr::leaf(Expr::Exists(Box::new(pattern))));
                }
                self.with_nesting(|p| {
                    p.expect(Tok::LParen, "'('")?;
                    let inner = p.parse_expr_depth()?;
                    p.expect(Tok::RParen, "')'")?;
                    p.wrap(&[inner.depth], start, inner.expr)
                })
            }
            Tok::Ident(name) => self.parse_ident_atom(start, name),
            Tok::QuotedIdent(name) => {
                self.advance();
                Ok(ParsedExpr::leaf(Expr::Var(name)))
            }
            other => Err(self.error_at(
                start,
                format!("expected an expression, found {}", tok_desc(&other)),
            )),
        }
    }

    /// A parenthesized node followed by a relationship is a pattern predicate.
    fn try_pattern_predicate(&mut self) -> Result<Option<Pattern>, ParseError> {
        let saved = (self.pos, self.depth);
        let is_pattern = self.parse_node().is_ok() && self.at_rel_start();
        (self.pos, self.depth) = saved;
        if !is_pattern {
            return Ok(None);
        }
        match self.parse_pattern() {
            Ok(pattern) => Ok(Some(pattern)),
            // `(x) - 1` is arithmetic, not a relationship.
            Err(_) => {
                (self.pos, self.depth) = saved;
                Ok(None)
            }
        }
    }

    fn parse_list_or_comprehension(&mut self, start: usize) -> Result<ParsedExpr, ParseError> {
        let comprehension = matches!(self.peek_at(1), Some(Tok::Ident(_) | Tok::QuotedIdent(_)))
            && matches!(self.peek_at(2), Some(Tok::Ident(kw)) if kw.eq_ignore_ascii_case("IN"));
        self.with_nesting(|p| {
            p.expect(Tok::LBracket, "'['")?;
            if comprehension {
                let var = p.expect_ident("a variable")?;
                p.advance(); // IN
                let list = p.parse_expr_depth()?;
                let filter = if p.eat_keyword("WHERE") {
                    Some(p.parse_expr_depth()?)
                } else {
                    None
                };
                let map = if p.check(&Tok::Pipe) {
                    p.advance();
                    Some(p.parse_expr_depth()?)
                } else {
                    None
                };
                p.expect(Tok::RBracket, "']'")?;
                let depths = [
                    list.depth,
                    filter.as_ref().map_or(0, |e| e.depth),
                    map.as_ref().map_or(0, |e| e.depth),
                ];
                return p.wrap(
                    &depths,
                    start,
                    Expr::ListComp {
                        var,
                        list: Box::new(list.expr),
                        filter: filter.map(|e| Box::new(e.expr)),
                        map: map.map(|e| Box::new(e.expr)),
                    },
                );
            }
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
            p.wrap(&[max_depth], start, Expr::List(items))
        })
    }

    fn parse_ident_atom(&mut self, start: usize, name: String) -> Result<ParsedExpr, ParseError> {
        let upper = name.to_ascii_uppercase();
        match upper.as_str() {
            "TRUE" => {
                self.advance();
                return Ok(ParsedExpr::leaf(Expr::Literal(Value::Bool(true))));
            }
            "FALSE" => {
                self.advance();
                return Ok(ParsedExpr::leaf(Expr::Literal(Value::Bool(false))));
            }
            "NULL" => {
                self.advance();
                return Ok(ParsedExpr::leaf(Expr::Literal(Value::Null)));
            }
            "CASE" => return self.parse_case(start),
            _ => {}
        }
        // Namespaced function names: `a.b(...)`.
        let mut ahead = 1;
        let mut qualified = name.clone();
        while self.peek_at(ahead) == Some(&Tok::Dot)
            && matches!(self.peek_at(ahead + 1), Some(Tok::Ident(_)))
            && matches!(self.peek_at(ahead + 2), Some(Tok::Dot | Tok::LParen))
        {
            if let Some(Tok::Ident(part)) = self.peek_at(ahead + 1) {
                qualified.push('.');
                qualified.push_str(part);
            }
            ahead += 2;
        }
        if self.peek_at(ahead) == Some(&Tok::LParen) {
            for _ in 0..ahead {
                self.advance();
            }
            return self.parse_function(start, qualified.to_ascii_lowercase());
        }
        self.advance();
        if self.check(&Tok::LBrace) {
            return self.parse_map_projection(start, name);
        }
        Ok(ParsedExpr::leaf(Expr::Var(name)))
    }

    fn parse_case(&mut self, start: usize) -> Result<ParsedExpr, ParseError> {
        self.advance(); // CASE
        self.with_nesting(|p| {
            let operand = if p.check_keyword("WHEN") {
                None
            } else {
                Some(p.parse_expr_depth()?)
            };
            let mut depths = vec![operand.as_ref().map_or(0, |e| e.depth)];
            let mut whens = vec![];
            while p.eat_keyword("WHEN") {
                let when = p.parse_expr_depth()?;
                p.expect_keyword("THEN")?;
                let then = p.parse_expr_depth()?;
                depths.extend([when.depth, then.depth]);
                whens.push((when.expr, then.expr));
            }
            if whens.is_empty() {
                return Err(p.error_here("CASE needs at least one WHEN"));
            }
            let otherwise = if p.eat_keyword("ELSE") {
                let value = p.parse_expr_depth()?;
                depths.push(value.depth);
                Some(Box::new(value.expr))
            } else {
                None
            };
            p.expect_keyword("END")?;
            p.wrap(
                &depths,
                start,
                Expr::Case {
                    operand: operand.map(|e| Box::new(e.expr)),
                    whens,
                    otherwise,
                },
            )
        })
    }

    fn parse_map_projection(
        &mut self,
        start: usize,
        var: String,
    ) -> Result<ParsedExpr, ParseError> {
        self.with_nesting(|p| {
            p.expect(Tok::LBrace, "'{'")?;
            let mut items = vec![];
            let mut depth = 0;
            if !p.check(&Tok::RBrace) {
                loop {
                    if p.check(&Tok::Dot) {
                        p.advance();
                        if p.check(&Tok::Star) {
                            p.advance();
                            items.push(MapItem::AllProps);
                        } else {
                            items.push(MapItem::Prop(p.expect_ident("a property key")?));
                        }
                    } else {
                        let key = p.expect_key()?;
                        if p.check(&Tok::Colon) {
                            p.advance();
                            let value = p.parse_expr_depth()?;
                            depth = depth.max(value.depth);
                            items.push(MapItem::Entry(key, value.expr));
                        } else {
                            items.push(MapItem::Entry(key.clone(), Expr::Var(key)));
                        }
                    }
                    if !p.check(&Tok::Comma) {
                        break;
                    }
                    p.advance();
                }
            }
            p.expect(Tok::RBrace, "'}'")?;
            p.wrap(&[depth], start, Expr::MapProjection { var, items })
        })
    }

    fn parse_function(&mut self, start: usize, name: String) -> Result<ParsedExpr, ParseError> {
        self.with_nesting(|p| {
            p.expect(Tok::LParen, "'('")?;
            match name.as_str() {
                "count" if p.check(&Tok::Star) => {
                    p.advance();
                    p.expect(Tok::RParen, "')'")?;
                    return Ok(ParsedExpr::leaf(Expr::CountStar));
                }
                "exists" if p.check(&Tok::LParen) => {
                    if let Some(pattern) = p.try_pattern_predicate()? {
                        p.expect(Tok::RParen, "')'")?;
                        return Ok(ParsedExpr::leaf(Expr::Exists(Box::new(pattern))));
                    }
                }
                "all" | "any" | "none" | "single" => {
                    let kind = match name.as_str() {
                        "all" => Quantifier::All,
                        "any" => Quantifier::Any,
                        "none" => Quantifier::None,
                        _ => Quantifier::Single,
                    };
                    let var = p.expect_ident("a variable")?;
                    p.expect_keyword("IN")?;
                    let list = p.parse_expr_depth()?;
                    p.expect_keyword("WHERE")?;
                    let predicate = p.parse_expr_depth()?;
                    p.expect(Tok::RParen, "')'")?;
                    return p.wrap(
                        &[list.depth, predicate.depth],
                        start,
                        Expr::Quantifier {
                            kind,
                            var,
                            list: Box::new(list.expr),
                            predicate: Box::new(predicate.expr),
                        },
                    );
                }
                "reduce" => {
                    let acc = p.expect_ident("an accumulator")?;
                    p.expect(Tok::Eq, "'='")?;
                    let init = p.parse_expr_depth()?;
                    p.expect(Tok::Comma, "','")?;
                    let var = p.expect_ident("a variable")?;
                    p.expect_keyword("IN")?;
                    let list = p.parse_expr_depth()?;
                    p.expect(Tok::Pipe, "'|'")?;
                    let expr = p.parse_expr_depth()?;
                    p.expect(Tok::RParen, "')'")?;
                    return p.wrap(
                        &[init.depth, list.depth, expr.depth],
                        start,
                        Expr::Reduce {
                            acc,
                            init: Box::new(init.expr),
                            var,
                            list: Box::new(list.expr),
                            expr: Box::new(expr.expr),
                        },
                    );
                }
                _ => {}
            }
            let distinct = p.eat_keyword("DISTINCT");
            let mut args = Vec::new();
            let mut max_depth = 0;
            if !p.check(&Tok::RParen) {
                loop {
                    let argument = p.parse_expr_depth()?;
                    max_depth = max_depth.max(argument.depth);
                    args.push(argument.expr);
                    if !p.check(&Tok::Comma) {
                        break;
                    }
                    p.advance();
                }
            }
            p.expect(Tok::RParen, "')'")?;
            if !crate::functions::known(&name) {
                return Err(p.error_at(start, format!("not supported yet: function {name}")));
            }
            let (min, max) = crate::functions::arity(&name);
            if args.len() < min || args.len() > max {
                return Err(p.error_at(start, format!("wrong number of arguments for {name}")));
            }
            p.wrap(
                &[max_depth],
                start,
                Expr::Call {
                    name,
                    distinct,
                    args,
                },
            )
        })
    }
}

const EXPECTED_CLAUSE: &str =
    "expected a clause (MATCH, OPTIONAL MATCH, MERGE, CREATE, SET, REMOVE, DELETE, UNWIND, WITH, CALL or RETURN)";

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
        Tok::Plus => "'+'".to_string(),
        Tok::PlusEq => "'+='".to_string(),
        Tok::Slash => "'/'".to_string(),
        Tok::Percent => "'%'".to_string(),
        Tok::Caret => "'^'".to_string(),
        Tok::Pipe => "'|'".to_string(),
        Tok::DotDot => "'..'".to_string(),
        Tok::RegexMatch => "'=~'".to_string(),
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

    fn only_return(q: &Query) -> &Projection {
        match q.clauses.last() {
            Some(Clause::Return(p)) => p,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn create_with_properties() {
        let q = parse("CREATE (c:Customer {entity_id: 'c1', name: \"Ana\"})").unwrap();
        assert_eq!(
            q.clauses,
            vec![Clause::Create(vec![Pattern {
                path: None,
                start: NodePat {
                    var: Some("c".into()),
                    labels: vec!["Customer".into()],
                    props: vec![("entity_id".into(), s("c1")), ("name".into(), s("Ana"))]
                },
                hops: vec![],
            }])]
        );
    }

    #[test]
    fn merge_with_on_create_on_match_set_and_remove() {
        let q = parse(
            "merge (c:Customer {entity_id: $id}) on create set c.name = $name on match set c += $m, c.age = 30 remove c.old",
        )
        .unwrap();
        let Clause::Merge {
            on_create,
            on_match,
            ..
        } = &q.clauses[0]
        else {
            panic!()
        };
        assert_eq!(on_create.len(), 1);
        assert_eq!(
            on_match,
            &vec![
                SetItem::Merge {
                    var: "c".into(),
                    value: Expr::Param("m".into())
                },
                SetItem::Prop {
                    var: "c".into(),
                    prop: "age".into(),
                    value: Expr::Literal(Value::Int(30))
                },
            ]
        );
        assert_eq!(
            q.clauses[1],
            Clause::Remove(vec![SetItem::Prop {
                var: "c".into(),
                prop: "old".into(),
                value: Expr::Literal(Value::Null)
            }])
        );
    }

    #[test]
    fn multi_hop_variable_length_untyped_and_undirected_patterns() {
        let q =
            parse("MATCH p = (a:A)-[:X|:Y]->(b)<-[r*1..3]-(c)-[*]-(d)--(e)-->(f)<--(g) RETURN p")
                .unwrap();
        let Clause::Match { patterns, .. } = &q.clauses[0] else {
            panic!()
        };
        let pattern = &patterns[0];
        assert_eq!(pattern.path.as_deref(), Some("p"));
        let hops: Vec<_> = pattern
            .hops
            .iter()
            .map(|(r, _)| (r.types.clone(), r.direction, r.length))
            .collect();
        assert_eq!(
            hops,
            vec![
                (vec!["X".to_string(), "Y".to_string()], Direction::Out, None),
                (vec![], Direction::In, Some((1, Some(3)))),
                (vec![], Direction::Both, Some((1, None))),
                (vec![], Direction::Both, None),
                (vec![], Direction::Out, None),
                (vec![], Direction::In, None),
            ]
        );
        for (source, length) in [
            ("*2", (2, Some(2))),
            ("*..4", (1, Some(4))),
            ("*0..", (0, None)),
        ] {
            let q = parse(&format!("MATCH (a)-[{source}]->(b) RETURN b")).unwrap();
            let Clause::Match { patterns, .. } = &q.clauses[0] else {
                panic!()
            };
            assert_eq!(patterns[0].hops[0].0.length, Some(length), "{source}");
        }
        assert!(parse("MATCH (a)-[*3..1]->(b) RETURN b").is_err());
        assert!(parse("MATCH (a)-[*1..65]->(b) RETURN b").is_err());
    }

    #[test]
    fn optional_match_with_and_projection_clauses() {
        let q = parse(
            "MATCH (c:Customer) OPTIONAL MATCH (c)-[:BUYS]->(p) WITH DISTINCT c, count(p) AS n WHERE n > 1 RETURN c.name AS name, n ORDER BY n DESC, name SKIP 1 LIMIT 5",
        )
        .unwrap();
        assert!(matches!(q.clauses[1], Clause::Match { optional: true, .. }));
        let Clause::With(with) = &q.clauses[2] else {
            panic!()
        };
        assert!(with.distinct && with.where_.is_some());
        let ret = only_return(&q);
        assert_eq!(ret.order.len(), 2);
        assert!(ret.order[0].descending && !ret.order[1].descending);
        assert_eq!(ret.skip, Some(Expr::Literal(Value::Int(1))));
        assert!(parse("RETURN 1 LIMIT 1 SKIP 1").is_err());
        assert!(parse("RETURN 1 LIMIT 5 ORDER BY 1").is_err());
    }

    #[test]
    fn expressions_follow_cypher_precedence() {
        let q = parse("RETURN 1 + 2 * 3 ^ 2 - -4 % 3 AS x").unwrap();
        let Expr::Arith(_, ArithOp::Sub, right) = &only_return(&q).items[0].expr else {
            panic!()
        };
        assert!(matches!(right.as_ref(), Expr::Arith(_, ArithOp::Mod, _)));
        let q = parse("RETURN NOT a.x STARTS WITH 'b' OR c XOR d AND e IS NOT NULL").unwrap();
        assert!(matches!(only_return(&q).items[0].expr, Expr::Or(_, _)));
        let q = parse("RETURN n:Person, xs[0], xs[1..], xs[..-1], m['k'], 'a' + 1 <> 'b'").unwrap();
        let items: Vec<_> = only_return(&q).items.iter().map(|i| &i.expr).collect();
        assert!(
            matches!(items[0], Expr::HasLabels(_, labels) if labels == &vec!["Person".to_string()])
        );
        assert!(matches!(items[1], Expr::Index(_, _)));
        assert!(matches!(items[2], Expr::Slice(_, Some(_), None)));
        assert!(matches!(items[3], Expr::Slice(_, None, Some(_))));
        assert!(matches!(items[5], Expr::Cmp(_, CmpOp::Ne, _)));
    }

    #[test]
    fn case_comprehensions_quantifiers_reduce_and_map_projection() {
        let q = parse(
            "RETURN CASE x WHEN 1 THEN 'a' ELSE 'b' END, CASE WHEN x > 1 THEN 1 END, [y IN xs WHERE y > 1 | y * 2], [y IN xs], any(y IN xs WHERE y = 1), reduce(s = 0, y IN xs | s + y), n {.name, .*, total: 1, x}",
        )
        .unwrap();
        let items: Vec<_> = only_return(&q).items.iter().map(|i| &i.expr).collect();
        assert!(matches!(
            items[0],
            Expr::Case {
                operand: Some(_),
                ..
            }
        ));
        assert!(matches!(
            items[1],
            Expr::Case {
                operand: None,
                otherwise: None,
                ..
            }
        ));
        assert!(matches!(
            items[2],
            Expr::ListComp {
                filter: Some(_),
                map: Some(_),
                ..
            }
        ));
        assert!(matches!(
            items[3],
            Expr::ListComp {
                filter: None,
                map: None,
                ..
            }
        ));
        assert!(matches!(
            items[4],
            Expr::Quantifier {
                kind: Quantifier::Any,
                ..
            }
        ));
        assert!(matches!(items[5], Expr::Reduce { .. }));
        let Expr::MapProjection {
            var,
            items: entries,
        } = items[6]
        else {
            panic!()
        };
        assert_eq!(var, "n");
        assert_eq!(entries.len(), 4);
    }

    #[test]
    fn functions_aggregates_and_pattern_predicates() {
        let q = parse("MATCH (n) WHERE (n)-[:KNOWS]->(:Person) AND exists((n)<--()) RETURN count(DISTINCT n.x), labels(n), toLower(n.name)").unwrap();
        let Clause::Match {
            where_: Some(Expr::And(a, b)),
            ..
        } = &q.clauses[0]
        else {
            panic!()
        };
        assert!(matches!(a.as_ref(), Expr::Exists(_)));
        assert!(matches!(b.as_ref(), Expr::Exists(_)));
        let items = &only_return(&q).items;
        assert!(
            matches!(&items[0].expr, Expr::Call { name, distinct: true, .. } if name == "count")
        );
        assert!(matches!(&items[2].expr, Expr::Call { name, .. } if name == "tolower"));
        let e = parse("RETURN foo(1)").unwrap_err();
        assert!(
            e.message.starts_with("not supported yet: function foo"),
            "{}",
            e.message
        );
        assert!(parse("RETURN (1) - 1").is_ok());
    }

    #[test]
    fn ddl_maps_to_catalog_procedures() {
        for (source, args) in [
            ("CREATE INDEX FOR (n:L) ON (n.p)", 4),
            ("CREATE INDEX idx IF NOT EXISTS FOR (n:L) ON (n.p)", 4),
            ("CREATE INDEX ON :L(p)", 4),
        ] {
            let q = parse(source).unwrap();
            let Clause::Call {
                name, args: given, ..
            } = &q.clauses[0]
            else {
                panic!()
            };
            assert_eq!(
                (name.as_str(), given.len()),
                ("db.createIndex", args),
                "{source}"
            );
        }
        let q = parse("DROP INDEX idx IF EXISTS").unwrap();
        assert!(matches!(&q.clauses[0], Clause::Call { name, args, .. }
            if name == "db.dropIndex" && args[1] == Expr::Literal(Value::Bool(true))));
        let q = parse("CREATE INDEX idx IF NOT EXISTS FOR (n:L) ON (n.p)").unwrap();
        assert!(matches!(&q.clauses[0], Clause::Call { args, .. }
            if args[3] == Expr::Literal(Value::Bool(true))));
        assert!(parse("CREATE INDEX FOR (n:L) ON (m.p)").is_err());
    }

    #[test]
    fn union_call_and_unwind() {
        let q = parse(
            "MATCH (a:A) RETURN a.x AS x UNION ALL MATCH (b:B) RETURN b.x AS x UNION RETURN 1 AS x",
        )
        .unwrap();
        assert_eq!(
            q.unions.iter().map(|(all, _)| *all).collect::<Vec<_>>(),
            vec![true, false]
        );
        let q = parse("call graph.pagerank($config) yield node AS `nó`, score where score > 0 return `nó`, score limit 10;").unwrap();
        let Clause::Call {
            name,
            yields,
            where_,
            ..
        } = &q.clauses[0]
        else {
            panic!()
        };
        assert_eq!(name, "graph.pagerank");
        assert_eq!(yields[0].alias, "nó");
        assert!(where_.is_some());
        assert!(parse("CALL db.labels()").is_ok());
        let q = parse(
            "UNWIND $rows AS row MERGE (c:Customer {entity_id: row.id}) SET c.name = row.name",
        )
        .unwrap();
        assert!(matches!(&q.clauses[0], Clause::Unwind { var, .. } if var == "row"));
    }

    #[test]
    fn delete_forms_and_errors_have_offsets() {
        let q = parse("MATCH (a)-[r:LINK]->(b) DELETE r, a DETACH DELETE b").unwrap();
        assert!(
            matches!(&q.clauses[1], Clause::Delete { detach: false, targets } if targets.len() == 2)
        );
        assert!(matches!(&q.clauses[2], Clause::Delete { detach: true, .. }));
        assert!(parse("DELETE").is_err());
        let e = parse("MATCH (c:Customer RETURN c").unwrap_err();
        assert!(e.offset > 0);
        assert!(parse("").is_err());
        assert!(parse("MATCH (a) WHERE a.x =~ 'x' RETURN a")
            .unwrap_err()
            .message
            .contains("regular"));
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
        let Clause::Return(Projection { items, .. }) = &query.clauses[0] else {
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
            let Clause::Return(Projection { items, .. }) = &query.clauses[0] else {
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
        let Clause::Return(Projection { items, .. }) = &query.clauses[0] else {
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
        let q = parse("CREATE ()").unwrap();
        assert_eq!(
            q.clauses,
            vec![Clause::Create(vec![Pattern {
                path: None,
                start: NodePat {
                    var: None,
                    labels: vec![],
                    props: vec![]
                },
                hops: vec![],
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
            "MATCH p = (a:A)-[r:X|Y*1..3]-(b) WHERE a.x STARTS WITH 'q' WITH a, count(*) AS n ORDER BY n DESC LIMIT 3 RETURN a {.name, n: n}",
            "RETURN CASE WHEN 1 < 2 THEN [x IN range(1, 3) WHERE x > 1 | x * 2] ELSE reduce(s = 0, y IN [1] | s + y) END",
            "MERGE (n:L {entity_id: 'x'}) ON CREATE SET n.a = 1 ON MATCH SET n += {b: 2} REMOVE n.c",
            "CREATE INDEX idx IF NOT EXISTS FOR (n:L) ON (n.p)",
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
