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
   `/Users/fraga/Documents/brainwire`; `main` tracks `origin/main`. Verify the
   current branch and working tree before changing files. Pushes and releases
   require Allan's explicit request for the relevant work. The old platform
   branch and its worktrees are historical
   recovery points. Older entries below retain their original paths and commit
   IDs; standalone history has rewritten IDs after subtree extraction.

## 2026-11-05: v0.15.0-beta.14 memory admission and integration

- **Done:** check sampled process headroom before Bolt ingress grows, while retaining bounded recovery messages; pre-admit query output structures before allocation; preflight adjacency and staged removals for high-degree `DETACH DELETE`. Inspired by documented memory-control principles from Memgraph and FalkorDB, without copying their source. See [reference notes](docs/design/memory-reference-notes.md).
- **Verification:** 1,100 Rust tests passed (3 ignored), full workspace Clippy and formatting passed; 234 Python client tests passed. Synthetic Python/Bolt ingestion, abrupt restart and offline audit passed. With a 256 KiB query pool, a separate 16-connection test held two reservations, refused 14 and released credit after client death; deadline rollback passed. The Docker Compose demo initialized and passed its ready probe with the candidate image. An isolated three-process quorum test passed, not multi-machine qualification. The local 512 MiB cgroup pressure smoke passed on both candidate builds; two subsequent untraced controls were inconclusive because external samples did not establish the decision-time value.
- **Memory evidence:** a short synthetic 200,000-node, 200,040-edge CREATE and incremental mutation scenario passed with 20 labels, a 2 GiB container and an application writer. Restart and offline audit passed. Separate small 100k-node/edge read diagnostics passed at 2 GiB (15 minutes), 4 GiB (30 minutes), and 8 GiB (15 minutes) on a shared host with concurrent work. These did not approach the container caps. See [diagnostic](docs/benchmarks/2026-11-05-load200k-015b14.md). Output list conversion can temporarily require a separately admitted result buffer. Sampled admission and separate pools do not guarantee a process-wide RSS ceiling.
- **Next step:** qualify mixed read/write workloads with representative data and real multi-machine failover; unify accounting where feasible and resolve the previously unexplained pressure result. Support keyed online relationship catalog declarations before relying on keyed event ingestion.
- **Blockers:** sustained 2/4/8 GiB qualification and strict global RSS limits remain open. The initial pressure failure is unexplained. Cgroup samples are not atomic, and cooperative checks cannot prevent every OOM. Do not claim production readiness.

## 2026-11-05: v0.15.0-beta.13 admission and recovery diagnostics

- **Done:** close configured process-memory admission when RSS observations are absent or stale; bracket cgroup working-set sampling with cache reads; bound Raft encoded inbound frame capacity; add an opt-in guarded two-node CSI volume handoff diagnostic. Recognize the new monitor refusal in the Python client. Make an externally bracketed but internally unverified pressure observation inconclusive, not a pass.
- **Verification:** 1,084 Rust tests passed (3 ignored); full workspace Clippy and formatting passed. Python client tests, 45 cgroup diagnostic tests and 12 mocked CSI harness tests passed. Raft ingress had a reproduced red regression and 46 green tests, with overload recovery repeated five times. CSI is mocked; no real CSI handoff or multi-machine failure was tested.
- **Memory evidence:** the final candidate passed traced standard and persistent-observer pressure repeats and a persistent untraced control at 512 MiB. An earlier standard smoke returned FAIL while server trace showed a below-threshold decision; the external sampling precondition is inconclusive and the original receipt remains unchanged. See [pressure investigation](docs/benchmarks/2026-11-05-pressure-beta13.md). A 30-minute synthetic 2 GiB read diagnostic on the previous beta passed with 100,000 nodes and edges; it is not a candidate or production qualification. A short shared-host performance comparison was inconclusive.
- **Integration:** a synthetic application writer parity test failed against both beta.12 and beta.13 when the graph was initialized without keyed timeline metadata. It passed on beta.13 when the type's immutable key was declared in the initial ontology. Online `db.createRelationshipType` cannot currently supply that key; existing unkeyed types cannot be converted in place.
- **Next step:** add safe keyed online catalog declaration and adapt clients to verify identity before writes; separately run representative sustained mixed loads and independent-machine/CSI failure tests on a dedicated environment.
- **Blockers:** process RSS remains a sampled cooperative guard, the earlier beta.12 pressure result cannot be reconstructed, and real multi-machine failover, CSI handoff and sustained representative workloads remain unqualified. Do not claim production readiness.

## 2026-11-05: v0.15.0-beta.12 memory, events and recovery

- **Done:** admit DISTINCT and UNION key copies before allocating; account for
  both grouped-key copies; add Cypher `nullIf`; add bounded keyed-event batch
  deletion; reject conflicting orphan catalogs during Raft materialization;
  improve quorum test diagnostics; add contextual help to standalone memory
  view. Add an optional persistent cgroup observer and bounded opt-in trace.
- **Verification:** 1,074 Rust tests passed (3 ignored), full Clippy and
  formatting passed, along with 233 Python, 108 script and 43 cgroup tests.
  Bolt tests cover keyed events, authorization, catalog conflicts and restart;
  an isolated application-writer parity test passed on a copied source tree.
- **Memory evidence:** a standard 512 MiB pressure smoke failed once on a local
  pre-trace image. Controlled repeats and later standard/observer runs on the
  final traced image passed; the original decision remains unexplained. See
  [pressure investigation](docs/benchmarks/2026-11-05-pressure-investigation.md).
  A 40,000-node/edge 30-minute read soak at 2 GiB and a 100,000-node/edge
  15-minute read soak at 4 GiB passed on the earlier pre-trace image. Neither
  qualifies production capacity, mixed writes or a global RSS ceiling. The
  final traced image passed standard and persistent observer runs with trace
  on, plus a standard run with trace off; logs confirmed refusal using the
  server's own readings in those runs.
- **Next step:** investigate the original pressure discrepancy and the earlier
  CI quorum lookup failure; qualify representative sustained mixed workloads,
  CSI volume behavior and failover across independent machines.
- **Blockers:** sampled admission is not a hard RSS cap; native Bolt TLS,
  historical revisions and production-qualified multi-machine failover remain
  open. Do not describe the beta as production ready.

## 2026-11-04: v0.15.0-beta.11 memory, quorum and event editing

- **Done:** allow sampled memory admission to recover under concurrent low RSS
  observations; budget `split()` before allocation and avoid deep copies in
  `size()`, `isEmpty()` and `exists()`; preserve exact inputs in
  `percentileDisc`; validate Raft commands before append and log reopening;
  allow Python to edit one keyed relationship; add an optional Kubernetes
  single-pod PVC overlay. Correct the protected image setup documentation.
- **Evidence:** 1,050 Rust tests passed (3 ignored), workspace Clippy and
  formatting passed; 224 Python and 108 script tests passed. A 512 MiB cgroup
  pressure injection recovered; a 100,000-row Docker load passed forced
  restart and indexed-overwrite checks. The local quorum harness passed 21
  checks, and 300,000 synthetic rows survived three voter restarts on the same
  host. Protected ACL, TUI, checkpoint and restore checks passed locally.
- **Small read soak:** the final candidate completed 30 minutes of synthetic
  reads in a 2 GiB container with correct counts and offline audit. Median
  process RSS went from 227.5 MiB to 215.9 MiB; no OOM was observed. The
  workload is small and the host shared; this does not qualify production
  capacity or explain the earlier observed increase.
- **Application integration:** an isolated synthetic writer parity scenario
  passed against the candidate image. A composite-key timeline and a large
  integer percentile were checked over Bolt; keyed events survived SIGKILL.
  A protected Bolt test confirmed that a read-only user and a writer using
  delegated visibility cannot mutate keyed events, while a trusted writer can.
- **Next step:** continue representative sustained memory and mixed-write
  qualification; test CSI volume behavior and quorum on separate machines;
  investigate the earlier RSS and leader-election failures before production.
  Harden quorum graph materialization to compare any existing catalog with the
  requested schema, matching the check performed when reopening a graph.
- **Blockers:** no hard global RSS ceiling, native Bolt TLS, automatic graph
  history or qualified multi-machine failover. The optional single-pod PVC
  overlay has only been rendered, not exercised on a real cluster.

## 2026-11-04: v0.15.0-beta.10 bounded writes and recovery

- **Done:** commit full Python node/edge batches before requesting another
  normalized row; verify checkpoint manifests and reject special files; budget
  `replace()` by its real output; avoid full JSON buffers during replicated
  command validation and hashing while preserving request digests. Add a
  receipt comparator for RSS windows, including anonymous/file-backed memory,
  and record more workload settings in new read-soak receipts.
- **Evidence:** 1,036 Rust tests passed (3 ignored), workspace Clippy and
  formatting passed, 217 Python and 108 script tests passed. Local three-voter
  acceptance passed 21 checks; the candidate passed the 512 MiB synthetic
  pressure smoke. A real checkpoint copy, offline audit and restored read
  passed in a disposable directory; protected ACL and TUI checks passed too.
- **Small read soak:** the same intended 2 GiB, 4,000-node synthetic fixture
  passed 30 minutes of reads, counts and offline audit. Median RSS fell from
  about 227 MiB to 208 MiB; this does not explain the beta.9 increase or prove
  a fix. Both runs used a shared host and older receipts omit some settings.
- **Quorum writes:** 300,000 synthetic rows verified after killing and
  restarting three local voters; their offline graph digests matched. This
  shared-host diagnostic does not resolve the earlier election failure.
- **Application integration:** a disposable writer parity scenario passed
  against two isolated databases. A producer received batch callbacks before
  yielding the next row, and keyed events survived an isolated SIGKILL/restart.
- **Next step:** investigate the read-soak RSS variation with component-level
  telemetry and production-shaped data; qualify independent voter machines,
  encrypted connections and safe snapshot/log compaction.
- **Blockers:** memory admission is sampled, not a hard process ceiling; native
  Bolt TLS and multi-machine failover qualification remain open. Checksums do
  not authenticate checkpoint data.

## 2026-11-04: v0.15.0-beta.9 application pilot

- **Done:** account for temporary Cypher list-comprehension and reduce bindings;
  validate replicated apply batches before graph effects; protect checkpoint
  staging from competing copies; add an optional authenticated single-instance
  Kubernetes ingress policy and an application pilot checklist.
- **Evidence:** 1,030 Rust tests passed (3 ignored), Clippy and formatting
  passed, 199 Python and 101 script tests passed. Local three-voter fault
  acceptance passed 21 checks. The candidate passed two 512 MiB synthetic pressure
  test, an isolated application-writer parity scenario, and a keyed-edge
  SIGKILL/restart. These are not multi-machine or production qualifications.
- **More recovery:** a brief 100,000-node load retained its count after a
  restart. Uncommitted indexed overwrites were interrupted in a separate
  container; confirmed data and the offline index audit survived each restart.
- **Quorum write diagnostic:** three local voters verified 300,000 synthetic
  rows after SIGKILL/restart, with matching offline audits. This shared-host,
  debug-binary run does not explain the earlier leader-election failure or
  qualify multi-machine behavior.
- **Small read soak:** four concurrent readers with delegated visibility ran for
  30 minutes after a 4,000-node load in a 2 GiB container; counts and offline
  integrity passed. Median sampled RSS rose about 53 MiB between the first and
  last five-minute windows. Investigate the increase; do not label it a leak or
  treat this small read-only soak as full memory qualification.
- **Next step:** qualify representative ingestion and permissions, restore a
  verified backup, and exercise independent volumes, protected peers, leader
  routing and machine loss. Implement safe snapshot and log-compaction support.
- **Blockers:** no hard process RSS ceiling, native Bolt TLS, or qualified
  multi-machine failover; Kubernetes NetworkPolicy enforcement was not tested
  in a live cluster.

## 2026-11-04: v0.15.0-beta.8 query admission and recovery

- **Done:** Account for temporary Cypher quantifier copies, validate persisted
  Raft votes before opening a voter, and provide an optional single-pod
  disruption budget for voluntary Kubernetes evictions.
- **Evidence:** Focused regressions and static deployment rendering passed. An
  isolated writer parity scenario passed with a preconfigured keyed graph.
- **Next step:** Qualify sustained memory and multi-machine recovery; exercise
  Kubernetes eviction in a real cluster.
- **Blockers:** No hard process RSS ceiling or qualified multi-node failover.

## 2026-11-04: v0.15.0-beta.7 release verification follow-up

- **Finding:** The beta.6 GitHub release failed in a quorum read under the
  Linux runner; its main CI also found a version check tied to old HTML markup.
- **Fix:** Apply `TCP_NODELAY` to accepted peer connections as well as outbound
  connections, before protocol/TLS handling. Keep quorum requirements intact.
  Update the version test for the refreshed documentation markup.
- **Evidence:** Two local Linux arm64 runs of the same 300-operation quorum
  test took 26.88/27.01 s before and 1.00/0.76 s after. This is not a
  multi-machine benchmark or proof of the specific CI refusal's cause.
- **Verification:** All seven Linux network tests, the local workspace suite,
  version scripts, 21 local quorum checks and the isolated writer comparison
  passed. The final image was tested against a preconfigured keyed graph.
- **Next step:** Verify GitHub CI, image publication and public pull. The exact
  cause of the original timeout is not proven.
- **Blockers:** Multi-machine recovery and sustained memory limits remain open.

## 2026-11-04: v0.15.0-beta.6 keyed relationship identity

- **Done:** Added an explicit, immutable merge-key policy for new relationship
  types. Legacy types keep pair identity. Added guards for Python updates and
  pair-only source synchronization and exposed policy through schema inspection.
- **Evidence:** Synthetic integration parity passed with a preprovisioned keyed
  type where the previous candidate lost five relationships. A local restart
  retained two distinct events. Focused tests cover replay, rollback, deletion,
  concurrency and the legacy update behavior.
- **Also:** Account and deduplicate entity-ID `IN` candidates; reject
  group/other-writable native client credential files; refresh the logo,
  documentation and TUI palette; extend local quorum coverage for keyed edges.
- **Decision:** Key policies cannot be added to existing types in place. Rebuild
  and reingest from authoritative sources; do not mix older binaries or
  downgrade a keyed graph. Pair-only source sync is rejected for keyed types.
- **Load smoke:** A local 2 GiB container loaded 100,000 synthetic nodes and
  retained the count after restart. This brief run is not sustained 2/4/8 GiB
  qualification or a comparison.
- **Next step:** Qualify representative data volumes, indexed key lookups and
  multi-machine failure and recovery; test migration with real source backups.
- **Blockers:** Native historical revisions and a global RSS ceiling remain
  unimplemented, and existing overwritten events require source recovery.

## 2026-11-04: v0.15.0-beta.5 memory observations and recovery

- **Done:** Preserve fresh RSS observations for query reservations, even when
  a sampler read overlaps an equal-valued RUN observation. Reject regressing
  Raft leader identities on append and recovery. Add an explicit timeline
  example reader with a row limit and deterministic timestamp ordering.
- **Decision:** Retain the documented relationship `MERGE` identity; changing
  its semantics needs coordinated client migration and recovery of lost data.
- **Evidence:** Focused memory and replication tests cover stale samples and
  rejected logs. The timeline reader and event-node model were exercised against
  a local server, including rollback, concurrency and restart. External adapter
  unit tests passed, but its existing property-keyed relationship scenario
  still lost five relationships compared with its reference graph.
  A local 512 MiB pressure smoke and 19 loopback quorum scenarios passed;
  neither qualifies an RSS ceiling or multi-machine recovery.
- **Next step:** Qualify representative workloads, identify a compatible keyed
  relationship migration, and exercise multi-machine recovery.
- **Blockers:** The RSS limit remains cooperative; history and distributed
  production qualification remain open.

## 2026-11-03: v0.15.0-beta.4 explicit timeline and vote protection

- **Done:** Added an opt-in Python example that records milestones as separate
  event nodes in a transaction and rejects conflicting sequential replays. Raft
  storage now rejects regressing or incomparable votes before persistence.
- **Evidence:** An isolated adapter scenario verified distinct milestones,
  replay, conflicting concurrent writes, rollback and restart. The existing
  external writer comparison still missed five relationship milestones; the
  example does not change that writer. Local replication tests exercise the
  vote guard. No live application services were changed.
- **Decision:** Document the relationship `MERGE` identity limitation and use
  rebuildable data for this experimental release.
- **Next step:** Design compatible relationship identity and migration, then
  repeat the full writer comparison and distributed failure qualification.
- **Blockers:** Automatic historical revisions, a global memory ceiling and
  qualified multi-machine failover remain unavailable.

## 2026-11-03: v0.15.0-beta.3 DuckLake conflict handling

- **Finding:** after beta.2 was tagged, the synthetic 10,000-node, four-worker
  DuckLake/Bolt acceptance twice hit an `Outdated` transaction conflict during
  concurrent node deletes. Distinct deleted nodes can share an incident edge.
- **Fix:** DuckLake sync retries an individual transaction at most eight times
  with backoff only for `Outdated` with the server's explicit rolled-back
  outcome. Unknown commit outcomes and connection failures are not retried.
  Exhausted failures retain the source lease and do not advance its cursor.
- **Evidence:** the same 10,000-node, four-worker workload passed three times
  after the fix. A 20,000-node, eight-worker run also passed, including
  incremental changes, full reconciliation and offline consistency audit.
  Tests cover the retry filter and failure behavior. This is not sustained-load or
  multi-machine qualification.
- **Verification:** the Rust workspace passed 994 tests (3 ignored) on rerun;
  an earlier run under concurrent Docker builds/load had an authentication
  deadline test fail, which passed in isolation. Python, documentation and
  formatting checks passed; the optional DuckLake tests passed with DuckDB.
  The local beta.3 image passed the synthetic 512 MiB cgroup smoke.

## 2026-11-03: v0.15.0-beta.2 memory, recovery and ingestion

- **Done:** sample RSS on RUN admission using the higher of the fresh and latest
  periodic reading; reject invalid or regressing Raft commit markers before
  persistence; add three Cypher list conversions; batch Python-side missing-key
  deletions during DuckLake full reconciliation.
- **Evidence:** targeted memory, replication and Cypher tests passed. All 12
  DuckLake sync tests passed with DuckDB 1.5.5 and cached DuckLake/SQLite
  extensions, including six real local DuckLake tests. A synthetic end-to-end
  DuckLake-to-Bolt run with 1,000 source nodes, two workers, incremental
  changes and full prune passed, including the offline consistency audit.
  A synthetic 512 MiB cgroup pressure smoke passed on the local arm64 image;
  a three-process loopback quorum test passed 19 checks, including partition,
  leader loss and retained-state recovery.
  A single-host TLS proxy test passed strict CA/hostname verification, negative
  certificate and plaintext checks, grants and recovery after SIGKILL with
  Python 3.14/OpenSSL 3.5. Python 3.9.6/LibreSSL 2.8.3 failed TLS readiness
  with an undiagnosed wrapped error. The proxy-to-backend link was plaintext.
  The TLS harness now retains the last connection error on timeout and records
  Python and SSL-library versions; three isolated readiness tests passed.
- **Checks:** Rust workspace tests passed (994 passed, 3 ignored), as did
  workspace Clippy and formatting; Python tests ran 174 (7 skipped in the base
  environment), script tests 101, and documentation checks passed. The separate
  DuckLake test above used its cached optional dependencies without skips.
- **Overhead diagnostic:** one sequential Docker Desktop arm64 trial of 300
  warmed `RETURN 1` auto-commit queries per image measured median 0.6503 ms on
  beta.1 and 0.6979 ms on the local beta.2 candidate (p95 0.9587 and 1.1905
  ms). This is a small unpaired synthetic sample, not a controlled performance
  comparison; repeat with randomized order and representative workloads.
- **Limits:** on-demand RSS remains cooperative and costs up to two OS reads
  per RUN. Python batching does not bound DuckDB or server enumeration memory.
  Native Bolt TLS, CDC, graph history, ANN/BM25, 2/4/8 GiB sustained memory
  qualification and multi-machine failover remain open.
- **Next:** measure the admission overhead under representative load, qualify
  long-running cgroup workloads on a host with verifiable power, and test
  distributed recovery across real machines.

## 2026-11-03: v0.15 memory admission and log recovery

- **Done:** cgroup v2 admission considers the lower finite `memory.high` or
  `memory.max` for each visible ancestor; decision logs identify the source.
  Raft rejects invalid first indices and regressing terms before persisting a
  batch. Invalid batch tests verify atomicity, reopen and valid prefix retries.
- **Decided:** publish as an experimental beta. The cgroup estimate discounts
  inactive file cache and cannot guarantee an RSS ceiling or prevent kernel
  throttling. Replication does not yet support snapshots or log purging.
