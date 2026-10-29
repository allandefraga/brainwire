# Brainwire v2 engineering roadmap

This is a roadmap, not a v2 release announcement or production certification.
Brainwire remains an independent, application-agnostic graph database. The
current implementation uses Rust, local RocksDB and Cypher over Bolt; deployment
manifests and an experimental three-voter consensus path already exist.

## Prerequisite: qualify v1 first

The v2 work is gated on a single integrated candidate meeting the v1 acceptance
criteria. No `PRODUCTION-v1.0.md` certification currently exists. The recorded
[v0.9 report](PRODUCTION-v0.9.md) and
[latest mixed-load diagnostic](docs/benchmarks/2026-10-28-production-session.md)
leave the production gate open: complete 30-minute 2/4/8 GiB runs, verify exact
data and ACL results, capture kernel/cgroup evidence, and pass restart and
restore checks using the same source, binary and configuration.

A shared query/ingress budget is not a global RSS ceiling. Linux cgroup pressure admission now rejects new RUN requests at 95% of a visible finite cgroup limit; it is cooperative, fails open when unavailable, and does not bound in-flight or native allocations. Bounded response
encoding is not execution that produces results on demand for Bolt `PULL n`.
Existing TLS proxy recipes are not native Bolt TLS or online identity management.
See the [production readiness roadmap](docs/production-readiness-roadmap.md).

Current local v1 work adds an advisory configured-memory summary and a bounded
[Rust-regex subset of `=~`](docs/cypher-regex.md). These are partial steps: they
do not establish an RSS cap, Java regex parity or the sustained-load gates.

Before approving v1, also verify the stable memory/unknown-commit contracts,
current differential Cypher corpus, transport security and live principals.
A prior passing unit test or an older acceptance receipt does not qualify a new
candidate automatically.

## Status and evidence rules

- **Done** means the entire item's acceptance contract is implemented and measured.
- **Partial** means a concrete subset exists; the remaining contract stays open.
- **Open** means the requested capability or its required proof is absent.
- **Not measured** means the stated target has no qualifying measurement. It is
  not a zero, estimate, vendor comparison or implied pass.

Links below identify code/tests or recorded evidence. Tests establish their
specific scope, not the complete production target. No item below is marked done.

## 1. Correctness and failure resistance

| # | Item / status | Acceptance target | Current evidence and measurement |
|---|---|---|---|
| 1 | Deterministic simulation — **Open** | Abstract I/O, network, clock and scheduling; replayable fault seeds; one week of nightly simulation without a new failure. | Process-crash coverage exists in [store crash tests](crates/store/tests/crash.rs); there is no all-I/O deterministic simulator. Target not measured. |
| 2 | Replication linearizability — **Open** | Verify operation histories across leader loss, partitions and lost acknowledgments with an independent checker; zero violations. | [Quorum acceptance harness](crates/server/tests/quorum_replication.py) exercises selected scenarios; no Elle/Knossos-equivalent history proof. Target not measured. |
| 3 | Formal protocol specification — **Open** | Model commit, consensus and fenced leases in TLA+/P; run a model checker in CI. | [Replication gates](docs/design/replication-next-gates.md) document remaining work. No checked formal specification established. |
| 4 | Continuous fuzzing — **Open** | Version parser/decoder/storage/planner corpora; run applicable sanitizers/Miri; zero crashes in 24 h per target. | Existing unit tests do not establish fuzzing coverage. Target not measured. |
| 5 | Generated Cypher differential — **Partial** | Generate random queries against a reference and retain minimized divergences as regressions. | [Fixed differential harness](crates/server/tests/differential.py) exists; it is not a random-query generator. No generated-corpus result recorded. |
| 6 | Power-loss resistance — **Open** | Drop unsynchronized writes with dm-flakey/LazyFS; retain every acknowledged commit and detect corruption. | [No-ack recovery harness](crates/server/tests/uncertain_commit.py) and [crash tests](crates/store/tests/crash.rs) cover process failure; neither simulates power loss. Target not measured. |

## 2. Query engine

