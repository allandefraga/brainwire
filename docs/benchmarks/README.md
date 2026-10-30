# Benchmark results and test runners

Start with [how to run benchmarks](../../BENCHMARKS.md). This directory keeps
raw receipts and reports so a result can be traced to its workload and binary.
The dates matter: historical results describe those candidates, not the current
working tree or latest release.

**Memory stability remains unqualified.** The 2, 4 and 8 GiB attempts in the
[v0.9 report](../../PRODUCTION-v0.9.md) stopped early. The later
[four-label diagnostic](2026-10-28-production-session.md) also stopped under
query-pool pressure. Neither completed the required sustained workload or
established a process RSS ceiling. Those results must not be presented as a
successful capacity benchmark.

## Recent pressure-admission smoke

A [v0.13 local diagnostic](2026-10-30-v013-memory.md) repeats the pressure
smoke and records a small idle-RSS experiment. Its limits are explicit.

A [512 MiB Linux cgroup test](2026-10-29-cgroup-pressure/README.md) observed
query rejection, rollback and recovery after synthetic pressure. It is not an
RSS ceiling or sustained 2/4/8 GiB qualification. The first inconclusive
attempt and the repeated run are both recorded.

## Recent work

| Report | What was measured | What remains open |
| --- | --- | --- |
| [2026-10-29 Bolt responses](2026-10-29-bolt-response/README.md) | Two native binaries returned the same eight 512 KiB strings; every ID and payload hash was checked. Raw samples, binary hashes and sampled RSS are included. | Timing varied between rounds. The results do not establish a 5% regression bound or an RSS improvement. |
| [2026-10-29 conflict history](2026-10-29-history-growth.md) | A focused test compared deque growth strategies while retaining conflict keys. | It excludes the database write path, fsync, Bolt and clients. Fewer capacity changes are not proof of better database throughput. |
| [2026-10-29 regex](../cypher-regex.md) | A small repeated-pattern query measured the cost of recompilation and a per-RUN cache; [uncached](2026-10-29-regex/uncached.log) and [cached](2026-10-29-regex/cached.log) logs are retained. | Three developer-build samples do not establish p99, general regex performance or Java-regex compatibility. |
| [2026-10-28 mixed load](2026-10-28-production-session.md) | Four labels, concurrent writers/readers and delegated visibility in a 4 GiB container. | Both loads aborted; exact final counts and `db check` were not reached. Different durations and host loads prevent a valid before/after comparison. |

A new change needs a new receipt. Preserve failed attempts rather than replacing
them with the fastest successful sample.

## Compare Brainwire, Neo4j and FalkorDB

The [three-database diagnostic](comparison/README.md) includes a new October 29
run as well as an October 19 historical baseline. It contains a checked-in runner,
raw JSON, pinned image identities and a chart for a 10,000-node, 20,000-edge
synthetic workload recorded on October 19. The recorded Brainwire source tree
was dirty, so its base commit alone cannot reproduce that exact image.

That run uses different client protocols and durability configurations across
products. It is useful as a historical engineering baseline, but it does not
support a general product ranking or an equivalent production comparison.
Refresh the workload, source identities and results for a new release before
using the chart to describe its performance.

To run a new diagnostic, install Docker and `uv`, then from the repository root:

```sh
docker pull neo4j:2026.09.0
docker pull falkordb/falkordb-server:6.0.0
docker build --build-arg CARGO_BUILD_JOBS=1 --tag brainwire-benchmark:local .
BRAINWIRE_IMAGE=brainwire-benchmark:local uv run --python 3.12 \
  --with-requirements docs/benchmarks/comparison/requirements.txt \
  docs/benchmarks/comparison/run.py \
  --nodes 10000 --batch 100 --repeats 5 --reads 200 \
  --neighborhoods 100 --update-batches 20
```

Finish the image build before timing and ensure the host has enough memory and
disk for all three databases. The runner creates uniquely named containers and
volumes, removes its own resources at exit and writes a new JSON receipt under
`comparison/results/`. Read the [full methodology](comparison/README.md) before
running it. Render that receipt's chart with:

```sh
python3 docs/benchmarks/comparison/plot.py \
  docs/benchmarks/comparison/results/<run-id>.json
```

Do not describe these results as LDBC results. A standardized LDBC result needs
its specified workload, execution rules and applicable audit process; the
current runner is a smaller custom diagnostic.

## Test memory under a container limit

Use the [four-label container guide](../../scripts/bench_v09_container.md).
Start with its small smoke test, then run 2, 4 and 8 GiB separately using the
same frozen image and configuration policy. The full defaults load four million
nodes and four million edges, then run a 30-minute read soak. Loading has a
separate deadline; the smoke test does not qualify the full workload.

Keep the complete receipt, exact counts and ACL checks, Docker/cgroup events,
restart state, kernel evidence where available, and offline `db check` results.
No observed Docker OOM event is narrower evidence than proof of no kernel OOM.
The [production roadmap](../production-readiness-roadmap.md) lists the remaining
gates.

## Historical results

These reports and charts remain available for their original candidates. Do not
combine their numbers with current diagnostics or use them to certify a new
binary.

| Report | Recorded scope |
| --- | --- |
| [October 13 beta qualification](2026-10-13-beta.md) | A frozen local candidate ran for one hour on 100,000 synthetic nodes with four readers and a throttled writer. The report binds its binary/image identities, checks and limits. It does not qualify current memory settings or multi-machine deployment. |
| [October 15 quorum writes](2026-10-15-quorum-write/README.md) | Short three-voter tests on one host compared single-row writes with batches and checked retained-volume restart. The report also preserves a larger failed attempt with an unknown commit outcome. It does not establish multi-machine or sustained capacity. |
| [October 19 product comparison](comparison/README.md) | A small one-host comparison with unequal protocol/durability settings and one fixed system order. |

<details>
<summary>Historical charts</summary>

The Brainwire-only chart below summarizes the older local measurements described
above. The [separate comparison chart](assets/comparison-20261019.svg) uses its
own fixture and methodology.

![Historical Brainwire-only measurements](assets/brainwire-performance.svg)

</details>

Other dated reports in this directory cover ACLs, Cypher, ingestion, indexes,
recovery and replication. Read each report's source identity, test boundary and
failure evidence before reusing its numbers.
