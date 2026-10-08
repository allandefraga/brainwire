# Brainwire journal

Sessions start when Allan asks; his requested duration overrides the 45-minute
default. This file is the memory
between sessions: each session starts with no context other than this file and
the README.

Internal documents are in English (Allan, 2026-09-24). The conversation follows
Allan's current language.

## How a session starts

1. Read the [README.md](README.md) and the newest entry below.
2. Run `date` and note the start time and requested duration. Reserve the final
   five minutes for verification and handoff; complete necessary cleanup.
3. Do the **Next step** of the newest entry. If it depends on a decision from
   Allan, ask first, one question at a time, and meanwhile move forward on
   whatever does not depend on it (research, corpus, drafts of later sections).

## How a session ends

1. A new entry **at the top** of the list, with: done, decided, next step,
   blockers.
2. Brainwire now lives in its standalone repository at
   `/Users/fraga/Documents/brainwire`, on `codex/brainwire`. Nothing is pushed
   unless Allan asks. The old platform branch and its worktrees are historical
   recovery points. Older entries below retain their original paths and commit
   IDs; standalone history has rewritten IDs after subtree extraction.

## Phases

1. Brainstorming (requirements, approaches, design section by section).
2. Spec in `docs/superpowers/specs/`, reviewed by Allan.
3. Implementation plan broken into tasks (skill `superpowers:writing-plans`).
4. Implementation: tasks from the plan, with TDD, several per session through
   parallel agents.

No line of code before the end of phase 2.

## Entries

### 2026-10-08 — October beta daily queue revised

- **Requested:** organize the remaining month for an October 31 client-testable
  beta toward production. Allan revised the cadence from 15 to **20 minutes/day**.
  October 9–31 gives 23 sessions / 460 minutes / 7h40, excluding today's completed
  implementation. This planning turn is not another timed coding session.
- **Done:** revised docs/2026-10-pilot-roadmap.md with a dated queue, security
  early, DuckLake snapshot ingestion, Python replay/overwrite, install/restart/
  restore, representative performance and one-hour acceptance evidence, real
  TUI/docs, candidate freeze and three final buffer days. Added the cadence to
  AGENTS.md so future sessions retain it. No automation or deployment created.
- **Boundary:** proposes a controlled single-node client shadow beta; the full
  requested matrix remains visible and is not credibly guaranteed in 7h40.
  Replication, ANN/search, complete history/maintenance, all source connectors,
  object backup, full connected TUI/MCP remain requirements, not completed work.
  Production status requires security, recovery, workload and explicit deployment
  authorization. DuckLake snapshot/export ingestion is not CDC or delete sync.
- **Review/checks:** advisory Astra checked priorities/dependencies; main applied
  the security-first order and protected freeze/fix buffer. Documentation-only:
  arithmetic/dates/links and git diff checks; no engine tests rerun unnecessarily.
- **Next:** October 9 session freezes the client schema/IDs, representative
  source shape, queries and proposed beta support boundary. If source details are
  missing, list assumptions and use synthetic data until confirmed.
- **Local:** docs/AGENTS/journal changes only; no push, publication or deployment.

### 2026-10-08 — Cypher size and gated pinned-read experiment

- **Requested:** 15 minutes of features/engineering/platform, all local. Started
  07:17:09 UTC (09:17:09 Europe/Paris); closed at 07:32:32 UTC after
  at least 15 minutes.
- **Done:** scalar `size(expression)` accepts a list, Unicode string or null;
  counts code points rather than bytes/graphemes, propagates null and rejects
  other types/invalid arity. Shared evaluation works in parameters, predicates,
  UNWIND, SET, grouped RETURN and count arguments. Existing nesting guards,
  variable validation, aggregate restrictions and allocation accounting remain.
- **Verified:** four integration tests cover Unicode/decomposed accents, lists,
  null, parameters, missing properties, unknown variables, wrong types/arity,
  nested aggregates/depth, grouped counts, successful SET and atomic rollback
  after a row-dependent runtime failure following CREATE. Final 438 workspace
  tests pass; workspace all-targets Clippy, rustfmt and diff checks pass.
  Actual CLI returns [2,3,null]. Python Neo4j driver 5.28.6 against a disposable
  local Bolt server returns Unicode/list sizes and persists SET length=2;
  server stopped and its temporary data removed. Runnable evidence:
  docs/benchmarks/2026-10-08-size/python_bolt.py and python-bolt.json.
- **Investigated:** crate-private callback decodes node bytes from overlay or
  Snapshot::get_pinned_cf, eliminating one intermediate raw Vec copy. Its
  snapshot/tombstone/error/LIMIT semantics and 435-test candidate workspace
  passed, but its performance gate did not. Initial five alternating release
  pairs favored 10K/4KiB median by ~10%; subsequent ten pairs did not repeat
  the gain. External virtualization and other-project Python CPU load were
  observed and left untouched. No isolated-host, RSS or production speed claim.
  Reverted candidate production helper and routing together; no unused API.
- **Engineering:** existing typed_adjacency benchmark now supports optional
  0–16KiB text payloads. Audited 10K empty, 10K/4KiB and 50K/4KiB stores with
  up to 1,000,020 edges. Retained baseline/candidate hashes, 90 raw timing
  documents, summarizer and candidate patch in
  docs/benchmarks/2026-10-08-pinned-read/. Highly compressible synthetic text,
  warm embedded timing, process-percentile medians explicitly documented.
- **Review:** focused storage/Cypher worker and advisory Astra; main independently
  ran workspace checks and CLI/Python integration. No dependencies introduced.
  Docs explain the scalar contract and update the read-batching investigation.
- **Next:** deterministic mutation/apply contract covering catalog, allocator and
  source-progress writes before three-voter replication; retry destination-read
  optimization only on a controlled idle host with incompressible/skewed values
  and allocation/RSS evidence. size() is not vector similarity or ANN search.
- **Local:** canonical /Users/fraga/Documents/brainwire; preserve untracked
  docs-site/ and .DS_Store files. No remote push/publication. All synthetic
  benchmark fixtures and local test executables removed before handoff.

## 2026-10-07, durable commits, resumable ingestion and extended acceptance

- **Requested:** work for 30 minutes, all local. Started 06:57:45 UTC
  (08:57:45 Europe/Paris); closed at 07:28:01 UTC after at least 30 minutes.
  Preserved the historical worktree and untracked
  docs-site/.DS_Store files. Canonical checkout remains /Users/fraga/Documents/brainwire.
- **Done:** versioned durable per-store commit counter shares the synchronous
  graph/index WriteBatch. Reopening continues it. Missing legacy state is
  explicitly untracked; overflow/corruption/reserved-key mutation fails closed.
  Snapshot counter does not follow pending/later writes. Ordinary disjoint
  writers do not depend on the internal marker; explicit marker dependencies
  still conflict. Allocator reservations are included and can survive outer rollback.
- **Done:** CALL db.commitState() yields exact decimal sequence/tracked/scope;
  offline db commit-state and overview JSON expose latest local state. Native
  checkpoint receipt captures the exact counter under the commit lock, retaining
  the original checkpoint API. Eight concurrent captures match marker and data.
  TUI pipeline explains real counters in EN/PT, including untracked/u64::MAX.
- **Done:** immutable-JSONL Python example hashes input incrementally, bounds
  rows/bytes/line size, and commits ImportCheckpoint source cursor with each data
  batch. Every retry rereads graph progress; completed resume is a no-op. One
  importer per source required. No DuckLake/CDC/delete propagation or general SDK
  claim. Examples now use a distinct customers-resumable graph.
- **Verified:** 434 Rust workspace tests, all-targets Clippy, rustfmt/diff checks;
  nine Python algorithm tests. Existing/new store crash suites cover eight
  SIGKILL cycles. Actual Python/Bolt tests cover old snapshots, disjoint writers,
  pending-index-update kill/restart, checkpoint independence and full audits.
  Actual importer failure after customer writes/before cursor update rolls back;
  lost receipt followed by kill/restart resumes safely and rejects changed input.
  Both driver 5.28.6 and a separate 10K driver 6.3.0 resumable/recovery run pass;
  the actual importer CLI also resumes a completed source without a new commit.
- **Volume:** 100K nodes with 16D values and 100K with seeded-random 768D values
  round-trip, recover and audit. Wide JSONL was 1.57 GB with <=1 MiB batch input;
  ~1,464 rows/s under simultaneous independent soak, not an ANN benchmark.
- **Mixed load:** 601.15-second release soak, one writer/four readers, 100K nodes:
  172,253 committed 250-row batches (43,063,250 updates), 3,846,944 point reads.
  Reader last-10K-window p95 1.259–1.269 ms; writer last-window p95 3.97 ms/batch.
  RSS peak 93.42 MiB, final 88.02 MiB with 64 MiB cache/16 MiB memtables. Indexed
  segment counts agree; SIGKILL/reopen counter exactly 172752; full audit passes.
  Short isolated 100K/1KiB/two-index release insert run: 2.459 s, ~40,661 rows/s,
  7.027 ms batch p95. No pinned 4-vCPU/8-GiB or one-hour-beta certification claimed.
- **UI:** six actual offline 160x48 PTY captures (five EN tabs and PT pipeline)
  for 100K nodes/zero instance edges; sequence121 agrees with CLI and inspection
  leaves it unchanged. Assets/receipt/capture script in
  docs/design/assets/2026-10-07-commit-state/. Main inspected EN/PT pipeline and
  disk PNGs and then independently reviewed all six captures. No test server,
  PTY or synthetic store remains.
- **Review:** focused storage, Cypher/Python and TUI workers with main independent
  integration checks. Astra reviewed the architecture and final code; no code
  blockers. Fixed its docs findings about exclusive embedded access and example
  graph/path collision. Checkpoints block begins/commits/status during capture.
  Counter is local, not a Raft position/bookmark/as-of identifier. Writable
  downgrade is unsupported; checkpoint forks can reuse future numbers.
- **Next:** deterministic mutation/apply contract covering all graph catalogs,
  allocators and source-progress writes before three-voter replication. In
  parallel, perform the scoped release destination-read batching experiment in
  docs/design/2026-10-07-batched-node-read-spike.md: compare copied/pinned multi-get,
  preserve order/overlay/snapshot/LIMIT/error timing and decide a result-byte policy.
  Current key-count caps alone do not bound arbitrary node payload bytes.
- **Evidence:** docs/commit-state.md, updated Python/transaction/checkpoint/serve
  guides and roadmap; docs/benchmarks/2026-10-07-commit-state.md and raw scripts,
  JSON and final source/binary hashes. Replication, temporal history, ANN, auth,
  full connected TUI and source connectors remain unfinished beta work.
- **Blockers:** none for this slice. Keep future sessions substantial and timed;
  no push, publication or remote PR was performed.

### 2026-10-06, expanded October 31 beta target

- **Requested:** reorganize the plan for an October-end beta with every requested
  product area minimally functional and strong performance.
- **Done:** revised the existing `docs/2026-10-pilot-roadmap.md` instead of adding
  another competing roadmap. The October beta matrix includes graph/value/Cypher
  functionality, scalar/range/text/vector indexes, CSR/ranking/weights, node AND
  edge history, retention/deprecation jobs, Python incremental DuckLake/DuckDB/
  Parquet/Delta, Arrow measurement, local/object backup roles, three-voter instance
  replication, Docker/Kubernetes/Swarm, authentication, connected TUI, docs and MCP.
  Existing November/December sections remain explicitly historical context.
