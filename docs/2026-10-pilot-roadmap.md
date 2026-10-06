# Brainwire October 31, 2026 beta roadmap

Revised October 6 at Allan's request: target a beta by October 31 covering every
requested product area with a working minimum and strong measured performance.
This replaces the earlier narrow October pilot scope. It is an ambitious target,
not evidence that pending features already exist or a guaranteed delivery date.
Keep implementation, documentation previews and release artifacts local; no push,
publication, DNS changes or deployment to an existing environment without the
user's explicit instruction.

## Capacity and release rule

There are 25 days from October 7 through October 31. Twenty-minute daily sessions
would provide only eight hours and twenty minutes of main-session time. The complete scope
needs sustained implementation, focused parallel work and integration time.
Subagents help independent slices; they do not eliminate shared storage and
correctness work. Maintain at most three coordinated workstreams: engine and
replication; data/ingestion/search; operation/TUI/docs. File ownership must not
overlap; shared transaction/storage contracts are integrated by the main agent.

On October 12, report whether the complete target is feasible with actual
progress and available work time. If the critical gates fail, mark the full beta
at risk and explicitly resolve capacity, date or scope with Allan. Do not silently
remove replication/history/search and keep calling the smaller pilot the complete
beta. A failed feature remains visibly incomplete; dates never override correctness.

## Current foundation — implemented, not the whole beta

- Independent Rust/RocksDB engine, multiple named graphs and versioned ontology.
- Python/Bolt individual and parameterized batch Cypher writes, identity MERGE,
  explicit property SET, snapshot transactions, atomic scalar index updates and
  synchronous WAL acknowledgement.
- String/Boolean/numeric equality indexes, typed adjacency seeks, bounded simple
  read pipelines, conservative query memory accounting and cooperative deadlines.
- Rich primitive/list/map values, CSR analytics, weighted-edge PageRank, degree,
  reachability/shortest path and OntoRank-inspired source ranking.
- Local checkpoints, integrity audit, retained-volume Docker/Compose/Kubernetes
  acceptance; Swarm templates have narrower execution evidence.
- Real offline graph/pipeline/memory/disk/ontology panels and connected ontology
  inspection. Other connected panels, query execution and live job progress remain
  work. Preserve the chosen strong-green Observatory identity and EN/PT-BR display.

The [October 6 measurements](benchmarks/2026-10-06-typed-adjacency.md) cover a
selective warm debug workload and crash acceptance; they are not beta performance
certification. Existing `docs-site/` is an untracked concept draft and must be
preserved and reconciled with implemented APIs before a release preview.

## Beta feature and acceptance matrix

All rows belong to the target. A minimum is an explicit supported limit, not a
placeholder, a simulated TUI panel or an undocumented fallback.

