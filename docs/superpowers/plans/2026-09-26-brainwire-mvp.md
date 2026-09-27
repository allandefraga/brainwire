# Brainwire MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `brainwire serve` reads a config file, and the official Neo4j Python
driver creates and reads nodes and edges with a small Cypher subset; the data
lives in RocksDB and survives a restart.

**Architecture:** Two new crates. `brainwire-cypher` parses a Cypher subset
into an AST and executes it against the store's graph operations, one storage
transaction per query. `brainwire-server` owns the `brainwire` binary: the
config file (memory, listen address, data directory), the CLI commands, the
registry of databases and the adapter that plugs the executor into the Bolt
server's `QueryEngine` trait.

**Tech Stack:** Rust (stable), the workspace crates (`brainwire-core`,
`brainwire-store`, `brainwire-bolt`), `clap` 4 (derive), `toml` 0.8, `serde`;
Python 3 with the `neo4j` driver (5.28.6 in the session scratchpad venv, and
6.3.0 in `client-core/.venv`) for the end-to-end check.

**Spec:** [2026-09-24-brainwire-design.md](../specs/2026-09-24-brainwire-design.md),
sections 1.5 (memory), 2.1 (schema from the ontology), 3.1 and 3.3 (query and
Bolt).

**Depends on:** the storage plan through Task 9 (graph operations) and the Bolt
plan, both merged into `feat/brainwire`.

**How this plan is written (ruling of 2026-09-26):** Allan asked for an MVP as
soon as possible, so the tasks give exact interfaces, exact behaviour and the
acceptance tests as code, but not the full implementation. Implementers run on
a mid or top tier model and write the code from these contracts; the task
reviews stay as they are.

## Current handoff (2026-09-26)

Brainwire now lives at `/Users/fraga/Documents/brainwire`, a standalone sibling
of the platform. Run the commands below from this repository.
Tasks 1 through 4 are complete and reviewed. Task 4 added the atomic executor
in `5ace18d`, with numeric literal boundary corrections in `51aca5a`. Its
focused verification passed 61 tests, clippy with warnings denied and formatting.
The storage foundation includes the five-cycle process-crash recovery test.
The latest consolidated verification and limitations are recorded in `JOURNAL.md`.
The separate local Cypher command and TUI iteration are tracked in
[the executor/backoffice plan](2026-09-26-brainwire-executor-backoffice.md).

The transaction requirement under Task 5 below supersedes the old assumption
that real engine transactions could wait until plan 3. Managed driver writes
already require them in this MVP.

## Global Constraints

- The config file is TOML; every value can be overridden by an environment
  variable `BRAINWIRE_<SECTION>_<KEY>` (for containers).
- Memory defaults follow spec 1.5 for a 16 GB node: block cache 10 GiB with the
  memtables charged inside it (1 GiB), 2 GiB for query execution.
- The default database is `client_memory` (spec 3.3); each database is one
  directory under `data_dir`.
- Errors use Neo4j codes (spec 3.3): `Neo.ClientError.Statement.SyntaxError`
  for syntax and unsupported constructs, `Neo.TransientError.Transaction.Outdated`
  for a commit conflict, `Neo.ClientError.Schema.ConstraintValidationFailed` for
  a CREATE of an existing `entity_id`, `Neo.ClientError.Database.DatabaseNotFound`
  for an unknown database, `Neo.ClientError.Statement.SemanticError` for an
  unknown label, variable or edge type, `Neo.ClientError.Statement.ParameterMissing`
  for a missing parameter.
- Nodes are keyed by `entity_id` and label, as in the platform; `entity_id` is
  a virtual property of every node (readable in RETURN and WHERE, matched in
  patterns, never stored as a column).
- Tests, fixtures and demos use synthetic data only.
- Code, identifiers, comments, commit messages and docs in English; no em
  dashes or en dashes in docs.
- Keep commits local on `codex/brainwire` or another `codex/` branch. Do not
  push or create a remote PR unless Allan explicitly asks.

## File structure

