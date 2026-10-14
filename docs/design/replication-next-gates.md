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
