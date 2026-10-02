# Sparse graph analytics through Cypher

Brainwire can build a CSR adjacency matrix from the local RocksDB graph and
run PageRank, inspect node degrees or compute directed reachability through
the existing `brainwire cypher`
command. These are implemented local procedures, not examples of future syntax.

## Directed reachability and shortest hop depth

```cypher
CALL graph.reachable({source:'a', sourceLabel:'Page', relationship:'LINK', maxDepth:3})
YIELD node, depth
RETURN node.entity_id, depth
```

The source is identified by label and stable `entity_id`; both are required.
The source appears at depth zero, and reachable nodes appear once at their
minimum directed hop count, in stable NodeId order. Cycles terminate. This is
an unweighted BFS over nonzero CSR entries, not weighted Dijkstra or a returned
path. `weightProperty` is rejected. All selected relationships have unit weight.
`maxDepth` defaults to 10 and accepts 1..1000. Unknown/missing sources are errors.

The existing `maxNodes` and `maxRelationships` projection limits apply, and the
full bounded projection is built before traversal; shallow traversal is not
a promise of an equally small projection. BFS uses O(nodes) temporary memory
and O(nodes + visited CSR entries) time. Projection and traversal observe the
same transaction snapshot and pending writes. Global query memory remains
unenforced; use the explicit projection caps.

The matrix is ephemeral. Each call rebuilds it from the query transaction's
snapshot and pending writes. `graph.project` returns a summary and releases the
matrix; it does not register a named projection, create a file or cache results.
The authoritative data remains in RocksDB. Reopening a database reconstructs
the same matrix from its records; projection recovery is also checked after
forced writer termination in the store crash test.

## Run the complete example

From the repository root, build the executable:

```sh
cargo build --locked --offline -p brainwire-server --bin brainwire
```

Initialize a synthetic graph. `MERGE` makes the example repeatable:

```sh
BRAINWIRE_MEMORY_CACHE=64MiB BRAINWIRE_MEMORY_MEMTABLES=16MiB \
./target/debug/brainwire cypher --database ./data/analytics-demo \
  --ontology examples/analytics/schema.json \
  --query "MERGE (a:Page {entity_id:'a', title:'Introduction'})
           MERGE (b:Page {entity_id:'b', title:'Storage'})
           MERGE (c:Page {entity_id:'c', title:'Recovery'})
           MERGE (a)-[:LINK {weight:2}]->(b)
           MERGE (a)-[:LINK {weight:1}]->(c)
           RETURN a.title, b.title, c.title"
```

Query the stored graph in a new process:

```sh
BRAINWIRE_MEMORY_CACHE=64MiB BRAINWIRE_MEMORY_MEMTABLES=16MiB \
./target/debug/brainwire cypher --database ./data/analytics-demo \
  --query "CALL graph.pagerank({relationship:'LINK', weightProperty:'weight'})
           YIELD node, score, iterations, converged
           RETURN node.title AS title, score, iterations, converged LIMIT 10"
```

`Storage` ranks above `Recovery`, which ranks above `Introduction`. Scores sum
to approximately one across the full result. The output is ordinary CLI JSON
with `fields` and `rows`. Returning `node` directly uses the usual node envelope.

## Procedure reference

The supported shape is:

```cypher
CALL graph.degree({relationship:'LINK'})
YIELD node AS page, outDegree AS outgoing, inDegree
WHERE outgoing > 0
RETURN page.title, outgoing, inDegree
LIMIT 10
```

Calls accept zero arguments or one options map (including a `$parameter`);
`graph.reachable` requires a map specifying `source` and `sourceLabel`.
Explicit `YIELD` fields and final `RETURN` are required. Keywords ignore case;
procedure names and option/field names use the casing shown here. Only one call
followed by one `RETURN` is supported per query. Standalone calls, `YIELD *`,
`WITH`, `ORDER BY`, multiple calls and calls mixed with writes in the same query
are not implemented. Earlier runs within a `QueryTransaction` may write data;
analytics sees those staged changes and rollback still discards them.

| Procedure | Available YIELD fields | Behavior |
| --- | --- | --- |
| `graph.project` | `nodeCount`, `relationshipCount`, `scannedRelationshipCount`, `nonZeroCount`, `csrBytes` | One summary row, including for an empty graph. |
| `graph.degree` | `node`, `outDegree`, `inDegree` | One row per node in NodeId order; counts selected relationships, including parallel edges, self-loops and zero-weight edges. |
| `graph.reachable` | `node`, `depth` | Each reachable node once in NodeId order, including source at depth zero; minimum unweighted directed hop depth. |
| `graph.pagerank` | `node`, `score`, `iterations`, `converged`, `residual` | One row per node, descending score then ascending NodeId for ties. `LIMIT` selects from that order. Empty graphs return no rows. |

