# Brainwire

An independent graph database developed alongside client. The name comes
from the brain's wiring: neurons and the connections between them.

This is a standalone Rust repository at `/Users/fraga/Documents/brainwire`,
beside `client-platform`. Its crates build without the platform checkout.
client integration uses Bolt/Cypher and the planned Python SDK and repository
adapter; the platform remains the first integration target.

> **Status: local MVP development.** The Rust workspace contains the RocksDB
> storage foundation, graph operations, the Bolt protocol server, configuration
> commands, a Cypher subset parser and atomic executor, and an interactive
> terminal backoffice demo. Real local queries are available through
> `brainwire cypher`; the durable query server is still under development. The design is in
> [the approved spec](docs/superpowers/specs/2026-09-24-brainwire-design.md),
> the implementation tasks in
> [the MVP plan](docs/superpowers/plans/2026-09-26-brainwire-mvp.md), and session
> progress in [JOURNAL.md](JOURNAL.md).

The terminal backoffice design is documented in
[Brainwire Observatory](docs/design/2026-09-26-brainwire-observatory.md), with
graph/query workflows, explained memory settings, disk inspection and concept
images. Allan selected the latest electric-green identity on 2026-09-26:
near-black background, restrained cyan and the connected B/W logo. That visual
identity is used by the first interactive TUI: Graph, Query, Memory and
Disk & Files. Run `brainwire shell --demo` to explore its synthetic scenario.
English is the default; use `--lang pt-BR` for Portuguese. Internal graph/schema
names remain English in either interface language.
It includes a clustered graph, guided query examples, explained memory budgets
and a file inspector with an illustrative LSM storage flow. See the
[terminal demo guide](docs/tui-demo.md) for controls and actual captures.

For real graph reads and writes, use the separate embedded
`brainwire cypher --database <directory> --query <text>` command. Queries commit
atomically to RocksDB and survive reopening. The [local query guide](docs/local-cypher.md)
walks through ontology initialization, parameters, bulk JSON files and JSON output. Connecting
the TUI to the engine, real telemetry and real files remains pending.

## Local development

```sh
cargo test --workspace --locked --offline
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
cargo fmt --all -- --check
cargo run -p brainwire-server -- config show
cargo run --locked --offline -p brainwire-server -- shell --demo
```

Offline commands require dependencies to have been fetched previously.
The `brainwire` binary provides configuration commands, local Cypher execution
and the terminal demo. The `brainwire-bolt-dev` binary uses a stub query engine.
The [shared-query transaction API and Bolt lifecycle hooks](docs/transactions.md)
provide commit/rollback foundations. The next MVP work is the production
worker/adapter, database registry and durable `brainwire serve` integration.

The local history was extracted from the platform's Brainwire feature branch.
Old journal entries retain their original commit IDs and checkout paths; the
rewritten history in this repository contains only Brainwire. No remote is
configured and no code has been pushed.

## Requirements (2026-09-24)

| Topic | Decision |
|---|---|
| Motivation | RAM and scale ceilings, a license that works for BYOC, native ACL and events, R&D. Several graphs/databases per instance. |
| Scale | A graph lives entirely on one node. The ID and file formats must not prevent partitioning later. Runs in a container. |
| Volume | 5 billion elements per graph with 16 GB of RAM. Most of it lives on disk, compact and easy to recover. |
| Query | Bolt protocol + Cypher, starting from the measured subset (code + `run_cypher` logs), each construct validated against the openCypher TCK. |
| Events | Entity timeline (today `Event` + `HAD_EVENT`): append-only, read by entity and time range. |
| Team | Allan + Claude agents, 45 minutes of work per day. |
| Deadline | Launch on 2026-10-31. Work like a Neo4j Enterprise, with cluster/HA, a simple RBAC and full openCypher (plan, section 1). |
| ACL | The client model (`AclPredicate`, ADR-016/017), evaluated inside the engine (plan, section 5). |
| Ontology | The platform's `graph_mapping.yaml` becomes the engine's schema. |
| Python | SDK with bulk, merge, index creation and delete, in the style of the port. |
| Surfaces | Animated CLI/TUI (graphs, queries, charts, disk, ontology, admin) and a website at the end, in the spirit of the HydraDB site. |
| Intelligence | Graph algorithms (depth, Dijkstra, proximity, similarity, centrality, community) and AI questions about a node ("quais são os concorrentes?", "who are the competitors?"). |
| Acceptance | The `GraphRepository` port (ADR-023) and the `client-core/tests/test_graph_repository_contract.py` suite. |

## Consequences already measured

- 16 GB for 5 billion elements is about 3 bytes of RAM per element. RAM is a
  cache; the on-disk format is the product.
- "Millisecond answers" hold for the hot set or for local NVMe. A cold
  expansion that touches 93 thousand nodes (the hub in doc 27) does about 93
  thousand random reads: about 30 s on an EBS gp3 volume at its 3,000 IOPS
  baseline.
- Compactness yardstick: Neo4j used 1.4 GB for 20M elements in doc 08 (70
  bytes per element, lean shape). At 5 billion that would be about 350 GB.
- On the worker's real path, the ingestion ceiling (about 10 thousand
  elements/s) comes from entity resolution, not from the graph (doc 08, §3).

## Open

- Language decided on 2026-09-24: Rust on top of RocksDB (plan, section 2).
- Transaction, index, physical deletion (LGPD) and multi-database details come
  out in the spec.
- Versioning and the release flow: parked for later.
