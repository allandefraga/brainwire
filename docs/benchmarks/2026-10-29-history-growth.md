# Conflict-history deque growth diagnostic

Scope: opt-in unit-test mechanism diagnostic, not a graph database benchmark,
production qualification or a release throughput comparison. Both variants
include the candidate 64 MiB conflict-history accounting. Only deque growth
policy differs: reserve-exact-one versus budget-aware amortized growth.

Command after building the store test binary:

```sh
cargo test -p brainwire-store --lib --locked --offline \
  conflict_history_growth_diagnostic -- --ignored --nocapture
```

The timed section measures the real `CommitState::record_history` call for
16,384 small distinct-key commits while an old writer pins history. It excludes
RocksDB, fsync, networking, the Python client and transaction overlay creation.
Seven rounds run per variant, after compilation has completed. The build uses
`CARGO_PROFILE_DEV_DEBUG=0`; it remains an unoptimized development build.

| Median across 7 rounds | Exact-one growth | Amortized growth |
| --- | ---: | ---: |
| Total timed record-history work | 13.880 ms | 13.833 ms |
| Per-call p50 | 833 ns | 833 ns |
| Per-call p99 | 1,167 ns | 1,167 ns |
| Capacity changes per round | 16,384 | 13 |
| Final estimated retained bytes | 5,734,400 | 5,734,400 |

The timing difference is too small to claim a speedup. The reproducible
improvement is the number of deque capacity changes, with unchanged recorded
retention in this fixture. The allocator may extend allocations in place, so
capacity changes alone do not prove copies or measured quadratic execution.
No system-wide power/thermal/load qualification was collected for this narrow
diagnostic, and it does not meet the full benchmark/regression acceptance gate.

Raw receipts in the local session:
- [exact log](2026-10-29-history/exact.log)
- [amortized log](2026-10-29-history/amortized.log)

Correctness regression: the exact-one implementation changed capacity 4,096
times for 4,096 commits and failed the new bounded-growth test. The amended
implementation passes its <=16 changes assertion and a near-budget test that
retains a fitting commit using exact reservation instead of discarding it.
