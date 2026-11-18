# Brainwire documentation

**Start with the [Brainwire documentation site](https://allandefraga.github.io/brainwire/).**
Use the site to [run your first query](https://allandefraga.github.io/brainwire/#start),
[explore the terminal workspace](https://allandefraga.github.io/brainwire/#terminal),
or [browse the guides](https://allandefraga.github.io/brainwire/#guides).
[Introdução em português](https://allandefraga.github.io/brainwire/pt-br.html).

This directory contains the Markdown sources for the site and historical
engineering records. The source index below helps maintainers find and edit the
guides.

## Start here

| Choose | Use it to | Guide |
| --- | --- | --- |
| Docker | Try a Cypher query against a disposable graph. | [Run one query](../README.md#try-a-connection-with-docker) |
| Homebrew on macOS | Open a local graph and run Cypher with `brainwire`. No server, Docker or Python environment is required. | [Native CLI on macOS](homebrew.md) |
| Docker Compose | Run a Bolt server for applications and connected clients, with data in a persistent volume. | [English](getting-started.md) · [Português (Brasil)](getting-started.pt-BR.md) |
| Python client | Connect a Python application or run the Python examples against a running Bolt server. | [Installation](python-package.md) · [Client guide](python-client.md) |

[Architecture](architecture.md) explains how queries, transactions, memory and
storage fit together.

## Model, query and explore

| I want to… | Guide |
| --- | --- |
| Define node types and relationships | [Ontology and online catalog](online-catalog.md) |
| Enforce scalar types in a new standalone graph | [Typed properties](typed-properties.md) |
| Query relationships | [Supported Cypher and CLI](local-cypher.md) |
| Inspect a running graph | [Connected terminal workspace](connected-status.md) |
| Add an index | [Node indexes](indexes.md) · [Relationship property indexes](relationship-property-indexes.md) |
| Import and update data | [DuckDB table sync](duckdb-sync.md) · [DuckLake table sync](parallel-ingestion.md#ducklake-full-load-then-incremental) · [DuckLake snapshots](ducklake-incremental.md) · [Source ingestion](source-ingestion.md) |
| Validate a typed input batch | [Client-side typed ingestion](../examples/typed_ingest/README.md) |

## Use cases

| Use case | Example |
| --- | --- |
| Knowledge graph | Connect projects to decisions and supporting records in the [company graph](../examples/company_brain/README.md). |
| Event timeline | Model events and query their connections in order. [Temporal data](temporal-data.md) |
| Semantic metrics | Define a named, read-only calculation over a node type. [Semantic metrics example](semantic-metrics.md) |
| Code exploration | Explore links across code symbols and files. [Code graph](code-graph.md) |
| Agent context | Retrieve connected records and their sources for an agent. [Agent quickstart](agent-quickstart.md) · [MCP adapter](agent-mcp.md) · [MCP em português](agent-mcp.pt-BR.md) · [Context and reranking](context-api.md) |

## Operate and evaluate

| I need… | Guide |
| --- | --- |
| Docker commands and volumes | [Containers](container.md) |
| Run a shared server | [Protected deployment](protected-containers.md) |
| Configuration and memory controls | [Server](serve.md) · [Memory budgets](memory-defaults.md) |
| Authentication and permissions | [Bolt authentication](bolt-auth.md) |
| An encrypted connection boundary | [Protected transport](protected-transport.md) |
| Backups and recovery | [Checkpoints](checkpoints.md) · [Object storage backup](object-storage-backup.md) · [Termination and recovery](termination-recovery.md) |
| Transaction and retry behavior | [Transactions](transactions.md) · [Identified request outcomes](standalone-requests.md) |
| Kubernetes or Swarm | [Orchestrator templates](orchestrators.md) |
| Experimental replication | [Three-voter quorum](quorum-replication.md) |
| Performance evidence | [Benchmark methods and results](../BENCHMARKS.md) |
| Explore the managed pilot plan | [Scope and readiness checks](managed-pilot.md) |

## Evaluate the beta

Read the [operator checklist](beta-operator-checklist.md) before using real
data. Memory admission is cooperative, Bolt TLS requires opt-in configuration,
and distributed production failover remains unqualified. The standard Docker,
Kubernetes and Swarm templates run one instance. The replication guide is a
separate experimental setup.

[Current limits](../README.md#current-limits) summarize the boundaries of the
current release. Benchmark reports apply to their recorded binary and workload.

## Contribute

- [Build and preview GitHub Pages](github-pages.md).
- [Apache 2.0 license](../LICENSE).

## Plans and past results

- [Production-readiness work](production-readiness-roadmap.md).
- [Longer-term roadmap](../ROADMAP-v2.md).
- [Archived October 2026 beta plan](2026-10-pilot-roadmap.md).

`design/` and `superpowers/` hold engineering proposals and decisions. Dated
benchmark reports describe their recorded workloads, settings, and builds.
