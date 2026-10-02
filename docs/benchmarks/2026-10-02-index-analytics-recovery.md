# Index, sparse analytics and container recovery, 2026-10-02

Local observations on arm64 Mac17,2, Rust 1.98.1. Native measurements used
development builds, 64 MiB shared cache and 16 MiB memtable budget. Inputs are
synthetic and temporary. The release Docker image was built separately as
`brainwire:20261002-test`; no image was published.

## Equality lookup

`index_lookup` inserts unique email strings and identical values in unindexed
`name`, audits the graph/index invariants and reopens the store. Each query has
five warmups followed by 25 correctness-checked samples. Times exclude database
opening, network latency and result serialization outside the embedded API.

| Nodes / run | Pattern index p50 | WHERE index p50 | Unindexed WHERE p50 |
|---|---:|---:|---:|
| 10,000, Docker compilation in parallel | 0.0719 ms | 0.0485 ms | 144.346 ms |
| 100,000, Docker compilation in parallel | 0.1595 ms | 0.1501 ms | 3,447.642 ms |
| 100,000, after Docker compilation ended | 0.0640 ms | 0.0598 ms | 1,941.594 ms |

The last run's WHERE index p95 was 0.0652 ms; scan p95 was 2,477.325 ms.
Other local application/test activity and cache conditions were not controlled.
These are individual warm observations, not an SLA or a claim about cold,
networked, nonselective or heterogeneous queries. The unindexed control checks
an equivalent value in another property; correctness tests separately compare
the same query with indexing declared versus disabled.

The change extracts safe literal/parameter equality conjuncts from WHERE,
preserves primary/pattern lookups and retains full residual filtering. Unit and
integration tests cover reversed equality, Boolean/string values, NULL/numeric
fallbacks, OR/NOT, contradictory filters, hops, multiple patterns, existing
bindings, own write overlays and old snapshots. A synthetic corrupted
non-candidate confirms that a physical WHERE/primary seek avoids unrelated
node decoding. `CALL db.indexes()` inspects readiness in the caller's snapshot.

```sh
cargo run --locked --offline -p brainwire-cypher --example index_lookup -- 100000
```

Raw observations are in [2026-10-02-results](2026-10-02-results/).

## CSR analytics

The sparse workload has 10,000 nodes and 100,000 directed relationships, with
bounded out-degree, ring offsets and a shared hub. Observed native debug values:

| Operation | Time |
|---|---:|
| Full CSR projection | 143.257 ms |
| Weighted PageRank kernel, 64 iterations to convergence | 192.118 ms |
| BFS kernel, max depth 10, 91 reachable nodes | 0.0344 ms |
| Full Cypher PageRank top-five query | 341.972 ms |
| Point lookup p50, 1,000 samples | 0.0387 ms |
| Ten-neighbor one-hop p50, 1,000 samples | 0.1638 ms |

CSR vector capacities occupied 1,280,008 bytes, excluding temporary construction
and algorithm buffers, NodeId maps, results and process RSS. BFS kernel time
excludes projection and node decoding; `graph.reachable` rebuilds its bounded
projection each call. It returns minimum unweighted directed hop depths, not
weighted paths. Randomized tests compare it with an independent all-pairs
shortest-hop reference, including depth limits and cycles.

```sh
cargo run --locked --offline -p brainwire-cypher --example sparse_analytics -- 100000
```

## Persistence and orchestration

- Native Python/Bolt acceptance (driver 5.28.6): 10,000 input rows, 250-row
  batches, insertion 0.651 s, idempotent replay 0.913 s. After an intentional
  deletion and two SIGKILL cycles, 9,999 nodes recovered. There were 7 and 10
  SST files before the kills. Pending indexed overwrites were discarded;
  index audit and the new catalog/reachability calls passed.
- Release Docker acceptance: 10,002 initial Customers and one Company,
  five acknowledged cycle-node inserts, seven verification passes (initial
  plus six recoveries). Six kills occurred with an uncommitted indexed email
  overwrite. Current indexed values survived; older and uncommitted values
  were absent. Final `db check` passed. Its temporary container/volume were
  removed. Expected connection-loss logs accompany the deliberate kills.
- Actual Compose template: offline ontology application, SIGKILL and forced
  service recreation retained the committed indexed node. Final `db check`
  passed; its unique project and volume were removed. Native Docker init
  forwards termination signals during local stops.
- Compose and Swarm configuration parsing passed. The rendered Kubernetes
  Service/StatefulSet passed strict Kubernetes 1.36 schema validation with
  kubeconform: 2 valid, 0 invalid, 0 errors, 0 skipped. This is not a live cluster
  failover test; no Kubernetes context was configured and no Swarm stack was
  deployed. Templates keep a single writer and retained member volume.

No replication, ANN index, enforced global query-memory budget, resumable
large index build or connected TUI execution was introduced in this session.
