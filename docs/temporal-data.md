# Temporal data: current behavior and a safe model

## What the engine versions today

Brainwire versions an ontology/catalog when its schema changes. Stored rows also
carry the schema version needed to decode their properties. Transactions read a
snapshot and commit atomically; `db.commitState()` reports a durable local
position for the current transaction. Checkpoints can preserve a point-in-time
copy for recovery.

These mechanisms do **not** keep previous values of a node or relationship.
A later `SET`, property removal, relationship deletion, or node deletion does not
leave a queryable historical revision. There is no `AS OF` query, historical
traversal, automatic validity interval, temporal index, or retention worker.
A checkpoint is a recovery copy, not a temporal query surface. A catalog version
is a schema version, not a record revision.

## Model history in the application today

If an application needs a ticket timeline before native temporal support exists,
it can create revision nodes and update a current projection in one explicit
transaction. The application must enforce immutability and ordering; Brainwire
does not do that for this model. In particular, `MERGE` followed by unconditional
`SET` is a mutable upsert: replaying a revision ID with a different payload
overwrites the earlier value.

Use stable, source-owned revision IDs. In each transaction, the application
should:

1. Look up the revision ID. If it exists, compare every immutable field with the
   incoming payload; treat an exact match as an idempotent replay and reject a
   mismatch. If it does not exist, create the revision and its `HAS_REVISION`
   edge.
2. Update the ticket projection only if the incoming source sequence is newer
   than the sequence currently stored on the ticket. Ignore older events for
   the projection while keeping their revisions if they are valid source
   history.
3. Commit the revision and any projection change together. Retry the whole
   transaction after a conflict, then repeat the revision-ID comparison.

The projection update can use a sequence predicate inside the same explicit
transaction:

```cypher
MATCH (t:Ticket {entity_id: $ticket_id})
WHERE $source_sequence > coalesce(t.source_sequence, -1)
SET t.source_sequence = $source_sequence,
    t.status = $status,
    t.priority = $priority
RETURN t.source_sequence AS applied_sequence
```

If the query returns no row, the application must not assume the projection was
updated; the event may be older or the ticket may be missing. The revision
creation step must validate or reject an existing ID before it writes. Use
`CREATE` only after that lookup and keep both steps in the same transaction.

The `TicketRevision` label and `HAS_REVISION` edge must be declared in the
ontology. `recorded_at` is an application-provided string here, not a native
DateTime value. Normalize timestamps to one fixed-width UTC format if sorting
strings lexically. A monotonically increasing source sequence is safer for
ordering corrections and equal timestamps. Validate source IDs, ordering and
retention in the application before writing.

To inspect recent revisions, query the revision records explicitly:

```cypher
MATCH (t:Ticket {entity_id: $ticket_id})-[:HAS_REVISION]->(r:TicketRevision)
RETURN r.source_sequence AS sequence, r.recorded_at AS recorded_at,
       r.status AS status, r.priority AS priority
ORDER BY sequence DESC
LIMIT 50
```

This is application-managed event modeling, not a native historical snapshot.
A deleted source ticket should be represented by a tombstone revision if the
history must remain. Do not remove old revisions until the owning application
has approved the retention cutoff and verified that the records are no longer
needed. Brainwire does not currently schedule or perform that cleanup for you.

## Native temporal work still required

A first-class implementation needs one durable revision contract for **both**
nodes and edges, plus recorded time and valid time, atomic current-state updates,
late-event/correction rules, `AS OF` reads and traversals, indexes for temporal
selection, schema migration, bounded retention, and crash/replay tests. Until
those pieces ship together, keep authoritative history in the source system or
model immutable events explicitly in the application.
