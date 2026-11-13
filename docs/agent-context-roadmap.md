# Agent context: current capabilities and next steps

Brainwire can store related records and retrieve bounded context for an agent.
The [company context example](../examples/company_brain/README.md) demonstrates
this today. It is an experimental starting point, not a complete agent platform.

| Capability | Available now | Next engineering work |
| --- | --- | --- |
| Ontology | Declared node and relationship types, properties and keys | Stronger property types, relationship cardinality and migration checks |
| Retrieval | Explicit IDs, visible graph paths, caller-provided ranked candidates and RRF | Native text/vector indexes, graph-filtered retrieval in one execution plan and recall testing |
| Evidence | Selected properties, reported source fields and bounded paths | Verify source references and support consistent evidence snapshots |
| Agent changes | Authenticated writes, grants and per-node visibility | Isolated graph branches, reviewable diffs, conflict handling and audited merge |
| Storage | RocksDB on a persistent volume, WAL, checkpoints and an experimental quorum path | Qualify multi-machine recovery; assess object-storage designs and their consistency, latency and cost |
| Operation | Docker, single-instance Kubernetes/Swarm templates and a terminal inspector | Tested distributed orchestration and a clearer live graph workspace |

A graph branch is not the same thing as a catalog version, backup or transaction
snapshot. Historical node/edge versions and `AS OF` reads are not implemented.
Likewise, provider fusion is not a native BM25 or vector index. Context is
untrusted data: application identity and authorization must come from the
backend, never from a model. Separate helper calls do not share a snapshot.

Start with a rebuildable dataset and a tested backup. See [context API](context-api.md),
[deployment](protected-containers.md), [memory](memory-defaults.md) and
[replication](quorum-replication.md) for the current guarantees and limits.
