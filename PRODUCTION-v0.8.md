# Production v0.8: progress and remaining work

Status: **in progress; not production qualified**. The global memory ceiling,
client timeout, native Bolt TLS and online principal administration are open.

## Before / after: container-aware defaults

Single observations on an Apple Silicon host, Docker Desktop, AC power and low
power mode off; no builds or tests ran during either measurement. Host load
averages at start were 4.21/5.00/4.57 before and 5.43/5.32/4.79 after. The
baseline image was built from commit `2c7f90a`; the after image contains that
commit plus the memory-default changes in this branch. Tests used distinct
fresh volumes, a 4 GiB cgroup limit with no extra swap, one synthetic label,
one star-shaped relationship type, 1,000,000 nodes and 999,999 edges, eight
writers and four point readers. Batch size was 250; cursor pages were 1,000.
One run per configuration is diagnostic, not a statistical comparison or the
multiple-label workload that previously OOMed.

| Measurement | v0.7.0-beta.1 | Branch | Notes |
| --- | ---: | ---: | --- |
| Mixed ingestion | 18.63 s | 18.56 s | Includes driver, commits and concurrent readers |
| Cgroup memory peak | 970.6 MiB | 979.8 MiB | Includes file cache; different from RSS |
| Maximum sampled process RSS | 619.6 MiB | 685.3 MiB | Sampled, not a continuous RSS high-water mark |
| Post-load idle RSS | 286.8 MiB | 288.7 MiB | 60 s idle before crash |
| Post-recovery idle RSS | 619.6 MiB | 685.3 MiB | After cursor traversal and 60 s idle |
| Label count | 41.65 ms | 42.64 ms | One end-to-end query after recovery |
| Unlabelled `MATCH` with `LIMIT 37` | 2.47 ms | 4.50 ms | One end-to-end query after recovery |
| Keyset cursor (1M rows) | 15.19 s | 15.18 s | Separate transactions; not a snapshot cursor |
| Client transaction timeout | Not measured | Not measured | Remains unimplemented |
| 2 GiB container defaults: cache / memtables / query | 10 GiB / 1 GiB / 2 GiB | 512 MiB / 128 MiB / 256 MiB | Memtables are charged inside cache |

Both runs completed without observed cgroup OOM and passed recovery counts,
property samples and offline `db check`. The comparison does **not** establish
RSS enforcement, characterize the earlier 4 GiB OOM, test crash during writes,
prove timeout cancellation, exercise restricted ACLs or certify production.
The benchmark harness and invocation are in `scripts/bench_v08_container.md`.
Raw local receipts are diagnostic artifacts, not published benchmark data.

## Completed

- Detect cgroup v1/v2 limits and host RAM for unset memory settings; show the
  source in `config show`. Explicit file and environment values retain precedence.
  See `docs/memory-defaults.md` for ratios, compatibility and limitations.
- Add an isolated synthetic benchmark, with separate cgroup and process memory
  observations, recovery verification and an offline integrity check.
- A newly created **empty** v0.7.0-beta.1 graph volume passed `db check` under
  the new image. Upgrade with representative populated data remains unverified.

## Open

- Global memory accounting and retention: Bolt ingress/egress, result lifetimes,
  catalog, native RocksDB allocations and process RSS are not fully bounded.
- Native Bolt TLS with certificate rotation; online principal administration;
  requested Cypher compatibility, transaction deadlines, constraints, plans,
  online graph administration, backup/export, HTTP metrics and Python client.
- Repeat the requested multi-label workload, profiling and OOM reproduction;
  verify client timeout, populated-volume upgrades, failure cases and realistic ACLs.
