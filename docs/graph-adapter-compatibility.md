# Graph adapter compatibility gaps

Read-only comparison on October 13 reviewed a sample application's graph
repository contract. No credentials or live catalog were accessed. Brainwire's
synthetic Ticket/Comment importer and DuckLake bridge are optional examples, not
proof of conformance with another application's repository interface.

The contract's methods and expected query shapes are summarized below as an
integration gap assessment; no other application's code or runtime is needed to
build or operate Brainwire.

| Contract area | Concrete compatibility gap | Beta dependency |
| --- | --- | --- |
| Node visibility | `run_read_cypher`/`read_nodes` require fail-closed `AclPredicate` enforcement. Existing adapters inject `any`/`all` predicates on `acl_allow`/`acl_deny` and connector provenance. Brainwire now enforces equivalent private engine ACL predicates through a trusted static `node_visibility` policy; existing adapters still emit unsupported list expressions and `STARTS WITH` | Add a capability-gated adapter and trusted per-request visibility binding; do not substitute graph grants or query parameters for node ACL |
| Query surface | Existing reads emit ORDER BY/SKIP and accept broader Cypher; ACL rewriting also introduces unsupported expressions | Freeze actual required queries; reject unsupported operations rather than silently changing pagination or visibility |
| Index provisioning | `ensure_index(label, prop)` emits online CREATE INDEX in current adapters | Provision scalar indexes through ontology while stopped; adapter may check `CALL db.indexes()` readiness, never pretend unsupported DDL executed |
| Schema introspection | Adapters use `db.labels()`, `db.relationshipTypes()`, `keys(n)` and SHOW INDEXES | Map declared labels/signatures/indexes through Brainwire `db.schema()`/`db.indexes()`; property-key sampling requires a supported separate contract |
| Merge identity | Port accepts arbitrary `key_props`; Brainwire node MERGE uses labeled `entity_id` | Explicit stable entity-ID mapping before mutation. Current UNWIND + MERGE/SET importer is the supported subset |
| Per-query timeout | Port defaults reads to 28,500ms and requires engine-side per-request timeout. Brainwire has a configured server-wide 30,000ms cooperative query deadline | Add and test per-request timeout metadata before claiming this contract; IO timeout is independent and excludes blocking engine work |
| Conformance suite | Tests assert node ACL, no ACL-field leaks, dynamic indexes, wipe and relationship introspection | Add an isolated disposable Brainwire target only after capability gating. Never run a destructive contract wipe against an existing graph |

## Smallest next integration slice

Implement a separate capability-gated adapter prototype against a disposable named
graph with a pre-applied synthetic ontology. Begin with entity-ID MERGE, bounded
UNWIND/SET batches, simple readback and declared schema/index readiness. Use the
same Python/Bolt server path as the current importer; do not introduce a second
engine. Explicitly reject unsupported governed read/pagination/DDL/timeout methods.
This is an ingestion prototype, not full port conformance or a production adapter.

Before broadening it, obtain the real authoritative mapping, required query fixtures,
visibility predicates and source/backend contract. Private engine ACL is implemented and tested, but the dynamic adapter and atomic
authoritative ACL ingestion remain integration blockers alongside actual DuckLake
source access. Engine ACL lists are bounded at 256 entries, below external application
knowledge-grant profiles of up to 5,000; oversized policies fail closed.
Synthetic pinned-snapshot diffs now cover updates/deletes/replay; this is not live
source qualification or native CDC. See [node ACL](node-acl.md),
[incremental ingestion](ducklake-incremental.md) and
[candidate qualification](benchmarks/2026-10-13-beta-acl.md).
The controlled rebuildable shadow beta and critical production cutover are separate
decisions. These findings belong in the remaining October queue rather than being
hidden by a successful synthetic graph/index audit.
