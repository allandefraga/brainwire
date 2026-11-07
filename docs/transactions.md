# Transaction lifecycle

Brainwire's embedded Cypher executor supports a transaction shared by several
queries. The Bolt session routes those lifecycle hooks through a scoped query
worker in `brainwire serve`. The existing `brainwire cypher` command still
executes one atomic query per invocation.

## Embedded queries

`QueryTransaction::begin(&database)` takes one storage snapshot and loads its
catalog from that snapshot. Each `run` uses fresh variables and parameters but
can read the earlier runs' staged graph changes. Other transactions do not see
those writes before commit.

```rust
use brainwire_cypher::QueryTransaction;
use std::collections::BTreeMap;

// `database` already has the synthetic Customer ontology applied.
let params = BTreeMap::new();
let mut transaction = QueryTransaction::begin(&database)?;
transaction.run(
    "CREATE (c:Customer {entity_id: 'tx-demo', name: 'Ana'})",
    &params,
)?;
transaction.run(
    "MATCH (c:Customer {entity_id: 'tx-demo'}) SET c.name = 'Ana Silva'",
    &params,
)?;
transaction.commit()?;
```

`rollback` or dropping the context discards pending graph changes. Any parse,
validation or execution error aborts the whole transaction, including writes
from previous successful runs. A failed or finished context cannot run another
query or commit again; begin a new context instead. A write conflict fails the
commit, without publishing that transaction's writes.

Commit returns an optional storage sequence: a write commit returns `Some`,
while a read-only completion returns `None`. The sequence is now durable within
one store and continues after reopening; it is not a causal bookmark or a global
replication position. See [durable commit state](commit-state.md). Allocated ID gaps can remain after abort;
ID allocation intentionally reserves durable blocks independently of graph writes.

The public context borrows its database. An owning worker can hold the database
and create the context inside its own scope; the context must end before the
database is dropped. Do not create a self-referential owner/borrower struct or
extend the snapshot lifetime artificially. The production Bolt worker follows
this ownership path; Bolt request handling runs outside Tokio's IO workers.

## Bolt session boundary

An engine implements begin, run, commit and idempotent abort operations.
The session routes explicit transactions to these hooks and owns pending cleanup.
An implicit transaction completes after its result is exhausted or discarded;
an explicit transaction waits for COMMIT. Partial results leave it open. These
boundaries are part of the Bolt session contract.

Query failures, rollback, processed RESET and session teardown release pending
transaction ownership. Cleanup failure must not acknowledge success or leave a
reusable connection. Disconnect/fatal network paths need cleanup even if the
peer cannot receive the failure response. The session returns to a reusable state only after successful cleanup.

Bookmarks are optional engine-provided output; the session no longer invents
commit counters. The development stub has no durable graph state and does not
promise causal progress. A test-only adapter verifies session lifecycle with a
real temporary store. The production adapter in `brainwire serve` adds value
conversion and named-database selection.

## Remaining work

- Define a restart-stable causal bookmark, if the replication model needs it.
- Expand driver concurrency and fault tests beyond the current 10K-record
  write/SIGKILL/restart/read check with Python drivers 5.28.6 and 6.3.0.
- Jump-ahead cancellation while synchronous query work is running; processed
  RESET cleanup alone does not implement that guarantee.
- Multiple concurrent result streams, full protocol metadata, allocator-level
  memory enforcement, bounded Bolt cursors, authorization and the connected TUI.
  Per-RUN guards, a shared reservation pool and cooperative execution deadlines
  are implemented; see [query limits](query-allocation-guards.md).

The [recovery guide](termination-recovery.md) describes crash and restart
behavior. The [server guide](serve.md) describes
the current local runtime and its limits.
