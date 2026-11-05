# Online catalog changes

A standalone server accepts catalog changes through Cypher while it keeps
serving queries. `brainwire db apply-ontology NAME FILE` still works offline
and remains the way to load a whole ontology file.

```cypher
CREATE INDEX [name] [IF NOT EXISTS] FOR (n:Label) ON (n.property)
CREATE INDEX ON :Label(property)
DROP INDEX name [IF EXISTS]
DROP INDEX ON :Label(property)
CALL db.createLabel('Label')
CALL db.createRelationshipType('TYPE', 'FromLabel', 'ToLabel')
CALL db.createRelationshipType('EVENT', 'Person', 'Ticket', ['event_id'])
CALL db.dropProperty('Label', 'property')
```

Each statement must be the only clause of its query and needs graph write and
instance administrator access. Composite indexes and index `OPTIONS` are not
supported. A later query in the same explicit transaction sees the change.

## How a change commits

A change rewrites the catalog in one transaction. Every Cypher transaction
reads the catalog for update when it begins, so writers that were already
running when the change commits fail with a transient conflict and are retried
against the new catalog. That is cheap for rare changes; do not issue DDL in a
tight loop.

`db.createLabel` and `db.createRelationshipType` leave existing entries alone
and add a new signature to an existing relationship type. New labels and types
are usable by the next transaction. The optional fourth argument declares the
immutable merge-key fields for a **new** relationship type. Keys become declared
properties; they must be distinct valid property names. Omitting the argument
preserves an existing type's identity. An explicit different key, or adding a
key to an existing unkeyed type, is rejected. Creating the type and its keys is
atomic with the catalog transaction; it does not create historical revisions.
See [keyed relationships](temporal-data.md#keyed-relationships).

## Index states

`CALL db.indexes() YIELD label, property, state, name` reports one row per
index. `state` is:

- `populating`: declared online and still being backfilled. Every write that
  touches the label maintains its entries from the moment it was declared, but
  queries do not use it and scan the label instead.
- `scalar_equality_ready`: built; queries use it.
- `declared_only`: the store has no global readiness marker (a legacy store);
  reapply the ontology offline to rebuild it. See [indexes](indexes.md).

The default index name is `index_<Label>_<property>`; ontology declarations use
the same default. `IF NOT EXISTS` turns an existing index with the same name or
on the same property into a no-op, and `IF EXISTS` does the same for a missing
index on drop. Without them both are errors.

The backfill runs in a background thread per database. It handles 1,024 nodes
per transaction and records its position in a job key outside the catalog, so
it never makes writers conflict while it runs and it resumes after a restart.
A batch whose nodes a writer changes first conflicts and is retried. Only the
last batch rewrites the catalog to mark the index ready, which makes running
writers retry once more. Offline commands (`brainwire cypher`, `db check`) do
not run the backfill; an index created there stays `populating` until a
server opens the database. Embedded users call `Database::build_pending_indexes`
or `Database::index_step`.

`DROP INDEX` removes the declaration at once, so queries stop using it, and
deletes its entries in background batches. Recreating an index on the same
property is refused until those entries are gone, usually within seconds.

`db check` validates index entries only for ready indexes.

## Interaction with the ontology file

Indexes created with `CREATE INDEX` survive a later `db apply-ontology` even
though the file does not list them, and an index that is still populating
keeps populating. An ontology index dropped online comes back the next time
the same file is applied. On a legacy store the offline path rebuilds every
declaration at once, which also completes any online build.

## Dropping a property

`db.dropProperty` undeclares a node property. It is refused while an index
covers the property; drop the index first. Stored values stay under the
retired column id, which no reader maps to a name, so they are not returned by
`n.property`, `keys(n)`, `properties(n)` or `db.schema()`. They are purged when
their node is next written. The id is never reused, so re-declaring the
property later (online or through the ontology file) starts empty. Writing the
same name after the drop stores an undeclared property, readable as usual.
Relationship properties cannot be dropped.

## Limits

- Cluster (replicated) transactions refuse catalog changes with a `Forbidden`
  error. The background backfill writes locally, which replicas would not
  repeat; change the catalog on a standalone server or offline.
- There is no `SHOW INDEXES`; use `CALL db.indexes()`.
- Labels and relationship types cannot be dropped online.
