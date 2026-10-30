# Lazy reads and parallel entity streaming

Status: candidate implementation in this checkout, 2026-10-30. Performance and
Linux memory qualification remain release gates; see `PRODUCTION-v0.12.md`.

## Execution

A scoped producer owns the transaction and its immutable snapshot while a result
is open. Eligible plans have one read-only part and no blocking projection,
aggregation, DISTINCT, SKIP/LIMIT, UNION or catalog procedure.

Large eligible label scans use the existing scan partitions and up to eight
workers. Each worker has a fixed share of the query budget and a bounded output
queue. The coordinator drains partitions in scan order. Nodes, relationships and
paths are materialized under the same snapshot and ACL visibility as matching.
Small or otherwise ineligible entity plans retain the handle-streaming path;
blocking plans retain their existing execution path.

For the parallel path, fields are reported before scanning. Scalar sequential
plans currently report fields on their first row, or when an empty scan ends.
Once fields arrive, Bolt RUN returns them with no records and an open result.
It does not wait for an initial page. PULL receives rows and detects the end of
the result, including an empty result. An execution error after fields is
reported by PULL, which rolls back the transaction.

## Bounded transport and accounting

Worker output carries owned query-pool credit through its queue, materialization,
Bolt session and response batch. The credit is released when its owner is
written or dropped. Page limits account for both row count and estimated bytes;
worker shares include output already handed to the transport. The socket writes
an engine page before requesting another, even for `PULL -1`, and sends exactly
one terminal summary per client request.

Finite `DISCARD n` uses the engine's actual consumed-row count, rather than an
unknown remaining-cardinality sentinel. `DISCARD -1` cancels the remaining lazy
producer without enumerating its tail. RESET and disconnect cleanup close queues,
cancel workers and release their leases. Consumer waiting time extends the
execution deadline; the transaction deadline still uses wall time.

## Cancellation limits

After RUN has returned fields, RESET can stop a parallel scan that has produced
no rows: cooperative checks inside the scan observe cancellation. The coordinator
closes all receivers before joining workers, so a blocked queue sender can exit.

This does not make a currently executing PULL preemptible. The Bolt connection
processes requests serially, and `LazyRows::fill` waits for a page or completion.
A queued RESET is processed after that work returns. Before fields, sequential
empty scans and planning may also delay RUN. Native storage calls and cooperative
deadlines can overshoot; there is no claim of immediate interruption.

## Remaining limits

- Eligibility is bounded; streaming is not universal and a scan without a label
  or with an indexed anchor may use another path.
- Fixed worker shares can refuse skewed results although other shares are idle.
- ORDER BY, aggregation and DISTINCT remain eager and budgeted. ORDER BY with
  LIMIT keeps a bounded top set; no operator spills to disk.
- Accounting is conservative and does not establish a hard process RSS ceiling.
- Mixed-load performance and sustained 2/4/8 GiB Linux qualification are still
  required before release claims can be made.