| # | Item / status | Acceptance target | Current evidence and measurement |
|---|---|---|---|
| 7 | Cost optimizer — **Open** | Online label/type/property/degree statistics and sketches select plans by measured cost. | [Planner](crates/cypher/src/exec/plan.rs) contains existing planning logic; no qualifying histogram/sketch cost model established. Target not measured. |
| 8 | Vectorized pipelined execution — **Open** | Columnar batches and parallel morsel scheduling; complete LDBC SNB Interactive and BI within memory budgets. | [Streaming tests](crates/cypher/tests/streaming.rs) cover existing incremental paths, not the requested vectorized engine. Target not measured. |
| 9 | Worst-case-optimal joins — **Open** | Choose generic/Leapfrog Triejoin for suitable cyclic patterns and validate results/cost. | No implementation or qualifying cyclic-join comparison established. |
| 10 | Adaptive execution and plan cache — **Open** | Replan on cardinality divergence; cache parameterized plans and support prepared statements. | Parameterized Bolt queries alone do not establish this capability. Target not measured. |
| 11 | Factorized intermediates — **Open** | Preserve shared multihop structure without materializing combinatorial intermediates. | [Projection-memory regression](crates/cypher/tests/final_projection_memory.rs) improves one materialized path; it is not factorization. Recorded accounted peak: 913,601 to 78,401 bytes for its 200-row synthetic query; not RSS. |
| 12 | ACL bitmap index pushdown — **Partial** | Filter principal/group visibility before joins with compressed bitmaps; governed reads within 10% of ungoverned reads. | [Node ACL tests](crates/cypher/tests/node_acl.rs) establish existing permissions on selected paths; no qualifying Roaring pushdown or overhead result. Target not measured. |

## 3. Graph for AI

| # | Item / status | Acceptance target | Current evidence and measurement |
|---|---|---|---|
| 13 | Native vector indexes — **Open** | Incremental HNSW and disk ANN, quantization, in-search predicates/ACL; recall@10 >= 0.95 and p99 < 20 ms on 10M filtered vectors. | [Cypher guide](docs/local-cypher.md) distinguishes stored lists from ANN. No qualifying vector-index implementation or measurement. |
| 14 | Hybrid retrieval — **Open** | One query combines vector, BM25, language analysis including Portuguese, graph expansion and RRF; explain every stage. | [Knowledge ranking](docs/knowledge-ranking.md) is an explicit source-evidence heuristic, not hybrid retrieval. Target not measured. |
| 15 | Parallel governed algorithms — **Partial** | ACL-safe PageRank, centralities, components, Louvain/Leiden, similarity, weighted paths and sampled k-hop; write or stream results. | [Analytics tests](crates/cypher/tests/analytics.rs) cover the existing sparse projection/algorithm subset. Parallel suite, community detection and streamed/write-back contract remain unqualified. Target not measured. |
| 16 | Agent introspection — **Partial** | Compact actual schema plus cardinalities/examples/indexes; preflight validation with corrective errors and caller cost caps. | [Schema procedure tests](crates/cypher/tests/schema.rs) cover bounded catalog metadata inspection. Actual-data samples/cardinalities and full preflight/cost contract remain open. No target measurement. |

## 4. Lakehouse ingestion

