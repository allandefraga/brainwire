# Brainwire distributed failure scenarios

These are reasoning exercises and future acceptance criteria for a proposed
three-voter replicated instance. They are not executed distributed tests.
The current Brainwire binary is single-node. See
[the conceptual contracts](2026-10-12-distributed-concepts.md) for the proposed
write, read and recovery boundaries.

Use synthetic graph `support`, ticket `T-42`, customer `C-7`, and members A, B, C
on separate machines. Initially A is leader, all members have applied the same
committed prefix, and the ticket is `open`. Exact log positions below are
illustrative; elections can add control entries and need not produce these
specific numbers.

## Healthy update with a slow follower

Python requests `status = resolved`. A and B durably retain the command and
the protocol commits it. A applies and safely publishes it before responding
success. C has not yet applied it.

The cluster can acknowledge without waiting for C's application. A fresh strong
read observes the update or a later state. A possibly stale read on C can still
show `open`. A causal read on C with the update's bookmark waits for catch-up or
fails its freshness timeout. Returning `open` as a successful bookmark-satisfying
read would violate that contract.

**Future oracle:** verify the receipt's lineage and position, inspect each
member's published prefix, and compare strong, causal and stale read outcomes.
Do not treat an old explicit transaction's snapshot as a newly started read.

## One follower is unavailable

C stops before the update. A and B form the required voting majority and can
commit new commands while healthy and communicating. C's absence does not
change the configured voting membership or authorize A to acknowledge alone.

C restarts with retained identity and disk. It catches up from retained log
entries, or installs a valid snapshot plus the committed suffix if it is too
far behind. It becomes eligible for a requested read mode only after satisfying
that mode's recovery and freshness conditions.

**Future oracle:** record acknowledged updates while C is down; after catch-up,
check node values, primary/scalar indexes, both edge directions, catalogs,
allocator state and source cursors. File names need not match across members.

## Leader dies after the client received success

The update was acknowledged only after the required commitment and A's local
publication. A now dies. The remaining membership uses election and log rules
to establish safe leadership; an up-to-date candidate retains the committed
history. Its local state may need application before it accepts fresh work.

Clients discover the leader through the eventual routing contract and retry
transient connection failures. A configured routing TTL is not itself a
failover-time guarantee. Recovery includes election, apply/catch-up and client
rediscovery, not only process restart.

**Future oracle:** the acknowledged ticket remains resolved after leader
replacement. Measure client-observed write outage separately from election
time. Confirm no old leader can confirm conflicting new writes.

## Leader dies before the response reaches Python

The command commits, but Python never receives its response. The transaction's
outcome is unknown to that client. A new attempt with an unrelated operation ID
can represent a second command even if the first one already ran.

An identity-only `MERGE` followed by fixed `SET` values may converge on the same
final row. That is narrower than once-only execution: future history appends,
counters and external side effects can duplicate. With a defined deduplication
contract, the new leader recognizes the same operation identity and content
and returns the already recorded outcome after authorization and publication.

**Future oracle:** deliberately lose the response after commitment, repeat the
same logical operation and verify one logical effect. Test conflicting content
under the same ID, expired IDs, revoked callers and restored cluster lineage.
Do not infer exactly-once semantics from the driver's automatic retry loop.

## Leader dies before commitment

A might have retained a proposed entry without obtaining the required protocol
commitment. That entry must not be applied as public graph state merely because
it exists in a local file. Following leadership changes it may survive and
later commit, or the protocol may replace an uncommitted suffix.

From a timed-out client's perspective, both this scenario and lost response
after commit can look identical. The client cannot use a connection error to
prove rollback. A result-resolution or safe retry contract must distinguish
what can subsequently be established.

**Future oracle:** inject failure around log persistence and commitment. Check
that no uncommitted entry became public and that replay follows protocol
commitment, not an arbitrary recovered `last_log` position.

## Old leader isolated from the majority

