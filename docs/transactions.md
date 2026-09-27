# Transaction lifecycle

Brainwire's embedded Cypher executor supports a transaction shared by several
queries. The Bolt session owns matching lifecycle hooks; the production worker,
database registry and durable `serve` adapter remain separate work. The existing
`brainwire cypher` command still executes one atomic query per invocation.

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
while a read-only completion returns `None`. This sequence is process-local,
not a restart-stable causal bookmark. Allocated ID gaps can remain after abort;
ID allocation intentionally reserves durable blocks independently of graph writes.

The public context borrows its database. An owning worker can hold the database
and create the context inside its own scope; the context must end before the
database is dropped. Do not create a self-referential owner/borrower struct or
extend the snapshot lifetime artificially. The test worker proves this scoped
ownership path, not the eventual async scheduling design.

## Bolt session boundary

An engine implements begin, run, commit and idempotent abort operations.
The session routes explicit transactions to these hooks and owns pending cleanup.
An implicit transaction completes after its result is exhausted or discarded;
an explicit transaction waits for COMMIT. Partial results leave it open. These
boundaries follow the [Bolt transaction message specification](https://neo4j.com/docs/bolt/current/bolt/message/).

Query failures, rollback, processed RESET and session teardown release pending
transaction ownership. Cleanup failure must not acknowledge success or leave a
reusable connection. Disconnect/fatal network paths need cleanup even if the
peer cannot receive the failure response. The [server state specification](https://neo4j.com/docs/bolt/current/bolt/server-state/)
describes the corresponding terminal and recovery states.

Bookmarks are optional engine-provided output; the session no longer invents
commit counters. The development stub has no durable graph state and does not
promise causal progress. A test-only adapter verifies session lifecycle with a
real temporary store; it is not the production protocol/registry adapter.

## Remaining integration

- Production ownership and bounded worker/runtime scheduling for RocksDB calls.
- Database registry, PackStream value/node conversion, error mapping and `serve`.
- Durable bookmark handling and driver restart/managed-transaction checks.
- Jump-ahead cancellation while synchronous query work is running; processed
  RESET cleanup alone does not implement that guarantee.
- Multiple concurrent result streams, full protocol metadata, query memory
  enforcement, streaming, authorization and the connected TUI.

The [implementation slice](superpowers/plans/2026-09-27-brainwire-transactions.md)
and [next-session preflight](superpowers/plans/2026-09-26-brainwire-bolt-transaction-preflight.md)
record the delivery boundary. The full engine MVP Task 5 is not complete until
the real server/driver integration passes.