```
crates/cypher/                            package brainwire-cypher
  Cargo.toml
  src/lib.rs                             module list, re-exports parse and execute
  src/ast.rs                             the AST
  src/lexer.rs                           tokens
  src/parser.rs                          recursive descent parser
  src/exec.rs                            executor over brainwire-store
crates/server/                            package brainwire-server, binary brainwire
  Cargo.toml
  src/main.rs                            CLI (clap)
  src/config.rs                          Config, sources, loading, validation
  src/size.rs                            ByteSize ("10GiB")
  src/databases.rs                       registry of open databases
  src/engine.rs                          QueryEngine adapter (Value and PackValue)
  tests/mvp_demo.py                      end-to-end check with the Python driver
crates/store/src/txn.rs                   + scan_range (Task 3)
crates/store/src/graph.rs                 + nodes_of_type (Task 3)
```

## Task 1: Config file and the config commands

**Files:**
- Create: `crates/server/Cargo.toml`, `src/main.rs`, `src/config.rs`, `src/size.rs`
- Modify: `Cargo.toml` (add `"crates/server"` to `members`;
  add `clap = { version = "4", features = ["derive"] }` and `toml = "0.8"` to
  `[workspace.dependencies]`)

**Interfaces:**
- Consumes: nothing from other crates (the mapping to `StoreConfig` is Task 5).
- Produces:
  - `pub struct ByteSize(pub u64)` with `FromStr` (accepts `123`, `64KiB`,
    `512MiB`, `10GiB`, `1TiB`, and decimal `KB`/`MB`/`GB`/`TB`; case-insensitive
    units; optional space before the unit), `Display` (largest binary unit that
    divides exactly, else bytes: `10 GiB`, `1536 MiB`, `123 B`) and
    `Deserialize` from a TOML string or integer.
  - `pub struct Config { pub server: ServerSection, pub memory: MemorySection }`
  - `pub struct ServerSection { pub listen: String, pub data_dir: PathBuf, pub default_database: String, pub max_message: ByteSize }`
    with defaults `127.0.0.1:7687`, `./data`, `client_memory`, `256MiB`.
  - `pub struct MemorySection { pub cache: ByteSize, pub memtables: ByteSize, pub query: ByteSize }`
    with defaults `10GiB`, `1GiB`, `2GiB`.
  - `pub enum Source { Default, File(PathBuf), Env(String) }`
  - `pub struct Loaded { pub config: Config, pub sources: BTreeMap<&'static str, Source> }`
    (keys like `"server.listen"`, `"memory.cache"`, one per field)
  - `pub fn load(path: Option<&Path>, env: &BTreeMap<String, String>) -> Result<Loaded, ConfigError>`
    (defaults, then the file if `path` is given or `./brainwire.toml` exists,
    then the environment map; the binary passes `std::env::vars()` collected
    into the map, tests pass their own)
  - `pub fn default_file() -> String` (a commented TOML with every key and its
    default, which `load` reads back to the defaults)
  - `pub enum ConfigError { Io(String), Parse(String), BadValue { key: String, message: String }, Invalid(String) }`
    with `Display`
  - `ByteSize`, `Config`, `ServerSection`, `MemorySection` and `Source` derive
    `Debug, Clone, PartialEq`; `tempfile` is a dev-dependency of the crate.
  - Validation in `load`: `memtables` must be smaller than `cache` (they are
    charged inside it), `cache` at least 64 MiB, `listen` must parse as a
    socket address.
  - CLI: `brainwire config init [--path <file>] [--force]` writes
    `default_file()` (refuses to overwrite without `--force`);
    `brainwire config show [--config <file>]` prints one line per key:
    `memory.cache      = 10 GiB   (default)`, `(file brainwire.toml)` or
    `(env BRAINWIRE_MEMORY_CACHE)`.

- [ ] **Step 1: Write the failing tests**

In `src/size.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_binary_and_decimal_units() {
        assert_eq!("123".parse::<ByteSize>().unwrap().0, 123);
        assert_eq!("64KiB".parse::<ByteSize>().unwrap().0, 64 << 10);
        assert_eq!("512 mib".parse::<ByteSize>().unwrap().0, 512 << 20);
        assert_eq!("10GiB".parse::<ByteSize>().unwrap().0, 10 << 30);
        assert_eq!("1GB".parse::<ByteSize>().unwrap().0, 1_000_000_000);
        assert!("ten".parse::<ByteSize>().is_err());
        assert!("10XB".parse::<ByteSize>().is_err());
        assert!("-1GiB".parse::<ByteSize>().is_err());
    }

    #[test]
    fn displays_the_largest_exact_binary_unit() {
        assert_eq!(ByteSize(10 << 30).to_string(), "10 GiB");
        assert_eq!(ByteSize(1536 << 20).to_string(), "1536 MiB");
        assert_eq!(ByteSize(123).to_string(), "123 B");
    }
}
```

