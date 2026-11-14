# Changelog

## Unreleased

## 0.21.0-beta.2 — experimental candidate

- Record a new single-run million-node comparison and a separate 100,000-node
  DuckLake acceptance, with their conditions and limitations.

## 0.21.0-beta.1 — experimental candidate

- Add an incremental local DuckDB snapshot importer with a private resumable
  journal, bounded batches and explicit deletion ownership. It compares
  snapshots rather than consuming CDC and requires a single-writer contract.
- Add bounded `db.assertNodeState` transaction guards for standalone writes.
  DuckLake synchronization verifies each batch's lease, cursor and mapping
  contract in the same transaction. Replicated mode is not supported.
- Let explicitly opted-in search providers receive a bounded candidate scope
  before ranking. Graph visibility and scope are still enforced on returned IDs.
- Fix snapshot import CLI status reporting after a successful import.

## 0.20.0-beta.2 — experimental candidate

- Reduce intermediate allocations for label-qualified `entity_id` lookups,
  preserving transaction snapshots and node visibility checks. Clarify that
  `db.propertyKeys()` describes declared catalog fields, not arbitrary extras.
- Bound accounted query growth by a periodically sampled cgroup margin as well
  as RSS. Stale or unreadable known limits refuse new growth; the change adds
  no cgroup gate to Bolt ingress. This is not a global RSS ceiling.
- Reject unsupported timestamp precision and timezone offsets in the timeline
  example instead of silently truncating event times. Accepted inputs use `Z`
  or `±HH:MM` and at most six fractional digits.
- Add an authenticated Bolt acceptance test for a concurrent ACL revocation:
  a stale stamp in the same transaction conflicts, and the revocation survives
  restart. Document the separate risk of carrying cached ACLs into a new
  transaction; this is not a general serializable-read guarantee.
- Repeat the synthetic one-million-node comparison three times on a local
  candidate image, retaining the raw receipts and a reproducible median chart.
- Keep the ingestion safeguards and synthetic benchmark evidence from beta.1.

## 0.20.0-beta.1 — experimental candidate

