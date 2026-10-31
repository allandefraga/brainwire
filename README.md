<p align="center">
  <img src="assets/brainwire-logo.svg" alt="Brainwire" width="360">
</p>

<h1 align="center">Give your context connections.</h1>

<p align="center">
  <strong>A Rust graph database for the context behind your AI apps.</strong>
  <br>Python in. Cypher out. Relationships that stay on disk.
</p>

<p align="center">
  <a href="https://github.com/allandefraga/brainwire/actions/workflows/ci.yml"><img src="https://github.com/allandefraga/brainwire/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-65ff54?style=flat-square" alt="License: Apache-2.0"></a>
  <a href="#current-limits"><img src="https://img.shields.io/badge/status-experimental_beta-65ff54?style=flat-square" alt="Status: experimental beta"></a>
  <img src="https://img.shields.io/badge/built_with-Rust-65ff54?style=flat-square" alt="Built with Rust">
</p>

<p align="center">
  <a href="#quickstart">Run it</a> &nbsp; / &nbsp;
  <a href="#write-from-python">Python</a> &nbsp; / &nbsp;
  <a href="#see-what-the-tui-can-do">Terminal UI</a> &nbsp; / &nbsp;
  <a href="#documentation">Docs</a> &nbsp; / &nbsp;
  <a href="BENCHMARKS.md">Benchmarks</a>
</p>

<p align="center">
  <img src="assets/brainwire-hero-v2.png" alt="Brainwire concept artwork: green graph nodes connected across a dark canvas" width="900">
  <br><sub>Concept artwork · the terminal UI is described below · <a href="assets/brainwire-social-preview-v1.jpg">Social card</a></sub>
</p>

People own projects. Documents explain decisions. Tickets depend on releases.
Brainwire stores those connections as a graph you can write from Python, query
with Cypher over Bolt and inspect in your terminal. Rust runs the engine;
RocksDB persists the data. Each named graph has its own data and ontology.

Bring application records, build a knowledge graph or keep structured context
for agents. Your application controls ingestion and decides what to remember.

