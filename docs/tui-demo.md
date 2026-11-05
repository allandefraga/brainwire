# Use the terminal UI

Brainwire has three terminal modes. Choose the one that matches what you want
to inspect:

| Mode | Command | Data |
| --- | --- | --- |
| Visual demo | `brainwire shell --demo` | Synthetic examples; no real queries or file access |
| Connected workspace | `brainwire shell --connect 127.0.0.1:7687 --graph demo` | Read-only data fetched from a running server |
| Offline overview | `brainwire shell --overview` | Real local files and named graphs; server must be stopped |

The default language is English. Add `--lang pt-BR` for Portuguese interface
text; stored names and values stay unchanged. Use `q` to exit and `?` for help.

## Build or use the Docker image

From a source checkout, build the CLI/server/TUI binary:

```sh
cargo build --locked --release -p brainwire-server
./target/release/brainwire shell --demo
```

The first build needs Rust and native RocksDB build dependencies. Use
`--offline` only after dependencies are cached. The examples below use
`brainwire` for readability; replace it with `./target/release/brainwire` if
the binary is not on your PATH.

With the [Docker demo](container.md#start-a-local-demo-with-one-command) already running:

```sh
docker compose -p brainwire-demo -f deploy/compose.demo.yaml \
  exec brainwire brainwire shell --demo
```

An interactive terminal is required on both input and output. Use snapshot mode
for scripts or environments without a terminal.

## Explore the visual demo

```sh
brainwire shell --demo
brainwire shell --demo --lang pt-BR
```

The demo shows Graph, Query, Memory and Disk & Files using synthetic values.
The query page accepts only its guided examples; it does not execute arbitrary
Cypher. Use [Python](python-client.md) or the [offline Cypher command](local-cypher.md)
for real mutations and queries.

| Key | Demo action |
| --- | --- |
| `1`–`4` | Graph, Query, Memory, Disk & Files when not editing |
| `Tab` | Switch the focused content/explanation panel |
| Arrow keys | Move the selection |
| `Enter` | Inspect or begin editing, depending on the page |
| `F5` | Run a supported guided query |
| `PgUp` / `PgDn` | Scroll the focused inspector or editor viewport |
| `F1` | Page help, including while editing |
| `?` | Help outside editing; ordinary query text inside the editor |
| `Esc` | Close help or finish editing |
| `q` | Exit outside editing |

The footer shows controls for the current page. The selected graph node and its
links remain identifiable without color. `NO_COLOR` disables the palette;
`--no-anim` provides a quiet/static presentation.

## Inspect a running graph

```sh
brainwire shell --connect 127.0.0.1:7687 --graph demo
```

The connected workspace opens Ontology, Memory, Indexes and Graph. Use `Tab` or
`1`–`4`, `r` to refresh, and `?` for explanations. It is read-only and has no
arbitrary query editor, property editor or graph selector. The Graph pane needs
an explicit source/type selection; see the
[connected workspace guide](connected-status.md).

All connected data travels over Bolt. The TUI does not open the running server's
RocksDB files. A protected server needs the credentials file described in
[protected Bolt](bolt-auth.md). Native TUI connections use plaintext Bolt, so
keep them on trusted loopback or behind an appropriate encrypted tunnel.

## Inspect local files with the server stopped

```sh
brainwire shell --overview
brainwire shell --overview --graph demo --config /path/to/brainwire.toml
```

This overview reads real named local graphs and their files. RocksDB requires
exclusive ownership: stop any server using those directories first. It is an
inspector, not a job scheduler or mutation editor. See the
[multi-graph backoffice](multi-graph-backoffice.md) for its controls and exact
inspection limits.

## Capture a plain-text frame

```sh
brainwire shell --demo --snapshot --view graph --width 160 --height 48
brainwire shell --demo --snapshot --view memory --width 120 --height 36
brainwire shell --connect 127.0.0.1:7687 --graph demo \
  --snapshot --view indexes --width 120 --height 30
```

Demo views are `graph`, `query`, `memory` and `disk`. Connected views are
`workspace`, `ontology`, `memory`, `indexes` and `graph`; the connected default
is `workspace`. Its snapshot captures the initial Ontology pane. Snapshot sizes
are 80–240 columns and 24–80 rows. Connected Graph snapshots need the four graph
selection flags described in the connected guide.

## Actual terminal preview

These are dated captures of the executable **synthetic demo**, taken at 160×48
on 2026-09-27. They illustrate that version's layout, not current server data or
a complete picture of the present connected workspace. Font rendering varies
between terminals.

![Graph demo capture](design/assets/brainwire-tui-demo-graph.png)

![Memory demo capture](design/assets/brainwire-tui-demo-memory.png)

![Disk and files demo capture](design/assets/brainwire-tui-demo-disk.png)

The [connected workspace guide](connected-status.md) shows the newer bounded
Graph renderer and documents its actual fetch and navigation limits. Demo RAM
numbers and file rows are illustrative. Connected Memory reports query
reservations; neither is a measurement of total process RSS.

## Toward the connected MVP

Current connected panels provide schema, query-pool counters, index state and a
bounded outgoing relationship sample. Interactive graph expansion, arbitrary
queries, editing, live file browsing and import-job controls remain future work.
Check [current capabilities and limits](../README.md#current-limits) before
using a screenshot as a feature reference.
