# Actual connected Ontology captures, October 5

These are actual interactive TUI frames, captured from a controlling PTY against
an isolated local Bolt server, decoded with pyte and rendered with Pillow/Menlo.
They are terminal renders, not generated UI mockups. The adjacent text files
retain each frame. No user graph data was used.

| Scenario | Capture |
| --- | --- |
| English, 160x40, server running | [image](connected-en-160x40.png) |
| Portuguese, 80x24 | [image](connected-pt-BR-80x24.png) |
| Server stopped, `r` failed; original snapshot retained | [image](connected-stale-160x40.png) |
| Failed-refresh warning at 40 columns | [image](connected-stale-40x24.png) |
| Same TUI after server restart, new schema and `r` | [image](connected-refreshed-160x40.png) |

The temporary `tickets` graph starts with the shared Customer/Company/Segment
ontology fixture. The recovery check stops the server, applies an ontology update
adding Ticket and Customer -[:OPENED]-> Ticket, restarts it at the same address,
and refreshes the still-open TUI. The image shows catalog v2. There are zero
instance nodes/edges throughout this capture; these are declared schema types.
The capture helper asserts the initial types, stale status, new version/type,
clean terminal exits and unchanged zero instance counts.

The refreshed schema is a snapshot, not continuous telemetry. The green identity
and existing ontology renderer are retained. Cache is 64 MiB and memtables 16 MiB
within that budget. Test servers are stopped and temporary databases removed.

## Reproduce

From the repository root, using the built debug CLI:

```sh
cargo build --locked --offline -p brainwire-server --bin brainwire
uv run --no-project --with pyte --with pillow python docs/design/assets/2026-10-05-connected/capture.py
uv run --no-project --with neo4j==5.28.6 python docs/design/assets/2026-10-05-connected/python_acceptance.py
```

Menlo on macOS is the default font; `BRAINWIRE_CAPTURE_FONT` can select another
monospace font. These Python packages are artifact/test tools, not new Brainwire
runtime dependencies. Helpers use synthetic temporary graphs only.

## Concurrent Python acceptance

With Neo4j Python driver 5.28.6, two readers completed 40 `CALL db.schema()`
queries while a writer committed 10 batches of 20 Customers. Every schema result
contained the three expected types and all three relationship signatures,
including both IN_SEGMENT pairs. The final Customer count was 200; connected CLI
snapshot and a post-shutdown `db check` both passed. This is an integration
acceptance test, not a throughput or latency benchmark.

The Rust workspace passed 409 tests after this feature. Added checks cover caps,
expanded schema accounting, snapshot isolation, rollback, malformed and oversized
Bolt responses, slow-drip deadlines, delayed trailing records, graph selection,
missing/empty graphs, no local configuration/storage access and stale UI recovery.
Workspace clippy, formatting and diff checks passed. See the
[connected guide](../../../connected-ontology.md) for bounds and known limitations.
