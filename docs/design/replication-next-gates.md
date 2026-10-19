# Gates after initial quorum replication

Status: experimental fixed three-voter protocol implemented on October 13.
See the [operations guide](../quorum-replication.md) and
[bound qualification report](../benchmarks/2026-10-13-quorum.md).

One OpenRaft group orders concrete mutations across named graph stores. Votes,
log entries, committed/applied progress and request deduplication are durable.
Acknowledgement follows majority commitment and local application. Leader-only
reads obtain a quorum barrier before opening the graph snapshot. Per-graph apply
ledgers recover the crash gap before advancing the instance applied marker.
Graph creation initializes physical stores before publishing the registry.

The [reviewed snapshot preparation](raft-snapshots-and-compaction.md) records
the consistent-cut, installation and purge requirements for the next session.

## Remaining release gates

1. Implement and review snapshots covering registry, schemas, graph data, IDs,
   indexes, ACL, cursors, deduplication and per-graph ledgers at one log position.
   Transfer must have checksums, staging, atomic installation and interrupted
   installation recovery. Only then enable safe log purging/compaction.
2. Implement learner catchup and joint membership changes. A permanently erased
   voter needs a new identity; never reuse an ID after losing its votes/logs.
   Normal startup deliberately refuses missing identity, Raft state or stores.
3. Qualify log-cap exhaustion, long-running workloads, sustained write throughput,
   power loss and storage corruption with a frozen release binary and disks.
4. Qualify three-machine Kubernetes/Swarm deployments: independent durable whole
   instance volumes, peer mTLS, leader-aware client routing, readiness, partitions,
   machine loss, rolling upgrades and explicit measured RPO/RTO.
5. Decide supported ontology evolution and graph deletion protocols. Current
   replicated lifecycle supports initial schemas and graph creation only.

The initial acceptance uses three independent native processes on one host,
200 synthetic nodes, two graphs, partitions, leader death, majority loss and
retained-state restart. It is not evidence of snapshot transfer, erased-machine
replacement or target-cluster deployment. Existing standalone orchestrator
receipts remain evidence for their original frozen candidates only.

Object storage can hold future coordinated snapshots; it does not replace
consensus, synchronous local durability or majority acknowledgement. A generic
load balancer must not send arbitrary Bolt transactions to followers.

The October 15 local acceptance adds a bounded 1 MiB retained-log exhaustion
and same-volume recovery scenario. Three native voters reached a point where
each had less free log space than the raw property bytes of the next identical
proposal; readiness still passed its Raft quorum-read barrier, then the client
returned an uncertain outcome on the write. This confirms readiness does not
assure future-write capacity. The rebuilt candidate's observation-only probe
immediately afterward reported the safe category `fatal_storage_error`; this
is recorded without asserting the exact post-error state as universal. After
raising the cap and
restarting the original volumes, quorum lookup resolved the request as absent,
same-ID retry applied it once, and replica audits matched. This partially
qualifies cap recovery only; it does not exhaust the 1 GiB default, test power
loss/corruption or replace production-sized sustained-load qualification. See
the [receipt and limits](../benchmarks/2026-10-15-distributed-ops.md).
