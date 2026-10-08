# Bounded destination-read batching: next performance experiment

Status: investigation only, October 7. No production batching is implemented.
The [October 8 single-value pinned-read experiment](../benchmarks/2026-10-08-pinned-read.md)
did not pass its performance gate under host contention; its production candidate
was reverted. The copied point-read baseline remains in use.
This follows the [October 6 dense-hop profile](../benchmarks/2026-10-06-typed-adjacency.md):
most samples were in destination node point reads, rather than codec decoding.
Typed adjacency already seeks the exact source/type prefix.

## Current path

`crates/cypher/src/exec/graph.rs::visit_pattern` streams matching edge references
and calls `matches` for each destination. `node_record` uses
`brainwire_store::graph::get_node`, which calls `Txn::get` against the RocksDB
snapshot after checking the pending overlay. Even an empty destination pattern
checks that the node exists and decodes successfully. Preserve these checks.

A whole-label scan can help dense label-wide joins, but scans irrelevant nodes
for sparse neighborhoods and complicates edge ordering, multiplicity and LIMIT.
Start by comparing batched point access, without adding another physical index.

## Experiment before integration

Use the locked local rocksdb 0.25 APIs, verified in its cached source:

1. Existing `Snapshot::get_cf` per target.
2. `Snapshot::multi_get_cf`, which preserves request order and returns copied
   `Vec<u8>` results bound to that snapshot.
3. `DB::batched_multi_get_cf_opt` with `ReadOptions::set_snapshot` explicitly
   set to the transaction snapshot. It returns pinned slices; the Snapshot
   wrapper has no corresponding batched wrapper. Never accidentally use latest
   DB reads when testing this path.

Compare chunks of 16, 64 and 128 keys on the existing 50K dense-target fixture,
plus empty, representative 1 KiB, 4 KiB and skewed large-record payloads. Include
sparse early-LIMIT queries. Use release binaries with recorded hashes, paired
warm repetitions and separate cold results. Report p50/p95, retained/pinned
bytes and process RSS, not only elapsed time. Keep alternate-source builds in
separate target directories; the earlier shared-target artifact issue must not
invalidate the baseline again.

## Semantics that must stay intact

- Resolve overlay put/delete entries first, including staged tombstones; retrieve
  remaining keys from the same snapshot. Restore input order and duplicates.
- Preserve one callback per edge, including several edges to the same target.
- Keep per-key RocksDB errors as positional results. Decode/check existence only
  when that destination is consumed, in the original order.
- A missing/corrupt target before the LIMIT cutoff must still fail. A trailing
  target after the cutoff must not produce a new application-level error merely
  because it was prefetched. Physical I/O can overread to a chunk boundary;
  semantic checks must not run past the cutoff.
- Stop before requesting another chunk after callback termination. Track raw
  buffer and decoded-node charges through existing accounting/deadlines.
- Test old snapshots under concurrent writes, pending create/update/delete,
  duplicate targets, rollback, type mismatch, missing/corrupt targets before and
  after LIMIT, and per-key storage errors where injectable.

## Byte-bound decision

A fixed key count is **not a result-byte bound**. The current store has no
persisted maximum node-record size. Copied multi-get can allocate all record
bytes before decoding; pinned results still retain/cache those values. Charging
memory after decoding does not prevent this overfetch.

The spike must expose this cost and choose a policy before production use:
validated record-size limits, a conservative one-at-a-time fallback, or another
measured approach with an explicit retained-byte contract. A new size index adds
its own atomicity and I/O obligations and should not be introduced speculatively.

Proceed only if batching yields a material repeatable gain, all ordering/error/
snapshot tests pass, and the chosen byte policy fits the query budget. If it
only improves the empty-property synthetic hub or changes LIMIT behavior, keep
point reads and record the result. This experiment does not implement temporal
history, source change capture or deterministic replicated mutation application.
