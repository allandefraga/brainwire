# Brainwire transaction foundation

30-minute session requested by Allan, 2026-09-27 06:17:34–06:47:34 UTC.
Wrap up by 06:42. Execute the first two slices of the linked
[transaction preflight](2026-09-26-brainwire-bolt-transaction-preflight.md).
Spec: ../specs/2026-09-24-brainwire-design.md; engine MVP Task 5.
Use subagent-driven development, one implementer at a time, task review and
final integration review. Baseline: 595979a.

## Global constraints

- Canonical standalone /Users/fraga/Documents/brainwire, codex/brainwire.
  Preserve local work; no push, remote PR, platform checkout changes or publish.
- English code/comments/docs; Portuguese user conversation. Synthetic fixtures.
- TDD engine behavior; preserve existing CLI execute signature and semantics.
  No unsafe lifetime extension, leaked databases, fake transaction persistence,
  redundant executor implementation, or broad framework changes.
- This session lays transaction foundations. Database registry, real server
  adapter/worker, durable serve, connected TUI, auth and memory enforcement
  remain later work. The stub server is still a stub.
- Keep tests deterministic, use tiny RocksDB resources, do not leave servers
  running. Controller owns docs/journal, full workspace checks and final review.

## Task 1: shared Cypher transaction context

Own crates/cypher only (no manifest changes should be necessary).

Introduce exported QueryTransaction<'db> borrowing a Database through one Txn,
with a catalog loaded from that same snapshot. Public methods:
- begin(&Database) -> Result<Self, ExecError>
- run(&mut self, &str, &BTreeMap<String, Value>) -> Result<QueryOutput, ExecError>
- commit(&mut self) -> Result<Option<u64>, ExecError>
- rollback(&mut self), idempotent cleanup
Expose/query or internally encode Active, Failed, Committed and RolledBack
states distinctly. Document the lifecycle. A finished/failed context rejects
run and commit with a meaningful existing ExecError category. Avoid expanding
the public ExecError enum just for state errors.

Run does not commit. Every run gets fresh bindings/parameters, reads the same
catalog/snapshot, and sees its transaction's staged graph writes. Any parse,
validation or runtime error fails the transaction and immediately discards ALL
staged writes. Later run/commit cannot resurrect it. Rollback/drop discard writes.
Commit consumes the pending storage transaction once; conflicts fail the
context. Return Some(store sequence) for a write commit and None for successful
read-only completion, preserving the existing standalone read-only semantics.
Do not claim the sequence is a restart-stable causal bookmark.

Refactor existing clause execution into one implementation reused by both APIs.
Preserve execute(db,query,params) atomic convenience semantics and result/error
behavior. No savepoints or added language features. The user-facing CLI stays
unchanged. State transitions must safely release the borrowed storage snapshot.

First add failing integration tests: two runs read own writes while an outside
query cannot see them; commit/reopen persists both; rollback and drop persist
neither; late parse/semantic/runtime error discards earlier successful runs;
failed/finished contexts reject further use; query-local variables/params do not
leak; an external concurrent update remains hidden by the same snapshot;
deterministic write conflict; catalog migration conflict; read-only completion.
Prove a safe ownership path by moving an Arc<Database> into a bounded worker
scope/thread, creating the borrowed QueryTransaction inside it, performing
multiple operations and dropping it before the owned database. This is a
compile/test proof, not a production worker pool or async integration claim.
Run package tests/clippy/fmt, commit owned files, report evidence and API details.

## Task 2: Bolt lifecycle hooks

Own crates/bolt plus Cargo.lock if dev-dependency entries change; consumes lifecycle concepts from Task 1 but stays storage
agnostic. Add real begin, commit and idempotent abort hooks to QueryEngine,
updating every implementation/test. Require explicit hook implementations;
only StubEngine may acknowledge stateless synthetic transactions as such.
Commit returns optional engine-provided bookmark; remove session-fabricated
commit counters. A durable bookmark is not claimed by StubEngine.

Session must begin explicit transactions before SUCCESS and implicit
transactions before RUN; implicit commit happens when the result is exhausted
or discarded, before final SUCCESS. Explicit RUN/PULL do not commit. Commit
errors produce FAILURE, not final SUCCESS. Any failed query or protocol path,
RESET, GOODBYE, fatal message or Session drop aborts pending transaction state.
Lifecycle cleanup is idempotent. Never abort an already committed transaction
as if it were still active. Failed BEGIN causes later pipelined operations to
be ignored until RESET. Preserve existing protocol/auth-state behavior.

