# Secondary-index implementation gate

This is the next query-scale slice after the
[2026-10-01 audit](../benchmarks/2026-10-01-query-ontology-audit.md).
The ontology already records `indexed_fields`, `er_fields` and
`identifier_columns`, but only `entity_id` currently has a physical lookup.
`brainwire db schema --json` therefore reports `declared_only`.

## Correctness contract

- Start with exact equality on non-null scalar values (Boolean, integer,
  finite float and string). Define a stable type-tagged encoding so distinct
  types and values cannot collide. Defer list/map membership and vector search
  to separate index designs; never use a scalar index for them silently.
- An `idx` key identifies `(node type, column ID, encoded value, node ID)`.
  The common prefix through `encoded value` yields all matching node IDs.
  Duplicate values are permitted. `ident` needs its own semantics for
  cross-type identifiers; do not infer uniqueness from `indexed_fields`.
- Node create, MERGE/SET property changes and DETACH DELETE remove obsolete
  index entries and insert new ones in the **same** Brainwire transaction as
  the node row and primary-key change. A failed commit changes neither side.
  Reads from a snapshot must see matching rows and index entries.
- Adding an indexed field to an existing ontology requires backfill. The
  current administration path holds exclusive database access, so the first
  implementation can backfill offline in bounded batches. Persist a generation
  and `building/ready` state; the planner must ignore `building`. On crash,
  resume or discard and rebuild that generation before marking it ready.
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
