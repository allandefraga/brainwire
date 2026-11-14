# Run Cypher queries

Python commands below use the environment from [Python setup](python-package.md).

Choose how to connect before running a query:

| Your task | Use |
| --- | --- |
| Query a running server | A Bolt client such as the [Python helper](python-package.md) |
| Query local database files | `brainwire cypher`, with the server stopped |
| Inspect the running graph | The [connected TUI](connected-status.md), which is read-only |

The local CLI needs exclusive access to its RocksDB directory. For the fastest
runnable server example, start with the [Docker guide](container.md).

## Run a local example

`brainwire cypher` runs the [supported Cypher](#supported-cypher) against an embedded
RocksDB database on this computer. Each successful query commits its writes
atomically; an execution error discards that query's staged graph changes.
Separate command invocations reopen the same directory and see committed data.
This command is separate from the synthetic `shell --demo` interface.

From the repository root, fetch dependencies once and build the CLI:

```sh
cargo fetch --locked
cargo build --locked --offline -p brainwire-server --bin brainwire
```

Initialize an isolated example database with the synthetic test ontology and
write a Customer. The small cache budgets below suit this example:

```sh
BRAINWIRE_MEMORY_CACHE=64MiB BRAINWIRE_MEMORY_MEMTABLES=16MiB \
./target/debug/brainwire cypher \
  --database ./data/local-demo \
  --ontology crates/core/tests/fixtures/graph_mapping.json \
  --query "MERGE (c:Customer {entity_id: 'demo-c1'}) SET c.name = 'Ana' RETURN c"
```

Read it in a new process, without applying the ontology again:

```sh
BRAINWIRE_MEMORY_CACHE=64MiB BRAINWIRE_MEMORY_MEMTABLES=16MiB \
./target/debug/brainwire cypher \
  --database ./data/local-demo \
  --query 'MATCH (c:Customer {entity_id: $id}) RETURN c.name AS name' \
  --params '{"id":"demo-c1"}'
```

## Read results and handle errors

The output is JSON with `fields` and `rows`. Scalars, lists and maps use normal
JSON values. A returned node has an explicit envelope with `type: "node"`,
`id`, `label` and `properties`; its properties include the virtual `entity_id`.
The `properties(n)` and `keys(n)` functions expose application node properties,
excluding that virtual identity; wildcard map projection `n{.*}` follows the same
rule. Read `n.entity_id` explicitly, or project `n{.entity_id, .*}`, when the
identity is needed. This changes earlier beta behavior, where these functions
included `entity_id`. Whole-node results retain it for identity and client
compatibility. A literal map key or a relationship property named `entity_id`
remains ordinary application data and is preserved. Reserved `acl_*` names,
matched case-insensitively, remain hidden from node and relationship scalar,
property-map, wildcard-map and whole-entity results, including legacy rows.
Errors print a category and explanation, and exit unsuccessfully. JSON integer
parameters must fit signed 64-bit; fractional parameters must be finite.
Nonfinite stored float values cannot be represented in JSON and cause an
output error. Output serialization or a broken output pipe can fail after a
successful write query has already committed.

`--database` names a directory, not a remote database alias. `--config` selects
a TOML configuration; memory settings follow the same defaults, file and
`BRAINWIRE_*` environment precedence as the existing config commands.
`--ontology` explicitly applies a JSON ontology before the query, in a separate
operation: a later query error does not undo the ontology update. RocksDB's
exclusive lock prevents this command from attaching to a directory already
open by another process. Stop that process or choose another local directory.

The ontology is a versioned catalog of labels, property column IDs, edge
signatures and event streams. Reapplying it preserves existing type and column
IDs and appends newly declared columns, so old rows remain decodable. Use
`brainwire db schema NAME --json` to inspect a named database's persisted
catalog while the server is stopped. `identifier_columns` remains a declaration. `indexed_fields` and
`er_fields` create physical equality indexes for string, Boolean and finite numeric
values in `idx`, with a bounded atomic backfill on ontology application.
List and map predicates still scan the label. `entity_id` has its
separate primary-key lookup. `db schema --json` reports whether a legacy
database's scalar index is ready.
Catalog versions are schema revisions, not a history of each node. Timeline
declarations do not yet record ticket revisions or support as-of queries.
Retention and ontology-driven cleanup are specified in the
[temporal graph and maintenance design](design/2026-10-01-temporal-graph-and-maintenance.md).

## Bulk parameters from a file

For larger imports, write the same parameter object to JSON and pass
`--params-file`. This avoids the operating system's command-line argument-size
limit and cumbersome shell quoting. It is mutually exclusive with explicit
`--params`; omitting both uses an empty object. The file is read and validated
before opening storage, with the same signed-integer and finite-float checks.

For example, after initializing the ontology above:

```sh
python3 - <<'PYDATA'
import json
from pathlib import Path
path = Path("data/import-params.json")
path.parent.mkdir(parents=True, exist_ok=True)
path.write_text(json.dumps({"rows": [
    {"id": f"bulk-{i}", "name": f"Customer {i}"}
    for i in range(10_000)
]}))
PYDATA

BRAINWIRE_MEMORY_CACHE=64MiB BRAINWIRE_MEMORY_MEMTABLES=16MiB \
./target/debug/brainwire cypher \
  --database ./data/local-demo \
  --params-file ./data/import-params.json \
  --query 'UNWIND $rows AS row MERGE (c:Customer {entity_id: row.id}) SET c.name = row.name RETURN count(*) AS processed'
```

File input still materializes the parameter document. Read-only MATCH/UNWIND
can process intermediate rows incrementally; writes and final JSON output remain
materialized and subject to conservative accounting.

## Supported Cypher

The clauses and functions below describe the current implementation. The fixed differential test suite covers synthetic query shapes; an older
passing run does not prove compatibility for a new binary or every query shape.
Test the queries your application uses.
Unsupported forms return an error.

**Clauses.** `MATCH`, `OPTIONAL MATCH`, `WHERE`, `WITH` (`DISTINCT`, `WHERE`,
`ORDER BY`, `SKIP`, `LIMIT`, aggregation mid-query), `RETURN` (same options,
plus `*`), `UNWIND`, `UNION` / `UNION ALL` (the two cannot be mixed in one
query), `CREATE`, `MERGE ... ON CREATE SET / ON MATCH SET`, `SET`
(`n.p = v`, `n += map`, `n = map`, `n:Label`), `REMOVE`, `DELETE`,
`DETACH DELETE` (nodes, relationships, paths), `CALL ... YIELD ... WHERE`, and
`CREATE INDEX` / `DROP INDEX` (mapped to catalog procedures).

`SHOW INDEXES` is a standalone alias for `CALL db.indexes()` and returns
Brainwire's existing index columns. `SHOW INDEXES YIELD`, filtering and other
`SHOW` variants are not implemented.

**Patterns.** Several hops in one pattern, comma-separated patterns, variable
length `*`, `*n`, `*n..m` (bound 64), untyped relationships `-[r]->`,
alternative types `[:A|B]`, undirected `-[r]-`, unlabeled `MATCH (n)`,
relationship property maps, named paths `p = (...)`, and relationship
uniqueness within one `MATCH`. Pattern predicates and `exists(pattern)` work
in `WHERE`.

**Expressions.** `CASE`, list comprehensions, `any/all/none/single`, `reduce`,
map projection, list indexing and slicing, `+ - * / % ^` on numbers, strings
and lists, `STARTS WITH`, `ENDS WITH`, `CONTAINS`, `IN`, `XOR`, `n:Label`,
`IS [NOT] NULL`, parameters and three-valued logic.

**Functions.** `id`, `elementId`, `labels`, `type`, `properties`, `keys`,
`startNode`, `endNode`, `nodes`, `relationships`, `length`, `size`,
`coalesce`, `head`, `last`, `tail`, `range`, `reverse`, `exists`, `isEmpty`,
`valueType`, `timestamp`, `rand`, `pi`, `e`; conversions `toString`,
`toInteger`, `toFloat`, `toBoolean` and the `...OrNull` forms; strings
`toLower`, `toUpper`, `trim`, `ltrim`, `rtrim`, `replace`, `substring`,
`left`, `right`, `split`; math `abs`, `ceil`, `floor`, `round`, `sqrt`,
`sign`, `exp`, `log`, `log10`.

`toBooleanList`, `toIntegerList` and `toFloatList` convert each list item using
the corresponding `...OrNull` conversion. Unconvertible values become `null`;
a `null` list returns `null`, and a non-list argument is an error. These are
list conversions, not native vector indexes.

`toBooleanOrNull` accepts booleans, strings and integers; unsupported value types
and invalid strings return `null`. `toBoolean` retains its stricter type errors.

`toInteger` accepts finite float inputs in the signed 64-bit range. Decimal
strings near the range boundaries retain more restrictive legacy parsing;
exact decimal conversion is not supported.

`round(value[, precision])` returns a float and propagates null from either
argument. With omitted or zero precision, ties round toward positive infinity:
`round(-2.5) = -2.0`. With positive precision, ties round away from zero:
`round(-1.25, 1) = -1.3`. Precision accepts an integer or an integral float in
`0..=2147483647`; invalid precision fails rather than wrapping. High precision
preserves a value when rounding cannot change its binary float representation.
NaN and infinity parameters remain unchanged. Decimal rounding still uses binary
floating-point arithmetic, so this is not exact decimal arithmetic or financial
rounding. The three-argument rounding-mode form is not supported. The zero and positive precision cases intentionally use different tie rules.

**Regular expressions.** `text =~ pattern` matches the whole string using a
bounded Rust regex subset. Pass patterns as parameters. Java-specific
lookaround and backreferences are unsupported. See the [regex
contract](cypher-regex.md) for flags, memory, errors and performance limits.

**Aggregates.** `count`, `sum`, `avg`, `min`, `max`, `collect`, `stdev`,
`stdevp`, `percentileCont`, `percentileDisc`, with `DISTINCT` and grouping.
`percentileDisc` retains the selected numeric input, including large integers;
its retained values count against the query budget, so large aggregates may be
refused. With `DISTINCT`, numerically equal integer and float inputs share a key
and the first representative is kept.

**Conditional value.** `nullIf(a, b)` returns `null` when the two values are
equal; otherwise it returns `a`. Comparisons that evaluate to unknown, such as
`nullIf(1, null)`, return the first value.

**Procedures.** `db.labels`, `db.relationshipTypes`, `db.propertyKeys`,
`dbms.components`, `db.schema`, `db.indexes`, `db.commitState`,
`db.checkpoint`, `db.memory`, `db.setNodeAcl(s)`, `db.nodeAcls`, `db.assertNodeState`,
`graph.project`, `graph.degree`, `graph.pagerank` and `knowledge.rank`. See
[sparse analytics](sparse-analytics.md) and
[explainable knowledge ranking](knowledge-ranking.md).

`CALL db.assertNodeState($label, $entity_id, $expected)` checks a node's
bounded scalar properties and adds a transaction dependency. Use it in the
**same explicit standalone write transaction** as the protected mutations.
If the node changes before commit, the transaction conflicts and rolls back.
Missing nodes and mismatched fields return a generic precondition error. A
null expected value matches an absent property. The call requires unrestricted
graph-write access; identified or replicated transactions are not supported.
An autocommit call only guards itself, not a later write. This is not a lock or
a guarantee against writers that do not participate in the protocol.

**Returned values.** Nodes, relationships, paths and lists or maps of them are
returned as Bolt Node, Relationship and Path structures (JSON envelopes with
`type` in the CLI). Relationship variables read properties
(`r.weight`, missing is null) and can be deleted with `DELETE r`.

**Identity rules.**

- A node has exactly one label, fixed at creation. Writes (`CREATE`, `MERGE`)
  need a label; reads may omit it.
- `MERGE` without `entity_id` derives the id from the node's properties, so
  the same properties find the same node. `CREATE` without `entity_id`
  generates one.

**Not supported.** `CALL { }` subqueries, `EXISTS { }` subquery syntax (use
`exists(pattern)` or a pattern predicate), pattern comprehensions,
`FOREACH`, `LOAD CSV`, temporal and spatial types, and aggregates
inside `size()`. Full Cypher, range/vector search and ANN remain separate
work.

Whole relationship `SET` is not available; create or update edge properties
with `MERGE (a)-[:TYPE {prop: value}]->(b)`. `DELETE r` removes the matched
relationship, preserves endpoint nodes and respects graph write authorization.
An ontology can opt a new relationship type into immutable merge keys; see
[keyed relationships](temporal-data.md#keyed-relationships) for the supported
semantics, migration and mixed-version restrictions.

Some read pipelines process intermediate rows incrementally; writes and the
final result remain materialized. Over Bolt, a read that returns nodes,
relationships or paths keeps them as handles and materializes their properties
one PULL page at a time ([design](design/bolt-result-streaming.md)); the row set
itself is still computed before RUN answers. This is not demand-driven Bolt
execution for every query. `memory.query` enforces conservative accounting rather than a
hard RSS ceiling. Optional graph-scoped authorization and connected
ontology/memory/index inspection are implemented. The durable Bolt server
uses the same executor and store.

The CLI uses configured `memory.query` as a [conservative allocation guard](query-allocation-guards.md).
An exceeded guard fails atomically. `LIMIT` stops read pipelines early.
`server.query_timeout_ms` sets a cooperative execution deadline, including
rollback on expiry. Caller mistakes (reserved properties, bad signatures, ACL
validation, unknown nodes, bad element ids) are reported as client errors
(`Neo.ClientError.Statement.SemanticError`); storage and IO failures stay
database errors.

A read pattern using an undeclared node label or relationship type produces no
matches. `OPTIONAL MATCH` preserves the input row with nulls for newly introduced
bindings; a zero-hop variable-length path can still match its starting node.
Schema validation remains strict for writes.

## How queries are planned

Each pattern starts from its cheapest node (the anchor), then expands:

1. a variable already bound earlier in the query;
2. `id(n)` / `elementId(n)` equality;
3. `entity_id` equality or `IN` (primary-key lookups), then an `entity_id`
   range or `STARTS WITH`;
4. an indexed property with `=`, `IN`, a string range or `STARTS WITH`;
5. a label scan (including numeric and other non-string ranges, until the
   scalar index can prove complete coverage).

`WHERE` conjuncts run right after the step that binds their variables.
A label scan expanded in one direction streams the adjacency column family
without reading the nodes, and property reads decode one field instead of the
whole row. `ORDER BY n.entity_id [WHERE n.entity_id > $after] LIMIT k` walks
the primary key in order and stops after `k` rows. A label scan of at least
50,000 nodes that ends in an aggregation, `ORDER BY ... LIMIT` or the final
`RETURN` is split across up to eight worker threads; results keep the
sequential order, and a visibility-restricted transaction filters each worker's
share with its policy. A visibility that hides nothing (`allow_all` without
`deny_all` or knowledge grants) runs ordinary read pipelines on the unrestricted
path. Procedure calls retain the policy so private ACL metadata and physical scan
counters stay protected; all visibility transactions remain read-only. `DISTINCT`
aggregates and cursor pages stay sequential. `UNWIND ... MERGE (n {entity_id: ...}) SET ...`
is fused into one node write per row when no value reads `n`.

### Writing fast queries

- Look nodes up by `entity_id`, or by an indexed property declared in the
  ontology (`indexed_fields`). An unindexed property predicate scans the label.
- Page with a cursor on the primary key, not `SKIP`:

  ```cypher
  MATCH (n:Customer) WHERE n.entity_id > $after
  RETURN n.entity_id, n.name ORDER BY n.entity_id LIMIT 1000
  ```

  Pass the last `entity_id` of each page as `$after`.
- Batch writes with `UNWIND $rows AS row MERGE (n:Label {entity_id: row.id})
  SET n.a = row.a`. Start with small batches, measure their memory/latency and
  increase only within the configured query budget.
- Filter early and anchor on the most selective node; a bound or
  `entity_id`-keyed node makes every later hop a seek.

## List and text size

```cypher
RETURN size([1, null, 3]) AS items, size('é🧠') AS characters
```

Returns `3` and `2`. `size(null)` returns null, including missing properties.
Strings count Unicode code points, not bytes or displayed grapheme clusters;
`size('é')` is `2` for the decomposed accent.

```cypher
MATCH (c:Customer) WHERE size(c.name) > 3 RETURN c.entity_id, size(c.name)
```

Exactly one argument is required. Numbers, maps, booleans and node values are
rejected. Lists can hold document chunks or vector components, but `size()`
does not provide vector similarity or ANN search. It also works in UNWIND,
SET and `count(size(...))`; aggregates inside `size()` remain unsupported.

The [Python/Bolt integration check](../benchmarks/archive/2026-10-08-size/python_bolt.py)
starts a disposable local server, verifies parameter sizes and a persisted SET,
and shuts down in `finally`. Build `brainwire-server`, then run
`.venv-brainwire/bin/python benchmarks/archive/2026-10-08-size/python_bolt.py`.

## Typed neighborhood reads

Outgoing and incoming typed hops seek the existing RocksDB adjacency key prefix
for the start node **and relationship type**, rather than visiting every edge
of that node and discarding other types. For example:

```cypher
MATCH (c:Customer {entity_id:$id})-[:WORKS_AT]->(company:Company)
RETURN company.entity_id
```

This is automatic in embedded queries and the Python/Bolt server, including
the eager matching path used by writes. It needs no additional index declaration
or storage migration. Snapshot visibility, pending transaction changes and
selected-edge order are preserved. `DETACH DELETE` still examines every type
and retains its existing conflict checks.

The gain depends on how many unrelated edges the start node has. This does not
change CSR projection or PageRank execution. See the
[mixed-type hub measurements](benchmarks/2026-10-06-typed-adjacency.md)
for the synthetic workload, raw samples and reproduction commands.

## String normalization

`toLower(expr)` and `toUpper(expr)` each accept exactly one string or null.
Function names are case-insensitive; numeric, Boolean, list, map and node inputs
raise an error rather than being coerced. Missing properties return null.
They compose with `coalesce`, `size`, predicates, SET and top-level `count`:

```cypher
RETURN toLower('OPEN') AS status, toUpper('straße') AS title
MATCH (c:Customer {entity_id:$id}) SET c.email=toLower(c.email)
MATCH (c:Customer {email:$normalized_email}) RETURN c.entity_id
```

The example returns `open` and `STRASSE`. Indexed SET removes the old equality
entry and writes the new value atomically; failure in a later row rolls back the
whole query/transaction. A predicate such as `WHERE toLower(c.email)=$email` does
not gain an equality seek from this function: normalize before writing and use
a plain indexed property predicate when possible.

Brainwire uses Rust whole-string Unicode casing, independent of locale. Unicode
expansion can change length (`toUpper('ß')` is `SS`), and whole-string lowercasing
preserves contextual Greek final sigma. It is not grapheme normalization,
case-insensitive collation or a promise of exact casing compatibility with every
JVM Unicode version. Null inputs propagate; non-string values are rejected.

Before conversion the existing budget/shared query pool reserves a deliberately
conservative allowance: 24 times input UTF-8 bytes plus value overhead. Total query
accounting also includes input/output and validation-time evaluations; it is not
simply one 24-times charge. Long strings can therefore be rejected despite lower
actual allocation. A measured normalization workload hitting this guard is the
trigger to refine it. Cooperative deadlines are checked before/after conversion;
one standard-library conversion can overshoot the deadline. No hard RSS or
immediate-cancellation guarantee is added.

## Nullable values with coalesce

`coalesce(expr, ...)` returns the first non-null argument, evaluated left-to-right.
It requires at least one argument and returns null when all arguments are null.
False, zero, empty text, lists/maps and node bindings are valid non-null results.
For example, `RETURN coalesce(null, 'normal') AS priority` supplies a default, and
`MATCH (t:Ticket) RETURN t.entity_id, coalesce(t.priority, 'normal') AS priority LIMIT 20`
works on optional ticket properties.

Later runtime evaluation stops after a result: `coalesce('normal', $missing)` does
not require the later parameter; `coalesce(null, $missing)` still fails. Variable
names and aggregate shapes in every argument are validated, even when a branch
is skipped or MATCH returns no rows. Existing nesting and allocation guards remain.

Protected Bolt sessions also check the complete parsed query against graph and
operation grants before execution. A read-only token cannot hide MERGE/SET behind
UNWIND or MATCH. See [service-token permissions](bolt-auth.md).
