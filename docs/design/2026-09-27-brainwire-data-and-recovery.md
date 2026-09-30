# Flexible data, vector search and distributed recovery

Recorded from Allan's requests on 2026-09-27. This is a product-requirement
addendum with a proposed implementation direction, not an implemented feature
report or a promise of Qdrant API compatibility. The existing storage and
transaction foundation remains the starting point.

## Confirmed product requirements

- Accept rich property data, including lists and nested document metadata.
- Search documents by vector similarity, in the style of Qdrant, while keeping
  their relationships to people, companies and other graph entities.
- Run across containers and machines, with explicit replication and recovery.
- After a crash, restart from persistent data rather than recreate an empty graph.
  Recovery must include document content, vectors, graph links and metadata.
- Preserve the English internal model and optional Portuguese interface.
- Fault resistance is mandatory, explicitly reinforced by Allan in the same
  conversation. It is a release acceptance gate, not optional polish after
  vector search. A deployment must not be described as fault tolerant until
  its claimed failure scenarios pass reproducible recovery tests.

These requirements extend the product scope. Implementation choices and delivery
dates below are proposals; this note does not replace the existing approved spec
or assert that vector search and clustering fit into the current one-month MVP.

## Sparse graph analytics

Brainwire keeps transactional `out` and `in` adjacency records as the
authoritative graph representation. Analytics can build a disposable CSR
(compressed sparse row) projection from those records. The projection assigns
stable row and column positions to sorted `NodeId` values, stores only existing
relationships, and can be rebuilt after restart or snapshot restore.

The first supported operation is weighted PageRank over a square projection.
This keeps graph traversal and writes independent from analytics memory, while
making sparse matrix operations available for later community detection,
propagation and recommendation workloads. As of 2026-09-30, read-only Cypher
procedures expose projection summaries, degree counts and weighted PageRank.
See [sparse analytics](../sparse-analytics.md) for the implemented syntax and
limits. These projections are ephemeral, rebuilt per call; named persisted
projections, incremental refresh and distributed analytics remain future work.

## Current implementation

`crates/core/src/value.rs` supports null, boolean, signed 64-bit integer,
64-bit float, string, recursive list and string-keyed map values. The row codec
persists lists/maps and the local JSON CLI handles them. Null assignment removes
a node property. Supported scalar/container values do not imply that every
filter or index is implemented for them.

There is no dedicated vector type, vector index, semantic search API, native
temporal/binary type or production Raft cluster yet. A numeric list can hold
numbers but does not provide similarity search. The storage transaction writes
with sync enabled; the existing five-cycle SIGKILL test checks acknowledged
graph writes after process restart, not a complete host/power-loss fault model.

## Proposed data and search model

Start with an extensible, versioned type system. Lists and nested maps remain
general data. Add temporal and binary representations in explicit subsequent
codec/API changes rather than silently coercing them to strings. Define size,
nesting and numeric limits; accepting rich data does not mean unbounded values.

Treat dense vectors as typed data with a declared dimension, numeric format,
distance metric and embedding-model version. Start with packed float32 vectors
and cosine similarity; validate finite values, dimensions and zero-vector
behavior. Support named vector fields so different embedding models can coexist.
Sparse vectors, multivectors, lexical/hybrid ranking and quantization are later
slices, not prerequisites for the first dense-vector implementation.

An illustrative document graph is:

```text
(Company) <-[:ABOUT]- (Document) -[:HAS_CHUNK]-> (Chunk)
                                              text, offsets, metadata
                                              named embedding + model version
```

The ingestion layer extracts document text, divides it into chunks and obtains
embeddings from a model. The database stores the resulting values; replay must
not call an external model to regenerate a committed vector. Keep original
large PDF/image/audio files in durable blob storage with versioned references
and checksums. Their availability and backup belong to the recovery contract.

Search turns the query into a vector using a compatible model, finds matching
chunks, applies metadata/visibility constraints, and traverses graph links to
return documents, source passages and related entities. Lists of nested objects
need defined same-element filter semantics. Similarity search does not itself
generate a natural-language answer.

Build an exact-search reference first for correctness tests and recall@k
measurement. Select an approximate index implementation only after measuring
its memory, disk, update/delete and restart behavior. Do not commit to an index
that assumes all vectors and graph connections fit in RAM.

## Transaction and index consistency

The authoritative transaction must include graph mutations, vector values and
document metadata together. Acknowledging independent writes to an external
vector service would require a separate consistency protocol; no such dependency
or cross-service atomicity is assumed here.

Search indexes are derived state. Their checkpoint and applied commit position
must be recorded so they can resume or rebuild from durable committed values.
Define a search visibility boundary: wait for the required position, use a
bounded exact fallback, or return an explicit not-ready response. Do not silently
return incomplete results after restart or expose deleted/unauthorized records
from a stale index. Metadata and permissions must affect candidate selection,
not merely remove a few records after an unrestricted top-k search.

