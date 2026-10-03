# Actual offline TUI captures, October 3, second session

Source: local commit `410e9ae`, native debug binary. An isolated synthetic graph
contains 100,000 Customers with indexed email and active properties (200,000
index entries); a second named graph is empty. Data was ingested through the
embedded Cypher CLI in batches of 250 and audited before capture. No real user
records were used. Temporary graphs were removed afterward.

The running interactive TUI was captured from a PTY, decoded with pyte and
rendered cell-by-cell with Pillow/Menlo. These are terminal renders, not desktop
screenshots or image-generated mockups. Adjacent `.txt` files retain terminal
text. The selected Observatory reference remains the design target; this is
its current offline inspection implementation.

| Panel | English 160x40 | Portuguese 160x40 | English 80x24 |
| --- | --- | --- | --- |
| Graph | [image](graph-160x40.png) | [image](graph-pt-BR-160x40.png) | [image](graph-80x24.png) |
| Pipeline | [image](pipeline-160x40.png) | [image](pipeline-pt-BR-160x40.png) | [image](pipeline-80x24.png) |
| Memory | [image](memory-160x40.png) | [image](memory-pt-BR-160x40.png) | [image](memory-80x24.png) |
| Disk | [image](disk-160x40.png) | [image](disk-pt-BR-160x40.png) | [image](disk-80x24.png) |

[Narrow Disk F1 explanation](disk-explain-80x24.png). PgUp/PgDn scroll explanations
that exceed the terminal. Graph counts use `--estimated`; displayed values matched
the fixture after flush but are RocksDB estimates rather than guaranteed counts.

Cache 64 MiB, memtables 4 MiB inside that budget, query guard/pool setting 32 MiB,
execution deadline 30,000 ms. Memory readings belong to the offline inspector;
the TUI does not observe reservations in another server process. The Memory panel
explains `CALL db.memory()` for live server counters. Neither accounting nor the
configuration is an RSS measurement. No server is running during inspection.
