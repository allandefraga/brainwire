# Inspect a running graph in the connected workspace

`brainwire shell --connect IP:PORT --graph NAME` opens one read-only workspace
with four panes: **Ontology, Memory, Indexes and Graph**. Ontology is the initial
pane. Press Tab/Shift+Tab or `1`/`2`/`3`/`4` to switch panes, `r` to refresh the
active pane and `q` to exit. Each pane is fetched only when first visited and
keeps its own selection, sample age and stale status. All data comes through
authenticated Bolt; the shell never opens the running server's RocksDB files.

From the repository root, with a built binary and an existing running server:

```sh
export BRAINWIRE_BOLT_AUTH_FILE=/private/path/observer.json
./target/release/brainwire shell --connect 127.0.0.1:7687 --graph tickets \
  --node-label Ticket --entity-id '["tenant-0","zendesk-main","ticket","0"]' \
  --edge-type HAS_COMMENT --target-label TicketComment
./target/release/brainwire shell --connect 127.0.0.1:7687 --graph tickets --lang pt-BR \
  --node-label Ticket --entity-id '["tenant-0","zendesk-main","ticket","0"]' \
  --edge-type HAS_COMMENT --target-label TicketComment
```

The entity ID above is the first ticket produced by the synthetic client
fixture and adapter. Quote it as shown; it is an identity value, not a Cypher
expression. The four Graph flags configure one fixed neighborhood for this
terminal session. Without them, the other three panes work normally; visiting
Graph shows a configuration error and leaves navigation available.

The explicit standalone views remain available:

```sh
./target/release/brainwire shell --connect 127.0.0.1:7687 --graph tickets --view memory
./target/release/brainwire shell --connect 127.0.0.1:7687 --graph tickets --view indexes --lang pt-BR
./target/release/brainwire shell --connect 127.0.0.1:7687 --graph tickets \
  --snapshot --width 120 --height 30
```

A workspace snapshot captures its initial Ontology pane. Use `--view graph`
with the four Graph flags for a plain neighborhood frame, or `--view memory`
and `--view indexes` for those standalone frames. `--view workspace` explicitly
selects the same workspace as the connected default.

The address is a numeric IP and port, including bracketed IPv6, rather than a
Bolt URL or DNS name. Credentials use the private JSON file described in
[protected Bolt connections](bolt-auth.md). The native TUI still uses plaintext
Bolt: run it on trusted loopback or the trusted database side of an encrypted
tunnel. The [optional TLS edge](protected-transport.md) is qualified for Python
clients, not a native TUI TLS connection. Internal graph/schema identifiers
remain English; interface labels and explanations support English and `pt-BR`.
There is no connected query editor, graph selector, property editor or import-job
controller yet. Other real panels still require offline access.

## Memory values and permissions

Memory uses exactly one row from:

```cypher
CALL db.memory()
YIELD poolLimitBytes, poolReservedBytes, poolPeakBytes, poolRejections
RETURN poolLimitBytes, poolReservedBytes, poolPeakBytes, poolRejections
```

For a fresh local policy, `python3 -m examples.python.create_service_tokens
/private/path/auth --graph tickets --include-observer` adds an explicit read-only
`observer.json` credential with the required instance-telemetry grant. Select that
file with `BRAINWIRE_BOLT_AUTH_FILE`; ordinary reader/writer/health credentials do
not acquire this grant. The generator never prints tokens or overwrites a directory.

| Counter | Meaning |
| --- | --- |
| LIMIT | Configured `memory.query` reservation budget shared by every graph on this server |
| RESERVED | Reservations accounted now, including the query collecting this sample |
| PEAK | Highest accounted reservation since this server process started |
| REJECTIONS | Rejected reservation attempts since this process started |

These are query reservation counters, **not process RSS, total host RAM,
RocksDB cache use or storage memtables**. The server and CLI do not report
unknown values as zero. A server without an attached shared pool returns all
null values, displayed as unknown. Restart resets the process-lifetime peak and
rejection counters. A graph name selects the authorized query context; it does
not make these counters graph-specific.

The principal needs read access to the graph **and `instance_admin: true`**.
Ordinary graph readers can inspect ontology and indexes without gaining
process-wide telemetry. Failure to fetch memory does not affect those views.

## Index declarations and readiness

Indexes uses the existing graph-scoped procedure:

