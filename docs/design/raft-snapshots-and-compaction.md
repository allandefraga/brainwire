# Coordinated Raft snapshots and compaction — reviewed preparation

Status: proposed next implementation, reviewed by Astra on October 14.
No snapshot transfer, installation or compaction is implemented by this document.
Keep `SnapshotPolicy::Never` until the crash/recovery gates pass.

## Capture a clean applied cut

Use the state-machine apply serialization to capture a full global LogId S after
an entire entry completes. The lock alone is insufficient: Application::apply
may have persisted graph/registry changes for S+1 while persisting APPLIED/dedupe
failed. Refuse a snapshot when the fatal apply latch or unresolved recovery state
is present. Recover through retained-log replay and validate graph ledgers and
registry before allowing capture. Inject this exact failure in the first tests.

Pause apply while taking immutable checkpoints of every published graph, the
committed registry and state-machine metadata at S. Include schemas, all column
families, allocator counters, indexes, ACL, cursors and per-graph apply ledgers.
Graph revisions differ from the global Raft index: record both. Export APPLIED,
StoredMembership, registry initialization and the complete request digest/reply
ledger. Keep dedupe history until an explicit retention contract permits eviction.
Hash and stream immutable files after releasing apply serialization.

Never transfer local VOTE, voter identity, certificates, config or the entire
Raft database to another member. Copying these is not valid replica recovery.

## Package and install

Use a versioned manifest binding cluster/snapshot identity, full LogId, membership,
graph revisions and file sizes/checksums. Authenticated transfer needs bounded
chunks, total bytes/files, concurrency, offsets and file-backed staging. Reject
path traversal, symlinks, wrong cluster and corrupt or incomplete files. Fsync a
complete validated package before publication; partial transfer leaves active
state intact.

Existing separate graph stores and consensus metadata cannot all be renamed
atomically. Installation requires durable intent and a generation/root manifest
with restart recovery. Quiesce apply/admission, drain or invalidate old graph and
transaction handles, publish the validated generation, reconcile SM metadata
and reopen admission only after durability. Keep the old generation until the
recorded phase is recoverable. Preserve each member's local vote and identity;
OpenRaft must govern accepted snapshot/log transitions.

## Storage and qualification gates

Implement durable last-purged full LogId and current snapshot metadata/data.
Prefix deletion, encoded-byte accounting and purge marker must commit together.
When the retained suffix is empty, last-log state falls back to the purge marker.
Startup must validate APPLIED/COMMITTED against a durable snapshot boundary or
retained suffix instead of requiring every historical entry physically present.
Purge only snapshot-covered applied entries after a durable usable snapshot,
through OpenRaft's purge contract.

Kill at every build/install/publish/purge phase. Test restart with an empty suffix,
lagging-member snapshot catchup plus suffix, interruption with a higher local
committed suffix and preserved vote, corrupt/oversized/wrong-cluster rejection,
all graph digests and allocator/index/ACL/cursor correctness. Lost-ACK dedupe must
survive compaction. Only then enable snapshot policy or advertise disk-loss
replacement; learner/joint membership remains a separate protocol requirement.
