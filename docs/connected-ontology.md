# Inspect ontology through the running server

The Ontology TUI can now fetch a read-only schema snapshot over Bolt. The client
never opens RocksDB or loads local database configuration in this mode. It can
run while the server owns its data directory and serves Python ingestion.

Build `brainwire`, start the server normally, then use another terminal:

```sh
./target/debug/brainwire shell --connect 127.0.0.1:7687 \
  --graph tickets --view ontology
# Portuguese interface; canonical graph names remain unchanged:
./target/debug/brainwire shell --connect 127.0.0.1:7687 \
  --graph tickets --view ontology --lang pt-BR
# One frame for scripts, without a terminal:
./target/debug/brainwire shell --connect 127.0.0.1:7687 \
  --graph tickets --view ontology --snapshot --width 160 --height 40
```

The graph must already exist on the server. Use its numeric IP and port (IPv6
uses `[address]:port`), not a `bolt://` URL or DNS name. The graph is required;
unknown names fail rather than switching to the default database. `--connect`
cannot be combined with `--overview`, `--demo`, `--config` or `--estimated`.
Connected mode also supports [Memory and Indexes](connected-status.md). Other real
overview panels remain offline; this does not provide a live query editor or disk browser.
The server supports optional [service-token authentication](bolt-auth.md),
while native TLS is pending; retain a trusted/encrypted transport boundary.

## Snapshot and refresh

Up/Down selects a type, Enter/F1 toggles the compact inspector, PgUp/PgDn scrolls,
`?` opens the contextual page guide, and `q` exits. In wide terminals connections
and details appear side by side. The guide explains schema signatures versus
stored relationship records and calls out that the connected pane is scoped to
one selected type.
The header identifies the graph and server; the status shows catalog version,
sample age and scalar-index readiness. Connections are allowed type signatures,
not individual stored edges. Changing the target graph requires reopening with
another `--graph` value; server-side graph discovery is not part of this slice.

The offline `--overview --view ontology` inspector also has a complete
signature-catalog mode: press `a` to list every declared source/type/target
signature in the selected graph and use PgUp/PgDn to traverse the catalog.
This mode shows all declarations through a scrollable list; it does not attempt
to draw a dense schema as an unreadable 400-edge hairball, and it does not imply
that any instance records exist. The connected read-only workspace remains
focused on one selected type at a time.

Press `r` to fetch a new complete snapshot using a fresh connection. The previous
selection is retained by label if it still exists. Refresh is synchronous: key
handling waits for that bounded fetch. Default network deadline is 3,000 ms;
`--timeout-ms` accepts 1..10000. It covers connection and all partial network
reads/writes, including result completion. Local decoding/rendering and OS
scheduling are not a hard real-time guarantee. There is no background polling.

Failed refresh retains the old schema and its original sample age and displays
**STALE / DESATUALIZADO**, even on narrow terminals. A later successful refresh
clears the warning. Initial fetch failure exits with an error and no fabricated
snapshot. A terminal still showing an old snapshot does not imply server health.

## Cypher and Python contract

```cypher
CALL db.schema()
YIELD version, nodes, relationships, indexesReady
RETURN version, nodes, relationships, indexesReady
```

This no-argument procedure returns exactly one row, including for an empty graph:

| Field | Meaning |
| --- | --- |
| `version` | Integer revision of the effective catalog; 0 before ontology initialization |
| `nodes` | List of maps with `label`, `properties`, `indexedFields` |
| `relationships` | List of maps with `name`, `source`, `target`, `properties`; one map per allowed signature |
| `indexesReady` | Boolean global scalar-index readiness marker in the same transaction snapshot |

Properties are **names**, not property-type declarations. Node properties include
`entity_id` once and effective catalog columns; retained columns can remain after
ontology updates so existing rows still decode. Reserved `acl_` columns are
excluded. Catalog ordering and canonical identifiers are preserved. Indexed extra
properties need not be promoted to catalog columns. Timeline stream metadata and
per-index progress are not returned. Readiness does not imply any instance data
exists; the TUI says “no declared indexes” when there are none. The Python
helper exposes the same snapshot as `graph.schema()`. Relationship endpoints
are resolved through a temporary, query-budgeted lookup, so large catalogs
do not repeatedly scan all node types for each relationship signature.

Development-mode example of the same query through the Python Bolt driver
on isolated loopback (protected credentials are described below):

```python
from brainwire import connect

with connect("bolt://127.0.0.1:7687", "tickets", auth=None) as graph:
    driver = graph.driver
    with driver.session(database="tickets") as session:
        schema = session.run("""
            CALL db.schema()
            YIELD version, nodes, relationships, indexesReady
            RETURN version, nodes, relationships, indexesReady
        """).single(strict=True).data()
        for relation in schema["relationships"]:
            print(relation["source"], relation["name"], relation["target"])
```

The procedure uses the transaction catalog and index marker without scanning
instance nodes/edges. Normal snapshot isolation, query memory accounting,
cooperative deadlines and transaction rollback-on-error remain in force.

## Bounds and current limits

The procedure rejects oversized output rather than returning a partial schema:
4,096 node types, 4,096 relationship signatures, 65,536 aggregate collection
entries and 1 MiB of schema strings, counting repeated names/properties after
signature expansion. It reserves conservative allocation charges before building
output values, including YIELD copies. Existing transaction initialization loads
the catalog before this preflight; the caps are not a process-RSS limit.

The CLI accepts at most 4 MiB of cumulative response bytes, validates the complete
result shape, exactly one row and successful completion, and sends GOODBYE then
requires socket closure. Truncated, extra and delayed trailing results fail.
It also limits rendered schema strings to 1 MiB after escaping control characters;
some control-heavy catalogs can therefore fit the procedure limit but fail the
stricter display limit. The wire cap does not bound decoded heap usage exactly.

The shared renderer has a 65,535-line scroll ceiling. Exceptionally large property
lists may exceed it after wrapping; inspect those lists through Cypher/Python
instead. Pagination and an asynchronous refresh loop are follow-up work.

[Actual terminal captures and integration checks](../benchmarks/archive/design-assets/2026-10-05-connected/README.md)
include a server failure, restart with a changed schema, and concurrent Python writes.

## Protected service sessions

Set `BRAINWIRE_BOLT_AUTH_FILE` to a private `{principal,credentials}` JSON file
for a principal with read access to the selected graph. The shared native client
authenticates before acquiring schema; denied login/graph access fails the fetch.
See [protected Bolt connections](bolt-auth.md). No token argument or new panel
is added. Native clients still require a trusted loopback/encrypted tunnel boundary.
