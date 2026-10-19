# Cypher reads on 100k nodes — October 15, 2026

One release-mode run of the existing embedded `streaming_reads` workload on a
fresh temporary RocksDB store with 100,000 synthetic indexed `Customer` nodes.
It used a 64 MiB block cache, 4 MiB memtable budget, 64 KiB query allocation
budget, one warmup and five measured samples. The results below are the median
and maximum among those five samples; the query allocator figures are
reservations, not process RSS.

| Operation | Median | Maximum | Peak query reservation |
|---|---:|---:|---:|
| Label scan, `LIMIT 1` | 0.006 ms | 0.012 ms | 5.7 KiB |
| Indexed equality, `LIMIT 1` | 0.010 ms | 0.012 ms | 6.0 KiB |
| Indexed equality count | 20.799 ms | 21.460 ms | 5.7 KiB |
| Full label count | 74.833 ms | 77.709 ms | 5.3 KiB |
| Late non-indexed match, `LIMIT 1` | 139.241 ms | 141.531 ms | 5.9 KiB |

The example built/imported the 100,000 nodes in 320.734 ms in this run. This is
a warm embedded microbenchmark, not a server, cold-storage, multi-process or
distributed result. The selective indexed lookup and early-return scans are
fast; a late non-indexed match still has to traverse almost the entire label.
The measurements do not include WAL commits because this workload is read-only
after fixture construction.

Reproduce with:

```sh
cargo run --release --locked --offline -p brainwire-cypher \
  --example streaming_reads -- 100000
```

The [raw JSON](2026-10-15-streaming-reads-100k.json) includes configuration,
individual samples, import time and reservation measurements.
