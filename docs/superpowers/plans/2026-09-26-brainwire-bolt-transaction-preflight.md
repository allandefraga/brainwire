# Next session: durable Bolt transaction integration

Implementation handoff for MVP Task 5, prepared after the local executor and
backoffice slice on 2026-09-26. The [2026-09-27 transaction slice](2026-09-27-brainwire-transactions.md)
implements the shared-query context and Bolt lifecycle hooks, with a test-only
real-store adapter. The production worker, registry and durable server contract
remain pending. The original preflight below records the starting seams; see
the [transaction guide](../../transactions.md) for the current public APIs. Keep all changes local unless Allan explicitly authorizes a
remote operation. Use subagent-driven development with sequential implementers,
focused reviews and one final integration review.

## Historical seams (before the transaction slice)

- `crates/cypher/src/exec.rs::execute` parsed, opened one `Txn`, read its catalog,
  evaluated every clause/result and committed before returning. Preserve this
  atomic API for `brainwire cypher`; it cannot implement an explicit Bolt
  transaction by being called independently for every RUN.
- `crates/store/src/txn.rs::Txn<'db>` borrows `Database` and its RocksDB snapshot.
  Its commit consumes the transaction; dropping it discards staged writes.
  An engine that holds both an `Arc<Database>` and a transaction borrowing that
  same field would be self-referential. Resolve ownership before building the
  adapter; do not extend lifetimes with transmute or leak databases.
- `crates/bolt/src/session.rs::QueryEngine` originally exposed only synchronous
  `run`. BEGIN, COMMIT, ROLLBACK and RESET changed session state without engine
  calls. The original stub's commit counter was not a durable store bookmark.
- `crates/bolt/src/server.rs` creates a per-connection engine with
  `Send + 'static` bounds and calls `session.handle` inside the async socket
  loop. Real RocksDB work must have an explicit execution boundary rather than
  silently blocking the async runtime. Keep the network and session error paths
  involved in cleanup: malformed messages, EOF, GOODBYE and failed socket writes.

Read the current files and `research/bolt.md` before the remaining integration. The
protocol research is already linked to primary sources; verify any new protocol
claim or version-specific behavior against its source during implementation.

## Slice 1: safe multi-query execution context

Delivered on 2026-09-27 as `QueryTransaction`; requirements below are retained
as the acceptance record, not a task to repeat.

First prove the storage-lifetime ownership model in a compiling, focused test.
A scoped blocking worker that owns the selected database and creates a local
borrowed transaction is one candidate; this is not a mandate for one thread per
connection. Bound worker queues, and make dropping/cancelling the owning session
release the transaction. Do not introduce an unbounded background worker pool
or a second fake transaction store to avoid the lifetime problem.

Extract clause execution so an explicit transaction can run several queries
against the same snapshot and write overlay. The catalog must come from that
same transaction. Bindings and parameters remain query-local; later RUNs see
prior graph writes but do not inherit the preceding query's variable scope.
Keep `execute` as the standalone begin/run/commit wrapper.

Represent Active, Failed/Aborted and Finished states explicitly. A query error
must make the enclosing transaction uncommittable and discard all of its staged
writes. This MVP does not require statement savepoints or recovery by continuing
inside the failed transaction. Commit happens once, and failures cannot be
turned into SUCCESS by a subsequent state transition. Preserve conflict mapping.

Acceptance: two RUN-equivalent queries read each other's staged changes, other
transactions cannot see them before commit, commit survives reopen, explicit
rollback removes both writes, and a late error prevents the earlier write from
being committed. Keep the standalone CLI rollback tests passing unchanged.

## Slice 2: lifecycle hooks and session transitions

Delivered on 2026-09-27, including a test-only real-store adapter and transport
cleanup tests. The production runtime and value conversion are still Slice 3.

Add real begin/commit/abort hooks at the QueryEngine boundary and decide how
implicit transactions complete when a result is exhausted or discarded. Check
that lifecycle against `research/bolt.md` and the supported driver versions.
Do not reuse the local CLI's commit-before-output behavior without examining the
Bolt result lifecycle. Conversion failures and commit failures must be returned
at the correct protocol boundary.

Use an instrumented test engine to verify hook calls before connecting RocksDB:

| Trigger | Required effect |
|---|---|
| BEGIN succeeds | One transaction bound to the resolved database |
| BEGIN fails | FAILURE; pipelined RUN/PULL cannot execute |
| RUN in explicit transaction | Shared overlay; no independent commit |
| COMMIT | Durable commit before SUCCESS; error becomes FAILURE |
| ROLLBACK | Drop the overlay before SUCCESS |
| Query or protocol error in a transaction | Abort pending writes; enter failed state |
| RESET | Abort pending state and clear results before returning to Ready |
| EOF, GOODBYE, malformed request, socket failure | Drop/abort transaction ownership |
| Database switch inside active transaction | Explicitly reject incompatible selection |

Keep existing authentication-state and protocol tests intact. Synthetic bookmark
counters must not be presented as durable causal progress. Document any limited
bookmark behavior honestly; follow the MVP's supported subset and error codes.

## Slice 3: registry, adapter and durable server

Proceed with MVP Task 5's database registry and `serve` CLI only after the above
transaction seam passes. One process-wide Resources instance supplies the shared
cache/memtable budgets to all databases. Database names resolve through the
registry, never arbitrary client filesystem paths. Preserve parameter types,
node element IDs and execution error categories in PackStream conversion.

Use the actual installed driver environments; the old temporary Python path in
the original plan may no longer exist. Check availability before recreating a
scratch environment. Run the restart and managed-transaction checks with the
supported 5.x and 6.x drivers, synthetic data and a nondefault local port. Stop
all test servers afterwards.

The delivery gate is real multi-query commit/rollback/reopen, RESET and disconnect
cleanup, deterministic conflict reporting, and the existing 10,000-row restart
demo. A successful single RETURN or a functioning TUI is not evidence for those
transaction guarantees. Live TUI queries and memory/file telemetry remain later
slices with their own bounded result and observability contracts.
