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
    removed = graph.delete_nodes("Ticket", obsolete_ticket_ids)
    rows = graph.run_cypher(
        "MATCH (t:Ticket {entity_id:$id})-[:HAS_COMMENT]->(c:TicketComment) "
        "RETURN c.entity_id AS id, c.body AS body LIMIT 20",
        {"id":"t-42"},
    )
    print(rows)
```

`run_cypher(query, parameters={})` runs one statement against this client's
named graph and returns fully materialized record dictionaries. Parameters are
separate from query text. Each statement is atomic; the helper does not retry
uncertain writes. Use `write_transaction(..., request_id=...)` for retained
quorum request identities. See the [implemented Cypher grammar and examples](local-cypher.md)
for supported clauses and explicit current limits.

This illustrates the generic mutation API; a source-specific importer can
apply its own stable identity mapping and schema contract.

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

`delete_nodes(label, entity_ids)` detach-deletes node IDs in bounded batches and
returns the number removed. Incident edges are deleted with their node. IDs are
deduplicated within each batch; missing IDs count as zero. Each batch is atomic,
but earlier batches remain committed if a later batch fails. Use this for an
explicit list of obsolete identities; it does not discover stale nodes or make
a multi-batch snapshot reconciliation atomic. The helper bounds compact UTF-8
ID bytes and item count, not exact Bolt bytes or process memory.

## Delete edges while keeping nodes

`delete_edges(source_label, relationship, target_label, rows)` deletes directed
relationships between exact endpoint labels and identities. It keeps both
endpoints and other relationship types intact:

```python
removed_edges = graph.delete_edges("Ticket", "HAS_COMMENT", "TicketComment", [
    {"source_id": "t-42", "target_id": "c-1"},
    {"source_id": "t-43", "target_id": "c-2"},
])
```

Rows are an iterable of flat dictionaries with `source_id` and `target_id`.
Repeated pairs are deduplicated within each batch. Missing endpoints or edges
count as zero, so replaying an already deleted pair is safe. The result counts
deleted physical relationships: if a pair has parallel edges of the selected
type, **all matching edges are removed**. This differs from `update_edge`, which
requires exactly one matching relationship and rejects ambiguous parallel edges.

The helper bounds each batch by item count and compact UTF-8 JSON input bytes,
not exact Bolt bytes, deleted-edge count or process memory. Each batch commits
atomically; earlier batches remain committed if a later input row or batch fails.
It does not discover stale relationships, maintain a source cursor or delete
arbitrary EdgeIds. Apply source-specific reconciliation before calling it.

Internally the helper uses parameterized Cypher `DELETE` on a relationship
variable. Plain `DELETE` is limited to relationships; node cleanup uses
`DETACH DELETE`. This does not imply full Neo4j `DELETE` compatibility.

## Different load sources

`load_nodes` accepts an iterable of flat dictionaries with `entity_id`, including
generators reading JSONL and DuckDB results. It also accepts Arrow `Table`,
`RecordBatch`, and `RecordBatchReader` objects directly when they provide
`to_batches()`, `to_pylist()`, or `read_next_batch()`:

```python
graph.load_nodes("Ticket", (
    {"entity_id":f"t-{i}", "status":"open", "tags":[]} for i in range(1000)
))
graph.load_edges("Ticket", "RELATED_TO", "Ticket", (
    {"source_id":f"t-{i}", "target_id":f"t-{i+1}", "weight":0.8}
    for i in range(999)
))
def checkpoint(batch_number, total_rows):
    print(f"committed batch {batch_number}: {total_rows} input rows")

# DuckDB relation -> Arrow RecordBatchReader -> bounded graph writes:
arrow_reader = relation.fetch_arrow_reader(batch_size=2_000)
graph.load_nodes("Ticket", arrow_reader, on_batch=checkpoint)