- **Decided:** preserve the approved one-Raft-group-per-instance boundary across
  all graphs. Start durable commit/apply identity and replication immediately;
  include allocator/catalog/registry/internal mutations, source checkpoints and
  maintenance in replay/snapshot contracts. Object storage is for checksummed
  backups/document blobs and restore; live RocksDB still requires local storage.
- **Schedule:** October 7–12 foundation/feasibility; 13–19 data/search/history and
  replication; 20–26 integrated candidate; 27–31 freeze, soak/failure/restore and
  documentation acceptance. No implied remote publication or deployment.
- **Performance:** proposed, unproven release-mode targets on a frozen resource
  profile, representative payloads and separate single-node/replicated/history/
  search modes. Mandatory 100K nodes/1M relationships and 100K vectors, stress
  toward 1M nodes/10M relationships with explicit supported bounds. Measure p95,
  throughput/RSS, recall, compaction, recovery and exact post-failure state.
- **Risk:** all-area beta is a stretch target. Twenty minutes daily across the
  remaining 25 days is only eight hours and twenty minutes; sustained work and integration
  capacity are required. October 12 explicitly assesses feasibility and raises
  capacity/date/scope if gates fail. Do not silently deliver a narrow pilot as
  the requested full beta or claim same-host containers prove machine-loss HA.
- **Verification:** documentation-only edit, local link/scope consistency review;
  advisory Astra reviewed architectural dependencies and release feasibility.
  No implementation tests rerun for prose. Existing functionality is unchanged.
- **Next:** durable commit/apply and bounded API feasibility slice, with early
  three-voter durable-write/restart prototype and release profiling of node reads.

### 2026-10-06, typed adjacency seeks and million-relationship acceptance

- **Requested:** 20 minutes on database-engine performance/features, keeping all
  changes local. Session began at 10:06 Europe/Paris; canonical standalone checkout.
- **Done:** Cypher outgoing/incoming typed MATCH now seeks the existing 10-byte
  node/type adjacency prefix through `visit_edges_of_type`, rather than visiting
  every relationship type. Streaming reads and eager matching before writes share
  the optimization. Storage format, scalar-index readiness and all-type detach
  conflict dependencies are unchanged; no migration is required.
- **Measured:** paired warm dev/debug runs on identical synthetic fixtures, five
  warmups and 30 samples/query. At 50,001 nodes / 1,000,020 relationships, outgoing
  count of ten WANTED neighbors changed from 367.388 ms to 0.112 ms p50; counting
  all 50,000 NOISE_0 neighbors changed from 683.062 ms to 316.302 ms. Incoming,
  absent-type and actual projection LIMIT queries also improved. The unchanged
  primary-key/count control remained about 0.040 ms. These are selective local
  measurements, not general throughput, release-build or cold-disk claims.
- **Acceptance:** Python driver 5.28.6, four reader tasks/100 queries and ten
  ten-node batches on a disposable copy of the million-relationship fixture.
  Snapshot isolation, stable MERGE overwrite, explicit rollback, DETACH DELETE,
  committed recovery after SIGKILL and disappearance of an uncommitted crash
  write passed. Final audit/counts: 50,100 nodes / 1,000,119 relationships.
  Warm Python/Bolt count p50 0.761 ms; RocksDB estimates captured separately.
- **Verification:** five new regressions cover directional/type/node boundaries,
  EdgeId ordering, overlays, snapshots, callback stop, invalid families, truncated
  selected keys, self-loops, residual predicates, eager writes, rollback, reopen
  and LIMIT. Workspace has 414 passing tests; Clippy warnings denied, fmt and
  diff checks. Advisory Astra found no production-code blockers; main checked
  actual tests and measurements.
- **Decision:** benchmark builds initially collided in a shared Cargo target
  directory. Disputed runs were discarded. Preserve the original executable,
  clean affected store/Cypher/server packages, rebuild the canonical code and
  measure immutable executable copies serially. Provenance and raw final samples
  are retained in `docs/benchmarks/2026-10-06-typed-adjacency/`; source and complete
  method are linked from `docs/benchmarks/2026-10-06-typed-adjacency.md`.
- **Profile:** a separate five-second macOS sample points to per-node RocksDB
  reads (`node_record`/`get_node`/`read_node`) as the dominant selected dense-query
  branch; empty-row decode itself is much smaller. Inclusive branch counts are
  saved separately and excluded from latency tables.
- **Next:** investigate bounded node-read batching/reuse in large returned
  neighborhoods and indexed counts; measure release builds and representative
  property payloads. Continue bounded Python batch import and failure acceptance
  before client integration. CSR projection/PageRank were not changed today.
- **Blockers:** none for this slice. Distributed replication, auth/TLS and
  DuckLake/Arrow incremental ingestion remain separate work. No push/publishing.

### 2026-10-05, connected ontology inspection over Bolt

- **Requested:** a 20-minute local session, starting 08:13:17 UTC
  (10:13:17 Amsterdam). No push, publication or remote PR.
- **Done:** `CALL db.schema()` returns one read-only transaction-consistent row
  with catalog version, node types/properties/index declarations, every relationship
  signature and scalar-index readiness. Empty catalogs return an empty schema row.
  Effective column/declaration order is preserved, including retained old columns;
  properties are names, not retained type declarations. No instance-data scans.
- **Connected TUI:** `shell --connect IP:PORT --graph NAME --view ontology` reads
  through Bolt without loading client database config or opening RocksDB. It reuses
  the green schema renderer, supports EN/PT-BR and manual `r` refresh, displays
  sample age/version and retains a clearly marked stale snapshot after failure.
  Successful refresh keeps selection by label where possible. Other panels remain
  offline; target graph is explicit and fixed for a session.
- **Bounds:** 4096 node types/signatures, 65536 aggregate entries, 1 MiB expanded
  schema strings; conservative allocation reservation before building output plus
  existing query guards/deadlines. Client validates exact shapes and complete
  results with a cumulative 4 MiB wire cap; GOODBYE/EOF rejects extra and delayed
  trailing records. Partial network reads/writes obey an absolute deadline.
  The shared health probe now uses that deadline and a cumulative 64 KiB wire cap.
- **Verification:** 409 workspace tests pass, including 14 added checks; clippy
  with warnings denied, fmt and diff checks pass. Two-graph/empty/missing database
  integration proves server-held RocksDB locks and invalid client-local settings
  do not affect connected reads. Fake-server tests cover malformed/over-limit,
  slow-drip and trailing responses. TUI checks include stale warnings at 40/60 columns.
  Astra reviewed the design and final changes; narrow-warning finding was fixed.
- **Real use:** Python driver 5.28.6 ran 40 schema reads concurrently with 10 write
  batches inserting 200 Customers; final count, connected snapshot and post-stop
  storage audit passed. Actual PTY captures exercise server stop, failed refresh,
  restart with schema v2 and refresh without restarting the TUI. Test servers and
  temporary databases are cleaned up. These checks are not performance benchmarks.
- **Artifacts:** `docs/connected-ontology.md`, README/server/backoffice updates,
  and `docs/design/assets/2026-10-05-connected/` with five captures, text and
  repeatable Python/PTY checks. No new runtime dependency or storage format.
- **Limits/next step:** refresh is synchronous (default 3-second network deadline,
  configurable 1..10000 ms), no background polling, no graph discovery, no arbitrary
  connected query editor or mutation. Catalog loading precedes procedure preflight;
  allocation/wire caps are not RSS bounds. Client additionally counts escaped text.
  Existing renderer scroll is capped at 65535 lines; huge property lists can require
  Cypher/Python inspection. Next: bounded instance-neighborhood reads and connected
  graph selection, then asynchronous refresh/pagination. Keep authentication/TLS and
  production readiness separate; current server remains for trusted testing.

### 2026-10-04, real ontology connections in the offline TUI

- **Requested:** a 15-minute local session, starting 09:18:41 UTC
  (11:18:41 Amsterdam). No push, publication or remote PR.
- **Done:** the real overview now has an Ontology panel backed by each graph's
  persisted RocksDB catalog. It shows type selection, directed connections,
  every source/target signature, incoming/outgoing relationships, properties
  and declared indexes. Isolated types, self-loops and empty ontologies have
  explicit representations. Schema controls are escaped before terminal output.
- **Interaction:** Up/Down selects types, Left/Right changes graphs, Tab cycles
  five panels, PgUp/PgDn scroll, and Enter/F1 opens the compact inspector. English
  is the default; Portuguese preserves canonical schema names. At 100 columns,
  connections and inspector fit side by side; compact terminals toggle them.
- **Performance:** starting with `--view ontology` skips full graph count scans
  and uses RocksDB estimates. Five warm debug-process runs on the local synthetic
  100K-node/1M-edge fixture had median startup 70.679 ms for the Ontology snapshot,
  912.528 ms for exact-count Graph and 83.524 ms for estimated Graph. This measures
  startup/render/exit with a small schema, not query throughput or cold-cache speed.
- **Verification:** 395 workspace tests pass (seven added); workspace clippy with
  warnings denied, fmt and diff checks pass. Coverage includes catalog projection,
  shared relationship signatures, dangling endpoints, navigation, clamped scrolling,
  Unicode/control characters, small/empty layouts and CLI process reopen. Actual
  PTY runs checked Down, Enter, PgDn, resize from 160x40 to 80x24 and clean exit.
  An additional isolated smoke loaded 2,000 types and 1,999 signatures and rendered
  its snapshot successfully; this was a single-run check, not a volume benchmark.
  Astra reviewed the final Rust and found no blockers. The main agent verified
  tests and inspected actual captures independently of agent reports.
- **Artifacts:** `docs/design/assets/2026-10-04-ontology/` contains actual terminal
  renders/text, a repeatable isolated capture helper and raw startup measurements.
  README and the multi-graph backoffice guide explain scope and commands. No new
  Rust dependency or storage format; pyte/Pillow are external capture tools only.
- **Limits/next step:** this is a read-only startup snapshot requiring exclusive
  offline access, not a live server connection or ontology editor. Connections
  represent allowed type signatures, not stored instance edges. Next: design a
  bounded read-only schema/neighborhood interface over the existing server so the
  TUI can inspect a running database without opening RocksDB itself. Keep ontology
  changes and data deletion out of that first connected slice. No blockers.

### 2026-10-03, explainable knowledge-source ranking

- **Requested:** add something inspired by OntoRank; explain PageRank with visual
  examples while continuing the feature. All changes remain local.
- **Done:** `CALL knowledge.rank` ranks an exact term and optional domain using
  explicit authority, author relevance, normalized usage, certification and
  freshness properties. Returns nodes, scores and five numeric contributions.
  Policy weights are documented; this is our heuristic, not Databricks' algorithm.
- **Performance/correctness:** reuse the existing streaming scalar term index
  with a label-scan fallback. A bounded heap retains only top K candidates;
  deterministic ties use node IDs. Per-candidate scopes release temporary
  accounting; memory, deadline and candidate guards fail without partial results.
  Snapshot reads, staged writes, atomic failure and index overwrites remain on
  the existing transaction path. No dependencies or storage format changes.
- **Verification:** 388 workspace tests pass, including five new integration
  tests for scoring/explanations, filter/index fallback, reopen, snapshot isolation,
  invalid evidence rollback, ties, heap replacement and resource limits. A 500-source
  top-1 query passes with a 32-KiB accounting budget. Workspace clippy with warnings
  denied, fmt and diff checks pass. The built CLI seeded an isolated temporary
  graph, reopened it in another process and returned official source 0.90 versus
  popular source 0.10. Temporary demo storage was removed.
