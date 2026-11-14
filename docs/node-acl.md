# Node visibility through Cypher, Python and the connected TUI

Node ACL metadata is private, versioned data in RocksDB's `acl` column family.
It follows transaction snapshots/overlays, survives reopening/checkpoints and
is deleted atomically with its node. Ordinary `acl_*` property writes remain
forbidden. Node envelopes and direct properties hide that namespace, including
legacy fields. An offline integrity audit rejects malformed/orphaned ACL records.

## Trusted static reader policy

Add `node_visibility` to a service principal in the protected auth policy:

```json
{
  "id":"alice-reader",
  "token_sha256":"REPLACE_WITH_SHA256_OF_PRIVATE_SERVICE_TOKEN",
  "graphs":{"tickets":"read"},
  "instance_admin":false,
  "node_visibility":{
    "principals":["user:zendesk-main:alice"],
    "allowed_connectors":["zendesk-main"],
    "open_connectors":[],
    "entry_strict":true,
    "knowledge_grants":[]
  }
}
```

Restart the server after changing the policy. Claims come only from trusted
server configuration, never Cypher parameters or client auth extras. This is
static service policy, not JWT validation or dynamic per-request external application user
delegation. Missing `node_visibility` preserves existing graph-wide service
access for compatibility; use it only for deliberately unrestricted services.
Explicit `null` is rejected. An explicit empty object is a deny-by-default
policy, and omitted ACL metadata makes ordinary nodes invisible to it.

For dynamic per-caller filtering, configure a separate service principal with
`"visibility_delegate": true` and no `node_visibility`. The trusted backend
must attach `brainwire_visibility` to **every** read transaction using the
caller identity it resolved from its own authenticated session. For example,
the Python Bolt driver accepts Bolt transaction metadata as `metadata=`:

```python
visibility = {"brainwire_visibility": {
    "principals": ["user:ticket-source:alice"],
    "allowed_connectors": ["ticket-source"],
    "open_connectors": [],
    "entry_strict": True,
    "knowledge_grants": [],
}}
# graph is a connected GraphClient created by the trusted backend.
with graph.driver.session(database=graph.database) as session:
    with session.begin_transaction(metadata=visibility) as tx:
        records = tx.run("MATCH (t:Ticket) RETURN t.entity_id LIMIT 20").data()
```

Missing or malformed metadata is denied, including for autocommit and explicit
transactions. Do not forward caller-supplied visibility claims directly from an
untrusted client; the application backend must derive them from its own trusted
identity/ACL source. Use a separate unrestricted writer/admin identity for
ingestion and ACL stamping.

## Visibility rules

The implementation follows the inspected external application `AclPredicate` composition:

- `deny_all` denies everything.
- With `knowledge_grants` present, document nodes require an exact opaque grant
  in their ACL allow list, even with `allow_all` or an open connector. Document
  detection includes `knowledge-doc:` grants, `doc_props__`/`clean/doc_props__`
  source provenance and any non-null stored `document_id`.
- Ordinary rows require overlap with `principals` and no deny overlap.
- `allowed_connectors` omitted means no entry axis; an empty list means no
  connector admitted. Strict entry rejects missing provenance.
- An admitted node with nonempty provenance entirely contained in
  `open_connectors` bypasses ordinary row ACL. This matches external application's
  `ENTRY AND (OPEN OR row_acl)` rule, including its OPEN precedence.

Comparisons are exact; canonicalization belongs to the trusted claim/stamp
producer. Lists cap at 5000 nonempty strings of at most 256 UTF-8 bytes; node ACL
encoding caps at 8 MiB. Knowledge grant syntax is `knowledge-doc:` plus 64
lowercase hex characters. external application's larger grant limits are not yet supported;
oversized claims fail closed.

## Enforcement and mutations

Filtering precedes MATCH predicates, index/primary-key results, counts, both
traversal endpoints, CSR topology/degrees/PageRank/WCC/SCC and ranking top-K.
Edges are visible only when both endpoint nodes are visible; independent edge
ACLs are not implemented. Hidden traversal sources behave as unknown sources.
Physical scan caps still bound hidden records. Restricted readers cannot request
`graph.project.scannedRelationshipCount`, which would reveal hidden edge volume.

Restricted visibility policies deny all writes, including MERGE without MATCH.
The node stamp procedure requires graph-write and instance administration, with
`node_visibility` omitted on the trusted service identity. Any explicit visibility
policy, including `allow_all: true`, forbids mutations:

```cypher
CALL db.setNodeAcl({label:'Ticket',entityId:'t-42',
                   allow:['user:zendesk-main:alice'],deny:[],
                   connectorIds:['zendesk-main'],sources:['tickets']})
YIELD updated RETURN updated
```

It replaces complete metadata; omitted fields become empty. Validation precedes
mutation, and failure discards the transaction.

Bulk loads stamp a whole batch in one call with `db.setNodeAcls`, which takes a
list of the same option maps (at most 10,000) and yields the number stamped.
Every entry is validated and every node resolved before anything is written, so
one missing node or invalid entry fails the whole call:

```cypher
CALL db.setNodeAcls($rows) YIELD updated RETURN updated
```

It has the same privilege requirements as `db.setNodeAcl`. Bolt `mode:'r'` transactions deny
mutations even for an unrestricted writer/admin, including this procedure.
Legacy protocol engines without READ-mode support fail closed.

### Avoid restoring a revoked grant

`db.setNodeAcls` replaces the ACL with the supplied values. An application that
reads an ACL, closes its transaction, and later writes a cached copy can restore
a grant revoked in between. There is no cross-transaction ACL compare-and-set.
Read the ACL and stamp the result in the **same write transaction**; a
concurrent stamp after that transaction's snapshot causes a commit conflict.
Reread and recompute after a confirmed rollback, and reconcile an unknown commit
outcome before retrying. Plain `MATCH` reads alone are snapshot reads, not a
general serializable lock.

If an application also stores its own source-level ACL bookkeeping, it must
keep that bookkeeping and the native ACL consistent in the same transaction.
When they disagree, stop and reconcile from authoritative data instead of
deriving a new grant from the stale copy. See
[transaction behavior](transactions.md) and [identified request outcomes](standalone-requests.md).

The connected Graph pane inherits these filters through its existing Cypher API;
an invisible source appears missing, and neighbors/counts never include hidden
endpoints. Schema and index declarations remain graph metadata, not per-node
secrets. Memory inspection still requires instance administration.

Query accounting reserves decoding estimates before row/ACL materialization;
these estimates and deadlines are cooperative and do not impose a hard RSS cap.
Static service identity, bounded claims and the existing snapshot importer
without automatic ACL stamping are explicit remaining integration limits.
