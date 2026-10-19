# ACL read performance: 100k and 1M-node synthetic graphs

This local release-mode comparison measures ACL-governed Cypher reads before
and after borrowed point reads and reuse of the decoded node record. Each
fixture contains 100,000 `Customer` nodes with private ACL metadata, split
evenly between `alice` and `bob`. The indexed case uses seven segment values.
Every query asserts the expected visible count.

| Query | Baseline p50 | Updated p50 | Change |
| --- | ---: | ---: | ---: |
| Indexed equality count (7,143 visible) | 177.83 ms | 54.81 ms | 3.24× faster |
| Label count (50,000 visible) | 967.06 ms | 267.57 ms | 3.61× faster |
| Visible property count (50,000 visible) | 1,420.45 ms | 356.31 ms | 3.99× faster |

The fixture uses a 64 MiB RocksDB cache, 4 MiB memtables and a 256 MiB query
budget. Each process run has one warmup and records seven samples per query;
the table aggregates 14 samples from two runs. Baseline and updated runs were
executed in opposite orders to reduce run-order bias. Setup and database reopen
are excluded from timings.
The measured interval includes transaction begin, policy validation, query and
commit. Query memory reservation is not process RSS.

Reproduce from the repository root:

```sh
cargo run --release --locked --offline -p brainwire-cypher --example acl_reads -- 100000
```

The baseline uses source commit `ad35fd8202e0a651edb8d1a41b18370780367760`;
the updated binary uses the same HEAD plus the local working-tree changes in
this session. Both use the same benchmark example and fixture. SHA-256:

- Baseline: `66de8308b9dbc4652437affbc1b52165a5829697d8d013f0ac9a4e0ab37650ca`
- Updated: `b4e0cf7df7cd9d41f7d325a7831df82e7f8431cc358119d4fb3d80f3422c5c18`

Raw per-sample receipts: [baseline run 1](2026-10-14-acl-read-performance/baseline.json),
[updated run 1](2026-10-14-acl-read-performance/optimized.json),
[updated run 2](2026-10-14-acl-read-performance/optimized-repeat.json) and
[baseline run 2](2026-10-14-acl-read-performance/baseline-repeat.json).

The same harness also ran against 1,000,000 nodes. All visible counts were
asserted (71,429 for the indexed equality query; 500,000 for each full scan).
The p50 results were:

| Query | Baseline p50 | Updated p50 | Change |
| --- | ---: | ---: | ---: |
| Indexed equality count | 1,938.86 ms | 586.94 ms | 3.30× faster |
| Label count | 9,775.43 ms | 2,519.16 ms | 3.88× faster |
| Visible property count | 9,843.64 ms | 4,049.49 ms | 2.43× faster |

These p50 values also aggregate 14 samples across two runs in opposite orders.
Raw results: [baseline 1M run 1](2026-10-14-acl-read-performance/baseline-1m.json),
[updated 1M run 1](2026-10-14-acl-read-performance/optimized-1m.json),
[updated 1M run 2](2026-10-14-acl-read-performance/optimized-1m-repeat.json)
and [baseline 1M run 2](2026-10-14-acl-read-performance/baseline-1m-repeat.json).

To reproduce the larger run, pass `1000000` to the command above.

These are single-host synthetic measurements, useful for validating this hot
path but not production SLOs. They do not measure cold reads, concurrent load,
unrestricted queries, RSS, network latency or application-specific data distributions.