In `src/config.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn defaults_follow_the_spec() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("none.toml");
        let loaded = load(Some(&missing), &env(&[])).unwrap_err();
        assert!(matches!(loaded, ConfigError::Io(_)));
        let loaded = load(None, &env(&[])).unwrap();
        assert_eq!(loaded.config.memory.cache.0, 10 << 30);
        assert_eq!(loaded.config.memory.memtables.0, 1 << 30);
        assert_eq!(loaded.config.memory.query.0, 2 << 30);
        assert_eq!(loaded.config.server.default_database, "client_memory");
        assert!(matches!(loaded.sources["memory.cache"], Source::Default));
    }

    #[test]
    fn file_then_env_override_and_sources_are_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("brainwire.toml");
        std::fs::write(&file, "[memory]\ncache = \"4GiB\"\n[server]\nlisten = \"0.0.0.0:7687\"\n").unwrap();
        let loaded = load(Some(&file), &env(&[("BRAINWIRE_MEMORY_MEMTABLES", "512MiB")])).unwrap();
        assert_eq!(loaded.config.memory.cache.0, 4 << 30);
        assert_eq!(loaded.config.memory.memtables.0, 512 << 20);
        assert_eq!(loaded.config.server.listen, "0.0.0.0:7687");
        assert!(matches!(loaded.sources["memory.cache"], Source::File(_)));
        assert!(matches!(&loaded.sources["memory.memtables"], Source::Env(v) if v == "BRAINWIRE_MEMORY_MEMTABLES"));
        assert!(matches!(loaded.sources["memory.query"], Source::Default));
    }

    #[test]
    fn memtables_must_fit_inside_the_cache() {
        let err = load(None, &env(&[("BRAINWIRE_MEMORY_CACHE", "1GiB"), ("BRAINWIRE_MEMORY_MEMTABLES", "1GiB")])).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(_)));
    }

    #[test]
    fn a_bad_value_names_its_key() {
        let err = load(None, &env(&[("BRAINWIRE_MEMORY_CACHE", "lots")])).unwrap_err();
        assert!(matches!(err, ConfigError::BadValue { ref key, .. } if key == "memory.cache"));
    }

    #[test]
    fn the_default_file_loads_back_to_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("brainwire.toml");
        std::fs::write(&file, default_file()).unwrap();
        let from_file = load(Some(&file), &env(&[])).unwrap().config;
        let defaults = load(None, &env(&[])).unwrap().config;
        assert_eq!(from_file, defaults);
    }
}
```

`load(None, ..)` must not read `./brainwire.toml` in tests that run from the
crate directory; make the implicit lookup a separate `pub fn find_file(cwd: &Path) -> Option<PathBuf>`
that `main.rs` calls, and keep `load` pure: `None` means defaults and env only.

- [ ] **Step 2: Run the tests to see them fail** (`cargo test -p brainwire-server`)
- [ ] **Step 3: Implement `size.rs`, `config.rs` and the two CLI commands in `main.rs`**
  (`serve` and `db` subcommands come in Task 5; the `Cli` enum already lists
  only `config` here).
- [ ] **Step 4: Run the tests, clippy with `-D warnings` and fmt**
- [ ] **Step 5: Try the binary by hand:** `cargo run -q -p brainwire-server -- config init --path /tmp/x.toml` then
  `BRAINWIRE_MEMORY_CACHE=4GiB cargo run -q -p brainwire-server -- config show --config /tmp/x.toml`; paste the output in the report.
- [ ] **Step 6: Commit:** `feat(brainwire): add the config file and the config commands`

## Task 2: Cypher subset parser

**Files:**
- Create: `crates/cypher/Cargo.toml` (depends on `brainwire-core`, `thiserror`),
  `src/lib.rs`, `src/ast.rs`, `src/lexer.rs`, `src/parser.rs`
- Modify: `Cargo.toml` (add `"crates/cypher"` to `members`)

**Interfaces:**
- Consumes: `brainwire_core::value::Value` (`Null`, `Bool`, `Int(i64)`,
  `Float(f64)`, `Str(String)`, `List(Vec<Value>)`, `Map(BTreeMap<String, Value>)`).
- Produces (exact, Task 4 consumes them):

