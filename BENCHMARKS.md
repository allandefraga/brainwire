# Benchmarks

Use this page to choose a test and find its results. All recorded workloads are
synthetic. Each result applies to the exact binary, data and environment named
in its report; an older report does not qualify the current version.

**The current memory qualification is incomplete.** Previous 2, 4 and 8 GiB
qualification attempts stopped during loading or reading and did not complete the required
30-minute workload. They do not prove a safe process-memory ceiling or absence
of OOM. Start with the [v0.9 report](PRODUCTION-v0.9.md) and
[production readiness roadmap](docs/production-readiness-roadmap.md).

A separate [short beta.6 load smoke](docs/benchmarks/2026-11-04-load100k-015b6.json)
loaded 100,000 synthetic nodes in batches of 250 into a 2 GiB Linux arm64
container, then verified the count after restart. One local run recorded 2.483 s
for loading and 12.161 s for the whole workflow. This is not the sustained
qualification above, a comparison, or a performance guarantee.

On a locally built v0.20.0-beta.1 candidate, short synthetic cgroup-pressure
checks passed at [512 MiB](docs/benchmarks/2026-11-14-cgroup-020b1-512.json),
[2 GiB](docs/benchmarks/2026-11-14-cgroup-020b1-2048.json), and
[3 GiB](docs/benchmarks/2026-11-14-cgroup-020b1-3072.json). They filled tmpfs
to induce pressure, checked refusal and rollback, then verified recovery and an
offline audit. They do not establish a global RSS ceiling, sustained capacity,
or recovery from a machine failure. These receipts describe a local candidate
image, not a pullable release image.

A separate [30-minute 2 GiB synthetic run](docs/benchmarks/2026-11-14-v09-020b1-30min-summary.json)
loaded 20,000 nodes and 20,000 relationships with delegated ACLs, then ran four
read workloads. It passed its count and offline integrity checks. Its sampled
RSS peaked at about 134 MiB during the read soak, so this did not stress the
2 GiB limit. The linked receipt is a redacted summary; it records the SHA-256
of the full local report. It is not a production memory qualification.

## Choose a test

| Question | Test and results |
| --- | --- |
| How did three engines handle one synthetic million-node fixture? | [Indexed graph comparison](benchmarks/comparison/README.md): one run per engine, with different durability and client settings; includes raw receipts and restart checks. |
| Does a change affect large Bolt responses? | [Response encoding diagnostic](benchmarks/archive/2026-10-29-bolt-response/README.md): compares two local binaries, checks every result and records timings and sampled RSS. |
| Does pressure admission reject new queries safely? | [Linux cgroup smoke](docs/benchmarks/2026-10-29-cgroup-pressure/README.md): 512 MiB synthetic rollback and recovery, not a production memory qualification. |
| How does the Raft peer transport behave locally? | [Linux peer diagnostic](docs/benchmarks/2026-11-04-raft-peer.md): two local runs before and after the accepted-socket change, not a multi-machine benchmark. |
| Does the server remain usable under a container limit? | [Container memory test](scripts/bench_v09_container.md): separate 2, 4 and 8 GiB runs with synthetic writes, reads and ACLs. The existing qualification attempts are incomplete. |
| Which other measurements exist? | [Result index](docs/benchmarks/README.md): current diagnostics and dated historical receipts. |

## Run a before/after response test

Build the baseline and candidate separately, then keep both executables. Finish
all builds and tests before timing. From the repository root:

```sh
python3 scripts/setup_python.py --venv .venv-bench
.venv-bench/bin/python scripts/bench_bolt_response.py \
  --baseline /path/to/baseline/brainwire \
  --candidate /path/to/candidate/brainwire \
  --output /tmp/brainwire-response-new.json \
  --rounds 5 --warmup 5 --repeats 100
```

Use a new output filename. The script creates its own temporary stores and
servers, verifies returned IDs and payload hashes, and stops only its own
processes. The timing includes client hashing and transaction completion.
RSS is sampled; it is not a continuous peak measurement. See the
[result and limitations](benchmarks/archive/2026-10-29-bolt-response/README.md).

## Rebuild the release evidence

For each new candidate:

1. Freeze the source, executable hashes, image digest and configuration. Keep
   the data generator and exact commands with the report.
2. Run a small correctness test first. Verify counts, properties, relationships
   and ACL results before using any latency number.
3. Run the complete workload with no concurrent builds. Record hardware,
   operating system, resource limits, power mode, host load, warmup, cache state
   and every failure. Use several rounds and keep all timing samples.
4. Check restart/recovery and offline integrity where the test requires them.
   A failed or incomplete run remains a failed or incomplete result.
5. Publish raw receipts and methodology before drawing charts. Compare products
   only when logical data, queries, permissions and durability are aligned;
   disclose differences that cannot be removed.

The existing scripts are engineering diagnostics. The repository does not have
an audited LDBC result, a general ranking against competitors, or a production
latency or throughput guarantee.

## Read the numbers

- **p50** is the median. **p99** is the time at or below which 99% of samples
  finished. With a small sample, p99 may simply be the slowest observation.
- **Sampled RSS** is process memory observed at intervals; it can miss a peak.
- **Cgroup memory** includes more than process RSS, such as charged file cache.
- **Query reservations** are the engine's accounting estimates, not measured RSS.
- A container's `OOMKilled=false` flag alone does not prove there was no OOM.

Keep ingestion, query latency, memory, recovery and replication measurements
separate. Do not combine different fixtures or dates into one comparison series.
