# Multiple graphs and the local backoffice

Brainwire can store several independent named graphs under one configured
`server.data_dir`. Each name maps to a separate RocksDB directory and ontology
catalog. `client_memory` is only the default name; it is not a limit of one
graph. The Bolt server opens existing names and clients choose a database in
their session. Cache and memtable budgets are shared by the process, so the
number of graphs and their active working sets still matter.

## What was on this checkout on 2026-10-01

The following values came from `brainwire db overview --json` in this local
checkout. They are exact graph counts from a full scan, not forecasts:

| Named graph | Nodes | Relationships | Ontology | Labels | Edge types |
| --- | ---: | ---: | --- | --- | --- |
| `analytics-demo` | 3 | 2 | v1 | `Page` | `LINK` |
| `staging-100k` | 100,000 | 1,000,000 | v1 | `Page` | `LINK` |
| `client_memory` | 0 | 0 | none | none | none |

The `staging-100k` directory occupied about 9.5 MiB on this host when
measured with `du -sh`; this synthetic fixture is highly compressible and
cannot predict production storage size. RocksDB's `node` and `pk` families
each estimated 100,000 keys; `out` and `in` each estimated 1,000,000; `idx`
and `ident` were empty. These key counts and bytes are estimates, not process
RAM usage. The [query/ontology audit](benchmarks/2026-10-01-query-ontology-audit.md)
records the pre-index baseline. New string/Boolean equality indexes populate
`idx` on indexed graph types; `staging-100k` declares no indexed fields.

## Run and inspect

From `/Users/fraga/Documents/brainwire`, after building the binary:

```sh
cargo build --locked --offline -p brainwire-server --bin brainwire
./target/debug/brainwire db list
./target/debug/brainwire db overview
./target/debug/brainwire db overview --json
./target/debug/brainwire db overview --estimated --json
./target/debug/brainwire shell --overview --lang pt-BR
./target/debug/brainwire shell --overview --estimated --lang pt-BR
./target/debug/brainwire shell --overview --snapshot --graph staging-100k --lang pt-BR --width 120 --height 30
./target/debug/brainwire db schema staging-100k --json
./target/debug/brainwire db metrics staging-100k --json
```

`shell --overview` is a **real, read-only, offline** TUI. Up/Down chooses a
graph and the inspector shows its exact node/edge counts, ontology version,
node labels and edge types; `q` or Esc exits. `--graph` sets the initial
selection, including in `--snapshot` mode. It opens the RocksDB directories
exclusively and scans every graph for counts, so stop `brainwire serve` first
and expect cost on large datasets. Add `--estimated` to use quick RocksDB
key-count estimates instead of the full scan; the output labels those values
as estimates. An unknown graph name is an error.

On this warm local 100K/1M fixture, one measured exact overview took 1.053 s
and one estimated overview took 0.078 s. They returned the same counts in
this run; that agreement is not a guarantee that RocksDB estimates are exact
after other write/compaction states. Neither measurement extrapolates to
billion-element graphs.

Create another independent graph and apply its own ontology:

```sh
./target/debug/brainwire db create tickets --ontology crates/core/tests/fixtures/graph_mapping.json
./target/debug/brainwire db create knowledge --ontology crates/core/tests/fixtures/graph_mapping.json
./target/debug/brainwire db overview
```

These are examples; choose an ontology appropriate to each graph. Do not
reapply the sample mapping merely to change an existing production graph.
Offline `brainwire cypher --database ./data/tickets --query '…'` addresses one
graph, while `brainwire serve` lets a Bolt driver choose a named database:

```python
from neo4j import GraphDatabase

with GraphDatabase.driver("bolt://127.0.0.1:7687", auth=None) as driver:
    with driver.session(database="tickets") as session:
        print(session.run("RETURN 1 AS ready").single()["ready"])
```

The server is single-node and currently has no authentication or TLS, so the
example is for a trusted local test network. The [server guide](serve.md)
covers configuration and transaction behavior.

## What each terminal surface can do

| Surface | Operations now | Data source |
| --- | --- | --- |
| `shell --overview` | Select named graph; inspect counts and ontology summary | Real local RocksDB, read only |
| `shell --demo` | Explore Graph, Query, Memory, Disk & Files; select nodes/files; run two guided queries; read F1 explanations | Synthetic fixture, no real writes |
| `db create/apply-ontology` | Create and update a named graph's catalog | Real local RocksDB, offline |
| `db stats/schema/metrics/overview` | Inspect counts, ontology and storage estimates | Real local RocksDB, offline |
| `cypher --database` | Execute the supported Cypher subset against one graph | Real local RocksDB, offline |
| `serve` | Query multiple named graphs over Bolt, selected by session | Real local RocksDB, online |

The current TUI does not run arbitrary Cypher, inspect actual SST files, edit
ontologies, delete graph data or perform maintenance jobs. The next connected
backoffice slice should expose a bounded per-graph neighborhood and read-only
query results through the server, then live storage and memory telemetry. Any
mutation from the TUI needs explicit scope to the selected graph and an
auditable operation; ontology changes must never implicitly delete old data.
