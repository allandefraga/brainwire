# Brainwire

An independent graph database developed alongside client. The name comes
from the brain's wiring: neurons and the connections between them.

This is a standalone Rust repository at `/Users/fraga/Documents/brainwire`,
beside `client-platform`. Its crates build without the platform checkout.
client integration uses Bolt/Cypher and the planned Python SDK and repository
adapter; the platform remains the first integration target.

> **Status: local MVP development.** The Rust workspace contains the RocksDB
> storage foundation, graph operations, a local durable Bolt/Cypher server,
> configuration commands, a Cypher subset parser and atomic executor, and an
> interactive terminal backoffice and connected read-only workspace. The server is single-node and disables
> authentication by default; optional service-token protection now grants read/write
> access per named graph. Native TLS, human-password login and JWT validation are
> pending. Conservative per-RUN guards, a shared query reservation pool and
> cooperative execution deadlines are enforced; a hard process-memory ceiling
> remains pending. Decoded request allocations and authenticated network IO now
> have independent limits. A verified single-host TLS proxy is available; native
> TLS and replicated failover remain pending. The design is in
> [the approved spec](docs/superpowers/specs/2026-09-24-brainwire-design.md),
> the implementation tasks in
> [the MVP plan](docs/superpowers/plans/2026-09-26-brainwire-mvp.md), and session
> progress in [JOURNAL.md](JOURNAL.md).

Start with the [protected local MVP quickstart](docs/mvp-quickstart.md) to create
a named client shadow graph, import through Python and inspect the running
server in the TUI. The [October beta queue](docs/2026-10-pilot-roadmap.md) records
remaining release gates and the full-product backlog. The
[operator checklist](docs/beta-operator-checklist.md) covers supported TUI actions,
import/restart/restore steps and the final simulation boundary.

The [October 13 qualification](docs/benchmarks/2026-10-13-beta.md) records the
frozen candidate's 20 validated evidence receipts: a full-hour mixed load, pinned
100k-ticket DuckLake import, checkpoint restore/corruption refusal, three graphs
of 100k nodes each, strict TLS/permissions and real connected TUI captures.
Measurements are synthetic localhost observations; client node ACL integration
and protected target-cluster qualification remain separate release gates.

The terminal backoffice design is documented in
[Brainwire Observatory](docs/design/2026-09-26-brainwire-observatory.md), with
graph/query workflows, explained memory settings, disk inspection and concept
images. Allan selected the latest electric-green identity on 2026-09-26:
near-black background, restrained cyan and the connected B/W logo. That visual
identity is used by the first interactive TUI: Graph, Query, Memory and
Disk & Files. Run `brainwire shell --demo` to explore its synthetic scenario.
English is the default; use `--lang pt-BR` for Portuguese. Internal graph/schema
names remain English in either interface language.
It includes a clustered graph, guided query examples, explained memory budgets
and a file inspector with an illustrative LSM storage flow. See the
[terminal demo guide](docs/tui-demo.md) for controls and actual captures.
`brainwire shell --overview` is a separate read-only view of the real named
graphs on local disk. The [multi-graph backoffice guide](docs/multi-graph-backoffice.md)
shows current counts, commands and which TUI operations are live or synthetic.
`brainwire shell --overview --view ontology` now shows real schema connections,
with type selection, property/index inspection and incoming/outgoing signatures.
It uses quick count estimates and remains an offline, read-only inspector.
For a running server, `brainwire shell --connect 127.0.0.1:7687 --graph tickets --view ontology`
fetches a real read-only server snapshot, with manual refresh and stale-error status.
See the [connected ontology guide](docs/connected-ontology.md) and `CALL db.schema()`.
Connected shell defaults to a four-pane workspace: Ontology, Memory, Indexes and
Graph, navigated with Tab or keys 1–4. Memory explains real shared query-pool
counters; Indexes lists ready versus declared indexes. Graph inspects a selected
ontology signature and node through a bounded sample of up to 20 real outgoing
neighbors, with source existence checked in the same read transaction. Supply
`--node-label`, `--entity-id`, `--edge-type` and `--target-label` for this pane.
See [connected status](docs/connected-status.md) for permissions, bounds and controls.

