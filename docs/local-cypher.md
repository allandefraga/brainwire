# Local Cypher queries

`brainwire cypher` runs the implemented Cypher subset against an embedded
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

## Implemented subset

- `CREATE` and `MERGE` nodes identified by label and `entity_id`; anonymous
  typed edges between bound nodes.
- Labeled `MATCH`, primary-key lookup or label scan, one outgoing/incoming hop,
  property equality and `WHERE` expressions.
- `UNWIND`, node property `SET`, and `DETACH DELETE`.
- `RETURN` values or nodes, top-level `count` aggregation and grouping,
  nonnegative integer `LIMIT`, parameters and three-valued boolean logic.
- Scalar `size(expression)` for lists and strings; null propagates.
- Read-only `CALL graph.project`, `graph.degree` and `graph.pagerank`, with
  explicit `YIELD`, aliases, optional `WHERE` and final `RETURN`/`LIMIT`.
  See [sparse analytics](sparse-analytics.md) for runnable examples and limits.

Unsupported constructs return an explicit error. Relationship variables and
relationship property filters are not implemented. The executor streams
supported read pipelines; writes, grouped aggregates,
CALL and final query results remain materialized. `memory.query` enforces
conservative accounting rather than a hard RSS ceiling. Full Cypher, authorization,
range/vector search and the connected TUI remain separate work. The
durable Bolt server uses the same executor and store.

The CLI uses configured `memory.query` as a [conservative allocation guard](query-allocation-guards.md).
An exceeded guard fails atomically. LIMIT stops supported read-only
MATCH/UNWIND pipelines early. `server.query_timeout_ms` sets a cooperative
execution deadline, including rollback on expiry.

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
