# Changelog

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
