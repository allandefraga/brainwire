# Secondary-index implementation gate

This is the next query-scale slice after the
[2026-10-01 audit](../benchmarks/2026-10-01-query-ontology-audit.md).
The ontology records `indexed_fields`, `er_fields` and
`identifier_columns`. The first implementation slice now stores string and
Boolean equality keys in `idx`, updates them with node writes, selects them
for `MATCH`, and atomically backfills at most 100,000 index keys during an
offline ontology application. `identifier_columns`, numeric/range indexes and
larger resumable backfills remain pending. `db schema --json` distinguishes
`string_boolean_ready` from a legacy `declared_only` catalog.

## Correctness contract

- Start with exact equality on non-null Boolean and string values. Integer
  and float need a shared encoding consistent with Cypher's cross-type numeric
  equality; until then both retain a scan. Defer list/map membership and vector
  search to separate index designs.
- An `idx` key identifies `(node type, property name, type-tagged encoded
  value, node ID)`. Property names, rather than column IDs, also cover
  indexed properties stored in a row's flexible map.
  The common prefix through `encoded value` yields all matching node IDs.
  Duplicate values are permitted. `ident` needs its own semantics for
  cross-type identifiers; do not infer uniqueness from `indexed_fields`.
- Node create, MERGE/SET property changes and DETACH DELETE remove obsolete
  index entries and insert new ones in the **same** Brainwire transaction as
  the node row and primary-key change. A failed commit changes neither side.
  Reads from a snapshot must see matching rows and index entries.
- Adding an indexed field to an existing ontology requires backfill. The
  first slice uses one atomic transaction capped at 100,000 keys and refuses
  larger migrations without changing the catalog. The next step is bounded
  batches with a persisted generation and `building/ready` state; the planner
  must ignore `building`. On crash, resume or discard and rebuild that
  generation before marking it ready.
  Removing an index declaration must not leave the planner using stale keys.
- Plan `MATCH` with `entity_id` through `pk` first, then one ready exact scalar
  property index, else a label scan. Apply remaining predicates after the
  candidate lookup. Expose the chosen path in an explain/diagnostic command.

## Acceptance tests

1. Compare indexed and forced-scan results on generated mixed-type values,
   updates, deletions and duplicate values, before and after reopen.
2. Reapply an ontology with a new indexed field to populated data. Kill during
   backfill at several checkpoints; restart and verify that queries either use
   a complete ready index or scan correctly. No partial index may be used.
3. Test two concurrent writers updating the same indexed property, including
   rollback and commit conflict. Verify node/index invariants after each.
4. Benchmark 10K, 50K, 1M and larger datasets through Bolt with warm and cold
   cache. A selective equality query should scale with the matching set and
   RocksDB seek cost, not with the number of nodes of that label. Record write
   amplification and backfill duration as well as read latency.

The current query executor materializes candidates and results. Index lookup
alone will not solve high-degree expansions, full label scans, `UNWIND` batches
or the 16-GiB query-memory goal; streaming and budget enforcement follow.
