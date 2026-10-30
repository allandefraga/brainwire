# Result streaming over Bolt PULL

Status: candidate implementation in this checkout, 2026-10-30. See
[lazy execution](lazy-executor.md) for the producer and cancellation limits.

## Result paths

Eligible scalar reads and large parallel label scans stream through a scoped
producer. RUN returns fields with no records once the producer reports its
columns. For parallel scans this happens before scanning. PULL obtains bounded
pages until the producer ends. Errors after fields arrive through PULL.

Other read plans that return graph entities may first compute row handles
(`NodeId`, relationship or path) and defer their property materialization. RUN
returns the fields and a deferred-row count. Each PULL materializes only its
page against the original transaction snapshot and read visibility. This still
uses memory proportional to the handle set.

Writes, catalog procedures and blocking operators keep their existing paths;
some results remain materialized before RUN returns. Streaming does not imply
that ORDER BY, aggregation or DISTINCT can process arbitrarily large inputs.

## Wire behavior

- `PULL n` returns at most the requested records and one summary. `has_more` is
  true while the session or producer still holds an open result.
- `PULL -1` writes engine pages incrementally. A slow socket retains the current
  page's credit until writing finishes; it does not assemble the full result.
- Finite `DISCARD n` consumes exactly that many rows, or stops at the end, without
  sending records. Lazy engines report actual consumption because their remaining
  cardinality is unknown. Handle streams drop handles without materializing them;
  lazy finite discard may still execute and materialize producer rows.
- `DISCARD -1` cancels a lazy tail. Buffered rows do not turn it into a finite
  enumeration request.
- Autocommit completes only after the result is exhausted or fully discarded.
  Partial PULL or DISCARD keeps the transaction open. RESET rolls it back.

## Snapshot, ACL and memory

The transaction snapshot and visibility remain fixed for the result lifetime.
Materialization uses the same visibility-aware record lookup as ordinary reads
and removes reserved ACL properties. Parallel workers use the same snapshot and
policy; their output is merged in scan order.

Handles, scratch state and output use the existing query budget. Each lazy row
holds an owned reservation through queues and response batches. Handle-based
results keep their statement reservation until the result ends. The transport
flushes a page before requesting the next. These are allocation controls, not a
hard process RSS ceiling.

## Failure and timeout limits

A failed PULL rolls back the transaction. RESET, disconnect and access changes
clean up the result and its reservations when processed. PULL finishes already
admitted work even if new RUN admission is under cgroup pressure.

Cancellation and deadlines are cooperative. RUN no longer waits for a first page
once fields are available, allowing a subsequent RESET to cancel a parallel scan
with no output. A PULL already waiting for a page cannot yet be interrupted by a
later request on the same sequential Bolt connection. Planning, pre-fields
sequential scans and synchronous storage calls can also delay interruption.

## Qualification

Unit, engine and socket tests exercise page ownership, ordering, ACL filtering,
PULL counts, finite/full DISCARD and RESET cleanup. Sustained Linux memory and
controlled mixed-load measurements remain separate release gates; synthetic
local checks do not establish production capacity or a universal performance gain.
