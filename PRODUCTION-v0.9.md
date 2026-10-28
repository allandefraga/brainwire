# Production v0.9: progress and open qualification

Status: **not production ready**. A shared query reservation now stays charged
while Bolt retains or sends its results. This is not a process RSS ceiling.

## Reproduction and measurements

The v0.9 synthetic diagnostic uses four labels, one million identities per label,
four relationship types, eight writers, four mixed readers and optional delegated
visibility. It creates its own Docker volume and synthetic credentials. See
`scripts/bench_v09_container.md`. The exact dataset is related to the prior
v0.7 fixture but the mixed-load ordering and ACL stamping differ; numbers are
not necessarily comparable. The runs below are diagnostic, not certification.

| Measurement | v0.8 baseline | Branch |
| --- | --- | --- |
| 2 GiB mixed load, 1M identities per label | Stopped during load: writer pool exhaustion, 25,000 identities per label acknowledged | Stopped during load: writer pool exhaustion, 29,000 identities per label acknowledged |
| 2 GiB cgroup / sampled RSS peak | 317.3 / 287.8 MiB over 12.1 s | 362.2 / 326.4 MiB over 11.8 s |
| 4 GiB mixed load, 1M identities per label | Stopped during load: two writers received `QueryMemoryPoolExhausted`; 74,250 identities per label acknowledged across writers | Stopped during load: broad-neighborhood reader timed out; 98,750 identities per label acknowledged |
| Observed cgroup / sampled process RSS peak | 741.3 / 635.7 MiB over 15.31 s | 810.8 / 669.7 MiB over 47.30 s |
| 8 GiB mixed load, 1M identities per label | Stopped during load: scan and broad-neighborhood timeouts, 155,500 identities per label acknowledged | Stopped during load: broad-neighborhood timeout, 136,750 identities per label acknowledged |
| 8 GiB cgroup / sampled RSS peak | 1181.5 / 926.2 MiB over 68.6 s | 1108.4 / 904.1 MiB over 64.6 s |
| 30-minute 2/4/8 GiB mixed workload and `db check` | Not completed | Not completed |
| Kernel OOM attribution | Kernel log unavailable in unprivileged container | Not qualified |
| Full requested read, count and cursor metrics | Not reached | Not reached |

The reported acknowledged batches were not independently counted after the
failed loads, and no `db check` was performed for either run. Start host load averages (1/5/15 minutes) were 27.55/18.22/12.19 and
35.64/21.08/13.44 for 2 GiB, 13.23/10.99/8.05 and 21.45/14.03/10.24
for 4 GiB, and 32.54/21.83/13.98 and 42.26/27.25/16.73 for 8 GiB. These
single, high-load, different-length runs are not a valid performance comparison. AC power was
confirmed and low power mode was off. No thermal warning was reported, while
actual CPU thermal throttling was not independently established. No OOM event
was observed in available samples or Docker events; unavailable kernel evidence
means this does not prove no kernel OOM. It does not reproduce or rule out the
longer-running OOM described in the task.

## Completed

- Keep existing query-memory reservations charged through buffered Bolt
  results, partial PULL, network drain or response discard, including failure
  and disconnect paths. This is conservative accounting of a query reservation,
  not exact per-allocation accounting. `Session::handle` returns a guarded
  response batch; embedded Rust consumers must keep its iterator alive until
  yielded responses have been processed.
- Add a four-label diagnostic harness with isolated resources and separate
  delegated reader and writer credentials.

## Open before production

- Hard process memory limit, input/encoding buffers, RocksDB native allocations,
  allocator retention and Bolt streaming/backpressure; full memory profiling.
- 30-minute 2/4/8 GiB load and read qualification, verified kernel OOM evidence,
  representative populated-volume upgrade and recovery under active writes.
- Cypher compatibility, constraints, native Bolt TLS, live principal management,
  operational APIs, independent Python client and production documentation.
