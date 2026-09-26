# Brainwire terminal demo

The first interactive shell demonstrates the selected electric-green identity
and the Graph, Query, Memory and Disk & Files workflow. It uses synthetic data.
It does not connect to a running database, inspect real files or execute
arbitrary Cypher yet.

From the standalone repository:

```sh
cd /Users/fraga/Documents/brainwire
cargo run --locked --offline -p brainwire-server -- shell --demo
```

Dependencies must already be downloaded for `--offline`. The default UI is
Portuguese; add `--lang en` for English. The built binary can also be launched
directly with `./target/debug/brainwire shell --demo` after building it.

## Actual terminal preview

These images render ANSI output captured from the running binary in a 120×36
pseudo-terminal (pt-BR, color enabled). They are executable-demo captures,
separate from the earlier generated design concepts. Font rendering varies
between terminal emulators.

![Graph demo](design/assets/brainwire-tui-demo-graph.png)

![Memory demo](design/assets/brainwire-tui-demo-memory.png)

## Explore the demo

| Key | Action |
|---|---|
| `1`, `2`, `3`, `4` | Switch to Graph, Query, Memory or Disk outside editing |
| `Tab` | Change focus |
| Arrow keys | Move selection; scroll query results with Up/Down |
| `PgUp`, `PgDn` | Move the query editor viewport |
| `Enter` | Inspect a selection or enter editing, according to focus |
| `F1` | Toggle contextual explanation |
| `F5` | Run the supported guided query example |
| `Esc` | Leave editing or help before exiting |
| `q` | Quit when not editing |

The footer shows the current controls. Typed shortcut characters remain text
while editing. A changed query outside the guided examples is refused
explicitly instead of receiving a fabricated successful result.

Memory figures and file rows are illustrative. The memory view explains the
difference between a container limit, an internal component budget and actual
usage. Memtables are included in cache accounting; query-budget enforcement
is not implemented by this demo. The disk inspector explains the roles of
SST, WAL, MANIFEST, CURRENT, OPTIONS, LOG and LOCK without opening or modifying
a real database directory.

## Preview without a terminal

The snapshot mode renders the same widgets once as plain text:

```sh
cargo run --locked --offline -p brainwire-server -- shell --demo --snapshot --view graph --width 160 --height 48
cargo run --locked --offline -p brainwire-server -- shell --demo --snapshot --view memory --lang en --width 120 --height 36
```

Views are `graph`, `query`, `memory` and `disk`. Snapshot dimensions are bounded
to 80..240 columns and 24..80 rows. Interactive mode requires a terminal on
both input and output. `--no-anim` and `NO_COLOR` are available for quiet or
plain presentation.

## Toward the connected MVP

The next engine work is the Cypher executor, followed by real Bolt
transaction handling and `brainwire serve`. A connected explorer additionally
needs bounded neighborhood results, and live memory/file views need the
server telemetry and storage-inspection contracts. The
[Observatory design](design/2026-09-26-brainwire-observatory.md) records those
interfaces and the selected identity.
