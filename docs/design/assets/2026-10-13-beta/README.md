# Connected beta workspace: 100,000-node terminal captures

These 13 PNGs contain actual interactive terminal cells from the frozen native
Brainwire release binary. `capture.py` launches a controlling PTY, decodes the
terminal stream with pyte, and renders its cells with Pillow and macOS Menlo.
Menlo lacks Braille glyphs, so the renderer draws the exact dot bits of each
Unicode Braille cell. No concept image or simulated graph response is used.
Each PNG has a matching text file preserving the captured cells.

The isolated synthetic graph contains **100,000 nodes and 99,999 edges**:
one `Ticket` with `entity_id = ticket-high-degree` and 99,999 `TicketComment`
nodes connected through outgoing `HAS_COMMENT` relationships. Its ontology
comes directly from `examples/source_ingestion/ontology.json`. Fresh records and edges
are created in batches of 250 through the Python Neo4j 6.3.0 driver, with a
1 MiB Bolt payload cap, 16 MiB decoded-message cap and 64 MiB query pool.
The driver verifies the exact node and edge counts; offline `db stats` confirms
them again after the server stops. Fixture population plus verification took
about four seconds in this run; this small-property fixture is not a general
import benchmark.

The captured binary SHA-256 is
`1042c946cb85d6c6d5b234db85163f3478ab505f20b4fb896e3b8e515d963150`.
[receipt.json](receipt.json) records the ontology hash, fixture counts, binary
hash, driver version and cleanup results. Counts describe the stored fixture;
the Graph pane deliberately displays at most 20 relationship rows rather than
attempting to render all 100,000 nodes.

The shell starts **without `--view`**, proving the default connected workspace.
The four panes are visited in one terminal process by pressing `2`, `3`, `4`:

- `workspace-ontology-en-120x30`: real declared schema, relationship direction
  and property/index inspector with the shared navigation sidebar.
- `workspace-memory-en-120x30`: real server query-pool counters and explanations.
- `workspace-memory-stale-en-120x30`: the server is stopped, then `r` is pressed;
  the same terminal retains its sample and marks the refresh failure.
- `workspace-memory-recovered-en-120x30`: the server restarts, then `r` succeeds
  in that same terminal; the error clears and the server lifetime counters reset.
- `workspace-indexes-en-120x30`: four real ready scalar equality indexes.
- `workspace-graph-en-120x30`: 20 actual outgoing relationship rows from the
  configured ticket, radial preview, target IDs and `MORE AVAILABLE`.
- `workspace-graph-scrolled-en-120x30`: 19 Down keys select the twentieth target;
  the visible list and highlighted relationship follow the selection.
- `workspace-ontology-pt-BR-80x24`, `workspace-memory-pt-BR-80x24`,
  `workspace-indexes-pt-BR-80x24`, `workspace-graph-pt-BR-80x24`: the four panes in
  a second shared terminal at the minimum supported size, with Portuguese UI
  and unchanged stored identifiers. The compact Graph preview shows the selected
  relationship; the target list remains scrollable.
- `workspace-reader-memory-forbidden-en-120x30`: a separate ordinary graph reader
  receives a real administrator-permission failure, without fabricated counters.
- `workspace-reader-indexes-after-forbidden-en-120x30`: the same reader terminal
  opens Indexes successfully after that failure, proving navigation remains usable.

All 13 PNGs were inspected after generation. The scripts use temporary private
service-token files, a fresh temporary store and an isolated localhost port.
Observer credentials have graph read access plus instance-administrator memory
permission; reader credentials have graph read access only. No real source data
is used. The script stops all child TUI/server processes and removes the temporary
credentials and store. Port numbers, snapshot age and lifetime reservation peaks
may differ when reproduced.

Run from the repository root:

```sh
cargo build -p brainwire-server --release --locked --offline
uv run --no-project --with neo4j==6.3.0 --with pyte --with pillow python docs/design/assets/2026-10-13-beta/capture.py --nodes 100000 --binary target/release/brainwire
```

The default fixture has 1,000 nodes; `--nodes 100000` reproduces this qualification.
Set `BRAINWIRE_CAPTURE_FONT` to a local monospace TrueType font on another OS.
Capture dependencies are not application dependencies.

Refresh remains synchronous and can wait for the transport deadline. The graph
sample covers one configured node label, entity ID, outgoing relationship type
and target label; it is not interactive expansion, full degree or a whole-graph
layout. Duplicate target IDs are allowed for parallel relationships. Memory
shows reservation accounting, not process RSS, RocksDB cache or total RAM.