- **Container checks:** a local v0.15 arm64 image passed the existing synthetic
  512 MiB cgroup smoke and the demo Compose startup, Bolt write and read after a
  normal restart. These do not exercise a real `memory.high` deployment or
  qualify sustained load. Test containers and their volume were removed.
- **Quorum check:** a synthetic three-process loopback test passed leader loss,
  partition, retained-state restart and offline digest checks. This does not
  qualify recovery across hosts or snapshot transfer.
- **Verification:** workspace tests passed (988 passed, 3 ignored), as did
  workspace Clippy and formatting, 171 Python tests (7 skipped), 101 script
  tests, and documentation link and site checks.
- **Next:** sustained 2/4/8 GiB tests, multi-machine failover and recovery,
  native transport security and realistic workload qualification.
- **Blockers:** these tests do not certify production availability or memory
  safety, and existing corrupt logs are not automatically repaired.

## 2026-11-03: public beta readiness and Docker access

- **Done:** made the existing GHCR package public with owner confirmation;
  anonymous clone, pull, Compose startup, Bolt read/write and retained-volume
  restart passed on the published beta.3 image in Docker Desktop arm64.
  Enabled GitHub Pages and private vulnerability reporting; the newest Pages
  deployment succeeded and the site responded with HTTP 200.
- **Code:** show accounted query-pool utilization and unreserved budget in the
  connected TUI; add `toBooleanOrNull`; reject unsafe Unix modes for the Raft
  peer-token file while accepting group-readable mounted secrets.
- **Candidate evidence:** 985 Rust tests passed (3 ignored), Clippy and fmt
  passed, 171 Python tests ran (7 skipped), 101 script tests and 23 cgroup
  harness tests passed. The final native binary passed 19 quorum checks, 20
  log-cap checks, a 5,000-row ingestion/restart and two-principal ACL test.
  The local beta.4 arm64 image passed the Compose/Bolt/restart test and an
  isolated 512 MiB cgroup refusal/rollback smoke; the sanitized receipt is in
  `docs/benchmarks/2026-11-03-cgroup-014b4.json`.
- **Limits:** this is a local image and synthetic smoke, not sustained 2/4/8 GiB
  or multi-host failover qualification. The previous pressure failure remains
  unexplained. A global RSS ceiling and native Bolt TLS remain open.
- **Next:** verify the public beta.4 release and anonymous pull after CI; qualify
  larger sustained loads and multi-machine quorum recovery before production.

## 2026-11-03: schema inspection and pressure diagnostics

- **Done:** expose `graph.schema()` through the authorized Python read path;
  resolve schema relationship endpoints through a budgeted lookup. Add
  incremental evidence processing so an oversized response stops early.
- **Correctness:** accept finite float inputs in the signed 64-bit range for
  `toInteger`; preserve the existing more restrictive string conversion.
  Extend the quorum acceptance with a synchronized leader loss after an
  identified commit and a replay check.
- **Memory:** align the 512 MiB smoke's observed pressure with the runtime's
  working-set estimate and require a structured cgroup rejection. The old
  failed receipt is unavailable and remains unexplained. A Docker daemon was
  unavailable locally, so the modified smoke was tested without a container.
- **Measured:** in a synthetic debug-process test of 4,096 types and 4,096
  signatures, the median including setup/teardown fell from 221.88 ms to
  100.37 ms across seven sequential samples each on a shared host. This is not
  an end-to-end or production benchmark.
- **Verification:** candidate source passed 981 Rust tests (3 ignored),
  workspace Clippy and formatting, 171 Python tests (7 skipped), 101 script
  tests, 23 cgroup harness tests, and documentation link/site checks. A 5,000-row
  synthetic Bolt ingestion/restart test and two-identity context/schema check
  passed on the final binary. The same binary passed 19 local quorum checks
  and 20 checks with log-cap recovery; all temporary resources were removed.
- **Next:** qualify sustained memory under cgroup limits, production ingestion,
  and quorum recovery across machines; explore durable status for ingestion
  and index jobs. No global RSS ceiling or production HA claim is established.

## 2026-11-02: evidence citations and reproducible comparison

- **Done:** added bounded evidence citations to the Python context helper and
  tested ACL isolation; added rate-limited cgroup pressure diagnostics without
  changing admission decisions. Corrected CI dependency installation order.
- **Measured:** repeated the synthetic three-database comparison with pinned
  image IDs, equal container CPU and memory limits, restart checks, recorded
  samples and a chart. The receipt describes important workload and durability
  differences; it is not a production ranking. A separate 512 MiB pressure
  smoke passed on the same candidate image; an earlier failure remains open.
- **Decided:** ship this as an experimental v0.14.0-beta.2, not a native
  hybrid-search release. Evidence citations identify current records, not
  historical revisions. Memory accounting is not a global RSS ceiling.
- **Next step:** investigate the prior pressure failure, qualify sustained
  memory use and recovery, and implement native indexed hybrid retrieval.
- **Blockers:** production readiness, distributed failover, and memory ceilings
  remain unqualified by these tests.

## 2026-11-02: experimental launch candidate

- Simplify the public-facing README and Python setup; keep the synthetic demo
  labeled and the limitations visible. Prepare v0.14.0-beta.1 for controlled trials.
- The previous session's work remains part of this candidate. Its failed 512 MiB
  pressure run and the limitations in the entry below remain unresolved; a
  successful smoke does not erase that failure or prove production readiness.
- **Next:** verify the exact candidate, release workflow and image publication;
  qualify memory under sustained load and multi-machine recovery separately.

## 2026-11-02: agent context, ontology and recovery work in progress

- Added bounded Python context reads, explicit-candidate lexical lookup and
  experimental local hybrid reranking. This is not indexed ANN or BM25.
  A two-principal Bolt acceptance verifies hidden candidates do not affect
  returned results or scores. Output caps are not process RSS caps.
- Added optional ontology descriptions, with old catalog read compatibility;
  downgrade and mixed-version clusters remain unqualified.
- Projection accounting avoids cloning unselected map properties. Tests cover
  large unselected values and admission before selected-value copies.
- Replication startup rejects missing retained-log prefixes and regressing terms;
  the zero-prefix invariant requires revisiting when purge/snapshots are added.
- Native client credential files reject special files; Python connect helper
  closes its driver. Added bounded Python AST extraction, opt-in static import
  and call candidates with snapshot isolation, and local degree/path/explain
  commands. Evidence is reported, not authenticated; this is not a complete
  multi-language dependency graph or a runtime call graph.
- Replaced README hero artwork with a labeled synthetic animated illustration.
- Workspace Rust tests and all-target Clippy passed on the integrated Rust diff;
  further Python, ingestion, packaging and review checks remain in progress.
- **More:** reject ambiguous delegated visibility fields before JSON conversion,
  bound replication frame growth, and yield cooperatively during Bolt encoding.
  Validate zero-field RESET/ROLLBACK and fixed peer endpoints. Index backfill
  limits items per batch; it is not a byte limit. Add source lease validation.
- **Evidence:** final workspace verification passed 974 Rust tests, with 3
  ignored, and Clippy passed. An earlier 988-test run included 14 tests for a
  standalone tokenizer prototype later removed because it was not integrated.
  Python discovery ran 160 tests, with 7 skipped (6 DuckLake dependencies and
  one pattern-syntax case on Python 3.9); harness unit tests passed 12/12.
  Final server SHA-256 e437091f9eba53a41fdf2c04ffa5693e177498b22a78c864791866d67ebfd12d
  passed 18 three-process quorum checks after rebuilding, including restart and
  identified-commit reconciliation without callback replay. Bolt integration
  passed for code graph snapshots and two-principal context reranking, with
  matching pre/post binary hashes. A two-file CLI demo found the call path and
  marked the inferred edge. These are synthetic local tests, not multi-machine
  qualification.
- **Container diagnostic:** isolated 512 MiB cgroup pressure smoke passed twice
  and failed once when a RUN was admitted between raw-pressure samples. The
  failure receipt remains FAIL: the server subtracts inactive file cache, which
  the original receipt did not measure, so the cause is unknown. A later PASS
  recorded the working-set estimate separately. No sustained 2/4/8 GiB or
  kernel OOM qualification was achieved. The image was built mid-session and
  is not qualified as the exact final checkout.
- **Caveat:** an asset-generation command reinstalled the same-version local
  Python package as editable in .venv; pytz was restored to its prior version,
  but the earlier installation mode could not be verified.
- **Next:** finish integrated checks; investigate the pressure failure with a
  decision-point receipt; qualify sustained memory, multi-machine recovery and
  production access controls. Keep work local.

## 2026-11-02: process memory telemetry

- **Done:** expose configured process limit and a best-effort on-demand resident
  memory sample in `CALL db.memory()`, with null for unavailable values. Existing
  admission semantics are unchanged. The counters do not establish an RSS cap.
- **Reference:** HelixDB documents query memory accounting separately from RSS;
  Brainwire continues to describe the distinction and its own limits explicitly.
- **Checks:** targeted memory and embedded-visibility tests passed after a red
  regression; affected-package Clippy and formatting passed. An independent
  review found no blockers. Release and image publication must be verified.
- **Next:** verify the candidate release workflow and image publication; qualify
  sustained 2/4/8 GiB workloads and multi-machine recovery separately.

## 2026-10-31: v0.13.0-beta.2 memory and quorum candidate

- **Memory:** path-output accounting now borrows node and relationship fields
  instead of deep-cloning their properties before admission. A regression test
  measured 132,376 transient allocated bytes before the change and zero after;
  the estimate itself remains unchanged. This is not a global RSS limit.
- **Replication:** opening a Raft store verifies that its membership marker
  equals the last applied membership in its retained log. New stores and
  unapplied membership entries remain valid; inconsistent metadata fails closed
  without rewriting data. Purge and snapshots remain unsupported.
- **Diagnostic:** the log-cap acceptance now recognizes the client's typed
  commit-outcome-unknown error and keeps the capacity check, quorum lookup and
  stable request-id reconciliation. The previous harness failed twice before
  reaching recovery because it missed this exception.
- **Checks:** 950 Rust workspace tests passed, three ignored; targeted RED/GREEN
  tests and an independent review passed. The exact candidate passed 18 synthetic
  loopback quorum checks including the 1 MiB log-cap/restart scenario, request
  outcome reconciliation and cleanup. These are not multi-machine qualification.
  Python tests: 97 passed, six skipped. Workspace Clippy and formatting passed; release remains to verify.
- **Next:** finish Clippy and release gates, then record the published image
  identity. Continue sustained 2/4/8 GiB memory and multi-machine failover
  qualification; this release remains experimental.

## 2026-10-30: v0.13 Docker and memory diagnostics

- **Release:** `v0.13.0-beta.1` at `88fc954` was published as an
  experimental prerelease. GitHub verified native amd64 and arm64 images,
  including credential setup and retained-volume restart, then published the
  manifest. The release receipt matches both tested architecture digests and
  the source. Manifest: `sha256:a49cf2b6dcb26edae3af8d6d58597c3053196964c8b39623e62c02da85af30d4`.
  The private repository and image require authorized access.
- **Checks:** built the Linux arm64 image locally, confirmed unprivileged
  `auth init`, and tested the actual demo Compose startup/write/restart path.
  Protected Compose acceptance passed with synthetic secrets, 1,000 indexed
  writes, authorization checks, SIGKILL recovery and offline audit. Five
  retained-volume restart cycles passed on the same local image.
- **Memory:** three 512 MiB cgroup-pressure runs passed with refusal, rollback,
  recovery, audit and no increase in the sampled OOM counters. This is not the
  required sustained 2/4/8 GiB workload, a hard RSS guarantee or host-kernel
  OOM verification. Six short allocator diagnostics and their caveats are in
  [the report](docs/benchmarks/2026-10-30-v013-memory.md).
- **Larger synthetic load:** 100,000 nodes with repeated 4,096-byte payloads
  completed in a 4 GiB container, then idled for 60 seconds. Peak process RSS
  was 223.1 MiB and final idle RSS was 194.8 MiB; count and offline audit passed.
  The fixture is highly compressible and does not qualify sustained production
  load or predict realistic throughput.
- **Diagnostic fix:** the first allocator run was inconclusive because the
  parser missed jemalloc's actual log prefix. Fixed and reviewed the parser;
  eight tests and its self-check passed, followed by six passing reruns. This
  harness fix and the report were made after the immutable release tag.
- **Social preview:** added a compact social card under 1 MB and linked it from
  the README. GitHub only shares a repository preview publicly after the repo
  itself is public; the repository remains private.
- **Python:** installing the package from a clean tagged checkout into a new
  virtual environment succeeded; the installed package passed the documented
  node/edge upsert, Cypher and streaming examples against a temporary container.
- **Next:** keep the release experimental. Qualify real workloads, sustained
  memory, TLS, distributed recovery and host-specific secrets separately. The
  private archive stays private; public publication needs a separate review.

## 2026-10-30: v0.13 beta candidate — ingress and onboarding

- **Done:** account Bolt assembled-buffer capacity and its old/new overlap in
  the shared ingress pool. Recovery remains bounded; allocator rounding and
  transient over-allocation are not a hard RSS ceiling. Added six regressions.
- **Done:** `brainwire auth init` creates private graph-scoped admin and health
  credentials from the OS random source without printing them. Existing paths
  are refused. Policy changes require a restart; this is not online user
  management. Reviewed the security and memory changes independently.
- **Done:** refreshed the README, release-image and memory guides; documented a
  fail-closed fresh protected Linux Docker setup and created a separate social
  card. Added image-user credential setup to the release workflow.
- **Checks:** workspace tests: 947 passed and three ignored; Clippy and format
  passed. The synthetic concurrent query-memory test passed and checked refusal,
  rollback and release of reservations. Authentication was exercised with a
  native server and a client; the health identity could not write.
- **Pending:** container image build, image-user credential setup, Docker
  pressure/recovery smoke, CI and release publication. These checks do not prove
  sustained 2/4/8 GiB qualification, a global RSS cap, native Bolt TLS or
  multi-machine failover. Keep the repository and historical archive private.
- **Next:** publish the explicitly experimental candidate only after its gates,
  then record the workflow outcome and image identity separately.

## 2026-10-30: v0.12 experimental beta published

- **Release:** `v0.12.0-beta.1` points to sanitized source
  `a246714da781757b044cacb7a038907f7264f03f`. The release workflow completed
  successfully, including workspace checks, volume upgrade, native amd64 and
  arm64 image tests and manifest publication. The matching release's
  `release-metadata.txt` records the source and tested image digests.
- **History:** the original 13 version tags and nine release records/assets
  were preserved and verified in the separate private history archive, then
  removed from the primary remote. Thirteen `clean/v*` aliases and the new beta
  tag remain. An independent scan of a fresh remote clone found no configured
  old identity/term in reachable refs, objects or paths, and no credential
  candidates for the selected patterns. This is not proof of total erasure:
  caches, external clones, other local branches and private packages require
  separate review before making the project public.
- **Checks:** 938 Rust tests passed with three ignored; Clippy and formatting
  passed. Python acceptance passed with driver 5.28.8 and 6.4.1, including
  streaming reads, read-only enforcement, rollback and process-kill recovery.
  These are synthetic checks, not a power-loss or production-load guarantee.
- **Open:** sampled admission is not a hard RSS ceiling; sustained 2/4/8 GiB,
  large-load qualification, production deployment and a comparable benchmark
  remain outstanding. Native Bolt TLS is not available. Both repositories
  remain private; review old package visibility and GitHub cached references
  before any public announcement.
- **Next:** run the release image with the documented protected setup or the
  local synthetic demo; qualify memory, failover, real-source workloads and
  public distribution separately.

## 2026-10-30: sanitized-history release preparation

- **Done:** preserved a verified private Git bundle, worktree patch, untracked
  files, release metadata and nine release assets. Built an isolated sanitized
  history; its current tree is unchanged, `git fsck` passed, and independent
  scanning found no configured private identifiers in reachable objects or
  paths. Compared 203 rewritten commits and found only planned replacements.
- **Tags and CI:** prepared 13 local `clean/v*` aliases; CI and the release
  helper recognize these aliases without firing the release workflow for them.
  Both use first-parent selection. Regression tests and actionlint passed.
- **Pending:** user choice on preservation of historical releases and tags,
  remote archive and any destructive reference updates. No historical remote
  tag has been changed. Re-run CI for the final sanitized commit before main
  and the v0.12 beta tag. A local rewrite cannot remove old clones or caches.

## 2026-10-30: experimental beta, memory and publication review

- **Done:** sample process RSS before the first admission decision, avoiding the
  startup zero-usage window; unavailable samples remain best-effort with a warning.
  Added read-only `GraphClient.stream_cypher`, which closes its cursor and session
  and reports late server errors. Updated the README and concept artwork, and
  added a redacted, scoped publication-audit helper.
- **Verification:** Rust workspace 938 passed, 3 ignored; Clippy and formatting
  passed. Python examples ran 103 tests, 6 skipped; a real synthetic Bolt test
  checked streamed reads, early exit, read-only enforcement and recovery. The
  publication audit's focused tests passed; it is heuristic, not a full scan.
- **Release:** the first v0.12 candidate commit passed CI. The user explicitly
  chose an experimental beta without the sustained 2/4/8 GiB qualification;
  memory, transport and recovery limitations remain documented.
- **History:** Allan requested a history rewrite to remove old private identity
  and client references. A verified private backup of all refs and uncommitted
  work is in `/private/tmp/brainwire-history-backup-20261030`. Historical
  tags and release assets need an agreed preservation plan before remote refs
  change; published image provenance must not be silently misrepresented.
- **Next:** complete and independently audit the isolated history rewrite,
  resolve historic releases, re-run CI for the exact sanitized commit, then
  update main and publish the new beta tag. Keep the repository private until
  history and external references have been reviewed.

## 2026-10-30: experimental v0.12 release preparation

- **Decision:** Allan explicitly chose to publish the experimental beta without
  waiting for sustained 2/4/8 GiB qualification. Production readiness, a hard
  RSS ceiling and the original load targets remain open and are documented.
- **Done:** reproduced and fixed an authenticated Bolt ingress-decode refusal
  that could prevent RESET under shared-pool contention; the regression failed
  before the fix and passed afterward. All 135 Bolt library tests passed. Added
  v0.12 release notes and prepared versioned manifests and deployment templates.
- **Verification:** the earlier candidate passed 936 workspace tests (three
  ignored), Clippy and formatting; Python client, release-script and soak
  harness unit tests passed. Documentation and local-link checks passed.
  The exact versioned release commit still requires CI before a tag.
- **Next step:** push the release branch, verify CI for its exact commit, then
  fast-forward main, tag and verify the release workflow and image manifest.
  Linux memory soak, power loss, downgrade and production qualification remain
  open; do not claim they passed. Local disk space limits rebuilding all Rust
  targets safely without affecting other workloads.

## 2026-10-30: Docker memory-pressure smoke on earlier image

- **Done:** ran the owned synthetic `cgroup_pressure_smoke.py` test with a
  512 MiB cgroup limit against existing image
  `sha256:3bd48e84a47a23f7f25e1596e94c3e9ba3d1ebe5bfe6cfa3086e8a21741da9e8`.
  The server refused work under pressure, rolled back pending writes, preserved
  an acknowledged node, recovered after relief and passed the offline check.
  Cgroup OOM counters remained zero and the test removed its own resources.
- **Receipt:** `/private/tmp/bw-container-smoke-20261030.json`. This earlier
  image predates the current candidate; the diagnostic does not qualify v0.12,
  a hard RSS ceiling or sustained 2/4/8 GiB behavior. Kernel logs were not
  independently verified.
- **Blockers / next:** only 9.4 GiB disk space was available before this run;
  do not risk a full Rust/RocksDB image build or delete other workloads' data.
  Free enough disk, build and identify the exact candidate image, then run
  the required isolated loads and ingress RESET regression before release.

## 2026-10-30: lightweight memory diagnostic

- **Done:** ran `query_memory.py` with the candidate native debug binary and a
  temporary synthetic graph. With a 256 KiB shared query budget and 16
  connections, 14 requests were rejected; reserved bytes fell from 162,703
  during the test to 177 afterward. Disconnect recovery, deadline rollback,
  index checks and offline audits passed.
- **Limit:** this exercises query reservations, not process RSS or an 8 GiB
  Docker cgroup. The Docker VM had 33 running containers, so no sustained
  multi-GiB load was added to their host. The v0.12 release gates remain open.
- **Next step:** run the existing container qualification on a dedicated or
  otherwise isolated Docker host and investigate the ingress RESET finding.

## 2026-10-30: v0.12 streaming and qualification (in progress)

- **Done:** fixed finite Bolt DISCARD accounting and preserved unlimited cancellation;
  return RUN fields before the first page for eligible parallel scans; account
  request-status parameters and retain its output reservation until delivery.
  Added typed Python errors and a request-status helper without automatic retry.
