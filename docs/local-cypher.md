# Local Cypher queries

`brainwire cypher` runs the [supported Cypher](#supported-cypher) against an embedded
RocksDB database on this computer. Each successful query commits its writes
atomically; an execution error discards that query's staged graph changes.
Separate command invocations reopen the same directory and see committed data.
This command is separate from the synthetic `shell --demo` interface.

Build from the standalone repository:

```sh
cd /Users/fraga/Documents/brainwire
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

The output is JSON with `fields` and `rows`. Scalars, lists and maps use normal
JSON values. A returned node has an explicit envelope with `type: "node"`,
`id`, `label` and `properties`; its properties include the virtual `entity_id`.
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

92 read queries return identical results on Brainwire and Neo4j 5
(`crates/server/tests/differential.py`, synthetic data). Everything below is
implemented; anything else returns an explicit error.

**Clauses.** `MATCH`, `OPTIONAL MATCH`, `WHERE`, `WITH` (`DISTINCT`, `WHERE`,
`ORDER BY`, `SKIP`, `LIMIT`, aggregation mid-query), `RETURN` (same options,
plus `*`), `UNWIND`, `UNION` / `UNION ALL` (the two cannot be mixed in one
query), `CREATE`, `MERGE ... ON CREATE SET / ON MATCH SET`, `SET`
(`n.p = v`, `n += map`, `n = map`, `n:Label`), `REMOVE`, `DELETE`,
`DETACH DELETE` (nodes, relationships, paths), `CALL ... YIELD ... WHERE`, and
`CREATE INDEX` / `DROP INDEX` (mapped to catalog procedures).

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

**Aggregates.** `count`, `sum`, `avg`, `min`, `max`, `collect`, `stdev`,
`stdevp`, `percentileCont`, `percentileDisc`, with `DISTINCT` and grouping.

**Procedures.** `db.labels`, `db.relationshipTypes`, `db.propertyKeys`,
`dbms.components`, `db.schema`, `db.indexes`, `db.commitState`,
`db.checkpoint`, `db.memory`, `db.setNodeAcl(s)`, `db.nodeAcls`,
`graph.project`, `graph.degree`, `graph.pagerank` and `knowledge.rank`. See
[sparse analytics](sparse-analytics.md) and
[explainable knowledge ranking](knowledge-ranking.md).

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
`exists(pattern)` or a pattern predicate), pattern comprehensions, regex
`=~`, `FOREACH`, `LOAD CSV`, temporal and spatial types, and aggregates
inside `size()`. Full Cypher, range/vector search and ANN remain separate
work.

Whole relationship `SET` is not available; create or update edge properties
with `MERGE (a)-[:TYPE {prop: value}]->(b)`. `DELETE r` removes the matched
relationship, preserves endpoint nodes and respects graph write authorization.

The executor streams read pipelines; writes and the final result stay
materialized. `memory.query` enforces conservative accounting rather than a
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

## How queries are planned

Each pattern starts from its cheapest node (the anchor), then expands:

1. a variable already bound earlier in the query;
2. `id(n)` / `elementId(n)` equality;
3. `entity_id` equality or `IN` (primary-key lookups), then an `entity_id`
   range or `STARTS WITH`;
4. an indexed property with `=`, `IN`, a range or `STARTS WITH`;
5. a label scan.

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
  SET n.a = row.a`, 500 to 1000 rows per query.
- Filter early and anchor on the most selective node; a bound or
  `entity_id`-keyed node makes every later hop a seek.

## List and text size

```cypher
RETURN size([1, null, 3]) AS items, size('é🧠') AS characters
```

Returns `3` and `2`. `size(null)` returns null, including missing properties.
Strings count Unicode code points, not bytes or displayed grapheme clusters;
`size('é')` is `2` for the decomposed accent. This follows the
[Cypher scalar function contract](https://neo4j.com/docs/cypher-manual/current/functions/scalar/#functions-size).

```cypher
MATCH (c:Customer) WHERE size(c.name) > 3 RETURN c.entity_id, size(c.name)
```

Exactly one argument is required. Numbers, maps, booleans and node values are
rejected. Lists can hold document chunks or vector components, but `size()`
does not provide vector similarity or ANN search. It also works in UNWIND,
SET and `count(size(...))`; aggregates inside `size()` remain unsupported.

The [Python/Bolt integration check](benchmarks/2026-10-08-size/python_bolt.py)
starts a disposable local server, verifies parameter sizes and a persisted SET,
and shuts down in `finally`. Build `brainwire-server`, then run
`uv run --no-project --with neo4j==5.28.6 python docs/benchmarks/2026-10-08-size/python_bolt.py`.

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
Neo4j/JVM Unicode version. Null and string-only semantics follow the
[Cypher string function contract](https://neo4j.com/docs/cypher-manual/current/functions/string/).

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
