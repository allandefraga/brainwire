# Workload-specific differentiation targets

Brainwire is a local MVP; superiority over FalkorDB or Neo4j is not established.
Choose one client workload and measure complete ingestion, index/projection
build, queries, memory, storage and recovery on frozen comparable resources.
Record competitor versions/editions, supported syntax and enabled indexes.

| Hypothesis | Brainwire target | Evidence required |
| --- | --- | --- |
| Lake-to-graph operation | DuckLake-first Python ingestion, explicit versions, replay-safe progress and reconciliation | Identical insert/update/delete batches, lost acknowledgements, schema/source changes, rows/s and peak RAM |
| Governed analytics | client row/connector/document visibility applied before traversal, components and ranking | Shared authorization corpus, no hidden topology/score/count leaks, policy overhead and failure behavior |
| Ticket history | Durable node/edge revisions, as-of questions and scoped retention | Exact ticket transitions, late events, historical neighborhood/weights and restartable cleanup |
| Operator experience | Real terminal schema, memory, files, indexes, ingestion and recovery explanations | Time/error rate for real inspect/restart/restore tasks; no simulated panels counted as evidence |
| Sparse disk-backed workload | RocksDB canonical graph and measured ephemeral CSR analytics | Hot/cold/skewed-degree latency, projection+algorithm total cost, bytes/entity and crash recovery |

These are differentiation hypotheses, not claims that competitors lack the
underlying features. FalkorDB already uses sparse graph representations and
offers indexing/search/graph tooling; see its [official documentation](https://docs.falkordb.com/).
Neo4j provides [fine-grained authorization](https://neo4j.com/docs/operations-manual/current/authentication-authorization/property-based-access-control/),
[CDC](https://neo4j.com/docs/cdc/current/),
[search indexes](https://neo4j.com/docs/cypher-manual/current/indexes/search-indexes/),
[GDS algorithms](https://neo4j.com/docs/graph-data-science/current/algorithms/) and
[clustering](https://neo4j.com/docs/operations-manual/current/clustering/).
DuckDB's [Arrow ingestion support](https://duckdb.org/docs/current/clients/python/data_ingestion)
is a useful interoperability path; converting a batch to Python rows is not
zero-copy transport to Brainwire.

Current delivery priorities are governed data, replayable lake ingestion,
recovery and a small Python mutation API. History, native incremental change
capture, vector/text indexes, dynamic user delegation and real quorum replication
remain separate engineering work. Compare equivalent queries/data/permissions;
never claim a large speedup from different return sizes or from omitting index,
projection, ingestion, network or failure costs.
