# Three named graphs on one protected server

The executable `crates/server/tests/multigraph_acceptance.py` qualifies three
independent named graphs, `graph_a`, `graph_b` and `graph_c`, inside one native
Brainwire server. It uses a new private temporary store, unused loopback port,
synthetic Customer data and random file credentials. It never accesses an
existing graph, container deployment or source system.

The fixture has 10,000 Customers per graph by default, with an optional maximum
of 100,000 per graph. Every graph deliberately reuses the same entity identities,
such as `customer-000000`; graph-specific email strings and tags make accidental
cross-graph reads visible. The ontology is `examples/python/schema.json`, with
scalar equality indexes on email and active. This fixture contains no edges.

Three writers and three readers start through a shared barrier. Each writer
imports then overwrites its own graph in acknowledged batches of at most 250
rows. Readers concurrently check bounded counts, the shared identity's local
properties, and absence of foreign graph emails through scalar index reads.
After the writers finish, exact counts, active values and first/middle/last
identity/index lookups are checked in all three graphs. Old indexed email values
must disappear after overwrite. Point samples supplement the final offline
whole-graph/index audits; they do not validate every property value individually.

Credentials are generated with the existing private service-token helper and
combined into one policy with distinct principal IDs. Writers, readers, health
clients and observers each retain an explicit grant for one graph only.
Qualification requires:

- Anonymous and wrong-token connections are rejected.
- Ordinary readers cannot write or access another graph.
- Writers cannot access another graph.
- Instance-administrator observers can read query-pool telemetry only through
  their authorized graph; the telemetry grant does not grant other graphs.
- Each observer returns positive configured/reserved/peak values and nonnegative
  rejection counts. Sequential samples report the same shared pool limit and
  process-lifetime peak/rejections after concurrent work finishes. These are
  query reservation counters, not RSS or a per-graph memory allocation.
- Each reader can capture its graph's default workspace Ontology and standalone
  Indexes frame through the native CLI using its private credentials.

The fixture then leaves an explicit transaction open on graph A. A pending CREATE
and indexed overwrite are visible inside that transaction while a separate reader
still sees acknowledged data. The harness sends SIGKILL to its own server,
restarts against the same exclusive store and verifies all acknowledged A/B/C
counts and index samples. Graph A's pending node and pending indexed values must
be absent. This observes an acknowledged transaction-statement boundary before
COMMIT, not a deterministic kill inside the storage commit phase.

Finally it stops the server, runs `db check --json` and `db stats --json` for
all three graphs, requires all audits to be `ok` and exact counts to match, then
removes its owned process, storage and credentials. SIGTERM drains only
for a bounded grace; stopping for offline checks is a process boundary.

## Run with a frozen candidate

Build the binary separately, then run from the repository root:

```sh
python3 crates/server/tests/multigraph_acceptance.py --self-check
uv run --no-project --with neo4j==6.3.0 python crates/server/tests/multigraph_acceptance.py --binary target/release/brainwire --rows-per-graph 1000
uv run --no-project --with neo4j==6.3.0 python crates/server/tests/multigraph_acceptance.py --binary target/release/brainwire --rows-per-graph 100000
```

The first command checks fixture identities and batch construction only; it does
not start a server. The 1,000-per-graph smoke passed on October 13 with native
candidate SHA-256
`1042c946cb85d6c6d5b234db85163f3478ab505f20b4fb896e3b8e515d963150`.
The larger command produces a separate runtime receipt; its outcome must be
reported from that receipt rather than inferred from the smoke. Driver diagnostics
can appear on stderr when the pending transaction loses its killed connection;
JSON results are written to stdout and contain no credentials.

The JSON receipt records binary hash, driver/OS, rows per graph, acknowledged
batch counts, concurrent reader cycles, exact before/after-restart checks,
query-pool samples, TUI text hashes, SIGKILL exit code, final audits and cleanup.
The constrained fixture uses a 1 MiB encoded request cap, 16 MiB decoded quota,
64 MiB query pool and 300-second authenticated IO budget. Python clients use
`liveness_check_timeout=30` and disable mutation retry in this harness.
See [connection limits](connection-limits.md), [connected workspace](connected-status.md)
and [termination recovery](termination-recovery.md).

This qualifies named-graph isolation and recovery in one server. It does not
implement Raft, distributed failover, multi-machine load balancing, graph-level
resource quotas, a query-latency SLO or an aggregate process-memory guarantee.
