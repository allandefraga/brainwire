# Four-label mixed-load diagnostic: production session

Status: **incomplete**, not a production qualification or a throughput comparison.
The test used a 4 GiB Docker cgroup without extra swap, synthetic data, eight
writers and four concurrent readers with delegated visibility. The requested
dataset was one million identities per label across four labels and four edge
types. The independent source image was built from v0.9.0-beta.1 commit
`5ef2d38227c8e8c595f63059ec2356817f1a253f`; the candidate image was built
from that source plus the session's uncommitted Rust changes. Its image digest
identifies the tested image, not a committed source revision.

| Observation | v0.9 baseline | Session candidate |
| --- | ---: | ---: |
| Acknowledged identities per label before abort | 66,750 | 123,750 |
| Sampling period | 25.3 s | 38.0 s |
| Cgroup peak observed | 732.8 MiB | 1,021.3 MiB |
| Process RSS peak sampled | 638.3 MiB | 824.7 MiB |
| Stopping reason | Writer query-memory pool exhaustion | Writer query-memory pool exhaustion |
| Start host load, 1/5/15 min | 9.29 / 11.77 / 16.08 | 10.81 / 11.94 / 15.94 |

The diagnostic stopped on errors before loading one million identities per
label; graph counts were not independently reconciled after the stop, and it did
not reach `db check` or the intended sustained read phase. The two different
sampling durations and host loads prevent a valid speed or memory comparison.
No OOM event was observed in collected cgroup or Docker evidence; kernel logs
were unavailable to the unprivileged container, so absence of an OOM cannot be
established. Process RSS is sampled; cgroup peak includes file cache. Both runs
were on AC with low-power mode disabled; the platform reported no thermal
warning, but actual thermal throttling was not independently measured.

- Baseline image: `sha256:b69d6f5b64c1f4d6c542e320b041c815e14e34015a81ceb0dfc2c0825e16c38b`
- Candidate image: `sha256:a1afb0e7cd394e26cd4a9c1257b566148affd46d5a05f8c4b589aacfd0d7d5ca`
- Command and safeguards: [`scripts/bench_v09_container.md`](../../scripts/bench_v09_container.md)

A separate sequential projection regression reduced *accounted query-memory
high-water* from 913,601 to 78,401 bytes for 200 rows with a 4 KiB unused
intermediate value; the same query failed a 128 KiB budget before the change
and passed afterward. This is not process RSS and does not measure blocking or
parallel plans. See `crates/cypher/tests/final_projection_memory.rs`.

Next qualification: account for remaining native/transport/result allocations,
provide sustained backpressure and actual Bolt streaming, then complete 2, 4 and
8 GiB workloads including 30 minutes of mixed read/write activity, exact result
and ACL checks, kernel evidence and offline `db check`. See
[production readiness roadmap](../production-readiness-roadmap.md).