## Proposed distributed deployment

First deliver one durable node, then the existing spec's three-voter Raft
replication design. Place voters on separate machines/failure domains. Each has
its own persistent graph/vector data, Raft log and stable member identity. Never
mount one RocksDB directory into several writers. Three containers on one host
are a development fixture and do not tolerate losing that host.

The leader validates a transaction and proposes deterministic mutations. A write
may be acknowledged after the committed log entry is durably replicated to a
majority and applied locally. Followers apply the same mutations. Persist Raft
term/vote/log state and coordinate state-machine data with its last-applied
position so replay cannot double-apply mutations after a crash. Keep the existing
transaction protocol and error mapping rather than rerun arbitrary Cypher on
each replica.

Three voters require two for progress and tolerate one unavailable voter. An
isolated minority must not acknowledge writes. Reads requiring current committed
state also need a safe Raft read/leadership check. A timed-out client may have
missed a successful commit acknowledgment; retries need a defined idempotency
contract, not an assumption that every unacknowledged operation was rolled back.

This first cluster replicates the whole data set. Partitioning one large graph
across machines is separate work involving routing, cross-partition traversals,
transactions and rebalancing. Replication alone does not multiply write capacity
or make a graph larger than an individual replica's storage capacity.

| Failure | Intended behavior |
|---|---|
| Process/container dies | Reattach its retained volume, recover local WAL/log state and catch up |
| One of three machines dies | Remaining majority elects/uses a leader; replacement copies a snapshot and catches up |
| Two voters are unavailable | Stop acknowledging new writes until a majority returns |
| All containers stop, disks intact | Restart the same cluster identities, recover logs and regain readiness after a majority is available |
| A replica's data is corrupt | Fail readiness, report the problem and restore/rebuild from a verified source |
| All authoritative copies are lost | Restore an external backup, bounded by the backup's recovery point |

Startup must distinguish a new database from a damaged/missing existing volume.
Do not silently bootstrap an empty cluster over a failed mount. Kubernetes, if
chosen, would provide stable pod identities, per-replica persistent claims and
placement constraints; volume retention and readiness need explicit settings.
Orchestration restarts processes; the database implements replication/recovery.

## Acceptance and delivery order

Durability and failure tests accompany every stage. The order below describes
dependencies, not permission to ship a distributed claim before the final
fault tests pass. Measure recovery time and maximum data loss separately. The
target for acknowledged writes is no loss under the supported replica/process
failure model; disaster restore from backups has its own documented recovery
point. No unconditional survival of destroyed disks and backups is promised.

1. Finish the real server/driver boundary and bounded memory/execution work.
2. Validate rich data round trips through commit, reopen and protocol conversion.
3. Implement a dense-vector/document slice, exact reference, filtered search,
   update/delete semantics and restart/index recovery tests.
4. Expose real properties, chunks, scores, model/dimension, index progress and
   RAM/disk accounting in the backoffice, retaining the selected visual identity.
5. Add three-node replication and a fault harness: kill leader, partition network,
   restart all nodes, fail storage writes and restore backups with synthetic data.

Record every acknowledged operation and verify its graph/vector effects after
recovery. Validate graph invariants, document checksums, vector versions, index
visibility and authorization. Test search recall, latency, ingestion, compaction,
index rebuild time and peak memory under the target container limit.

Mandatory fault cases include process kill during commit, abrupt host restart,
one replica/disk lost, minority network isolation, full-cluster stop/start with
retained volumes, disk-full/write/sync failures, and restoring a backup into a
fresh environment. Cover index-update interruption and partially installed
snapshots. Readiness must wait for safe recovery; storage failure must never
be turned into a successful write response or an empty replacement database.

Report nodes, edges and vectors separately. One million 768-dimensional float32
vectors occupy 3,072,000,000 bytes before index structures, metadata, logs,
replicas or backups. Vectors therefore need their own capacity budget; the
five-billion graph-element target is not automatically a five-billion-vector
guarantee. Large-volume benchmarks follow measured smaller gates.

## Sources and existing contracts

- [Existing cluster and operations design](../superpowers/specs/2026-09-24-brainwire-design.md#51-cluster).
- [Implemented transaction boundary](../transactions.md).
- [Qdrant vector concepts](https://qdrant.tech/documentation/manage-data/vectors/): dense, sparse, named and multivector representations.
- [Qdrant payload concepts](https://qdrant.tech/documentation/manage-data/payload/): metadata and nested payload filtering.
- [Raft](https://raft.github.io/): replicated state machines and majority progress.
- [Kubernetes persistent volumes](https://kubernetes.io/docs/concepts/storage/persistent-volumes/): persistence independent of pod lifetime and retention policies.
- [RocksDB synchronous writes](https://github.com/facebook/rocksdb/wiki/Basic-Operations): write batches, WAL and sync behavior.