The network splits A from B and C. A still has a working process and disk, and
may initially believe it is leader. B and C can form a majority and elect a
leader through the protocol. A cannot independently obtain a quorum for a new
write. Strong reads on A require a safe read barrier and therefore cannot
silently succeed from its isolated local view.

A stale-read mode could expose A's safely published old snapshot only if the
API explicitly permits that mode. Its output must not be presented as fresh.
When connectivity returns, A reconciles through the protocol and cannot
overwrite the majority's committed history with its uncommitted proposals.

**Future oracle:** make distinct clients reach both sides, issue conflicting
updates, and record responses. Only the permitted majority history gains
successful new writes. A blackholed network and an explicit rejection should
both be covered; packet loss can produce different client timing.

## Two voters unavailable

Only A remains reachable. It may contain valid acknowledged data, but one of
three voters is not a majority. New writes and quorum-dependent strong reads
must not succeed. Being able to read files locally does not restore the cluster's
current authority.

Restart a second member with its preserved state. The protocol can reestablish
a majority once the necessary recovery and communication succeed. If both
missing members have irrecoverably lost their disks, use the disaster recovery
procedure rather than silently reducing membership on A.

**Future oracle:** availability stops safely without new successful writes;
retained-volume recovery resumes the same history. No health script force-
bootstraps a new one-member cluster.

## Every container stops and every disk remains

All processes stop. Persistent member identities, votes, logs and graph state
remain. On restart, each member recovers its local storage, validates identity,
reconciles apply metadata and participates in the original membership.

The system must recover the established history rather than initialize three
fresh graphs. It need not become ready at the exact moment ports open. Readiness
requires the applicable recovery and authority conditions. A partial restart
with only one voter stays unavailable for quorum-dependent operations.

**Future oracle:** run a known acknowledged sequence, stop every member,
restart them in different orders and verify the entire acknowledged prefix.
Include entries committed but not yet applied on a follower before shutdown.

## Process restarts with the expected volume missing

The orchestrator starts the member on a host where `/data` is an empty directory
or a newly created local volume. The member name alone is not proof of identity.
The cluster restart path must detect that its expected persistent member state
is missing and refuse ordinary readiness.

The remedy is restore/reattach the correct volume or perform explicit member
replacement. Creating a default graph as development startup does today must
not turn this failure into a healthy empty cluster member.

**Future oracle:** detach or misconfigure storage, start the process and verify
an actionable error without any new bootstrap, vote reset or empty public graph.

## Member disk lost while the majority survives

C's disk is irrecoverably lost. A and B retain quorum and the history. A new
member D joins through the supported membership procedure, receives a snapshot
and/or logs, catches up, then becomes a voter through a safe configuration
transition. Reusing C's ID with an empty vote/log store is not ordinary restart.