For real graph reads and writes, run `brainwire serve` to connect over Bolt, or
use the embedded `brainwire cypher --database <directory> --query <text>` command
while the server is stopped. Queries commit atomically to RocksDB and survive
reopening. The [server guide](docs/serve.md) covers configuration, driver access
and current limits; the [single-node container guide](docs/container.md) covers
the local image and retained-volume test. The [local query guide](docs/local-cypher.md)
walks through ontology initialization, parameters, bulk JSON files and JSON output,
including lazy `coalesce(...)` defaults. Ontology, Memory, Indexes and the sampled
Graph pane connect to the running server; disk inspection still requires offline access. Real offline file metadata
browsing is available in the Disk panel. Offline
index readiness, RocksDB counters and shared memory accounting are available
in `shell --overview`. See [Python ingestion](docs/python-ingestion.md) for a
bounded JSONL importer, MERGE/SET semantics and crash-recovery acceptance.
The [client shadow guide](docs/client-shadow.md) adds a synthetic Zendesk-shaped
Ticket/Comment snapshot, source-qualified identities, atomic HAS_COMMENT links
and a cursor bound to both input digest and mapping contract.
It also includes an optional DuckDB SELECT exporter that publishes complete,
validated immutable JSONL for this importer, preserving null/list values and
refusing to overwrite an existing snapshot. A separate read-only SQLite DuckLake
exporter requires an explicit version and existing local managed files. Live
source qualification and DuckLake CDC remain pending. The
[client contract comparison](docs/client-contract-gap.md) records actual adapter
and node-level ACL gaps; the synthetic importer is not a production drop-in.
[Protected Bolt connections](docs/bolt-auth.md) cover Python/native credentials,
graph permissions and the optional [container overlays](docs/protected-containers.md).
The [TLS proxy guide](docs/protected-transport.md) documents strict Python driver
certificate/hostname checks and its single-host plaintext backend boundary.
[Connection limits](docs/connection-limits.md) explain decoded request quotas,
complete-request deadlines and Python pool liveness.
[Termination recovery](docs/termination-recovery.md) records abrupt SIGTERM
recovery; graceful drain and immediate cancellation are not implemented.

Sparse analytics now runs through Cypher: `CALL graph.project`, `graph.degree`
and `graph.pagerank`, plus directed `graph.reachable` hop distances and
`graph.shortestPath` node paths and `graph.wcc`/`graph.scc` connected groups,
with CSR projections built from the transaction
snapshot. See the [analytics guide](docs/sparse-analytics.md) for a complete
example, options, memory limits and a reproducible volume workload.

`CALL knowledge.rank` ranks competing sources for an exact term and optional
domain using explicit authority, author relevance, usage, certification and
freshness signals. It returns per-signal contributions and uses bounded top-K
selection with the existing term index. See [knowledge ranking](docs/knowledge-ranking.md)
for the synthetic example, policy and limits; automated evidence collection and
per-source permission-aware ranking are not implemented; protected readers still
need a grant on the selected graph.

## Local development

```sh
cargo test --workspace --locked --offline
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
cargo fmt --all -- --check
cargo run -p brainwire-server -- config show
cargo run --locked --offline -p brainwire-server -- shell --demo
```

Offline commands require dependencies to have been fetched previously.
The `brainwire` binary provides configuration commands, durable Bolt serving,
local Cypher execution, `brainwire probe` health checks and the terminal demo.
The `brainwire-bolt-dev` binary
uses a stub query engine.
It now also has local `brainwire db create`, `db list`, `db overview`, `db stats`, `db schema`,
`db metrics`, `db files`, `db checkpoint`, `db commit-state`, `db check` and `db apply-ontology` commands. They operate on the named RocksDB directories
under `server.data_dir` and require exclusive access; `db stats` scans nodes
and relationships and accepts `--json` for scripts. For example:

```sh
brainwire db create research --ontology examples/analytics/schema.json
brainwire db stats research
brainwire db list
```