Bind a transaction to the selected database; explicit RUN metadata must not
switch databases silently. Validate incompatible/non-string database metadata
explicitly. Keep current single-result support; do not broaden qid/streaming.
Check local research/bolt.md before settling protocol details, and primary
sources for any new uncertain protocol claims.

Add deterministic instrumented-engine tests for ordering and propagation,
commit/rollback/abort/reset/drop, begin/run/commit errors, multiple explicit
RUNs, implicit partial/final PULL and DISCARD, and database selection. Network
cleanup should be tested with a synthetic engine if needed, not a real server
adapter. Keep all existing Bolt socket tests passing. No async/runtime rewrite.
Add one cross-layer integration suite using a test-only borrowed adapter over
Task1 QueryTransaction and a temporary real RocksDB Database. Dev dependencies
on cypher/store/core/tempfile/serde_json are allowed as needed. Exercise the
actual Session BEGIN/RUN/PULL/COMMIT and ROLLBACK/RESET/drop paths with real graph
writes, verify outside visibility/reopen, late-query rollback and conflict
reporting. The test adapter may support only the scalar output/empty-parameter
queries required by this fixture; document that it is not the production
PackStream adapter. This proves the two layers fit without adding serve/runtime.
Report what hooks now work and what remains before actual RocksDB-over-Bolt.

Protocol/lifecycle refinements verified during preflight:
- Mandatory trait signatures: begin(&mut self, db: Option<&str>) ->
  Result<(), EngineError>; commit(&mut self) -> Result<Option<String>, EngineError>;
  abort(&mut self) -> Result<(), EngineError>. Abort is idempotent; rollback
  uses it. Drop invokes best-effort abort only for pending/dirty ownership.
- Set pending ownership before begin, so a begin failure can clean partially
  initialized engine state. Clear pending after successful commit/abort.
  A cleanup error must never produce SUCCESS or reusable Ready; mark Defunct.
  Drop may retry cleanup still pending after error; do not fabricate success.
- PULL/DISCARD with a nonnegative n consume/discard at most n rows; n=-1 means
  all. Partial DISCARD must preserve has_more and must not commit. Keep existing
  omitted-n-as-all compatibility (its stricter protocol validation is separate).
  Reject other negative or noninteger n values explicitly before consuming rows.
- Socket-level malformed/oversize input should close/abort the session BEFORE
  awaiting the failure response write, rather than retain a live transaction
  while a nonreading peer stalls that write. Session drop remains the fallback
  for EOF/write errors/task cancellation. Add a narrow synthetic network test.
- This adds cleanup when RESET is processed. Jump-ahead cancellation of running
  synchronous work requires the future worker/runtime boundary and is not claimed.

Primary protocol sources checked 2026-09-27:
[transactions and messages](https://neo4j.com/docs/bolt/current/bolt/message/)
and [server states](https://neo4j.com/docs/bolt/current/bolt/server-state/).
Implicit transactions end on final result consumption/discard; explicit ones
end through COMMIT/ROLLBACK. A failed reset cannot leave a reusable connection.
The existing single-result restriction remains explicit for this slice.

## Task 3: English default, Portuguese option

User steering during this session explicitly sets internal English with an
option for Portuguese. Canonical graph/schema identifiers, code and technical
docs already use English. Change only the shell language default in
crates/server/src/main.rs from `default_value = "pt-BR"` to `default_value = "en"`,
and make its help clear: "Interface language: en (default) or pt-BR." Preserve
both supported language options, all graph identifiers/data, and current visual
composition. No translation rewrite or localization framework is needed.
Own crates/server/src/main.rs only unless an existing assertion needs updating.
This reversible default change needs no new implementation-mirroring test:
run existing relevant server/TUI tests and verify actual binary snapshots
without --lang (English) and with --lang pt-BR (Portuguese), plus invalid locale
still fails. Cargo offline build/clippy/fmt and owned commit, short report.
Controller owns AGENTS, spec, design, run guides and optional updated captures.
Do not create or change any image asset. No subagents; keep all work local.

## Completion

All task reviews and final integration review; cargo workspace test, clippy
all-target -D warnings, fmt and binary build. Update README/JOURNAL/preflight
with exact status and next worker/adapter/serve work. Do not mark MVP Task 5
complete merely because its prerequisite APIs are implemented.