`RETURN count(*)` and the existing projection/aggregation rules also work.
Filtering with `YIELD ... WHERE` filters results after the algorithm; it does
not restrict the graph used to calculate scores. `LIMIT` likewise does not
reduce the projection size or bypass validation/limits.

## Options and limits

| Option | Procedures | Default | Accepted values |
| --- | --- | --- | --- |
| `relationship` | All | All types | Existing relationship type name. |
| `maxNodes` | All | 100,000 | Integer 1–1,000,000. Counts all nodes, including isolated nodes. |
| `maxRelationships` | All | 1,000,000 | Integer 1–10,000,000. Bounds all outgoing records scanned, before type filtering. |
| `weightProperty` | project, pagerank | Unit weights | Nonempty property name on relationships. |
| `source` | reachable | Required | Stable entity_id of the starting node. |
| `sourceLabel` | reachable | Required | Existing node label of the starting node. |
| `maxDepth` | reachable | 10 | Integer 1–1,000; source is returned at depth 0. |
| `maxIterations` | pagerank | 100 | Integer 1–1,000. |
| `dampingFactor` | pagerank | 0.85 | Finite number in [0, 1]. |
| `tolerance` | pagerank | 0.00000001 | Finite nonnegative L1 residual threshold; zero disables early stopping. |

Unknown options, unknown output fields, duplicate aliases and invalid arguments
are errors, including on an empty graph. A failed call poisons an explicit
query transaction and discards its staged writes, matching existing Cypher
error behavior. Analytics uses snapshot isolation; it does not add serializable
range dependencies to a mixed read/write transaction.

Missing weight properties default to 1. Explicit weights must be numeric,
nonnegative, finite and representable as a nonzero float32 when positive.
Both catalog columns and flexible extra properties are supported. Zero weights
have no matrix entry and no influence on ranking. Multiple relationships with
the same source and target have their weights summed; a float32 sum overflow
is rejected. Therefore `relationshipCount` can exceed `nonZeroCount`.

PageRank normalizes outgoing weights per source. Nodes with no positive outgoing
weight redistribute their rank uniformly to all nodes. Accumulation uses
float64; topology weights use float32. `iterations` is the number actually run,
and `residual` is the last L1 score change. With tolerance zero, `converged` is
false because convergence checking was disabled. Damping 1 is accepted but
convergence is not guaranteed for every graph.

## Memory and durability boundaries

The store scan visits the transaction snapshot and write overlay in key order,
without materializing all node/relationship property values. The projection,
CSR construction buffers and results still live in memory. Cardinality limits
are guardrails, **not a hard process or query memory budget**.

`csrBytes` measures allocated CSR vector capacities: row offsets, column indices
and float32 values. It excludes NodeId mapping, degree arrays, build buffers,
algorithm buffers, result rows, allocator overhead and RocksDB. It is not RSS.
On a 64-bit target, packed CSR with N nodes and E nonzero coordinates needs
approximately `8 * (N + 1) + 12 * E` bytes. Building it has a higher peak.

This is single-process analytics. The embedded CLI needs exclusive database
access; the Bolt server can run the same procedures against a selected named
database. There is no distributed execution, persistent projection registry,
vector similarity index, permission filtering or live graph TUI integration
yet. Production deployment still needs enforced memory budgets.

## Reproduce the volume workload

```sh
cargo build --locked --offline -p brainwire-cypher --example sparse_analytics
/usr/bin/time -l ./target/debug/examples/sparse_analytics 100000
/usr/bin/time -l ./target/debug/examples/sparse_analytics 1000000
```

`time -l` is macOS-specific. The example itself works without that wrapper.
It creates a temporary database, imports through normal graph APIs in bounded
transactions, closes/reopens it, builds the matrix, computes PageRank, and runs
an end-to-end Cypher top-five query. It checks counts, score mass and the expected
top node, prints JSON timings, and removes its own temporary database on exit.
The workload combines ring links and a common hub, so PageRank is nonuniform.
It uses a 64 MiB RocksDB cache and 16 MiB memtable budget. Results from the first
measured run are in [the benchmark report](benchmarks/2026-09-30-sparse-analytics.md).
