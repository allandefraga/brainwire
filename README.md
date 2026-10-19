<p align="center">
  <img src="assets/brainwire-logo.svg" alt="Brainwire" width="520">
</p>

<h1 align="center">Context has connections.</h1>

<p align="center"><strong>A graph database for the context your AI apps need to remember.</strong><br>
Connect people, projects, events and documents. Keep those relationships durable. Query the graph with Cypher.</p>

<p align="center"><code>RUST</code> &nbsp; <code>ROCKSDB</code> &nbsp; <code>BOLT</code> &nbsp; <code>CYPHER</code> &nbsp; <code>PYTHON</code></p>

<p align="center">
  <a href="#quickstart">Get started ↓</a> &nbsp;·&nbsp;
  <a href="docs/local-cypher.md">Cypher guide</a> &nbsp;·&nbsp;
  <a href="docs/2026-10-pilot-roadmap.md">Roadmap &amp; beta limits</a>
</p>

<p align="center">
  <img src="docs/design/assets/brainwire-tui-demo-graph.png" alt="Brainwire terminal backoffice visualizing a synthetic connected graph" width="100%">
</p>
<p align="center"><sub>Live terminal UI · 36 synthetic nodes · 64 relationships · no customer data</sub></p>

Brainwire is an independent graph database for builders who need structured
relationships and durable state behind their applications. It is written in
Rust, stores data in RocksDB, speaks Bolt, and accepts bounded Python batches.
Run it locally or in Docker with a persistent volume.

**Brainwire 0.1 is the graph database layer**—not an agent runtime or an
out-of-the-box RAG pipeline. Your application chooses what to ingest; Brainwire
keeps entities and relationships connected so your app can ask better questions.

> **0.1.1 · beta prerelease.** This release adds bounded relationship deletion
> from Cypher and Python. Brainwire is under active development and has
> not been qualified for production. Check the beta checklist below before
> using important data; later changes on `main` may differ from the tag.

## ⚡ Quickstart

You need Docker Engine and Docker Compose. From the repository root, this builds
the image, creates a persistent demo graph with a bundled synthetic ontology,
and waits for the server to become ready:

```sh
docker compose -p brainwire-demo -f deploy/compose.demo.yaml \
  up --build --wait --wait-timeout 900
```

The server listens on `bolt://127.0.0.1:7687`; the graph is named `demo`. The
first build compiles RocksDB and can take several minutes. If that port is in
use, set `BRAINWIRE_BIND_PORT=17687` before the command and connect to port
17687.

### 🐍 Feed it from Python

This example uses the standard Neo4j Bolt driver. It inserts nodes and edges
with parameterized Cypher, then reads the relationship back. The bundled demo
server has authentication disabled and binds the published port to localhost;
use synthetic data only with this quickstart.

```sh
python3 -m venv .venv
.venv/bin/python -m pip install neo4j==6.3.0
.venv/bin/python - <<'PYTHON'
from neo4j import GraphDatabase

customers = [
    {"id": "customer-1", "name": "Ada", "email": "ada@example.test"},
    {"id": "customer-2", "name": "Grace", "email": "grace@example.test"},
]
segments = [{"id": "segment-1", "name": "beta"}]
links = [
    {"customer_id": "customer-1", "segment_id": "segment-1"},
    {"customer_id": "customer-2", "segment_id": "segment-1"},
]

with GraphDatabase.driver("bolt://127.0.0.1:7687", auth=None) as driver:
    with driver.session(database="demo") as session:
        session.run("""
            UNWIND $rows AS row
            MERGE (c:Customer {entity_id: row.id})
            SET c.name = row.name, c.email = row.email
        """, rows=customers).consume()
        session.run("""
            UNWIND $rows AS row
            MERGE (s:Segment {entity_id: row.id})
            SET s.name = row.name
        """, rows=segments).consume()
        session.run("""
            UNWIND $rows AS row
            MATCH (c:Customer {entity_id: row.customer_id})
            MATCH (s:Segment {entity_id: row.segment_id})
            MERGE (c)-[:IN_SEGMENT]->(s)
        """, rows=links).consume()
        result = sorted(session.run("""
            MATCH (c:Customer)-[:IN_SEGMENT]->(s:Segment)
            RETURN c.name AS customer, s.name AS segment
        """).data(), key=lambda row: row["customer"])
        print(result)
PYTHON
```

Expected result:

```text
[{'customer': 'Ada', 'segment': 'beta'}, {'customer': 'Grace', 'segment': 'beta'}]
```

The demo schema includes these node labels and the `IN_SEGMENT` relationship.
For your own graph, create it with your ontology before starting the server.
See the [container guide](docs/container.md) for persistent volumes, custom
schemas, restart checks and running Python from another container.

Stop the demo and keep its data:

```sh
docker compose -p brainwire-demo -f deploy/compose.demo.yaml down
```

Do not add `--volumes` unless you intend to delete the demo database.

## What ships in 0.1

- Persistent named graphs stored in RocksDB, with atomic local transactions and
  WAL durability.
- Bolt connectivity, a growing Cypher subset, and node/edge creation, merge,
  property updates and removals.
- Equality indexes for supported scalar values and typed adjacency lookups.
- A Python convenience client with bounded node cleanup and independent edge
  deletion, plus JSONL and source-snapshot import examples.
- Graph operations including PageRank, shortest path, reachability, WCC and SCC.
- A terminal backoffice with a synthetic demo, an offline local inspector and
  a connected read-only workspace for ontology, memory, indexes and bounded
  graph neighborhoods.
- Local Docker/Compose packaging with retained-volume restart and health checks.

For the Python helper's batch API, property semantics, and supported inputs, see
[Python client and ingestion](docs/python-client.md). The helper currently lives
in this repository; it is **not** a separately published Python package.