- **Documentation:** `docs/knowledge-ranking.md` and `examples/knowledge/schema.json`
  provide a runnable example. Interactive PageRank explainer stays in the thread's
  visualization directory; three scenarios and iteration controls were checked.
- **Limits/next step:** source evidence is ingestion-supplied, with no automatic
  freshness decay, usage collection, certified-owner verification or ACL. Domain
  filtering is not authorization. PageRank and source ranking are separate. Next:
  validate this policy against real client questions and provenance requirements
  before adding automatic evidence collection or a combined ranking.

### 2026-10-03, second hour: streaming reads, shared reservations and deadlines

- **Requested:** another hour of local work, starting 08:54:26 UTC
  (10:54:26 Amsterdam). No push, publication or remote PR.
- **Duration:** wrap-up recorded at 09:55:38 UTC
  (11:55:38 Amsterdam), after 1h 1m 12s.
  The requested additional hour was completed.
- **Engine:** `410e9ae` adds row-at-a-time read-only MATCH/UNWIND, early LIMIT
  and ungrouped counts without retaining every input binding. Shared traversal
  preserves eager writes and grouped/CALL behavior. Scalar property access
  avoids building a complete node property map; promotion/extra precedence is
  covered. Full static validation remains before execution, including LIMIT 0.
- **Resource admission:** all server connections and named graphs share one
  `memory.query` pool. Each transaction holds its conservative high-water
  reservation through multiple RUNs until commit/rollback/failure/drop. Pool
  exhaustion is retryable; `CALL db.memory()` exposes process-wide counters.
  This is estimated accounting, not RSS or a hard allocator cap.
- **Execution deadlines:** `server.query_timeout_ms`, default 30,000 ms,
  config/env provenance and bounds, server/CLI enforcement. Deadline starts
  after parsing/catalog setup and is cooperative; idle transactions, native
  calls and commit are not preempted. A Python 200^4 fanout expires at a 1-second
  configured deadline, rolls back its prior CREATE and frees reservations.
- **Correctness:** fixed the Bolt worker so value conversion errors also discard
  earlier writes/reservations. Added regressions for pool contention, rollback,
  repeated RUNs and temporary scopes. Differential test `357522c` compares the
  streaming path with eager fallback for correlated/rebound/empty/null inputs.
- **Verification:** 383 Rust tests, full workspace clippy with warnings denied,
  fmt and diff checks pass. Neo4j Python drivers 5.28.6 and 6.3.0 each passed
  16-connection/two-graph 256-KiB pool tests, mixed read/indexed-overwrite traffic,
  four writers racing on one node (one commit, three retryable conflicts), and
  physical offline graph/index audits after closing the server. Test commits
  `733b38c`, `415e5e8`, `71373dc`. `d4edec6` also SIGKILLs a separate Python
  client holding an explicit transaction: both drivers released its reservation
  (65,671 to 312 observed bytes), discarded the indexed CREATE and passed offline
  audits while the server remained running. No new dependencies.
- **Scale:** final-code native Python test inserted/replayed 100K nodes in
  15.66/17.63 seconds, survived two forced exits and recovered 99,999 nodes
  after the intentional deletion. Driver 6 also passed the 10K workload.
  Separate embedded debug test passed at 1M nodes with a 64-KiB query guard:
  LIMIT 1 retained about 5.2 KiB of accounting; full label count p50 12.94 s;
  whole-process peak resident size 64.97 MiB. These are synthetic/shared-host
  samples, not release capacity or RSS guarantees. The report includes raw
  timings, sample counts and the 100K comparison.
- **TUI:** Memory/Pipeline explanations now cover streaming/eager boundaries,
  shared pool lifetime and cooperative timeout in EN/PT-BR. Admin JSON marks
  live server usage as unobserved offline. Thirteen actual PTY terminal renders
  show a 100K-node fixture at 160x40 and 80x24, including real SST inspection.
- **Linux/orchestrators:** validation in progress. Docker pins installed Rust
  1.98.1 despite the development stable toolchain and defaults C++ builds to two
  jobs. Kubernetes harness now keeps timeout diagnostics and bounded restart
  waits. A queued Docker build exposed stale workspace artifacts (old Cypher
  API reused against the new server); `ba683ad` cleans only release workspace
  artifacts under the locked target cache, preserving RocksDB/dependencies.
  The repaired Linux arm64 image `brainwire:20261003-session2-release` passed
  Docker retained-volume crash/recreate/index audits and Compose acceptance.
  Kubernetes 1.36.4 passed with 10K nodes: zero-timeout container stop and Pod
  recreation retained the same PVC/data/indexes; offline audits and checkpoint
  query verification passed. The earlier ready cluster timed out waiting for the
  image and was removed; the retry used the repaired image. All owned clusters,
  containers and test volumes were removed; only the final local image remains.
  The complete pool/deadline/concurrency/killed-client Python test also passed
  against that Linux release image through an isolated temporary Docker wrapper.
- **Artifacts:** [session report](docs/benchmarks/2026-10-03-streaming-pool-deadlines.md),
  [raw results](docs/benchmarks/2026-10-03-session2-results/),
  [TUI gallery](docs/design/assets/2026-10-03-session2/README.md).
- **October target:** Allan asked about an end-of-month product with Docker/K8s,
  direct Python individual/batch writes, independent graph creation, a docs site
  and a polished TUI. Assessment: a tested client pilot is plausible; full
  production readiness and distributed HA are not promised. Proposed milestones:
  retained-volume deployment/recovery by Oct 10, Python/query/resource hardening
  by Oct 17, connected TUI/docs by Oct 24, workload validation/pilot by Oct 31.
  See the [pilot proposal](docs/2026-10-pilot-roadmap.md) for acceptance gates.
  Allan also asked about November beyond MCP: proposed a conditional closed beta
  focused on incremental ingestion, query maturity, security/operations, connected
  TUI/docs and a first history/retention slice. MCP adapts the same APIs. Vector
  search versus distributed replication remains an explicit priority decision.
  For December, proposed a scoped self-hosted commercial release plus support,
  conditional on beta stability and recovery/upgrade/security acceptance. No
  production replication or automatic failover promise was made.
  Existing untracked `docs-site/` is a concept draft containing future syntax;
  preserve it and reconcile its advertised capabilities before publication.
- **Immediate client pilot:** Allan asked whether testing this week is viable.
  Proposed a separate Docker graph, a small ontology, Python individual/batch
  writes and representative queries/recovery checks. Audit client's actual
  Cypher before pointing it at Brainwire; automatic DuckLake ingestion and
  connected TUI remain pending. No existing client service was reconfigured.
- **Next:** bounded Bolt cursors with cancellation and live allocation leases;
  grouped/write streaming afterward. Idle transactions can still hold quota.
  Preserve single-writer retained-volume deployments until replication and
  fencing exist. Connected TUI, resumable index generations and DuckLake/Arrow
  incremental ingestion remain separate work; no auth/TLS or HA was added.

### 2026-10-03, allocation guards, numeric indexes, native checkpoints and real files

- **Requested:** one hour of substantial local work, starting 07:33:49 UTC
  (09:33:49 Amsterdam). No push, publication or remote PR.
- **Duration:** closed at 08:34:01 UTC
  (10:34:01 Amsterdam), after
  1h 0m 12s; the requested hour was completed.
- **Query limits:** the real Bolt worker and embedded CLI now use configured
  `memory.query` as a conservative per-RUN allocation guard. Query source,
  parameters, candidates, bindings, output/grouping, write-overlay growth and
  CSR estimates share accounting. Failures discard earlier staged RUNs and
  their indexes. A test first exposed separate source/parameter checks; fixed
  them to use the same guard. This is not RSS or a shared concurrent pool;
  individual decode/store/transport operations are not fully reserved.
- **Storage/indexes:** streamed label/index/adjacency visitors avoid eager copies
  of every record payload. Transaction overlays have an O(1) memory estimate;
  typed entity_id lookup now uses one primary seek. Finite numeric equality
  indexes preserve all i64 bits and equal integral-float keys, including -0.0.
  v1 stores scan until atomic v2 rebuild. Differential precision/overwrite/
  snapshot/reopen checks pass; a 50,001-entry migration exceeds the deletion+
  insertion cap and leaves the old catalog/marker/entries unchanged.
- **Algorithms:** shared CSR BFS now returns ordered minimum-hop node paths via
  `CALL graph.shortestPath`, with typed source/target, depth bounds, overlay
  visibility and deterministic ties. It stops at target discovery. Unweighted
  only; the complete bounded projection is still rebuilt for each call.
- **Checkpoints:** RocksDB's native checkpoint captures one committed graph,
  ontology, indexes and allocator state. CLI `db checkpoint NAME --destination`
  refuses existing/broken-symlink/nested destinations. Reopened copies preserve
  old values after source overwrites and allocate new identities correctly.
  This is a local snapshot, not off-machine backup or replicated durability.
- **TUI/CLI:** `db files` and the real offline Disk panel show actual files,
  sizes, live-SST CF/level/entries/tombstones and explanations. No contents are
  read or symlinks followed; output is bounded and metadata non-atomic.
  Left/Right selects graphs, Up/Down files, F1 explains on narrow terminals.
  Memory labels guard/accounting scope precisely. Green/cyan identity retained;
  English default, optional Portuguese. Four panels captured at 160x40, narrow
  80x24, and Portuguese, plus all four wide EN/PT-BR panels at 100,000
  nodes. The gallery records synthetic fixture provenance.
- **Verification:** 372 Rust tests passed, workspace fmt and denied-warning
  clippy passed. Final native Python/Bolt 10K acceptance passed with drivers
  5.28.6 and 6.3.0, including allocation failure/connection reuse and two
  interrupted-write recovery cycles. A 100K driver workload also passed;
  lists and embedding lists round-trip, without providing ANN/vector search.
- **Measured:** warm debug embedded 100K string/numeric WHERE index p50
  0.0639/0.0615 ms; equivalent scans 3.442/3.163 s. Python/Bolt 100K point
  p50 1.704 ms primary / 2.245 ms email; a 14,286-match numeric count costs
  582.590 ms. One 100K import/replay took 14.472/17.253 s; another loaded run
  varied to 17.978/17.739 s. On 100K nodes/1M edges, CSR vectors use 12.8 MB
  excluding build buffers/RSS; projection 2.440 s and direct PageRank 2.278 s.
  Shortest path through Cypher costs 2.519 s including full projection. These
  are local synthetic observations, not isolated production or SLA results.
- **Deployment:** Dockerfile now caches Cargo/RocksDB build artifacts and copies
  the executable outside the cache mount. A reusable isolated kind acceptance
  script exercises retained PVC, container/Pod replacement, numeric indexes,
  audit and native checkpoint via offline Jobs. First cluster was removed while
  waiting for the image; pinned 1.36.4 bootstrap timed out at 600 seconds before
  Brainwire deployment and was removed. Cold Linux RocksDB compilation
  had not completed after more than 36 minutes; the final-source build waited
  on its cache. Both owned builds were interrupted; current-image Docker/Compose
  runtime acceptance never started. No live Kubernetes recovery or HA claim
  is supported by this session.
- **Saved:** code in local commit `ae188f9`; current reports/raw JSON under
  `docs/benchmarks/2026-10-03-*`, captures under
  `docs/design/assets/2026-10-03-session/`, updated MVP handoff and user guides.