- Bind each DuckLake synchronization checkpoint to its source generation,
  mapping, graph, writer principal and endpoint policy. A changed or legacy
  contract is refused before workers start. This is a **breaking change**:
  supply `--source-identity` (or `source_identity=`), declare
  `ImportCheckpoint.contract_fingerprint`, and follow the
  [migration guidance](docs/parallel-ingestion.md#ducklake-full-load-then-incremental).
- Refuse Decimal values that cannot be represented exactly as floats instead of
  silently rounding; explicitly map source text or `DOUBLE` as appropriate.
  A failed import can have committed earlier batches and retains its lease.
- Run a small real DuckLake snapshot, checkpoint race and incremental-sync
  acceptance test in CI using a pinned DuckDB version.
- Add a reproducible million-node, two-million-edge synthetic comparison,
  including raw results, the failed first harness attempt, and process-restart
  checks. Separate runs use v0.19.0-beta.1 and a locally built v0.20 candidate,
  not the published v0.20 image. Settings differ between engines and neither
  run qualifies production performance.
- Record short 512 MiB, 2 GiB and 3 GiB cgroup pressure checks against a local
  candidate image, including refusal, rollback and recovery. These are not a
  hard RSS limit or a sustained-capacity qualification.
- Record a 30-minute synthetic 2 GiB read soak with delegated ACLs; the small
  dataset did not approach the memory limit.
- Simplify the README, add an agent quickstart and reusable prompt, and show the
  benchmark and its limits on the docs site. Clarify Colima bind mounts and
  current online-catalog capabilities.

## 0.19.0-beta.1 — experimental candidate

- Narrow provider-ranked context to an optional bounded set of candidate IDs
  before authorization and reciprocal-rank fusion. The scope is supplied by
  the application and is not a permission boundary or a shared snapshot.
- Add a runnable synthetic company-context example with a declared ontology,
  decisions, source references and a project-scoped lookup.
- Refresh the logo, documentation site and terminal palette. The terminal
  remains a read-only workspace for the capabilities described in its guides.


## 0.18.0-beta.6 — experimental candidate

- Reject clearly oversized strings and collections in node and edge batch
  writes and deletions before serializing a complete JSON copy.
- Keep existing byte accounting and per-batch commit behavior. This does not
  bound source, driver or server memory.
- Expose client-side oversized-item failures as `BatchTooLargeError`, a
  `ValueError` with the item type and configured byte limit that can be passed
  between Python worker processes.
- Exercise recovery of two independent graphs with uncommitted writes at a
  crash, and explain how to create another graph while the server is stopped.
- Record short 512 MiB, 2 GiB and 3 GiB pressure tests plus local mixed-load
  recovery diagnostics. These do not qualify a global RSS ceiling.
- Reduce a benchmark test's sensitivity to slow process startup.
- Document explicit private-CA trust for Python runtimes that ignore
  `SSL_CERT_FILE`; add an opt-in strict TLS test for that configuration.
- Measure generated code-graph JSON in small batches instead of making a
  second complete serialized graph solely to enforce the output limit.

## 0.18.0-beta.5 — experimental candidate

- Reject oversized context IDs and search text before creating a UTF-8 copy.
  This is input validation, not a process-wide memory limit.

## 0.18.0-beta.4 — experimental candidate

- Check nested context values before serialization and refuse values over the
  byte budget or 256 levels deep without materializing a large JSON copy.
  Driver allocations and process RSS remain outside this bound.

## 0.18.0-beta.3 — experimental candidate

- Reject obviously oversized context string properties before making an extra
  JSON copy. Driver allocations and overall process RSS remain unbounded.

## 0.18.0-beta.2 — experimental candidate

- Check context-search and reranking JSON size incrementally, avoiding a full
  extra serialized response allocation while preserving the output limit.

## 0.18.0-beta.1 — experimental candidate

- Explore bounded visible instance paths in outgoing, incoming or mixed
  directions using the Python context helper.
- Quarantine a store after an uncertain native commit result until reopening,
  including stores without an identified-request ledger.
- Account for a conservative write-batch materialization estimate before
  standalone Cypher commits; refuse work exceeding the query budget.
- Harden credential-file loading in Python examples against named pipes and
  group/world-writable files on Unix.

## 0.17.0-beta.3 — experimental candidate

- Explain the RocksDB write path, SST levels and compaction with a diagram in
  the README and practical storage guidance in the architecture guide.
- Retain the query-memory and quorum recovery improvements in beta.2.

## 0.17.0-beta.2 — experimental candidate

- Reduce avoidable intermediate copies in Cypher value inspection and list
  functions; query budgets and other materialization still apply.
- Validate persisted quorum request records and the initialization marker at
  startup, rejecting malformed encodings while preserving the existing format.
- Simplify the documentation, add architecture diagrams, and archive historical
  benchmark artifacts without changing their recorded data.

## 0.17.0-beta.1 — experimental candidate

- Add bounded schema-path discovery, ACL-aware instance path samples, and
  configured-provider context search with rank fusion. Search providers are
  supplied by the application; this does not add native vector or text indexes.
- Improve connected TUI navigation and explain the scope of graph and memory
  views. Refresh the logo, documentation site, installation guide and README.
- Add unit tests and authenticated integration checks for the context APIs,
  including visibility of intermediate nodes in graph paths.

## 0.16.0-beta.3 — experimental candidate

- Reuse a ready relationship property index for eligible keyed `MERGE` calls;
  otherwise retain the pair scan. Full key validation and conflict detection
  remain in place. No new on-disk format is introduced by this optimization.
- Account for peak temporary key decoding rather than summing allocations
  already released during a keyed scan; the matched full record is still
  admitted before an update.
- Add a paginated explicit-event timeline example, a standalone `SHOW INDEXES`
  alias and advisory retained-log capacity in Raft diagnostics.
- Add reproducible synthetic diagnostics for high-degree keyed writes and
  mixed reads and writes followed by an abrupt restart.

## 0.16.0-beta.2 — experimental candidate

- Narrow eligible relationship-type scans to catalog endpoint labels, including
  endpoint access checks. This is not a constant-time count store.
- Add opt-in relationship property indexes for standalone graphs, with online
  population and TUI status. Enabling them changes the disk format; read the
  [upgrade and rollback guide](docs/relationship-property-indexes.md).
- Record a synthetic wide-graph comparison and short 2/3 GiB container
  pressure checks. These do not establish production memory or failover limits.

## 0.16.0-beta.1 — experimental candidate

- Declare immutable merge keys when creating a new relationship type online on
  a standalone server. Existing relationship identities cannot be changed.
- Send a single bounded Python node or edge batch with a caller-supplied request
  ID on supported servers. No automatic replay occurs; reconcile unknown outcomes.
- Inspect one standalone request outcome in the connected TUI. `not_recorded`
  is inconclusive and quorum lookup uses a different contract.
- Record synthetic event, crash and restart checks, including a 10,000-edge
  high-degree pair where lookup cost increased. Historical revisions, a global
  batch dashboard and production qualification are not included.

Earlier release details are available in [GitHub Releases](https://github.com/allandefraga/brainwire/releases).

## 0.15.0-beta.8 — experimental candidate

- Account for temporary row copies and check deadlines during Cypher list
  quantifiers; this does not guarantee a process RSS limit.
- Reject unreadable persisted Raft vote metadata at startup without rewriting it.
- Add an optional Kubernetes disruption budget for the single authenticated
  persistent instance. This blocks voluntary eviction, not machine failure.

## 0.15.0-beta.7 — experimental candidate

- Apply `TCP_NODELAY` to accepted Raft peer sockets, matching outbound sockets
  and avoiding a possible Linux delay between the frame header and body.
  A [local peer diagnostic](docs/benchmarks/2026-11-04-raft-peer.md) records
  the same test before and after the change; it is not a production SLO.
- Fix the release-version check for the refreshed documentation layout. The
  beta.6 tag remains available, but its GitHub verification did not complete.

The keyed-relationship, integration, memory, security and visual improvements
from beta.6 are included below. Distribution remains experimental.

## 0.15.0-beta.6 — experimental candidate

- New relationship types can opt in to immutable string merge keys in their
  ontology, allowing distinct events between the same endpoints. Existing types
  keep their endpoint-pair identity; conversion requires a new graph and source
  reingestion. Keys use a budgeted pair scan rather than an index.
- The Python single-edge update now matches an existing edge without creating
  one. Pair-based DuckLake edge synchronization rejects keyed types before
  acquiring a lease, and the schema reports configured keys.
- Connected schema inspection accepts the new metadata. All writers and
  voters must be upgraded before using keyed graphs; older binaries can ignore
  the policy. Do not downgrade a keyed volume.
- Account and deduplicate `entity_id IN` candidates before primary-key seeks,
  and check deadlines during candidate processing. Native Unix Bolt clients
  reject credential files writable by group or others.
- Refresh the graph logo, documentation colors and TUI selection hierarchy.
  Extend the local quorum acceptance test to cover keyed edges across failover
  and restart; this is not multi-machine qualification.
- Record a short synthetic 100,000-node load and restart in a 2 GiB container;
  this is not sustained-load qualification or a cross-database comparison.

This does not add native temporal history, recover overwritten events, qualify
multi-machine failover, or guarantee a global RSS ceiling.

## 0.15.0-beta.5 — experimental candidate

- Preserve fresh RUN RSS observations for query reservation headroom until a
  later sampler confirms relief, including overlapping equal-valued readings.
- Reject Raft log entries with regressing leader identities, including a lower
  node ID in the same term; reopening an inconsistent log now fails closed.
- Add a bounded-row chronological reader to the explicit timeline example.
  It is application-managed, may scan and sort more rows, and is not a native
  history or pagination feature.

Relationship `MERGE` still keys by endpoints and type; existing integrations
using its properties to distinguish events need a model migration. A hard RSS
ceiling, native historical revisions and multi-machine failover remain open.

## 0.15.0-beta.4 — experimental candidate

- Add a Python example for recording explicit timeline milestones as separate
  event nodes, with deterministic IDs and transactional writes. This is an
  application-managed model, not automatic graph history.
- Reject regressing or incomparable Raft votes before persisting them.
- Relationship `MERGE` still identifies edges by endpoints and type; distinct
  property-keyed milestones on the same pair may overwrite each other. Existing
  integrations using that pattern need a model migration and may need to
  reingest missing events. The example does not change those integrations.

The example and local replication checks do not qualify production recovery,
multi-machine failover or a process-wide memory limit.

## 0.15.0-beta.3 — experimental candidate

- Retry a DuckLake sync transaction with bounded backoff only when the server
  explicitly confirms an `Outdated` conflict rolled back. Never retry an
  unknown commit outcome or a lost connection automatically. Failed workers
  still retain the source lease and do not advance its cursor.
- A synthetic 10,000-node, four-worker DuckLake/Bolt workload exposed
  contention between deletions of nodes sharing an edge in beta.2. The same
  workload passed twice after this fix. This does not qualify all workloads.

## 0.15.0-beta.2 — experimental candidate

- Sample process RSS at RUN admission as well as periodically, using the higher
  available reading. This reduces a sampling gap but does not create a hard
  process-wide memory ceiling; it adds up to two OS RSS reads per RUN.
- Reject regressing or log-inconsistent Raft commit markers before persistence.
- Add `toBooleanList`, `toIntegerList` and `toFloatList` for supported Cypher.
- Fetch and delete missing DuckLake snapshot keys in bounded Python batches.
  The DuckDB anti-join and graph enumeration have separate memory needs.

Native Bolt TLS, graph history, CDC, native ANN/BM25, and multi-machine failover
qualification remain open. Use rebuildable data for controlled beta trials.

## 0.15.0-beta.1 — experimental candidate

- Consider Linux cgroup v2 `memory.high` as well as `memory.max` when admitting
  new queries; diagnostics identify the selected threshold. This is cooperative
  admission, not a hard RSS limit or a guarantee against kernel throttling.
- Reject Raft log entries with a missing origin or regressing terms before
  persistence, so invalid appends cannot create a log that fails to reopen.
  Existing corrupt logs are not automatically repaired.
- Clarify memory accounting and the boundaries of replicated recovery.

Sustained memory qualification, cross-machine failover, snapshots and native
Bolt TLS remain open. Use rebuildable data for controlled beta trials.

## 0.14.0-beta.4 — experimental candidate

### Added

- `toBooleanOrNull` for supported boolean, string and integer conversions.
- Contribution and security reporting guides and structured issue forms.
- Query-pool utilization and unreserved budget in the connected TUI.

### Security

- On Unix, reject unsafe Raft peer-token file permissions and inspect the opened
  file before reading. Group-readable secret mounts remain supported.

### Limits

- Memory and multi-machine failover qualification remain open. A 512 MiB
  pressure smoke passed on the local beta.4 candidate; it does not resolve a
  prior failure or guarantee a process-wide RSS limit. Native Bolt TLS and
  vector indexes are not yet available.


## 0.14.0-beta.3 — experimental candidate

### Added

- Python `graph.schema()` returns the authorized catalog snapshot, including
  relationship signatures and index readiness.
- A synthetic quorum test checks identified-write replay after a synchronized
  leader is stopped and another voter takes over.

### Improved

- Resolve schema relationship endpoints with a budgeted temporary lookup,
  avoiding a repeated scan of node types for each signature.
- Stop reading context evidence as soon as its response byte budget is exceeded.
- Align the cgroup pressure smoke with the server's working-set estimate and
  require a validated cgroup refusal before treating a refusal as that evidence.
- Accept representable float values near signed 64-bit bounds in `toInteger`.

### Limits

- This remains an experimental beta. There is no qualified global RSS ceiling,
  native full-text/vector index or tested multi-machine failover. A previous
  pressure-test failure remains unexplained. Decimal strings near integer
  boundaries retain the existing more restrictive conversion behavior.


## 0.14.0-beta.2 — experimental candidate

### Added

- Bounded context evidence with explicitly mapped, application-declared source
  properties and stable record citation identifiers. Sources are not externally
  authenticated and identifiers do not represent historical revisions.
- Diagnostic logging of sampled process-group memory pressure decisions,
  without changing admission or promising a hard RSS ceiling.

### Fixed

- Install the pinned Bolt driver before Python helper tests in CI.
- Harden the synthetic comparison runner with pinned images, resource limits,
  ownership-checked cleanup and recorded runtime settings.

### Limits

- Native full-text/vector indexes and sustained production memory qualification
  remain open. A previous pressure failure is still under investigation.


## 0.14.0-beta.1 — experimental candidate

This release is intended for controlled trials with rebuildable data.

### Added

- A bounded Python context API and local hybrid reranking over explicit candidate IDs.
  This is not native vector search, ANN or BM25.
- Python source extraction with source evidence, optional inferred cross-file
  candidates, and local explain/path tools. Inferred links do not prove runtime calls.
- Optional ontology descriptions, a simpler Python connection helper and an
  animated synthetic walkthrough.

### Improved

- Reduce unnecessary Cypher projection and slice allocations and yield during
  large Bolt response encoding.
- Reject ambiguous delegated visibility metadata and invalid control message
  shapes; bound replication frame growth and validate peer configuration.
- Add replication recovery tests, limit index backfill batch counts, and reject
  invalid DuckLake batch sizes before acquiring a lease.

### Known limits

- One synthetic 512 MiB pressure run failed; two passed. The failure's cause
  remains unknown. This is not a global RSS cap or a sustained 2/4/8 GiB test.
- Quorum is experimental and local fault tests do not qualify multi-machine
  failover. Native Bolt TLS, native vector/text indexes, historical graph
  versions and CDC remain open. See README current limits.


## 0.13.0-beta.3 — candidate

### Added

- `CALL db.memory()` reports the configured process memory limit and a
  best-effort on-demand resident-memory sample. Missing values are `null`.
  The sample and guard do not guarantee a hard RSS ceiling.


## 0.13.0-beta.2 — candidate

Experimental beta for controlled trials. Image publication must be verified in
the release workflow; the tag alone does not guarantee availability.

### Fixed

- Avoid deep-cloning node and relationship properties while estimating the
  memory needed to return a path. The accounting estimate is unchanged, but
  this estimation no longer makes those transient allocations.
- On opening a replicated store, compare the persisted membership with the
  latest membership entry actually applied in its retained consensus log.
  A missing, stale or inconsistent marker fails startup instead of restoring
  a conflicting quorum configuration. Valid new stores and membership entries
  not yet applied remain supported. No data is rewritten or repaired.

- The log-cap recovery diagnostic recognizes the Python client's typed
  `CommitOutcomeUnknownError` and still reconciles the request through quorum
  before any retry.

### Limits and upgrade notes

- The membership check relies on the current retained-log model: snapshots and
  log purging are unsupported. An inconsistent store requires investigation and
  recovery from verified backups, not deletion of its metadata.
- This is not a production qualification. Query accounting and admission do
  not guarantee a process RSS ceiling, and sustained 2/4/8 GiB load, native
  Bolt TLS and multi-machine failover remain unqualified. See
  [current limits](README.md#current-limits).

## 0.13.0-beta.1 — candidate

Experimental beta for controlled trials. Publication and image availability are
confirmed by the release workflow; a tag alone does not prove either.

### Added

- `brainwire auth init` creates independent random admin and health credentials
  and a graph-scoped policy in a new private directory, without printing tokens.
  Setup is offline; changing the policy requires a restart.
- The release workflow checks credential creation using the image's unprivileged
  user on both native architectures. It does not verify host secret mounts.

### Fixed

- Bolt ingress accounts for assembled buffer capacity, including overlapping
  old and replacement allocations during growth. Small recovery messages remain
  bounded; real allocator over-allocation is checked after allocation.
- Docker and memory guides distinguish the sampled admission threshold from a
  hard RSS limit and describe persistent storage and protected access.

### Limits

- This is not a production qualification: sampled admission and ingress budgets
  cannot guarantee a global RSS ceiling or prevent OOM. Sustained 2/4/8 GiB
  workloads, large initial loads and multi-machine recovery remain open. Native
  Bolt TLS is unavailable. See [current limits](README.md#current-limits).

## 0.12.0-beta.1 — candidate

Experimental beta for controlled trials. Publication and image availability must
be confirmed by the release workflow; this file alone does not announce a
published release. Review the [qualification report](PRODUCTION-v0.12.md) before
using this version with real data.

### Added

- Cooperative process-memory admission through `memory.limit` and
  `BRAINWIRE_MEMORY_LIMIT`, with sampled RSS, memory-pressure logging and
  allocator purge requests. The default threshold is derived from detected
  capacity. This is an admission guard, not a hard process RSS ceiling. See
  [memory defaults](docs/memory-defaults.md) and the
  [process-memory design](docs/design/process-memory-ceiling.md).
- Experimental standalone write-outcome receipts on new dedicated volumes with
  `server.request_ledger = true`. Authenticated explicit transactions can supply
  a request ID, and a fresh autocommit `CALL db.requestStatus($seed)` can inspect
  its outcome. Graph mutations and the receipt share one synchronous storage
  batch. See [standalone request outcomes](docs/standalone-requests.md).
- Python `GraphClient.stream_cypher(...)` reads records incrementally in a
  scoped read-only session, without building one list of all rows. Server-side
  streaming remains limited to eligible query plans.
- Python `GraphClient.request_status(seed)` for that experimental endpoint and
  exported `BrainwireError`, `MemoryPressureError` and
  `CommitOutcomeUnknownError` exceptions. The helper preserves the original
  driver exception as its cause and does not retry these failures automatically.
- Reproducible synthetic diagnostics for allocator behavior, external cgroup
  pressure and sustained container load. Prepared diagnostics are not completed
  qualification results.

### Changed

- Eligible scalar reads and large parallel label scans stream bounded result
  pages over Bolt. RUN can return fields before its first page; an execution
  error may therefore arrive during PULL after RUN succeeds. Other entity plans
  can retain all row handles before property materialization. See
  [result streaming](docs/design/bolt-result-streaming.md).
- Bolt `PULL -1` writes pages incrementally, retaining each page's memory credit
  until it is sent or discarded. Finite `DISCARD n` consumes the requested
  number of rows; full discard cancels a lazy tail. Partial consumption keeps
  the transaction open.
- Memory refusals use `Neo.TransientError.General.MemoryPressure` with a terminal
  machine-readable hint describing the reason, requested/available bytes and
  rollback. This replaces the older memory-limit, query-pool and ingress-pool
  error codes. Failed COMMIT distinguishes rollback from
  `Neo.ClientError.Transaction.CommitOutcomeUnknown`. See the
  [memory and commit error contract](docs/design/memory-error-contract.md).
- **Python error compatibility:** the two recognized server codes now become
  `BrainwireError` subclasses of `RuntimeError`, rather than escaping from the
  helper as the original Neo4j exception type. Applications catching only
  `Neo4jError` must update those handlers. Other driver exceptions keep their
  original types. Parsed memory fields remain unset when the server hint is
  missing or malformed. See [Python errors](docs/python-client.md).
- Linux cgroup admission discounts recoverable inactive file cache when
  estimating the working set, avoiding refusal caused solely by that cache.

### Fixed and verified locally

- Process admission samples RSS at startup before serving requests, avoiding an
  initial zero-usage window. If sampling is unavailable, admission remains
  best-effort and emits a warning. It is not a hard RSS limit.
- Result-page reservations survive queueing, autocommit and response ownership;
  releasing the final response releases its memory credit.
- Request-status queries account for all decoded parameters and retain their
  response reservation through delivery.
- Regression tests cover finite/full discard, ACL and snapshot behavior,
  bounded LIMIT reads, memory refusal, rollback, RESET and synthetic restart
  recovery. Native RSS tests use isolated subprocesses to avoid interference
  from allocations in concurrent tests.

### Limits and upgrade notes

- Process admission is sampled and cooperative. Native allocations, compaction,
  socket buffers and other processes can still exceed the estimated headroom.
  Sustained 2/4/8 GiB qualification, the large initial-load scenario and the
  reported short-load peak-RSS regression remain open in the qualification
  report. No performance improvement or OOM guarantee is claimed here.
- Streaming is partial. Blocking ORDER BY, DISTINCT and aggregation retain
  intermediate state and refuse oversized work; they do not spill to disk.
  A PULL already executing cannot be preempted by a later request on the same
  Bolt connection, and some planning/scans delay the initial fields.
- The standalone request ledger is disabled by default. It cannot be enabled
  for an existing ordinary graph, and its format requires the opt-in to reopen.
  Checkpoint/backup/restore, fork lineage and old-binary downgrade remain
  unqualified or refused for this experimental format. Use a new dedicated
  volume. An absent receipt is not proof of rollback or permission to retry.
- Existing Cypher compatibility, transport and distributed-operation limits
  continue to apply. See [current limits](README.md#current-limits).
- The release workflow prepares and checks amd64 and arm64 images. Availability
  and per-platform digests must be verified after a successful workflow; they
  are not implied by these notes.
