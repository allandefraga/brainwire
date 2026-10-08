# Brainwire October 31, 2026 beta roadmap

Revised October 8 at Allan's request: retain October 31 as the beta target,
with **20 minutes of active development per day**. October 8's implementation
session is complete; October 9–31 provides 23 sessions, **460 minutes (7h40)**.
This revision supersedes the older full-scope execution dates and uses an
updated remaining-days count for the twenty-minute cadence.

The complete requested product remains the target described by the feature
matrix below. Delivering every pending row in 7h40 is not a credible commitment.
The executable schedule proposes a **controlled single-node client beta**;
it does not rename that subset as a completed distributed/full-feature beta.
Reduced October acceptance scope is a visible planning proposal, not an assertion
that Allan withdrew any requirement. If the complete matrix is mandatory on
October 31, additional development capacity or a different date is required.

Keep implementation, docs previews and release artifacts local. No push,
publication, DNS changes or deployment to an existing environment without explicit
authorization. This is a work queue, not a configured automatic daily run.

## Capacity and release rule

Spend each daily session on one dependency: approximately 3 minutes reviewing
state, 13 implementing/integrating, and 4 verifying and recording the next step.
For engine/security changes, focused workers and advisory review can assist within
the session; parallel agents do not make integration or failure tests free.
Large features span several sessions rather than being repeatedly abandoned.

Long local load/recovery checks may run beyond a session only when explicitly
started, supervised appropriately and their evidence is collected. Twenty minutes
of coding is not a one-hour endurance test. Do not assume scheduled wakeups or
unattended acceptance jobs exist. A blocked task consumes a later slot or displaces
a lower-priority task; there is no hidden buffer of unlimited extra development.

The October 12 gate assesses the client workload and API/storage gaps. October
24 is the candidate/feature-freeze gate. October 31 is a release decision based on
evidence, not a promise to deploy regardless of failed gates.

## Proposed October beta boundary

- One durable instance with multiple independent named graphs. Python/Bolt writes
  and supported Cypher/indexes remain the engine path. No second ingestion engine.
- client first consumes an explicitly scoped synthetic/anonymized data profile,
  then a separately authorized source snapshot. Define stable entity IDs, mapping,
  overwrite semantics and a replay-safe progress cursor before integration.
- DuckLake stays the priority source. Start with a pinned source snapshot exported
  to bounded immutable input and the existing importer, if direct snapshot reads
  cannot be validated in time. Call this **snapshot ingestion**, not DuckLake CDC.
  Source deletes, out-of-order events and arbitrary schema evolution remain
  unsupported until their independent contracts/tests pass.
- Token authentication and graph-scoped read/write/admin access are release blockers
  for any shared or production environment. Document the encrypted transport boundary.
- A supported Docker single-instance profile, retained storage, restart/checkpoint/
  restore acceptance, and explicit Kubernetes/Swarm evidence status. Container
  scheduling does not supply replica durability or machine-loss failover.
- Real supported TUI inspection/status, EN/PT-BR, a local documentation quickstart,
  limits, runbook and measured client workload. Connected unsupported panels stay
  explicitly unavailable; a demo panel is never acceptance evidence.

Use shadow mode first: DuckLake remains authoritative and a new independent graph
can be rebuilt. Beta testing and production readiness are different decisions.
Do not make Brainwire the sole copy of essential client records until security,
restoration, failure and workload acceptance pass and deployment is authorized.

## Current foundation — implemented, not the whole beta

- Independent Rust/RocksDB engine, multiple named graphs and versioned ontology.
- Python/Bolt individual and parameterized batch Cypher writes, identity MERGE,
  explicit property SET, snapshot transactions, atomic scalar index updates and
  synchronous WAL acknowledgement.
- String/Boolean/numeric equality indexes, typed adjacency seeks, bounded simple
  read pipelines, conservative query memory accounting and cooperative deadlines.
- Rich primitive/list/map values, scalar Cypher size() for list/Unicode text,
  CSR analytics, weighted-edge PageRank, degree, reachability/shortest path and OntoRank-inspired source ranking.
- Local checkpoints, integrity audit, retained-volume Docker/Compose/Kubernetes
  acceptance; Swarm templates have narrower execution evidence.
- Durable per-store commit sequence, snapshot inspection through Cypher/CLI,
  exact checkpoint receipts and a bounded immutable-JSONL Python importer whose
  source cursor commits atomically with each data batch (October 7). DuckLake
  change capture, general source deletion handling and replicated apply remain pending.
- Real offline graph/pipeline/memory/disk/ontology panels and connected ontology
  inspection. Other connected panels, query execution and live job progress remain
  work. Preserve the chosen strong-green Observatory identity and EN/PT-BR display.

The [October 6 measurements](benchmarks/2026-10-06-typed-adjacency.md) cover a
selective warm debug workload and crash acceptance; they are not beta performance
certification. Existing `docs-site/` is an untracked concept draft and must be
preserved and reconciled with implemented APIs before a release preview.