- **Next:** first complete current-image Docker/Compose and isolated Kubernetes
  runtime acceptance. Then streaming MATCH/UNWIND/RETURN with live reservations
  and a shared query pool, concurrent load/RSS measurements, resumable index
  generations and connected TUI reads. DuckLake/Arrow incremental ingestion and
  temporal retention remain recorded separate tasks; no connector added.
- **Blockers:** cold Linux build and test-cluster bootstrap exceeded available
  runtime progress on this shared local environment. Native implementation and
  driver checks have no blocker. Replication, authentication/TLS, vector search,
  historical node revisions and maintenance execution remain future work.

### 2026-10-02, WHERE index seeks, CSR reachability and retained-volume deployments

- **Requested:** a 20-minute local session starting 06:33:18 UTC (08:33:18
  Amsterdam), covering Cypher, storage, TUI, indexes, algorithms, performance,
  Kubernetes and Docker. No push or image publication.
- **Duration:** closed at 06:53:29 UTC, after at least 20 minutes.
- **Cypher/indexes:** safe literal/parameter equality conjuncts in WHERE now use
  existing primary/string/Boolean indexes, with full residual filtering. Tests
  first reproduced unnecessary scanning and cover reversed equality, OR/NOT,
  NULL/numeric fallbacks, contradictions, hops, bound variables, staged writes,
  snapshot isolation and index audit. `CALL db.indexes()` reports the caller's
  snapshot readiness without constructing a graph projection.
- **Algorithms:** directed unweighted CSR BFS and `graph.reachable` return
  minimum hop depths with source label/entity_id, relationship filter and depth
  bound. Snapshot/overlay tests and randomized independent shortest-hop checks
  passed. Projection remains rebuilt per call, within existing graph-size guards.
- **Storage/deployment:** added Compose, single-writer retained-PVC Kubernetes
  and storage-node-pinned Swarm templates, documented recovery/fencing limits.
  Current release Docker image passed six kills with pending indexed overwrites,
  six recoveries and final offline audit. The actual Compose template passed
  ontology setup, kill/recreation, indexed reads and audit. Native Python/Bolt
  ingestion/replay and two kill cycles also passed. Owned test resources removed.
- **TUI:** English/Portuguese pipeline explanations now expose WHERE index
  behavior and the index/CSR procedures. Captured Graph, Pipeline, Memory and
  Disk panels from a real 10,000-node synthetic store under
  `docs/design/assets/2026-10-02-session/`; panels remain offline/read-only.
- **Measurements:** warm embedded development queries on 100,000 nodes after
  Docker compilation: indexed WHERE p50 0.0598 ms, equivalent unindexed field
  p50 1,941.594 ms. Timing excludes opening/network and is not an SLA. CSR
  workload covered 10,000 nodes/100,000 edges; full methodology and raw JSON
  are in `docs/benchmarks/2026-10-02-index-analytics-recovery.md`.
- **Verification:** 353 Rust tests passed; workspace fmt and clippy with denied
  warnings passed. Compose/Swarm parsing passed; Kubernetes render passed strict
  1.36 schema validation (2 valid resources). No live Kubernetes context or
  Swarm deployment was available; those runtime tests remain pending.
- **Next:** enforce query-memory bounds and benchmark end-to-end mixed workloads;
  execute retained-volume recovery in a real Kubernetes test cluster before
  promising failover. Replication, resumable large index builds and connected
  TUI execution remain future work. DuckLake/Arrow requirements from October 1
  remain recorded; no connector implementation was added in this session.
- **Blockers:** live cluster recovery needs a configured test cluster and durable
  volume driver. No local implementation blocker.

### 2026-10-01, DuckLake-first Arrow ingestion direction

- **Requested:** automate fast client ingestion, primarily DuckLake, also
  DuckDB, Parquet and Delta; assess Arrow in Rust.
- **Checked:** official DuckLake change-feed/time-travel, Delta CDF, DuckDB Arrow
  output and Rust Arrow/Flight documentation; local code has Python/Bolt but no
  Arrow/Flight implementation. Existing approved spec already proposes Flight.
- **Recorded:** priority in README and a concrete integration contract in
  `docs/python-ingestion.md`: bounded columnar batches, shared transactional
  writer, source-specific change detection, durable checkpoints, ordered replay,
  source-history retention, mapping ownership and TUI ingestion metrics.
- **Next:** implement and benchmark one DuckLake snapshot-to-Arrow-to-Rust path
  against the existing Bolt baseline, then add incremental recovery. No throughput
  gain is measured yet, no connector is installed and no recurring job was created.
- **Verification:** documentation-only diff reviewed; all changes kept local.


### 2026-10-01, Python ingestion, index consistency and real TUI storage panels

- **Requested:** 20-minute integration session, starting 08:46:59 UTC, covering
  Python, Cypher, MERGE/SET, indexes, RocksDB/memtables and TUI visibility. Allan
  also requested captures of every TUI panel. All work stayed local.
- **Correctness:** reproduced a concurrent index-backfill bug with a failing test:
  a node could commit after the migration snapshot yet escape its new index.
  Backfill now streams node/index prefixes with conflict dependencies; it aborts
  atomically when the writer wins. Existing Cypher catalog dependencies cover
  the opposite commit order. Added mixed-type overwrite differential tests
  (indexed versus scan paths), including embedded NUL and Unicode strings.
- **Audit:** extended graph invariants to catch missing/stale active scalar index
  entries and exposed `db check NAME [--json]`. Legacy unready indexes are
  explicitly skipped; retired prefixes are not audited. Full scans remain
  expensive and are not a repair mechanism.
- **Python:** added `examples/python/ingest_customers.py`, schema and JSONL fixture,
  plus a self-contained Bolt acceptance harness. Tests cover duplicate CREATE
  rollback within a batch, bounded MERGE replay, SET overwrite, null removal,
  delete, rollback, graph isolation, lists/vectors, and two SIGKILL cycles.
  The extended case kills an open uncommitted indexed overwrite and proves it
  disappears while acknowledged rows/indices survive. The importer itself was
  run twice and survived restart without duplicate nodes.
- **Measured:** 50K input rows took 5.880 s to ingest / 5.215 s to replay in
  250-row batches; 100K took 12.064 / 13.943 s in 500-row batches. The extended
  100K crash run took 10.838 / 14.079 s, recovered 99,999 rows after deliberate
  deletion, passed `db check`, and had 21/12 SST files before its two kills.
  Individual development-build observations, not an SLA; see
  `docs/benchmarks/2026-10-01-python-ingestion.md`.
- **TUI:** offline overview now cycles through Graph, Write pipeline, Memory and
  Disk panels with Tab. Shows persisted index readiness/fields, shared budgets,
  cache/write-buffer accounting and CF estimates/memtable bytes/L0 file counts.
  Memory is labeled as the offline process, not running-server RSS; query budget
  remains explicitly unenforced. PgUp/PgDn reaches long explanations. Both
  English and Portuguese are supported. Real 120×30 ANSI captures and text are
  in `docs/design/assets/2026-10-01-ingestion/`; inspected all four PNGs.
- **Verification:** 345 workspace tests passed; clippy all targets with warnings
  denied passed; formatting passed. Driver 5.28.6 accepted 5K/50K/100K workloads.
  Interactive 80×24 PTY navigation restored the terminal on exit. Test servers
  and capture processes were stopped; temporary databases were removed.
- **Limits / next:** connect read-only observability to the running server;
  bounded query results and enforced execution budgets; resumable index builds;
  planner diagnostics and WHERE index selection. Vector storage is not ANN search,
  property SET is not whole-map replacement, and local crash recovery is not
  replicated failover. The selected rich graph/file-browser composition still
  belongs to the demo/design until those connected backends exist.


### 2026-10-01, first physical scalar property index

- **Requested:** 15 minutes on an index feature. Started at 08:27 UTC. All
  work remained local; no push, remote deployment or PR.
- **Implemented:** ontology-declared `indexed_fields`/`er_fields` now create
  type-tagged string and Boolean equality keys in the RocksDB `idx` family.
  Node create/MERGE/SET and DETACH DELETE update keys in the same transaction
  as the row. `MATCH` chooses a ready index before a label scan, with remaining
  predicates checked on the row. `entity_id` still uses `pk` first.
- **Migration:** ontology application atomically backfills a new declaration
  and persists a ready marker. Legacy catalogs without the marker scan safely
  until reapplied. Removing and re-adding a declaration clears stale keys
  before rebuilding. A migration above 100,000 old/new keys is rejected
  without changing the catalog; a resumable batched migration remains open.
- **Correctness:** new tests cover duplicate values, updates, NULL removal,
  delete, reopen, added declaration, old unmarked catalog, removed/re-added
  declaration, Boolean/string type separation, numeric fallback, rollback and
  concurrent writer conflict. Admin schema now reports
  `string_boolean_ready` or `declared_only` as appropriate. A malformed index
  key fails closed. All 341 Rust
  workspace tests passed, as did denied-warning clippy, formatting and diff
  checks.
- **Measured:** on a temporary 10K-Customer graph, five separate local CLI
  processes per lookup had median 90.14 ms for indexed email and 244.98 ms
  for unindexed name. The [report](docs/benchmarks/2026-10-01-scalar-index.md)
  explains that process-open time dominates and this is not a production
  benchmark. The temporary database was removed.
- **Next:** add resumable indexed backfill and index statistics/EXPLAIN, then
  canonical numeric equality, range and vector indexes. Verify cold/warm Bolt
  latency and write amplification at larger heterogeneous volumes. Streaming
  query results and memory ceilings remain separate priorities.
- **Blockers:** no local blocker; the large-graph migration design is not yet
  implemented.

### 2026-10-01, real multi-graph backoffice overview

- **Requested:** about 15 more minutes on useful MVP work, with current data,
  execution instructions, UI state and TUI operations. Allan emphasized that
  Brainwire must manage multiple graphs, not a single graph. Started at 07:46
  UTC. All work remained local; no push, deployment or PR.
- **Done:** added `brainwire db overview [--json]`, which summarizes every
  named graph with exact node/edge counts, ontology version, label and edge
  type names, index declaration count and timeline stream count. Added
  `brainwire shell --overview` as a read-only TUI over real local RocksDB data,
  with Up/Down selection, a selected-graph inspector, `--graph` initial
  selection, `--snapshot`, English and pt-BR. It remains offline because the
  local process needs exclusive RocksDB access; it is separate from the four
  synthetic Graph/Query/Memory/Disk demo views.
- **Measured:** local `db overview --json` showed `analytics-demo` 3 nodes/2
  edges, `staging-100k` 100,000 nodes/1,000,000 edges, and
  `client_memory` empty. The 100K fixture's directory was 9.5 MiB on this
  machine because its data are synthetic and highly compressible. An actual
  pt-BR TUI snapshot selected `staging-100k` and showed `Page`/`LINK` from its
  ontology. The small PageRank graph yielded Storage 0.4069, Recovery 0.3333
  and Introduction 0.2597 through the real Cypher executor.
- **Validated:** tests cover two independent named graphs, their ontology and
  count isolation, snapshot selection and a missing graph. All 335 Rust
  workspace tests passed; clippy with denied warnings, formatting and diff
  checks passed. A
  quick `--estimated` mode now reads RocksDB key-count estimates instead of
  scanning; a single warm local measurement was 0.078 s versus 1.053 s for
  exact counts on the retained graphs. These are not production benchmarks.
