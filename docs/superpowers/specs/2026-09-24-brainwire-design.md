# Original Brainwire direction

This September 2026 specification recorded the original goals. It is historical
design context, not a list of implemented features. Detailed prototype recipes
and external application assumptions were removed from the public guide; the
original text remains in Git history.

## Goals retained

- A standalone Rust graph database with named graphs and durable local storage.
- Cypher over Bolt, explicit transactions and stable record identities.
- An ontology/catalog describing labels, properties and allowed connections.
- Python ingestion with bounded batches, incremental updates and reconciliation.
- Explainable memory settings and a terminal workspace for operators.
- Authentication, graph grants and visibility rules derived by a trusted backend.
- Recovery testing and an experimental path to quorum replication.

These goals do not promise full query-language compatibility, an RSS ceiling,
historical revisions, native search indexes or production distributed failover.

Use [architecture](../../architecture.md), [current documentation](../../README.md)
and the [production roadmap](../../production-readiness-roadmap.md) for current
behavior, qualification gaps and next work.
