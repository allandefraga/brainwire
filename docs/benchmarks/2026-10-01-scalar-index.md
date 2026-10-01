# First scalar-index slice — 2026-10-01

Brainwire now writes physical `idx` keys for ontology-declared string and
Boolean properties. A key contains the node type, escaped property name,
type-tagged value and node ID. `MATCH` chooses that prefix when the index is
marked ready; all remaining predicates are rechecked against the node row.
The keys change in the same transaction as create, MERGE/SET and DETACH DELETE.
Tests cover duplicates, update, null removal, deletion, rollback, concurrent
writer conflict and reopen.

Applying an ontology backfills newly indexed fields and sets a persisted ready
marker atomically. A legacy catalog without the marker keeps using label scans
until reapplication. If more than 100,000 old or new keys would be handled,
the operation refuses the migration without changing the catalog; a resumable
bounded-batch backfill is still needed. Dropping a declaration disables the
planner path. Reintroducing it clears stale keys and rebuilds the field.

## Focused local measurement

A temporary 10,000-Customer graph had unique `email` (indexed) and `name`
(unindexed). One `UNWIND` transaction loaded it through the embedded CLI, then
five separate CLI processes queried the last value of each field. The temporary
directory was removed. The cache was warm, and each timing includes process
startup, RocksDB open, query and JSON output.

| Query | Median of five |
| --- | ---: |
| Indexed `email` equality | 90.14 ms |
| Unindexed `name` equality | 244.98 ms |

This one-host fixture shows that the index is used and lowers work even when
process-open time dominates. It is not a production latency or throughput
claim. The [earlier audit](2026-10-01-query-ontology-audit.md) measured the
same kind of declared email field before a physical index existed, but with a
different driver, dataset size and process lifetime, so the figures are not
directly comparable.

## Remaining gates

- Integer and float equality currently scan. Cypher considers some integer
  and float values equal, so separate type-tagged numeric keys would miss
  matches. Define a canonical numeric lookup encoding before indexing them.
- A 100,000-key atomic backfill is only a small-graph bridge. Use persisted
  generations, bounded batches, resume after crash, and `building/ready`
  states for large graphs. The current backfill scans nodes and can be costly
  even below the key cap.
- Add index statistics and an `EXPLAIN` path, then benchmark cold/warm Bolt
  queries, write amplification and mixed data at 1M+ nodes. Current Cypher
  execution still materializes output rows and has no enforced memory budget.
- `identifier_columns`, range predicates, list membership, full-text and
  vector similarity need distinct index contracts.