| # | Item / status | Acceptance target | Current evidence and measurement |
|---|---|---|---|
| 17 | Direct bulk lakehouse import — **Partial** | Parallel declarative Parquet/Delta/Iceberg/DuckDB/DuckLake local/S3 import without Bolt; map entities, edges, properties and ACL. | [Source ingestion](docs/source-ingestion.md) and [parallel ingestion](docs/parallel-ingestion.md) document existing Python/snapshot adapters. They are not a complete native direct import engine. 100M nodes + 300M edges target and time bound X not measured. |
| 18 | Change-aware keyed batch upsert — **Partial** | Skip unchanged payloads, remove null properties, and report created/changed/unchanged/deleted counts; identical snapshot writes zero rows. | [Python helper](docs/python-client.md) and [DuckLake snapshot bridge](docs/ducklake-incremental.md) provide parts of mutation/replay semantics. No general server-side hash-aware batch contract or qualifying zero-write measurement. |
| 19 | Atomic bulk delete — **Partial** | Key/predicate batch deletion with attached edges and the stable transaction/error contract. | [Deletion tests](crates/cypher/tests/delete_edges.rs) and [source ingestion](docs/source-ingestion.md) cover existing delete paths. A general dedicated bulk deletion API and load qualification remain open. |
| 20 | Atomic source cursor with fencing — **Partial** | Store source cursor and changed data in one transaction; reject stale writers using durable fencing tokens. | [Incremental bridge](docs/ducklake-incremental.md) records progress with graph data; [parallel ingestion](docs/parallel-ingestion.md) uses cooperative leases. Cooperative leases do not provide fencing. No complete fenced-source qualification. |
| 21 | Output CDC — **Open** | Ordered resumable node/edge/property/ACL change stream for Kafka/file/HTTP. | [Commit positions](docs/commit-state.md) identify local durable positions; they are not a CDC feed. Target not measured. |

## 5. Time and versions

| # | Item / status | Acceptance target | Current evidence and measurement |
|---|---|---|---|
| 22 | Time travel — **Open** | Every read supports AS OF timestamp/version with bounded retention. | [Temporal guide](docs/temporal-data.md) explicitly distinguishes schema versions/checkpoints from row history. No native AS OF contract. |
| 23 | Valid-time graph — **Open** | Optional validity intervals on nodes/edges and consistent historical traversals/corrections. | [Temporal design](docs/design/2026-10-01-temporal-graph-and-maintenance.md) is a design; application revision nodes are not native temporal storage. Target not measured. |
| 24 | Copy-on-write graph branches — **Open** | Near-zero-copy branch/clone with defined merge/discard semantics. | [Native checkpoints](docs/checkpoints.md) can share immutable SST files locally but have no branch/merge contract. Target not measured. |
| 25 | Object-store PITR — **Open** | Continuous incremental S3/GCS/Azure backup, arbitrary-point recovery and daily verified restores. | [Checkpoint support](docs/checkpoints.md) provides local snapshots; retention, remote storage and replay manager remain open. Target not measured. |

## 6. Scale and cloud

| # | Item / status | Acceptance target | Current evidence and measurement |
|---|---|---|---|
| 26 | Compute/storage separation — **Open** | Object-backed SSTs, tiered local cache and readers starting within seconds. | Local RocksDB/checkpoints are the existing storage path. No shared-object SST engine or startup measurement established. |
| 27 | Sharding — **Open** | Graph/vertex partitions with per-shard Raft, online rebalance and distributed pushdown. | [Quorum replication](docs/quorum-replication.md) replicates an entire instance across three fixed voters; it does not shard. Target not measured. |
| 28 | Bounded-staleness follower reads — **Open** | Caller declares freshness; driver routes automatically while preserving the declared consistency. | Existing quorum operations do not establish this follower-read and routing contract. Target not measured. |
| 29 | Tenant resource isolation — **Partial** | Per-graph/principal CPU, memory, IOPS and storage quotas, priorities and tested noisy-neighbor isolation. | Named graphs, grants and [shared memory admission](docs/memory-defaults.md) exist. Full tenant quota/isolation contract and measurement remain open. |
| 30 | Scale to zero — **Open** | Idle graph releases memory and reopens on demand within a measured time bound. | No qualifying idle eviction/reopen policy or measurement established. |

## 7. Security and compliance

