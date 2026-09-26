# Brainwire executor and backoffice iteration

User request: improve the database and CLI further, autonomously, local only.
Start 2026-09-26 18:33:56 UTC. Allan extended this session until 21:30
Europe/Amsterdam (19:30 UTC), beginning wrap-up near 19:20. Base: 5e7b86c6b735cb0c9b1704689a10aff041c19dd3.

Spec: ../specs/2026-09-24-brainwire-design.md (storage transactions, query subset,
CLI), engine plan 2026-09-26-brainwire-mvp.md Task 4, and
../../design/2026-09-26-brainwire-observatory.md. Use subagent-driven development
with one implementer at a time, task review, then whole-change review.

## Global Constraints

- All work stays in /Users/fraga/Documents/brainwire on codex/brainwire. No push,
  publication, remote PR or edits in the historical platform checkout.
- English code/comments/docs; pt-BR default and English for the TUI. Preserve
  the electric green #57FF48, black #070B09 and restrained cyan direction.
- The saved brainwire-backoffice-user-reference.png is the primary composition
  target, excluding its external image-editor toolbar. Current TUI is an
  intermediate synthetic demo, not the visual target itself.
- Task 1 implements the existing engine MVP executor contract. One atomic
  storage transaction per execute call; any error discards staged graph writes.
  No pretend transactions, silent unsupported clauses or swallowed errors.
- This remains an eager local MVP. No claim of bounded query RAM, full Cypher,
  ACL/RBAC enforcement, production readiness or the five-billion-element target.
- Leave Bolt and durable serve integration for the next task. A direct local
  cypher command must be labeled embedded/offline, not a network client.
- TUI graph, memory and file data remain visibly illustrative in this iteration;
  do not imply that its F5 examples or file list use the new executor.
- Synthetic fixtures only. Preserve config commands and shell flags, safe
  terminal cleanup, non-TTY guards, bounded snapshot/editor sizes and NO_COLOR.

## Task 1: Atomic Cypher executor

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

### Clarifications from preflight

- Read the catalog from the same transaction snapshot as graph data (Cf::Meta,
  key b"catalog"); Database::catalog starts a separate snapshot. A query without
  graph access (RETURN literals/UNWIND) should work without an applied ontology.
- Reject relationship variables/properties in MATCH with explicit Unsupported
  until an edge value/property read contract exists. Anonymous typed hops work.
  Do not bind a relationship as a node or ignore its property filters. For
  CREATE/MERGE edges, anonymous bound endpoints are supported; stored edge
  properties can be passed to merge_edge, but RETURN relationship is unsupported.
- Validate semantic structure before row execution: unknown labels/types,
  variables and missing parameters must not silently succeed because MATCH or
  UNWIND produced zero rows. RETURN must be final, aggregation nesting and
  aggregate predicates must fail explicitly; nonaggregate keys plus top-level
  count aggregates are supported. LIMIT must be a nonnegative row-independent
  integer; zero is legal. Nonboolean WHERE/logic operands are semantic errors.
- Respect already-bound pattern variables and repeated variables in a single
  pattern, including (a)-[:T]->(a). Unsupported CREATE/MERGE shapes fail rather
  than overwrite unrelated variable bindings. Tests must cover late-query errors
  rolling back earlier writes, repeated variables, zero-input aggregation,
  grouping, numeric comparison including large i64 precision, null logic,
  parameter errors on empty input, and reopen persistence in addition to the
  original ten acceptance scenarios. Do not coerce all integers through f64.
- Small helper modules under src/exec/ are welcome to keep responsibilities
  readable. Avoid one huge mixed executor/tests file. Do not change store APIs
  unless a concrete correctness requirement requires it and report that need.
- TDD for engine behavior: capture failing test evidence before implementation.
  Commit only owned cypher files plus Cargo.lock as needed. Controller owns docs.
  Report at task-1-report.md in this plan's .superpowers/sdd workspace, including
  exact tests/commands/results, limitations, commit and self-review.

### Boundary correction discovered during Task 1

The existing lexer rejects -9223372036854775808 before the parser can negate
its magnitude. Fix this within the cypher crate as a separate small commit.
Accept exactly i64::MIN, continue rejecting positive overflow and one below
the negative minimum, and add lexer/parser/executor regressions. Keep other
parser behavior and unsupported constructs unchanged. Document any token
representation change so review can inspect its callers.

Reject float literal overflow to non-finite values (for example 1e309) with
ParseError while retaining finite exponent literals. This prevents lossy JSON
output of infinity in the new local command.

## Task 2: Local Cypher command and richer terminal backoffice

Depends on reviewed Task 1. Owned files: crates/server, crates/tui, their manifests
and Cargo.lock. Controller owns docs and images. Do not modify executor/store/Bolt.

### Real local command

- Add `brainwire cypher --database <directory> --query <text>` with optional
  `--ontology <json-file>`, `--params <json-object>`, `--config <toml-file>`.
  Explain that this opens an embedded local store; it cannot attach to an already
  open server's database. Keep serve/Bolt out of scope.
