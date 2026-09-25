# client-brainwire

client's own graph engine. The name comes from the brain's wiring: neurons
and the connections between them, which is literally a graph.

> **Status: brainstorming.** There is no code yet. The design becomes a spec
> before the first implementation commit. The plan is in [PLAN.md](PLAN.md) and
> the progress of each session in [JOURNAL.md](JOURNAL.md).

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