Offline `db` commands require exclusive access: stop `brainwire serve` first.
For a fresh 100K-node / 1M-relationship synthetic database, run the
[volume example](crates/cypher/examples/sparse_analytics.rs) with
`1000000 --database data/staging-100k`, then inspect it with
`brainwire db stats staging-100k`. The example refuses to overwrite an
existing path. Local database files under `data/` are ignored by Git.
The [shared-query transaction API and Bolt lifecycle hooks](docs/transactions.md)
now back `brainwire serve`. The next MVP work is stronger driver, failure and
concurrency testing, orchestrator recovery acceptance with retained volumes,
and live TUI integration.
The [RocksDB/query/ontology audit](docs/benchmarks/2026-10-01-query-ontology-audit.md)
records the pre-index baseline. The first physical secondary-index slice now
accelerates string, Boolean and finite numeric equality; range and vector indexes,
large backfills, grouped/write streaming and bounded Bolt cursors remain
scale-critical work. Read-only MATCH/UNWIND supports early LIMIT and ungrouped
counts without materializing every input row.
The [first scalar-index measurement](docs/benchmarks/2026-10-01-scalar-index.md)
records the local test and the 100,000-key atomic backfill limit.
[Offline native checkpoints](docs/checkpoints.md) copy one committed graph
into a new directory. [Durable local commit state](docs/commit-state.md) survives
reopening and is visible through Cypher, CLI and the offline TUI pipeline.
The [resumable Python import example](docs/python-ingestion.md#resumable-immutable-jsonl)
commits its source cursor with each data batch.
[Query allocation guards](docs/query-allocation-guards.md)
explain limits, rollback behavior and their current ceilings.
Equality in `WHERE` can now seek the same ready scalar indexes as pattern
properties; `CALL db.indexes()` exposes readiness through Cypher. See
[index lookup](docs/indexes.md) and the reproducible warm benchmark example.
[Current measurements and recovery evidence](docs/benchmarks/2026-10-03-guards-indexes-checkpoints-recovery.md)
include numeric indexes and Python/Bolt ingestion at 100,000 nodes.
Typed one-hop queries now seek the existing node/relationship-type adjacency
prefix directly, avoiding unrelated edges. See the
[mixed-type neighborhood benchmark](docs/benchmarks/2026-10-06-typed-adjacency.md)
and its Python batch, rollback and restart acceptance check.
[Docker Compose, Kubernetes and Swarm templates](docs/orchestrators.md) package
a retained single-writer volume and recovery probes; replication remains pending.
The proposed [distributed deployment and routing design](docs/design/2026-10-12-distributed-deployment.md)
maps future members to Pods/tasks, machines and volumes, with separate client
entry and authenticated peer paths; these cluster contracts are not implemented.
The [October 31 beta roadmap](docs/2026-10-pilot-roadmap.md) now targets working
minimums across all requested areas, with an October 12 feasibility gate and
explicit performance/recovery acceptance. It is a target, not an implemented
feature report or a guaranteed delivery date.
The [temporal graph and maintenance design](docs/design/2026-10-01-temporal-graph-and-maintenance.md)
defines the next steps for ticket history, node and edge weights, retention
and safe cleanup after ontology changes.

The local history was extracted from the platform's Brainwire feature branch.
Old journal entries retain their original commit IDs and checkout paths; the
rewritten history in this repository contains only Brainwire. No remote is
configured and no code has been pushed.

## Requirements (2026-09-24)

| Topic | Decision |
|---|---|
| Motivation | RAM and scale ceilings, a license that works for BYOC, native ACL and events, R&D. Several graphs/databases per instance. |
| Scale | A graph lives entirely on one node in the current stage. Kubernetes and Docker Swarm are deployment targets; multi-machine replication and later partitioning are explicit goals, not current capabilities. The ID and file formats must not prevent partitioning later. |
| Volume | 5 billion elements per graph with 16 GB of RAM. Most of it lives on disk, compact and easy to recover. |
| Query | Bolt protocol + Cypher, starting from the measured subset (code + `run_cypher` logs), each construct validated against the openCypher TCK. |
| Events | Entity timeline (today `Event` + `HAD_EVENT`): append-only, read by entity and time range. |
| Team | Allan + Claude agents, 45 minutes of work per day. |
| Deadline | Launch on 2026-10-31. Work like a Neo4j Enterprise, with cluster/HA, a simple RBAC and full openCypher (plan, section 1). |
| ACL | The client model (`AclPredicate`, ADR-016/017), evaluated inside the engine (plan, section 5). |
| Ontology | The platform's `graph_mapping.yaml` becomes the engine's schema. |
| Python | SDK with bulk, merge, index creation and delete, in the style of the port. |
| Ingestion sources | DuckLake first for client; DuckDB, Parquet and Delta Lake also targeted. Native Arrow batches into Rust, resumable incremental synchronization and TUI progress are planned; the current working path is Python/Bolt. |
| Surfaces | Animated CLI/TUI (graphs, queries, charts, disk, ontology, admin) and a website at the end, in the spirit of the HydraDB site. |
| Intelligence | Graph algorithms (depth, Dijkstra, proximity, similarity, centrality, community) and AI questions about a node ("quais são os concorrentes?", "who are the competitors?"). |
| Acceptance | The `GraphRepository` port (ADR-023) and the `client-core/tests/test_graph_repository_contract.py` suite. |

## Consequences already measured

- 16 GB for 5 billion elements is about 3 bytes of RAM per element. RAM is a
  cache; the on-disk format is the product.
- "Millisecond answers" hold for the hot set or for local NVMe. A cold
  expansion that touches 93 thousand nodes (the hub in doc 27) does about 93
  thousand random reads: about 30 s on an EBS gp3 volume at its 3,000 IOPS
  baseline.
- Compactness yardstick: Neo4j used 1.4 GB for 20M elements in doc 08 (70
  bytes per element, lean shape). At 5 billion that would be about 350 GB.
- On the worker's real path, the ingestion ceiling (about 10 thousand
  elements/s) comes from entity resolution, not from the graph (doc 08, §3).

## Open

- Language decided on 2026-09-24: Rust on top of RocksDB (plan, section 2).
- Transaction, index, physical deletion (LGPD) and multi-database details come
  out in the spec.
- Versioning and the release flow: parked for later.
