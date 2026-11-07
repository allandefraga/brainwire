# Production readiness roadmap

Status: engineering roadmap, not production certification. Start from the
[v0.9 qualification report](../PRODUCTION-v0.9.md): the recorded 2, 4 and 8 GiB
mixed workloads stopped during loading, and none completed the requested
30-minute qualification. Their different host loads and early stops do not
support a performance comparison.

The working tree may contain newer ingress, response encoding, Cypher and
recovery changes. Treat those as candidates until the integrated source,
binary, container and acceptance checks all pass. Record the exact commit and
image digest for any result used to approve deployment.

## Priorities for an application pilot

Work on a recoverable protected instance and distributed failure testing in
parallel. A pilot needs a persistent volume, bounded batches, application query
and ingestion tests, an encrypted network boundary, and a tested restore. Use a
rebuildable graph in shadow mode while these gates remain open. A pilot on one
instance does not establish high availability; the quorum path needs independent
volumes, protected peers and the failure tests described below.

| Priority | Work | Acceptance evidence |
| --- | --- | --- |
| P0 | Bound aggregate input, query results and transport allocations | Concurrent pre-authenticated and authenticated clients cannot overbook managed admission. Rejected requests do not execute. Disconnect, malformed input, timeout and cancellation release their leases. A worker that continues after socket cancellation retains its lease until it actually finishes. |
| P0 | Profile remaining process memory | Account separately for encoded/decoded input, query reservations, results waiting for a slow client, RocksDB cache/memtables/table metadata/compaction, allocator retention and OS page cache. Explain residual memory rather than equating a configured budget with RSS. |
| P0 | Qualify mixed load under container limits | Complete the specified four-label load and at least 30 minutes of varied concurrent reads/writes in separate 2, 4 and 8 GiB runs. Record peak process RSS and cgroup memory, kernel evidence, restart count, useful throughput, denials and timeouts. Verify exact counts/properties/ACL results and offline `db check` afterward. |
| P0 | Establish predictable overload behavior | A large request or query is bounded or rejected before exhausting process memory; other sessions remain usable. A slow reader cannot release accounting while still retaining results. Test recovery of useful throughput after pressure ends, not merely absence of a crash. |
| P0 | Confirm atomic recovery and backup restoration | Preserve all acknowledged batches after process kill and retained-volume restart. For COMMIT sent without a consumed acknowledgment, reconcile complete committed or absent state, including edges and indexes. Restore a verified checkpoint into a fresh volume and compare exact logical data and ACLs. |
| P0 | Test distributed recovery in parallel | Exercise the experimental three-voter path with independent storage, leader loss, partitions and retained-volume restart. Do not route writes to arbitrary pods or claim multi-machine HA until those paths are qualified. |
| P0 | Protect the actual network path | Enforce authenticated least-privilege access and a verified encrypted boundary. Reject plaintext where TLS is required. Test invalid/expired/wrong-host certificates and rotation. No credential, query parameter or token appears in logs or diagnostic artifacts. |
| P0 | Qualify the application's source and queries | Test stable node and relationship identity, bounded writes, updates, deletes, replay and unknown-commit reconciliation against the real source contract. Compare the application's supported queries and permissions with expected results. |
| P0 | Make authorization and compatibility measurable | Run ACL allow/deny matrices across point lookup, scans, indexes, traversals, algorithms, aggregates and result metadata. Publish a Cypher feature matrix and a fixed differential corpus whose results match or return an explicit supported error. |
| P1 | Expose actionable operational state | Provide liveness separately from readiness and pressure state. Export connection counts, request/query latency, memory reservations/denials, compaction, disk usage, errors and per-graph status. Verify alerts by injecting the condition, including low disk and sustained memory pressure. |

Managed byte admission is one mechanism within the memory work. It does not by
itself enforce a process RSS ceiling: native allocations, allocator slack,
socket buffers and downstream copies need profiling and explicit treatment.
Likewise, reducing temporary response copies does not make a materializing
executor a streaming executor. Preserve these distinctions in operator docs and
TUI explanations.

## Recovery and client contract

Keep an explicit distinction between a rejected operation, a rolled-back
transaction and a submitted commit whose result is unknown. Retry only when the
server's result and the application's idempotency rules make it safe. When the
connection is lost after COMMIT submission, reconcile by stable operation key or
durable source checkpoint before issuing another mutation.

Promote process-crash tests into recurring acceptance checks. A COMMIT/no-ack
race must record its recovered outcome per cycle; a passing run containing only
absent batches does not establish that the post-commit crash window was hit.
Process SIGKILL does not simulate disk power loss. Test fsync and storage faults
separately before advertising those guarantees.

Restoration also needs an operator contract: supported checkpoint format,
validation before activation, exclusive target ownership, configuration and
catalog compatibility, and a documented rollback path. Test upgrades using a
populated previous-version volume with indexes and ACLs, not only an empty graph.

