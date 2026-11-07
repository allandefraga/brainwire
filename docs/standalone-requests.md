# Experimental standalone request outcomes

Use this only for controlled trials on a **new, dedicated volume**. The mode is
opt-in and keeps Brainwire's normal storage format and request handling unchanged
when disabled. It provides an outcome record for an authenticated explicit write
transaction. It does not rerun callbacks or guarantee application-level replay.

## Start a protected experimental server

Provide a private authentication policy as described in [Bolt authentication](bolt-auth.md).
Use a data directory that contains no existing graph. For example:

```sh
BRAINWIRE_SERVER_REQUEST_LEDGER=true \
BRAINWIRE_SERVER_AUTH_FILE=/run/secrets/brainwire-policy.json \
BRAINWIRE_SERVER_DATA_DIR=/data/experimental-requests \
brainwire serve
```

The equivalent TOML key is `server.request_ledger = true`. Authentication is
required; quorum serving and `server.checkpoint_dir` are refused in this mode.
Only newly created graphs get the format. Existing ordinary graphs cannot be
activated or migrated by setting this flag. Reopening an experimental graph
requires the flag again. Disabling it fails clearly instead of opening the
volume as an ordinary graph.

The experimental format persists an additional RocksDB column family. Ordinary offline commands without the opt-in refuse it. With an explicit
opt-in configuration, `brainwire db check` can inspect invariants; offline
checkpoint/backup/restore and embedded-query commands still refuse it. Supported old-binary downgrade, upgrade from an existing graph, restore and
fork semantics remain qualification gates. Do not use this mode for an existing
production volume or remove its column family to make an older tool open it.

## Write with an immutable request ID

Initialize labels/catalog using a separate ordinary authenticated administrator
transaction; for example `CALL db.createLabel('Item')`. An identified transaction
cannot call procedures or change the catalog.

Using the Python Bolt driver, create an **explicit transaction** and provide
its request ID before the first query:

```python
import os
from brainwire import connect

with connect(
    os.environ["BRAINWIRE_URI"], "default",
    auth=(os.environ["BRAINWIRE_PRINCIPAL"], os.environ["BRAINWIRE_TOKEN"]),
    max_transaction_retry_time=0,
) as graph:
    driver = graph.driver
    with driver.session(database="default") as session:
        tx = session.begin_transaction(metadata={"brainwire_request_id": "source-event-42"})
        tx.run("CREATE (:Item {entity_id: $id})", id="item-42").consume()
        tx.commit()
```

Only a principal with graph write access and no static/delegated node visibility
may use identified writes. Read-only and implicit transactions with a request ID
are refused. Every `CALL` procedure, including calls in UNION branches, is
refused before execution; earlier pending writes are rolled back on that error.
Ordinary transactional Cypher stages graph/index mutations and the request
outcome in the same synchronous RocksDB WriteBatch.

The ID belongs to the authenticated principal and the graph's stored incarnation.
A different principal or graph has a separate namespace. Token rotation for the
same principal preserves it. Reusing an old principal ID deliberately inherits
its former namespace: use a new unique principal ID for a new identity.

## Read the outcome after a lost acknowledgement

Use a **fresh autocommit call**, without a managed or explicit transaction:

```python
with driver.session(database="default") as session:
    rows = session.run(
        "CALL db.requestStatus($seed)", seed="source-event-42"
    ).data()
```

The call accepts one string seed argument and returns one row:

| Field | Meaning |
| --- | --- |
| `status` | `committed` or `not_recorded` in this current store history |
| `requestId` | The requested seed |
| `incarnation` | The stored graph identity, not proof of restore/fork lineage |
| `sequence` | Decimal storage commit sequence; null for `not_recorded` |

The initial endpoint supports an isolated `CALL` only: no `YIELD`, `RETURN`,
`WHERE`, UNION or additional clauses. It is a protected server endpoint, not an
embedded Cypher procedure. The server supplies the principal scope; a caller
cannot ask for another principal's request. Graph grants are checked first and
all static/delegated visibility principals are refused. A graph reader may
inspect its own requests left from an earlier writer grant.

`not_recorded` is **not proof of rollback or permission to retry**. Another
connection may still commit the request. A restored or copied history can omit
later outcomes or share an incarnation with its source. Keep an authoritative
application-side source cursor and reconcile owned data rather than blindly
replaying an event.

## Errors, quotas and recovery

- `RequestAlreadyCommitted`: BEGIN found an earlier committed seed, or another
  concurrent transaction committed it first. It is an error, never fabricated
  success or a replay of earlier rows.
- `RequestPayloadMismatch`: the seed was reused with different staged bytes;
  the new transaction was rolled back. The fingerprint is deliberately physical:
  allocated internal IDs can differ even when application intent is similar.
- `RequestLedgerFull`: the new transaction was rolled back. The initial quota
  is 100,000 entries and 16 MiB of canonical request-entry bytes per graph. It
  does not bound physical disk or process memory. There is no TTL, eviction,
  online quota change or supported maintenance command yet.
- `CommitOutcomeUnknown`: a storage write reported an uncertain result, or the
  client lost its acknowledgement. Never interpret this as rollback. After an
  uncertain storage write, the opt-in store blocks commits, request lookups,
  replication preparation and checkpoints until exclusive close/reopen. Raw
  reads and consumption of preallocated IDs may continue; they cannot make a
  durable commit while the store is quarantined.

No automatic logical retry is promised. The [storage design](design/standalone-commit-outcomes.md)
records the atomicity, format and crash tests and the remaining qualification
requirements, including actual old-binary downgrade, ENOSPC, power loss and
restore/fork lineage. This is synthetic/local validation, not qualification of
an application data source.