| Area | Minimum functional beta | Required acceptance |
| --- | --- | --- |
| Multiple graphs | Create/select independent named graphs, catalogs, data, indexes and permissions through documented administration and Python | Isolation, concurrent use, restart, backup/restore; no cross-graph transactions promised |
| Values | Published versioned matrix for scalar/list/map, binary, temporal and dense-vector values; explicit size/depth/dimension limits | Python/protocol/codec round trips, invalid values rejected, upgrade/reopen tests; no promise to serialize arbitrary Python objects |
| Cypher and functions | Stable documented subset for create/upsert/update/delete/traversal/filter/aggregate, scalar/list/date functions and procedure calls; bounded cursors and cancellation | Supported queries match expected answers; unsupported syntax errors; slow/disconnected clients release resources |
| Indexes | Equality and bounded numeric/time range access; restart-safe resumable builds with readiness and progress | Insert/overwrite/null/delete and concurrent build correctness, cancellation/restart and full audit |
| Text/document/vector search | Documents/chunks linked to graph entities, basic lexical ranking and dense cosine top-k with metadata filters; exact reference plus measured ANN index | Dimension/model checks; updates/deletes/restart/rebuild; exact-reference recall, filter correctness, latency and memory at supported volume; no Qdrant API compatibility claim |
| Sparse analytics/ranking | CSR, existing algorithms and source ranking; explicit node/edge weight properties with validated meanings/defaults/ranges | Known-answer graphs, score provenance, bounded memory/runtime; historical queries use historical weights |
| History | Atomic node AND edge revisions, durable commit identity, recorded/valid clocks, event identity and bounded history/as-of reads with documented late-update rules | Ticket transitions, equal timestamps, late events, rollback, concurrent writes, crash/replay, temporal neighborhood correctness; reject unsupported ambiguous intervals |
| Maintenance | Retention in days/calendar months and ontology deprecation; explicit dry-run and resumable scoped cleanup jobs with progress/pause/resume | Exact retained/deleted sets, crash between batches, both edge mirrors/indexes/history consistent; no automatic destructive purge on ontology apply |
| Python and incremental sources | Single/batch API with bounded rows/bytes, durable replay-safe source checkpoint; DuckLake first, DuckDB/Parquet/Delta adapters reuse that contract | Inserts/updates/source deletions, duplicate/out-of-order events, restart between source read and graph commit, schema changes and reconciliation; a timestamp watermark alone is insufficient |
| Arrow | Benchmark bounded Arrow batches against the existing Python/Bolt path; adopt a fast path only if it wins while preserving semantics | End-to-end throughput, conversion/peak memory, rollback/index/checkpoint consistency; no zero-copy claim without measurement |
| Local/object storage | Local durable working store; consistent local checkpoints and checksummed object-store backups/document blobs with restore into fresh storage | Interrupted uploads rejected, complete manifest, checksums, restore/audit and documented RPO; live RocksDB files are not served directly from S3 |
| Distribution | Experimental three-voter replicated instance covering all its graphs, static membership, one leader, quorum writes and explicit read consistency | Distinct-machine leader loss, minority partition fencing, whole-cluster restart, snapshot catch-up and no missing acknowledged writes; same-host containers prove only process failures |
| Docker/Kubernetes/Swarm | Reproducible image, single-node and three-member profiles, persistent member storage, configuration, probes and upgrade/rollback instructions | Execute each claimed target; retained storage and placement tests; orchestrator scheduling is not replication; never share one RocksDB directory between writers |
| Security | Token authentication and graph-scoped read/write/admin authorization; documented encrypted transport boundary | Denied reads/writes/admin operations, invalid credentials and graph isolation; TUI/MCP/Python share policy; per-node ACLs require separate evidence |
| TUI/backoffice | Real graph selector and bounded neighborhood, query results, memory explanation, disk/SST metadata, ontology connections, indexes, ingestion/history/maintenance/admin status | Every tab uses real data with timestamps/live/stale/estimate labels, responsive bounded refresh, EN/PT-BR and selected green identity; original composition remains the design reference |
| Documentation/MCP | Local docs-site release preview with quickstart, Python, Cypher, schema/search/history, sizing/deploy/recovery; thin bounded MCP tools reuse engine/auth APIs | Executable examples, denied operations, result limits, no second query/storage implementation; public hosting awaits explicit publication instruction |

Unsupported arbitrary data types, complete Cypher, general Qdrant compatibility,
unbounded values/results and partitioning one graph across machines are not
implied by these minima. The replicated beta copies the instance's full data set;
sharding and distributed execution require their own future design and tests.

## Sequence and dates

| Dates | Engine/replication critical path | Data/search path | Operation and product path | Exit gate |
| --- | --- | --- | --- | --- |
| October 7–12 | Durable commit/apply identity; deterministic mutation contract; node-read batching investigation; bounded Bolt cursors; first three-voter durable-write/restart prototype | Freeze value/index semantics; replay-safe atomic source checkpoint; DuckLake change contract and release benchmark baseline | Authentication/graph scopes; pinned benchmark profile; existing orchestrator acceptance rerun; live API inventory | October 12 feasibility gate below |
| October 13–19 | Replica apply/snapshot/restart; allocator/catalog/graph lifecycle replication; node+edge history and current indexes atomic | Python incremental adapters, range/resumable indexes, lexical search, exact vector reference then ANN integration | History/maintenance API status, real connected graph/query/memory/disk views; backup manifests and object-store restore | Each new write path passes rollback, restart and replicated replay; searches never silently return stale/deleted data |
| October 20–26 | Leader-loss/partition/snapshot catch-up; resumable retention/deprecation; mixed-load performance and recovery hardening | Full source replay including deletes/out-of-order events; search recall/rebuild; client-shaped integration fixture | Three target orchestrators, all connected TUI tabs, docs preview and thin MCP; Python end-to-end walkthrough | Integrated candidate with every matrix row demonstrable and measured |
| October 27–31 | Feature freeze; sustained load, repeated failures, restore and upgrade/rollback; fix beta blockers | Validate exact source/graph/history/search agreement after recovery | Run documented install-to-restore flows, capture every TUI tab, package locally and publish truthful support matrix | Beta only if correctness, recovery and performance gates pass; no unreviewed late feature additions |

