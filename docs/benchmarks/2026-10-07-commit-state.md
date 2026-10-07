# October 7: durable commits, resumable Python and mixed-load acceptance

All work was local. Data is synthetic and disposable. These checks establish
single-store commit/restart behavior; they do not implement replicated apply,
causal bookmarks, historical reads, DuckLake CDC or ANN search.

## Implementation

The store persists a versioned unsigned commit sequence in its synchronous
WriteBatch, loads it on reopen, and reserves its metadata key. Overflow,
malformed markers and attempted marker replacement fail closed. Read-only
completion does not advance it; internal allocator/ontology commits do.

`CALL db.commitState()` reports transaction-snapshot state. Offline
`db commit-state`, JSON overview and the TUI pipeline report latest local state.
All public numbers use exact decimal strings. Native checkpoint capture and
its sequence receipt share the commit lock, including a concurrent-writer test.
See [commit state](../commit-state.md) and [checkpoints](../checkpoints.md).

The [Python example](../../examples/python/resumable_customers.py) stores a file
SHA256 and byte cursor in the graph, atomically with each customer batch. It
requires immutable input and one importer per source ID. Retries reread the
cursor; a completed source is a read-only no-op. Defaults bound rows, bytes and
line length. This is an example, not a general SDK or source-change connector.

## Verification

- 434 Rust workspace tests passed; all-targets Clippy, rustfmt and diff checks passed.
- Nine Python algorithm tests passed without driver dependencies.
- Store tests include eight SIGKILL recovery cycles across the existing and new
  crash suites, reserved-key/overflow/write-failure atomicity, legacy opening,
  old snapshots, disjoint writers and eight checkpoints under writer contention.
- Real Bolt acceptance at 10K and 100K nodes keeps old snapshots stable, reads
  without advancing the position, commits 20 disjoint concurrent updates,
  kills an uncommitted indexed update, restarts and audits the graph/indexes.
  Checkpoint receipt, restored marker and captured data agree.
- Real resumable import raises after customer writes and before cursor update,
  verifies rollback, then commits a batch without delivering a receipt. SIGKILL
  and reopen recover its cursor and data. Completion/replay, source mismatch,
  lists/vectors and scalar-index checks all pass.

## Volume and measurement scope

Runs use macOS ARM64 and Neo4j Python driver 5.28.6 (plus a separate 10K
resumable/recovery check with driver 6.3.0), 64 MiB cache
and 16 MiB memtable budget. Those are component settings, not a hard RSS cap.
Raw JSON records profile, source shape, hashes and cleanup status.

| Run | Volume | Evidence |
| --- | --- | --- |
| Commit/restart/checkpoint | 10K and 100K small indexed customers | All recovery/snapshot/index assertions pass |
| Resumable import | 100K customers, 16 repeated-valued dimensions | Batches/cursor recover; final sequence 501 |
| Wide-value import | 100K customers, 768 seeded-random dimensions | 1,570,442,893-byte JSONL; batch input <=1 MiB; first batch 66 rows; final sequence 1617 |
| TUI | 100K customers, zero instance edges | Six real offline PTY captures; sequence 121 matches CLI; inspection does not advance it |

The wide-value import resumed its remaining 99,934 records in about 68.27 seconds
(~1,464 rows/s), including source rehashing and byte-bounded batches. It ran
alongside the independent mixed-load soak. Treat it as volume/failure acceptance,
not an isolated performance gate or vector-search benchmark. It stores vectors
as ordinary lists; no ANN index or search was exercised. The earlier debug
100K run also overlapped initial release compilation. Do not derive a before/
after speedup from differently loaded/profiled runs.

The mixed-load soak uses 100K customers, one 250-row update writer and four
point readers. Raw results record latency in each worker's last 10K samples,
commit counts, five-second RSS samples, final segment/index agreement and
post-SIGKILL reopening/audit. The 601.15-second run completed 172,253 committed
batches (43,063,250 row updates on the same 100K nodes) and 3,846,944 point reads.
Each reader's final-window p95 was 1.259–1.269 ms; the writer's final-window p95
was 3.97 ms per 250-row batch. These are last-10K-sample percentiles, not the
entire run's percentiles. Native server RSS peaked at 93.42 MiB and ended at
88.02 MiB; all final segment counts matched indexed counts, and the reopened
sequence was exactly 172752. This is observed memory behavior, not an RSS limit.

The independent wide-vector tests and TUI capture ran during parts of the soak,
so it intentionally includes some extra same-host contention. It is a ten-minute single-node check, shorter than
the roadmap's one-hour gate and outside its frozen 4-vCPU/8-GiB environment.

## Isolated short release ingestion baseline

A separate final run had no simultaneous soak or compilation: 100K new nodes,
1,024 bytes of deterministic SHA-derived name text per node, email/segment
scalar indexes, and 250-row commits. First/last payloads round-trip exactly.
It completed in 2.459 seconds (~40,661 rows/s including
client payload generation and a state query after each batch). Batch p95 was
7.027 ms. Snapshot, disjoint update, SIGKILL, checkpoint and audit
assertions still passed. This short same-host reference is not a sustained
insert benchmark or certification on the beta's pinned resource environment;
it does not isolate the sequence marker's overhead against an older binary.

## Reproduce

```sh
cargo build --release --locked --offline -p brainwire-server
uv run --no-project --with neo4j==5.28.6 python \
  docs/benchmarks/2026-10-07-commit-state/python_acceptance.py \
  --binary target/release/brainwire --rows 100000 --payload-bytes 1024
uv run --no-project --with neo4j==5.28.6 python \
  docs/benchmarks/2026-10-07-commit-state/resumable_acceptance.py \
  --binary target/release/brainwire --rows 100000 --dimensions 768 --varied-vectors
uv run --no-project --with neo4j==5.28.6 python \
  docs/benchmarks/2026-10-07-commit-state/mixed_soak.py --seconds 600
```

Each harness creates its own graph/server directories and removes them on exit.
Capture all five offline tabs and Portuguese pipeline using:

```sh
cargo build --locked --offline -p brainwire-server
uv run --with pyte --with pillow python \
  docs/design/assets/2026-10-07-commit-state/capture.py
```

Raw [results and executable harnesses](2026-10-07-commit-state/) and
[TUI captures/receipt](../design/assets/2026-10-07-commit-state/) preserve the
measurement/capture provenance. Next performance work is the
[bounded destination-read experiment](../design/2026-10-07-batched-node-read-spike.md),
with snapshot/order/LIMIT behavior and retained-byte policy preserved.
