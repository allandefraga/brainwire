# Benchmarks and reproducibility

Brainwire is an experimental beta. Measurements are workload-specific; no
result here establishes production readiness or a general ranking against
another database.

- [Benchmark index and charts](docs/benchmarks/README.md) describes the
  available workloads, commands, receipts and limitations.
- [Neo4j and FalkorDB diagnostic](docs/benchmarks/comparison/README.md) includes
  raw results and pinned images. Protocols and durability configurations differ,
  so its results are not an equivalent production comparison.
- [Four-label memory diagnostic](docs/benchmarks/2026-10-28-production-session.md)
  stopped early under query-pool pressure. It is not a throughput comparison,
  and it does not prove that the process avoids OOM.
- [v0.9 production qualification](PRODUCTION-v0.9.md) records the open 2, 4
  and 8 GiB stability gates.

For a new comparison, use identical logical datasets, query semantics,
authorization, durability, clients and hardware where possible; disclose any
differences. Record the source commit and image digest, data generator, exact
commands, operating system and hardware, resource limits, warmup and cache
state, power and background load, repetitions, median and p99, failures and
correctness checks. Run builds separately from timed trials. Keep raw receipts
and include recovery and offline integrity results where relevant. A diagnostic
is not an audited LDBC result.