```rust
pub struct Query { pub clauses: Vec<Clause> }
pub enum Clause {
    Match { patterns: Vec<Pattern>, where_: Option<Expr> },
    Merge(Pattern),
    Create(Vec<Pattern>),
    Set(Vec<SetItem>),
    DetachDelete(Vec<String>),
    Unwind { expr: Expr, var: String },
    Return { items: Vec<ReturnItem>, limit: Option<Expr> },
}
pub struct Pattern { pub start: NodePat, pub hop: Option<(RelPat, NodePat)> }
pub struct NodePat { pub var: Option<String>, pub label: Option<String>, pub props: Vec<(String, Expr)> }
pub struct RelPat { pub var: Option<String>, pub rel_type: String, pub direction: Direction, pub props: Vec<(String, Expr)> }
pub enum Direction { Out, In }
pub struct SetItem { pub var: String, pub prop: String, pub value: Expr }
pub struct ReturnItem { pub expr: Expr, pub alias: Option<String>, pub text: String }
pub enum Expr {
    Literal(Value),
    Param(String),
    Var(String),
    Prop(Box<Expr>, String),
    CountStar,
    Count(Box<Expr>),
    List(Vec<Expr>),
    Map(Vec<(String, Expr)>),
    Cmp(Box<Expr>, CmpOp, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
}
pub enum CmpOp { Eq, Ne, Lt, Le, Gt, Ge }
pub struct ParseError { pub message: String, pub offset: usize }
pub fn parse(query: &str) -> Result<Query, ParseError>
```

All AST types derive `Debug, Clone, PartialEq`. `ReturnItem::text` is the
item's source text, trimmed, used as the column name when there is no alias
(`RETURN c.name` gives the column `c.name`, as Neo4j does).

Grammar of the subset (keywords case-insensitive, `//` comments, identifiers
with letters, digits and `_` or in backticks; string literals in single or
double quotes with `\\`, `\'`, `\"`, `\n` escapes):

```
query     := clause+ [';']
clause    := MATCH pattern (',' pattern)* [WHERE expr]
           | MERGE pattern
           | CREATE pattern (',' pattern)*
           | SET var '.' key '=' expr (',' var '.' key '=' expr)*
           | DETACH DELETE var (',' var)*
           | UNWIND expr AS var
           | RETURN item (',' item)* [LIMIT expr]
item      := expr [AS name]
pattern   := node [rel node]
node      := '(' [var] [':' label] [map] ')'
rel       := '-' '[' [var] ':' type [map] ']' '->'
           | '<-' '[' [var] ':' type [map] ']' '-'
map       := '{' key ':' expr (',' key ':' expr)* '}'
expr      := or;  or := and (OR and)*;  and := not (AND not)*;  not := [NOT] cmp
cmp       := primary [('=' | '<>' | '<' | '<=' | '>' | '>=') primary]
primary   := literal | '$' name | var ['.' key] | count '(' ('*' | expr) ')'
           | '[' [expr (',' expr)*] ']' | map | '(' expr ')'
literal   := integer | float | string | TRUE | FALSE | NULL | '-' number
```

Anything else (`OPTIONAL MATCH`, `WITH`, `ORDER BY`, `SKIP`, variable-length
relationships, several labels, several hops, functions other than `count`,
`CALL`, `DELETE` without `DETACH`) returns `ParseError` whose message starts
with `not supported yet: ` and names the construct.

