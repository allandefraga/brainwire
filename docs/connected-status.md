# Inspect memory and indexes through a running server

The native shell now offers three read-only connected views: `ontology`,
`memory` and `indexes`. They use authenticated Bolt procedures and never open
the server's RocksDB files. Other real panels still require offline access;
there is no connected query editor, graph selector or import-job controller yet.

From the repository root, with a built binary and an existing running server:

```sh
export BRAINWIRE_BOLT_AUTH_FILE=/private/path/client.json
./target/release/brainwire shell --connect 127.0.0.1:7687 --graph tickets --view memory
./target/release/brainwire shell --connect 127.0.0.1:7687 --graph tickets --view indexes --lang pt-BR
./target/release/brainwire shell --connect 127.0.0.1:7687 --graph tickets --view memory \
  --snapshot --width 120 --height 30
```

The address is a numeric IP and port, including bracketed IPv6, rather than a
Bolt URL or DNS name. Credentials use the existing private JSON file described
in [protected Bolt connections](bolt-auth.md). Native TLS is pending; retain the
trusted loopback or encrypted tunnel boundary. Internal graph/schema identifiers
remain English; labels and explanations support English and `pt-BR`.

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

## Refresh, bounds and failures

Both views show the source server, graph and sample age. Press `r` for a fresh
complete snapshot and `q` to exit. A failed refresh retains the previous data
and original sample age with **STALE / DESATUALIZADO**. Initial failure exits
without fabricated data. Successful refresh clears stale status; indexes retain
the selection by label/property when possible.

Refresh is synchronous: input waits until the fetch returns. The default total
network deadline is 3,000 ms; `--timeout-ms` accepts 1..10000 and includes
connection, authentication, partial reads/writes and result completion. Local
rendering and OS scheduling are not hard real-time guarantees. There is no
background polling or automatic graph discovery.

Memory accepts four nonnegative integer counters, or four nulls, one record
and successful completion within **64 KiB aggregate response bytes**. Indexes
accept at most **200 unique label/property rows and 128 KiB aggregate response
bytes**. Larger, malformed, duplicate, incomplete or trailing responses fail;
the shell never presents a partial result as complete. Index display strings
are escaped and bounded. Wire limits are not exact heap/RSS limits, nor do they
turn the server's materialized query results into a streaming executor.

Plain snapshots accept 80..240 columns and 24..80 rows. Interactive views also
remain safe in smaller terminals, although explanations may not fit. `NO_COLOR`
removes color. The chosen strong-green/cyan Observatory identity is retained.

For the complete schema contract, controls and separate bounds, see
[connected ontology](connected-ontology.md). Actual candidate captures and
acceptance receipts are linked from the [October 12 MVP evidence](benchmarks/2026-10-12-mvp.md).