- **Next:** use persisted exact counters or an explicit cache before assuming
  the exact overview scales to billion-element graphs. Connect bounded
  graph/query exploration to the server, then
  live telemetry and storage files. Physical indexes and temporal history
  remain higher-priority engine work.
- **Blockers:** no local blocker. Real overview requires the server to be
  stopped; live server-side backoffice access needs an API.

### 2026-10-01, recovery probe and query/ontology audit

- **Requested:** continue the local feature/test session for about 20 minutes,
  verify RocksDB's role in queries and ontology, then account for temporal
  history, different node/edge weights and graph maintenance. Started around
  07:20 UTC. No push, remote deployment or PR.
- **Recovery health:** added `brainwire probe` with Bolt session (`live`) and
  read-only query (`ready`) modes, bounded network operations and response
  parsing, plus a Docker health check. Integration tests cover down/up/down
  server states. The local ARM64 image became healthy and a one-cycle
  container kill/recreate test passed with a retained volume.
- **Admin visibility:** `brainwire db schema NAME --json` exposes the persisted
  ontology catalog and explicitly reports `declared_only` secondary indexes.
  `brainwire db metrics NAME --json` reports per-family RocksDB estimates.
  The retained 100K-node/1M-edge fixture shows `idx` and `ident` at zero keys.
- **Query finding:** `entity_id` uses the `pk` prefix; edge traversal uses
  `out`/`in` prefixes. Other property predicates scan their label; index
  declarations do not yet create keys or change plans. On a temporary warm
  50K-Customer fixture, median Bolt lookup was 3.31 ms by `entity_id` and
  514.22 ms by declared-index `email`. These are local synthetic figures.
  The audit and a concrete physical-index gate are in `docs/benchmarks/` and
  `docs/design/`.
- **Temporal/maintenance decision:** catalog versions are schema versions, not
  entity revisions. Existing edge properties can drive weighted CSR/PageRank,
  but node weighting, historical/as-of reads, revision retention and cleanup
  jobs are not implemented. The temporal design separates recorded and valid
  time, keeps current Ticket rows independent of expiring old revisions, and
  requires dry-run, auditable, restartable cleanup after ontology deprecation.
  Applying an ontology must never implicitly delete graph data.
  `db schema --json` now labels temporal history and maintenance as
  `not_implemented`, and the text view lists declared timeline streams. An
  integration test verifies that applying an ontology omitting a former label
  retains its catalog type and stored node.
- **Validated:** full Rust workspace tests, warning-free clippy and formatting
  passed after the final probe fix. Docker health and restart behavior were
  checked locally. No claim of 5B-element/16-GiB query performance or
  multi-machine recovery is established.
- **Next:** implement atomic physical scalar secondary indexes plus backfill
  and query-plan selection. Then add bounded streaming and the first durable
  entity-revision/history slice. Follow with maintenance dry-run and resumable
  retention jobs; validate under larger, less compressible graphs.
- **Blockers:** none for local implementation. Production scale and cluster
  recovery remain unproven.

### 2026-10-01, durable Bolt server and orchestrator goals

- **Requested:** continue the longer local Brainwire session, keep all work
  local, and make distributed deployment on both Kubernetes and Docker Swarm
  explicit objectives. Started at 06:41 UTC. No push, remote deployment or PR.
- **Server:** connected the named database registry and Cypher executor to
  `brainwire serve`. A bounded per-connection worker owns the scoped RocksDB
  transaction across multiple Bolt RUNs, while request execution stays off
  Tokio IO threads. The listener caps concurrent connections at 64. PackStream
  parameters and results include nested values and Bolt node structures;
  storage conflicts return retryable errors. No restart-unstable bookmark is
  fabricated.
- **Validated:** engine tests cover parameter conversion, node output, error
  mapping, transaction visibility, commit/rollback, query failure, RESET,
  session drop and concurrent commit conflict. The Python driver acceptance
  script wrote two Customers, one Company, a relationship and 10,000 bulk
  Customers. Both Neo4j Python drivers 5.28.6 and 6.3.0 separately passed
  write, SIGKILL, restart and read checks. The Rust workspace had 328 passing
  tests, and clippy with warnings denied, formatting and diff checks passed.
- **Container:** built a local Linux ARM64 `brainwire:local` image with a
  non-root runtime and explicit `/data` mount. A first smoke attempt exposed a
  mismatched bundled example ontology; corrected it and rebuilt the cached
  image. The final test created a named Docker volume, wrote 10,002 Customers
  and one Company through Bolt, killed the container, started a new container
  with the same volume and read everything back. The temporary test container
  and volume were removed; the local image remains. A saved Docker acceptance
  script then passed five kill/recreate cycles with driver 6.3.0 and two cycles
  with driver 5.28.6. No image was pushed.
- **Concurrency:** 16 local driver clients made 1,600 independent autocommit
  Customer writes. The reopened store contained exactly 1,600 nodes. The
  synthetic warm-cache run took 0.306 seconds here; it is not a production
  throughput result.
- **Deployment goal:** the data/recovery design now specifies a single durable
  node and then a three-voter replicated cluster on **both** Kubernetes and
  Docker Swarm. Each target must keep member identity tied to durable storage
  and pass process, member and machine failure tests. Orchestration alone is
  not replication or proof of high availability. Sharding is a later separate
  milestone.
- **Limits:** this is a single-node local server. Authentication, TLS, query
  memory enforcement, streaming results, live TUI data and cluster replication
  remain open. The default listener is loopback; deployments need explicit
  network controls. The 10K-driver workload is synthetic, not a performance
  or multi-machine availability claim.
- **Next:** add recovery-aware health/readiness behavior, then local Kubernetes
  and Docker Swarm single-node manifests and restart tests. Expand
  failure/concurrency tests, query budgets and streaming before calling it
  production-ready. Design and test three-voter replication separately.
- **Blockers:** none for local single-node work. The remaining distributed
  milestones require implementation and acceptance tests.

### 2026-10-01, named database administration and retained volume dataset

- **Requested:** a 15-minute local work session toward next week's test
  deployment. Started at 06:03 UTC. No push, publish, remote deployment or PR.
- **Done:** added a named database registry with one shared RocksDB resource
  budget, a default database, existing-database discovery and `get/create/names`.
  Names cannot contain paths; symlinked or unrelated directories are refused,
  and a corrupt named database stops startup rather than disappearing from the
  registry. Added `brainwire db create`, `db list`, `db stats [--json]` and
  `db apply-ontology` for offline administration.
- **Volume:** extended the existing synthetic workload example with an optional
  fresh `--database` path that refuses overwrites. Generated an ignored local
  `data/staging-100k` store with 100,000 Page nodes and 1,000,000 LINK edges;
  `db stats` confirmed both counts after reopen. A direct Cypher read from
  `synthetic-99999` returned its ten neighbors. The generated store occupies
  about 8.6 MiB on this host because the data is highly compressible; this is
  not representative of a production document graph.
- **Verified:** workspace tests, workspace clippy with warnings denied,
  formatting and diff checks passed. Process-level tests cover create/list,
  ontology, Cypher write/reopen/read, stats text/JSON, invalid names and invalid
  ontology. Registry tests cover reopening, symlinks, unrelated directories
  and corrupt database refusal. The retained example also refused to overwrite
  its existing destination.
- **Next:** implement the production Bolt adapter with scoped transaction
  ownership and bounded blocking execution, then `brainwire serve` and driver
  write/kill/restart tests. `db` administration is offline and needs exclusive
  access. Add container packaging only after that end-to-end server path works.
  The local client Python environment has Neo4j driver 6.3.0; the historical
  temporary 5.x environment exists but no longer imports `neo4j`, so refresh
  that test environment when the server is ready.
- **Blockers:** none for local work. This is not yet a networked deployment;
  the Bolt production worker, value conversion and serve command remain open.

### 2026-09-30, single-node latency and commercialization review

- **Done:** extended the reproducible sparse-analytics example with 100 warm-up
  and 1,000 timed embedded Cypher reads per query, using varied `entity_id`
  values. Measured point lookup and ten-neighbor expansion on both the 10K/100K
  and 100K/1M synthetic graphs. The 1M-edge debug run measured 0.043 ms p50 /
  0.129 ms p95 for a point read, and 0.255 ms p50 / 0.973 ms p95 for ten
  neighbors. These are single warm-cache local runs without Bolt, concurrency
  or production workload guarantees; [report](docs/benchmarks/2026-09-30-sparse-analytics.md).
- **Verified:** both example sizes ran successfully, workspace tests passed,
  workspace clippy passed with warnings denied, formatting check passed.
- **Commercial direction discussed:** client can be the first design partner
  while Brainwire remains a separate product. Recommend a narrow paid pilot
  before a production subscription, with explicit success criteria and a
  written ownership/commercial-use arrangement. Repository is currently
  `UNLICENSED` and `publish = false`; no public license or pricing was chosen.
- **Deferred at Allan's request:** a live TUI view of the 100K-node graph.
  The current TUI still uses its 36-node synthetic fixture; no live-preview
  code was retained. A bounded real sample plus true database counts would be
  the right first display for a larger graph.
- **Next:** production Bolt worker/adapter and durable `brainwire serve`, then
  driver-based restart tests. Benchmark point reads through Bolt under
  concurrent, cold-cache and high-degree workloads; add secondary property
  indexes. Clarify IP ownership and licensing before external distribution.
- **Blockers:** none for local engineering. Product license and first external
  offer need an explicit business decision.

### 2026-09-30, Cypher sparse analytics and volume validation

- **Requested:** a substantial session across multiple tasks, minimum ten
  minutes, followed by an accurate engine/CLI/TUI status report. Work started
  at 08:47:36 UTC; no remote actions. Completed the previously deferred Cypher
  integration, numerical fixes, transaction/recovery tests and volume workload.
- **Engine:** corrected CSR zero retention, duplicate-weight overflow,
  impossible-dimension panic and nonfinite matvec handling. PageRank now uses
  f64 accumulation, precomputed row totals, reusable buffers and convergence
  diagnostics. Randomized tests compare it with an independent dense reference.
- **Storage:** added a visitor merging snapshot keys with transaction overlay
  updates/deletes without materializing all property values. Projections and
  stats use this path. Projections support relationship filters, catalog/extra
  weight properties, graph degrees and cardinality limits. Analytics errors
  stop scans; projections remain ephemeral and use the caller's transaction.
- **Cypher:** implemented `CALL graph.project`, `graph.degree`, `graph.pagerank`
  with options/parameters, explicit `YIELD`, aliases, filtering and final
  `RETURN`/`LIMIT`. Procedures cannot be mixed with other clauses beyond RETURN
  in the same query; earlier runs in a query transaction remain visible.
  Unknown options/fields, invalid weights and exceeded limits fail explicitly.
- **Tests:** 314 workspace tests passed, including CLI process/reopen output,
  transaction privacy, rollback/error poisoning, external snapshot isolation,
  node deletion, numeric edge cases and projection rebuilding/ranking after
  five SIGKILL cycles. Workspace clippy (`-D warnings`), formatting and diff
  checks passed. Reviewed the new parser/executor/visitor paths locally.
- **Volume:** imported/reopened 10K nodes/100K relationships and 100K nodes/1M
  relationships with the normal durable graph API. The million-edge debug run
  measured 1.20 s CSR construction, 1.13 s PageRank, 3.35 s Cypher top-five,
  12,800,008 CSR bytes and 160,923,648 bytes whole-process peak RSS. Single local
  measurements, not production claims; [report](docs/benchmarks/2026-09-30-sparse-analytics.md)
  and [reproducible example](crates/cypher/examples/sparse_analytics.rs) retained.
