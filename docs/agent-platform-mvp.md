# Brainwire agent platform: initial product slice

**Status: proposal only.** This document scopes an initial Python-facing agent
layer over Brainwire. It does not claim that an agent runtime, durable memory,
temporal history, or identity-bound multi-user ACL is implemented. It keeps
model/provider code outside the database engine.

## Product goal

Let an application build a small, inspectable agent that answers from a named
Brainwire graph, cites the records and relationships it used, and can retain
explicit application-approved memories across sessions. The first use case is a
read-mostly support-ticket assistant over synthetic data. A model may propose
an answer; only explicitly registered application tools may perform actions.

## Proposed first API

The names below are illustrative and are not existing Brainwire APIs:

```python
agent = BrainwireAgent(
    model=provider,
    graph="support",
    tools=[get_ticket, find_related_tickets],
    limits=AgentLimits(max_steps=6, timeout_seconds=30),
)

answer = agent.run("What is blocking ticket T-42?", context=verified_request)
```

`verified_request` is created by the application backend after authentication.
The caller/model cannot choose its principal, tenant, graph grant, or ACL scope.
The SDK reuses the existing authenticated Python/Bolt and Cypher path; it does
not add a privileged side channel or a generic unrestricted Cypher tool.

The initial loop is sequential and bounded: retrieve permitted context, call a
configured model adapter, optionally call one of the registered read tools,
then return an answer with source IDs and graph paths. Tool arguments are
validated against schemas, but authorization is independently checked for every
read and write. External side-effect tools and resumable workflows are later
work.

## Memory and history contract

Memory is explicit application data, separate from RocksDB/cache/query-memory
telemetry. The application chooses what to save. The initial memory API should
support stable IDs, explicit upsert, retrieval, supersede, and forget; distinguish
user-provided facts, preferences, and model inferences; attach source/event IDs,
provenance, recorded time, valid time, and a retention policy. Inferences must
not silently replace an explicit fact. Mutations need idempotency keys and a way
to inspect an uncertain commit before retrying.

**Durable memory depends on real versioning for both nodes and edges.** A memory
update must preserve the prior node revision, and changes to its graph links must
preserve edge revisions too. Commit current rows, node revisions, edge revisions,
indexes, and source/checkpoint identity atomically. Record database-assigned
`recorded_at` separately from source-provided `valid_from`/`valid_to`; history
queries must say whether they ask what Brainwire knew at a time or what was true
at a time. An as-of graph traversal is unsupported until the historical edge
access path exists. Do not emulate history by overwriting a `version` property.

The existing [temporal design](design/2026-10-01-temporal-graph-and-maintenance.md)
already specifies these node-and-edge requirements and explicitly says they are
not implemented. Implement that foundation before advertising persistent agent
memory or historical answers. Retention/forget semantics must also say what
happens to active rows, history, indexes, WAL, traces, checkpoints and backups;
“forget” must not imply physical erasure that has not been verified.

## Security and observability

- Resolve principal, tenant and graph from trusted backend authentication. Apply
  the same ACL to retrieval, graph expansion, source IDs, memory and any trace
  exposed to a caller. A `user_id` field supplied by an untrusted client is not
  access control.
- Until identity-bound ACL enforcement is qualified end to end, limit the demo
  to a dedicated graph and one configured trusted service identity. Do not call
  this shared multi-tenant isolation.
- Keep traces useful but small: run ID, model/provider identifier, timings,
  token counts when available, tool names, result IDs, commit IDs and errors.
  Prompt/context bodies are opt-in, redacted, ACL-protected and retention-bound.
- Treat retrieved text as untrusted data, not instructions. Never grant the model
  arbitrary shell, filesystem, network, or unrestricted write access.
- Surface run limits, source references, tool calls, errors and commit status in
  the Python result and later in the TUI; do not show fictional model confidence.

## Delivery order and acceptance

1. Finish the node **and edge** revision contract, atomic writes, as-of reads and
   bounded retention design/implementation. Verify concurrent updates, replay,
   equal/late timestamps, delete, crash/reopen and historical neighborhood reads.
2. Close identity-bound ACL for the Python/server route, including graph access,
   traversal and source visibility. Test two principals against the same graph.
3. Add a small provider interface and sequential read-only runner with step,
   token/result and wall-time budgets. Keep provider failures and timeout outcomes
   explicit; a timeout is not proof that a write rolled back.
4. Add explicit memory CRUD over versioned nodes/edges, with provenance,
   idempotency, supersede/forget policy and run metadata. No silent model writes.
5. Demo a synthetic ticket question across two principals: each receives only
   visible evidence; an approved preference survives a new session; updates
   preserve node and edge history; citations resolve to readable source records.

Acceptance also includes adversarial instructions embedded in a ticket, a
concurrent memory update, a repeated idempotency key, an uncertain commit, and a
retention/forget audit. Keep these measurable rather than claiming “agent-ready”.

## Not in the first slice

Autonomous multi-agent planning, a connector marketplace, hundreds of built-in
connectors, arbitrary action tools, production multi-tenant SaaS, a built-in
embedding/ANN engine, and an object-storage-native writer/indexer/reader
architecture are not first-slice requirements. Vector search may be connected
later through a bounded adapter after the existing exact/text search contracts
and ACL filtering are clear. MCP should expose the same governed APIs later,
not create a second memory or authorization implementation.

HydraDB is useful product inspiration for explicit memory operations, source
citations, filtered retrieval and separating user-facing orchestration from the
graph core. Its architecture and benchmark claims are vendor statements, not
Brainwire performance evidence. References: [HydraDB architecture](https://hydradb.com/),
[memory concepts](https://docs.hydradb.com/essentials/v2/memories),
[query](https://docs.hydradb.com/essentials/v2/query), and
[access control](https://docs.hydradb.com/essentials/v2/access-control).