| # | Item / status | Acceptance target | Current evidence and measurement |
|---|---|---|---|
| 31 | Per-graph encryption/KMS — **Open** | At-rest keys via AWS/GCP/Azure/Vault with online rotation. | No native per-graph KMS contract established. Storage-platform encryption is a separate deployment property. |
| 32 | Property-level authorization — **Open** | Hide/mask selected application properties by principal across every result path. | [Property visibility tests](crates/cypher/tests/property_visibility.rs) hide reserved internal fields; that is not configurable per-principal field security. |
| 33 | Signed immutable audit — **Open** | Export verifiable append-only read/write audit records with identity. | No qualifying signed audit format, key rotation or tamper verification established. |
| 34 | mTLS and OIDC/JWT — **Partial** | Rotating JWKS, least-privilege service identities and mTLS. | [Bolt authentication](docs/bolt-auth.md) and [TLS composition test](crates/server/tests/compose_tls.py) cover parts of the existing protected deployment path. Native mTLS/OIDC/JWKS contract remains open. |
| 35 | Verifiable personal-data erasure — **Open** | Delete personal data across current state, native history and retained backups under a defined retention policy. | Ordinary deletes do not prove erasure from RocksDB files/backups. No end-to-end verification established. |

## 8. Operations

| # | Item / status | Acceptance target | Current evidence and measurement |
|---|---|---|---|
| 36 | OpenTelemetry — **Open** | Query/operator traces, correlated logs/metrics and slow-query plans. | [Probe tests](crates/server/tests/probe.rs) and [connected memory](crates/server/tests/connected_memory.rs) cover existing observability subsets; no complete OTel pipeline or overhead measurement established. |
| 37 | Safe storage autotuning — **Open** | Adapt cache, memtables and compaction to workload within explicit safe limits. | [Memory defaults](docs/memory-defaults.md) derive budgets from limits; static/derived defaults do not establish adaptive tuning. Target not measured. |
| 38 | Rolling upgrades — **Open** | Upgrade three-voter cluster without downtime; prove data/availability across mixed versions and failures. | [Populated-volume upgrade](docs/volume-upgrade-acceptance.md) covers a single-node upgrade contract, not a rolling cluster upgrade. Target not measured. |
| 39 | Kubernetes operator and signed image — **Partial** | Operator, Helm chart, distroless image, cosign signature and SBOM. | Existing [container guide](docs/container.md) and deployment manifests cover a subset; operator/Helm/distroless/signature/SBOM acceptance is open. |
| 40 | Web operations console — **Open** | Lightweight UI for graphs, schema, slow queries, memory and replication. | Existing TUI is a terminal interface; [connected graph tests](crates/server/tests/connected_graph.rs) do not establish a web console. Target not measured. |

## 9. Ecosystem

| # | Item / status | Acceptance target | Current evidence and measurement |
|---|---|---|---|
| 41 | ISO GQL — **Open** | ISO/IEC 39075 surface using the shared planner, with a documented conformance corpus. | Supported Cypher is documented in [local Cypher](docs/local-cypher.md). No GQL conformance claim. |
| 42 | Arrow query output — **Open** | Flight/ADBC analytics plus Python to_arrow/to_pandas/to_polars output. | Existing Arrow input adapters do not establish Arrow query output or Flight/ADBC. Target not measured. |
| 43 | Official cross-language clients — **Partial** | Python/TypeScript/Go/Java use one typed error contract, including memory sizing and unknown commits. | [Python helper](docs/python-client.md) uses the Neo4j driver; it is not an independent four-language client family. No complete SDK contract qualification. |
| 44 | Embedded and WASM — **Partial** | Supported embedded library and WASM build for local applications/tests. | [Local Cypher](docs/local-cypher.md) and Rust store APIs run embedded with RocksDB. No supported browser/WASM storage build or portability qualification. |

## Delivery and measurement policy

Do not promote a roadmap item by adding only documentation. Each substantial
implementation starts with a short design in `docs/design/`, including limits,
alternatives, failure behavior and how it will be measured. Add a regression
that fails before the engine change, then run relevant tests and consolidate
with workspace tests, Clippy and formatting checks.

Use isolated synthetic stores and preserve existing data. Record the exact
source commit, candidate binary/image digest, dataset, resource limits, power
state, host load and concurrency. Benchmarks run without concurrent builds;
report repeated medians/p99 and correctness checks. An observed regression over
5% blocks a performance claim until explained and resolved. No number in this
roadmap proves comparative superiority over Neo4j, FalkorDB or another engine.
