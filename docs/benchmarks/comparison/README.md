# Local comparison: Brainwire, Neo4j and FalkorDB

This is a reproducible diagnostic run, not an audited benchmark or a general
product ranking. It compares one workload on one developer machine. The raw
receipt and chart are checked in beside this report:

- Run: `20261019T115937Z-c6b05d`
- [Raw JSON receipt](results/20261019T115937Z-c6b05d.json)
- [Comparison chart](../assets/comparison-20261019.svg)

![Measured local comparison](../assets/comparison-20261019.svg)

## Results

All three systems loaded the same 10,000-node, 20,000-edge graph and recovered
those counts after a graceful restart of the same container with its persistent
volume attached. This is a restart check, not a power-loss, machine-loss or
container-recreation test. Each container had a 4 CPU / 8 GiB limit and ran
alone on the host.

| System | Indexed point read p95 | 1-hop `LIMIT 20` p95 | 20 × 100-row update series median / max | Fresh ingest records/s |
|---|---:|---:|---:|---:|
| Brainwire | 0.654 ms | 0.944 ms | 42.149 / 48.268 ms | 28,261 |
| Neo4j Community | 2.177 ms | 8.249 ms | 77.763 / 173.122 ms | 15,392 |
| FalkorDB | 0.462 ms | 0.346 ms | 21.175 / 30.180 ms | 73,099 |

Point-read p95 uses 1,000 measured requests per system; one-hop p95 uses 500.
Each update sample executes twenty parameterized 100-row property-update
statements. We report the median and maximum of five update-series samples
rather than treating five observations as a meaningful p95. Fresh ingest is
one measurement per system and is indicative only. Raw JSON contains every
read and update timing sample, p50/p95/p99 and mean latencies, sampled
container memory/CPU, image IDs/digests, client versions, persistent paths,
and data-directory byte counts. Memory is one post-workload snapshot, not peak
RSS; on-disk byte counts are not normalized across database formats.

For this particular local run, FalkorDB was fastest on the listed query,
update and ingest measurements; Brainwire was between FalkorDB and Neo4j on
those metrics. This is not a general ranking or proof of superiority. The
graph is small and synthetic, each product has a different client/protocol and
durability setup, and only one run was made in a fixed system order.
These results are not production capacity or service-level guarantees.

## What was measured

- Deterministic graph: 10,000 `Item` nodes with unique, indexed `entity_id`
  and a small `value` property; 20,000 directed `LINK` edges (a ring plus an
  offset-17 edge from every node).
- Batch size: 100. The graph starts empty. Ingestion uses parameterized
  `CREATE` statements; it does not measure `MERGE` or repeated upserts.
- Indexed point read: `MATCH (n:Item {entity_id: $key}) RETURN n.value AS value`.
- One-hop read: `MATCH (n:Item {entity_id: $key})-[:LINK]->(m:Item)
  RETURN m.entity_id AS key LIMIT 20`.
- Property update: twenty 100-row batches per measured series. Each batch
  matches by `entity_id` and sets `score`.
- One persistent driver session per service, one warm-up before timing, five
  repetitions for reads and update series. Databases ran sequentially, each in
  a fresh container and fresh named volume.
- After timings, each container was gracefully restarted on the same volume. The
  harness checked exact node and edge counts, selected property values and the
  last update score. This does not simulate abrupt process death, container
  recreation, host failure or a machine/network partition.

The Neo4j and Brainwire runs use the same Python Neo4j Bolt driver. FalkorDB
uses its official Python client over its native Redis-compatible protocol, so
latency includes different client and wire-protocol stacks. Durability is also
not identical: Brainwire uses its default WAL settings, Neo4j uses its image's
default transaction-log configuration, and FalkorDB used AOF with
`appendfsync always`. Brainwire explicitly enables synchronous WAL writes for
this benchmark candidate. The settings are not equivalent, so the measured
latency comparison must not be read as equivalent durability/performance tradeoffs.

## Versions and environment

| Component | Pin |
|---|---|
| Brainwire | local image ID in the raw receipt, built from dirty worktree based on commit `b6a3eb4`; includes the Bolt `TCP_NODELAY` change |
| Neo4j | Community `neo4j:2026.09.0`, image digest in receipt |
| FalkorDB | `falkordb/falkordb-server:6.0.0`, image digest in receipt; volume mounted at `/var/lib/falkordb/data` |
| Neo4j Python driver | `neo4j==5.28.8` |
| FalkorDB Python client | `falkordb==1.2.2` |
| Host | macOS 26.6.2, arm64; Docker Engine 29.7.2 |

The image digests are captured in the JSON receipt so a later run can identify
the exact container images, even if tags move. The Brainwire image is local;
the receipt records its immutable image ID. The source worktree was dirty, so
the commit alone does not reproduce that exact candidate.

## Reproduce

Install Docker and `uv`, then from the repository root:

```sh
docker build --build-arg CARGO_BUILD_JOBS=1 --tag brainwire-benchmark:local .
BRAINWIRE_IMAGE=brainwire-benchmark:local uv run --python 3.12 --with-requirements docs/benchmarks/comparison/requirements.txt \
  docs/benchmarks/comparison/run.py \
  --nodes 10000 --batch 100 --repeats 5 --reads 200 \
  --neighborhoods 100 --update-batches 20
```

Set `BRAINWIRE_IMAGE` to a local Brainwire image tag or ID to benchmark another
build. The runner creates only uniquely named `brainwire-comparison-*`
containers and `bw-compare-*` volumes, removes those exact resources at exit,
and leaves all images in place. It does not prune or inspect other workloads.
Each execution writes a new raw JSON receipt. Render its graph using:

```sh
python3 docs/benchmarks/comparison/plot.py \
  docs/benchmarks/comparison/results/<run-id>.json
```

## Interpretation limits

This does not follow the LDBC SNB specification and is not an audited result.
It excludes concurrency, large working sets, cold-storage random reads,
complex multi-hop traversals, ACL overhead, delete/merge workloads, sustained
ingestion, replication, and machine-failure testing. It also does not compare
memory use as a normalized database metric; container memory is only one
post-run sample and storage files are laid out differently by each product.
Use the LDBC SNB Interactive workload and its required execution/audit process
before making standardized claims.