Temporal/history and replication work cannot independently invent transaction or
snapshot formats. Use the existing approved design's **one Raft group per instance
covering all graphs**. This roadmap does not change that boundary. Resolve IDs,
clocks and nondeterministic values before deterministic replica application;
replicate allocator reservations, catalogs, graph registry/lifecycle, history,
maintenance and source-checkpoint mutations as well as user graph mutations.
Do not replicate Cypher text and assume all replicas generate the same state.

The current transaction conflict sequence is in memory and advances after the
RocksDB write; it is not a durable revision/log position. ID-block reservations
also use internal transactions. The initial durable apply contract must account
for both. Snapshot installation needs a consistent graph registry/catalog,
per-store state and committed/applied log position. Use a mature consensus
implementation selected through a bounded integration spike; do not invent a
home-grown consensus algorithm to meet the date.

Derived text/vector indexes have explicit applied positions and visibility rules:
wait for freshness, use a bounded exact fallback or report not-ready. Preserve
filter/permission correctness, rather than filtering an unrestricted top-k after
search. Histories, checkpoints and indexes must not expose partially committed
state after recovery. Text/vector indexes must rebuild from durable canonical
records without regenerating embeddings externally. A completed backup manifest
includes every referenced document blob and checksum; restore must verify those
objects before declaring the recovered document/search service ready.

## October 12 feasibility checkpoint

Require all of the following before declaring the complete October beta on track:

1. Representative release-mode baseline and frozen beta workload/resource profile.
2. Durable commit/apply identity across restart, including internal allocations.
3. Graph mutation, index/history metadata and replay-safe batch/source checkpoint
   share an atomic/recoverable contract; idempotent replay has exact expected state.
4. Bounded Python reads/writes/cursors, cancellation and graph-scoped authentication.
5. A three-voter prototype commits deterministic mutations durably and recovers
   after restart. Explicitly record local versus distinct-machine test coverage.
6. Reviewed contracts for vector/text freshness, temporal clocks and all four
   incremental sources. Delta/DuckLake source deletions must have a real strategy.

Failing these is a risk to the **whole requested beta**, not an excuse to rename a
single-node subset as the completed distributed release. Do not consume the final
five-day freeze with unfinished feature construction unnoticed.

## Performance gates — engineering targets, not measured guarantees

Freeze the reference configuration by October 12: release binaries, pinned image,
4 vCPU / 8 GiB per member, SSD-backed retained storage, fixed cache/memtable/query
budgets and a documented network. Measure single-node and replicated modes
separately. Include one-kilobyte representative property payloads, two scalar
indexes, skewed degrees, updates/deletes and concurrent readers/writers. History,
text and vector costs need separate enabled profiles rather than being hidden.

Initial targets to validate against that configuration:

| Workload | Target beta gate |
| --- | --- |
| Python/Bolt primary or selective scalar lookup, four clients | p95 <= 5 ms warm; separately report cold and mixed-load latency |
| Typed neighborhood returning up to 20 small records | p95 <= 10 ms warm |
| Indexed count over 10,000 matches | p95 <= 200 ms, without materializing every match |
| Python batch ingest, 1 KiB/node, two scalar indexes, history/search off | >= 5,000 committed nodes/s sustained; separately measure overwrite and replicated throughput |
| Filtered ANN search, 100,000 vectors, one frozen 384/768-dimensional profile | recall@10 >= 0.95 versus filtered exact reference and p95 <= 50 ms on specified selectivity/workload |
| Supported mixed workload | One-hour soak with bounded RSS, no unbounded queues/results and no missing acknowledged graph/index/history mutations |
| Recovery | Zero missing acknowledged commits in supported quorum/retained-storage scenarios; record RTO, stale-index rebuild and readiness times; backup restore has its own measured RPO |

