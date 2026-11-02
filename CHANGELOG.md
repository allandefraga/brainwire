# Changelog

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