- **Verification:** the integrated workspace suite passed 936 tests with 3 ignored;
  Python examples ran 98 tests with 6 skipped; 74 script tests passed, and
  local docs and site checks passed. The LIMIT regression covers
  early termination, ACL and snapshot behavior. RSS tests now run in isolated
  subprocesses; 30 repeated module runs and 156 server unit tests passed.
  Review confirmed remaining partial-streaming and cancellation limitations.
- **Local acceptance:** rebuilt the candidate binary; a synthetic Python/Bolt
  ingestion, rollback, index and SIGKILL/reopen check passed. A separate
  unacknowledged-COMMIT SIGKILL exercise passed with both committed and
  not-committed outcomes. Neither exercise simulates power loss. Updated the
  ingestion test to force a blocking operator instead of discarding a lazy
  result before the intended memory-pressure test.
- **Qualification tooling:** a Linux-only synthetic soak harness prepares 2/4/8
  GiB, eight-writer/four-reader runs, optional 4M initial rows, kernel/cgroup
  OOM evidence, checks and scoped cleanup. Its unit tests pass; no soak was run.
  Power, isolation and kernel evidence are fail-closed and sampled, not continuous.
- **Blockers:** no sustained qualifying runs, independently verified peak/idle
  RSS fix, mixed-workload performance, real old-image downgrade or multi-arch CI
  release. The local host has other running containers and lacks safe isolation;
  the private repo standard runner lacks headroom for the 8 GiB test. The GHCR
  credential currently lacks read:packages. Do not claim a hard RSS ceiling.
- **Review follow-up:** a possible Bolt recovery gap was identified when
  decoding exceeds the ingress pool: unlike chunk-assembly refusal, the decode
  path may not enable the recovery-message allowance. This is a code-reading
  finding, not a reproduced failure; add a deterministic concurrent-pool RESET
  test and fix if confirmed before release. No runtime change was made for it.
- **Next step:** review the complete working diff, obtain an isolated Linux host
  with sufficient memory and verifiable conditions, run the qualifying workloads
  and mixed-performance/downgrade checks, fix any failures, repeat gates, then
  publish the requested release only when its stated acceptance criteria pass.
  No v0.12 push, tag or release has been made.

## 2026-10-29: v0.12 release review (in progress)

- **Done:** preserved the incoming complete patch in `/tmp/bw-pre-v012.patch` and
  created local branch `release/v0.12`. Reviewed the memory, Bolt and release
  changes. The multi-architecture workflow and release-gate documentation have
  separate local commits; they have not been pushed.
- **Corrections:** avoid retaining the databases forever in the memory sampler;
  invoke jemalloc's void purge correctly and check its result; obtain native RSS
  on macOS; prevent the same sampled headroom being spent repeatedly; page Bolt
  PULL responses on the socket. Admission remains conservative, not a hard RSS
  ceiling. Lazy output leases now follow pages through the response batch; further concurrent socket and Linux qualification is required.
- **Verification:** the final workspace suite, Clippy, format, site/link checks,
  90 Python tests (six skipped), 73 script tests and multi-architecture workflow
  tests passed. The
  purge and macOS RSS regressions failed before their fixes and passed after.
  A synthetic standalone crash/recovery exercise passed. Later changes require
  another full integrated run.
- **Experimental integration:** authenticated opt-in standalone writes and
  scoped status use the atomic receipt. Synthetic Bolt tests cover unknown
  outcome, quarantine, reconnect and reopen. Restore/fork, downgrade, power
  loss and real application use remain unqualified.
- **Open gates:** real process-wide memory qualification including external
  cgroup pressure, repeat peak/idle RSS
  measurements, fully lazy parallel entity results and spill/error qualification.
  The externally supplied measurements were not independently reproduced. The
  4 GiB pressure run is deferred because other Docker workloads are active and
  host memory headroom is insufficient for safe, isolated measurement.
  See `PRODUCTION-v0.12.md`.
- **Release:** no v0.12 tag, push or image until all mandatory gates pass;
  then run CI against the release branch before merging/tagging and verify the
  multi-architecture manifest and per-platform digests.

## 2026-10-29: memory admission, documentation and logo

- **Done:** add cooperative Linux cgroup pressure admission before RUN, with a
  stable transient error and rollback of pending work. It is not a hard RSS cap;
  unavailable or hidden cgroups and in-flight/native allocations remain risks.
- **Measured:** a 512 MiB synthetic Docker smoke rejected two RUN requests,
  rolled back pending work, preserved committed data and recovered after relief.
  Initial and repeat receipts are in `docs/benchmarks/2026-10-29-cgroup-pressure/`.
- **Benchmarks:** fix the comparison runner's development authentication and
  record a new three-system synthetic run and chart. Different protocols and
  durability settings, a dirty Brainwire source tree and one run limit claims.
- **Documentation and identity:** simplify core setup, Docker, CLI, TUI, Cypher
  and Python guides, add a docs index and link checker, update the docs site,
  and replace the logo with a green graph monogram. Private Pages remains
  disabled; the workflow prepares an artifact for later private review.
- **Verification:** full workspace tests, all-target Clippy and formatting passed;
  Python examples passed 90 tests (six skipped), scripts 67, and comparison
  runner three. The local Docker quickstart and Python example worked. Site
  validation and 635 local inline Markdown links passed (external URLs and
  fragments were not checked).
- **Next step:** verify sustained 2/4/8 GiB production-like workloads, implement
  a comprehensive process budget and true streaming, and continue the open
  v2 goals and review of the remaining historical documents.
- **Blockers:** the memory ceiling and v2 goals remain incomplete. No push or
  tag until the requested items are completed and verified.

## 2026-10-29: v1 memory diagnostics and Cypher regex

- **Done:** add an advisory configured-memory summary to `config show` and
  a startup warning for overcommitted/overflowing configured budgets. Count
  memtables within cache; retain detected source and never block offline
  recovery or change explicit overrides.
- **Cypher:** support a bounded Rust-regex subset of `=~`, including whole-string
  matching, nulls and a per-RUN/worker cache. Validate the pattern AST before
  anchoring and reject unsupported Java features. The cache is charged to the
  query budget and is dropped if the pattern changes. See `docs/cypher-regex.md`.
- **Verification:** regression tests and Clippy passed for Cypher and server;
  the real config command printed its summary and a deliberately oversized
  configuration warned before failing on its missing synthetic auth file.
- **Measured:** in a narrow 10,000-row developer-build synthetic diagnostic,
  median elapsed fell from 2,620.415 ms to 17.211 ms after caching; three
  samples, no p99 or general performance claim. Raw logs are in
  `docs/benchmarks/2026-10-29-regex/`.
- **Workspace verification:** full Rust workspace tests were interrupted while
  compiling because free disk fell from about 3 GiB to 1.1 GiB; no result is
  claimed. Focused Cypher and server tests, Clippy and formatting passed.
- **Next step:** complete integrated workspace verification after freeing disk, add representative
  varied-pattern and concurrent benchmarks, build real incremental query output,
  enforce process-wide memory safety, and qualify sustained 2/4/8 GiB runs.
- **Blockers:** the diagnostic is not an RSS limit; regex differs from Java,
  deadlines are cooperative and changing patterns still recompile. The v1
  prerequisite and 44 v2 acceptance goals remain open. No push or tag until
  those goals are completed and verified.

## 2026-10-29: memory, packaging and documentation work

- **Done:** replaced full Bolt response encoding buffers with bounded framed
  output; added a per-database estimated 64 MiB cap for retained conflict
  history with fail-closed pruning; added a locally installable Python helper
  using the existing Neo4j driver and release version synchronization.
- **UX:** refreshed the README and concept artwork, documented local Docker
  bootstrap and proposed Homebrew and desktop delivery. These are designs,
  not a published Homebrew package or Electron application.
- **Verification:** Bolt and store integration tests, their all-target Clippy
  and formatting passed; workspace server built. Python examples ran 90 tests
  with 6 skips; release helper ran 9 tests. A synthetic Docker bootstrap used
  an isolated v0.9 image/volume. The wheel was built and installed outside the
  source tree in the isolated packaging checkout.
- **Measured:** synthetic response comparison receipts and limits are in
  `docs/benchmarks/2026-10-29-bolt-response/`; results are variable and do not
  establish the 5% regression gate. Conflict deque diagnostic is recorded in
  `docs/benchmarks/2026-10-29-history-growth.md`.
- **Next step:** expand representative many-small-value and concurrent workload
  benchmarks; finish true executor streaming, RSS accounting, standalone commit
  outcome ledger, and the 2/4/8 GiB acceptance runs. Validate the one-line
  Docker bootstrap against a published v0.10 image.
- **Blockers:** process RSS is not globally capped; 30-minute stability,
  production TLS, commit reconciliation and full Cypher parity remain open.
  Desktop/Homebrew/PyPI distribution and real DuckLake validation remain open.

## 2026-10-29: release preparation and v2 roadmap

- **Done:** prepared the prior production-hardening work for a new beta; added
  a v2 roadmap and a benchmark entry point with explicit qualification limits.
- **Decided:** v1 production gates precede v2 claims. This is a beta candidate,
  not v1 qualification; numeric index ranges can fall back to scans.
- **Next step:** validate the release CI and repeat controlled mixed workloads,
  then close the RSS, true streaming, TLS, recovery and Cypher gates.
- **Blockers:** prior 2/4/8 GiB loads did not complete, and no benchmark proves
  the requested performance regression bound or end-to-end production safety.

## 2026-10-28: production hardening session

- **Done:** added shared Bolt input admission and reduced response-copy overhead;
  fixed missing-schema read semantics, property/ACL filtering, rounding and
  index range correctness. Accounted `IN` candidates and reduced retained
  projection data. Python write helpers and ingestion paths no longer replay
  managed transactions blindly after an uncertain commit.
- **Recovery:** added owned-process no-ack COMMIT reconciliation and populated
  v0.9-to-candidate volume-upgrade tests, with CI steps. The integrated test
  observed both committed and uncommitted outcomes and passed offline checks.
- **Verification:** workspace tests, Clippy and formatting passed; Python
  examples and scripts tests passed with optional DuckDB-dependent tests
  skipped. Read-only architecture and security reviews were performed.
- **Measured:** four-label 4 GiB baseline and candidate aborted early under
  query pool pressure; see `docs/benchmarks/2026-10-28-production-session.md`.
  No production stability or throughput improvement is established.
- **Next step:** genuine process-wide memory bounds, streaming results,
  visibility/alerts, TLS and online principals; qualify real DuckLake changes,
  run sustained representative loads and validate the new CI workflow remotely.
- **Blockers:** RSS/OOM and 30-minute 2/4/8 GiB gates remain open. The DuckLake
  lease is cooperative, not fencing; unknown commits require reconciliation.

## 2026-10-28: output reservation lifetime

- **Done:** keep query-memory reservations alive until Bolt response batches are
  sent or discarded. Added a four-label concurrent container diagnostic.
- **Verification:** workspace tests, Clippy, formatting, benchmark unit tests and
  a small ACL smoke passed. Before/after 2/4/8 GiB attempts ended early under
  query-pool pressure or reader timeouts; see `PRODUCTION-v0.9.md`.
- **Decided:** these results do not qualify production or establish an RSS cap.
- **Next step:** bound ingress, output encoding and native allocations, implement
  real streaming/backpressure, then repeat representative 30-minute workloads.
- **Blockers:** global memory enforcement, native Bolt TLS, online principals,
  Cypher compatibility, recovery and production qualification remain open.

## 2026-10-28: production memory groundwork

- **Done:** added cgroup/host-aware defaults, a disposable container benchmark,
  and cooperative Bolt client transaction deadlines. Compared 1M synthetic nodes
  before and after each change; recovery and `db check` passed. See
  `PRODUCTION-v0.8.md`.
- **Verification:** workspace tests, Clippy with warnings denied, formatting,
  13 benchmark unit tests, a real Python 0.5 s timeout/rollback test, and an
  empty v0.7 volume check passed.
- **Decided:** derived budgets are not an RSS ceiling; do not claim production
  readiness or infer security/replication from the single-node diagnostic.
- **Next step:** implement global admission/lifetime accounting and reproduce the
  higher-memory multi-label workload; qualify timeouts under blocking I/O and TLS.
- **Blockers:** full memory cap, native Bolt TLS, online principals and the other
  production requirements remain open; populated-volume upgrade unverified.

## Phases

1. Brainstorming (requirements, approaches, design section by section).
2. Spec in `docs/superpowers/specs/`, reviewed by Allan.
3. Implementation plan broken into tasks (skill `superpowers:writing-plans`).
4. Implementation: tasks from the plan, with TDD, several per session through
   parallel agents.

No line of code before the end of phase 2.

## Entries

### 2026-10-29 — memory ceiling, one memory refusal, lazy scalar results (uncommitted)

- **Done (working tree only, on top of the uncommitted iterations 1-2):** jemalloc
  as process allocator with background decay; `memory.limit` sampled admission threshold
  (default 90% of detected memory; not a hard RSS ceiling) checked by every query-pool reservation and at
  RUN admission (`docs/design/process-memory-ceiling.md`); one memory refusal code
  `MemoryPressure` with a `reason`/size hint, sessions that survive an input
  refusal, COMMIT errors that say rolled back vs unknown
  (`docs/design/memory-error-contract.md`); lazy production of scalar read
  results on a producer thread (`docs/design/lazy-executor.md`).
- **Externally reported, not independently verified:** 30-min workload (8 writers + 4 readers, 2M nodes) at 2/4/8 GiB
  and a 4M-node run at 8 GiB: no kernel kill, `db check` ok. The previous tree at
  2 GiB held 1.96 GiB RSS and refused 5,641 RUNs in 10 min; this one peaked at
  1.18 GiB with no refusal. The external report is not bundled and this run was not reproduced here.
- **Open:** standalone outcome lookup by request id, spilling, label counters,
  the P1 Cypher list, P2 operations. Not a production release.
- **Next step:** review the diff, commit in coherent parts, release a beta.

### 2026-10-27 — v0.7.0-beta.1 local candidate

- **Done:** prepared a local beta version for the TUI, ACL read-path, query
  memory accounting and benchmark improvements recorded below.
- **Measured:** the 1M-visible-node Bolt ACL cursor took 26.74 s in one
  synthetic run; restricted count and top-N missed their targets. Both before
  and after fixtures passed offline checks; see `PERF-v0.7.md`.
- **Limits:** a hard 4 GiB process RSS ceiling, the full concurrent acceptance
  workload, additional Cypher features, server batch procedures and an
  independent Python client remain open. This is not a production release.
- **Next step:** finish those items and repeat qualification before publishing.

### 2026-10-27 — v0.7 read and memory work

- **Done:** created local `perf/v0.7-reads`; integrated delegated read fast
  paths while preserving procedure-level visibility restrictions. Improved
  parallel worker accounting, optional cache admission and property preflight.
- **Verified:** ACL tests, the full Rust workspace, Clippy and Rustfmt passed
  after integration. The connected TUI integration test was updated and passed.
  Benchmark tools passed synthetic unit tests and a live 16-node Bolt smoke.
- **Measured:** embedded 4M-node/4M-edge before/after diagnostics and a frozen
  Bolt ACL fixture; both offline checks returned `ok`. See `PERF-v0.7.md` for
  exact numbers, host conditions and missing targets. Restricted count and
  top-N remain above their targets; a 1M-visible-row ACL cursor is unmeasured.
- **Limits:** memory accounting remains an estimate, not a 4 GiB RSS ceiling;
  full concurrent qualification, requested Cypher, batch and Python client
  work remain open. No new tag, push or public release has been made.
- **Next step:** safely optimize restricted ACL metadata reads, repeat Bolt
  measurements and implement the remaining v0.7 priorities before tagging.

### 2026-10-27 — README and documentation refresh

- **Done:** replaced the schema hero with a synthetic entity graph illustration;
  updated the README and documentation site to describe current capabilities,
  TUI limits, Python batch commits, security, benchmarks and release status.
  Corrected stale connected-graph wording in the TUI guide.
- **Verified:** docs-site link/asset check, Markdown local links, SVG parsing and
  eight release-script unit tests. The v0.6.0-beta.2 GitHub CI and release runs
  failed on Rust formatting; no GitHub release exists for that tag. The formatting
  correction is part of the local connected-graph changes.
- **Next step:** commit the local changes and run CI on the resulting commit;
  publish a new tag only after the release gates succeed.

### 2026-10-27 — Connected graph visualization

- **Done:** redesigned the connected Graph view around actual sampled source and
  target IDs, directed edges, selected relationship details, bounded target
  navigation, duplicate multiplicity and self-links. The workspace gives Graph
  more space on 120-column terminals. Added a synthetic renderer preview.
- **Verified:** 58 TUI tests and TUI Clippy passed; reviewed by Astra. The
  rendering uses the existing read-only server contract, which was not changed.
- **Limits:** one configured source and outgoing relationship type, up to 20
  rows; no interactive expansion, arbitrary Cypher or node/edge properties.
- **Next step:** add a bounded, ACL-aware graph exploration contract before
  implementing interactive expansion and richer properties.

### 2026-10-27 — Public readiness, security and TUI (v0.6.0-beta.2)

- **Done:** simplified TUI navigation, refreshed the logo and ontology diagram,
  tightened remote listener defaults and delegated visibility, documented
  temporal data limits, and prepared authenticated deployment profiles.
- **Verified:** 54 TUI tests, release unit tests, documentation and deployment
  profile checks passed. The full workspace test did not finish: the Linux
  build ran out of memory and a serial rebuild was interrupted. The Bolt ACL
  acceptance script was syntax checked but not run end to end.
- **Decided:** this beta is a candidate pending CI, not a production qualification.
  The existing repository remains private while the publication path is decided.
- **Next step:** inspect CI for this exact tag, run Bolt ACL acceptance and
  complete the full workspace checks. Preserve private history and local data
  when preparing a public repository.

### 2026-10-27 — Brainwire as a primary read/write backend (v0.6.0-beta.1)

- **Requested (Allan):** turn the evaluation of Brainwire as a platform's
  primary graph backend into engine work: read speed, the Cypher agents
  write, online catalog, false conflicts, write throughput, ACL size, error
  classes and the protected healthcheck; then cut a new version. Work stays
  local on an integration branch; nothing pushed, `main` untouched.
- **Done:** reproducible benchmark (`crates/cypher/benches/baseline.rs`);
  new planner and streaming executor (multi-hop, variable length, OPTIONAL
  MATCH, WITH, UNION, CASE, comprehensions, functions, aggregates, RETURN of
  relationships and paths); index and primary-key access for `=`/`IN`/ranges/
  `STARTS WITH`; ordered cursor pages; scan-local property reads; parallel
  label scans; exact conflict history for active writers; bounded snapshot
  iterators (the collapse beyond 4 writers); MERGE+SET fusion; online
  catalog, ACL lists up to 5000, client error classes and the handshake
  healthcheck (the last three items and the catalog were done by delegated
  agents on separate branches and cherry-picked). `PERF.md` has the
  before/after table; `crates/server/tests/differential.py` compares 92 read
  queries with Neo4j 5 (all identical); `bolt_read_bench.py` measures both
  servers over Bolt.
- **Verified:** workspace 694 tests, clippy and fmt clean; differential 92/92
  with `db check` ok; benchmark load + reads + writes with `db check` ok and 0
  conflict retries from 1 to 16 sessions.
- **Decided:** a release tag is created locally on this branch (the release
  script only applies on `main`, which this work must not touch); publishing
  and merging into `main` wait for Allan.
- **Next step:** with Allan's approval, fast-forward `main`, push and publish
  the image; then label counters for `count(n)`, group commit, and
  `CALL {}`/`EXISTS {}` subqueries if agents need them.

### 2026-10-27 — online catalog changes (branch `codex/online-catalog`)

- **Requested:** make catalog changes work while the server runs, through
  Cypher, instead of the offline `db apply-ontology` path with its downtime.
- **Done:** `CREATE INDEX` / `DROP INDEX` (named, legacy, `IF [NOT] EXISTS`),
  `db.createLabel`, `db.createRelationshipType` and `db.dropProperty`, in the
  new `brainwire_store::catalog` module. A node type now records index names,
  a `populating` list and retired column ids. Writers maintain a populating
  index at once; `Database::index_step` backfills 1,024 nodes per transaction
  with its cursor in an `index-build/` meta key (resumable, never rewrites the
  catalog until the final batch flips the index online). Drops remove the
  declaration at once and delete keys through `index-drop/` jobs. Queries,
  `db check` and the planner only use online indexes. A standalone server runs
  one weak-referenced worker thread per database. `db.indexes()` gains
  `populating` and a `name` column. Dropped property values stay hidden under
  a retired column id that is never reused and are purged on the next write.
  Replicated transactions refuse catalog changes. Docs:
  `docs/online-catalog.md`.
