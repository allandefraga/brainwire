# Brainwire documentation

Brainwire is a Rust graph database with durable RocksDB storage, Cypher over
Bolt, a Python helper and a terminal workspace. It is an experimental beta.
Start with a local graph you can rebuild, then test the queries, permissions
and recovery steps your application needs.

## Start here

1. [Run your first graph](getting-started.md): start Docker, write from Python,
   query the graph and open the terminal workspace.
2. [Understand the architecture](architecture.md): how clients, queries,
   transactions, memory and persistent files fit together.
3. [Choose a deployment](protected-containers.md): credentials, volumes and
   protected access for an application trial.

## Build an application

| I want to… | Guide |
| --- | --- |
| Connect from Python | [Install the helper](python-package.md) |
| Create, update or delete data | [Python operations and batches](python-client.md) |
| Write a query | [Supported Cypher and CLI](local-cypher.md) |
| Define allowed types and connections | [Ontology and online catalog](online-catalog.md) |
| Add an index | [Node indexes](indexes.md) · [Relationship property indexes](relationship-property-indexes.md) |
| Retrieve context for an agent | [Agent quickstart](agent-quickstart.md) · [Context and reranking](context-api.md) |
| Keep explicit events and timelines | [Temporal data](temporal-data.md) |
| Import a source | [DuckDB table sync](duckdb-sync.md) · [DuckLake table sync](parallel-ingestion.md#ducklake-full-load-then-incremental) · [DuckLake snapshots](ducklake-incremental.md) · [Source ingestion](source-ingestion.md) |
| Explore source code as a graph | [Code graph](code-graph.md) |
| Inspect a running graph | [Connected terminal workspace](connected-status.md) |

## Operate and evaluate

| I need… | Guide |
| --- | --- |
| Docker commands and volumes | [Containers](container.md) |
| Configuration and memory controls | [Server](serve.md) · [Memory budgets](memory-defaults.md) |
| Authentication and permissions | [Bolt authentication](bolt-auth.md) |
| An encrypted connection boundary | [Protected transport](protected-transport.md) |
| Backups and recovery | [Checkpoints](checkpoints.md) · [Termination and recovery](termination-recovery.md) |
| Transaction and retry behavior | [Transactions](transactions.md) · [Identified request outcomes](standalone-requests.md) |
| Kubernetes or Swarm | [Orchestrator templates](orchestrators.md) |
| Experimental replication | [Three-voter quorum](quorum-replication.md) |
| Performance evidence | [Benchmark methods and results](../BENCHMARKS.md) |

## Evaluate the beta

Read the [operator checklist](beta-operator-checklist.md) before using real
data. Memory admission is cooperative, native Bolt TLS is not implemented,
and distributed production failover remains unqualified. The standard Docker,
Kubernetes and Swarm templates run one instance. The replication guide is a
separate experimental setup.

[Current limits](../README.md#current-limits) summarize the boundaries of the
current release. Benchmark reports apply to their recorded binary and workload.

## Contribute and maintain

- [Build and preview GitHub Pages](github-pages.md).
- [Production-readiness work](production-readiness-roadmap.md).
- [Longer-term roadmap](../ROADMAP-v2.md).

Files in `design/`, `superpowers/`, dated plans and benchmark receipts preserve
decisions or historical evidence. They can describe an earlier version or an
unimplemented proposal; use the guides above for current behavior.