```cypher
CALL db.indexes() YIELD label, property, state RETURN label, property, state
```

Up/Down or PgUp/PgDn selects a declaration. The visible range and total count
identify rows outside the viewport. `scalar_equality_ready` displays READY;
`declared_only` displays DECLARED. The pane explains the selected state.
Readiness is the transaction catalog's scalar-index marker, not a per-field
build-progress percentage or a promise of vector/range/list indexes. Ready
scalar equality access supports strings, Booleans and finite numbers. Other
value kinds do not acquire indexed access merely because their field is listed.
Legacy declarations without a ready marker use safe label scans until ontology
reapplication/backfill. Inspecting a declaration does not build or change it.

## Typed outgoing Graph sample

Graph first checks the requested relationship signature against the real server
ontology. It then queries source existence and matching outgoing targets in one
read transaction. `--node-label`, `--edge-type` and `--target-label` accept
1..128-byte ASCII identifiers starting with a letter or underscore; `--entity-id`
accepts a nonempty value up to 1,024 bytes and is passed as a query parameter.
The pane reads identity strings only, never arbitrary properties or mutations.

The target query uses `LIMIT 21`: the pane keeps at most **20 relationship rows**
and sets **MORE AVAILABLE / MAIS DISPONÍVEIS** only when it actually observes the
extra row. It does not calculate full degree or traverse other types/directions.
Parallel relationships can repeat the same target ID, so the displayed count is
not a count of distinct neighbors. A missing source is marked not found without
inventing a node; a present source with no matching rows does not establish that
it has no other graph connections.

Up/Down and PgUp/PgDn select and scroll the sampled target rows. The wide layout
has a radial preview and target list; at 80 columns it shows the selected
relationship and a scrollable list. Selection does not expand or change the
configured source. Every displayed connection comes from the completed server
sample. Graph requires ordinary graph read access, not instance-administrator
permission. Target IDs are checked as nonempty strings up to 1,024 bytes;
malformed fields, excess rows and incomplete responses fail rather than becoming
a partial sample.

## Refresh, bounds and failures

All panes show the source server, graph and sample age. A failed refresh retains
the previous data and original age with **STALE / DESATUALIZADO**. Successful
refresh clears that status and retains selection by identity when possible.
Interactive workspace initial failures show an error pane without fabricated
data, so a reader denied Memory can still open Ontology, Indexes or Graph.
An initial workspace snapshot failure returns a nonzero exit status. Standalone
views retain their initial-failure exit behavior.

Refresh is synchronous: input waits until the fetch returns. The default total
network deadline is 3,000 ms; `--timeout-ms` accepts 1..10000 and includes
connection, authentication, partial reads/writes and result completion. Local
rendering and OS scheduling are not hard real-time guarantees. There is no
background polling or automatic graph discovery.

Memory accepts four nonnegative integer counters, or four nulls, one record
and successful completion within **64 KiB aggregate response bytes**. Indexes
accept at most **200 unique label/property rows and 128 KiB aggregate response
bytes**. Larger, malformed, duplicate, incomplete or trailing responses fail;
the shell never presents a partial result as complete. Graph's existence and
neighborhood transaction has a **128 KiB aggregate response limit**, in addition
to the separate bounded schema precheck; both share the total fetch deadline.
Display strings are escaped and bounded. Wire limits are not exact heap/RSS limits, nor do they
turn the server's materialized query results into a streaming executor.

Plain snapshots accept 80..240 columns and 24..80 rows. Interactive views also
remain safe in smaller terminals, although explanations may not fit. `NO_COLOR`
removes color. The chosen strong-green/cyan Observatory identity is retained.

The constrained pilot profile uses an 8 MiB encoded request cap, a 16 MiB decoded
request allowance and a 300,000 ms authenticated IO budget. These are server
settings, independent of the TUI's at-most-10-second fetch deadline and response
caps. Configure Python pool reuse with `liveness_check_timeout=30`; see
[connection limits](connection-limits.md), [protected transport](protected-transport.md)
and the [MVP quickstart](mvp-quickstart.md). None of these limits defines total RSS.

For the complete schema contract and separate bounds, see
[connected ontology](connected-ontology.md). The [October 13 real terminal captures](design/assets/2026-10-13-beta/README.md)
show the four-pane workspace backed by 100,000 synthetic nodes, including
permission denial, stale recovery and a bounded high-degree Graph sample.