- **Usage:** added [analytics guide](docs/sparse-analytics.md) and synthetic
  ontology in `examples/analytics/schema.json`; created an ignored local demo
  database at `data/analytics-demo` and verified the documented CLI queries.
  TUI Graph/Memory snapshots verified in English/Portuguese. TUI data and file
  telemetry remain simulated; no live attachment or production serve yet.
- **Boundaries:** graph/vector search indexes, named persisted projections,
  hard query-memory enforcement and distributed fault tolerance remain pending.
  Cardinality limits do not impose a byte-based memory ceiling. Earlier
  untracked `docs-site/` and `.DS_Store` files were preserved.
- **Next:** production Bolt worker/adapter + database registry + durable
  `brainwire serve`, with driver-driven write/kill/restart acceptance, then
  connect TUI queries and observability to that backend. Improve byte budgets
  and cancellation before increasing analytics workloads beyond this range.
- **Blockers:** none for this delivery. All changes and demo data stay local.

### 2026-09-30, sparse graph projection session

- **Done:** added `brainwire_core::sparse::SparseMatrix`, a compact CSR matrix
  builder with duplicate-edge accumulation, finite-value validation, shape
  inspection and sparse matrix-vector multiplication. Added
  `Database::sparse_projection`, which maps persisted NodeIds and optional edge
  types into a rebuildable CSR adjacency view, plus weighted PageRank over the
  projection. Added focused core and store tests for graph-like weighted edges,
  filters and invalid inputs. The persistent RocksDB graph remains the source
  of truth.
- **Verified:** `cargo test --workspace --locked --offline` (all workspace
  tests passed), `cargo clippy --workspace --all-targets --locked --offline --
  -D warnings`, `cargo fmt --all -- --check`, and `git diff --check`.
- **Decided:** use sparse matrices for analytics/search projections rather than
  replacing transactional `out`/`in` adjacency storage.
- **Next step:** expose the projection through a read-only Cypher procedure,
  then benchmark rebuild cost on 100K and 1M synthetic edges.
- **Blockers:** none.

### 2026-09-27, product direction: rich data, vectors and recovery

- **Requested:** understand a distributed deployment across containers/machines
  that recovers after failures, accept rich data including lists, and search
  documents by vectors in the style of Qdrant.
  Allan explicitly reinforced that the system must resist failures; recorded
  fault tolerance as a mandatory acceptance gate, including vector data.
- **Recorded:** [data and recovery addendum](docs/design/2026-09-27-brainwire-data-and-recovery.md),
  linked from the main spec. Requirements are distinguished from proposed
  implementation choices and current capabilities. No engine code changed.
- **Current evidence:** the value model already supports scalar values, recursive
  lists and maps. Dedicated vector types/indexes/search and production Raft are
  absent. The existing sync-write/SIGKILL checks prove process-crash recovery,
  not a full distributed recovery guarantee.
- **Direction:** durable authoritative graph/document/vector mutations; rebuildable
  indexes with explicit visibility; three replicas on separate hosts with retained
  volumes, quorum commits, safe restart and external backup. Whole-graph sharding
  remains a distinct capability. No external Qdrant dependency or API parity was
  selected. Original large document files need their own durable blob/backup path.
- **Next:** retain real server and bounded-memory work as prerequisites; prepare
  a focused vector slice with exact-search reference, filtering, update/delete,
  recall and restart tests. The month estimate must account for the added scope.
- **Verification:** read the current Value/codec/transaction/crash-test code and
  primary Qdrant, Raft, Kubernetes and RocksDB documentation. Documentation-only
  change; validate local links and diff whitespace. All changes stay local.

### 2026-09-27, 30-minute session: transaction foundation

- **Requested:** work on the graph for 30 minutes and identify the next task.
  Started 06:17:34 UTC; requested end 06:47:34 UTC. All work remains local.
- **Scope:** first two slices of the transaction preflight: shared-query
  execution context, then real Bolt lifecycle hooks and cross-layer tests.
  Production runtime/registry/serve integration remains next.
- **Plan:** [transaction foundation](docs/superpowers/plans/2026-09-27-brainwire-transactions.md).
  All three task reviews and final integration review passed. The final review's
  one minor stale-status paragraph was corrected in `10ba45a` and its scoped
  re-review passed. No findings remain open. Verification closed at 06:43 UTC,
  within the requested session window.
- **Embedded transaction:** `34dc1b8` adds `QueryTransaction` with a shared
  snapshot/catalog and staged changes across queries. A later query error
  discards all earlier writes; commit/rollback/terminal states are explicit.
  The existing one-query CLI remains compatible. Task review is clean; its
  focused suite and a rebuilt CLI's 1,000-row write/reopen/rollback checks pass.
- **Bolt lifecycle:** `6b6020a` connects BEGIN, RUN, COMMIT, ROLLBACK, RESET and
  teardown to mandatory engine hooks. Final implicit consumption commits;
  partial PULL/DISCARD leaves the transaction open. Errors discard pending work,
  failed cleanup closes the session, and fatal socket paths abort before waiting
  to send a failure. Database selection stays bound to its transaction. Task
  review is clean; 62 focused tests pass, including 5 real-store integration
  cases and deterministic transport cleanup checks. The real-store adapter is
  deliberately test-only; `brainwire-bolt-dev` still uses the development stub.
- **Language decision:** English code, schema identifiers, docs and default UI;
  Portuguese remains available through `--lang pt-BR`. Localization changes
  display text, never stored identifiers or user data. This agreement is recorded
  in AGENTS, the design, the spec and the demo guide.
  `fd3771e` sets the shell default; real binary snapshots confirm English by
  default, Portuguese with the flag, and rejection of an invalid locale. Updated
  Graph/Memory/Disk captures show the running English demo at 160×48. The exact
  user-selected reference image remains unchanged (SHA256
  `a41830e261f00f05ef247c107f4442db0cf3819e1c7f678103e649866a144237`).
- **Bookmark decision:** the session must not fabricate a commit counter that
  looks durable. The storage sequence is process-local; engines may provide a
  bookmark only when they can honor it. Causal/restart bookmark support remains
  pending the production adapter.
- **Handoff:** the [transaction guide](docs/transactions.md) documents the public
  boundary and the remaining production integration.
- **Verification:** 291 workspace tests passed; all-target clippy with warnings
  denied, formatting, locked offline build and diff whitespace checks passed.
  No test server remains running; the branch stays local without a remote.
- **Next step:** production ownership/runtime for the borrowed transaction,
  then registry and PackStream adapter plus `brainwire serve`. Verify real driver
  multi-query commit/rollback, disconnect/reset cleanup and restart persistence
  before connecting the TUI. The full MVP Task 5 remains open.
- **Blockers:** none for this slice. The branch and canonical checkout remain
  available locally for the next session; no push, merge or publication occurred.

### 2026-09-26, continued session until 21:30: executor and backoffice

- **Requested:** improve both database and CLI; Allan explicitly extended work
  until 21:30 Europe/Amsterdam (19:30 UTC). All work remains local.
- **Engine complete:** `5ace18d` adds the atomic Cypher executor;
  `51aca5a` fixes signed integer minimum and nonfinite float literal boundaries.
  MATCH, CREATE, MERGE, SET, DETACH DELETE, UNWIND, RETURN, grouping/count and
  LIMIT operate on one storage snapshot/overlay and commit once after success.
  The catalog shares that snapshot; late execution errors discard graph writes.
  No store API changes were needed. The executor is split into validation,
  evaluation, graph access and orchestration modules.
- **Engine verification:** 61 focused tests pass (45 lexer/parser, 16 executor),
  including 1,000-row UNWIND/reopen, duplicate and late-error rollback, directed
  hops, self-loops, grouping/null logic, exact integer/float comparisons and
  deterministic conflict mapping. Focused clippy/fmt pass. Task review of
  `5e7b86c..51aca5a` is clean for spec and quality.
- **CLI delivered:** `f6d94fb` adds embedded `brainwire cypher` with ontology,
  parameters, configuration and explicit node/scalar JSON output. Parameter
  integers outside i64 and nonfinite stored floats fail explicitly. Process
  tests cover exclusive locks and reopen/rollback; ontology applies separately
  before the query, and output errors can follow already-committed writes.
  The [local query guide](docs/local-cypher.md) documents those boundaries.
- **Backoffice delivered:** the same commit adds clustered Braille graph edges,
  a proportioned mini-preview, file tree/table, LSM stage panels and selected-file
  explanation, plus memory cards, inclusion diagram and usage bar. Inspector
  focus now scrolls independently, both query examples wrap at 80 columns and
  longer text has a viewport hint. `7571721` preserves EDITING in that hint.
  The two polish follow-ups from the preceding demo session are addressed.
  Actual 160×48 captures are in the [terminal guide](docs/tui-demo.md); the saved
  user image remains the visual target. The
  [executor/backoffice plan](docs/superpowers/plans/2026-09-26-brainwire-executor-backoffice.md)
  records this implementation slice.
- **Consolidated verification:** **265 workspace tests** pass after the
  parameter-file addition, plus all-target
  clippy with denied warnings, formatting and binary build. Independent command
  QA verified 1,000 persistent records across processes, Unicode and parameters,
  node JSON, directed hops, duplicate/late-error rollback, numeric boundaries,
  null/SET/delete and missing parameters. Actual PTY checks covered all layouts
  at 160×48, 120×36 and 80×24, second-query wrapping, result 12, a bounded long
  paste, shortcut typing, inspector scrolling/help, resizing, normal exit and
  exact termios restoration. Task 2 review of `51aca5a..7571721` passes spec
  and quality with no findings. Final integration review of
  `5e7b86c..c4a7462` also passes with no findings; the parameter-file addendum
  below has its own review gate.
- **Decisions/limits:** use the existing standalone branch; reject unsupported
  relationship bindings/property filters explicitly; expose real execution
  through a local command while keeping the TUI's fixture/metrics/files labeled
  synthetic. Queries still materialize rows; grouping searches first-seen groups
  linearly. Streaming, a query memory ceiling, full Cypher and authorization
  are not implemented. Per-call atomic execution must not be mistaken for
  multi-query Bolt transactions. The original backoffice image remains the
  primary composition reference.