## Beta feature and acceptance matrix

All rows remain requested product requirements. This is the full-product matrix,
not a promise that its pending features fit the October 20-minute/day schedule.
The proposed October boundary above is narrower and explicitly identified. A
minimum is an explicit supported limit, not a placeholder or simulated TUI panel.

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

## Daily execution queue — October 9–31

Dates allocate a session, not guaranteed completion. Each deliverable needs a
runnable check or evidence; unfinished security/recovery work takes priority over
later interface polish. Carry-over is recorded in JOURNAL.md, with displaced dates
visible. Do not split a storage invariant merely to mark a day complete.

| Date | Task | Reviewable result / check |
| --- | --- | --- |
| Oct 9 | Freeze the client workload and proposed beta support boundary | Entity/relationship mapping, stable identities, required properties, important queries, volume/resource profile and targets; source assumptions listed |
| Oct 10 | Assemble the bounded Python integration fixture | Executable ontology + MERGE/SET import/query script, separate named graph, configurable batch rows/bytes and expected answers |
| Oct 11 | Add token verification at Bolt session boundary | Accepted/denied authentication, no token leakage; first replay/overwrite tests retained |
| Oct 12 | Enforce graph selection and transaction authorization; feasibility gate | Explicit/implicit transaction modes and database selection cannot bypass scope; assess remaining blockers/capacity |
| Oct 13 | Close read/write/procedure permissions | Negative tests for reads, writes, CALL and cross-graph access; failed authorization precedes mutation |
| Oct 14 | Audit administration/TUI access and transport configuration | No admin bypass; unsupported connected operations unavailable; encrypted transport boundary explicit |
| Oct 15 | Security and API-limit gate | Protected fixture reads/writes, bounded result/query timeout, cancellation and rollback; otherwise reserve later slots for blockers |
| Oct 16 | Define DuckLake snapshot consistency and source identity | Pinned snapshot identity, stable IDs, mapping and cursor; direct read versus explicit immutable export chosen from tested behavior |
| Oct 17 | Implement the chosen bounded snapshot bridge | Local DuckLake snapshot fixture if available, or documented export fallback; batch bytes/rows and source consistency enforced |
| Oct 18 | Exercise import replay/overwrite/failure | Duplicate batch, property change/null, changed input, crash/resume and index audit; source deletes explicitly unsupported unless proven |
| Oct 19 | Package the single-node runtime | Pinned local release image/configuration, persistent path, fresh install and Python smoke test |
| Oct 20 | Exercise retained-volume restart | Docker recovery and existing Kubernetes harness where available; acknowledged writes preserved; Swarm execution evidence stated |
| Oct 21 | Restore a checkpoint into a fresh store and resume import | Exact receipt/counter, data/index/source cursor audit, independent restored graph and restore time |
| Oct 22 | Run representative release benchmark | Agreed client queries; 100K nodes / 1M edges if within declared profile, representative payloads/two indexes; latency/throughput/RSS/host load |
| Oct 23 | Run and inspect extended mixed load/failure evidence | One-hour supervised local acceptance, or explicitly incomplete endurance gate; restart/replay and audit under the workload |
| Oct 24 | Candidate gate and feature freeze | Installs, protected import/query/restart/restore work; performance gaps and blockers recorded; remaining time goes to fixes |
| Oct 25 | Reconcile real TUI/status and local documentation | Graph/import/index/commit status where implemented, accurate live/offline labels, EN/PT-BR; executable quickstart/limits/runbook |
| Oct 26 | Rehearse the client shadow pilot locally | Install → schema → source/input → import → queries → restart → restore, with expected answers and provenance |
| Oct 27 | Prepare upgrade/cutover/rollback and local release package | Version/config compatibility, restore path, support matrix and unresolved limitations; no new major features |
| Oct 28 | Buffer: highest-priority blocker | Correctness/security/recovery first; affected acceptance repeated |
| Oct 29 | Buffer: integration/performance blocker | Fix measured issue or complete missing evidence; no cosmetic work ahead of blockers |
| Oct 30 | Final rehearsal and freeze verification | Candidate hashes, install/restore instructions, security/recovery checks and rollback reviewed |
| Oct 31 | Final beta decision | Ready/conditional/no-go against published checklist; local beta artifacts and release notes; deployment separately authorized |

If authentication/authorization cannot safely be implemented by October 15,
reserve subsequent feature slots for it. An unauthenticated single-node build may
remain an isolated developer test, but must not be called shared-production ready.
If the DuckLake bridge cannot pass source consistency/replay tests, ship only the
validated immutable-input importer and identify direct DuckLake integration as
incomplete. Neither fallback meets the original full-product beta matrix.

## Full-product backlog retained beyond the daily beta queue

