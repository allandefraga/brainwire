# Brainwire documentation

Brainwire is a Rust graph database with durable RocksDB storage, Cypher over
Bolt, a Python helper and a terminal workspace. It is an experimental beta.
Start with a local graph you can rebuild, then test the queries, permissions
and recovery steps your application needs.

## Start here

1. Run a first graph with Docker, Python and the terminal workspace.
   [English](getting-started.md) · [Português (Brasil)](getting-started.pt-BR.md)
2. Learn how queries, transactions, memory and storage fit together.
   [Architecture](architecture.md)
3. Set up credentials, volumes and encrypted connections.
   [Protected deployment](protected-containers.md)

O guia de primeiros passos está em português. Os demais guias técnicos deste
índice estão em inglês.

## Build an application

| I want to… | Guide |
| --- | --- |
| Connect from Python | [Install the helper](python-package.md) |
| Create, update or delete data | [Python operations and batches](python-client.md) |
| Write a query | [Supported Cypher and CLI](local-cypher.md) |
| Define allowed types and connections | [Ontology and online catalog](online-catalog.md) |
| Enforce scalar types in a new standalone graph | [Typed properties](typed-properties.md) |
| Validate a typed input batch | [Client-side typed ingestion](../examples/typed_ingest/README.md) |
| Define a named metric | [Semantic metrics example](semantic-metrics.md) |
| Add an index | [Node indexes](indexes.md) · [Relationship property indexes](relationship-property-indexes.md) |
| Retrieve context for an agent | [Agent quickstart](agent-quickstart.md) · [Context and reranking](context-api.md) |
| Keep explicit events and timelines | [Temporal data](temporal-data.md) |
| Import a source | [DuckDB table sync](duckdb-sync.md) · [DuckLake table sync](parallel-ingestion.md#ducklake-full-load-then-incremental) · [DuckLake snapshots](ducklake-incremental.md) · [Source ingestion](source-ingestion.md) |
| Explore source code as a graph | [Code graph](code-graph.md) |
| Inspect a running graph | [Connected terminal workspace](connected-status.md) |

## Operate and evaluate

| I need… | Guide |
| --- | --- |
| A native CLI on macOS | [Homebrew source formula](homebrew.md) |
| Docker commands and volumes | [Containers](container.md) |
| Configuration and memory controls | [Server](serve.md) · [Memory budgets](memory-defaults.md) |
| Authentication and permissions | [Bolt authentication](bolt-auth.md) |
| An encrypted connection boundary | [Protected transport](protected-transport.md) |
| Backups and recovery | [Checkpoints](checkpoints.md) · [Object storage backup](object-storage-backup.md) · [Termination and recovery](termination-recovery.md) |
| Transaction and retry behavior | [Transactions](transactions.md) · [Identified request outcomes](standalone-requests.md) |
| Kubernetes or Swarm | [Orchestrator templates](orchestrators.md) |
| Experimental replication | [Three-voter quorum](quorum-replication.md) |
| Performance evidence | [Benchmark methods and results](../BENCHMARKS.md) |

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

## Plans and past results

- [Production-readiness work](production-readiness-roadmap.md).
- [Longer-term roadmap](../ROADMAP-v2.md).
- [Archived October 2026 beta plan](2026-10-pilot-roadmap.md).

The guides above describe current behavior. Files in `design/`, `superpowers/`,
dated plans and benchmark receipts record proposals, decisions and past results.