- **Bulk-input addendum:** a real launch attempt with a 1,708,900-byte JSON
  argument failed before the binary ran (this Mac's ARG_MAX is 1,048,576).
  `664d428` adds `--params-file` using the same strict converter, mutually
  exclusive with explicit `--params`. Independent verification imported all
  10,000 synthetic records from that file and confirmed count and property
  lookup in separate reopened processes. File input remains materialized.
  The final 265-test workspace suite, all-target clippy and fmt pass.
  Task 3 review passes spec and quality with no findings. Final integration
  amendment review of `c4a7462..186cf28` passes; the combined change through
  `186cf28` is approved with no open findings.
- **Next:** real Bolt transaction lifecycle and durable `serve` (engine MVP
  Task 5), then connected TUI query/neighborhood and telemetry/file contracts.
  The [transaction preflight](docs/superpowers/plans/2026-09-26-brainwire-bolt-transaction-preflight.md)
  identifies the borrowed Txn lifetime, async runtime boundary, transaction
  failure states and mandatory reset/disconnect cleanup tests.
- **Autonomous decisions and costs:** reused the clean standalone branch
  (a different isolation choice would require moving local commits); explicitly
  rejected relationship bindings/property filters pending an edge read API
  (some parsed queries remain unavailable); exposed real execution separately
  from the synthetic TUI (network-connected UI comes later); corrected the
  signed-minimum parser boundary now (small parser/review scope and UI delay);
  added file parameters after the measured OS argument limit (one additional
  CLI option and its tests/review). These are the complete session rulings.
- **Local only:** standalone `/Users/fraga/Documents/brainwire`, no push,
  publication or remote PR. Local database files under `/data/` are ignored.
  All verification databases were temporary and removed by their test harnesses.
  No test server remains running. Final reports are summarized here and the
  completed plan's scratch workspace is removed after clean review.

### 2026-09-26, extra 30-minute session: runnable terminal demo

- **Requested:** Allan asked for roughly 30 more minutes toward the MVP, with
  the CLI visibly implementing the selected concept, and authorized autonomous
  local work. The earlier stop-until-tomorrow instruction was superseded.
- **Delivered:** `brainwire shell --demo`, integrated with the existing binary.
  The new Ratatui/Crossterm crate has Graph, Query, Memory and Disk & Files,
  electric-green styling, pt-BR/en text, keyboard navigation, contextual F1
  explanations, bounded Unicode editing, plain-text snapshots and NO_COLOR.
  A shared synthetic fixture has 36 nodes and 64 edges; two guided read
  examples return its actual values. Other query text is rejected visibly.
  Memory values distinguish configured budgets and usage, with memtables
  inside cache. Seven illustrative file types explain the on-disk layout.
- **Documentation:** [run guide and actual terminal captures](docs/tui-demo.md),
  [implementation slice](docs/superpowers/plans/2026-09-26-brainwire-tui-demo.md),
  README and Observatory status updated. Captures render the binary's ANSI
  output at 120×36; they are separate from the generated concept art.
- **Scope and decisions:** keep the standalone `codex/brainwire` checkout;
  deliver the design's synthetic Stage A before returning to the executor.
  Static graph layout is intentional. There is no live backend, arbitrary
  Cypher execution, live memory measurement or actual disk inspection yet.
  The query budget is shown as configuration, not enforced by the demo.
- **Review/verification:** implementation `c02a9de`, review fixes `71349e6`
  and `b54b3fe`. Final workspace run passed **233 tests**, all-target clippy
  with warnings denied and formatting checks. The runnable binary was rebuilt.
  Review identified small-terminal query clipping and partial terminal-setup
  cleanup; fixes add separate scrollable editor/results and conservative
  cleanup for partially written escape sequences. Independent PTY checks
  covered navigation, help, resize to 80×24, normal exit and exact terminal
  restoration. A final 80×24 flow reached result 12, kept unsupported status
  visible after a 512-character paste and preserved `q` while editing.
  Existing `config show` and CLI negative/snapshot paths were checked.
  Final whole-change review of `b6d3519..2a892f2` approved local demo delivery,
  with no open Critical/Important findings. Two nonblocking polish items remain:
  inspector focus is mostly a visual cue, and the second query initially clips
  at 80 columns (its tail is reachable with Enter/PgDn). Preserve these as
  follow-ups; they are not marked fixed. All work remains on the standalone
  branch, isolated from the platform; no merge or remote operation was needed.
- **Next:** executor (engine MVP Task 4), then real Bolt transaction lifecycle
  and durable `serve` (Task 5). Connect this shell only after those contracts
  exist; add bounded graph neighborhoods and real telemetry/file metadata.
- **Reconfirmed visual target:** Allan resent the backoffice image and asked
  that it not be forgotten. Saved the exact upload as
  `docs/design/assets/brainwire-backoffice-user-reference.png` with matching
  SHA-256, marked it the primary composition reference in the design, and
  added an AGENTS reminder for future UI work. Its external editor toolbar
  is not part of the product. The runnable demo does not replace this target.
- **Local only:** code and docs remain in `/Users/fraga/Documents/brainwire`;
  no push, remote PR or publication.

### 2026-09-26, visual identity selected; continue tomorrow

- **Decision:** Allan explicitly chose the latest green v2 identity: electric
  green `#57FF48`, near-black `#070B09`, restrained cyan, and the interwoven
  B/W graph logo. The [Observatory design](docs/design/2026-09-26-brainwire-observatory.md#2-selected-visual-identity)
  identifies the canonical logo, memory and backoffice references. Earlier
  color variants are historical explorations. Production assets should refine
  this choice, not reopen the visual direction.
- **Scope:** visual identity approval does not imply the proposed backend
  contracts or TUI features are implemented. README and design status updated.
- **Next session:** Allan asked to stop and continue tomorrow. Resume the
  executor (MVP Task 4), then real Bolt transactions and serve (Task 5), with
  the selected identity preserved for the TUI/backoffice work. No automation
  or scheduled run requested.
- **Verification:** documentation whitespace checked; local changes only.

### 2026-09-26, UI planning after the engine session

- **Requested:** plan the generated graph-centric terminal concept. Allan
  chose both graph exploration and Cypher in the first prototype, then added
  memory usage/budget visibility, explanations for memory variables, a logo,
  and a backoffice to inspect and understand database files. After trying
  magenta/cyan, he preferred stronger green with restrained supporting colors.
- **Delivered:** [Observatory design proposal](docs/design/2026-09-26-brainwire-observatory.md),
  local concept assets and image-generation prompts. The plan includes Graph,
  Query, Memory and Disk & Files, contextual F1 help, file-purpose inspection,
  a shared config explanation catalog, client/server boundaries, staged
  acceptance and a green visual direction. Images are synthetic concepts;
  no UI code or engine changes in this planning session.
- **Findings:** configured memory budgets are not allocated RAM. Memtables
  are charged within the shared cache budget. Cache accounting, process RSS
  and container usage are overlapping scopes and must not be summed. Query
  memory enforcement is not implemented; global versus per-query admission
  needs an explicit contract. A connected graph explorer also needs bounded
  neighborhood/edge results beyond the current MVP node/scalar output plan.
- **Next:** continue the existing engine MVP with Task 4 (executor), then
  Task 5 (real Bolt transaction lifecycle and serve). Use this design to brief
  the integrated TUI prototype and telemetry/storage inspection work. Before
  shipping live backoffice features, implement the listed backend contracts.
- **Verification:** local design links, PNG signatures and selected-image
  copy hashes checked; Git whitespace checks pass. No code tests rerun for
  this documentation and concept-art-only change.
- **Local only:** design and documentation changes are kept in this standalone
  repository; no remote, publication or push.

### 2026-09-26, 17:48 (Codex, one-hour local session)

- **Scope:** Allan requested one hour of work and explicitly prohibited pushes.
  During the session he requested moving Brainwire into an independent sibling
  repository at `/Users/fraga/Documents/brainwire`. No remote PR or push.
- **Done:** re-ran the merged workspace's 177 tests; the scoped review
  of `711a3e3` found all three graph fixes addressed and no new blocking issue.
  Merged the existing local Cypher branch (`d51482ffd`) and closed its second
  depth-limit fix round (`dd054f759`): expression depth now follows the actual
  subtree, including mixed operators and wrappers. All 41 parser tests pass;
  the scoped review is clean. MVP Task 3 is complete (`a6a741050`): snapshot
  range scans and label scans, including maximum ID bounds and transaction
  overlay behavior; 70 store tests, clippy and fmt pass, review clean. Added an
  independent regression for non-null SET after property promotion. Storage
  Task 10 is complete (`a0165496d`): five SIGKILL/reopen cycles verify every
  acknowledged Customer, Company and WORKS_AT edge; focused test and review
  pass. This proves process-crash recovery, not hardware power-loss durability.
  Corrected the README's obsolete brainstorming status.
- **Relocation complete:** extracted 54 Brainwire-only commits, moved all 50
  working source/documentation files and the build cache, preserved task
  ledgers, and removed the main platform checkout's untracked shortcut.
  SHA-256 verification matched every moved source/documentation file. The old
  feature branch/worktree remains a clean historical recovery checkpoint.
  The standalone repo has no remote
  and its Git objects have no dependency on the platform object store.
  Rewritten equivalents: parser fix `73cb05c`, label scans `2c52501`, crash
  test `1315687` (the original IDs remain above for historical context).
- **Verification after relocation:** all 225 workspace tests pass (44 Bolt,
  46 core, 41 parser, 23 config, 70 store, 1 crash integration test), as do
  workspace clippy with warnings denied and formatting. `brainwire config
  show` works from the new root. No server was left running.
- **Next step:** MVP Task 4 (executor), then Task 5 (`brainwire serve` and the
  driver demo). The executor agent had only read its brief when relocation was
  requested; no executor code or tests were written. Resume in the standalone
  repository. The session stops at 18:48 CEST.
- **Integration finding:** the current Bolt `QueryEngine` exposes only `run`;
  BEGIN, COMMIT and ROLLBACK still operate only on session state. Task 5 must
  add real engine transaction ownership/hooks before accepting durable writes
  inside explicit transactions. Wiring per-query commits into that stub would
  incorrectly acknowledge rollback after persisting the write.
  Updated the active MVP plan with standalone paths and branch, completed
  task status, and required transaction lifecycle acceptance checks.
- **Decisions:** the user's one-hour request replaces the usual 45-minute
  limit for this session. Parser work continues in the main Brainwire worktree
  after a local merge; the old parser checkout is preserved. Scratch reports
  from `/tmp/brainwire-20260926` are preserved under the standalone repository's
  ignored `.superpowers/sdd/session-2026-09-26/`; this journal records the
  durable handoff. The crash test verifies every acknowledged write, not only
  the most recent one. Scan APIs still materialize vectors per the MVP
  contract; streaming and enforcement of execution memory limits remain ahead.

### 2026-09-26, 11:03 (MVP push, interrupted at about 12:15)

- **Done:** the MVP plan (config file and CLI, a Cypher subset, the executor
  on the store, `brainwire serve`) written and committed
  ([plan](docs/superpowers/plans/2026-09-26-brainwire-mvp.md)). Merged into
  `feat/brainwire`: the Bolt server (final review plus one fix wave,
  re-reviewed clean), and the config file with `config init` and `config show`
  (unknown keys are refused; `BRAINWIRE_<SECTION>_<KEY>` overrides; memory
  defaults 10 GiB cache with 1 GiB of memtables inside it and 2 GiB for
  queries). `Cargo.lock` is committed. Storage Tasks 7 (transactions, bounded
  conflict history, read-for-update proven by tests, poison recovery) and 8
  (id blocks, poison recovery that never reuses an id) are complete and
  reviewed. Task 9 (graph operations) is implemented and reviewed. The Cypher
  parser (Task 2 of the MVP plan) is implemented and reviewed once. The
  workspace had 164 tests green at the last merge.
- **Found by the reviews:** a dangling edge when an edge commits before a
  concurrent DETACH DELETE of its node (fix: prefix dependencies in the
  committer); SET NULL not clearing a property promoted to a column; an
  invariant check blind to dangling edges; long AND/OR chains that crash the
  parser (bounded, but the bound had a bypass that the second fix closes);
  a typo in a config key passing silently.
- **Interrupted:** the API session limit stopped two implementers mid-work.
  At 16:07 Allan asked for a little more, and the Task 9 fix was finished and
  committed (`711a3e3`: DETACH DELETE conflicts with a concurrent edge in both
  orders, promotion clears the old value, the invariant check sees dangling
  edges; 177 workspace tests green); its scoped re-review is still to do.
  `.worktrees/brainwire-cypher` is clean; its fix round 2 had not started.
- **Next step:** the scoped re-review of the Task 9 fix, MVP Task 3 in
  `.worktrees/brainwire`, and MVP Task 2 fix round 2 in
  `.worktrees/brainwire-cypher` (resume the implementers from the ledgers);
  review, merge the parser, then MVP Task 4
  (executor) and Task 5 (`brainwire serve` and the driver demo: create, restart,
  read from disk).
- **Blockers:** none.

### 2026-09-26, 08:13

- **Done:** plan 1 Task 5 (catalog from the ontology) implemented on the
  temporary branch `feat/brainwire-catalog` (worktree
  `.worktrees/brainwire-catalog`), with two review fixes: id arithmetic is
  checked (`IdSpaceExhausted` instead of a silent wrap that would give two
  columns the same id), and entries that share a label merge into one type, as
  the platform's `GraphMappingConfig` allows; spec 2.1 now says so. A demo over
  the synthetic fixture showed ids staying stable when the ontology changes.
  Task 6 (RocksDB with the eleven column families and a shared cache and
  memtable budget) done and reviewed on `feat/brainwire`. Task 7 (snapshot
  transactions, first committer wins) implemented, 20 store tests. The final
  review of plan 2 (most capable model) asked for fixes before the merge.
- **Found by the final review of plan 2:** a client could reach READY without
  HELLO or LOGON through RESET (the plan's own state table was wrong against
  research/bolt.md 3.1, 3.5 and 6.6), and one `accept()` error, such as running
  out of file descriptors, ended the process. Both are in a fix wave. Parked as
  blockers for plan 3 and for any client outside this machine: the "message
  too large" error never reaches the driver, which retries for up to 30 s, and
  decoding multiplies memory by up to about 33 before authentication.
- **Decided under Allan's go-ahead (to review):** Task 5 ran in parallel on its
  own branch; shared labels merge instead of failing (union of properties,
  first `display_color`, `DuplicateLabel` removed); the plan 2 fix wave covers
  the two Important findings the reviewer asked for before the merge, and the
  rest waits for plan 3. The ledgers hold each ruling with its cost.
- **In flight at the end of the session:** the plan 2 fix wave, the re-review
  of Task 5's last fix, and the review of Task 7 (most capable model, because
  it is the concurrency code).
- **After the session:** the plan 2 fix wave came back done (driver check
  ok), and the Task 7 review asked for three fixes: the conflict history grows
  without limit while any transaction is open, no test proves the
  read-for-update dependency, and a poisoned lock panics. The rulings for fix
  round 1 are in the plan 1 ledger.
- **Next step:** finish those loops from the ledgers (Task 7 fix round 1, the
  scoped re-review of the plan 2 fix wave); re-review the plan
  2 fix wave, merge `feat/brainwire-bolt` and `feat/brainwire-catalog` into
  `feat/brainwire`, and commit `Cargo.lock`; then Task 8 (id blocks), Task 9
  (graph operations) and Task 10 (the kill -9 crash test), which closes Gate 1
  (due 2026-09-30).
- **Driver 5.x:** production pins the Python driver to `neo4j>=5.0,<6`
  (`client-core/pyproject.toml`). With Allan's OK, neo4j 5.28.6 went into a
  temporary venv in the session scratchpad, and the driver check and the demo
  pass with it too (Bolt 5.4, 0.083 ms per round trip). Rerun it after the fix
  wave.
- **Blockers:** none.

### 2026-09-25, 14:45 (second session: the code starts)

- **Done:** Rust installed with Allan's permission (rustup, stable 1.98.1,
  minimal profile with clippy and rustfmt, about 470 MB; the shell PATH was not
  changed, so run `source "$HOME/.cargo/env"` first). Worktree
  `.worktrees/brainwire` on `feat/brainwire`; the untracked copy of this folder
  in the main checkout went to the session scratchpad and the old path is now a
  symlink to the worktree. Plan 2 runs in a second worktree,
  `.worktrees/brainwire-bolt`, on `feat/brainwire-bolt`. Plan 1: Tasks 0 to 4
  done and reviewed (workspace, keys, ids, values and the row codec; 32 tests).
  Plan 2: all six tasks done and reviewed (chunk framing, PackStream,
  negotiation, messages, session, TCP server; 31 tests). The official Neo4j
  Python driver 6.3.0 connects to `brainwire-bolt-dev`, negotiates Bolt 5.4 and
  runs `RETURN 1 AS x` in auto-commit and in a managed transaction; 2,000 round
  trips in one session took 0.087 ms each (release build, stub engine).
