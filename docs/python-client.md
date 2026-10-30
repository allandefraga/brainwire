# Write and update a graph from Python

Install the helper from this checkout using the [Python package guide](python-package.md).
`GraphClient` uses the Neo4j Bolt driver to send Cypher to one named Brainwire
graph. The application creates and closes the driver and chooses credentials.
For a runnable synthetic demo, use the [README example](../README.md#write-from-python).

## Choose the operation

| Task | Helper |
| --- | --- |
| Create or update one node | `upsert_node` |
| Patch an existing node without creating it | `update_node` |
| Create or update a relationship between existing nodes | `upsert_edge` |
| Patch an existing relationship | `update_edge` |
| Load rows in bounded transactions | `load_nodes`, `load_edges` |
| Remove known nodes and their incident relationships | `delete_nodes` |
| Remove selected relationships and keep the nodes | `delete_edges` |
| Run parameterized Cypher | `run_cypher` |
| Stream read results page by page | `stream_cypher` |

## Single records and property patches

This example assumes an ontology declaring `Ticket`, `TicketComment` and
`HAS_COMMENT`, plus the properties used below. It runs from the checkout and
uses its credential-file helper; [protected connections](bolt-auth.md) explains
that file. Apply your ontology first and use a writer credential.

```python
from neo4j import GraphDatabase
from examples.python.connection import auth_from_env
from brainwire import GraphClient

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
    # Explicit identities only; this does not discover stale source records.
    removed = graph.delete_nodes("Ticket", ["t-obsolete"])
    rows = graph.run_cypher(
        "MATCH (t:Ticket {entity_id:$id})-[:HAS_COMMENT]->(c:TicketComment) "
        "RETURN c.entity_id AS id, c.body AS body LIMIT 20",
        {"id":"t-42"},
    )
    print(rows)
```

`run_cypher(query, parameters={})` runs one statement against this client's
named graph and returns a materialized list of record dictionaries. Parameters are
separate from query text. Each statement is atomic; the helper does not retry
uncertain writes. Use `write_transaction(..., request_id=...)` for retained
quorum request identities. See the [implemented Cypher grammar and examples](local-cypher.md)
for supported clauses and explicit current limits.

This illustrates the generic mutation API; a source-specific importer can
apply its own stable identity mapping and schema contract.

## Read results incrementally

Use `stream_cypher` for reads that would create a large Python list:

```python
with graph.stream_cypher(
    "MATCH (t:Ticket) RETURN t.entity_id AS id, t.subject AS subject",
    fetch_size=200,
) as records:
    for record in records:
        print(record["id"], record["subject"])
```

The method requests read-only access and yields one record dictionary at a time.
The server rejects writes through this method. Keep iteration inside the `with`
block; exit closes its session and invalidates the iterator, while the supplied
shared driver stays open. An early exit discards unread results and checks the
terminal summary. Query and memory errors during iteration or final consumption
propagate, including the typed failures described below; no retry occurs. An
exception raised by your own loop is preserved if cleanup also fails.

`fetch_size` is a positive integer controlling the driver's requested page size.
It does not cap total RSS, each record's size, or allocations in server operators
that still materialize their results. Your code can still accumulate memory by
storing every yielded record. Use `run_cypher` or explicit write transactions for
writes; this streaming helper is for reads.



## One attempt per write

`GraphClient` CRUD and batch helpers use explicit transactions and make one
attempt per batch or operation. They do not use the driver's managed
`execute_write` retries. This changes earlier beta behavior: transient errors
now propagate to the caller instead of automatically replaying the callback.
A successful return and `on_batch` notification require an acknowledged COMMIT.
Callback errors roll back through the transaction context; a failed or lost
COMMIT response is propagated without a replay or a successful result.

A transport exception after COMMIT submission may mean the write was applied
even if the exception is not named `CommitOutcomeUnknown`. Reconcile the
application state before retrying. Stable MERGE identities alone do not make a
replay safe against concurrent newer changes. On a quorum server, persist a
request seed before sending and use `write_transaction(..., request_id=...)`
plus the request lookup contract below. Ordinary convenience calls do not add
a retained request identity or durable source cursor automatically.

## Typed server failures and standalone status

The helper translates two exact server codes into `MemoryPressureError` and
`CommitOutcomeUnknownError`, both subclasses of `BrainwireError`/`RuntimeError`.
The original driver error remains in `original_error` and `__cause__`; `code`
and `message` retain the server values. Other driver and callback exceptions
are propagated unchanged. Transport disconnects do not become a typed server
failure merely because they happen near COMMIT.

`MemoryPressureError` exposes `reason`, `requested_bytes`, `available_bytes`
and `rolled_back`. These fields are populated only when the terminal server
hint has a recognized reason, unsigned 64-bit sizes and `rolled_back=true`.
A missing or malformed hint leaves them `None`, so it does not authorize a
retry. `CommitOutcomeUnknownError.rolled_back` is always `None`. The helper
never retries either exception automatically.

```python
from brainwire import MemoryPressureError, CommitOutcomeUnknownError

try:
    graph.write_transaction(apply_ticket, request_id=seed)
except MemoryPressureError as error:
    # Only a verified rollback hint can inform an application retry decision.
    print(error.reason, error.requested_bytes, error.available_bytes,
          error.rolled_back)
    raise
except CommitOutcomeUnknownError:
    # Reconcile the retained request and source state before deciding what to do.
    raise
```

On an authenticated standalone server with the experimental ledger enabled:

```python
outcome = graph.request_status(seed)
# Fields: status, requestId, incarnation, sequence.
```

`request_status` opens a fresh session and sends an isolated parameterized
`CALL db.requestStatus($seed)` in autocommit mode. It validates one response row;
it does not open an explicit or managed transaction, replay an operation or
turn `not_recorded` into permission to retry. Compare the stored incarnation
with the application's expected graph identity and follow the
[standalone outcome contract](standalone-requests.md), including restore and
fork limitations. This helper is separate from quorum `request_key` lookup.

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
count as zero. Replaying a confirmed deletion is a no-op only while the source
state and edge ownership are unchanged; it can delete a concurrently recreated
edge. The result counts
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
own checkpoint must reconcile the source and graph before replaying a committed
batch; stable entity/edge identities avoid duplicate records but do not prevent
stale properties from overwriting concurrent newer values.
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
ACL property names are reserved. The helper is installable from this checkout;
see [Python package setup](python-package.md). It is not published on PyPI.

`load_nodes` and `load_edges` do not maintain a persistent source cursor. For
replay after a lost acknowledgement, use the
[durable snapshot importer](python-ingestion.md) or
[version-pinned incremental DuckLake importer](ducklake-incremental.md). `load_edges(..., require_endpoints=False)` skips rows
whose endpoint node is missing instead of rolling the batch back. To mirror any DuckLake table with
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

For bulk loads, pass `with_acl=True` to `load_nodes` and give each row an
`acl` dict. Each batch merges its properties and stamps all its ACLs with one
`db.setNodeAcls` call in the same transaction:

```python
graph.load_nodes("Ticket", [
    {"entity_id": "t-42", "status": "open",
     "acl": {"allow": ["group:tenant-a"], "connectorIds": ["crm-a"]}},
], with_acl=True)
```

The provided request seed uses the explicit quorum transaction path. Omit it for
standalone one-shot transactions. Node mutation and ACL replacement commit together;
ACL authorization/validation failure rolls both back. This API requires an
unrestricted trusted principal with graph-write and instance-administration grants.
It does not infer permissions from ordinary properties or make an untrusted caller
an administrator. The [DuckLake sync](parallel-ingestion.md) stamps ACLs from
mapped `acl_*` columns; the older snapshot/diff importer does not stamp ACLs. See [node ACL](node-acl.md).

## Explicit transaction identity

`write_transaction` sends an explicit transaction with an application-retained
request seed. It works with a quorum leader or an authenticated standalone server
with the [experimental request ledger](standalone-requests.md) enabled. The helper
does not retry, discover a leader or swallow uncertain outcomes. Existing
`load_nodes`/CRUD helpers
also make only one explicit attempt, but do not gain a durable source cursor or
retained request identity automatically.

```python
from brainwire import GraphClient, request_key

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
