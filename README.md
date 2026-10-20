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

> **v0.2.0-beta.1 · private beta candidate.** <!-- brainwire-release-version --> Brainwire is under active development and has
> not been qualified for production. The release pipeline publishes a versioned
> image only after source and container recovery checks pass.

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
- Explainable `knowledge.rank` source ranking with per-query evidence weights.
- A terminal backoffice with a synthetic demo, an offline local inspector and
  a connected read-only workspace for ontology, memory, indexes and bounded
  graph neighborhoods.
- Local Docker/Compose packaging with retained-volume restart and health checks.

For the Python helper's batch API, property semantics, and supported inputs, see
[Python client and ingestion](docs/python-client.md). The helper currently lives
in this repository; it is **not** a separately published Python package.

## Releases, images and docs

Every push to `main` runs formatting, workspace tests, Clippy, Python helper
tests, release-version tests and a real Bolt/Python ingestion and restart check.
The `v*` tag workflow repeats those checks, builds a versioned Docker image,
verifies the image through Python/Bolt writes and retained-volume restarts,
then pushes that same image to the private GitHub Container Registry (GHCR) and
creates a GitHub release with its digest attached. A `-beta.N` or `-rc.N` tag
creates a prerelease; a plain SemVer tag creates a stable release. It does not
publish a floating `latest` tag.
The docs site in `docs-site/` is link-checked and packaged as a private Actions
artifact on pushes to `main`. GitHub Pages is unavailable for this private repo
on its current plan; publishing a live public site requires a different plan or
an external host.

The source is tagged `v0.2.0-beta.1`. <!-- brainwire-release-version --> Its release
workflow failed before publishing because the GitHub runner ran out of disk
space ([run details](https://github.com/allandefraga/brainwire/actions/runs/39229131214)).
The revised pipeline reduces Rust build artifacts; the next versioned tag will
rerun checks and test the container before a private image is available.
After that, these commands authenticate Docker to GHCR and download the server
image to your local Docker engine. They do not start Brainwire or copy graph
data:

```sh
docker login ghcr.io -u YOUR_GITHUB_USERNAME
# At the password prompt, enter a GitHub personal access token (classic)
# with read:packages access. Do not use your GitHub account password.
docker pull ghcr.io/allandefraga/brainwire:v0.2.0-beta.1 # brainwire-release-version
```

`docker login ghcr.io` authenticates Docker to GitHub Container Registry. The
token is needed because this repository and its container package are private.
The release workflow uses GitHub's built-in `GITHUB_TOKEN` to publish the image;
you only need your own token when pulling it from another machine. See GitHub's
[GHCR authentication guide](https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry#authenticating-with-a-personal-access-token-classic).
Use the [local release workflow](docs/releasing.md) to preview and create a
SemVer version, synchronize Docker/Kubernetes references, and make a local tag
without pushing it.
The Compose quickstart above builds locally from source; see the
[container guide](docs/container.md) for running the published image with a
persistent graph-data volume.

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

## Performance snapshot

These are measured local runs on synthetic data, not production SLOs. See the
[Brainwire-only receipts](docs/benchmarks/README.md) and the separate
[local diagnostic against Neo4j and FalkorDB](docs/benchmarks/comparison/README.md).

![Brainwire benchmark charts: one-hour mixed workload latency and quorum batch-write throughput](docs/benchmarks/assets/brainwire-performance.svg)

![Local synthetic workload comparison with Neo4j and FalkorDB](docs/benchmarks/assets/comparison-20261019.svg)

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
  SLOs or a comparison against another database. See the [benchmark results and
  comparison status](docs/benchmarks/README.md).

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
| Review measured performance and comparison status | [Benchmarks](docs/benchmarks/README.md) |
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

**Version:** `v0.2.0-beta.1` · private beta candidate · not production-qualified. <!-- brainwire-release-version -->
The [tag source](https://github.com/allandefraga/brainwire/tree/v0.1.1) identifies
the exact source snapshot. That tag predates adoption of Apache-2.0 and carries
no open-source license grant; use a later tagged release for Apache-licensed
distribution. The release workflow marks `-beta.N` and `-rc.N` tags as
prereleases and plain SemVer tags as stable releases. It publishes a versioned
Docker image to GHCR only after source and container recovery checks pass. Follow the
[release workflow](https://github.com/allandefraga/brainwire/actions/workflows/release.yml)
for its current status. It does not publish a Rust crate or Python package. The
image is a delivery artifact, not a production-readiness claim; see the
[beta checklist](#the-honest-beta-checklist).

**License:** Brainwire uses [Apache-2.0](LICENSE), a permissive open-source
license. People may use, modify, redistribute, and sell the software, including
for commercial and BYOC deployments, while following its notice and license
terms. Others may sell their own versions too; this license does not reserve
exclusive commercial rights. The Docker image includes the license at
`/usr/share/licenses/brainwire/` and carries the `Apache-2.0` license label.
`publish = false` is separate: it keeps these Rust crates from being uploaded to
package registries such as crates.io. It does not prevent use of the source or
Docker image and does not control GitHub releases.

The GitHub repository and GHCR package are still private, so this license
change alone does not make the source or image available to the public.
