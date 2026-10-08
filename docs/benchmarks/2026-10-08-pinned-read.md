# Single-node pinned-read experiment — October 8

**Decision: do not integrate the candidate.** The experiment removes a temporary
encoded-record `Vec` copy by decoding inside `Snapshot::get_pinned_cf`. It
preserved snapshot, overlay/tombstone and error behavior, but did not pass the
repeatable performance gate. Production still uses `Txn::get` for node reads.
The [candidate patch](2026-10-08-pinned-read/candidate.patch) is evidence, not an
implemented feature.

## Workload and controls

The existing `typed_adjacency` example now accepts an optional payload-byte
argument (0–16384). Fixtures contain 10,001 nodes / 200,020 edges with empty or
4096-byte text properties, and 50,001 nodes / 1,000,020 edges with 4096-byte
properties. Text is deliberately repeated `x`, highly compressible synthetic
data; this is not representative document compression or vector search.
Each build finishes a full integrity audit.

Canonical baseline: `7dcd8766bba065480efed780ea195a02dbc3a399`, with the same
payload-enabled benchmark source in both binaries. Built the baseline release
executable, copied it outside Cargo's target directory, then changed only the
store candidate and built/copied the candidate. No alternate checkout/shared
build artifacts were used. [Hashes](2026-10-08-pinned-read/provenance.txt) identify
both executables and Cargo.lock. Apple M5, 32 GiB RAM, rustc 1.98.1, locked
RocksDB 0.25.0. Budgets: 64 MiB cache, 16 MiB memtables, 64 MiB query accounting.

Five initial paired rounds and ten subsequent paired rounds alternate execution
order. Each process reads the same fixture, with five warmups and thirty timed
samples per query, checking exact result counts. No engine build or workspace
tests ran during the subsequent rounds. **Other user workloads were active**:
a spot observation at 07:24 UTC showed a virtualization process at ~377% CPU
and another project's Python at ~99%. They were left running. Thus these are
contended warm embedded measurements, excluding open/import/network; the
subsequent rounds cannot be claimed to be an isolated host benchmark.

## Dense query result

Values below are medians of independent process p50s, not pooled percentiles.
All raw p50/p95 and thirty samples per process are retained in the evidence
folder; [summary](2026-10-08-pinned-read/summary.json) keeps both sets separately.

| Fixture | Initial baseline / candidate p50 ms | Subsequent baseline / candidate p50 ms |
| --- | ---: | ---: |
| 10K empty | 9.088 / 8.683 | 11.633 / 11.022 |
| 10K, 4 KiB | 10.807 / 9.709 | 13.119 / 15.365 |
| 50K, 4 KiB | 81.691 / 79.026 | 94.299 / 94.319 |

The favorable initial ~10% median reduction at 10K/4KiB did not repeat. The
subsequent empty-record median p95 changed from 15.745 to 17.654 ms; payload
p95 from 20.482 to 20.724 ms. Both noise and regressions prevent a reliable
latency claim. No allocation/RSS improvement was measured. The callback held
one pin at a time and decoded into owned values; it did not bound record size
or decoded memory, and was not a batched read implementation.

Candidate validation passed 435 workspace tests, store Clippy, snapshot across
update/delete, staged put/delete, missing/corrupt records, callback failure and
early LIMIT before a bad neighbor. Correctness alone did not override the gate.

## Reproduce

```sh
cargo build --release --locked --offline -p brainwire-cypher --example typed_adjacency
./target/release/examples/typed_adjacency build /tmp/brainwire-perf-fresh 10000 4096
./target/release/examples/typed_adjacency run /tmp/brainwire-perf-fresh
python3 docs/benchmarks/2026-10-08-pinned-read/summarize.py
```

Use a fresh fixture path. For a comparison, save immutable executables before
and after applying the candidate with `git apply --unidiff-zero` on its patch;
run them serially on the same closed store.
The summarizer regenerates the recorded summary, not new timing runs. A future
retry needs an idle controlled host, incompressible/skewed payloads and actual
allocation/RSS evidence before changing production reads or introducing batching.
