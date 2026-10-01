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
catalog while the server is stopped. `indexed_fields`, `er_fields` and
`identifier_columns` remain declarations only. `indexed_fields` and
`er_fields` now create physical equality indexes for string and Boolean
values in `idx`, with a bounded atomic backfill on ontology application.
Numeric, list and map predicates still scan the label. `entity_id` has its
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

This still materializes the parameter file and query rows in memory; file input
does not add streaming or enforce the configured query-memory ceiling.

## Implemented subset

- `CREATE` and `MERGE` nodes identified by label and `entity_id`; anonymous
  typed edges between bound nodes.
- Labeled `MATCH`, primary-key lookup or label scan, one outgoing/incoming hop,
  property equality and `WHERE` expressions.
- `UNWIND`, node property `SET`, and `DETACH DELETE`.
- `RETURN` values or nodes, top-level `count` aggregation and grouping,
  nonnegative integer `LIMIT`, parameters and three-valued boolean logic.
- Read-only `CALL graph.project`, `graph.degree` and `graph.pagerank`, with
  explicit `YIELD`, aliases, optional `WHERE` and final `RETURN`/`LIMIT`.
  See [sparse analytics](sparse-analytics.md) for runnable examples and limits.

Unsupported constructs return an explicit error. Relationship variables and
relationship property filters are not implemented. This is an eager MVP
executor: query results are materialized, and `memory.query` does not enforce
a query memory ceiling yet. Full Cypher, authorization,
indexed property search and the connected TUI remain separate work. The
durable Bolt server uses the same executor and store.
