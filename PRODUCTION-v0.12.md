# v0.12 experimental beta: qualification status

**Experimental use only.** This report separates the beta's functional checks
from production qualification that remains open. The experimental prerelease
scope was explicitly chosen on October 30, 2026; sustained 2/4/8 GiB workloads,
the four-million-node load and process-wide RSS guarantees are not claimed.
Correctness, recovery and CI checks remain required for publication.

Use the [release workflow](https://github.com/allandefraga/brainwire/actions/workflows/release.yml)
and the matching GitHub release's `release-metadata.txt` to verify publication,
image architectures and digests. This source report alone does not establish
that a tag or container image has been published.

| Area | Current evidence | Remaining qualification |
| --- | --- | --- |
| Memory pressure | The existing workspace tests, Clippy and formatting passed. A synthetic local recovery exercise passed. A purge regression failed before its fix and passed after it. | Repeat sustained 2/4/8 GiB runs, qualify process-wide accounting and external cgroup pressure, and measure peak and idle RSS under controlled conditions. Sampled admission is not a hard RSS ceiling. |
| Bolt | Eligible large label scans stream entity records in parallel; focused PULL, finite DISCARD, cancellation and ACL/snapshot tests pass. RUN can return fields before its first page. | Qualify mixed-workload speed and socket contention. Other entity plans and several operators still materialize eagerly; an in-progress PULL is not preemptible. |
| Commit outcome | Experimental opt-in authenticated writes and scoped status passed synthetic Bolt recovery tests; graph data and receipt share a durable batch. | Qualify downgrade with a real old binary, ENOSPC/power loss, restore/fork and application integration; the existing Bolt status exercise is not a power-loss test. |
| RocksDB | The write buffer manager already shares the block cache. | Qualify compaction and external memory pressure with the requested 4 GiB workload. |
| Query execution | Synthetic Bolt tests confirmed refusal, rollback and recovery for oversized ORDER BY, aggregation and DISTINCT. | Extend lazy reads beyond the eligible parallel scan and qualify memory accounting under representative workloads. |
| Performance and Cypher | Earlier and external measurements are diagnostic, not reproduced release results. | Run the requested differential suite and performance targets; remaining compatibility gaps are open. |
| Multi-architecture image | The release workflow builds and tests amd64 and arm64 candidates before publishing a manifest. | A source checkout or tag is insufficient evidence. Verify the matching successful workflow and `release-metadata.txt` before pulling a release image. |

## External reports awaiting reproduction

| Workload | Previous | Candidate (externally reported) | Verification here |
| --- | ---: | ---: | --- |
| 2 GiB, 10 min peak RSS | 1,957 MiB | 1,175 MiB | Not independently repeated |
| 2 GiB, refused RUNs | 5,641 | 0 | Not independently repeated |
| 4 GiB idle RSS | 3,100 MiB | 1,524 MiB | Not independently repeated |
| Short-load peak RSS | 243 MiB | 545 MiB | Regression reported; fix and repeat pending |
| Entity-result read at 2 GiB | 115/115 refused | 1.6 s | Not independently repeated |

External workload reports cited in candidate design notes were not independently
repeated during candidate review. They do not qualify production or prove
behavior on a real ingestion source. Obtain image digests from the matching
successful release workflow and release metadata; the table records external
reports, not verified performance of a published image.

The 4 GiB external-pressure harness is prepared but has not run: other Docker
services are active and host memory headroom was insufficient for a safe, clean
measurement. This is a smoke test only, not the required sustained qualification.

## Prepared qualification tooling

`crates/server/tests/memory_soak.py` prepares a synthetic 8-writer/4-reader
30-minute run at a specified 2, 4 or 8 GiB limit. An optional four-million-node
fixture requires `--rows-per-writer 500000`. The harness pins a local image ID,
requires an isolated Linux Docker host, records sampled power and OOM evidence,
and removes only its own labelled resources. Its unit tests pass; it has not
been run against a candidate image. Sampling cannot prove continuous conditions.
Linux guests can expose virtual power without proving their physical host is on
AC; a host without observable mains or kernel logs yields an inconclusive result.
A separate larger, isolated host and additional external-pressure, downgrade
and mixed-performance tests are still required for the original production
qualification targets. Preparing this harness does not satisfy those targets.

Ingress recovery under shared-pool contention is part of the Bolt regression
checks. Use the matching CI result for the exact source revision; a prior passing
workspace run does not validate later recovery changes.