- Use the existing config loader/environment precedence for memory settings.
  Explicit database path is required. New stores may be initialized with an
  explicitly supplied ontology; existing stores can be reopened without it.
  JSON integer parameters outside i64 must fail explicitly, not silently become
  lossy floats; normal finite JSON floats remain supported.
  Parse parameter JSON and ontology before opening/mutating the database where
  possible. Honor RocksDB's exclusive lock, report errors actionably.
- Execute through brainwire_cypher::execute; output JSON with fields and rows,
  using normal JSON scalar/list/map values and an explicit node envelope
  {"type":"node","id":...,"label":...,"properties":...}. Include virtual
  entity_id in node properties. Explain format in --help or module docs for the
  controller's guide. Error has meaningful category and nonzero exit status.
- Integration tests launch the actual binary in a temporary directory, initialize
  schema/create/read/reopen, accept parameters, verify failed writes are absent,
  and preserve config/shell routing. Use small test config memory budgets.

### Terminal improvements guided by the saved reference

- Inspect docs/design/assets/brainwire-backoffice-user-reference.png first.
- At 160x48, build a denser Disk & Files composition: navigation and a small
  graph preview at left, illustrative folder tree and file table (name/type/size)
  in the center, LSM memtable -> L0 -> L1 -> L2 flow below, contextual selected
  file explanation at right and a compact memory explanation at the bottom.
  Strong green selection, quiet cyan structure, thin panels, readable hierarchy.
- Use available terminal space intentionally. At 120x36 retain core file list,
  flow and explanations; at 80x24 use a compact view with reachable help and
  selection. No overlap, panic or hiding required controls. Every view remains
  clearly DEMO/SYNTHETIC. No real file operations or fake measurements.
- In Memory, improve the visual hierarchy of limit, configured budget, usage
  and remaining headroom. Use compact cards/bars and a clear inclusion diagram
  for memtables within cache; keep the existing consistent synthetic figures.
- Improve graph edge drawing with a Ratatui braille canvas or equivalent smooth
  terminal lines and clustered positions; retain selection/incident highlight,
  stable fixture IDs, readable selected-node properties and no-color cues.
- Give inspector focus a useful independent scroll action, or remove that focus
  stop until meaningful. Make long query text discoverable at narrow widths
  with wrapping or explicit overflow hints. Preserve q/shortcut editor behavior.
- Split the current monolithic TUI file into focused modules as useful for these
  changes. Do not turn cosmetic changes into a framework rewrite.
- Meaningful tests for selection/explanation/focus/overflow and adaptive render
  assertions on all views at 160x48,120x36,80x24. Controller does final PTY/visual QA.
- Package tests, clippy with -D warnings, fmt; commit owned files. Report precise
  behavior, checks and limitations to task-2-report.md. No subagents.

## Completion

Controller updates README, JOURNAL, engine handoff and terminal guide; captures
actual running UI previews. Run workspace tests/clippy/fmt once consolidated,
review the whole change, keep commits local and give runnable examples.

## Verified handoff

Task 1 completed in `5ace18d` and `51aca5a`; Task 2 in `f6d94fb` and
`7571721`. Both task reviews pass spec and quality with no findings.
Before the bulk-input addendum, the workspace passed 262 tests and clippy with denied
warnings, formatting and binary build. Independent actual-binary and PTY
verification covers persistence/rollback/numeric boundaries and terminal
layouts/interaction/restoration. See JOURNAL and the local query / terminal
run guides for scope and limitations. The whole-change review through
`c4a7462` passed clean. No remote operations are part of this handoff.

## Task 3 addendum: file input for bulk query parameters

The final local-load check exposed an OS argument limit: a synthetic 10,000-row
JSON object (1,708,900 bytes) cannot be passed through `--params` on this Mac
(ARG_MAX 1,048,576; process launch returns Argument list too long). Add
`--params-file <json-file>` to the embedded command, mutually exclusive with
explicit `--params`, retaining empty parameters when both are omitted. Read and
validate before storage mutation through the same strict JSON converter. Keep
all transaction and output behavior unchanged. Missing/malformed/non-object
files and out-of-i64 integers must fail explicitly. Process tests cover import,
reopen, invalid files and conflicting options. Controller verifies a >ARG_MAX
10,000-record import and repeats affected integration gates after the amendment.


Task 3 completed in `664d428`; spec/quality review passes with no findings.
The final workspace now passes 265 tests, all-target clippy and formatting.
A real 1,708,900-byte parameter file imported 10,000 records, confirmed after
reopening, without passing that object through process arguments. The original
whole-change review of `5e7b86c..c4a7462` passed clean. Final amendment review
of `c4a7462..186cf28` also passes, approving the combined change with no open
findings. All three tasks are complete. Next: the linked Bolt transaction
preflight and MVP Task 5; no durable-server or live-TUI claim is made.
