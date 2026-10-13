# Gates after deterministic apply

Status: design queue, not implemented or a distributed beta claim. The current
[deterministic apply ledger](../deterministic-apply.md) applies concrete mutations
atomically inside one store. Kubernetes/Swarm qualification exercises one retained
owner. Neither elects a database leader or replicates acknowledged writes.

## Smallest coherent architecture to review next

Use one consensus group per database instance initially, with explicit membership
and private authenticated peer transport. Named graphs remain isolated stores but
share the ordered instance log. Do not replay Cypher independently on followers:
resolve identities, generated IDs, schema and concrete writes before proposing an
entry. Acknowledgement requires durable quorum commitment and local application.

The global log index and each graph's contiguous apply sequence are different.
An entry carries its target graph and graph sequence. The existing per-store ledger
can recover a crash between graph application and advancement of an instance
applied marker by replaying the same latest digest. Graph creation/deletion and
registry metadata need their own recoverable ordering protocol. Multi-graph writes
are not automatically atomic merely because there is one log.

Serve linearizable reads only after a quorum read barrier, and create the store
snapshot after that barrier. Fence stale leaders and followers from writes; do not
route arbitrary Bolt writes through a generic load balancer to every replica.
Define a separate explicitly stale follower-read mode only if required.

Snapshots must bind the registry, schema, allocation state, ACL, import cursors and
per-graph ledgers to one log position. Snapshot transfer needs checksums, staging,
atomic installation and interruption recovery. Object storage may hold checkpoints;
it does not replace consensus, local RocksDB ownership or quorum durability.

## Acceptance before enabling replication

1. Review the consensus library, failure model, entry format, membership changes
   and idempotency key protocol. Lost acknowledgements must not duplicate writes.
2. Three independently owned stores receive committed concrete entries; every
   follower's audit and logical state matches, including private ACL and cursors.
3. Kill the leader before/after durable commit and before/after apply. A new leader
   retains every acknowledged write; uncommitted work may disappear.
4. Partition a minority, reject its writes, continue with quorum and reconcile it.
   Lose quorum and refuse writes rather than acknowledging unsafe local progress.
5. Interrupt snapshot installation, restart all members, replace a failed machine
   and test storage exhaustion. Preserve explicit RPO/RTO evidence by binary hash.
6. Only then qualify Kubernetes/Swarm membership, routing, readiness, retained
   volumes and rolling upgrades across multiple machines.

A load-balanced set of containers is not evidence for any of these gates. No Raft
server, quorum protocol, peer transport or machine-loss guarantee is enabled today.
