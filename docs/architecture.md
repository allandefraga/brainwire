# How Brainwire works

Brainwire is a graph database written in Rust. A server accepts Bolt
connections, executes a documented subset of Cypher and stores named graphs
in RocksDB. Python, the CLI and the terminal workspace are ways to use that
engine.

```text
Python helper / Bolt driver / connected terminal workspace
                         │
                         │ Bolt: graph, identity, query, parameters
                         ▼
                Brainwire server (Rust)
         authentication → graph grants → query execution
                         │
            transaction snapshot + staged writes
                         │
                         ▼
                  RocksDB graph store
         records · adjacency · catalog · indexes · commit state
                         │
                         ▼
             persistent local disk or mounted volume
```

## Data model and named graphs

A graph contains nodes, directed relationships and properties. A node has one
fixed label and an immutable `entity_id`. The ontology/catalog declares node
labels, properties and allowed relationship endpoints. It also records index
configuration. Different named graphs have separate stores; selecting a graph
is part of the client session.

Ordinary relationship identity is its endpoint pair and type. A new relationship
type can opt into immutable merge keys to keep distinct events on the same pair.
The application chooses those keys. See [online catalog changes](online-catalog.md)
and [keyed relationships](temporal-data.md#keyed-relationships).

## Reads and writes

A transaction reads a storage snapshot and can see its own staged writes.
Other transactions see those writes after commit. The engine checks conflicts
before publishing changes; errors abort the transaction. Graph records, index
changes and the local commit sequence are written in one synchronous RocksDB
batch. RocksDB provides the write-ahead log and immutable data files used when
reopening the store.

A local commit sequence identifies progress within that store. It is not an
`AS OF` timestamp or a cluster-wide position. A failed connection while COMMIT
is in flight may leave the client uncertain whether a write committed. Use the
[transaction contract](transactions.md) and [identified request outcomes](standalone-requests.md)
when designing reconciliation; the client does not blindly replay writes.

The offline `brainwire cypher` command opens a local database directory.
Stop a server that owns that store before using offline commands. A connected
client or TUI talks to the server over Bolt instead of opening its files.

## Memory and query execution

Brainwire uses query and ingress accounting, shared reservations and sampled
memory-pressure admission to reject some work under pressure. RocksDB uses a
shared cache and memtable budget; the memtable allowance is counted inside
the cache budget. Some query operators stream rows and others materialize data.
The Python client can also retain results in its own memory.

These are cooperative controls. They do not guarantee a hard process-wide RSS
ceiling or prevent every out-of-memory kill. Native allocations, page cache,
connections and result encoding still matter. Keep an operating-system/container
limit and test representative concurrent workloads. See [memory settings](memory-defaults.md)
for budgets, metrics and defaults.

## Indexes and context retrieval

Node indexes support selected scalar equality and string predicates. The
experimental relationship property index supports selected equality/range
queries and eligible keyed merges. It must be explicitly enabled on a standalone
store; enabling it changes the disk format, and older binaries cannot reopen
that upgraded volume. Queries use only a ready index and retain a scan fallback.
See [indexes](indexes.md) and [relationship index setup](relationship-property-indexes.md).

The [context helpers](context-api.md) return bounded records, source fields and
one-hop evidence using the caller's normal graph access. They can suggest
ontology paths, sample visible instance paths and combine ranked candidate IDs
from application-configured providers after an authorized graph read. Schema
paths describe catalog shapes, not records or node-ACL visibility. Instance
paths are bounded samples, not shortest or complete routes. Providers are
application code, not a native ANN or BM25 index, automatic source discovery,
or permission to run agent actions.

## Permissions and transport

A protected server authenticates a principal and checks its graph grants.
Configured node-visibility rules filter records and traversal endpoints in
supported query paths. The client chooses credentials; an agent-supplied user
name alone does not grant access.

Native Bolt TLS is not implemented. Shared environments need authentication
and a verified encrypted network boundary. The local demo explicitly enables
unauthenticated access for synthetic data on host loopback. Read
[authentication](bolt-auth.md), [protected containers](protected-containers.md)
and [transport](protected-transport.md) before connecting an application.

## Persistence, backup and recovery

Container data belongs in a persistent mounted volume. Restarting with the same
volume reopens the existing graph; starting on an empty volume creates a new
store. RocksDB recovery handles its committed local state, while checkpoints
create a consistent local graph copy. Keep a tested copy on independent storage
to protect against loss of the machine or disk.

Online checkpoints require an operator-configured destination and administrator
permission. A local checkpoint is not a cluster-wide backup or automatic backup
retention. Follow [checkpoints](checkpoints.md) and [recovery](termination-recovery.md).

## Standalone and experimental quorum

The standard Docker, Kubernetes and Swarm templates run one database instance.
More pods do not automatically replicate its store. The separate experimental
three-voter quorum setup gives each voter its own storage and uses Raft for
replicated writes and recovery.

Quorum tests cover local processes, simulated partitions and restarts. Production
failover across machines remains unqualified. Snapshot transfer and log purge
are not implemented; capacity diagnostics are advisory. The experimental
relationship-index disk format is not supported by quorum stores. Read the
[quorum guide](quorum-replication.md) before evaluating that mode.

## Choose a use case

| Use case | What the application supplies |
| --- | --- |
| Connected context for an agent | Entities, relationships, source references and access rules |
| Event timeline | Stable event identities, timestamps and retention policy |
| Incremental source ingestion | Mapping, stable source keys and reconciliation rules |
| Code understanding | Source files and the local code-graph extractor |

Brainwire provides the graph and its query/storage behavior. It does not infer
an ontology automatically, capture all application changes, retain historical
revisions, or turn a deployment into a production-qualified distributed system.
The [documentation index](README.md) links each capability to its usage guide.
