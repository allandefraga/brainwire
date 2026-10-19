# High-degree adjacency collection — October 15, 2026

The `detach_delete` adjacency helper now uses `Txn::visit_prefix_for_update`
instead of `scan_prefix_for_update`. This comparison isolates that changed
piece: both methods scan the same persisted `Out` and `In` prefixes and build
the same `Vec<EdgeRef>`. It measures warm-cache collection and decoding only;
it excludes transaction creation/destruction, vector destruction, delete
staging, commit/WAL sync and end-to-end Cypher.

## Results

| Hub degree (out + in) | Out row payload | Materialized median | Streaming median | Scan speedup |
|---:|---:|---:|---:|---:|
| 20,000 (10k each way) | 0 bytes | 3.617 ms | 1.133 ms | 3.19× |
| 200,000 (100k each way) | 256 bytes | 43.440 ms | 14.628 ms | 2.97× |

Each scan row is nine paired samples with alternating method order. The 100k
fixture has 100,001 nodes, 200,000 logical relationships and 400,000 mirrored
adjacency records. Outgoing values contain a valid encoded row with a repeated-`x`
synthetic property; incoming mirror values are empty, matching the storage
layout. The `Item` nodes have no declared properties or secondary indexes, so
the full-delete result does not include indexed-property cleanup. The smaller
fixture uses a valid row with an empty-string payload. Exact edge vectors
matched and `check_invariants` passed before timing.

With `--full-delete`, the same isolated example also ran the production
`detach_delete` helper through transaction commit (including WAL sync) once
after the scan comparison:

| Hub degree (out + in) | Logical relationships removed | Full helper + commit |
|---:|---:|---:|
| 20,000 (10k each way) | 20,000 | 17.297 ms |
| 200,000 (100k each way) | 200,000 | 203.229 ms |

Each full-delete value is a single warm observation, not a latency percentile.
Both runs then verified the hub was absent, exactly `degree` peer nodes
remained, both adjacency column families were empty, and `check_invariants`
passed. The
[10k receipt](2026-10-15-detach-delete/full-delete-10k.json) and
[100k receipt](2026-10-15-detach-delete/full-delete-100k.json) retain full
configuration and scan samples.

The scan result supports the narrow claim that the streaming adjacency
collection is about three times faster for these fixtures. It avoids the
materializing method's temporary merged `BTreeMap` and copied key/value vectors.
The delete still retains `O(degree)` `EdgeRef` values and builds a write
overlay. The full-delete values are single observations in this warm local
store-helper benchmark; they exclude Cypher, Bolt and quorum and do not establish
production capacity, memory/RSS, standalone WAL throughput or distributed
quorum performance. Commit sync remained enabled; SST compaction and physical
disk-space reclamation may occur later.

## Reproduce

```sh
cargo run --release --locked --offline -p brainwire-store \
  --example detach_delete -- --degree 10000 --payload-bytes 0 --repeats 9
cargo run --release --locked --offline -p brainwire-store \
  --example detach_delete -- --degree 100000 --payload-bytes 256 --repeats 9
cargo run --release --locked --offline -p brainwire-store \
  --example detach_delete -- --degree 100000 --payload-bytes 256 --repeats 3 --full-delete
```

The example builds and flushes a temporary synthetic database, reopens it,
checks integrity and exact scan results, alternates the old and new scan order,
prints all samples as JSON, and removes the fixture on exit. Cache and memtable
budgets are 256 MiB and 32 MiB. The host was an Apple M5 Mac with 32 GiB RAM,
running rustc 1.98.1 (`aarch64-apple-darwin`). The [raw 10k receipt](2026-10-15-detach-delete/scan-10k-payload-0.json)
and [raw 100k receipt](2026-10-15-detach-delete/scan-100k-payload-256.json)
contain the full samples and configuration.

Fixture SSTs are created through RocksDB's default flush options before
reopening with Brainwire's configured cache and memtable budgets; they do not
recreate the production block/compression profile. Both scan methods read the
same SSTs, so their relative comparison stays matched, but this limits absolute
storage-throughput conclusions.
