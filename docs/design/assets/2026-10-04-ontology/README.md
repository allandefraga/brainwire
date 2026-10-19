# Actual ontology TUI captures, October 4

These images render the running `brainwire shell --overview --view ontology`
from a real controlling PTY, decoded with pyte and drawn cell-by-cell with
Pillow/Menlo. They are terminal renders, not image-generated mockups. Adjacent
`.txt` files retain the terminal text. The selected Observatory reference stays
the design target.

The temporary `ontology` graph uses `crates/core/tests/fixtures/graph_mapping.json`:
Customer, Company and Segment; WORKS_AT and both IN_SEGMENT signatures. It has
zero instance nodes/edges. The `knowledge` fixture demonstrates an isolated type;
the default graph demonstrates no declared ontology. Captures show actual
persisted catalogs, not a fabricated instance network. Temporary storage is
removed after capture. Cache is 64 MiB and memtables 16 MiB within that budget.

| State | Capture |
| --- | --- |
| English, 160x40 | [image](ontology-en-160x40.png) |
| Portuguese, 160x40 | [image](ontology-pt-BR-160x40.png) |
| Company selected with Down | [image](ontology-company-160x40.png) |
| English, 80x24 | [image](ontology-en-80x24.png) |
| Compact inspector opened with Enter | [image](ontology-inspector-80x24.png) |
| Inspector scrolled with PgDn | [image](ontology-inspector-scrolled-80x24.png) |
| Resized from 160x40 to 80x24 with inspector open | [image](ontology-resized-inspector-80x24.png) |
| Isolated type | [image](ontology-isolated-160x40.png) |
| No declared ontology | [image](ontology-empty-80x24.png) |
| Existing synthetic 100K-node/1M-edge graph | [image](ontology-staging-100k-160x40.png) |

The last image uses the existing local `staging-100k` graph: its ontology is just
Page -[:LINK]-> Page. The panel therefore shows one declared self-loop, regardless
of the million stored instance edges. Ontology views are read-only and offline;
stop the server before opening the same RocksDB directories.

## Reproduce the temporary captures

From the repository root, build the debug CLI, then run the artifact helper:

```sh
cargo build --locked --offline -p brainwire-server --bin brainwire
uv run --no-project --with pyte --with pillow python docs/design/assets/2026-10-04-ontology/capture.py
```

The helper creates isolated synthetic databases and checks key navigation,
inspector content, live resizing, continued process liveness and clean exit. It writes the
images/text next to itself. It defaults to macOS Menlo; set
`BRAINWIRE_CAPTURE_FONT` to another monospace font path on other Unix hosts.
The Python packages are capture tools only, not Brainwire runtime dependencies.
Add `--existing-100k` only when the local synthetic `staging-100k` graph exists
and is available for exclusive offline access.

## Measured startup, not query performance

[Raw timings and commands](startup-timings.json): local macOS ARM64, debug build,
one unmeasured warm-up followed by five separate process runs per command.
Each process opens the local registry (analytics-demo, staging-100k and
default), renders a 160x40 text snapshot and exits. Medians:

| Initial panel | Median elapsed |
| --- | ---: |
| Ontology, automatic RocksDB count estimates | 70.679 ms |
| Graph, exact counts | 912.528 ms |
| Graph, explicit estimated counts | 83.524 ms |

This demonstrates the avoided full count scan on this fixture. It is not a
query benchmark, cold-cache measurement, production SLA or a claim about
billion-element graphs. Rendering cost also depends on schema size; this graph
has a one-type ontology. Index status and graph counts remain separate concepts.

A separate isolated smoke check also loaded 2,000 types and 1,999 declared
signatures and rendered the first type successfully. It had no instance data;
that one run validates larger catalog handling, not a throughput claim.
