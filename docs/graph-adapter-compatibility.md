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
| Node visibility | `run_read_cypher`/`read_nodes` require fail-closed `AclPredicate` enforcement. Existing adapters inject `any`/`all` predicates on `acl_allow`/`acl_deny` and connector provenance. Brainwire now enforces equivalent private engine ACL predicates through a trusted static `node_visibility` policy; the predicate shapes an adapter may push down (`any`, `all`, `coalesce`, `size`, `STARTS WITH`) all parse and run, but the engine's own visibility policy is the recommended route because it cannot be bypassed by a query rewrite | Add a capability-gated adapter and trusted per-request visibility binding; do not substitute graph grants or query parameters for node ACL |
| Query surface | `ORDER BY`/`SKIP`/`LIMIT`, `WITH`, `OPTIONAL MATCH`, multi-hop and variable-length patterns, untyped and undirected relationships, unlabeled `MATCH`, `labels()`/`keys()`, and relationship and path values are supported (see [Cypher](local-cypher.md#supported-cypher)). Remaining gaps: `CALL {}` subqueries, `EXISTS {}` syntax, pattern comprehensions, full Java regex compatibility, `FOREACH`, `LOAD CSV`, temporal/spatial types, and multi-label nodes | Freeze actual required queries and run them against a disposable graph; reject operations outside the list rather than silently changing pagination or visibility. Prefer `entity_id` cursor pages over `SKIP` |
| Index provisioning | `ensure_index(label, prop)` emits online CREATE INDEX in current adapters | Provision scalar indexes through ontology while stopped; adapter may check `CALL db.indexes()` readiness, never pretend unsupported DDL executed |
| Schema introspection | `db.labels()`, `db.relationshipTypes()` and `db.propertyKeys()` describe the graph catalog. `keys(n)` lists stored properties on the matched node | Use `db.propertyKeys()` for declared fields and `keys(n)` for fields on visible records. Read index state through `db.indexes()` |
| Merge identity | Port accepts arbitrary `key_props`; Brainwire node MERGE uses labeled `entity_id` | Explicit stable entity-ID mapping before mutation. Current UNWIND + MERGE/SET importer is the supported subset |
| Per-query timeout | Port defaults reads to 28,500ms and requires engine-side per-request timeout. Brainwire has a configured server-wide 30,000ms cooperative query deadline | Add and test per-request timeout metadata before claiming this contract; IO timeout is independent and excludes blocking engine work |
| Conformance suite | Tests assert node ACL, no ACL-field leaks, dynamic indexes, wipe and relationship introspection | Add an isolated disposable Brainwire target only after capability gating. Never run a destructive contract wipe against an existing graph |

`CALL db.propertyKeys() YIELD propertyKey RETURN propertyKey` returns the
declared node and relationship property names plus `entity_id`. It omits
reserved ACL fields and never reads records. The catalog names are the same
for readers of the same graph regardless of node visibility.

Use `MATCH (n:Doc) RETURN n.entity_id, keys(n)` to discover stored properties
on the nodes the reader can see. For example, an undeclared `note` set on a
visible node appears in `keys(n)`. The virtual node ID is available as
`n.entity_id`, and reserved ACL fields stay hidden.

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
source access. Engine ACL lists are bounded at 5,000 entries, matching external application
knowledge-grant profiles of up to 5,000; oversized policies fail closed.
Synthetic pinned-snapshot diffs now cover updates/deletes/replay; this is not live
source qualification or native CDC. See [node ACL](node-acl.md),
[incremental ingestion](ducklake-incremental.md) and
[candidate qualification](benchmarks/2026-10-13-beta-acl.md).
The controlled rebuildable shadow beta and critical production cutover are separate
decisions. These findings belong in the remaining October queue rather than being
hidden by a successful synthetic graph/index audit.
