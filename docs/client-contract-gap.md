# client GraphRepository integration gap

Read-only comparison on October 13 used the sibling client checkout. Platform
code was read without modification; no credentials or live catalog were accessed. Brainwire's synthetic
Ticket/Comment importer and DuckLake bridge are working paths, but they are not
proof of client `GraphRepository` conformance.

The port is `client-core/src/client_core/repository/port.py`, lines 272–413.
`tests/test_graph_repository_contract.py` currently parametrizes only FalkorDB and
Neo4j (line 49; fixture branches 158–218). There is no Brainwire adapter branch.

| Contract area | Concrete compatibility gap | Beta dependency |
| --- | --- | --- |
| Node visibility | `run_read_cypher`/`read_nodes` require fail-closed `AclPredicate` enforcement. Existing adapters inject `any`/`all` predicates on `acl_allow`/`acl_deny` and connector provenance. Brainwire service tokens grant graph-level access; list comprehensions and `STARTS WITH` are unsupported | Do not substitute graph grants for node ACL. Add equivalent supported engine enforcement or separately tested application mediation before governed reads |
| Query surface | Existing reads emit ORDER BY/SKIP and accept broader Cypher; ACL rewriting also introduces unsupported expressions | Freeze actual required queries; reject unsupported operations rather than silently changing pagination or visibility |
| Index provisioning | `ensure_index(label, prop)` emits online CREATE INDEX in current adapters | Provision scalar indexes through ontology while stopped; adapter may check `CALL db.indexes()` readiness, never pretend unsupported DDL executed |
| Schema introspection | Adapters use `db.labels()`, `db.relationshipTypes()`, `keys(n)` and SHOW INDEXES | Map declared labels/signatures/indexes through Brainwire `db.schema()`/`db.indexes()`; property-key sampling requires a supported separate contract |
| Merge identity | Port accepts arbitrary `key_props`; Brainwire node MERGE uses labeled `entity_id` | Explicit stable entity-ID mapping before mutation. Current UNWIND + MERGE/SET importer is the supported subset |
| Per-query timeout | Port defaults reads to 28,500ms and requires engine-side per-request timeout. Brainwire has a configured server-wide 30,000ms cooperative query deadline | Add and test per-request timeout metadata before claiming this contract; IO timeout is independent and excludes blocking engine work |
| Conformance suite | Tests assert node ACL, no ACL-field leaks, dynamic indexes, wipe and relationship introspection | Add an isolated disposable Brainwire target only after capability gating. Never run a destructive contract wipe against an existing graph |

Concrete source references in the sibling checkout:

- `repository/neo4j.py`: ACL rewriting/read API 861–980, merge 984–1001,
  index DDL 1005–1027, introspection 1140–1192, driver query timeout 568–589.
- `repository/falkordb.py`: ACL/read API 919–1059, merge 1063–1088,
  index DDL 1092–1108, introspection 1145–1188.
- `auth/visibility.py`: ACL rendering 312–375.
- `tests/test_graph_repository_contract.py`: governed reads 338–545,
  additional contract assertions 668–721.

## Smallest next integration slice

Implement a separate capability-gated adapter prototype against a disposable named
graph with a pre-applied synthetic ontology. Begin with entity-ID MERGE, bounded
UNWIND/SET batches, simple readback and declared schema/index readiness. Use the
same Python/Bolt server path as the current importer; do not introduce a second
engine. Explicitly reject unsupported governed read/pagination/DDL/timeout methods.
This is an ingestion prototype, not full port conformance or a production adapter.

Before broadening it, obtain the real authoritative mapping, required query fixtures,
visibility predicates and source/backend contract. Per-node ACL is a concrete
integration blocker, alongside actual DuckLake access and protected deployment.
The controlled rebuildable shadow beta and critical production cutover are separate
decisions. These findings belong in the remaining October queue rather than being
hidden by a successful synthetic graph/index audit.