- **Measured:** the first full build takes 1m57s (RocksDB compiled from
  source) and the incremental `cargo test` 0.70s, far below the 2-minute
  reversal rule of the plan: Rust stays.
- **Found by the task reviews and fixed:** two crash paths on hostile input
  (PackStream and the row codec decoded nested values without a depth limit,
  so a stream of nested lists aborted the process; both now stop at 64
  levels); `NodeId::type_range` panicked on an invalid type and now returns a
  `Result`; decoders and error variants without tests; the server's message
  cap moved to the spec's 256 MiB (the first version used 1 MiB, which would
  refuse the platform's 200 MB batches); a malformed message now closes the
  connection.
- **Decided under Allan's go-ahead (to review):** plan 2 on its own branch,
  merged back into `feat/brainwire` later; plan 2 Tasks 1 to 4 in one dispatch
  with one review; the driver check uses the neo4j 6.3.0 already in
  `client-core/.venv`, so nothing was installed; nesting limit of 64 levels
  on the wire and on disk; `type_range` returns a `Result` (plan 3 uses it
  that way); `Cargo.lock` is committed right after the merge; the message cap
  is configurable through `serve_with_limit`. Every ruling, with what it costs
  if wrong, is in the ledgers
  (`.worktrees/brainwire/.superpowers/sdd/<plan>/progress.md`, git-ignored).
- **Next step:** final whole-branch review of plan 2 on the most capable
  model, then merge `feat/brainwire-bolt` into `feat/brainwire` and commit
  `Cargo.lock`; then plan 1 from Task 5 (schema and catalog) to Task 10 (crash
  test), with `superpowers:subagent-driven-development`.
- **Open for plan 3 (from the reviews):** `ChunkReader` copies each message
  twice, while spec 3.3 asks for decoding straight from the chunks;
  auto-commit PULL does not return a bookmark yet; the server has no logging,
  read timeout or connection cap; LOGOFF is not handled.
- **Blockers:** none.

### 2026-09-25, 11:48 (session)

- **Done:** the spec moved to English and was completed
  ([spec](docs/superpowers/specs/2026-09-24-brainwire-design.md)); Allan
  approved sections 1 to 4 one by one; implementation plan 1, the storage
  foundation, written in 11 tasks with tests first
  ([plan 1](docs/superpowers/plans/2026-09-25-brainwire-storage-foundation.md));
  implementation plan 2, the minimal Bolt server, written in 6 tasks
  ([plan 2](docs/superpowers/plans/2026-09-25-brainwire-bolt-minimal.md)); the
  docs committed to the local branch `feat/brainwire` (no push), written without
  switching branches, so the PII branch was not touched. All three research notes
  are now in English (translated in parts after two earlier rounds of
  translation agents stalled; headings and line counts checked against the
  originals).
- **Decided by Allan:** an event inherits the visibility of its owning entity
  unless it comes with its own ACL; "see comments" in the CLI means people's
  annotations on nodes and types.
- **Decided under Allan's go-ahead to proceed without asking (to review):**
  section 5 of the spec (cluster, operations, tests) as written; each RBAC user
  has one role and a list of databases, set with `ALTER USER ... SET
  DATABASES`; a `reader` can add notes; the `pk` encoding uses an escaped
  terminator instead of a length prefix; ids come in blocks whose end is
  committed before use; one implementation plan per gate (plan 1 storage,
  plan 2 Bolt, plan 3 Cypher and ACL).
- **Next step:** Allan reviews the go-ahead decisions above and plans 1 and 2.
  Then execute plan 1 and plan 2 with `superpowers:subagent-driven-development`,
  starting with Task 0 of plan 1.
- **Blockers:** the Rust toolchain is not installed on this machine, and
  installing it (rustup from the official source) is a download that needs
  Allan's permission; consent for the worktree of `feat/brainwire`; the Python
  `neo4j` package for plan 2's driver check, if it is missing.

### 2026-09-24, 22:00 (wrap-up)

- **Done:** rhythm corrected; scope and date confirmed; internal documents moved
  to English, with `PLANO.md` renamed to [PLAN.md](PLAN.md) and `DIARIO.md` to
  this file.
- **Decided:** 45 minutes of work per day, not per hour, with the session
  starting when Allan calls it (the hourly loop from 21:00 was a
  misunderstanding and is cancelled). Scope: everything stays, and so does the
  2026-10-31 launch date, with a simple RBAC (three fixed roles and access per
  database; fine-grained visibility stays with the ACL). Versioning and the
  release flow are parked for later.
- **Next step:** with Allan's approval of spec section 1, present section 2
  (already drafted). Without it, draft section 3 (query and protocols) from
  `research/bolt.md` and `research/cypher-corpus.md`.
- **Blockers:** approval of section 1; the decision on whether an event
  inherits the visibility of its owning entity (spec section 2.2).

### 2026-09-24, 21:00 (first scheduled hour)

- **Done:** section 1 of the spec written and presented
  ([spec](docs/superpowers/specs/2026-09-24-brainwire-design.md)); the
  platform's Cypher inventoried
  ([research/cypher-corpus.md](research/cypher-corpus.md)), with the `pk`
  changed to start with `entity_id`; draft of section 2 (data model and
  security) in the spec, not yet presented; two platform problems found along
  the way became separate tasks (scope of the ACL predicate's variables; five
  inconsistencies in the Cypher path). Bolt research done
  ([research/bolt.md](research/bolt.md)): target Bolt 5.1 to 5.4 without the
  manifest, and the HELLO `server` value must start with `Neo4j/` or driver 5.x
  refuses the connection. RocksDB research done
  ([research/rocksdb.md](research/rocksdb.md)) and already applied to section
  1: a 10 GB cache with the memtables inside it, a filter chosen per column
  family (bloom on everything would cost 12.5 GB), an in-house overlay instead
  of WriteBatchWithIndex, which the crate does not expose, and the RAM peak of
  the import with a ZSTD dictionary.
- **Decided:** nothing new from Allan in this hour.
- **Next step:** superseded by the 22:00 entry.
- **Blockers:** approval of section 1.

### 2026-09-24, session 0

- **Done:** requirements round of the brainstorming; name; folder; proposal of
  the plan (scope, approach, ontology as schema, ACL, RBAC, cluster, Python
  SDK, algorithms and AI, CLI, calendar with gates).
- **Decided:** see the requirements table in the README. The name is
  Brainwire, with "brain" in it at Allan's request (Engrama and Brainfold were
  dropped). Approach approved: Rust + RocksDB. Launch on 2026-10-31, with
  cluster/HA, RBAC and full openCypher in scope. The ACL follows the client
  model (`AclPredicate`), native to the engine. No commit until the spec.
- **Next step:** the spec in five sections, each approved before the next:
  (1) storage; (2) data model and security: ontology, ACL, RBAC, events;
  (3) query, protocols and SDK; (4) algorithms, AI and CLI; (5) cluster,
  operations and tests.
- **Blockers:** the open items listed in the plan.