## Cypher, plans and performance

Maintain a versioned corpus of real application queries. Cover nulls, numeric
boundaries, missing labels/types, OPTIONAL MATCH, zero-hop paths, duplicate
output columns, UNION, ordering, mutations and ACL filtering. Preserve write
schema validation when adding permissive read behavior. Unsupported syntax must
produce a stable, actionable error.

Measure query plans and resource use alongside elapsed time. Prioritize cheap
label counts where visibility permits, indexed cursor seeks, projection of only
required values, bounded traversal and eventual incremental output respecting
`PULL n`. Check that an optimization still rejects genuinely oversized results
and preserves ordering, properties and permissions. A narrow synthetic memory
improvement is evidence for that path, not an overall throughput claim.

A comparative benchmark needs identical logical data, query results, durability,
visibility, client behavior, hardware and versions for every database. Record
warm/cold state, repetitions, percentiles, background load, power/thermal state,
resource limits and failures. Keep ingestion, read latency, recovery and cost as
separate measurements. Do not derive a general ranking from one query or from
vendor results.

## Lakehouse ingestion and graph-in-place are separate paths

Brainwire currently has Python/Arrow ingestion and DuckLake synchronization
examples. The [pinned snapshot bridge](ducklake-incremental.md) compares complete
source snapshots and advances a durable artifact cursor; the
[parallel sync guide](parallel-ingestion.md) describes a separate direct table
mirror. Qualify each adapter independently rather than applying one adapter's
semantics or measurements to the other.

For a production ingest MVP, require a stable source identity and snapshot,
deterministic entity/edge keys, explicit source ownership, bounded batches,
authoritative property-null/deletion behavior, and atomic data-plus-checkpoint
updates. Test initial load, insert/update/delete, source replay, reconnect,
concurrent importers, duplicate keys, missing endpoints, expired source snapshots
and a lost commit acknowledgment. State whether readers can observe intermediate
batches and what denotes a complete source transition. ACL changes must travel
with the affected records. Do not delete unrelated application-owned graph data.

Arrow can reduce conversion overhead; it does not supply identity, CDC,
reconciliation or transaction semantics. Measure source scan time, conversion,
network transfer, graph writes, index work and reconciliation separately. Include
peak source-engine memory and scratch disk, not only graph-server memory.

A future query-in-place design needs its own decision and prototype: source
snapshot consistency across joins, predicate/projection pushdown, source ACLs,
credentials, cache invalidation, remote failure handling, cross-source semantics
and source query/egress cost. Benchmark freshness, source load and tail latency
against a maintained local graph. Reusing external tables is an architectural
option, not a free connector feature.

## Distributed operation and wider failure domains

The standard Docker, Kubernetes and Swarm manifests run a single instance.
Additional pods are not independent database replicas. The
[experimental quorum path](quorum-replication.md) has three fixed voters and
replicates the whole instance; it is not sharding or distributed query execution.

Before an HA pilot, package independent persistent volumes, stable voter
identity, protected peer traffic, routing and backup. Demonstrate leader loss,
minority isolation, asymmetric partitions, follower lag/rejoin, restart with
retained disks and recovery after a machine failure. Require majority-authorized
writes and the documented read consistency; a minority must fail safely. Verify
unknown commit reconciliation during failover. Report recovery time, unavailable
operations and any data loss against the declared contract. Test rolling upgrades
and membership changes before offering them operationally.

## Reference points, not equivalence claims

- [Apache HugeGraph's architecture](https://hugegraph.apache.org/docs/guides/architectural/)
  and [operations API](https://hugegraph.apache.org/docs/clients/restful-api/)
  suggest inspectable schema and long-running import or index jobs with explicit
  status. Future Brainwire jobs need durable identity, progress and restart
  semantics before they can be operated reliably. HugeGraph separates local
  RocksDB from its distributed backend; multiple servers must not open the
  same Brainwire RocksDB directory as a substitute for replication.
- [FalkorDB's project documentation](https://github.com/FalkorDB/FalkorDB)
  describes sparse-matrix representation and linear-algebra query execution.
  Sparse projections in Brainwire do not establish equivalent execution,
  performance or operational behavior.
- [PuppyGraph's source tutorials](https://docs.puppygraph.com/getting-started/)
  show graph access over external systems, including DuckDB and lakehouse tables.
  Its [cluster deployment guide](https://docs.puppygraph.com/installation/cluster-deployment/)
  separates control, leader and compute roles. These are useful design inputs;
  they do not imply that Brainwire's local ingestion or three-voter replication
  already provides that architecture.

Advance from shadow evaluation to a protected single-node pilot only after its
workload, security, restoration and upgrade gates pass. Advance to HA only after
its additional fault and consistency gates pass. Publish the evidence and open
limits with each candidate version.
