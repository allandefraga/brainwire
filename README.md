<p align="center">
  <img src="assets/brainwire-hero.png" alt="An illustration of connected data" width="100%">
</p>
<p align="center"><sub>Concept artwork.</sub></p>

<p align="center">
  <img src="assets/brainwire-logo.svg" alt="Brainwire graph database" width="300">
</p>

<h1 align="center">Give your data connections.</h1>

<p align="center">
  A graph database for the people, documents, decisions and events behind your applications.
  <br>Rust · RocksDB · Cypher · Bolt · Python
</p>

<p align="center">
  <a href="#quickstart">Run it</a> &nbsp; / &nbsp;
  <a href="#write-from-python">Python</a> &nbsp; / &nbsp;
  <a href="#see-what-the-tui-can-do">Terminal UI</a> &nbsp; / &nbsp;
  <a href="#documentation">Documentation</a> &nbsp; / &nbsp;
  <a href="BENCHMARKS.md">Benchmarks</a>
</p>

<p align="center">
  <a href="https://github.com/allandefraga/brainwire/actions/workflows/ci.yml"><img src="https://github.com/allandefraga/brainwire/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-65ff54?style=flat-square" alt="License: Apache-2.0"></a>
  <a href="#current-limits"><img src="https://img.shields.io/badge/status-beta-65ff54?style=flat-square" alt="Status: beta"></a>
</p>

Brainwire stores records and their relationships in a durable graph. Write from
Python, query with Cypher, and inspect a running server from the terminal.
Each named graph has its own data and ontology. Your application controls
source ingestion, identity mapping and orchestration.

