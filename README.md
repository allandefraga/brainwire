# Brainwire

**Context has connections. Give your AI apps a graph to work with.**

Brainwire is an independent, local-first graph database for builders who need
structured relationships and durable state behind their applications. It is
written in Rust, stores data in RocksDB, speaks Bolt, and supports a growing
subset of Cypher. Feed it from Python in bounded batches; run it in Docker with
data on a persistent volume.

Think of Brainwire as graph infrastructure for context-rich apps: your code
decides what to ingest, the graph keeps the entities and relationships
connected, and Cypher lets your app retrieve them. **Brainwire 0.1 is the graph
database layer, not an agent runtime or an out-of-the-box RAG pipeline.**

> **0.1.0 · local beta candidate.** This is an actively changing pre-release,
> not a published or production-qualified build. The current checkout contains
> uncommitted work. Check the honest beta checklist below before using important
> data.

## ⚡ Spin it up

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
- A Python convenience client with bounded node cleanup, plus JSONL and
  source-snapshot import examples.
- Graph operations including PageRank, shortest path, reachability, WCC and SCC.
- A terminal backoffice for graph, query, memory-budget, ontology and disk
  inspection.
- Local Docker/Compose packaging with retained-volume restart and health checks.

For the Python helper's batch API, property semantics, and supported inputs, see
[Python client and ingestion](docs/python-client.md). The helper currently lives
in this repository; it is **not** a separately published Python package.

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
| Create graphs and apply an ontology | [Quickstart](docs/mvp-quickstart.md) · [ontology](docs/connected-ontology.md) |
| Inspect the terminal UI | [TUI demo and controls](docs/tui-demo.md) · [backoffice](docs/multi-graph-backoffice.md) |
| Configure authentication and network protection | [Auth](docs/bolt-auth.md) · [container auth](docs/protected-containers.md) · [TLS boundary](docs/protected-transport.md) |
| Review measurements, recovery and remaining gates | [Beta roadmap](docs/2026-10-pilot-roadmap.md) · [operator checklist](docs/beta-operator-checklist.md) |
| See design decisions | [Design documents](docs/design/) · [session journal](JOURNAL.md) |

## Build it locally

Install Rust stable as selected by `rust-toolchain.toml`. The Docker builder
uses Rust 1.98.1. Then run:

```sh
cargo test --workspace --locked --offline
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
cargo fmt --all -- --check
```

Build the server binary with `cargo build --release --locked --offline -p brainwire-server`.
The [local demo](examples/python/local_demo.py) exercises Python batches,
Cypher, indexes, restart and the connected TUI with synthetic data.

## Release status and license

The Rust workspace currently declares version `0.1.0`, `publish = false`, and
`UNLICENSED`. No public release or registry image is implied. Choose and review
a license and freeze a tested source tree before publishing this database or an
image. For local distribution, use a reviewed Git revision and build the image
from its Dockerfile; see [application delivery](docs/container.md#application-delivery).