## Releases, images and docs

Every push to `main` runs formatting, workspace tests, Clippy, Python helper
tests and a real Bolt/Python ingestion and restart check. The `v*` tag workflow
repeats those checks, builds the Docker image from that exact tag, publishes the
versioned image to GitHub Container Registry (GHCR), and creates a GitHub
prerelease with generated notes. It does not publish a floating `latest` tag.
The docs site deploys from `docs-site/` through GitHub Pages.

After the first successful release workflow, pull the private image with:

```sh
docker login ghcr.io
docker pull ghcr.io/allandefraga/brainwire:v0.1.1
```

GHCR inherits this repository's private access. Authenticate with a GitHub
token that has package read access before pulling. The Compose quickstart above
builds locally from source; see the [container guide](docs/container.md) for
using a published image and retaining its data volume.

## See what the TUI can do

After building the binary locally, inspect the running demo server:

```sh
./target/release/brainwire shell --connect 127.0.0.1:7687 --graph demo \
  --node-label Customer --entity-id customer-1 \
  --edge-type IN_SEGMENT --target-label Segment
```

Run the Python quickstart first to populate that neighborhood. Switch between
Ontology, Memory, Indexes and Graph with `Tab`, refresh with `r`, open help with
`?` and quit with `q`. Add `--lang pt-BR` for Portuguese interface text.
The connected workspace is read-only; run Cypher and mutations through Python
or the CLI. See the [connected workspace guide](docs/connected-status.md) for
credentials, permissions, stale samples and the exact meaning of memory values.

For the visual demo, run `./target/release/brainwire shell --demo`. Its queries,
files and memory values are synthetic. For real local files and named graphs,
use the [offline backoffice](docs/multi-graph-backoffice.md) with the server
stopped so the shell can acquire the RocksDB lock.

## The honest beta checklist

- This is not full Neo4j Cypher compatibility. See the exact supported grammar
  and examples in the [Cypher guide](docs/local-cypher.md).
- The demo Compose profile has authentication disabled. The server also defaults
  to unprotected when no auth policy is configured. Do not expose that profile
  to an untrusted network. Read [server security](docs/serve.md),
  [protected containers](docs/protected-containers.md) and
  [transport guidance](docs/protected-transport.md) before connecting other
  services.
- Versioned ontology/catalogs do **not** mean historical data revisions. Durable
  history and as-of traversal for both nodes and edges are not implemented.
- DuckLake ingestion is version-pinned snapshot comparison, not native CDC. A
  production source mapping, authoritative ACL integration and representative
  application workload still need validation.
- Three-voter quorum replication is experimental. Current local fault tests do
  not qualify multi-machine Kubernetes/Swarm failover or production recovery.
- Text/vector indexes, arbitrary object serialization, sharding and a published
  SDK are not included.
- Performance receipts use synthetic local workloads. They are not production
  SLOs or a comparison against another database.

The current release boundary and acceptance evidence are tracked in the
[October beta roadmap](docs/2026-10-pilot-roadmap.md). For an application trial,
start in **shadow mode** with a rebuildable graph and keep the existing source as
the authority until integration, security, recovery and workload gates pass.

## Pick your next move

| Goal | Guide |
|---|---|
| Install, run, restart, and connect Docker services | [Docker](docs/container.md) · [orchestrators](docs/orchestrators.md) |
| Write nodes and edges from Python | [Python client](docs/python-client.md) · [Python ingestion](docs/python-ingestion.md) |
| Understand supported Cypher and functions | [Cypher](docs/local-cypher.md) |
| Inspect equality indexes and query lookup behavior | [Indexes](docs/indexes.md) |
| Create graphs and apply an ontology | [Quickstart](docs/mvp-quickstart.md) · [ontology](docs/connected-ontology.md) |
| Inspect a running graph through the terminal | [Connected workspace](docs/connected-status.md) |
| Explore the visual demo or inspect local files | [TUI demo and controls](docs/tui-demo.md) · [offline backoffice](docs/multi-graph-backoffice.md) |
| Resume version-pinned source snapshot differences | [DuckLake incremental ingestion](docs/ducklake-incremental.md) |
| Configure authentication and network protection | [Auth](docs/bolt-auth.md) · [container auth](docs/protected-containers.md) · [TLS boundary](docs/protected-transport.md) |
| Evaluate experimental quorum replication | [Quorum operations and limitations](docs/quorum-replication.md) |
| Review measurements, recovery and remaining gates | [Beta roadmap](docs/2026-10-pilot-roadmap.md) · [operator checklist](docs/beta-operator-checklist.md) |
| See design decisions | [Design documents](docs/design/) · [session journal](JOURNAL.md) |

## Build it locally

Install Rust stable as selected by `rust-toolchain.toml`. The Docker builder
uses Rust 1.98.1. The commands below use `--offline`, so dependencies must already
be cached. On the first connected build, run `cargo fetch --locked` first.
Then run:

```sh
cargo test --workspace --locked --offline
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
cargo fmt --all -- --check
```

Build the server binary with `cargo build --release --locked --offline -p brainwire-server`.
The [local demo](examples/python/local_demo.py) exercises Python batches,
Cypher, indexes, restart and the connected TUI with synthetic data.

## Release status and license

This is a private beta prerelease. The Rust workspace declares `publish =
false` and `UNLICENSED`; the workflow publishes a versioned container image and
GitHub release, but no Rust crates or Python package. Choose and review a
license before wider distribution. The image is a delivery artifact, not a
production-readiness claim. See the [application delivery](docs/container.md#application-delivery)
guide and the [journal](JOURNAL.md) for evidence and remaining release gates.