- [ ] **Step 1: Write the failing tests** (in `src/parser.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::*;
    use brainwire_core::value::Value;

    fn s(v: &str) -> Expr { Expr::Literal(Value::Str(v.into())) }

    #[test]
    fn create_with_properties() {
        let q = parse("CREATE (c:Customer {entity_id: 'c1', name: \"Ana\"})").unwrap();
        assert_eq!(q.clauses, vec![Clause::Create(vec![Pattern {
            start: NodePat { var: Some("c".into()), label: Some("Customer".into()),
                props: vec![("entity_id".into(), s("c1")), ("name".into(), s("Ana"))] },
            hop: None,
        }])]);
    }

    #[test]
    fn merge_then_set_with_parameters() {
        let q = parse("merge (c:Customer {entity_id: $id}) set c.name = $name, c.age = 30").unwrap();
        assert_eq!(q.clauses[1], Clause::Set(vec![
            SetItem { var: "c".into(), prop: "name".into(), value: Expr::Param("name".into()) },
            SetItem { var: "c".into(), prop: "age".into(), value: Expr::Literal(Value::Int(30)) },
        ]));
    }

    #[test]
    fn one_hop_match_with_where_and_return_names() {
        let q = parse("MATCH (c:Customer)-[:WORKS_AT]->(e:Company) WHERE c.name = 'Ana' AND e.name <> 'X' RETURN c.name, e.name AS company LIMIT 5").unwrap();
        let Clause::Match { patterns, where_ } = &q.clauses[0] else { panic!() };
        let (rel, end) = patterns[0].hop.clone().unwrap();
        assert_eq!(rel.rel_type, "WORKS_AT");
        assert_eq!(rel.direction, Direction::Out);
        assert_eq!(end.label.as_deref(), Some("Company"));
        assert!(matches!(where_, Some(Expr::And(_, _))));
        let Clause::Return { items, limit } = &q.clauses[1] else { panic!() };
        assert_eq!(items[0].text, "c.name");
        assert_eq!(items[1].alias.as_deref(), Some("company"));
        assert_eq!(limit, &Some(Expr::Literal(Value::Int(5))));
    }

    #[test]
    fn incoming_relationship_and_count() {
        let q = parse("MATCH (e:Company)<-[r:WORKS_AT]-(c:Customer) RETURN count(*)").unwrap();
        let Clause::Match { patterns, .. } = &q.clauses[0] else { panic!() };
        assert_eq!(patterns[0].hop.as_ref().unwrap().0.direction, Direction::In);
        let Clause::Return { items, .. } = &q.clauses[1] else { panic!() };
        assert_eq!(items[0].expr, Expr::CountStar);
        assert_eq!(items[0].text, "count(*)");
    }

    #[test]
    fn unwind_rows_merge() {
        let q = parse("UNWIND $rows AS row MERGE (c:Customer {entity_id: row.id}) SET c.name = row.name").unwrap();
        assert_eq!(q.clauses[0], Clause::Unwind { expr: Expr::Param("rows".into()), var: "row".into() });
    }

    #[test]
    fn detach_delete_and_literals() {
        let q = parse("MATCH (c:Customer {entity_id: 'c1'}) DETACH DELETE c").unwrap();
        assert_eq!(q.clauses[1], Clause::DetachDelete(vec!["c".into()]));
        let q = parse("RETURN -1.5, true, null, [1, 'a'], {k: 2}").unwrap();
        let Clause::Return { items, .. } = &q.clauses[0] else { panic!() };
        assert_eq!(items[0].expr, Expr::Literal(Value::Float(-1.5)));
        assert_eq!(items[2].expr, Expr::Literal(Value::Null));
    }

    #[test]
    fn errors_have_offsets_and_unsupported_is_named() {
        let e = parse("MATCH (c:Customer RETURN c").unwrap_err();
        assert!(e.offset > 0);
        let e = parse("MATCH (c:Customer) WITH c RETURN c").unwrap_err();
        assert!(e.message.starts_with("not supported yet: WITH"), "{}", e.message);
        let e = parse("MATCH (a)-[:T*1..3]->(b) RETURN a").unwrap_err();
        assert!(e.message.starts_with("not supported yet:"), "{}", e.message);
        assert!(parse("").is_err());
    }
}
```

- [ ] **Step 2: Run them to see them fail** (`cargo test -p brainwire-cypher`)
- [ ] **Step 3: Implement the lexer and the parser.** No panics on any input
  (fuzz-style test: parse every prefix of every test query and assert it
  returns `Ok` or `Err`, never panics); nesting of parentheses, lists and maps
  limited to 64 levels (`ParseError "nested deeper than 64 levels"`).
- [ ] **Step 4: Run the tests, clippy with `-D warnings` and fmt**
- [ ] **Step 5: Commit:** `feat(brainwire): parse a Cypher subset`

## Task 3: Label scans in the store

**Files:**
- Modify: `crates/store/src/txn.rs` (add `scan_range`), `crates/store/src/graph.rs`
  (add `nodes_of_type`)

**Interfaces:**
- Consumes: `Txn`, `Cf` (storage Task 7), `NodeId::type_range` (returns
  `Result<(NodeId, NodeId), IdError>`), graph encodings (storage Task 9: the
  `node` key is the node id as u64 BE).
- Produces:
  - `Txn::scan_range(&self, cf: Cf, lo: &[u8], hi: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StoreError>`:
    keys `lo <= k < hi`, snapshot merged with the overlay, deletes applied, key
    order; uses `ReadOptions::set_iterate_upper_bound`.
  - `pub fn nodes_of_type(txn: &Txn, ty: TypeId) -> Result<Vec<NodeId>, GraphError>`:
    every node of that type in partition 0, in id order, including nodes
    written earlier in the same transaction.

- [ ] **Step 1: Failing tests:** `scan_range` returns only keys inside the
  range, applies an overlay delete and includes an overlay insert; nodes of two
  types interleaved by insertion come back per type, in id order; a node
  deleted in the transaction is gone; an empty type returns an empty list.
- [ ] **Step 2: Run to see them fail**, **Step 3: implement**, **Step 4: tests,
  clippy, fmt**, **Step 5: commit** `feat(brainwire): scan the nodes of one type`

## Task 4: Executor

**Files:**
- Create: `crates/cypher/src/exec.rs`
- Modify: `crates/cypher/Cargo.toml` (add `brainwire-store`; dev: `tempfile`,
  `serde_json`), `src/lib.rs`

**Interfaces:**
- Consumes: `parse` and the AST (Task 2); `Database`, `Txn`, `Cf`,
  `TxnError::Conflict` (storage Task 7); `merge_node`, `get_node`,
  `find_by_entity_id`, `merge_edge`, `out_edges`, `in_edges`, `detach_delete`,
  `Database::catalog`, `Database::apply_ontology`, `NodeRecord`, `EdgeRef`,
  `GraphError` (storage Task 9); `nodes_of_type` (Task 3); `Catalog`,
  `NodeType::column` (core schema); `decode`/`Row` (core codec).
- Produces:

```rust
pub struct QueryOutput { pub fields: Vec<String>, pub rows: Vec<Vec<Out>> }
pub enum Out { Value(Value), Node(NodeOut) }
pub struct NodeOut { pub id: NodeId, pub label: String, pub props: BTreeMap<String, Value> }
pub enum ExecError {
    Syntax(ParseError),
    Unsupported(String),
    Semantic(String),
    ParameterMissing(String),
    ConstraintViolation(String),
    Conflict,
    Graph(GraphError),
}
pub fn execute(db: &Database, query: &str, params: &BTreeMap<String, Value>) -> Result<QueryOutput, ExecError>
```

`NodeOut::props` always holds `entity_id` plus the stored properties (columns
named through the catalog, then the row's flexible map).

Semantics (one storage transaction per call, committed at the end only if the
query wrote; `TxnError::Conflict` becomes `ExecError::Conflict`):

- Rows are bindings from variable name to a node id or a value, starting with
  one empty row; each clause maps the list of rows to a new list.
- `UNWIND expr AS v`: for each row, a list value gives one row per element; a
  null or an empty list gives none; any other value is `Semantic`.
- `MATCH`: a node pattern needs a label (`Unsupported("MATCH needs a label")`
  otherwise). With an `entity_id` property the candidates come from
  `find_by_entity_id`; otherwise from `nodes_of_type`. Every other property in
  the pattern filters by equality. A hop follows `out_edges` or `in_edges` of
  the bound start node, keeps the edges of the named type, and keeps the other
  end when its type is the end pattern's label and its properties match.
  A variable already bound in the row constrains the pattern to that node.
  Comma-separated patterns combine as a cartesian product. `WHERE` keeps the
  rows where the expression is `true` (null is false).
- `MERGE (v:L {entity_id: x, ...})`: `merge_node` with the other properties;
  binds `v`. `MERGE (a)-[:T]->(b)` with `a` and `b` bound: `merge_edge`. Any
  other MERGE shape is `Unsupported`.
- `CREATE (v:L {entity_id: x, ...})`: `ConstraintViolation` when a node with
  that label and `entity_id` exists, else `merge_node`. `CREATE (a)-[:T]->(b)`
  with both bound: `merge_edge` (one edge per source, type and target in this
  MVP). A node pattern without `entity_id` is `Unsupported`.
- `SET v.p = e`: collects the values per node and applies them with
  `merge_node` (label and `entity_id` from the node itself); `NULL` removes the
  property; `SET v.entity_id` is `Semantic`.
- `DETACH DELETE v`: `detach_delete` for each bound node, once.
- `RETURN`: evaluates each item per row; a node variable returns `Out::Node`;
  `v.p` reads the property (missing is null); if any item is `count(*)` or
  `count(e)` (non-null values), the other items are grouping keys; `LIMIT`
  takes an integer. Field names are the aliases or `ReturnItem::text`. A query
  without RETURN returns no fields and no rows.
- Comparisons: integers and floats compare numerically, strings and booleans
  with their own kind, anything with null is null, other mixes are false for
  `=` and null otherwise; `AND`, `OR`, `NOT` use three-valued logic.
- A missing `$param` is `ParameterMissing`; an unknown variable, label or edge
  type is `Semantic`; a `ParseError` is `Syntax`.

- [x] **Step 1: Failing tests** (in `src/exec.rs`, with a tempdir database and
  the synthetic fixture `crates/core/tests/fixtures/graph_mapping.json` applied
  through `Database::apply_ontology`):
  1. `CREATE (c:Customer {entity_id: 'c1', name: 'Ana'})` then
     `MATCH (c:Customer {entity_id: 'c1'}) RETURN c.name` gives field
     `c.name` and one row `Str("Ana")`.
  2. The same CREATE again is `ConstraintViolation`.
  3. `MERGE (c:Customer {entity_id: $id}) SET c.name = $name` twice with
     different names leaves one node with the last name
     (`MATCH (c:Customer) RETURN count(*)` gives `Int(1)`).
  4. Company `k1`, then
     `MATCH (c:Customer {entity_id: 'c1'}), (k:Company {entity_id: 'k1'}) MERGE (c)-[:WORKS_AT]->(k)`,
     then `MATCH (c:Customer)-[:WORKS_AT]->(k:Company) RETURN c.name, k.name AS company`
     gives one row, and the incoming form
     `MATCH (k:Company)<-[:WORKS_AT]-(c:Customer) RETURN c.entity_id` gives `c1`.
  5. `UNWIND $rows AS row MERGE (c:Customer {entity_id: row.id}) SET c.name = row.name`
     with 1,000 rows, then `MATCH (c:Customer) WHERE c.name = 'name-500' RETURN c.entity_id`
     gives exactly one row, and `RETURN count(*)` over all Customers gives 1,000.
  6. `RETURN c` returns `Out::Node` with label `Customer` and props holding
     `entity_id` and `name`.
  7. `SET c.name = null` removes the property (`RETURN c.name` is `Null`).
  8. `MATCH (c:Customer {entity_id: 'c1'}) DETACH DELETE c` removes the node and
     its edges (`check_invariants` passes; the hop query returns no rows).
  9. Errors: `MATCH (x:Nope) RETURN x` is `Semantic`; `RETURN $missing` is
     `ParameterMissing`; `MATCH (c) RETURN c` is `Unsupported`; a syntax error
     is `Syntax`.
  10. Two queries racing on the same `entity_id` through two transactions: the
      second commit gives `ExecError::Conflict` (drive `execute` from two
      threads on an `Arc<Database>` with a barrier, or test the mapping from
      `TxnError::Conflict` directly if the race is not deterministic).
- [x] **Step 2: Run to see them fail**, **Step 3: implement**, **Step 4: tests,
  clippy, fmt**, **Step 5: commit** `feat(brainwire): execute the Cypher subset on the store`

## Task 5: `brainwire serve`, databases and the driver check

**Files:**
- Create: `crates/server/src/databases.rs`, `src/engine.rs`, `tests/mvp_demo.py`
- Modify: `crates/server/Cargo.toml` (add `brainwire-bolt`, `brainwire-cypher`,
  `brainwire-store`, `brainwire-core`, `tokio`, `serde_json`), `src/main.rs`

**Interfaces:**
- Consumes: `Config`, `Loaded`, `load`, `find_file` (Task 1); `execute`,
  `QueryOutput`, `Out`, `NodeOut`, `ExecError` (Task 4); `Database::open`,
  `Resources::new`, `StoreConfig` (storage), `Database::apply_ontology`;
  `brainwire_bolt::server::serve_with_limit`, `QueryEngine`, `QueryResult`,
  `EngineError`, `PackValue` (Bolt plan; `QueryEngine::run(&mut self, db: Option<&str>, query: &str, params: &PackValue) -> Result<QueryResult, EngineError>`).
- Produces:
  - `pub struct Databases { .. }` with
    `open_all(config: &Config) -> Result<Databases, String>` (one
    `Resources` for the process; every subdirectory of `data_dir` is a
    database; the default database is created when missing),
    `get(&self, name: &str) -> Option<Arc<Database>>`, `names(&self) -> Vec<String>`,
    `create(&self, name: &str) -> Result<Arc<Database>, String>`.
  - `StoreConfig` from the config: `cache_bytes = memory.cache`,
    `memtable_bytes = memory.memtables`, `id_block = 1024`,
    `conflict_history_keys = 1_000_000`.
  - `pub struct DbEngine { dbs: Arc<Databases>, default_db: String }`
    implementing `QueryEngine`: parameters from `PackValue::Map` to
    `BTreeMap<String, Value>` (PackStream Integer, Float, String, Boolean,
    Null, List and Map map to `Value`; anything else is
    `Neo.ClientError.Statement.TypeError`); rows back to `PackValue`, with a
    node as `PackValue::Struct` tag `0x4E` and fields
    `[Integer(id as i64), List([String(label)]), Map(props), String("n:<id>")]`;
    `ExecError` to the codes in the Global Constraints.
  - CLI: `brainwire serve [--config <file>]` prints one banner line with the
    listen address, the databases and the memory settings, then serves;
    `brainwire db list`, `brainwire db create <name> [--ontology <file.json>]`,
    `brainwire db apply-ontology <name> <file.json>`.

The [2026-09-27 transaction slice](2026-09-27-brainwire-transactions.md) covers
shared-query execution and Bolt lifecycle hooks with test-only integration.
Production worker/adapter/registry/serve work below remains open.

Read the [transaction integration preflight](2026-09-26-brainwire-bolt-transaction-preflight.md)
for the current ownership/runtime seams and the next implementation slices.

**Required production integration:** shared-query `QueryTransaction` and Bolt
begin/run/commit/abort hooks are implemented. The production worker and adapter
must connect those APIs while preserving the transaction across RUNs. Calling
the per-query `execute` helper independently for each RUN would commit before
COMMIT and break ROLLBACK. Preserve Task 4's atomic standalone `execute` API;
use the shared transaction context for Bolt. Do not substitute an accepted no-op
for a production transaction operation that has not been implemented.

Acceptance must cover a multi-query transaction committing all its writes;
ROLLBACK leaving no writes after reopen; RESET and connection teardown
aborting pending writes; a later query error preventing earlier staged writes
from being committed; and commit conflicts returning the transient error.
These checks belong alongside the adapter and driver tests below.

- [ ] **Step 1: Failing tests:** engine conversion tests (a params map with
  every PackValue kind; a node output becomes the 0x4E struct with the right
  element id; each `ExecError` gets its code); `Databases::open_all` on an empty
  tempdir creates `client_memory`, and a second `open_all` finds it again.
- [ ] **Step 2: Run to see them fail**, **Step 3: implement.**
- [ ] **Step 4: The driver check.** `tests/mvp_demo.py <uri> write|read`:
  `write` applies nothing itself (the server is started on a data directory
  prepared with `brainwire db create client_memory --ontology crates/core/tests/fixtures/graph_mapping.json`),
  creates two Customers and one Company with the driver (`execute_query` and a
  managed `execute_write`), links them with `WORKS_AT`, loads 10,000 Customers
  with one `UNWIND $rows` query and prints the time it took; `read` checks the
  hop query, the node struct (`record["c"].labels == {"Customer"}` and
  `record["c"]["name"]`), and `count(*) == 10002`, then prints
  `mvp demo: ok`. Run: create the data directory, start `brainwire serve` with
  a config on port 37687, run `write`, stop the server, start it again, run
  `read`. Do it with the 5.28.6 driver
  (`/private/tmp/claude-501/-Users-fraga-Documents-client-platform/41f59c99-d001-444a-9711-f2c8aecf8c78/scratchpad/neo4j5/bin/python`)
  and with 6.3.0 (`/Users/fraga/Documents/client-platform/client-core/.venv/bin/python`).
  No server may be left running.
- [ ] **Step 5: Workspace tests, clippy, fmt; commit**
  `feat(brainwire): serve the store over Bolt with the brainwire binary`

## What comes next

The platform's own queries need more of the language (`WITH`, `OPTIONAL
MATCH`, `ORDER BY`, functions, the ACL predicate), the real engine seam from
the Bolt final review (cursors and execution off the runtime) and the
pre-authentication limits parked as blockers. That is plan 3.
