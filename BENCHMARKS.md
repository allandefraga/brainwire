# Benchmarks

Use this page to choose a test and find its results. All recorded workloads are
synthetic. Each result applies to the exact binary, data and environment named
in its report; an older report does not qualify the current version.

**The current memory qualification is incomplete.** The recorded 2, 4 and 8 GiB
runs stopped during loading or reading and did not complete the required
30-minute workload. They do not prove a safe process-memory ceiling or absence
of OOM. Start with the [v0.9 report](PRODUCTION-v0.9.md) and
[production readiness roadmap](docs/production-readiness-roadmap.md).

## Choose a test

| Question | Test and results |
| --- | --- |
| Does a change affect large Bolt responses? | [Response encoding diagnostic](docs/benchmarks/2026-10-29-bolt-response/README.md): compares two local binaries, checks every result and records timings and sampled RSS. |
| Does pressure admission reject new queries safely? | [Linux cgroup smoke](docs/benchmarks/2026-10-29-cgroup-pressure/README.md): 512 MiB synthetic rollback and recovery, not a production memory qualification. |
| Does the server remain usable under a container limit? | [Container memory test](scripts/bench_v09_container.md): separate 2, 4 and 8 GiB runs with synthetic writes, reads and ACLs. The existing qualification attempts are incomplete. |
| How does a small workload compare with Neo4j and FalkorDB? | [Three-database diagnostic](docs/benchmarks/comparison/README.md): runner, raw JSON and charts, with differences in protocol and durability. This needs a fresh run for a new release. |
| Which other measurements exist? | [Result index](docs/benchmarks/README.md): current diagnostics and dated historical receipts. |

## Run a before/after response test

Build the baseline and candidate separately, then keep both executables. Finish
all builds and tests before timing. From the repository root:

```sh
python3 -m venv .venv-bench
.venv-bench/bin/python -m pip install neo4j==5.28.8
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
[result and limitations](docs/benchmarks/2026-10-29-bolt-response/README.md).

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
