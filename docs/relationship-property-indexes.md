# Index relationship properties

Use an experimental relationship property index to find edges by a value such
as an event timestamp. Indexes are available on opted-in standalone stores.

For a relationship type with immutable merge keys, an index on one of its key
properties can also narrow the search used by `MERGE`. Wait until the index is
ready first. If no eligible index is ready, `MERGE` scans the endpoint pair.
The indexed path still checks the complete key and the current catalog; it is
not a constant-time guarantee.

## Enable on a standalone server

Take a backup before enabling this option and verify that you can reopen it.
For an evaluation, start with a separate data directory or a disposable copy.

```sh
BRAINWIRE_SERVER_RELATIONSHIP_INDEXES=true brainwire serve
```

The equivalent setting in `brainwire.toml` is:

```toml
[server]
relationship_indexes = true
```

The setting defaults to false. On startup the server creates the
`relationship_index_v1` RocksDB column family in each graph it opens, including
existing graphs. It also creates it in graphs opened later. The format change
persists after you turn the setting off or drop the last index. Older binaries
that lack this column family refuse to open the upgraded graph. To return to
an older binary, use a backup taken before activation. A checkpoint taken
after activation includes the new format. Quorum replicas refuse it.

## Create, inspect and drop an index

Use an existing relationship type, such as `EVENT`. Run each DDL statement as
a separate query with graph write and instance administrator access.

```cypher
CREATE INDEX event_at IF NOT EXISTS FOR ()-[r:EVENT]-() ON (r.at)
```

The supported declaration has one relationship type and one property. The
name is optional and defaults to `index_rel_EVENT_at`. Names are shared with
node indexes. `IF NOT EXISTS` makes a duplicate name or an already indexed
property on the same type a no-op.

```cypher
CALL db.indexes() YIELD name, label, property, state, entityType
WHERE entityType = 'relationship'
RETURN name, label, property, state
```

Here `label` contains the relationship type. Existing named `YIELD` columns
remain available. Clients that need a node-only inventory can filter on
`entityType = 'node'` on a server that provides the column.

| State | What it means |
| --- | --- |
| `populating` | A background job is indexing existing edges in bounded batches. Writes maintain the index as they commit and queries use the ordinary scan. The job resumes after restart. |
| `scalar_range_ready` | The build is complete and supported query shapes can use the index. |
| `failed_value_too_large` | An existing property's estimated decode allocation exceeded the 4 MiB build bound. Queries keep using the scan. Drop the index and recreate it after removing or reducing oversized values. |

```cypher
DROP INDEX event_at IF EXISTS
```

Drop removes the declaration on commit and cleans up index entries in
background batches. Wait for cleanup before recreating an index on the same
property.

## Query a timeline

Pass consistently formatted, sortable timestamps or use numeric timestamps.
For example, use UTC strings in one fixed format and precision for both the
data and the parameters:

```cypher
MATCH ()-[r:EVENT]->()
WHERE r.at >= $from AND r.at < $until
RETURN r.at ORDER BY r.at
```

```cypher
MATCH ()-[r:EVENT]->() WHERE r.at = $at
RETURN count(r)
```

A ready index can provide candidates for string, numeric and Boolean
equality, or string and numeric ranges with literal or parameter bounds.
Brainwire evaluates the full predicate and checks visibility of both
endpoints before returning a match.

The index preserves each stored relationship, even when several share the
same endpoints and indexed value. To store multiple events between the same
pair, declare a [keyed relationship type](temporal-data.md#keyed-relationships)
and use `MERGE` with a distinct event key. For unkeyed types, `CREATE` and
`MERGE` use one directed edge per endpoint pair and type.

The indexed path needs an immediate, fixed-length expansion over one type
from an unbound node scan. Bound endpoints, partitioned anchor scans,
variable-length patterns, multiple types, complex values and unsupported
predicate shapes use the ordinary plan. For example, a typed anchor on a
large label can be partitioned and use a scan even if the index is ready.
Use `ORDER BY` when the result must have a stable sort order.

The [connected TUI](connected-status.md) Indexes pane shows declarations and
states for nodes and relationships:

```sh
brainwire shell --connect 127.0.0.1:7687 --graph default --view indexes
```

The offline admin schema and overview readiness summaries cover node
indexes. To audit the physical index entries, stop the server before running
`brainwire db check NAME` against its data directory. For implementation
details, see the [relationship index design](design/relationship-property-index.md).