> **v0.12.0-beta.1 · experimental beta.** <!-- brainwire-release-version --> Use for controlled trials.
> This beta has no guaranteed process-wide memory ceiling. Sustained 2/4/8 GiB
> qualification remains open. Review the [current limits](#current-limits),
> [v0.12 qualification report](PRODUCTION-v0.12.md) and
> [operator checklist](docs/beta-operator-checklist.md) before using real data.

The capabilities below describe this checkout. For a release image, use the
documentation from its matching tag; local changes may not be included.

## Choose how to run it

| Option | What it provides | Guide |
|---|---|---|
| **Docker Compose** | Builds and starts a persistent local demo | [Quickstart](#quickstart) |
| **Versioned Docker image** | Runs a published release without compiling | [Container guide](docs/container.md) |
| **Rust source** | Builds the server, CLI and TUI | [Build locally](#build-it-locally) |
| **Kubernetes / Swarm** | Single-instance templates with persistent storage | [Deployment guide](docs/orchestrators.md) |

Homebrew and an Electron desktop app are planned; installers are not available.
The Python helper is installable from this checkout, but is not published on PyPI.

## Quickstart

Install Docker Engine and Docker Compose, then run this from the repository root:

```sh
docker compose -p brainwire-demo -f deploy/compose.demo.yaml \
  up --build --wait --wait-timeout 900
```

This creates the synthetic `demo` graph, stores it in a named volume, and starts
Bolt at `bolt://127.0.0.1:7687`. The first build compiles Rust and RocksDB and
can take several minutes. Set `BRAINWIRE_BIND_PORT=17687` if port 7687 is busy,
then use that port in clients.

The demo disables authentication and publishes only on host loopback. Use it
with synthetic data. For shared environments, follow the
[protected container setup](docs/protected-containers.md) and
[transport guide](docs/protected-transport.md).

Useful Docker commands:

```sh
# Check the service and its startup logs.
docker compose -p brainwire-demo -f deploy/compose.demo.yaml ps
docker compose -p brainwire-demo -f deploy/compose.demo.yaml logs --tail 50 brainwire

# Stop the demo and keep its data.
docker compose -p brainwire-demo -f deploy/compose.demo.yaml down
```

Do not add `--volumes` unless you intend to delete the demo database. For a
plain `docker run`, custom ontology, another container or a versioned image,
use the [Docker guide](docs/container.md).

## Write from Python

With the demo running, install the helper from this checkout:

```sh
python3 -m venv .venv
.venv/bin/python -m pip install .
```

The package uses the Neo4j Bolt driver and does not install the database server.
Run the following with `.venv/bin/python`:

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

The demo ontology declares these labels and relationship. For your own data,
define an ontology before writing. Batch helpers commit each bounded batch
separately. `None` removes a property; omitted properties stay unchanged.
After a lost commit response, reconcile the source keys before retrying.

See [Python installation](docs/python-package.md),
[mutation semantics](docs/python-client.md), and
[source ingestion](docs/source-ingestion.md) for authentication, lists,
generators, Arrow inputs and resumable loads.

## See what the TUI can do

The TUI is the `brainwire shell` command. After [building locally](#build-it-locally),
connect it to the demo populated by the Python example:

```sh
./target/release/brainwire shell --connect 127.0.0.1:7687 --graph demo \
  --node-label Customer --entity-id customer-1 \
  --edge-type IN_SEGMENT --target-label Segment
```

Use `Tab` or `1–4` to switch between **Ontology**, **Memory**, **Indexes** and
**Graph**. Press `r` to refresh, `?` for help, and `q` to exit. Add
`--lang pt-BR` for Portuguese interface text.

The connected workspace is read-only. Graph shows a sample of up to 20 outgoing
relationships for the chosen node and type. Memory shows shared query
reservations, not process RSS. See the [connected TUI guide](docs/connected-status.md)
for authentication and the exact controls.

Other modes:

- `brainwire shell --demo` opens a synthetic visual demo.
- `brainwire shell --overview` inspects local files. Stop the server first;
  RocksDB permits one owner of each database directory.

## Use the command line

The same binary starts the server, administers local graphs and runs offline
Cypher. Start with:

```sh
./target/release/brainwire --help
./target/release/brainwire config show
./target/release/brainwire probe --mode ready
```

`probe` checks the default Bolt address. `config show` explains effective values
and where they came from. To create a graph or query its files directly, follow
[the local CLI quickstart](docs/mvp-quickstart.md) and
[Cypher guide](docs/local-cypher.md). Offline commands need exclusive access;
use Python over Bolt while the server owns the graph.

## What works today

| Area | Available in this checkout |
|---|---|
| Graphs and writes | Independent named graphs, ontology catalogs, atomic local transactions, node/edge creation and deletion, property patches and bounded Python upserts |
| Queries and indexes | Multi-hop patterns, `WITH`, `OPTIONAL MATCH`, `UNION`, aggregates, scalar equality/`IN`, string range/prefix access and online index builds |
| Storage | Local RocksDB, synchronized WAL commits, consistency checks, checkpoints and retained-volume restart |
| Analytics | Sparse CSR projection, degree, weighted PageRank, shortest path, reachability, WCC/SCC and explainable `knowledge.rank` |
| Ingestion and inspection | Python/Arrow helpers, resumable JSONL import, pinned DuckLake snapshot comparison, CLI and TUI |

The experimental beta adds paged Bolt results, parallel streaming for eligible
large label scans, shared query/ingress accounting and memory-pressure errors.
A successful `RUN` can return before execution finishes: consume or discard its
result and check the final response. Other plans can still retain node handles
or materialize blocking results; oversized sorts, aggregates and `DISTINCT`
are refused rather than spilled to disk. See the
[streaming scope](docs/design/bolt-result-streaming.md) and
[memory error contract](docs/design/memory-error-contract.md).

For a new dedicated volume, an authenticated opt-in mode can record the outcome
of an explicit write transaction and expose `CALL db.requestStatus($seed)`.
The record is scoped to the graph and principal. `not_recorded` does not prove
rollback or make retry safe; ordinary graphs are not migrated by enabling it.
Read the [experimental request-outcome guide](docs/standalone-requests.md)
before using this separate storage mode.

The local Cypher implementation includes a bounded Rust-regex subset of `=~`;
see [its syntax and memory limits](docs/cypher-regex.md). That subset is not
full Java regex compatibility. Check the [Cypher guide](docs/local-cypher.md)
before porting a query.

## Current limits

| Area | What to plan around |
|---|---|
| **Memory** | Query and ingress budgets estimate allocations; sampled process/cgroup admission can refuse new work under pressure. These controls do not enforce a global RSS ceiling, and the sustained 2/4/8 GiB qualification is incomplete. Read [memory defaults](docs/memory-defaults.md) and the [production roadmap](docs/production-readiness-roadmap.md). |
| **Cypher** | One fixed label per node and immutable `entity_id`. Subqueries, pattern comprehensions and native temporal/spatial types remain unsupported. See [supported Cypher](docs/local-cypher.md). |
| **Security** | The demo is unauthenticated and published on host loopback. Shared access needs an auth policy and verified encryption boundary; native Bolt TLS is not implemented. See [server security](docs/serve.md). |
| **Recovery** | Synthetic restart and process-kill tests cover specific paths; they do not qualify power loss, full disks or every restore/upgrade scenario. Experimental request outcomes have additional volume and downgrade limits. See [request outcomes](docs/standalone-requests.md) and [checkpoints](docs/checkpoints.md). |
| **Time and sources** | Catalog versions are not node/edge history. No native `AS OF` or automatic retention. DuckLake support compares pinned snapshots rather than native CDC. See [temporal data](docs/temporal-data.md) and [DuckLake ingestion](docs/ducklake-incremental.md). |
| **Scale and ecosystem** | Three-voter quorum is experimental; ordinary deployment templates run one instance. Sharding, native text/vector indexes, a public Python package and an independent sync/async Bolt driver remain open. See [quorum operations](docs/quorum-replication.md) and the [v2 roadmap](ROADMAP-v2.md). |
| **Performance** | Large ACL scans and cursor workloads need application-specific tests. Recorded benchmarks are synthetic diagnostics, not production SLOs. Start with [BENCHMARKS.md](BENCHMARKS.md). |

For a first trial, use a rebuildable shadow graph and keep the existing source
as the authority until integration, security, recovery and workload checks pass.

## Documentation

| Task | Guide |
|---|---|
| Install, configure, restart and connect containers | [Docker](docs/container.md) · [Kubernetes / Swarm](docs/orchestrators.md) |
| Create graphs and apply an ontology | [Local quickstart](docs/mvp-quickstart.md) · [ontology](docs/connected-ontology.md) |
| Write, patch, remove and import data | [Python API](docs/python-client.md) · [source ingestion](docs/source-ingestion.md) · [DuckLake sync](docs/parallel-ingestion.md) |
| Query and inspect | [Cypher](docs/local-cypher.md) · [indexes](docs/indexes.md) · [connected TUI](docs/connected-status.md) · [offline inspector](docs/multi-graph-backoffice.md) |
| Protect and recover data | [Authentication](docs/bolt-auth.md) · [transport](docs/protected-transport.md) · [checkpoints](docs/checkpoints.md) · [operator checklist](docs/beta-operator-checklist.md) |
| Evaluate measured results | [Benchmark index](BENCHMARKS.md) · [Neo4j/FalkorDB diagnostic](docs/benchmarks/comparison/README.md) |

The Markdown guides in `docs/` are readable on GitHub. `docs-site/` is a separate
static documentation landing page. Learn how to preview it and what GitHub Pages
does in the [site guide](docs/github-pages.md). The [documentation index](docs/README.md)
separates current guides from historical reports.

## Releases and container images

Use the image only after the matching release workflow publishes it and records
a tested digest in `release-metadata.txt`:

```sh
docker pull ghcr.io/allandefraga/brainwire:v0.12.0-beta.1 # brainwire-release-version
```

Private images require `docker login ghcr.io` with a GitHub token that can read
the package. Repository and image visibility are separate settings. See the
[container guide](docs/container.md) for startup and authentication.

Pushes to `main` run CI. Version tags run release checks, build the image and
verify writes and restart recovery before publication. Beta/RC tags create
prereleases. A tag alone does not prove that an image exists, and a published
image does not qualify production. See [releasing](docs/releasing.md) for the
local helper, versioning and workflow details.

## Build it locally

Install the Rust toolchain in `rust-toolchain.toml` and native build dependencies
(Clang/libclang, CMake and pkg-config). From the repository root:

```sh
cargo fetch --locked
cargo build --release --locked --offline -p brainwire-server
./target/release/brainwire --version
```

The resulting binary includes the server, CLI and TUI. To verify a source change:

```sh
cargo test --workspace --locked --offline
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
cargo fmt --all -- --check
```

## Release status and license

**Version:** `v0.12.0-beta.1` · experimental beta · production qualification open. <!-- brainwire-release-version -->

Current source uses [Apache-2.0](LICENSE). You may use, modify and redistribute it,
including commercially, while following its terms. Check the license in the tag
you distribute: some older releases predate this license grant. The license does
not make a private repository or image publicly accessible.

`publish = false` prevents uploading the Rust crates to registries such as
crates.io. It does not prevent running the source, using Docker images or
creating GitHub releases. The current release workflow does not publish a Rust
crate or Python package to a registry.