> **v0.13.0-beta.2 · experimental beta.** <!-- brainwire-release-version --> Built for controlled trials.
> No guaranteed global RSS ceiling. Sustained 2/4/8 GiB qualification remains
> open. Start with synthetic or rebuildable data and read the
> [current limits](#current-limits) and the [v0.12 qualification report](PRODUCTION-v0.12.md).

## What works today

| Build with | What is available |
|---|---|
| **Connected data** | Independent named graphs, ontology catalogs, nodes, edges and property patches |
| **Familiar queries** | Cypher patterns, `WITH`, `OPTIONAL MATCH`, `UNION`, aggregates and bounded regex support |
| **Index-backed reads** | Scalar equality/`IN`, string range/prefix access and online index builds |
| **Durable local state** | Atomic transactions, synchronized RocksDB WAL commits, checks and checkpoints |
| **Graph algorithms** | Sparse CSR projection, weighted PageRank, shortest path, reachability, WCC/SCC and explainable `knowledge.rank` |
| **Application ingestion** | Bounded Python upserts, Arrow helpers, resumable JSONL imports and pinned DuckLake snapshot comparison |
| **Inspection** | CLI and terminal views for ontology, query memory, indexes and a selected node's relationships |

The capabilities describe this checkout. Read the documentation at the matching
tag when using a release image. For supported query syntax, start with the
[Cypher guide](docs/local-cypher.md).

## Quickstart

**Run a versioned image, write a few records, then explore their connections.**
You need Git, Docker Engine and Docker Compose. Start with the matching checkout:

```sh
git clone --branch v0.13.0-beta.2 https://github.com/allandefraga/brainwire.git # brainwire-release-version
cd brainwire
export BRAINWIRE_IMAGE=ghcr.io/allandefraga/brainwire:v0.13.0-beta.2 # brainwire-release-version
docker compose -p brainwire-demo -f deploy/compose.demo.yaml pull
docker compose -p brainwire-demo -f deploy/compose.demo.yaml \
  up --no-build --wait --wait-timeout 900
```

Private repositories require GitHub access; private images also require
`docker login ghcr.io` with a token authorized to read the package. Public
images need no registry login. Use a release whose image publication succeeded;
the [release page](https://github.com/allandefraga/brainwire/releases) records it.

Compose creates the synthetic **`demo`** graph, waits for readiness and serves
Bolt at **`bolt://127.0.0.1:7687`**. It keeps graph files in a named volume across
container restarts. If port 7687 is busy, run `export BRAINWIRE_BIND_PORT=17687`
before starting Compose and use that port in clients.

<details>
<summary>Prefer to build the image locally?</summary>

From your checkout, use this instead of the image pull/start commands:

```sh
unset BRAINWIRE_IMAGE
docker compose -p brainwire-demo -f deploy/compose.demo.yaml \
  up --build --wait --wait-timeout 900
```

The first build compiles Rust and RocksDB and can take several minutes.

</details>

The local demo disables authentication and binds the published port to host
loopback. Before sharing access, use the [protected container setup](docs/protected-containers.md)
and [transport guide](docs/protected-transport.md).
Use `brainwire auth init --directory <new-private-directory> --graph default`
to generate a graph-scoped admin and health credential. See [authentication](docs/bolt-auth.md)
for secure file access and restart requirements.

### Write from Python

Install the helper from the same checkout:

```sh
python3 -m venv .venv
.venv/bin/python -m pip install .
```

Run this with `.venv/bin/python`:

```python
from brainwire import GraphClient
from neo4j import GraphDatabase

with GraphDatabase.driver("bolt://127.0.0.1:7687", auth=None) as driver:
    graph = GraphClient(driver, "demo", batch_size=100)
    graph.upsert_node("Customer", "customer-1", {"name": "Ada"})
    graph.upsert_node("Segment", "segment-1", {"name": "beta"})
    graph.upsert_edge(
        ("Customer", "customer-1"), "IN_SEGMENT", ("Segment", "segment-1")
    )
    print(graph.run_cypher("""
        MATCH (c:Customer)-[:IN_SEGMENT]->(s:Segment)
        RETURN c.name AS customer, s.name AS segment
    """))
```

Expected result: `[{'customer': 'Ada', 'segment': 'beta'}]`.
The demo ontology declares these labels and the relationship. For your own
records, define an ontology before writing.

For larger reads, consume records incrementally within the same driver context:

```python
from brainwire import GraphClient
from neo4j import GraphDatabase

with GraphDatabase.driver("bolt://127.0.0.1:7687", auth=None) as driver:
    graph = GraphClient(driver, "demo")
    with graph.stream_cypher("MATCH (c:Customer) RETURN c.name AS name", fetch_size=200) as rows:
        for row in rows:
            print(row["name"])
```

`stream_cypher` uses a read-only session and closes it when the block ends.
Its page size does not limit process RSS, and some server plans still materialize
intermediate results. See [streaming reads](docs/python-client.md).

Each bounded batch commits separately. `None` removes a property; omitted
properties stay unchanged. After a lost commit response, reconcile source keys
before retrying. The helper uses the Neo4j Bolt driver and is installable from
this checkout; a public `pip install brainwire` distribution is not available.
See [Python setup](docs/python-package.md), [mutation semantics](docs/python-client.md)
and [source ingestion](docs/source-ingestion.md).

### See what the TUI can do

Open a second terminal and inspect the graph created above:

```sh
docker compose -p brainwire-demo -f deploy/compose.demo.yaml \
  exec brainwire brainwire shell --connect 127.0.0.1:7687 --graph demo \
  --node-label Customer --entity-id customer-1 \
  --edge-type IN_SEGMENT --target-label Segment
```

| Control | Action |
|---|---|
| `Tab` or `1`–`4` | Switch Ontology, Memory, Indexes and Graph |
| `r` | Refresh the current view |
| `?` | Explain the current pane |
| `q` | Exit |

The connected TUI is read-only. Graph samples up to 20 outgoing relationships
for the selected node and type; Memory shows shared query reservations. Add
`--lang pt-BR` for Portuguese interface text. The [connected TUI guide](docs/connected-status.md)
explains authentication, selection and the available fields.

For a synthetic visual demo, replace the shell arguments with `shell --demo`.
The offline `shell --overview` mode inspects local files and requires the server
to be stopped before opening the same database directory.

### Manage the demo

```sh
# Check service readiness and recent logs.
docker compose -p brainwire-demo -f deploy/compose.demo.yaml ps
docker compose -p brainwire-demo -f deploy/compose.demo.yaml logs --tail 50 brainwire

# Stop the containers and keep the graph files.
docker compose -p brainwire-demo -f deploy/compose.demo.yaml down
```

Adding `--volumes` deletes the demo's data. For a plain `docker run`, versioned
image or custom ontology, see the [Docker guide](docs/container.md).

## Inside this beta

**Bounded results.** Bolt sends eligible reads in pages, with memory reservations
held until the page is sent or discarded. Large eligible label scans stream
entities in parallel. `RUN` can return before execution finishes, so consume or
discard the result and check its final response. Other plans can still retain
node handles or materialize intermediate results; oversized sorts, aggregates
and `DISTINCT` are refused rather than spilled to disk. Read the
[streaming scope](docs/design/bolt-result-streaming.md).

**Memory-pressure errors.** Query and ingress budgets, together with sampled
process/cgroup admission, can reject work under pressure. Python exposes typed
memory and unknown-commit exceptions. These controls are conservative allocation
accounting; they do not guarantee a hard global RSS ceiling. See
[memory defaults](docs/memory-defaults.md) and the [error contract](docs/design/memory-error-contract.md).

**Experimental write outcomes.** A new dedicated volume can opt into authenticated
write-outcome receipts and `CALL db.requestStatus($seed)`. Receipts are scoped
to the graph and principal. `not_recorded` never proves rollback or makes a
retry safe. Existing ordinary graphs are not migrated by enabling the option.
Read the [request-outcome guide](docs/standalone-requests.md) before using this
separate storage mode.

## Choose how to run it

| Option | Start here |
|---|---|
| **Local Docker demo** | [Quickstart](#quickstart) |
| **Published container image** | [Docker setup](docs/container.md) |
| **Rust server, CLI and TUI** | [Build locally](#build-it-locally) |
| **Kubernetes or Swarm** | [Persistent single-instance templates](docs/orchestrators.md) |

Homebrew and an Electron desktop app are planned. Installers are not available.

## Current limits

| Area | Plan around this |
|---|---|
| **Memory** | No guaranteed global RSS ceiling. Sustained 2/4/8 GiB and large initial-load qualification remain open. [Memory guide](docs/memory-defaults.md) |
| **Cypher** | One fixed label and immutable `entity_id` per node. Subqueries, pattern comprehensions and native temporal/spatial types remain unsupported. [Supported syntax](docs/local-cypher.md) |
| **Security** | Shared access requires an auth policy and a verified encryption boundary; native Bolt TLS is not implemented. [Server security](docs/serve.md) |
| **Recovery** | Synthetic restart and process-kill tests cover specific paths. Power loss, full disks and every restore/upgrade scenario are not qualified. The opt-in receipt format has additional volume/downgrade limits. [Recovery guide](docs/checkpoints.md) |
| **History and sources** | Catalog versions are not node/edge history. No native `AS OF` or automatic retention. DuckLake compares pinned snapshots rather than native CDC. [Temporal data](docs/temporal-data.md) · [DuckLake](docs/ducklake-incremental.md) |
| **Distributed operation** | Three-voter quorum is experimental. Ordinary deployment templates run one instance. Sharding remains open. [Quorum operations](docs/quorum-replication.md) |
| **Search and performance** | Native text/vector indexes remain open. Recorded benchmarks are synthetic diagnostics, not production SLOs. [Benchmark conditions and results](BENCHMARKS.md) |

For a first trial, use a rebuildable shadow graph and keep the existing source
as the authority until integration, security, recovery and workload checks pass.
The [qualification report](PRODUCTION-v0.12.md) records completed checks and open
work; the [operator checklist](docs/beta-operator-checklist.md) guides a trial.

## Documentation

**New here?** Follow [Docker](docs/container.md) → [Python](docs/python-client.md)
→ [Cypher](docs/local-cypher.md) → [TUI](docs/connected-status.md).

| I want to… | Read |
|---|---|
| Install and connect | [Docker](docs/container.md) · [Kubernetes / Swarm](docs/orchestrators.md) |
| Create graphs and define types | [CLI quickstart](docs/mvp-quickstart.md) · [ontology](docs/connected-ontology.md) |
| Write, patch, remove and import | [Python API](docs/python-client.md) · [source ingestion](docs/source-ingestion.md) · [DuckLake sync](docs/parallel-ingestion.md) |
| Query and inspect | [Cypher](docs/local-cypher.md) · [indexes](docs/indexes.md) · [TUI](docs/connected-status.md) · [offline inspector](docs/multi-graph-backoffice.md) |
| Protect and recover | [Authentication](docs/bolt-auth.md) · [transport](docs/protected-transport.md) · [checkpoints](docs/checkpoints.md) |
| Evaluate performance | [Benchmark index](BENCHMARKS.md) · [Neo4j/FalkorDB diagnostic](docs/benchmarks/comparison/README.md) |

Browse the [documentation index](docs/README.md) for current guides and historical
reports. The Markdown guides work directly on GitHub; `docs-site/` is a separate
static landing page. The [site guide](docs/github-pages.md) explains previewing it
and GitHub Pages.

## Releases and container images

After the matching release workflow publishes an image and records its digest
in `release-metadata.txt`, pull it with:

```sh
docker pull ghcr.io/allandefraga/brainwire:v0.13.0-beta.2 # brainwire-release-version
```

Private images require `docker login ghcr.io` with a GitHub token that can read
the package. Repository and image visibility are separate settings. Follow the
[container guide](docs/container.md) for startup and authentication.

Pushes to `main` run CI. Version tags run release checks and image recovery tests
before publication; beta/RC tags create prereleases. A tag alone does not prove
that an image exists. See [releasing](docs/releasing.md) and [changelog](CHANGELOG.md).

## Build it locally

Install the Rust toolchain in `rust-toolchain.toml` and native prerequisites:
Clang/libclang, CMake and pkg-config. From the repository root:

```sh
cargo fetch --locked
cargo build --release --locked --offline -p brainwire-server
./target/release/brainwire --version
```

The binary includes the server, CLI and TUI. Run `brainwire --help`,
`brainwire config show` or `brainwire probe --mode ready` to explore it.
Offline administration needs exclusive access to graph files; use Bolt while
the server owns them. See the [CLI guide](docs/mvp-quickstart.md).

To verify a source change:

```sh
cargo test --workspace --locked --offline
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
cargo fmt --all -- --check
```

## Release status and license

**Version:** `v0.13.0-beta.2` · experimental beta · production qualification open. <!-- brainwire-release-version -->

[Apache-2.0](LICENSE). Use, modify and redistribute the current source, including
commercially, while following its terms. Check the license in the tag you
redistribute; some older releases predate this grant. Repository and image
visibility still control access.

<details>
<summary>What does <code>publish = false</code> mean?</summary>

It prevents uploading the Rust crates to registries such as crates.io. You can
still run Brainwire, use Docker images and create GitHub releases. The release
workflow does not publish a Rust crate or Python package to a registry.

</details>