Adding a voter before it can contribute may raise the quorum threshold at the
wrong moment. A learner lets data recovery occur without pretending that the
new machine is already a voting copy. The exact transition must use the chosen
library's membership rules, not hand-edited lists on individual members.
See [OpenRaft membership](https://docs.rs/openraft/latest/openraft/docs/cluster_control/dynamic_membership/index.html).

**Future oracle:** remaining writes retain their guarantees during replacement;
the old C identity is fenced, D has the complete logical state, and the final
voting configuration is recorded consistently.

## Graph batch durable but global publication unfinished

Entry 41 targets graph `support`. Its graph mutations, exact replay marker and
retry result are durable in one graph batch. The process dies before the
separate `system` cursor advances from 40 to 41.

Recovery uses the retained committed log. It verifies the exact entry identity
and graph incarnation in the graph marker, skips only that already completed
graph application and persists/publishes global progress. New snapshots and
successful completion responses stay gated until publication finishes.

**Future oracle:** there is one graph effect, one required history effect,
matching indexes and a global prefix of 41 after recovery. Repeat with a global
cursor sync error while the process remains alive; it must stop publication
and further application. A different hash at position 41 must fail closed.

## Two concurrent MERGEs before either proposal applies

Transactions X and Y both read that ticket `T-42` is absent. X validates and
proposes creation but is waiting for commitment/application. Y validates against
only the applied graph and also sees absence.

Serial log order alone does not correct those stale validations. The proposal
admission rule must serialize the interval or make Y conflict with X's pending
identity reservation. An aborted proposal releases reservations only under a
defined outcome/leadership rule, rather than guessing from a client timeout.

**Future oracle:** one stable entity identity is published, and the second
transaction either observes it under a permitted retry or receives the correct
conflict. Repeat for an edge insertion racing a node's DETACH DELETE and an
ontology/index change racing a write using the old catalog.

## ID returned before transaction COMMIT

An explicit transaction creates a node and RUN returns its ID. The client then
disconnects before COMMIT. Today a durable allocator reservation protects that
handed-out ID even if the graph write rolls back.

After leader replacement, the cluster must preserve the chosen ID contract.
Replicating only the final graph mutation would omit the earlier reservation.
A new leader needs the authoritative allocator high-water mark and must abandon
unused old leader ranges. Otherwise a later node might reuse an ID the client
already saw.

**Future oracle:** every ID actually handed out remains protected under the
advertised contract, including rolled-back transactions and leadership changes.
Numerical gaps are acceptable and should be explained separately from lost data.

## Snapshot mixes different global cuts

Graph A is captured after entry 100. Entries 101–110 apply to A and B. Graph B
is captured after entry 110. Labeling the combined directories as snapshot 110
does not reconstruct A's missing later effects.

A whole-instance snapshot must capture all graphs, registry and system state
at one published prefix. A bounded capture barrier is a candidate first design.
Each graph's last targeted entry can be lower than that prefix; that is valid
when its state still represents the same complete global cut.

**Future oracle:** mutate several graphs around snapshot capture, restore to a
fresh environment and compare every graph with a reference at the declared
prefix. Include graph lifecycle, allocator and dedup state, not just node counts.

## Snapshot installation interrupted

D receives a snapshot generation containing `support`, `research` and system
metadata. It dies after writing only `support` into staging.

On restart the generation is either incomplete and discarded/resumed, or fully
validated and published by a recoverable installation protocol. D must not serve
new `support` together with old `research` and an incompatible registry. Snapshot
checksums and component identities must be checked before public readiness.

**Future oracle:** kill around every publication boundary and corrupt or omit a
component. The exposed generation is complete and internally consistent;
retained snapshot/log state always supports recovery.

## Backup restore after all authoritative copies are lost

The last completed verified backup covers prefix 35. The live cluster later
acknowledged commands through 41. All current disks are destroyed and there is
no independent retained log suffix after 35.

The backup can recover the state at 35, not promise the unavailable commands
36–41. The recovered cluster has a new recovery lineage and must fence the old
one. Old bookmarks and request identities need explicit restore behavior.
Fresh source data can help rebuild later state, but only if its retention,
mapping and event-order contracts make that possible.

For example, an external producer checkpoint at event 41 does not authorize
skipping events 36–41 when Brainwire restored graph data and its source cursor
at 35. Resume/reconcile from the recovered graph baseline using a still-available
source and the compatible mapping. Do not rebind an old write operation to the
new recovery lineage automatically. Effects outside Brainwire require their
own reconciliation and cannot inherit its deduplication guarantee.

**Future oracle:** verify the restored state at the declared recovery point,
report missing acknowledged operations rather than hiding them, and reject
bookmarks and retry contexts belonging to the discarded history. Measure backup
RPO and service RTO.

## Replica agrees on an accidental deletion

An authorized maintenance command deletes tickets too broadly. Consensus can
replicate the same mistake to every member. A healthy cluster and matching
replicas do not recover the deleted business state by themselves.

Recovery needs retained temporal history or a verified backup and a controlled
repair/restore policy. Any external document or backup retention/deletion rule
must accompany that recovery so that data is not resurrected against policy.

**Future oracle:** a maintenance preview and command scope match the requested
catalog/policy generation; a recovery drill restores the intended data while
preserving required deletion and authorization boundaries.

## Authentication policy differs after failover

A loads a policy that revokes a token. B still has an older local policy file.
If B becomes leader and serves that token, failover has restored revoked access.
Graph consensus alone does not distribute today's file-based policy.

The cluster needs either a defined replicated policy contract or coordinated
operational policy generations with eligibility/readiness rules. Peer identities
and secure transport are separate from client service-token checks. All remain
future cluster requirements.

**Future oracle:** revoke credentials, fail over and verify denial everywhere;
test policy-generation mismatch and unauthorized peer membership attempts.
Point a peer endpoint at the wrong member or another cluster and verify it is
rejected rather than counted as the expected vote.
Do not record raw tokens in log entries, snapshots or test receipts.

## Search or analytics behind the graph

The graph has published a document deletion at prefix 41, while a future derived
search index has only processed 40. Search must follow the stated freshness,
deletion and permission contract; an index hit cannot authorize exposing a
deleted or forbidden source document.

A sparse PageRank projection built from graph state at 40 is a result for that
captured state. It is not made current merely by executing on a replica that
later reached 41. Record the projection's graph version and chosen algorithm
inputs; avoid silently attaching old scores to current records.

**Future oracle:** delays, index rebuilds and failover produce either correct
results at an identified permitted version, a correct bounded fallback or an
explicit not-ready response. Validate recall separately from consistency.

## Lifecycle interrupted, then the graph name is reused

A committed lifecycle command creates or drops graph `support`, but a member
crashes between directory work, registry persistence and publication. Those
filesystem effects are not covered by a normal one-graph WriteBatch. Startup
must replay the defined lifecycle protocol or refuse readiness; directory
existence alone cannot decide the graph's authoritative identity.

If the name is recreated, its incarnation changes. A pending transaction,
bookmark or retry from the former incarnation must not affect the replacement.
Old snapshot/file references and physical cleanup need an explicit lifetime
rule; logical deletion is not permission to reuse an identity while old work
still targets it.

**Future oracle:** interrupt every lifecycle persistence boundary, recover, and
verify exactly one registry/directory incarnation is publicly selected. Reuse
the name and reject stale-context requests. If the first experiment instead
limits the graph registry to fixed graphs, lifecycle operations must be rejected
explicitly rather than accidentally bypassing consensus.

## A rolling version cannot interpret a committed command

A runs a newer binary and proposes a command that B's or C's supported format
cannot decode. If consensus commits the bytes, applying them is still required;
ignoring the command or marking it applied would diverge the graph.

Compatibility and activation rules must prevent this unsupported history before
proposal. A member that encounters a genuinely incompatible committed entry
must fail safely and recover through an authorized compatible upgrade or
replacement. A rollback may be impossible after a storage format transition;
restarting the old image is not proof of compatibility.

**Future oracle:** mixed supported versions preserve logical results across
leadership changes and snapshot transfer. Unsupported format activation and
incompatible member promotion are refused under the chosen policy. No committed
entry is silently skipped to make a member healthy.

## Conceptual acceptance ledger

Every future fault run should record request identity, principal/graph scope,
invocation/response interval, outcome classification, acknowledged commit
receipt where available, member identities, log/application positions, storage
configuration and the exact fault boundary. Preserve unknown outcomes.

Use a reference model of entity identities, node properties, edge endpoints and
mirrors, index results, source cursors and any enabled history. After recovery,
the recovered state must correspond to a permitted committed history containing
every acknowledged logical write under the supported failure contract. Later
legitimate overwrites and deletions can supersede earlier values; preserving
committed history through recovery does not promise retaining every old value
as a queryable record without an enabled temporal-history contract.
Unknown-outcome operations may have committed; their presence alone is not an
error. A successful fresh snapshot must correspond to a permitted state
respecting preceding completed operations and the declared isolation boundary.

Node counts alone cannot detect duplicated business identities, missing edge
mirrors, stale indexes, counter reuse or advanced source cursors with missing
records. Local process-kill results also do not certify host power-loss,
filesystem honesty, disk destruction or distinct-machine quorum behavior.