These are proposed pass/fail targets, not extensions of today's 0.112 ms debug
count measurement to every query. If October 12 baseline cannot support a target,
show the measured gap and negotiate a visible target or release decision; do not
quietly change the dataset, disable history or omit network/replication costs.

Use 100,000 nodes / 1,000,000 relationships as the mandatory representative beta
profile, plus 100,000 search vectors on its declared memory profile. Stress toward
1,000,000 nodes / 10,000,000 relationships with explicit bounds and separate
reports; a successful stress import does not certify every analytic/query at
that size. Collect p50/p95/p99, throughput, peak RSS, cache/memtable/compaction
pressure, write amplification, history growth, recovery and restore time.

## Daily execution and remaining months

Each session selects the next dependency from this roadmap, makes a bounded
reviewable implementation, tests correctness and records measurements in the
journal. Use focused authorized subagents with explicit ownership, and advisory
architecture review before consequential storage/replication decisions. Avoid
spending engine sessions on extra visual polish before a critical gate passes.
The next implementation is the durable commit/apply and bounded API feasibility
slice, paired with the measured node-read bottleneck; start replication early.

The old November/December proposals below remain historical planning context.
Features promoted into the October matrix are no longer silently deferred by
those older sections. After beta, prioritize hardening and measured scale,
commercial support boundaries, more complete language/search semantics, dynamic
cluster membership and eventual graph partitioning based on pilot evidence.

## November extension, proposed October 3

Allan also asked what could be available by November 30 beyond MCP. Target a
closed beta only if the October pilot gates pass and development continues at a
similar cadence. This is a prioritization proposal, not an approved promise to
complete every item by that date.

1. Incremental ingestion, prioritizing DuckLake/DuckDB/Parquet: durable source
   checkpoints, bounded batches, resumability, stable identities and idempotent
   replay. Measure Arrow against the existing Python/Bolt path before adopting
   a separate transport.
2. Query maturity: bounded result delivery, cancellation, improved access-path
   selection and sustained mixed read/write workloads. Publish latency, memory,
   failure and recovery results with reproducible datasets.
3. Operational boundaries for external users: authentication, per-graph access,
   transport security, live metrics, backup/restore and documented upgrades.
4. A connected TUI and documentation covering supported operations accurately,
   including understandable errors and the distinction between configured
   budgets, estimates and measured process usage.
5. A first ticket-history/retention slice after the foundations above: explicit
   revision semantics, bounded retention and auditable maintenance. Ontology
   changes must not silently delete previously stored records.

MCP should adapt the same supported APIs and permission model, with bounded
results and explicit mutation scope. It does not replace the Python driver or
create a second storage/query implementation. Vector search and multi-machine
replication remain explicit competing priorities; do not promise both plus all
beta gates in the same month. Prefer measured client requirements when choosing
which large capability follows reliable ingestion and operation.

## December extension, proposed October 3

Allan asked what could follow by December 31. Aim for a first commercially
supported release with a narrow published scope, conditional on the pilot and
closed beta demonstrating stability. A self-hosted database plus support has a
smaller operational scope than simultaneously building a fully managed SaaS.

The desired release includes independent Docker/Compose/Kubernetes installation,
multiple graphs, Python and MCP over the same APIs, individual/batch writes,
incremental client ingestion, measured query/index behavior, basic history and
retention, a connected polished TUI, versioned documentation, authentication,
graph isolation, metrics, backup/restore and a documented upgrade path.

The commercial gate is evidence: representative sustained workloads, fault
injection, successful restore and upgrade/recovery procedures, and resolution
of beta correctness issues. Record supported volumes and limits from actual
tests rather than promising generic billion-element capacity. Do not sell
replication or automatic failover as supported just because Kubernetes packaging
exists. A first distributed implementation could be explored, but it needs its
own acceptance campaign before inclusion in the supported offer.

Proposed progression: October pilot, November closed beta, December scoped
commercial release. Stability determines each transition; dates alone do not.
