# Brainwire documentation

Start with a local, synthetic graph. Brainwire is a source beta; check the
[limits and operator checklist](beta-operator-checklist.md) before using real data.

## Start and connect

- [Run with Docker](container.md): build or use a versioned image, connect,
  restart and keep the volume.
- [Install the Python helper](python-package.md): install from this checkout and
  connect using the Neo4j Bolt driver.
- [Write and import data](python-client.md): nodes, edges, properties and bounded
  batches; [source ingestion](source-ingestion.md) covers source mappings.
- [Use the terminal workspace](connected-status.md): inspect a running server;
  [TUI modes](tui-demo.md) also explain the synthetic demo and offline inspector.
- [Use the CLI and Cypher](local-cypher.md): run queries and find the supported
  language subset. [Indexes](indexes.md) describes readiness and query paths.

## Operate and evaluate

- [Server configuration](serve.md) and [memory budgets](memory-defaults.md).
- [Authentication](bolt-auth.md), [protected containers](protected-containers.md)
  and [encrypted transport](protected-transport.md).
- [Checkpoints and recovery](checkpoints.md), [transactions](transactions.md)
  and [shutdown](termination-recovery.md).
- [Kubernetes and Swarm](orchestrators.md), plus the separate experimental
  [quorum guide](quorum-replication.md).
- [Benchmarks and reproducibility](../BENCHMARKS.md); older reports describe
  their own versions and do not establish current performance.
- [GitHub Pages and local preview](github-pages.md).

## Design and history

[Production readiness](production-readiness-roadmap.md) records open gates;
[ROADMAP-v2](../ROADMAP-v2.md) tracks the longer-term goals. Files under
`design/`, `superpowers/` and dated benchmark reports contain proposals or
historical evidence. They may describe older versions and are not a substitute
for the current guides above.