# Arrow Table and RecordBatch are also accepted directly:
graph.load_nodes("Ticket", arrow_table)
graph.load_edges("Ticket", "RELATED_TO", "Ticket", record_batch)
```

`on_batch` is optional and works for both `load_nodes` and `load_edges`. Its
arguments are the one-based batch number and cumulative **input row** count for
this call; they are not counts of newly created graph elements. The callback
runs only after the corresponding transaction commits. If it raises, that
batch remains committed and the exception propagates. A caller persisting its
own checkpoint should therefore use stable entity/edge identities so a retry
can safely replay the committed batch.
This callback reports progress only; it does not create a durable source cursor
or make the entire load atomic. For restartable imports, use the importer below.

`load_edges(source_label, relationship, target_label, rows)` uses the same row
and byte limits as node batches. Edge rows require `source_id` and `target_id`;
other fields are relationship properties. Endpoints must already exist. The
endpoint/type pair is the MERGE identity, so replay updates supplied properties,
preserves omitted properties and removes fields supplied as `None`. Each bounded
batch commits atomically; a missing endpoint rolls back that batch, while earlier
batches remain committed.

| Producer | Feed to the same API | Responsibility |
| --- | --- | --- |
| Python list/generator | Flat typed dictionaries | Stable IDs and ontology fields |
| JSONL | Decode one JSON object per line | Mapping; use durable importer for restart |
| CSV | Transform reader rows to typed dictionaries | Parse numbers/bools/lists explicitly |
| DuckDB / Parquet / Arrow | Bounded results or Arrow objects directly | Mapping and bounded materialization |
| DuckLake incremental | Separate pinned snapshot-diff importer | Source identity, baseline and cursor |
| Other sources, including Delta | Source library produces the same dictionaries | No native Delta reader is provided |

Arrow batches are converted to Python dictionaries in slices no larger than
the SDK's row limit, then split again by its row/byte bounds. This avoids
materializing an entire table as one Python list, but it is not zero-copy
Rust/Bolt ingestion. The SDK does not require PyArrow; supplied objects need
the standard `to_batches()`, `to_pylist()`, or `read_next_batch()` methods.
RecordBatchReader sources are consumed but not closed; the caller owns them.
Lists are allowed property values; vector similarity indexes are not provided by
this helper. The ontology remains authoritative for accepted property types.

The helpers preserve input order for repeated identities, including mixed
property shapes. Each bounded batch commits atomically; a later invalid row or
failed batch does not undo earlier batches. The byte estimate counts compact
UTF-8 JSON input, not exact Bolt bytes, wire quota or RSS. Values are null, bool,
int64, finite float, string, list and string-keyed map, with nesting capped at 32.
Identifiers use ASCII letters/digits/underscore, starting with a letter or
underscore, and at most 128 characters; values are parameters, never query text.
ACL property names are reserved. This helper is currently a repository example,
not a published/installable SDK.

`load_nodes` and `load_edges` do not maintain a persistent source cursor. For
replay after a lost acknowledgement, use the
[durable snapshot importer](python-ingestion.md) or
[version-pinned incremental DuckLake importer](ducklake-incremental.md). To mirror any DuckLake table with
parallel workers and incremental `table_changes`, see
[parallel writes and DuckLake sync](parallel-ingestion.md).
Those batch helpers are idempotent only for the same identities and values; they
are not a CDC service.

Long imports can be deliberately paused between durable batches with
`--max-batches N` on the snapshot and DuckLake importer commands. The command
prints `paused` with the committed byte cursor; rerun with the same immutable
artifact, source ID and mapping to continue. A cursor equal to the artifact's
byte size prints `complete`. The limit counts committed source batches, not
individual records or source snapshots, and never splits a transaction.
This is cooperative pausing: it cannot interrupt a batch already in flight.

## Online checkpoint

An instance-admin client can capture the selected graph while the server runs,
when the server sets `server.checkpoint_dir`:

```python
receipt = graph.checkpoint("nightly-2026-10-23")
# {"graph": ..., "name": ..., "path": ..., "sequence": "...", "tracked": True,
#  "ontology_version": ...}
```

The name is validated locally before connecting. See
[checkpoints](checkpoints.md#copy-to-independent-storage-from-python) to copy
the result to independent storage with a verifiable manifest.

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
or stamp authoritative ACLs: real external application mapping and importer integration remain
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