- **Verified:** core 2, store 6, Cypher 7 and one Bolt server test written for
  the new behavior; a mutation check confirmed the readiness gate is covered.
  Workspace 686 tests, clippy and fmt clean (container, debug).
- **Next step:** replicated DDL (the backfill would need to go through the
  log), and `SHOW INDEXES` if clients ask for it.

### 2026-10-26 (cont.) — batch ACL stamping and a multi-source lake workload

- **Requested:** keep working; simulate how a platform client would insert data.
- **Done:** read the shape of a real platform write path (mapped entity tables
  with external IDs and ACL columns, FK edges, physical deletes, concurrent
  sources, tenant-restricted readers) and reproduced it synthetically in
  `crates/server/tests/lake_workload.py`. New `db.setNodeAcls($rows)` stamps up
  to 10,000 node ACLs in one call, validating and resolving every entry before
  writing. `GraphClient.load_nodes(..., with_acl=True)` merges properties and
  stamps the batch's ACLs in one transaction; `load_edges(...,
  require_endpoints=False)` skips edges with missing endpoints. DuckLake sync
  maps `acl_*` columns through an `acl` block and has
  `--allow-missing-endpoints`. Review fixes: JSON-lines spool for prune keys
  (empty strings stay distinct from NULL), reserved change-feed column names,
  a failed lease release no longer hides the original error, no cursor
  regression, spawn-based worker pool, no DuckDB progress bar on stdout.
- **Verified:** 2 new Rust ACL tests and 4 new Python tests written first.
  Workload at scale 10,000 (debug, container): 100K nodes with ACL in 4.7 s,
  90K edges in 2.0 s, 3 change cycles of ~12K node and ~10K edge changes in
  ~2.5 s each, 420 restricted reads during writes with 0 errors and 0
  cross-tenant rows, graph equal to the lake, per-tenant visibility exact,
  `db check` ok. Python example suite 75 tests ok. The same workload (scale
  1,000) passed against a DuckLake with a Postgres 16 catalog and Parquet in a
  MinIO bucket (throwaway containers, credentials only in env vars; 31 objects
  written): graph equal to the lake, 441 restricted reads, 0 leaks.
  `create_service_tokens --acl-writer` makes the writer an instance admin for
  ACL stamping; `docs/parallel-ingestion.md` has a step-by-step test run.
  Release build, scale 100,000: 1M nodes with ACL in 12.9 s, 900K edges in
  6.1 s, change cycles of ~120K node and ~100K edge changes in ~4.5 s and
  ~2 s, 2,464 restricted reads (p95 30 ms) with 0 errors and 0 leaks, graph
  equal to the lake, `db check` ok.
- **Next step:** release the new write path (needs Allan's go-ahead) so a test
  agent can use the published image; then stream prune's key listing.
- **Blockers:** none. No push or release requested.

### 2026-10-26 — parallel writers, DuckLake sync, relationship property reads

- **Requested:** about an hour making the write path ready for an external
  agent to load data: many simultaneous writers (10 Python processes), inserts,
  updates and deletes, reading directly from DuckLake, incremental runs, and
  guidance on batches versus rows and on parallelism.
- **Done:** `crates/server/tests/concurrent_writers.py` drives 10 processes
  through disjoint upserts, hot overlapping updates, concurrent edge MERGE,
  DETACH DELETE and re-upserts, then audits exact final state and `db check`.
  New `examples/python/ducklake_sync.py` mirrors one DuckLake table into a
  label or edge type: full load at a pinned snapshot, then
  `ducklake_table_changes` collapsed per key in DuckDB (last snapshot wins;
  post-image beats pre-image; key and endpoint changes become delete + upsert),
  split over N processes by `hash(key) % N`, with an `ImportCheckpoint`
  snapshot cursor and lease advanced only after every partition commits.
  `--full --prune` reconciles: it anti-joins the graph's keys against the
  source snapshot in DuckDB and deletes the stale ones. Cypher now reads relationship properties (`RETURN r.weight`,
  `WHERE r.weight > x`), charged to the query memory guard, so a writer can
  verify edges it loaded. New guide `docs/parallel-ingestion.md`; README and
  related docs link it.
- **Verified:** 3 new Rust tests (edge property reads, staged reads, memory
  guard) written first and failing for the right reason; the delete-edges test
  no longer expects `r.weight` to fail. 8 DuckLake sync unit tests on a real
  local DuckLake; 69 Python example tests pass in the container. Real server,
  debug build: concurrent writers 51K rows/s disjoint, 26 transient conflicts
  all retried, exact state, row-by-row ~90x slower than batches; DuckLake sync
  of 100K nodes 2.8 s and 100K edges 3.7 s, incremental 20K changes 5.0 s with
  11 retried conflicts, graph equal to the lake, lease and `--break-lease`
  checked; `--full --prune` removed exactly the injected stray nodes/edges;
  `db check` ok. Workspace tests/clippy/fmt: see below.
- **Decided:** batches of 250–1000 rows, partition by key hash, rely on the
  driver's transient retry; `--full` never deletes; one run per source through
  the lease. Not released: no push or tag requested this session.
- **Next step:** release-build numbers for the DuckLake sync at 1M rows, then
  stream prune's key listing in pages so it does not depend on `memory.query`.
- **Blockers:** none. The build container was recreated (rust:1.98.1-bookworm,
  `CARGO_TARGET_DIR=/target` volume, duckdb + ducklake installed for tests).

### 2026-10-23 (late) — checkpoint copy to independent storage, beta release

- **Requested:** a scheduled ~45-minute session starting 23:40 CEST on features
  and improvements, then push and release following `docs/releasing.md`
  (explicitly authorized for this run). The one-shot schedule was deleted first.
- **Done:** `GraphClient.checkpoint(name)` runs `CALL db.checkpoint` and returns
  a receipt with graph, path, sequence, tracked and ontology version, validating
  the name before connecting. New `examples/python/checkpoint_backup.py`
  copies a checkpoint to another location through `DEST.partial`, fsyncs files
  and directories, renames, and writes a SHA-256 `brainwire-manifest.json`;
  `verify` reports missing/extra/changed files and exits 1 on problems. It
  refuses existing destinations, symlinks and receipt/source name mismatches.
  Documented in `docs/checkpoints.md` and `docs/python-client.md`.
- **Verified:** 6 new Python tests written first; 62 Python client tests and the
  release-helper tests pass; docs-site validation and `git diff --check` pass.
  Real Bolt run (debug binary, container): 100K synthetic nodes loaded in 3.9 s,
  online checkpoint 0.24 s, copy of 9 files / 3.1 MB 0.02 s, verify < 0.01 s,
  restore copy plus full `db check` 2.9 s; restored 100,000 nodes versus 100,001
  live after a later write. One local run, not a benchmark.
- **Decided:** keep the copy tool as a Python example rather than a server
  feature; no upload, encryption or retention. Release as `v0.5.0-beta.1`.
- **Released:** `scripts/release.py --apply --prerelease beta` ran every gate
  in the container and created `011f4b9` with annotated tag `v0.5.0-beta.1`.
  Pushed with `git push origin main --follow-tags`. GitHub CI (21m17s),
  Documentation site and Release (20m44s) succeeded. Release published
  prerelease `v0.5.0-beta.1` and image
  `ghcr.io/allandefraga/brainwire:v0.5.0-beta.1`
  (`sha256:3b6a0918def13ca87b68efbf2418ede97e237b0901ba3e4ad2829433536d8207`,
  source `011f4b9`) after its container recovery test.
- **Next step:** a retention/prune command for `checkpoint_dir` (keep N newest,
  never delete the newest verified copy), and a restore helper that strips the
  manifest and runs `db check`.
- **Blockers:** none. Host Cargo unavailable; release gates run in the container.

### 2026-10-23 — online admin checkpoints over Bolt

- **Requested:** continue development with one verifiable, high-value
  improvement; keep the work local (no push, tag or release).
- **Done:** checkpoints previously required stopping the server. Added
  admin-only `CALL db.checkpoint(name) YIELD name, path, sequence, tracked`,
  enabled only when the new `server.checkpoint_dir`
  (`BRAINWIRE_SERVER_CHECKPOINT_DIR`) is set. It reuses the native RocksDB
  checkpoint that blocks commits during capture, writes to
  `<checkpoint_dir>/<name>` (plain names only, no caller paths), refuses
  existing names, must be the first clause without writes or other procedures,
  and is refused for replicated serving and visibility-restricted principals.
  Config rejects a root lexically inside or containing `data_dir`. Documented
  in `docs/checkpoints.md`, `docs/serve.md`, the operator checklist and the
  docs-site checkpoint card.
- **Verified:** tests were written first (3 Cypher integration, 1 engine, 1
  config). 659 workspace tests (`--no-fail-fast`; the existing
  every-key env test was extended to the new key), denied-warning Clippy and fmt
  pass in the `rust:1.98.1-bookworm` container with `-j 2` (the default
  parallel link was OOM-killed). The release helper's Bolt acceptance
  (`python_ingestion.py`, 100 rows) passed. Manual Bolt run with the real binary and neo4j 5.28.4: 500
  synthetic nodes, online checkpoint, a later write, invalid and duplicate names
  refused; after SIGTERM, the checkpoint moved into `data_dir`, `db check`
  returned `ok` and counted 500 nodes while the live graph had 501. Docs-site
  validation and `git diff --check` pass. No remote CI ran.
- **Decided:** keep restore offline and per graph; no retention, upload or
  multi-graph atomic snapshot. A side effect cannot be rolled back.
- **Next step:** add a Python client helper and an operator script that copies
  a named checkpoint to independent storage with a manifest (graph, sequence,
  ontology version), then time a restore of a 100K-node checkpoint.
- **Blockers:** none. Host Cargo still unavailable.

### 2026-10-22 — bounded resumable source ingestion and crash verification

- **Requested:** use this hour to improve durable writes and failure recovery,
  then commit, push to `origin/main`, and prepare a release.
- **Done:** added `max_batches` to the shared resumable JSONL importer, snapshot
  and DuckLake incremental CLI paths. Each invocation can pause only between
  committed graph transactions and returns its durable byte cursor; invoking
  again with the same immutable artifact and mapping resumes safely. Pinned the
  batch target across managed-driver retries so a lost commit acknowledgement
  cannot consume an extra batch. Added pause/resume, replay, invalid-bound and
  lost-ack tests; exercised an actual server `SIGKILL` between DuckLake batches,
  restart on the same disk, and completion from the persisted cursor. Updated
  Python and DuckLake docs. Also made CI and the local release checker build all
  workspace binaries before integration tests, whose cached executables use
  absolute `CARGO_BIN_EXE_*` paths.
- **Verified:** 56 Python client tests, 19 DuckLake snapshot/incremental tests,
  8 release-helper tests, all Rust workspace tests and Clippy pass. The Rust
  suite includes process-kill/reopen and the new source-import `SIGKILL` test.
  `actionlint`, docs-site validation, `git diff --check`, and the release
  helper's real Bolt acceptance pass. The helper created local release
  `v0.4.0-beta.1`. No client-specific references were found in README, docs, examples,
  scripts, crates, deploy or workflows. Existing remote CI and docs workflows
  were green before these changes; updated workflow has not yet run remotely.
- **Decided:** publish this as a beta minor release if the local release helper
  and all gates pass; leave multi-writer lease coordination and distributed
  high availability outside this feature.
- **Next step:** push `main` and the generated beta tag, then verify GitHub CI
  and release workflows.
- **Blockers:** none so far. Host Cargo is unavailable, so Rust and release
  validation use the repository's Rust 1.98.1 container toolchain.

### 2026-10-20 — direct bounded Arrow ingestion in Python

- **Requested:** continue a short engineering session; leave atomic graph
  generations for later; commit and push the work.
- **Done:** confirmed the `v0.3.0-beta.1` CI and release workflows succeeded.
  CI took 23m43s; its artifact-cache lookup was cold (`Cache not found`) and it
  saved the Rust artifacts at the end. Added direct Python ingestion from Arrow
  `Table`, `RecordBatch`, and `RecordBatchReader`, including the reader's
  `read_next_batch()` API. Conversion is sliced to the configured graph batch
  size before dictionary rows pass through existing row/byte bounds. Documented
  DuckDB reader usage and ownership; no Arrow dependency or zero-copy claim.
- **Verified:** 53 Python client tests and 8 release-helper tests pass;
  actionlint, docs-site validation and `git diff --check` pass. Manual integration
  with PyArrow and DuckDB accepted a 103-row Table, a 41-row RecordBatch, and a
  250-row DuckDB Arrow reader; each write remained at or under 37 rows. Pushed
  commit `e3328a0`; its GitHub CI passed in 3m37s with the 639 MiB Rust cache
  restored. Workspace tests took 2m13s versus 21m53s on the previous cold run;
  Clippy took 13s. The run also exposed Node 20 deprecation and a yanked Neo4j
  driver pin, which were updated to cache action v5 and Neo4j 5.28.4 LTS.
- **Decided:** ship this additive Python source adapter on `main` without making
  a new release tag yet. Atomic graph generations remain deferred.
- **Next step:** verify the cache-action and driver-pin maintenance change in
  GitHub Actions. Arrow values still convert to Python dictionaries; measure
  throughput and memory before proposing a native Rust/Arrow path.
- **Blockers:** none for the current feature; the next cache run should also
  validate v5 on the GitHub-hosted runner.

### 2026-10-20 — batch progress and CI runtime repair

- **Requested:** make Brainwire more professional with a feature, create a new
  version, and investigate why CI takes about 20 minutes and fails.
- **Done:** added post-commit batch progress callbacks to Python `load_nodes`
  and `load_edges`, with explicit semantics for retries, callback exceptions,
  and the fact that progress is not a durable resume cursor. Fixed the Rust
  Clippy failure by replacing deprecated `AtomicUsize::fetch_update` with
  `try_update`, pinned Rust 1.98.1 consistently across CI and Docker, and added
  Cargo build-artifact caching that saves even after a failed check. Updated
  release helper support for isolated `CARGO_TARGET_DIR` and corrected README
  status for the failed v0.2.0-beta.1 run. Released locally as `v0.3.0-beta.1`.
- **Verified:** 654 Rust tests pass, as do workspace Clippy with warnings denied,
  formatting, server build, 51 Python SDK tests, 8 release-helper tests,
  actionlint, docs-site validation, and the 100-row Python ingest/replay/restart
  acceptance test. The failing GitHub run lasted 22m37s: workspace tests passed
  after 21m44s, then Clippy rejected the deprecated API. The release helper
  created the local release commit and tag after re-running all release gates.
- **Decided:** keep changes and `v0.3.0-beta.1` local; nothing was pushed or
  published. GitHub must run again to measure the cache and confirm the CI fix.
- **Next step:** push only when requested; then inspect the GitHub run's cache
  hit and completion time before claiming the CI runtime improved.
- **Blockers:** remote CI and cache behavior remain unverified until the next
  authorized push.

### 2026-10-20 — configurable knowledge ranking and beta release

- **Requested:** implement a feature, merge it to `main`, push to the private
  repository and create the next minor version. Asked how to publish a long
  write session atomically; clarified that this needs staged versions and a
  short publish-pointer switch, while current transactions remain short.
- **Done:** added per-query evidence weights to `knowledge.rank`, preserving
  current defaults and returning contributions normalized to sum to the score.
  Documented validation and usage. Made the 300-write quorum test follow the
  current Raft leader and retry only explicit leader-forwarding errors, so an
  election no longer fails the pooled-connection test.
- **Verified:** all workspace Rust tests pass, including the 300-operation
  network test; workspace Clippy and formatting pass; all 46 Python client
  tests, 7 release-helper tests and the documentation-site check pass. The
  release server binary builds, and the Python ingestion/restart acceptance
  test passes for 100 rows. The source tree contains no project-specific
  customer references. GitHub CI and release workflow results are pending the
  authorized push.
- **Decided:** release as `v0.2.0-beta.1`, a minor beta bump from `v0.1.1`.
  Long-running writes should stage batches under a draft generation; readers
  remain on the previous snapshot until validation completes and a durable
  manifest/revision pointer is published. No one-hour RocksDB transaction.
- **Next step:** commit the feature, merge local `main`, create the annotated
  version tag, push `main` and the tag, then check CI, GHCR image publication
  and GitHub release status.
- **Blockers:** remote checks/publication have not run yet. The repository and
  package remain private.

### 2026-10-19 — reproducible local comparison with Neo4j and FalkorDB

- **Requested:** do the real comparison described in the benchmark page and
  make the results clear with charts.
- **Done:** executed the pinned Docker harness against Brainwire, Neo4j Community
  and FalkorDB, one at a time with 4 CPU / 8 GiB limits, using a deterministic
  10k-node / 20k-edge graph. Added per-operation timing samples to the raw JSON,
  exact semantic checks (indexed values, neighbor sets, update counts and
  post-restart properties), waited for Neo4j indexes, and verified FalkorDB's
  persistent data directory/mount. Updated the comparison report and SVG. The
  report explicitly labels the graceful same-container restart test and the
  non-equivalent client/protocol and durability configurations. Temporary
  smoke receipts were removed; the final receipt is retained.
- **Measured:** Brainwire: indexed point p95 0.654 ms, one-hop p95 0.944 ms,
  update-series median/max 42.149/48.268 ms, ingest 28,261 records/s. Neo4j:
  2.177 ms, 8.249 ms, 77.763/173.122 ms, 15,392 records/s. FalkorDB: 0.462 ms,
  0.346 ms, 21.175/30.180 ms, 73,099 records/s. All recovered 10k nodes and
  20k edges following graceful container restart with persistent volumes.
- **Verified:** release Docker image built and the complete comparison harness
  passed; all per-system semantic and persistence checks passed; Python syntax,
  SVG parsing, docs-site checks and `git diff --check` are recorded at handoff.
  Seven release-helper tests pass both at the current version and against a
  simulated already-bumped worktree; 46 Python client tests pass. Host `cargo`
  is unavailable, so Rust tests could not be run locally.
- **Decided:** use these as a transparent local engineering baseline only, not a
  vendor ranking or production claim. Five update samples are reported as median
  and maximum rather than p95; fresh ingest is a single measurement per system.
- **Next step:** repeat multiple randomized rounds on a Linux host, include cold
  and warm cache states, capture peak memory and query plans, and test abrupt
  kill/container recreation before making broader durability or performance
  claims.
- **Blockers:** this host is macOS arm64; database client protocols differ;
  configurations are not durability-equivalent; only one fixed-order run was
  completed. `cargo` is not installed on this host.


### 2026-10-19 — benchmark charts and release/CI status

- **Requested:** clarify whether the version/release pipeline is in place and
  make benchmark results easy to understand with charts, including a future
  comparison to Neo4j and FalkorDB.
- **Done:** added a benchmark overview with two charts based only on retained
  Brainwire receipts: one-hour 100k-node mixed-load latency and short three-voter
  batch-write throughput. Linked the overview from the README and documented a
  fair, repeatable comparison protocol. No competitor result is fabricated.
  Clarified the local release-to-tag workflow and that current repository
  settings do not require CI for direct `main` updates. Reduced CI Cargo build
  parallelism to one after the latest run's Rust linker crashed while linking
  test binaries.
- **Verified:** latest Documentation site workflow passed. Latest CI on
  `b6a3eb4` failed during Rust test linking (`rust-lld` bus error); previous
  `v0.1.1` release failed before publication on runner capacity. The local CI
  mitigation is not yet committed or tested on GitHub. Benchmark SVG/data is
  tied to the dated local receipts, not a Neo4j/FalkorDB comparison.
- **Decided:** repository and artifacts remain private. Do not push or publish
  as part of this session. Comparative claims wait until equivalent workloads
  are executed against all three products.
- **Next step:** verify the local workflow and documentation changes, commit the
  intended pending work only after review, then run the revised CI on GitHub.
  After a green CI, prepare a new SemVer tag rather than reusing `v0.1.1`.
- **Blockers:** `cargo` is not installed in this local executor; the revised
  runner behavior is not yet known.

### 2026-10-19 — local SemVer releases and private image pipeline

- **Requested:** make release/deploy automation work locally, choose major,
  minor or patch versions from the change, keep all distribution private, and
  make every delivered version straightforward to run.
- **Done:** added a local release preview/apply helper. It derives major from
  breaking-change commits, minor from `feat:`, patch otherwise; synchronizes
  Cargo, lockfile, Kubernetes release pin and marked install docs; refuses a
  dirty worktree or non-`main` branch; runs local gates; creates only a local
  release commit and annotated tag. It never pushes. Added SemVer tests and a
  release guide. Reduced CI debug/incremental artifacts after the failed
  `v0.1.1` runner exhausted disk. The tag workflow now validates SemVer,
  builds and checks the exact versioned Docker image through Python/Bolt and
  persisted-volume restarts before pushing to private GHCR, records the image
  digest on the GitHub release, derives prerelease status from `-beta.N` /
  `-rc.N`, and verifies or safely resumes same-commit partial publications.
- **Verified:** seven release-helper tests, preview (`v0.1.2` stable or
  `v0.1.2-beta.1` with `--prerelease beta`), `actionlint`, docs-site links and
  `git diff --check` pass. Applying was refused as designed because existing
  local license/docs work is still uncommitted. No commit, tag, push, or
  publication was made. `cargo` is unavailable in this executor, so Rust tests
  and Docker image build were not run here. A fresh GitHub runner must confirm
  the disk-space fix and release workflow.
- **Decided:** repository and GHCR remain private. Never move/reuse a release
  tag; publish corrections with a new version. A plain SemVer tag is stable; a
  prerelease suffix is required for a GitHub prerelease.
- **Next step:** review and commit the current local work when ready, preview
  `python3 scripts/release.py --prerelease beta`, then use `--apply` on a clean
  `main` to produce a local versioned commit/tag. Push only when explicitly
  requested; the first new tag will validate the updated private pipeline.
- **Blockers:** full Rust and container checks were not executable locally
  because `cargo` is missing; the previous remote `v0.1.1` run failed before
  publication due to runner disk exhaustion.

### 2026-10-19 — Git author identity

- **Requested:** use the user's personal Gmail identity for Git and avoid
  exposing the former work address.
- **Done:** set both global and repository-local `user.email` to the requested
  Gmail identity. No email address was added to project documentation.
- **Verified:** new Git commits will use the requested identity. Existing
  history still contains 250 author/committer email entries using the previous
  identity across 129 commits.
- **Next step:** before making the repository public, decide whether to rewrite
  the existing history to replace those identities. Rewriting keeps the commit
  sequence and file changes but changes commit hashes and requires coordinated
  remote ref updates; the repository remains private meanwhile.
- **Blockers:** existing history was previously requested to be preserved, so
  it has not been rewritten without a clear instruction.

### 2026-10-19 — Apache-2.0 adoption and Docker metadata

- **Requested:** choose a license that lets people discover, use and adopt
  Brainwire publicly, while retaining a viable commercial path; clarify Docker
  implications.
- **Decided:** Apache-2.0 best fits broad adoption, commercial/BYOC usage,
  redistribution and an explicit patent grant. It also allows others to sell
  modified or unmodified versions, so it does not preserve exclusivity.
- **Done:** added the standard Apache-2.0 text and copyright notice, set the
  workspace license to `Apache-2.0`, and updated the Docker image with the
  license file and OCI license label. Updated README and container docs. Kept
  `publish = false` so individual Rust crates are not released to crates.io.
- **Verified:** `cargo metadata` reports `Apache-2.0` for all seven crates;
  docs-site validation and `git diff --check` pass. The local Docker CLI did not
  return from a build attempt and was interrupted, so image packaging is not
  yet verified. Changes remain local.
- **Next step:** before people can actually access it, make the GitHub source
  repository public and publish a release/image whose source tag contains this
  license. Confirm all copyright holders and third-party notices before that
  public launch.
- **Blockers:** the GitHub repository and GHCR package are currently private;
  the existing `v0.1.1` tag predates this license change. Review preserved
  commit metadata before making the repository public.

### 2026-10-19 — GitHub About, release and access clarity

- **Requested:** make the repository overview clearer, explain its license and
  `publish = false`, clarify the GHCR login/pull commands, and check whether a
  separate website needs to be created.
- **Done:** added a concise GitHub About description and relevant topics; left
  Website blank because the docs are not publicly hosted. Updated the README to
  distinguish a source tag from a successful prerelease/image publication,
  explain that `publish = false` blocks Cargo registry publishing only, and
  show how GHCR login and `docker pull` work for this private image. Preserved
  the repo's private visibility and all existing history.
- **Verified:** the GitHub About panel shows the saved description/topics. The
  `v0.1.1` release workflow is still running; the README links to its status and
  makes image availability conditional on success. GitHub Pages is not enabled
  for this private repository on its current plan, so there is no public docs
  URL yet and no external site/account was created.
- **Next step:** check the release workflow and its GHCR package after it
  finishes. If a public documentation URL becomes important, choose either a
  Pages-capable plan or a separate static host/domain then; neither is needed
  for private source access or the container registry.
- **Blockers:** the release workflow must pass before the private image can be
  pulled. The repository has no open-source license; choose one before
  distributing source or binaries beyond the intended private beta.

### 2026-10-19 — v0.1.1 release candidate and versioned deployment path

- **Requested:** make the public-facing README polished, add a Brainwire logo,
  remove all application-specific coupling, and prepare releases, tags,
  deployments and CI/CD on GitHub. Keep Docker, Swarm and Kubernetes images
  consistently versioned.
- **Done:** implemented bounded Cypher `DELETE r` and Python
  `GraphClient.delete_edges`, preserving endpoint nodes and enforcing graph
  write authorization, adjacency consistency, memory limits and batch rules.
  Refreshed the README and developer docs, added matching SVG logos, clarified
  beta capabilities and limits, and documented relationship deletion. Bumped
  the Rust workspace to `0.1.1`. Added CI for Rust/Python/Bolt acceptance, a
  tag workflow that verifies the Cargo and Kubernetes versions before creating
  a GitHub prerelease and publishing the exact tagged image to GHCR, and a
  private CI artifact workflow for the docs site. Added a Kubernetes release overlay
  pinned to `ghcr.io/allandefraga/brainwire:v0.1.1` with an image pull secret;
  documented safe secret creation and Swarm credential forwarding.
- **Verified:** workspace tests, warning-free Clippy, Rust formatting, 46 Python
  client tests, `actionlint`, rendered Kubernetes manifests and `git diff
  --check` passed. Docker built `brainwire:v0.1.1-rc` from this tree; the
  container reports `brainwire 0.1.1`. Search of the current
  source/documentation tree found no external application name or integration
  coupling.
- **Decided:** version all release artifacts with the same `v0.1.1` tag. Publish
  only a prerelease; production qualification remains open.
- **Next step:** publish local commit `e4d38e3` and tag after resolving
  whether the old published history should be rewritten to scrub historical
  references; configure a real Kubernetes/Swarm target separately when one is
  selected.
- **Blockers:** the private repository's current plan does not support GitHub
  Pages. A public docs deployment needs a plan change or external host. The
  existing `v0.1.0` history contains old references but is being preserved as
  requested. No remote database deployment target or cluster credentials are set.

### 2026-10-19 — release documentation refresh

- **Requested:** improve onboarding and documentation while preparing the next
  release from `main`.
- **Observed baseline:** local `main` and `origin/main` point to `8b32f69`
  (`Prepare Brainwire v0.1.0`); annotated tag `v0.1.0` resolves to that commit.
  The working tree was clean at the start of this documentation slice. Earlier
  entries describing uncommitted work or no remote are historical snapshots.
- **Done:** replaced the stale uncommitted/local-only README status, clarified
  the source-tag versus package/image distinction, and added a real connected
  TUI walkthrough alongside explicit synthetic/offline boundaries. Added direct
  navigation to indexes, connected status, DuckLake snapshot differences and
  experimental quorum operations. Documented dependency caching before offline
  Cargo commands. Updated the session workflow to identify `main` and its
  upstream rather than the historical `codex/brainwire` branch. Documented the
  new bounded `GraphClient.delete_edges` API, endpoint/type scoping, physical
  parallel-edge deletion counts, replay and partial-batch failure semantics;
  limited the Cypher claim to relationship `DELETE`. Replaced the remaining
  application-specific brand references in historical entries with generic
  external-integration wording, preserving Brainwire's independent identity.
- **Verified:** checked the README's relative links against repository files;
  checked Docker/Python schema names against the bundled ontology, and connected
  shell flags against the built binary's `shell --help` and existing guides;
  checked the baseline Git refs and
  `git diff --check`. These documentation checks do not repeat runtime or
  production qualification. A case-insensitive audit of all tracked and
  nonignored untracked paths and contents found zero external application brand
  matches. A broader audit found ignored historical recovery artifacts under
  `.superpowers/sdd` and diagnostic logs under `data/default`; these local
  artifacts remain outside the release tree. Old Git history and tags were not
  rewritten.
- **Next step:** integrate the session's feature work, run its targeted checks
  and release verification, then record the actual final commit/tag and remote
  publication outcome in a new entry.
- **Limits:** this documentation slice creates no commit, tag, release or push.
  Real source/ACL integration, temporal data history and multi-machine quorum
  qualification remain open; the existing beta boundaries still apply.

### 2026-10-17 — bounded Python node cleanup

- **Requested:** spend roughly 20 minutes adding robustness and usability for
  the upcoming application integration.
- **Done:** added `GraphClient.delete_nodes(label, entity_ids)`. It accepts an
  iterable (rejecting a single string, bytes value, mapping or non-iterable),
  validates labels and IDs, bounds each transaction by item count and compact
  UTF-8 ID bytes, deduplicates IDs within a batch, and uses `DETACH DELETE` so
  incident edges are removed with nodes. Full batches commit promptly; each is
  atomic, and a later invalid row does not undo already committed batches.
  Documented its non-goals: it is not a source cursor or snapshot reconciler,
  and independent edge deletion is still not exposed by this helper. Updated the
  README capability list and Python client guide. Added unit and live Bolt tests,
  including duplicate/missing IDs, Unicode byte bounds, partial input failure,
  incident-edge cleanup, and protected reader/writer authorization.
- **Verified:** all workspace Cargo tests passed; workspace Clippy with warnings
  denied and `cargo fmt --all -- --check` passed. All 42 Python example tests
  passed. Real Python/Bolt acceptance passed at 10,000 and 100,000 source nodes;
  each run also created and deleted 1,000 synthetic nodes and 1,000 incident
  edges, survived SIGKILL/reopen and passed the offline audit. On the final
  100,000-node local debug run, insertion took 21.1 s, replay 23.5 s, and the
  1,000-node cleanup 0.13 s. These are single-machine synthetic measurements,
  not production SLOs. The first 100,000-node attempt exposed that a whole-graph
  relationship `count(*)` exceeded the 30 s cooperative query deadline; the
  acceptance now uses bounded point/adjacency seeks and relies on the offline
  audit for full invariants. Protected Docker Compose auth acceptance passed
  on a locally built image: reader deletion was forbidden, writer deletion and
  replay of a missing ID succeeded, and the audit was clean. Image and owned
  Compose resources were removed.
- **Decided:** leave all source changes local and uncommitted; retain `.venv` for
  the next Python integration session.
- **Next step:** apply a real independently authorized source/ACL mapping and
  source cursor in shadow mode; define an API for independent edge deletion.
- **Blockers:** none for this feature. Dynamic ACL/source adapter qualification,
  and multi-machine replication qualification, remain separate release gates.


### 2026-10-17 — builder-focused README

- **Requested:** make the README trendier and more appealing to AI builders.
- **Done:** refreshed the headline, positioning and section labels while keeping
  the Docker/Python quickstart, concrete 0.1 capabilities, security boundaries
  and limitations intact. The copy describes Brainwire as graph infrastructure
  for context-rich apps and explicitly says it is not yet an agent runtime or
  turnkey RAG pipeline.
- **Decided:** keep docs in English; preserve candid beta status and standalone
  product identity.
- **Next step:** review the README and continue with the 0.1 integration gate.
- **Blockers:** none.


### 2026-10-17 — local 0.1 candidate and Docker/Python quickstart

- **Requested:** prepare Brainwire 0.1 for GitHub later and make Docker startup
  plus Python/Bolt ingestion straightforward for an external integration session.
- **Done:** rewrote the root README as a v0.1 entry point: local-candidate
  status, Docker demo, parameterized Python writes for nodes/edges, feature
  boundaries, security warning and links to operational guides. Removed
  application-specific branding from the README. Replaced `.venv-brainwire`
  with the ignored `.venv` path and added Docker ignore patterns to prevent a
  local Python environment entering Git or image context. Corrected the Rust
  toolchain description: development selects stable; Docker pins 1.98.1. Added
  `deploy/compose.shared-network.yaml` and documented how a separate consumer
  Compose project joins an explicit external network. The first executed README
  query exposed unsupported Cypher `ORDER BY`; sorted returned rows in Python.
- **Verified on the current working tree:** `cargo test --workspace --locked
  --offline` passed 646 tests across 54 suites; workspace Clippy with warnings
  denied and `cargo fmt --all -- --check` passed; Python client unit tests passed
  (18). Docker Compose built the image from this tree, initialized an isolated
  synthetic graph and reported healthy. Neo4j driver 6.3.0 sent bounded UNWIND
  writes for Customer/Segment nodes and IN_SEGMENT edges over Bolt; query
  returned Ada and Grace. After container restart, the readiness probe passed
  and both relationships remained. A separate ephemeral Python 3.12 container
  also resolved `brainwire:7687` over the shared-network overlay, wrote nodes
  and an edge, and read the edge back. Protected Compose profile rendering and
  the Docker auth acceptance passed (anonymous/wrong-token denial, graph-scoped
  reader/writer, 1,000 writes, pending-transaction SIGKILL rollback, retained
  volume and offline audit). Isolated containers/networks/volumes were removed.
- **Limits:** this is a local 0.1 candidate check, not a frozen release or live
  external application integration. The checkout contains extensive uncommitted changes;
  no exact release tree has been selected or tagged. The workspace still says
  `publish = false`, `license = UNLICENSED`, no Git remote is configured, and
  no separately installable Python package exists. The demo profile has auth
  disabled. Real adapter/authoritative ACL integration, production workload,
  multi-machine quorum qualification and node/edge temporal revisions remain
  open; temporal history is not included in the 0.1 claim.
- **Next:** review and freeze an exact candidate tree with Allan; tomorrow,
  validate the external adapter and ACL contract using authorized representative
  data, starting in shadow mode. Resolve the repository/image license before a
  distributable release. Publish only after explicit authorization.
- **Blockers:** safe GitHub push/release requires an explicit remote and exact
  staged/committed file set; none was configured or published in this session.

### 2026-10-16 — SIGTERM drain qualified in a Linux container

- **Done:** the Docker registry was reachable again, so the container check left
  open on 2026-10-15 ran. Extended `crates/server/tests/compose_restart.py`: an
  explicit transaction stays open while `docker compose stop` runs; the fixture
  waits for the server's "shutdown requested" log, asserts the stop is still
  waiting, commits, then requires container exit 0 and the "drained" log, and
  finds the row after restart. The final idle-only stop must also exit 0.
- **Verified:** image `636f63b1…` built from the current working tree (binary
  `372bdeb0…`, Debian glibc 2.36, aarch64) passed 3/3 runs with offline audit
  `ok`; receipt `docs/benchmarks/2026-10-15-graceful-drain/compose-linux.json`.
  Each run removed its project and volume; the test image was removed after.
- **Limits:** Docker Desktop on one Mac; no amd64, Kubernetes or Swarm target,
  no load during the drain beyond one open transaction.
- **Next step:** unchanged from the 2026-10-15 quorum entry (opt-in timing to
  locate the sub-2 s pauses; leader-following Python client) — both need
  Allan's go-ahead.

### 2026-10-15 — quorum write failure: per-RPC peer connections exhausted ports

- **Context:** same three-hour session, second increment after the drain was
  closed. Picked the open `CommitOutcomeUnknown` from the earlier distribution
  entry, because an uncertain write that cannot be reconciled is the exact risk
  the shadow beta must avoid.
- **Diagnostics (subagent, test-only):** `quorum_write_benchmark.py` now records,
  before cleanup, the failing repeat/mode/offset, request ID, elapsed time,
  every voter's status, a quorum-authoritative `cluster request --id` lookup and
  redacted log tails, via `--failure-receipt`. It never retries the write.
- **Root cause (measured):** the 2000×100×10 run reproduced
  `CommitOutcomeUnknown` (3.2 s; lookup `absent`; leader moved from voter 1 to 2
  in term 4). Host TIME_WAIT sockets rose from 11 to 58,046 in ~26 s: the peer
  client in `crates/replication/src/network.rs` opened a TCP connection for
  every AppendEntries/Vote, and the harness's Python fault proxy also opened one
  upstream connection per frame. Ports ran out, `connect()` failed, openraft's
  `ensure_linearizable` heartbeat round got no quorum and leadership churned.
  Earlier "leader/quorum unavailable" failures (and probably the quorum ACL
  acceptance failure at `Replica::begin` noted in an earlier entry) match this
  mechanism. After the fix the 500-row `quorum_replication.py` acceptance, which
  includes the ACL reader, passed twice (17 checks each).
- **Fix:** pooled peer connections (≤8 idle per peer, reused within 10 s,
  returned only after a complete authenticated reply); the peer server answers
  successive frames per connection (first frame 5 s, then 30 s idle, sender
  pinned, mTLS per frame). A failed reused connection clears that peer's pool
  and retries once on a new connection; only AppendEntries/Vote use the pool.
  That also keeps one-request peers (older binary, proxy) working, which an
  intermediate build without the retry broke on the first write. The test
  proxy now forwards successive frames per connection.
- **Verified:** new unit tests (reuse = 1 accepted connection for 3 RPCs;
  one-request peer still answers all) and integration tests (successive frames
  on one server connection; 300 linearizable reads + writes); replication crate
  3 + 6 + 9 pass; clippy `-D warnings`, fmt and `git diff --check` clean.
  An independent review found no correctness defect (reply pairing, retry,
  pinned sender, proxy fault semantics checked) and led to: idle-connection
  frames get the 5 s body deadline (only the wait for the next frame is 30 s),
  a client timeout purges that peer's pool, pooled streams are replaced after
  60 s (server certificate rechecked at handshake). Final binary `9a09b8c1…`:
  2000×100×10 PASS (1,517 single rows/s, p50 0.607 ms, p95 0.941 ms; 40,000
  rows verified after SIGKILL restart), host TIME_WAIT 9 before and 63 peak
  (was 58,046); `quorum_replication.py` 500 rows PASS (17 checks, SIGTERM exit
  0 per voter); termination fixture PASS with drivers 6.3.0 and 5.28.6.
  Intermediate binary `6f9c102f…` passed the benchmark twice and quorum at 200
  and 500 rows.
- **Limits:** loopback, one client, one host; the native vs proxy share of the old
  churn was not separated; no multi-machine, endurance or mixed-version
  rolling-upgrade run (only the unit test for one-request peers). Anyone holding
  the shared peer token can still occupy the 64 peer connection slots; that
  token already lets them act as a voter, so this is not a new exposure, but a
  per-sender cap is open. The new real-Raft integration test exercises the pool
  and does not reproduce port exhaustion.
- **Soak:** the write harness now verifies an exact count plus every row in
  2,000-ID blocks (a single unbounded read of 800k rows hit the 64 MiB query
  guard, by design) and accepts up to 300 repeats. 200 × (2,000 single + 2,000
  batched rows) on `9a09b8c1…`: PASS in 352 s, 1,299 single rows/s (p50
  0.589 ms, p95 1.015 ms, one 1.82 s outlier not diagnosed), all 800,000 rows
  verified after SIGKILL restart, term 1 throughout, 450 MB retained log,
  TIME_WAIT ≤87, voter RSS 35–135 MB.
- **Open finding:** a repeat of the same soak failed after 105 s: leadership
  moved 1 → 3 (term 2) at repeat 71 and the harness's client, pinned to the old
  leader, got `leader/quorum unavailable`. So a follower missed heartbeats for
  ≥500 ms, consistent with the 0.6–1.8 s pauses of the passing soak. RocksDB
  write stalls were zero and flushes ~6 ms on all voters; the host was busy
  (mediaanalysisd, Docker VM with 33 containers, BTLEServer) but contention is
  not proven. No tracing exists to locate the pause. A standalone 60 s
  `fsync` probe on the same host (1 KiB appends) saw 79 calls >100 ms and a
  530 ms maximum (p99.9 0.06 ms), so sync pauses are a plausible cause; the
  voters were not instrumented to confirm it.
- **Next step:** add opt-in timing/tracing of leader heartbeat, follower
  append/apply and fsync, rerun the soak on a quiet host, and make the harness
  follow a leader change (re-resolve the leader, reconcile by request ID)
  instead of failing; then a mixed-version rolling restart for the one-request
  fallback.

### 2026-10-15 — bounded SIGTERM drain (Oct 15 queue slot)

- **Requested:** three-hour session; pick one beta-blocking increment. Started
  20:12 UTC. Chose the dated October 15 slot, "resolve shutdown and cancellation
  support boundary", because every orchestrator rollout sends SIGTERM and the
  server died mid-session, turning each planned stop into unknown commit
  outcomes for clients. No Allan decision was needed.
- **Done:** `brainwire serve` now drains on SIGTERM/SIGINT. The listener closes;
  sessions idle at a message boundary (pre-auth, READY, FAILED) close; a request
  read from an idle session during the drain is refused with
  `Neo.TransientError.General.DatabaseUnavailable` and not executed; explicit
  transactions and result streams keep running until idle or until
  `server.shutdown_grace_ms` (new key, default 8,000 ms, 0–3,600,000, file/env).
  Exit 0 = complete drain, exit 1 = sessions abandoned (their pending work rolls
  back). Code: `serve_until` in `crates/bolt/src/server.rs`, signal/exit handling
  in `crates/server/src/serve.rs`, config in `config.rs`, `libc` dependency.
- **Defect found and fixed:** the first build aborted (SIGABRT) in 2 of 6 runs of
  one fixture batch. Crash reports showed an engine worker thread in
  `rocksdb_close` while `main` ran `exit()` C++ static destructors. Abrupt
  SIGTERM never ran destructors, so the race was new. `serve` now waits up to
  5 s for the engine threads to release the stores and closes RocksDB on the main
  thread; otherwise `_exit(1)` (the old abrupt behaviour). Not reproducible on
  demand (40 ad hoc cycles without the fix also passed), so the evidence is the
  stack plus the structural fix.
- **Verified:** 3 new Bolt drain tests (idle close + admission refused, commit
  during drain then pipelined BEGIN refused, grace deadline count); config test.
  `termination_recovery.py` rewritten for drain semantics (commit during drain
  survives restart with indexes; abandoned transaction rolls back; unacknowledged
  RUN atomic; stalled reader bounded by grace; idle pool exits 0): 8/8 PASS on
  binary `301346eb…` with drivers 6.3.0 and 5.28.6. Quorum fixture (200 rows) now
  ends with SIGTERM per voter: exit 0 each, digests match, 17 checks PASS.
  `connection_limits`, `multigraph_acceptance` (2,000 rows/graph),
  `typed_neighborhood_acceptance`, `query_memory`, `beta_acl_api` PASS.
  `cargo test --workspace --locked --offline` 642 passed / 0 failed; clippy with
  `-D warnings` and `cargo fmt --check` clean; `git diff --check` clean.
  Evidence: `docs/benchmarks/2026-10-15-graceful-drain.md` and receipts.
- **Not verified:** Linux container. Docker's daemon timed out resolving base
  image metadata (`rust:1.98.1-bookworm`, `debian:bookworm-slim`), so no image
  was built; signal delivery via the image and the destructor ordering on
  Linux/glibc are unverified with this build. No Kubernetes/Swarm target.
- **Limits:** running engine calls are not interrupted; a second signal does not
  shorten the drain (use SIGKILL); quorum stop does not transfer leadership; no
  COMMIT-phase kill hook; loopback single-host timings only.
- **Docs:** termination recovery rewritten; serve, quickstart, orchestrators,
  multigraph, quorum, operator checklist, README and roadmap (Oct 15 progress,
  resource gate row) updated. The Oct 13 termination receipt stays historical.
- **Next step:** build the image and rerun the drain on Linux Compose
  (`docker compose stop` → exit 0, data retained, audit ok) once the registry is
  reachable; then the October 18 slot (source revision ordering and import
  recovery) or the quorum ACK-timeout diagnosis from the earlier entry.
- **Blockers:** Docker registry access for the container check. No commit, push,
  publication or deployment was made.

### 2026-10-15 — initial agent platform scope

- **Done:** drafted [the initial agent platform proposal](docs/agent-platform-mvp.md)
  and linked it in the full-product backlog. It scopes a bounded Python runner,
  cited graph context and explicit memory operations; provider orchestration is
  outside the database engine.
- **Decided:** durable memory is gated on atomic history for both nodes and
  edges, with distinct recorded/valid times, idempotent writes and historical
  edge access before promising as-of traversal. Identity-bound ACL is also a
  prerequisite for shared-user use. This is a proposal, not an implemented
  agent runtime or temporal storage feature.
- **Verified:** documentation diff and whitespace checks; no code or runtime
  behavior changed. All work remains local.
- **Next step:** review the proposal, then sequence node/edge history and
  identity-bound ACL foundations before implementing the small read-only Python
  runner and explicit versioned memory API.
- **Blockers:** none for the design draft; implementation remains gated by the
  existing approved-spec workflow and the temporal/ACL acceptance contracts.

### 2026-10-15 — 30-minute distribution and performance session

- **Requested:** spend 30 minutes on distributed systems, performance, Cypher,
  RocksDB and WAL; keep changes local.
- **Measured:** added an isolated three-voter write diagnostic comparing
  sequential Cypher `CREATE` with `UNWIND` batches. In the canonical 400-row × 5
  alternating run, single writes reached 827 rows/s and batches of 25 reached
  15,081 rows/s; a repeat measured 826 and 14,784 rows/s. All 4,000 rows
  survived same-volume SIGKILL/restart and offline voter digests matched.
  Separate batch-100 and batch-400 runs measured 31,819 and 41,875 rows/s, but
  had only 20 and 5 ACK samples. The batch-1,000 run reached 35,409 rows/s with
  25 ms p50 and only three samples.
- **Measured:** a larger three-voter run measured 6,000 single rows at 937
  rows/s and 60 batches of 100 at 34,106 rows/s (batch ACK p50/p95
  2.999/3.113 ms). All 12,000 rows survived same-volume restart and matching
  offline digests; the run lasted eight seconds. A 10-round follow-up measured
  10,000 single ACKs at 841 rows/s (p50/p95 1.050/1.708 ms) and 100 batches of
  100 at 32,090 rows/s (p50/p95 3.073/3.748 ms); all 20,000 rows survived
  restart with matching digests. Batching delivered about 38× aggregate row
  throughput in this one-host shape; the 34 ms maximum single-write latency
  confirms host/queueing outliers. This 14.5-second run is not endurance or
  multi-machine qualification. A higher `--rows 2000 --batch-size 100
  --repeats 10` run did not complete: one single-row CREATE received
  `CommitOutcomeUnknown` at the 10-second `raft.client_write` wait. Request ID
  and limits are preserved in the failure receipt, but voter status, exact
  offset and durable outcome were not captured before scratch state was cleaned
  up. This is an unresolved latency/commit outcome, not evidence of lost data;
  do not raise the timeout or disable WAL sync before diagnosing it. A preceding
  run varied from 173–310 single rows/s and 1,934–12,983 batch rows/s. WAL and
  sync settings were not changed.
  The harness rejects optimized Python, retains process ownership through
  confirmed exit and tests cleanup failure paths.
- **Measured:** extended `detach_delete`'s isolated benchmark with optional
  `--full-delete`, timing the production helper plus the synchronous transaction
  commit. On warm synthetic RocksDB fixtures, one 10k-in/10k-out run removed
  20k relationships / 40k adjacency records in 17.297 ms; one 100k-in/100k-out
  run removed 200k relationships / 400k records in 203.229 ms. Post-timing
  checks verified the hub was absent, the exact peer count remained, both
  adjacency families were empty and graph invariants passed. These are single
  local observations, not repeated percentiles or Cypher/quorum results. The
  separate warm streaming-scan comparison remains about 3× faster than its
  materializing baseline at 20k and 200k hub degree.
- **Measured:** a release ACL-filtered read example over 100k nodes returned
  7,143 indexed matches for Alice in 33.650 ms p50, 50k visible label rows in
  149.760 ms, and 50k visible property values in 221.571 ms. These are warm
  embedded reads, not server or cluster timings.
- **Measured:** a separate indexed one-hop fixture over a 100k-edge hub returned
  one neighbor with `LIMIT 1` in 0.012 ms p50 (max 0.033 ms), while counting
  all 100k outgoing links took 234.941 ms p50 (max 238.149 ms). Query
  reservation stayed below 2.7 KiB; its high-water mark includes warmup and
  measured runs and is not process RSS. A 200k-edge repeat returned `LIMIT 1`
  in 0.010 ms p50 and counted all links in 399.205 ms p50 (about 1.7× time for
  2× degree); exact results and invariants passed. The limited result is asserted
  to be an actual peer.
- **Durability review:** graph transaction writes (`txn.rs`) and Raft metadata
  batches (`replication/storage.rs`) still use RocksDB `WriteOptions` with
  `sync=true`; WAL remains enabled. Store crash/reopen checks and the full
  workspace tests passed. Full Clippy with warnings denied, formatting and
  `git diff --check` passed. No production write/durability behavior was
  changed in this session.
- **Artifacts:** `docs/benchmarks/2026-10-15-quorum-write/`,
  `docs/benchmarks/2026-10-15-detach-delete/`,
  `docs/benchmarks/2026-10-15-acl-reads.md`, and
  `docs/benchmarks/2026-10-15-streaming-reads.md`,
  `docs/benchmarks/2026-10-15-cypher-edges.md` and its 100k/200k receipts;
  quorum run receipts; README links added.
- **Next:** first update the quorum harness to preserve per-voter status and
  look up the request ID before cleanup when an ACK times out; reproduce the
  20k-single workload at a bounded size before scaling. Then repeat distributed
  loads under controlled idle-host conditions and capture RSS/compaction for
  high-degree deletion. Multi-machine replication, crash-at-ACK and power-loss
  qualification remain open. No remote actions, commits or pushes.
- **Blockers:** none for local loopback work. Current receipts do not establish
  multi-machine capacity, production workload performance or power-loss safety.

### 2026-10-15 — high-degree deletion and easier local deployment

- **Requested:** improve distribution, Cypher, WAL and RocksDB performance, and
  make Docker/Kubernetes operation straightforward.
- **Done:** `detach_delete` now streams dependency-tracked adjacency scans through
  `visit_prefix_for_update`, decoding edge keys without building the previous
  temporary key/value `BTreeMap`. A regression covers 2,048 incident edges,
  incoming/outgoing mirrors, a self-loop, and inserts/tombstones in the pending
  transaction overlay; the committed graph passes `check_invariants`. This
  removes temporary copies but still retains an O(degree) `EdgeRef` vector; no
  before/after performance figure is claimed yet. WAL sync remains enabled for
  durable acknowledged commits and quorum metadata.
- **Deployment:** the Docker guide now waits for readiness and documents project
  isolation, alternate ports, logs and retained-volume cleanup. The Kubernetes
  guide adds an isolated local `kind` walkthrough with a private kubeconfig,
  local image loading, namespace, rollout/probe, port-forward and PVC-backed Pod
  restart. The demo image built in 26 seconds on the cached second build; Bolt
  batch writes and reads passed after a container restart, and offline RocksDB
  integrity check returned `consistency=ok`. The isolated smoke project, volume
  and image were removed afterward.
- **Validation:** full `cargo test --workspace --locked --offline`, full Clippy
  with warnings denied, `cargo fmt --all -- --check`, Compose config validation,
  and Kustomize render passed.
- **Next:** measure high-degree `DETACH DELETE` scan/commit latency and peak
  memory against the materializing baseline at representative degrees. Run the
  documented kind acceptance when `kind` and an available Docker/Kubernetes
  environment are present; local manifest rendering is not a live cluster test.
- **Blockers:** this host has no installed `kind` and no current kubectl context,
  so no Kubernetes cluster was started. The optimization has correctness tests
  but not yet a before/after benchmark.

### 2026-10-15 — page guidance and clearer TUI navigation

- **Requested:** make the UI more polished, explain each page and its terms,
  and make interactions easier to discover.
- **Done:** added a persistent active-page strip to the synthetic demo and the
  local read-only overview; page-specific help overlays now describe purpose,
  field meaning, controls and mode limits. `?` opens help in local/connected
  overview workspaces; F1 opens page help in the demo and still inspects the
  selected ontology/file item where that was its existing action. Help is
  modal and preserves selection/query state. Offline help clarifies that
  ontology signatures are schema permissions, not stored edges; connected help
  notes its selected-type scope. Query-only F5 is now shown only where it works,
  and repeated arrow navigation keeps focus on the page strip.
- **Validation:** TUI contextual-help render tests cover all 13 views/panes,
  English and pt-BR, at 80×24 and 160×48 (connected panes also at 120×30), with
  `NO_COLOR`. The 45 TUI tests, full workspace tests, Clippy with warnings
  denied and rustfmt check pass. The admin ontology integration test also
  passes with its offline-schema/index status header.
- **Next:** if desired, continue visual polish from the Observatory composition
  by adding bounded shortcuts to the 20-type ontology catalog; its full list is
  currently scrollable and explained, rather than a dense all-at-once hairball.
- **Blockers:** none.

### 2026-10-15 — dense ontology and complete TUI connection catalog

- **Requested:** stress the graph and TUI with a complex 20-type ontology and
  make every possible connection visible.
- **Done:** added `examples/complex-ontology-20/ontology.json`: 20 node types,
  400 directed type-pair signatures (including self-loops and reverse
  directions), eight relationship names, temporal/weight/source properties,
  and declared indexes. The offline Ontology TUI now toggles (`a`) between the
  selected-type view and a scrollable catalog of all declared signatures.
  `PgUp`/`PgDn` traverse all 400 without implying they are all stored edges.
  Created local ignored demo graph `data/ontology20_stress` with 20 nodes and
  400 actual synthetic edge records; `db stats` reports those counts and
  `db check` reports `ok`. Screen captures (first/last pages, 160×48) are in
  `docs/design/assets/2026-10-15-ontology-stress/`.
- **Validation:** TUI tests exhaustively visited all 400 rendered IDs at
  80×24, 120×30 and 160×48. `cargo test --workspace --locked --offline`,
  `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`,
  and `cargo fmt --all -- --check` passed. The local graph reports 20 nodes,
  400 edges, and an `ok` integrity check.
- **Next:** if a simultaneously drawn global node-link map is desired, build it
  as a separate bounded/zoomable view; the new catalog intentionally keeps the
  complete schema legible through scrolling. The connected read-only TUI still
  shows the selected type only.
- **Blockers:** none.

### 2026-10-15 — standalone product, Python Cypher demo and Docker onboarding

- **Requested:** keep Brainwire fully independent of the adjacent company, make
  the Cypher/Python path easy to demonstrate, provide plug-and-play Docker
  startup, and prepare a reviewable local change for a future private-repository
  PR. Nothing was pushed or published.
- **Done:** removed company-name references from the working tree, renamed
  integration-specific example/docs paths to generic source-ingestion names,
  and changed the default graph name to `default`. Preserved the existing local
  RocksDB data by renaming its directory to `data/default`; no records were
  rewritten. Historical benchmark numbers and sanitized artifacts remain
  historical evidence: old source/log/image hashes may no longer match the
  renamed or sanitized working-tree files.
- **Cypher/Python:** added `GraphClient.run_cypher(query, parameters=None)` with
  validation and materialized dictionary results; the standalone demo exercises
  point and indexed lookup, typed traversal, relationship count, parameterized
  update/readback, edge/node batch replay, property removal and ready indexes.
  The 100,000-node / 99,999-edge run passed and then reopened the same RocksDB
  data after a server restart. The demo uses disposable synthetic data and
  cleans up credentials and temporary storage. Its six connected TUI captures
  and receipt are under `docs/design/assets/2026-10-15-demo/`; it is a local
  single-node demonstration, not a quorum/HA acceptance.
- **Docker:** added `deploy/compose.demo.yaml` for build, one-time ontology
  initialization, a persistent named volume, readiness checks and bounded
  resources. Verified current-source image build, Python batch writes and
  parameterized query, clean restart with records retained, offline integrity
  audit, and a second Compose `up` while healthy. The demo volume was removed
  after the isolated acceptance. `docs/container.md` now includes explicit
  `pull` / `up --no-build` instructions for a future private image.
- **Verified:** `cargo test --workspace --locked --offline`, formatting,
  Clippy with warnings denied, Python client tests, 100K end-to-end demo, Docker
  Compose integration and working-tree text/path scan all pass. Markdown
  relative-link check reports zero broken links. Screenshots containing the
  former default graph name were regenerated from the current TUI; OCR checked
  all 109 repository images with no remaining company-name matches.
- **Blockers / next:** quorum ACL acceptance still fails at the Raft
  `Replica::begin` linearizable read barrier immediately after a 500-row Cypher
  create; the ACL mutation itself has not yet run, so distributed acceptance is
  not qualified. There is no Git remote configured and the authenticated GitHub
  account currently lists no target repository. Ask Allan for the exact private
  repository URL, then inspect its branch/protection conventions before making
  a local PR branch or publishing anything.

### 2026-10-15 — generic Python edge batches

- **Requested:** prepare Brainwire for the beta, especially smooth Python batch
  writes and a usable TUI. Allan will provide the reorganized external application pipeline
  job/write contract from Claude later today; do not implement that adapter
  before reviewing the contract. Started approximately 07:24 UTC. All work stays
  in `/Users/fraga/Documents/brainwire`; no push, publication or deployment.
- **Done:** added `GraphClient.load_edges(source_label, relationship,
  target_label, rows)`, a bounded iterable API parallel to `load_nodes`. Edge
  rows use `source_id`/`target_id` plus properties; each transaction is capped
  by the configured row and compact-JSON byte bounds. It refuses unknown
  endpoints and rolls back their whole batch. Same endpoint/type rows merge;
  omitted properties remain unchanged and `None` removes a property. Updated the
  Python guide and README. Extended the real Bolt ingestion harness with edge
  upsert/replay, deduplication and missing-endpoint rollback checks.
- **TUI:** when the connected workspace opens without a graph sample configured,
  the Graph pane now says no sample was configured and that no graph data was
  queried; it no longer implies that a configured entity is missing. The
  bilingual prompt gives the four CLI fields needed for the bounded preview.
- **Verified:** all 16 Python client unit tests pass. Full
  `cargo test --workspace --locked --offline` passed before the TUI prompt edit;
  afterward, all 39 `brainwire-tui` tests passed. Workspace Clippy with
  warnings denied, `cargo fmt --all -- --check`, and `git diff --check` pass.
  The native Python
  Bolt/restart acceptance passes with 100 synthetic nodes, batch size 25 and
  the new 3-row edge scenario, including recovery and audit. Python compilation
  passes. The current Cypher subset cannot read relationship variables, so the
  Bolt acceptance verifies merged edge cardinality/endpoints; property-row
  transformation is checked by the client unit test, not a query readback.
- **UI / pending:** current connected TUI is genuinely read-only and offers
  Ontology, Memory, Indexes and a fixed-signature bounded Graph sample. It has
  no connected Cypher editor/import job panel; the demo remains synthetic. Keep
  the selected Observatory layout and defer job progress/mapping UI to the
  supplied pipeline contract. General ontology-to-source inference, CDC and
  production external application binding remain open.
- **Next step:** review the pipeline/job contract Allan brings from Claude,
  then bind it to these shared node/edge batch semantics, source cursor and
  explicit removal/reconciliation rules. For the October beta, close the
  TUI/query workflow and run the agreed external application source qualification before
  claiming source-ready operation.
- **Blockers:** production source ontology/mapping, job/retry contract and
  representative external application source profile have not yet been provided.

### 2026-10-15 — retained-log exhaustion and recovery qualification

- **Requested:** 30 minutes on distributed systems. Started 06:54:35 UTC.
  Changes stayed in the standalone repository; no commit, push or deployment.
- **Done:** added the opt-in `--log-cap-recovery` mode to the synthetic quorum
  acceptance. It keeps the standard acceptance unchanged, uses a 1 MiB cap and
  four-row setup, then issues bounded writes with stable request IDs. Before
  the final proposal, all voters had 13,462 retained-log bytes free, less than
  its 16,402 raw property bytes. Readiness still passed at that boundary, and
  the following write got `CommitOutcomeUnknown`; the rebuilt candidate's
  observation-only probe immediately afterward returned `fatal_storage_error`.
  Added a safe stable `running_issue` category to Raft status and readiness;
  it never exposes the fatal message or storage path. After
  restarting the same voter identities and data volumes with a 1 GiB cap,
  quorum lookup returned absent and same-ID retry applied the request once.
  Replaying a known committed request returned `RequestAlreadyCommitted` before
  rerunning its callback. All replicas converged at commit/apply index 45,
  offline logical graph digests matched, and processes, proxies and temporary
  data were cleaned. Candidate binary SHA-256:
  `8008c6218a44d678512566c9e26c3bb4741aa306da61c584fd8428ab85ff8572`.
- **Evidence/docs:** opt-in 17/17 checks and default 500-row 16/16 checks pass
  on the rebuilt candidate; three further consecutive pre-diagnostic opt-in
  repeats also passed 17/17. The four-row
  receipt and reproduction commands are in
  `docs/benchmarks/2026-10-15-distributed-ops.md`. The quorum guide, README and
  [replication next-gates](docs/design/replication-next-gates.md) link the
  qualification and retain its limits.
- **Verified:** quorum Python self-check; `cluster_probe` (5 tests),
  `cluster_inspect` (7 tests), replication storage (9 tests), full
  `cargo test --workspace --locked --offline` and
  `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`.
  Compose, Swarm and Kubernetes manifests rendered locally; no containers were
  started or resources applied. Formatting, Python compilation and diff checks
  passed. Astra reviewed the final change and receipts with no blockers.
- **Limits / next:** readiness passed at the capacity boundary; it is a Raft
  quorum-read signal, not future-write capacity. The post-error
  `fatal_storage_error` observation is not asserted as a universal behavior.
  This is 1 MiB local native
  process testing, not exhaustion at the 1 GiB default, power-loss/storage
  corruption testing or multi-machine deployment. Snapshot transfer/purge and
  joint membership remain separate high-risk gates; keep snapshots disabled.
  Next step: implement a small, separately planned stage of the reviewed global
  snapshot design, beginning with failure tests for an inconsistent applied cut.

### 2026-10-14 — ACL read performance and distributed inspection

- **Requested:** one hour focused on performance, plus new distributed-systems
  features. Started 20:27 UTC. All changes and tests stayed local; no push,
  publication or target deployment.
- **Performance:** added borrowed transaction point reads over snapshot values
  and staged writes/tombstones. ACL-governed Cypher now reads and decodes each
  candidate node once, while charging node and ACL allocations before decode.
  Unrestricted repeated-variable mismatches still reject before loading a large
  target. The 100k and 1M synthetic comparisons use identical release harnesses,
  two runs in opposite order, and 14 measured samples per query. Report and raw
  JSON are in `docs/benchmarks/2026-10-14-acl-read-performance.md` and its
  receipt directory. At 1M nodes, median gains range from 2.43x to 3.88x; this is
  warm single-host embedded data, not a product SLO or external application workload.
- **Distributed operations:** `cluster inspect` fans out to all configured
  voters concurrently, records each peer's `response_time_ms`, keeps each
  request's deadline and preserves voter-ID report order. It now reports
  sampled commit/apply-index spread and per-voter lag when every initialized
  voter supplies those indexes; incomplete or uninitialized samples omit the
  comparison, which is explicitly non-atomic and never presented as readiness.
  A synchronization test proves every voter is contacted before any response
  is awaited. Local release acceptance with three native voter processes
  passed all 15 quorum, partition, restart, request-idempotency, ACL and
  offline-audit checks on 500 rows; all owned processes, fault proxies and
  temporary data were cleaned. A second release run with the replication-lag
  summary also passed all 15 checks; all voters were at committed/applied index
  512 with zero sampled lag. Receipt:
  `docs/benchmarks/2026-10-14-distributed-ops/acceptance-500-replication-progress.json`,
  binary SHA-256 `d046506eaf9b821abb12da9dc1a3a522781c6163c618a27f7ec8a31bb6e82580`.
  A third run kept one follower reachable for Status while dropping its consensus
  traffic, then committed three writes on the majority: `cluster inspect` showed
  a real three-index commit/apply gap and showed zero after healing. All 16
  checks and owned-resource cleanup passed; receipt:
  `docs/benchmarks/2026-10-14-distributed-ops/acceptance-500-live-lag.json`.
  A repeat reproduced the same gap and catch-up; receipt:
  `docs/benchmarks/2026-10-14-distributed-ops/acceptance-500-live-lag-repeat.json`.
  The bounded catch-up poll now returns only when all three commit indexes are
  equal and each applied index equals commit. Two subsequent release runs
  passed all 16 checks and verified the three-index lag plus exact convergence
  after healing: `docs/benchmarks/2026-10-14-distributed-ops/acceptance-500-live-lag-recovery.json`
  and `docs/benchmarks/2026-10-14-distributed-ops/acceptance-500-live-lag-final-repeat.json`.
  Inspection remains diagnostic, not a quorum/readiness proof.
- **Verified:** full `cargo test --workspace --locked --offline -- --test-threads=1`,
  full-workspace Clippy with `-D warnings`, formatting and diff checks passed
  (628 tests across 54 suites). The focused inspect suite now has six tests,
  including initialized voters with missing indexes; null progress and absent
  lag fields are verified for that case and for invalid/stopped peers.
  The release quorum acceptance is local native-process testing, not Kubernetes,
  Swarm or multi-machine qualification. Astra reviewed the progress reporter,
  requested the initialized-but-missing-index test, and later re-reviewed the
  bounded convergence poll and receipts; no correctness blockers remain in
  this diagnostic feature.
- **Next / limits:** qualify mixed concurrent workloads, cold-cache and realistic
  ingestion separately. The distributed release gates still include coordinated
  snapshots/log compaction, dynamic membership and safe erased-voter replacement,
  plus actual Docker/Kubernetes/Swarm and multi-machine network qualification.

### 2026-10-14 — Quorum operation and atomic Python ACL writes

- **Requested:** use remaining weekly allowance from 7% toward 1%, checking actual
  account usage and preserving a completion margin. Started 08:22 UTC; verified
  at 08:41 UTC. Work stayed local, with no publication, push or target deployment.
- **Done:** each private status reports retained encoded log bytes, configured cap
  and remaining bytes. Atomic boundary tests prove rejected multi-entry append
  leaves entries/accounting/commit/apply unchanged, full-cap duplicate replay,
  lower-cap refusal and increased-cap reopen without data deletion.
- **Operation:** `cluster inspect` samples all three members, validates identity
  and capacity, emits explicit unavailable/invalid statuses and exits nonzero on
  incomplete sampling or cap mismatch. It is not a quorum proof. Local `cluster
  probe --kind liveness|readiness` keeps running followers live while readiness
  requires the local leader and a real quorum read barrier without forwarding.
  These are Raft samples, not graph/authentication/future-write guarantees.
- **Python:** `write_transaction` uses explicit retained metadata and does not
  automatically retry unknown outcomes; `request_key` matches the Rust UTF-8
  namespace. `upsert_node_with_acl` commits node changes and authoritative private
  ACL together, optionally using the stable seed. Permission refusal rolls the
  MERGE back. Existing managed CRUD and source importer behavior stays unchanged.
- **Verified:** 624 Rust workspace tests, 31 Python tests, Clippy -D warnings,
  fmt/diff checks. Astra reviewed the bounded plan and final source without
  blockers. Native debug qualification (200 input nodes) and release (500) passed
  all 15 fault/operation checks; three members' eleven-CF logical graph digests
  match and all owned child processes, proxies and temporary directories cleaned.
  Evidence verifier binds actual source/binary/check logs and rejects six false
  readiness/cleanup/capacity receipts. Optimized binary SHA
  `04a18d258f7f4811dd573a9dbc8ff84c6be14e03f62c6200f9cf7336cc5ad7e1`.
- **Docs/evidence:** `docs/benchmarks/2026-10-14-quorum-capacity.md` and receipt
  directory; Python and quorum guides updated. Previous candidate receipts remain
  bound to their previous source/binary, never relabeled as current evidence.
- **Preparation:** saved Astra-reviewed snapshot/compaction plan at
  `docs/design/raft-snapshots-and-compaction.md`; snapshot creation must refuse a
  fatal apply gap rather than capture graphs ahead of global APPLIED. Added a
  regression proving Raft probes do not promise future write headroom. Serving
  sources/binaries remain unchanged by that final test-only addition.
- **Next / limits:** coordinated snapshots/log compaction, joint membership/new-ID
  disk-loss replacement and real machine/container qualification remain pending.
  Real cluster log exhaustion was not tested; storage-boundary exhaustion was.
  Real external application/DuckLake authoritative ACL mapping and importer integration remain
  open. No TUI changes or throughput claims; native Bolt TLS remains separate.


### 2026-10-13 — Real fixed three-voter quorum protocol

- **Requested:** implement quorum replication. Started 13:53:38 UTC; final
  verification at 15:00 UTC. All work stayed local; no pushes or target deployment.
- **Done:** optional OpenRaft 0.9.25 group across all named graphs, independent
  synchronously durable votes/logs/commit/apply/deduplication, authenticated bounded
  peer RPC and non-loopback mTLS. Leader-only transactions obtain a linearizable
  quorum barrier before the graph snapshot. COMMIT prepares concrete mutations,
  stages allocator counters and commits after majority agreement; followers never
  replay Cypher. Per-graph revision CAS and command-bound ledgers fence stale
  proposals and recover graph/global-applied crash ordering.
- **Operations:** explicit one-time voter provision, guarded normal startup,
  committed schema/graph creation, status/request lookup/offline audit. Stable
  Python transaction metadata resolves duplicate requests; unknown commit outcomes
  are nonretryable errors until resolved with the retained request identity.
- **Safety fixes/review:** Astra reviewed plan and final implementation. Missing
  published graph, registry, voter identity or Raft state refuses empty recovery.
  An erased-voter same-ID experiment failed and is retained as diagnostic evidence;
  this unsupported procedure is guarded, not advertised as recovery.
- **Verified:** 614 workspace tests passed (single test thread), Clippy -D warnings,
  fmt and diff checks. Frozen debug SHA `ffd228769250700adb182ad6f1064ecb66e93581e0b91ad9fbcaa3d16c241cd5`
  passed 11 actual fault/restart checks with 200 synthetic nodes and two graphs;
  all three eleven-CF logical digests match, all owned processes/proxies/data cleaned.
  First full-suite attempt hit a pre-existing Bolt deadline timing failure during
  concurrent compilation; unchanged assertions passed in the final isolated run.
- **Evidence/docs:** `docs/quorum-replication.md`,
  `docs/benchmarks/2026-10-13-quorum.md` and its bound source/check/fault receipts.
  Existing standalone orchestrator evidence remains bound to its earlier candidate.
- **Next step / blockers:** implement coordinated snapshots/log compaction and
  learner/joint membership for new-identity disk-loss replacement. Then qualify
  release throughput, power/storage failures and three-machine Kubernetes/Swarm
  routing, peer TLS and persistent volumes. No current snapshot, purge, sharding,
  dynamic membership, graph drop/schema migration or erased-member replacement.
  Native Bolt TLS and dynamic external application adapter remain separate gates. TUI unchanged.


### 2026-10-13 — Governed ingestion and recoverable deployment candidate

- **Requested:** node ACL, DuckLake incremental ingestion, local container
  deployment/recovery, qualification and operation; steering added competitive
  feature targets and easy Python single/batch/property mutations. Started
  10:37:06 UTC / 12:37:06 Europe/Amsterdam. Final evidence verified at
  12:51:55 UTC / 14:51:55. No remote actions.
- **Done:** bounded private ACL persisted separately from user properties;
  protected static identity policies filter reads, traversals, ranking and sparse
  components before results. Privileged `db.setNodeAcl` and Python helper stamp
  metadata; READ-mode Bolt transactions forbid mutation. Predecode structured
  allocation estimates and borrowed ACL stamping avoid large NULL-list/property
  decoding before budget checks. Policies cannot be supplied through query params.
- **Ingestion:** Python GraphClient supports single/batch upserts and partial
  property addition/removal for nodes/edges. A separate pinned DuckLake full-
  snapshot diff importer covers updates/deletes and resumes durable batch/source
  progress after lost acknowledgement. It uses real DuckLake with synthetic
  managed Parquet fixtures, not the live external application catalog or native CDC.
- **Replication foundation:** canonical concrete-mutation batches, SHA-256,
  strict contiguous per-store ledger, synchronous atomic state+ledger and latest
  replay idempotency. Three-store/checkpoint/failed-commit tests pass. No consensus,
  serving integration, leader election, quorum or machine-loss guarantee.
- **Verification:** 590 workspace tests, zero ignored; Clippy with warnings denied,
  fmt and 21 Python units passed. DuckLake 11 tests and protected API/ACL/TUI
  restart/checkpoint fixture passed against native SHA256 `843054cd…5dd00`.
  Compose, isolated protected Swarm and private-kind Kubernetes recovery/restore
  passed against image `01eb88e2…334cb`, with owned-resource cleanup. Full
  3,600.010-second 100k synthetic load passed: 4,991,261 reads, 59,427 writes,
  no query-pool rejections; point p50/p95/p99 1.421/9.783/16.160ms, scalar
  1.485/10.061/16.546ms, write 4.911/17.790/29.583ms. Sampled RSS peak
  76.625MiB. SIGKILL/reopen preserved 100k nodes and latest 200 acknowledged
  updates/indexes; audit passed. See final candidate report/strict verifier.
- **Review:** Astra reviewed integrated paths and the final borrowed setter. An
  earlier candidate was superseded and its partial hour intentionally stopped;
  evidence stays in `docs/benchmarks/2026-10-13-beta-acl/pre-stamp-fix/`. The corrected
  candidate is qualified separately. One existing authentication timing test failed
  under concurrent builds; isolated and final full runs passed without changing it.
- **Next step:** obtain the real source catalog/mapping and authoritative ACL
  source; implement a capability-gated external application adapter and atomic ACL ingestion;
  qualify representative governed workload. Replication is a separate reviewed
  workstream in `docs/design/replication-next-gates.md`, not extra replicas.
- **Limits:** ACL list bound 256 vs larger external application knowledge-grant corpus; static
  service policy only, no dynamic per-request binding. Snapshot differences scan
  full pinned snapshots and need exclusive source ownership. No live catalog
  provided, no production cutover, native CDC, ANN/text index or history claim.

### 2026-10-13 — Weak/strong connected components through Cypher

- **Requested:** graph algorithms in Cypher, steering the session after a request
  for node ACL, adapter integration and distributed deployment/replication.
  Started 09:24:52 UTC / 11:24:52 Europe/Amsterdam. Work remains local.
- **Done:** `CALL graph.wcc` and `CALL graph.scc` return `node`, representative
  `component` node and component `size`. Representatives use minimum internal
  NodeId; aliases, representative properties, WHERE/count and graph readers work.
  All nodes participate, including isolates after relationship-type filtering.
  Unit topology ignores stored weight values; `weightProperty` is rejected.
  WCC uses union by size/path compression; SCC uses iterative Kosaraju with a
  sparse transpose. No recursion, fallible algorithm buffers and periodic
  interruption checks, including high-degree scans and transpose construction.
- **Memory:** all CSR analytics projections now reserve conservative growth
  through the existing shared query pool before allocation, preserving original
  pool/per-query/deadline errors. Components precharge their algorithm allowance.
  Accounting remains conservative, not an allocator/RSS cap; storage calls and
  CSR sorting can overrun cooperative deadlines. Results remain eager, and
  WHERE/LIMIT apply after computation. No new dependency.
- **Verification:** 554 workspace tests passed, zero failures or ignored tests;
  workspace Clippy with warnings denied, formatting and whitespace checks passed.
  Differential connectivity tests, 100k chains/rings, overlay/snapshot/rollback/
  reopen, strict validation, read-only access and pool rejection/release passed.
  Astra reviewed the integrated change without blocking findings.
  Protected Python Neo4j 6.3.0/Bolt qualification passed for WCC/SCC exact rows,
  zero-weight edges, reader access/write denial and server restart; owned server
  processes and temporary databases were removed.
- **Measured:** release example asserts 1,000 WCC and 50,500 SCC on 100k nodes.
  Final five-warm-sample medians: 99,500 edges WCC 379.60 ms / SCC 303.98 ms;
  972,500 edges WCC 325.17 ms / SCC 391.07 ms. Host/load/cache variation means
  these small sets are not a scaling law or production percentiles. Timings
  include projection and eager Cypher output; 256 MiB accounting limit, no RSS
  measurement. See `docs/benchmarks/2026-10-13-components.md`, raw receipts and
  source/binary provenance. Earlier frozen beta image/receipts were not changed.
- **Next step / pending:** node-level ACL, the actual external application adapter contract,
  Kubernetes/Swarm qualification of the new candidate and real replication
  remain outstanding. Algorithms are single-process over one graph snapshot;
  neither distributed execution nor persistent community IDs were introduced.
  No UI changes or remote push/publication. Preserve user-owned untracked
  `docs-site/` and `.DS_Store` files.

### 2026-10-13 — Protected beta workspace and comprehensive failure qualification

- **Requested:** full two hours toward beta, followed by a large final simulation.
  Started 06:48:45 UTC / 08:48:45 Europe/Amsterdam; requested end 08:48:45 UTC /
  10:48:45 local. Session closed 2026-10-13 08:49:49 UTC /
  10:49:49 local, after the full requested two hours.
  All final runtime resources were removed.
  All work, images and containers remain local; no remote publication or push.
- **Done:** independent requested-owned PackStream allocation quotas with checked
  pre-reservation/fallible exact allocation, depth/minimum-encoded-length guards,
  and independent native-client response decoding bounds. Server configuration
  adds decoded-message and authenticated complete-request IO settings with file/
  environment provenance. Default network IO is 300 seconds; incomplete bytes
  and NOOPs cannot renew it. IO failures close sessions and abort pending state.
  Blocking engine work remains outside the network deadline. Responses now encode
  one frame at a time instead of adding an aggregate encoded response vector.
- **Done:** one connected read-only Ontology/Memory/Indexes/Graph workspace with
  Tab/1–4, lazy fetches, retained selection, refresh/stale recovery and navigable
  permission errors. Typed Graph checks existence and samples up to 20 outgoing
  neighbors in one read transaction; 21st row only indicates further matches.
  Thirteen actual 100k-node EN/PT PTY images cover all panes, compact layout,
  scrolling, stale recovery and forbidden Memory navigation. Full PNG decode,
  dimensions and hashes are verified; chosen green/cyan identity retained.
- **Done:** optional pinned single-host HAProxy TLS overlay, verified CA/hostname
  checks on drivers 5.28.6/6.3.0, denied plaintext/CA/hostname/credentials, indexed
  container writes, restart/audit and reconnection after proxy idle expiry.
  Producers use liveness_check_timeout=30 below proxy 60/native 300 seconds.
  Backend bridge and native TUI remain plaintext on the trusted side.
- **Done:** actual SIGTERM acknowledged-write/pending/uncertain-RUN/stopped-reader
  recovery, decoded-amplification and authenticated-drip/pool-expiry checks,
  production ten-second admission with 64 peers, and local protected deployment
  template rendering. No graceful drain, exact COMMIT-phase kill hook, immediate
  cancellation or Kubernetes/Swarm runtime qualification is claimed.
- **Verified:** 541 Rust workspace tests, zero failed/ignored, all-target Clippy
  warnings denied and formatting; 17 Python-example and 23 external application tests.
  Frozen native SHA-256 1042c946cb85d6c6d5b234db85163f3478ab505f20b4fb896e3b8e515d963150;
  Linux image brainwire:beta-20261013 ID
  sha256:d08c2e5af98ea237b568b86bf58af99af7d80ae5425ad7aaecdc37f620d821d3.
- **Verified:** continuous 3,600.003-second synthetic 100k-Customer workload:
  18,865,545 total primary/scalar reads and 65,927 throttled indexed updates.
  Read p95 1.557/1.623 ms, write p95 6.214 ms; sampled peak RSS 91.8 MiB over
  719 observations, zero observed pool rejections. SIGKILL/reopen retained latest
  200 acknowledged values/indexes; full offline audit OK. Neither RSS ceiling,
  write saturation, historical versions nor production SLO is inferred.
- **Verified:** final rehearsal after the completed hour: pinned SQLite DuckLake
  100k tickets/200k comments/200k edges (8,303 input rows/s), lost-ack/pending-cursor
  recovery and fresh checkpoint restore, three isolated graphs of 100k nodes
  with concurrent insert/overwrite/read and restart, and 99,999-neighbor hub with
  1,000 warm LIMIT-20 reads (p95 0.396 ms) plus 20 complete CLI snapshots.
  Corrupted independent SST-copy footer fails with exact RocksDB bad-table-magic
  diagnostic; original/checkpoint remain intact. A corrupted neighboring store
  blocks eager offline catalog opening; namespaces are not corruption domains.
- **Evidence:** [qualification report](docs/benchmarks/2026-10-13-beta.md),
  [20-receipt manifest](docs/benchmarks/2026-10-13-beta/validation.json), hashed logs,
  actual RSS/latency figure and [TUI captures](docs/design/assets/2026-10-13-beta/README.md).
  Standard-library collector has negative checks against false PASS, stale verdicts,
  missing/corrupt/mismatched receipts/assets, insufficient durations and profiles.
  Advisory final review independently obtained the same passing manifest.
- **Decided:** preserve the full-product backlog and October 31 target. Remaining
  October 14–31 cadence is 18 x 20 minutes / six hours; no automatic work scheduled.
  Synthetic qualification supports a controlled rebuildable single-node shadow
  beta, not a production or fully distributed release. Pre-existing docs-site and
  .DS_Store files were preserved; unsupported site examples are queued explicitly.
- **Next step:** capability-gated external application ingestion adapter against a disposable
  named graph and real required query/mapping fixtures. The actual GraphRepository
  suite has no Brainwire branch; node ACL, pagination, dynamic DDL/introspection
  and per-request timeout contracts are concrete compatibility gaps. Governed
  production reads must fail closed until equivalent enforcement is tested.
  Then qualify the selected protected runtime and representative workload.
- **Blockers:** real external application mapping/catalog/backend/workload approval, node ACL
  integration, protected target-cluster execution (kind/node image absent here),
  graceful drain/cancellation/streaming, native TUI TLS, Raft, search/history/
  retention/object-storage recovery and reconciled/published docs remain pending.

### 2026-10-12 — Protected external application MVP source bridge and connected inspection

- **Requested:** one additional hour, as much useful MVP implementation as possible,
  then reorganize the beta queue through October 31. Started at 17:01:27 UTC /
  19:01:27 Europe/Amsterdam; requested end 18:01:27 UTC / 20:01:27 local.
  Session closed at 2026-10-12 18:01:33 UTC / 20:01:33 local, after the full requested hour.
  All implementation and release artifacts remain local; no remote publication.
- **Done:** bounded DuckDB SELECT → normalized immutable JSONL export and optional
  read-only local SQLite DuckLake export pinned to an explicit snapshot version.
  Both reuse the external application mapping/importer; active caller transactions, partial
  publication and destination overwrite are rejected. DuckLake path overrides and
  implicit extension installation are disabled. Synthetic old snapshots survived
  newer source UPDATE/DELETE. Added Unicode `toLower`/`toUpper` with null/type
  semantics, conservative allocation accounting and indexed SET/rollback tests.
- **Done:** authenticated connected Memory/Indexes TUI panes, EN/PT explanations,
  exact/human byte units, manual refresh, sample age, stale/recovered state and
  index selection/scrolling. Memory requires instance administration; graph readers
  can inspect indexes. Seven actual interactive PTY screenshots preserve the green
  identity. The optional observer credential is explicit and private; default
  writer/reader/health grants remain non-administrative.
- **Done:** independent assembled Bolt request caps, early announced-oversize
  rejection and trailing PackStream rejection; absolute ten-second negotiation
  and authentication IO deadlines including response writes and LOGOFF re-login.
  A read cursor removes repeated buffer shifts while decoding NOOPs/coalesced
  messages; structural fragmentation/compaction regressions cover the optimization.
  Blocking engine work is not cancelled. Real listener tests exhausted/released
  64 slots before and after HELLO; Python drivers 5.28.6/6.3.0 and protected Linux
  Compose passed denial, import, transaction isolation, SIGKILL/reopen and audits.
- **Evidence:** [MVP qualification](docs/benchmarks/2026-10-12-mvp.md) records
  candidate hashes and separate build phases. Final protected DuckLake bridge:
  100K tickets + 200K comments + 200K edges, 300K input rows / 291,493,330 bytes,
  export 3.032s and import 28.080s on this synthetic local workload. A separate
  100K-ticket recovery harness exercises lost receipts, pending data/cursor
  rollback, replay and independent checkpoint restore. Workspace checks and
  Python helper tests passed; final counters are in the qualification receipt.
  The earlier-build 30-minute mixed load passed with 9.38M reads and 32K updates.
  Final candidate additionally passed 10 continuous minutes with 4.08M reads,
  10,681 updates, point/index p95 1.045/1.072ms and sampled RSS peak 93,424 KiB.
  Both verified latest acknowledged values after SIGKILL/reopen and audited indexes;
  these separate periods do not satisfy the one-hour continuous endurance gate.
- **Decided:** propose a controlled single-node external application shadow beta by October 31,
  with 19 remaining 20-minute sessions / 6h20. The full requested product matrix
  remains visible; replication, ANN/search, history, maintenance, object-storage
  support, fully connected operation and MCP are not silently declared complete.
  Source row throughput is not graph-mutation throughput or production capacity.
- **Next step:** October 13 protected transport boundary (same-host loopback or
  verified encrypted tunnel/proxy), then admission/slow-client qualification and
  the supervised shutdown/recovery boundary. Run the
  [local MVP quickstart](docs/mvp-quickstart.md); see the
  [dated beta queue](docs/2026-10-pilot-roadmap.md) for source mapping, actual backend,
  packaging, target execution, restore, performance and release decision gates.
- **Blockers/limits:** actual external application mappings, catalog/storage access and SLOs
  need Allan's input. No TLS, native drain, authenticated idle bound or replicated
  failover claim. Connected refresh is synchronous; memory shows query reservations,
  not RSS/cache. SELECT is trusted, external mutable files are not transaction-pinned,
  and source filename publication is not directory-fsynced for power-loss durability.
  DuckLake managed files must survive snapshot retention. Kubernetes/Swarm protected
  target execution and one-hour continuous candidate endurance remain unqualified.

### 2026-10-12 — Pods, machines and load-balancing concepts

- **Requested:** 30 more minutes focused on distribution, Kubernetes, Docker,
  containers, Pods, machines and load balancing. Started at 16:22:31 UTC /
  18:22:31 Europe/Amsterdam; the requested end was 16:52:31 UTC / 18:52:31 local.
  Session closed at 2026-10-12 16:52:41 UTC, after the full requested interval.
  Continued the concepts-only scope of the preceding session. No engine, TUI
  or deployment implementation was performed; all changes remain local.
- **Done:** [deployment and routing companion](docs/design/2026-10-12-distributed-deployment.md),
  linked from README, the orchestrator guide and capacity notes. Distinguished
  logical instances/members from containers, Pods/tasks, hosts and volumes;
  separated client entry, named authenticated peer traffic and operations.
  Covered routing alternatives, transaction affinity/unknown outcomes, external
  advertised addresses, entry availability, host/zone placement, storage
  provenance, per-member resources, bootstrap cycles, health phases and drain.
  Added proposed TUI deployment fields and a proof sequence that includes crash
  recovery from the first stage; later gates consolidate measured evidence.
- **Decided:** retain three proposed complete-instance voters on independent
  machines. A transport load balancer cannot supply write authority, transaction
  migration or peer identity. A pure TCP entry requires a defined endpoint
  purpose/conservative destination; read-mode routing needs an application
  contract. Kubernetes OrderedReady and readiness-dependent peer DNS can cause
  separate bootstrap deadlocks. Swarm manager quorum is distinct from database
  quorum. Kubernetes API-dependent routing updates can fail independently of
  ongoing peer progress; no universal DNS/LB/storage survival is assumed.
  Orchestrator counts, stop-first and PVC access modes are not universal
  membership or fencing proofs. A PDB constrains supported voluntary evictions,
  not all failures or controller updates; its availability count is not quorum.
- **Recovery boundary:** a valid lagging follower can catch up while retaining
  its durable votes/logs. A known voter backup needs controlled restore or
  replacement; historically valid local IDs do not universally detect rollback.
  Whole-instance disaster restore and member replacement are different lineage
  procedures. No automatic failover or distributed durability is claimed today.
- **Verification:** reviewed current server, Bolt and deployment files;
  checked official Kubernetes/Docker/Bolt references with read-only explorer,
  researcher and focused Astra review. Added ten future deployment-fault
  exercises with required evidence; these are not executed cluster tests.
  Local document references, code fences and whitespace passed. No runtime
  behavior changed and no Rust or real-cluster tests were needed for this scope.
- **Educational visual:** 16 illustrative platform/placement/failure combinations
  in `/Users/fraga/.codex/visualizations/2026/09/26/01a0de61-91ef-7403-b7d7-5f354611fe90/brainwire-deployment-explorer.html`.
  Wide and narrow browser checks passed, with no JavaScript errors. It is a
  teaching diagram, not a consensus simulator. Owned preview servers and tab
  were closed; the temporary browser viewport was reset.
- **Next step:** resolve the mutation/apply/retry durability contract, then
  choose fixed-member bootstrap, routing, health and drain contracts before
  claiming a three-member topology. Test protocol/process failures before
  distinct-machine Kubernetes and Swarm acceptance and recovery/load evidence.
- **Blockers:** Raft replication, member identity, ROUTE/bookmarks, native TLS
  and application-level drain remain unimplemented. This session does not
  approve a new implementation phase or convert current manifests into HA.

### 2026-10-12 — Distributed systems concepts only

- **Requested:** one hour of distributed systems concepts only, starting
  2026-10-12 14:29:41 UTC / 16:29:41 Europe/Amsterdam. The session's requested
  end was 15:29:41 UTC / 17:29:41 local. Conceptual work and verification
  completed at 15:29:48 UTC, after the full requested hour. No engine, API,
  TUI or deployment implementation was performed; all work remains local.
- **Done:** [consistency and recovery contracts](docs/design/2026-10-12-distributed-concepts.md),
  [21 future failure exercises](docs/design/2026-10-12-distributed-scenarios.md)
  and [capacity/operations concepts](docs/design/2026-10-12-distributed-capacity.md).
  Developed durable commitment versus published apply, separate graph/system
  store recovery, proposal-aware conflicts, pre-COMMIT ID reservations,
  read barriers/bookmarks, pending and retained retry deduplication, graph
  incarnation and recovery lineage, whole-instance snapshots, safe member
  replacement, version/policy compatibility and source-cursor boundaries.
- **Decided:** retain the approved direction of a complete-instance Raft group;
  three independent voters is the proposed first topology, not sharding.
  Container scheduling does not provide database consensus. Retained-volume
  restart, learner replacement and disaster restore are separate procedures.
  Object storage is initially proposed for independent backup/snapshot/blob
  artifacts; each voter still needs durable filesystem storage. A partition
  can require safe refusal. Full-copy replication does not preserve deleted
  history or recover commits newer than a disaster backup.
- **Open integration choices:** deterministic command format and size bounds;
  serialized admission versus pending conflict/operation reservations;
  graph/system publication and lifecycle replay; bulk durability/activation;
  retry retirement and lineage; policy generations, peer identity, compatible
  feature activation and consistent snapshot install/retention. The approved
  WAL-disabled bulk wording needs an explicit distributed recovery contract;
  current Python resumable batches use synchronous transactions.
- **Educational visual:** eight independent proposed failure scenarios in
  `/Users/fraga/.codex/visualizations/2026/09/26/01a0de61-91ef-7403-b7d7-5f354611fe90/brainwire-distributed-failures.html`.
  Browser checks covered every selector case, narrow/wide layout and no
  JavaScript errors. It is not a Raft simulator or a running cluster. Owned
  temporary preview server and browser tab were closed.
- **Verification:** primary Raft/OpenRaft, RocksDB, etcd, Bolt, Kubernetes and
  Docker documentation checked; focused Astra conceptual reviews incorporated.
  Majority/placement arithmetic, document links/fences and whitespace checked.
  The failure exercises were not executed and no distributed benchmark was
  claimed. The current binary remains single-node.
- **Next step:** review the mutation/apply contract before any distributed
  implementation. A bounded experiment should first cover one fixed graph's
  node/index/edge/cursor changes, exact replay identity and publication crash
  boundaries, then integrate three authenticated members and apply the failure
  ledger. Define graph lifecycle limits explicitly rather than bypass consensus.
- **Blockers:** replication, routing and bookmarks are not implemented. Target
  failure domains, durability assumptions, recovery/RPO promises and the open
  contracts above need concrete acceptance before a production claim. This
  conceptual session does not approve or begin a new implementation phase.

### 2026-10-12 — Protected Cypher, connection APIs and deployment profiles

- **Requested:** one hour starting 13:26:17 UTC / 15:26:17 Europe/Amsterdam;
  then explicit focus on Cypher, Docker Swarm, connection APIs and Kubernetes.
  Finished at 2026-10-12 14:26:38 UTC, after the requested hour.
  Worked in the standalone repository; kept everything local.
- **Done:** strict optional high-entropy service-token policy loaded before bind,
  per-session exact named-graph read/write grants, separate instance telemetry
  permission and whole-AST Cypher authorization before execution/ID allocation.
  LOGOFF/re-auth abort staged writes and discard results; RESET cannot restore
  revoked identity. Unknown procedures/impersonation fail closed. Development
  mode remains explicit, unprotected by default and warns at startup.
- **Cypher/API:** lazy `coalesce(...)` with semantic/depth/memory validation;
  shared strict credentials-file loading in Python and native probe/connected
  ontology clients; private service-token generator. Real driver 5.28.6 and
  6.3.0 acceptance covers graph selection, denied mutations, snapshot importer,
  module/direct-script Customer import/resume, sparse analytics and explainable
  ranking, token revocation after restart and offline index audit.
- **Deployment:** optional Compose/Swarm/Kubernetes secret overlays, independent
  read-only health identity and local manifest/render checks. Fresh Linux Docker
  image passed real authenticated health, denial and 1,000 indexed writes. A
  protected explicit transaction remained open at SIGKILL; recreation retained
  confirmed records/old index and discarded its pending node/index changes.
  Temporary owned containers/volume were removed. Swarm/Kubernetes protected
  profiles were rendered only, with no target-cluster mutation.
- **Measured:** protected 100K-node, four-reader/one-writer 900-second observation:
  4,603,488 reads and 16,449 updates; identity/scalar-read p95 1.680/1.708ms,
  update p95 5.272ms. Import 3.846s (~26,003 records/s); sampled RSS peak 91.75MiB.
  Distinct synthetic 1KiB payloads, ID/email responses, disjoint reader/writer
  keys, writer waits 50ms; unisolated Apple M5/32GiB host, 64/16/64MiB budgets.
  One local observation, not certification or a before/after speedup. SIGKILL
  after traffic preserved counts/latest acknowledged updates and index audit.
- **Additional load:** separate 600-second run overlaps identity reads with the
  writer's 200 hot records: 3,749,330 reads and 11,080 updates, hot identity p95
  1.225ms, scalar read p95 1.260ms and update p95 2.043ms; sampled RSS 101.19MiB.
  No request errors; post-traffic SIGKILL/latest-update/index audit passed. One
  throttled writer; this is not multi-writer contention or a linearizability test.
- **Verified:** 475 Rust workspace tests, all-target Clippy with warnings denied,
  rustfmt; 16 Python helper/importer + 3 adapter tests, real driver/runtime and
  profile checks. Four new protected Session+DbEngine tests exercise logout and
  relogin during both transaction modes, with reservations returning to zero.
  Astra advisory reviews found no blocking issue; main independently verified.
- **Decided/limits:** service tokens are random machine credentials, not passwords,
  JWT/JWKS or tenant/row ACLs. Native TLS, online policy reload and replication
  remain absent; policy changes require restart. Secret mounts and orchestrator
  scheduling do not encrypt Bolt or copy RocksDB writes. Preserve Observatory
  layout; only connected credential handling changed. User `docs-site/` and
  `.DS_Store` files remain untouched/uncommitted.
- **Next step:** execute protected profiles in a separately authorized target
  cluster and validate the chosen encrypted transport; then continue the
  DuckLake pinned-snapshot consistency/source identity contract and bounded
  importer bridge. Do not claim CDC, source deletes or machine-loss failover.
- **Evidence:** [session report](docs/benchmarks/2026-10-12-auth.md),
  [connection guide](docs/bolt-auth.md), [container guide](docs/protected-containers.md),
  [updated beta security checklist](docs/beta-auth-checklist.md).

### 2026-10-10 — external application snapshot adapter and recovery acceptance

- **Requested:** 20 minutes of active work, starting 14:22:11 UTC / 16:22:11
  Europe/Paris. Continued the October 9–10 pilot workload/fixture dependency.
- **Done:** added a runnable synthetic Ticket/TicketComment snapshot adapter,
  HAS_COMMENT edges, scoped immutable identities, fixed authoritative/null SET
  fields and parent validation. Reused the existing bounded Python/Bolt importer;
  checkpoint content SHA, mapping contract and byte cursor now guard resume.
  Added deterministic fixture generation, real-server acceptance and a local
  quickstart with schema, queries and accurate TUI inspection instructions.
- **Measured:** 100K tickets + 200K comments + 200K edges imported in 25.101s
  (11,952 input records/s), driver 5.28.6, release, localhost Apple M5/32GiB,
  64MiB cache / 16MiB memtables / 64MiB query guard. One short synthetic run,
  highly compressible 1KiB comments and smaller tickets, host contention; no
  sustained throughput, p95, peak RSS, production or 1M-edge certification.
- **Correctness:** exact counts/index answers, tenant-qualified identities,
  overwrite/null index cleanup, duplicate-edge replay, missing/immutable parents,
  conflicting duplicate parents, mapping mismatch, rollback, pending-write
  SIGKILL/reopen and independent native checkpoint restore/resume/index audit.
  Driver 6.3.0 separately passed changed-source rejection and lost-ack partial
  resume. Raw receipts distinguish cases actually run at each size.
- **Compatibility/checks:** 438 Rust workspace tests, all-targets Clippy with
  warnings denied and rustfmt; 13 importer + 3 adapter Python tests; module CLI
  help, schema JSON, receipt and guide checks. Existing Customer failure suite
  passed after helper extension. Actual old Customer catalog/checkpoint without
  contract_id also resumed, replayed and accepted a new default source through
  Bolt with a clean index audit. Advisory Astra found no implementation blockers.
- **Finding:** a mass SET over 33,334 open tickets hit the unchanged 64MiB query
  guard. Acceptance records denial/rollback and then uses a bounded primary-key
  write for recovery; budgets were not increased to hide this limit. Allocator
  reservations may survive rollback; the source byte cursor remains authoritative.
- **Boundary:** external application starter graph mapping is empty. Documented Zendesk
  ontology fragment supplied candidate concepts, not a confirmed live mapping.
  Synthetic data only. Snapshot revisions require new source IDs and serial,
  chronological application per tenant/connector. No CDC, delete synchronization,
  source-version arbitration, external edge repair or new connected TUI feature.
- **Next:** implement protected Bolt authentication and graph/operation scopes
  across HELLO/LOGON, implicit RUN and explicit BEGIN. The read-only trace is in
  docs/beta-auth-checklist.md; consult Astra on policy/state/config first. Current
  AllowAll, RESET/LOGOFF behavior, read-only mutation checks and process-wide CALL
  telemetry need explicit denial tests. Do not expose current Bolt on a shared
  network. Confirm real external application export mapping before any live-data integration.
- **Evidence/local:** docs/benchmarks/2026-10-10-source-ingestion.md and its raw JSON,
  reproducible harnesses and final source inventory. Temporary stores and owned
  servers cleaned. All work local; no push, publication, PR or deployment.

### 2026-10-08 — October beta daily queue revised

- **Requested:** organize the remaining month for an October 31 external application-testable
  beta toward production. Allan revised the cadence from 15 to **20 minutes/day**.
  October 9–31 gives 23 sessions / 460 minutes / 7h40, excluding today's completed
  implementation. This planning turn is not another timed coding session.
- **Done:** revised docs/2026-10-pilot-roadmap.md with a dated queue, security
  early, DuckLake snapshot ingestion, Python replay/overwrite, install/restart/
  restore, representative performance and one-hour acceptance evidence, real
  TUI/docs, candidate freeze and three final buffer days. Added the cadence to
  AGENTS.md so future sessions retain it. No automation or deployment created.
- **Boundary:** proposes a controlled single-node external application shadow beta; the full
  requested matrix remains visible and is not credibly guaranteed in 7h40.
  Replication, ANN/search, complete history/maintenance, all source connectors,
  object backup, full connected TUI/MCP remain requirements, not completed work.
  Production status requires security, recovery, workload and explicit deployment
  authorization. DuckLake snapshot/export ingestion is not CDC or delete sync.
- **Review/checks:** advisory Astra checked priorities/dependencies; main applied
  the security-first order and protected freeze/fix buffer. Documentation-only:
  arithmetic/dates/links and git diff checks; no engine tests rerun unnecessarily.
- **Next:** October 9 session freezes the external application schema/IDs, representative
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
  before external application integration. CSR projection/PageRank were not changed today.
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
  validate this policy against real external application questions and provenance requirements
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
  and a polished TUI. Assessment: a tested external application pilot is plausible; full
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
- **Immediate external application pilot:** Allan asked whether testing this week is viable.
  Proposed a separate Docker graph, a small ontology, Python individual/batch
  writes and representative queries/recovery checks. Audit external application's actual
  Cypher before pointing it at Brainwire; automatic DuckLake ingestion and
  connected TUI remain pending. No existing external application service was reconfigured.
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

- **Requested:** automate fast external application ingestion, primarily DuckLake, also
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
  `external_memory` empty. The 100K fixture's directory was 9.5 MiB on this
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
  The local external application Python environment has Neo4j driver 6.3.0; the historical
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
- **Commercial direction discussed:** external application can be the first design partner
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
  (`external-core/pyproject.toml`). With Allan's OK, neo4j 5.28.6 went into a
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
  `external-core/.venv`, so nothing was installed; nesting limit of 64 levels
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
  cluster/HA, RBAC and full openCypher in scope. The ACL follows the external application
  model (`AclPredicate`), native to the engine. No commit until the spec.
- **Next step:** the spec in five sections, each approved before the next:
  (1) storage; (2) data model and security: ontology, ACL, RBAC, events;
  (3) query, protocols and SDK; (4) algorithms, AI and CLI; (5) cluster,
  operations and tests.
- **Blockers:** the open items listed in the plan.
