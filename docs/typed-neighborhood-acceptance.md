# Typed neighborhood acceptance and latency observation

The fixture in `crates/server/tests/typed_neighborhood_acceptance.py` seeds a
fresh protected graph with one `Ticket` and `N-1` `TicketComment` nodes, all linked
outward through `HAS_COMMENT`. It reuses the shape of the connected TUI capture
fixture. The maximum is 100,000 nodes and 99,999 edges, with one high-degree
source. Each comment has only its entity ID and source ticket ID; this is not a
1 KiB document/property workload.

It imports fresh CREATE batches of 250 comments and their edges in explicit
transactions. It checks actual online counts and, after stopping the server,
exact offline node/edge counts and the graph/scalar-index audit. Credentials,
ports, store and server process belong exclusively to a private temporary
fixture and are removed in cleanup.

## What is measured

The query selects one source and one outgoing edge signature using typed
adjacency, then stops at LIMIT 20:

```cypher
MATCH (n:Ticket {entity_id:$id})-[:HAS_COMMENT]->(m:TicketComment)
RETURN m.entity_id AS entity_id LIMIT 20
```

Twenty requests warm the connection before timing. Each measured Python request
includes `session.run(...).data()` and consumes the complete 20-row Bolt result
using a reused authenticated connection. Every result must contain 20 unique
actual seeded comment IDs. The sequential default is 1,000 measured requests,
maximum 2,000. Receipts contain integer-microsecond nearest-rank p50/p95/p99,
minimum/maximum and a zero-error count after all assertions pass.

The fixture separately verifies a READ transaction while another authenticated
writer has a pending comment and edge. The writer sees its pending relationship;
the reader retains the previous count and sample. The writer rolls back and
final counts remain unchanged. Reader mutation is rejected with the exact
Forbidden error. This proves pending-write visibility and permission boundaries;
it does not introduce a concurrent committed-writer snapshot stress test.

The native CLI snapshot uses the same configured types and source, fetches one
extra row to detect further relationships, and renders at most 20. At 160×40,
all 20 distinct sampled IDs and `MORE AVAILABLE` must appear. Its text and SHA-256
are included in the receipt. Optional CLI timings cover a complete new process,
schema validation and two authenticated connections, existence query and
LIMIT-21 acquisition; they must be reported separately from the warm Python
LIMIT-20 query. The default is 20 CLI processes; set zero to omit repeated CLI
timing while retaining one validated snapshot.

## Reproduce

Build the release binary first. Run from the repository root with the pinned
Neo4j driver:

```sh
python3 crates/server/tests/typed_neighborhood_acceptance.py --self-check
uv run --no-project --with neo4j==6.3.0 python crates/server/tests/typed_neighborhood_acceptance.py --binary target/release/brainwire --nodes 1000 --iterations 100 --cli-iterations 3
uv run --no-project --with neo4j==6.3.0 python crates/server/tests/typed_neighborhood_acceptance.py --binary target/release/brainwire --nodes 100000 --iterations 1000 --cli-iterations 20
```

The allowed node range is 22..100,000 so every fixture has more than the 20-row
sample. Receipts identify the binary/ontology hashes, driver/OS, memory and
message limits, dataset size, import time, actual counts, snapshot, timings,
audit and cleanup. No real external application source or existing deployment is accessed.

This is a small-property, warm, sequential, localhost observation. Other work
on the host can affect timings. It is not a whole-graph, multi-hop, cold-cache,
large-document, concurrent production or distributed service-level guarantee.
LIMIT 20 bounds the returned sample; it does not represent full degree or all
neighbors. See [connected status](connected-status.md) for the TUI's 128 KiB wire
budget and total snapshot deadline.
