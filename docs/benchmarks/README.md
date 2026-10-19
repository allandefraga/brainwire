# Brainwire benchmark results

These charts summarize two repeatable local measurements. They describe the
recorded synthetic workloads only; they are not production service levels and
do not compare Brainwire with Neo4j or FalkorDB.

![Measured Brainwire latency and batched-write throughput](assets/brainwire-performance.svg)

## One-hour mixed workload

The frozen candidate ran for 3,600 seconds against a synthetic graph with
100,000 nodes. Four readers and a throttled writer produced these tail latencies:

| Operation | p95 | p99 |
|---|---:|---:|
| Primary read | 1.557 ms | 4.292 ms |
| Scalar-index read | 1.623 ms | 4.483 ms |
| Indexed write acknowledgement | 6.214 ms | 13.636 ms |

The run acknowledged 9.43 million primary reads, 9.43 million indexed reads,
and 65,927 indexed updates. After SIGKILL and reopen, the last 200 acknowledged
values and their index lookups were verified; the store audit passed. The
process was sampled on a local macOS host, so these measurements do not establish
a production memory bound. Full receipt, exact binary identity and reproduction
commands are in the [beta qualification report](2026-10-13-beta.md).

## Three-voter batch-write diagnostic

The quorum diagnostic used three native voter processes on one host. In a
10-round test, 10,000 single-row writes averaged 841 rows/s; 100-row Cypher
batches averaged 32,090 rows/s. The 100-row transaction acknowledgement p95 was
3.748 ms across 100 measured batches. That is about 38x the row throughput in
this specific run, not a general speedup claim. All 20,000 rows survived a
same-volume restart and the three offline logical digests matched. This was a
short localhost test, not multi-machine capacity or an endurance test. See the
[full report and raw receipt](2026-10-15-quorum-write/README.md).

## Neo4j and FalkorDB comparison

A reproducible local diagnostic is now available: [Brainwire vs Neo4j vs
FalkorDB](comparison/README.md), including raw JSON, pinned container digests,
the runner, client versions, workload definition, recovery checks and a chart.
It reports only measurements from this host; it is not audited, not an LDBC
result and not a general product ranking. The workloads use different client
protocols and do not have identical durability configurations, which are
documented with the receipt. Treat the result as a useful engineering baseline,
not a claim that one system is categorically faster.

The existing Brainwire-only charts above remain separate measurements and must
not be combined with the comparative run as one benchmark series.

The [LDBC SNB Interactive benchmark](https://ldbcouncil.org/benchmarks/snb/interactive/)
is a useful standardized workload reference for transactional graph reads and
updates. Brainwire results should only be called LDBC results after following
the required specification and audit process. The
[FalkorDB benchmark project](https://github.com/FalkorDB/benchmark) is another
useful workload reference, but its published figures are vendor-provided and
must not be presented as independently audited or directly comparable.
Neo4j's own guidance also notes that the query plan, memory, indexes and host
configuration affect measured performance; capture these alongside every run
([performance guidance](https://neo4j.com/docs/operations-manual/current/performance/),
[query profiling](https://neo4j.com/docs/cypher-manual/current/planning-and-tuning/execution-plans/)).

## Other measured scenarios

The dated reports and raw receipts in this folder cover ACL reads, Cypher
traversals, indexes, ingestion, recovery, quorum operations and memory use.
Each report states whether it measures embedded execution, Bolt, a local
container, or co-located quorum voters; do not combine their numbers as one
benchmark series.
