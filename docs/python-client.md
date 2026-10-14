# Python single, batch and partial mutations

The small `examples.python.client.GraphClient` convenience API uses the existing
Neo4j driver, named Brainwire graph, Bolt transactions and Cypher executor. It
does not create another storage path or promise the complete Neo4j API.
Apply the ontology first; use a protected writer credential file.

```python
from neo4j import GraphDatabase
from examples.python.connection import auth_from_env
from examples.python.client import GraphClient

with GraphDatabase.driver("bolt://127.0.0.1:17687", auth=auth_from_env(),
                          liveness_check_timeout=30) as driver:
    graph = GraphClient(driver, "tickets", batch_size=250, batch_bytes=1 << 20)
    graph.upsert_node("Ticket", "t-42", {"status":"open", "tags":["urgent"]})
    graph.update_node("Ticket", "t-42", {"status":"closed"}, remove=["priority"])
    graph.upsert_node("TicketComment", "c-1", {"body":"Investigating"})
    graph.upsert_edge(("Ticket","t-42"), "HAS_COMMENT",
                      ("TicketComment","c-1"), {"weight":0.7})
    graph.update_edge(("Ticket","t-42"), "HAS_COMMENT",
                      ("TicketComment","c-1"), {"weight":0.9}, remove=["old_score"])
```

This illustrates the generic mutation API; a client ingestion source must use
the source-qualified identities and required fields in the
[snapshot contract](client-shadow.md), rather than substituting `t-42`.

## Property meanings

| Input | Effect |
| --- | --- |
| Omitted property | Preserve its current value |
| Provided value | Add or replace that property |
| `None` or a name in `remove` | Remove that property |
| `entity_id` inside a property patch | Error: identity cannot be changed |

Empty patches check existence. `update_node` and `update_edge` return false for
missing targets and do not create them. `upsert_node` creates or updates;
`upsert_edge` requires both endpoints to exist, and identifies the public MERGE
edge by source, type and destination. Updating a parallel-edge group is rejected
when the preflight sees more than one relationship; arbitrary EdgeId editing is
not exposed by this helper. Edge checks and patch occur in one transaction.

## Different load sources

`load_nodes` accepts an iterable of flat dictionaries with `entity_id`, including
generators reading JSONL, DuckDB results or bounded Arrow record batches:

```python
graph.load_nodes("Ticket", (
    {"entity_id":f"t-{i}", "status":"open", "tags":[]} for i in range(1000)
))
# For an Arrow batch: graph.load_nodes("Ticket", batch.to_pylist())
```

| Producer | Feed to the same API | Responsibility |
| --- | --- | --- |
| Python list/generator | Flat typed dictionaries | Stable IDs and ontology fields |
| JSONL | Decode one JSON object per line | Mapping; use durable importer for restart |
| CSV | Transform reader rows to typed dictionaries | Parse numbers/bools/lists explicitly |
| DuckDB / Parquet / Arrow | Bounded results or `batch.to_pylist()` | Mapping and bounded materialization |
| DuckLake incremental | Separate pinned snapshot-diff importer | Source identity, baseline and cursor |
| Other sources, including Delta | Source library produces the same dictionaries | No native Delta reader is provided |

Arrow-to-Python conversion is interoperable, not zero-copy Rust/Bolt ingestion.
Lists are allowed property values; vector similarity indexes are not provided by
this helper. The ontology remains authoritative for accepted property types.

The helper preserves input order for repeated identities, including mixed
property shapes. Each bounded batch commits atomically; a later invalid row or
failed batch does not undo earlier batches. The byte estimate counts compact
UTF-8 JSON input, not exact Bolt bytes, wire quota or RSS. Values are null, bool,
int64, finite float, string, list and string-keyed map, with nesting capped at 32.
Identifiers use ASCII letters/digits/underscore, starting with a letter or
underscore, and at most 128 characters; values are parameters, never query text.
ACL property names are reserved. This helper is currently a repository example,
not a published/installable SDK.

For replay after a lost acknowledgement, use the
[durable snapshot importer](python-ingestion.md) or
[version-pinned incremental DuckLake importer](ducklake-incremental.md).
`load_nodes` has no persistent source cursor and is not a CDC service. Rerunning
authoritative upserts is idempotent only for the same identities and values.

## Trusted ACL stamping

A trusted service with graph-write **and** instance-administration grants, and
no `node_visibility` policy, can replace a node's private ACL metadata:

```python
graph.set_node_acl("Ticket", "t-42", {
    "allow":["user:zendesk-main:alice"], "deny":[],
    "connectorIds":["zendesk-main"], "sources":["tickets"],
})
```

Omitted metadata fields reset to empty. `set_node_acl` remains a separate privileged
transaction per node; it is not atomic with a prior `load_nodes` call.
To create/update one node and replace its source-authoritative ACL atomically:

```python
graph.upsert_node_with_acl("Ticket", "t-42", {"status":"open"}, {
    "allow":["user:zendesk-main:alice"], "deny":[],
    "connectorIds":["zendesk-main"], "sources":["tickets"],
}, request_id="ticket-t42-source-revision-8")
```

The provided request seed uses the explicit quorum transaction path. Omit it for
standalone managed transactions. Node mutation and ACL replacement commit together;
ACL authorization/validation failure rolls both back. This API requires an
unrestricted trusted principal with graph-write and instance-administration grants.
It does not infer permissions from ordinary properties or make an untrusted caller
an administrator. The snapshot/diff importer still does not automatically supply
or stamp authoritative ACLs: real client mapping and importer integration remain
open. See [node ACL](node-acl.md).

## Explicit quorum transaction identity

For a replicated server, `write_transaction` sends one explicit transaction with
an application-retained request seed. This API does not use managed retries,
discover a leader or swallow uncertain outcomes. Connect to the current leader;
standalone servers reject the identity metadata. Existing `load_nodes`/CRUD helpers
keep their existing managed-transaction behavior and do not gain a durable source
cursor or retained request identity automatically.

```python
from examples.python.client import GraphClient, request_key

seed = "ticket-t42-source-revision-7"  # Persist before sending; reuse on retry.
key = request_key("brainwire-quorum-demo", "tickets", "writer", seed)

def apply_ticket(tx):
    tx.run("MERGE (t:Ticket {entity_id:$id}) SET t.status=$status",
           id="t-42", status="closed").consume()
    return "t-42"

# Reuse a driver connected to the current quorum leader.
graph = GraphClient(driver, "tickets")
acknowledged_id = graph.write_transaction(apply_ticket, request_id=seed)
```

Consume query results inside the callback; return ordinary materialized values.
The return value is delivered only after COMMIT acknowledgement. Callback errors
rollback via the driver's transaction context. `CommitOutcomeUnknown` or
`RequestAlreadyCommitted` propagate without a second execution or invented result.
Inspect `key` using `cluster request --id KEY` with protected cluster credentials.
A committed reply resolves the durable write outcome, not its lost query results.
Retain the same seed, payload and authenticated principal across retries; never
assign a fresh seed merely because the first response was lost. An absent lookup
is not proof that an in-flight proposal will never commit. See
[quorum operations](quorum-replication.md) for the full outcome protocol.

Seeds accept 1–128 ASCII letters/digits or `-`, `_`, `:`, `.`. `request_key` uses
compact UTF-8 JSON matching the Rust namespace, including Unicode principal IDs.
Custom callbacks still obey ontology, authorization, query and replicated-batch
limits; this method does not split an oversized transaction automatically.