| Workstream | Pending requirement | Dependency / completion evidence |
| --- | --- | --- |
| Distribution | Deterministic mutation/apply, three-voter Raft, leader loss, partitions, snapshots and whole-cluster restart | All graphs/catalogs/allocators/progress replicated; distinct-machine failure acceptance; local counter alone is insufficient |
| Search/indexes | Range indexes, lexical documents, exact vectors then ANN, resumable builds/freshness | Mutation/rebuild/delete correctness; measured recall/filter/latency and memory |
| Temporal/maintenance | Node AND edge history, valid/recorded clocks, weights across history, resumable retention/deprecation | Atomic revisions, ticket transitions, late events and exact dry-run/cleanup sets |
| Sources | Full DuckLake incremental change capture including deletes, DuckDB/Parquet/Delta adapters and Arrow comparison | Durable source event/checkpoint contract, schema evolution, replay and reconciliation |
| Storage/operation | Checksummed object-storage backups/blobs, validated multi-machine profiles and upgrades | Interrupted upload, complete manifests, restore/RPO and upgrade rollback |
| Product | Fully connected Observatory, query/jobs/history/maintenance tabs, hosted docs and MCP | Shared authenticated APIs, real bounded status and examples; publication separately authorized |

These are not dropped. Their dates require a new capacity decision based on the
October pilot evidence. The next large engine task remains deterministic apply;
this revision deliberately puts the client pilot blockers ahead of claiming
an untested distributed system in a 7h40 development budget.

Temporal/history and replication work cannot independently invent transaction or
snapshot formats. Use the existing approved design's **one Raft group per instance
covering all graphs**. This roadmap does not change that boundary. Resolve IDs,
clocks and nondeterministic values before deterministic replica application;
replicate allocator reservations, catalogs, graph registry/lifecycle, history,
maintenance and source-checkpoint mutations as well as user graph mutations.
Do not replicate Cypher text and assume all replicas generate the same state.

The transaction sequence is now persisted in the same synchronous batch as
data and continues after reopening; ID-block reservations also advance it through
internal transactions. This per-store counter is not a global replicated log
position or a deterministic apply contract. Snapshot installation needs a consistent graph registry/catalog,
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

## Beta decision gates

**October 12:** workload/queries and support boundary fixed; replay/overwrite,
crash resume and index audit demonstrated. Record the gaps, owners and displaced
sessions. Include first protected query-path evidence or record it as blocked.
This is not certification of the complete product matrix.

**October 15:** authentication, graph authorization and admin surface review pass,
or the build stays an isolated developer test. The transport boundary is explicit.

**October 24:** a candidate installs, ingests the agreed source form, answers the
important queries, restarts and restores independently. Any failed critical path
blocks feature freeze and consumes the remaining buffer.

**October 31:** classify separately:

- **client shadow beta:** validated source/input and queries on independent data,
  security/isolation appropriate to its environment, reproducible restart/restore,
  documented limits and measured workload results.
- **Production candidate:** all shadow gates plus shared-environment security,
  operational readiness, completed sustained/failure acceptance, tested rollback
  and explicit authorization for the actual production environment. Installation
  instructions or a container image alone do not satisfy this status.
- **Complete requested beta:** every row of the full feature matrix passes. No claim
  that this is achievable with the remaining 7h40.

Open data-loss, authorization, failed restore, unknown source consistency or
unbounded-memory blockers mean **no-go** for production. Report failed or unrun
tests rather than interpreting the calendar as a passing gate.

## Performance gates — engineering targets, not measured guarantees

These retained targets describe the original full product. For the proposed
October shadow beta, gate supported point/neighborhood/count/ingest workloads,
sustained load and single-node recovery. ANN and quorum acceptance apply only
once those implementations exist and are claimed as supported.

Freeze workload/resource assumptions by October 12; pin the local candidate image
during October 19 packaging. Reference: release binaries, 4 vCPU / 8 GiB per
member, SSD-backed retained storage, fixed cache/memtable/query budgets and a
documented network. Measure single-node and future replicated modes separately. Include one-kilobyte representative property payloads, two scalar
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
count measurement to every query. Freeze applicable shadow-beta targets by
October 12 and measure them on October 22. If that baseline cannot support a
target, show the gap and record a visible support limit or release decision; do not
quietly change the dataset, disable history or omit network/replication costs.

Use 100,000 nodes / 1,000,000 relationships as the representative supported
client profile where it fits the declared resources; a smaller passing profile
is explicitly a smaller support limit. The full product additionally requires
100,000 search vectors on its declared memory profile. Stress toward
1,000,000 nodes / 10,000,000 relationships with explicit bounds and separate
reports; a successful stress import does not certify every analytic/query at
that size. Collect p50/p95/p99, throughput, peak RSS, cache/memtable/compaction
pressure, write amplification, history growth, recovery and restore time.

## Daily execution and remaining months

The dated queue above is the active 20-minute/day plan. Each session reads the
newest journal entry, completes the next dependency, verifies it and records any
carry-over. Tests may span sessions; daily sessions are not automatically scheduled.

The November/December proposals below are historical planning context, not new
promises. Pending full-product requirements remain in the matrix and backlog.
After the October decision, re-estimate capacity for distributed durability,
search/history, connected operation and commercial support using real pilot data.

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
