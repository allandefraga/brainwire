# 2026-10-15 graceful drain qualification

Native macOS arm64 release binary
`8662a2326e9983390c83e8ba1cf9519b3af66f2e6025695257f9e71b8eda45f5`, loopback,
synthetic data, protected service tokens. Behaviour and fixture are described in
[termination recovery](../termination-recovery.md).

| Run | Result | Exit codes (5 stops) | Exit seconds |
| --- | --- | --- | --- |
| Termination fixture, driver 6.3.0 × 2 | PASS ×2 | 0, 1, 1, 1, 0 | 0.005–0.032, 3.021–3.064 ×3, ≤0.002 |
| Termination fixture, driver 5.28.6 × 2 | PASS ×2 | 0, 1, 1, 1, 0 | 0.029–0.040, 3.012–3.041 ×3, ≤0.005 |
| Quorum fixture, 200 rows, three voters | PASS (17 checks) | 0, 0, 0 on final SIGTERM | 0.002–0.005 |

The previous build `301346eb…` differed only in installing the signal handlers
before the stores open; it passed the same fixture 8/8 (4 per driver, new
connections refused 0.023–0.032 s after the signal) and the quorum fixture.
Grace was 3,000 ms. In every run the unacknowledged RUN committed nothing; the
fixture accepts either atomic outcome. One quorum attempt on `8662a232…` failed
before any write with `EADDRNOTAVAIL` (ephemeral ports exhausted while another
three-voter benchmark ran concurrently); the rerun passed. Receipts:
[driver 6](../../benchmarks/archive/2026-10-15-graceful-drain/termination-driver6.json),
[driver 5](../../benchmarks/archive/2026-10-15-graceful-drain/termination-driver5.json),
[quorum](../../benchmarks/archive/2026-10-15-graceful-drain/quorum-graceful-stop.json).

The later peer-transport fix produced binary `9a09b8c1…`; on it the termination
fixture passed with both drivers
([driver 6](../../benchmarks/archive/2026-10-15-graceful-drain/termination-driver6-final.json),
[driver 5](../../benchmarks/archive/2026-10-15-graceful-drain/termination-driver5-final.json)) and the
500-row quorum acceptance ended with SIGTERM exit 0 on every voter.

## Defect found and fixed during qualification

The first build exited cleanly from `main` while per-connection engine threads
were still dropping the last store handle. Two of six runs in one fixture batch ended
in SIGABRT: macOS crash reports show `rocksdb_close` → `PosixFileSystem::UnlockFile`
aborting on a destroyed mutex while the main thread ran `exit()` C++ static
destructors. The abrupt SIGTERM used before never ran those destructors.
`serve` now waits for the engine threads to release the stores and closes
RocksDB on the main thread; if a running engine call holds them past 5 seconds
it exits with `_exit(1)`. After the fix: 12/12 fixture runs across both builds, 40 ad hoc
write-then-SIGTERM cycles (all exit 0, final audit `ok`) and the quorum runs,
with no new crash reports. The race was not reproduced on demand, so the
evidence is the crash stack plus the structural fix, not a failing test.

## Also run on this build

`connection_limits.py`, `multigraph_acceptance.py --rows-per-graph 2000`,
`typed_neighborhood_acceptance.py --iterations 200 --cli-iterations 5`,
`query_memory.py` and `beta_acl_api.py` all PASS; each stops the server with
SIGTERM.

## Not covered

- Linux container, added 2026-10-16 once the registry was reachable: image
  `636f63b1…` (binary `372bdeb0…`, Debian glibc 2.36, aarch64) passed the
  extended `compose_restart.py` three times: commit during the SIGTERM drain
  survives restart with exit 0, SIGKILL recovery, idle-only stop exit 0,
  offline audit `ok` ([receipt](../../benchmarks/archive/2026-10-15-graceful-drain/compose-linux.json)).
  No amd64, Kubernetes or Swarm run.
- No Kubernetes/Swarm target, no interruption of running engine calls, no
  COMMIT-phase kill hook, no load during the drain beyond the fixture cases.
- Times are single-host loopback observations, not SLOs.
