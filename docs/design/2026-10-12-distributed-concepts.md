# Brainwire distributed system concepts

This is a conceptual design supplement for the proposed replicated Brainwire
instance. It explains the contracts needed to survive failures without losing
acknowledged graph changes or quietly serving a different database. It does not
add replication, routing, causal bookmarks or failover to the current binary.

The existing design chooses one Raft group per instance, covering all named
graphs. This supplement develops that boundary; it does not approve a new
implementation or replace the existing specification. Concrete failure examples
are in [the scenario workbook](2026-10-12-distributed-scenarios.md); capacity
and deployment boundaries are in [the capacity companion](2026-10-12-distributed-capacity.md).

## What distribution should give external application

A Python ingestion process should write to a logical Brainwire instance rather
than depend permanently on one machine. If a supported replica fails, the
remaining majority should continue from the committed history. An acknowledged
ticket update must retain its node, property values, edge mirrors, index changes
and ingestion cursor after the supported failure and recovery procedure, except
where later legitimate committed changes supersede them. Keeping old values
queryable for months requires the separate temporal-history contract.

There are three separate goals:

| Goal | Mechanism | What it does not establish |
| --- | --- | --- |
| Survive one unavailable machine | Replicate to independent failure domains and use consensus | Extra capacity for a graph larger than one replica |
| Recover after every authoritative disk is lost | Verified backup on independent storage | Preservation of updates newer than the backup recovery point |
| Fit or process more data across machines | Partition data and distribute execution | Atomic cross-partition transactions or cheap traversals automatically |

The first cluster should replicate complete instances. Each member holds all
named graphs; three members hold three copies, not three disjoint thirds. A
ten-terabyte logical data set still needs a sufficiently large disk on each
member, plus space for logs, compaction and recovery. Full replication is a
failure-resistance step. Sharding requires a separate design.

## Failure assumptions

The proposed initial contract handles process crashes, unavailable members and
network partitions using honest protocol participants and storage that honors
its declared durability contract. Safety means preserving the decided history;
availability means being able to complete new requests. Safe refusal while
quorum is unavailable is expected behavior.

Raft does not protect against malicious voters, a shared software bug that
commits the wrong mutation, or destruction of every recoverable copy. Disk
corruption needs detection and a safe repair/replacement procedure. Independent
volumes can still share a failing storage backend; topology claims must include
those common dependencies. No finite failover-time promise follows from an
arbitrarily delayed network or disk.

For a fresh read after a completed write, an isolated member cannot both answer
from old data and preserve that freshness promise. A minority may safely refuse
writes and strong reads while an eligible majority continues. Returning an error
is operationally useful, but does not count as completing every successful
read/write operation under the CAP theorem's availability definition. Its
consistency definition is linearizability, distinct from ontology validation or
ACID's use of the word consistency. See the original
[Gilbert–Lynch result](https://www.comp.nus.edu.sg/~gilbert/pubs/BrewersConjecture-SigAct.pdf).
The proposed API's explicitly stale mode has a different data-freshness contract;
it does not silently weaken the strong mode or its security requirements.

## Members and majority

A member is one stable participant in the consensus protocol. A container is a
process deployment unit; restarting it must not invent a new member or erase its
previous votes. The protocol assumes one active participant for each member
identity; copying a live voter volume is not a safe way to add another member. Distinct containers on one host can help test process failures
but share the host's failure domain.

For a fixed voting configuration with N members, a majority is
`floor(N / 2) + 1`. This arithmetic assumes members can communicate and the
protocol's other requirements hold; a count of running processes alone is not
proof that a usable majority exists.

| Voters | Majority | Unavailable voters tolerated while retaining a majority |
| --- | --- | --- |
| 1 | 1 | 0 |
| 2 | 2 | 0 |
| 3 | 2 | 1 |
| 4 | 3 | 1 |
| 5 | 3 | 2 |

The proposed initial deployment has three voters on three independent machines,
preferably placed according to the intended host or zone failure model. Five
voters increase the tolerated independent member failures but add replication,
storage and operational cost. A nonvoting learner can catch up before joining a
voting configuration; it does not increase voting fault tolerance by itself.
The [etcd FAQ](https://etcd.io/docs/v3.6/faq/) explains the majority tradeoff.

An isolated former leader can still believe it is leader. The guarantee is not
that every process instantly agrees on the leader's name. It is that obsolete
leadership cannot obtain the protocol's authority to acknowledge new writes.
The majority side can elect a leader while the minority refuses success for new
writes. A system that lets both sides independently confirm conflicting ticket
updates has a different consistency contract.

## One ticket update through the cluster

Suppose Python changes ticket `T-42` from `open` to `resolved`. The conceptual
write path is:

1. Authenticate the request and authorize the entire operation. Before the
   first attempt, acquire a context identifying the recovery lineage and graph
   incarnation. Preserve it alongside the operation identity on retries; do not
   resolve a recreated graph name into a new incarnation silently.
2. The leader executes the Cypher transaction against an appropriate snapshot
   and stages its graph changes. Resolve generated IDs and nondeterministic
   inputs under the allocation and ordering contracts.
3. Validate its write conflicts, identity dependencies, range dependencies and
   catalog dependencies against both applied changes and earlier accepted
   proposals which have not finished application.
4. Propose a versioned deterministic mutation command. Persist the replicated
   log using the consensus storage contract; the protocol determines commitment.
5. Apply the committed command locally in log order. Persist the complete graph
   batch and its replay metadata, then publish the instance's applied prefix.
6. Respond success after commitment and local durable publication. The slowest
   follower need not have applied the command before this response.

Before admitting new decisions, a newly elected leader must complete a
protocol-safe initial barrier covering its retained inherited log prefix, then
apply and publish through that barrier, including deduplication state. A
previously known committed index alone is insufficient: a retained request can
become committed through that boundary. The consensus library determines the
barrier and commitment; a committed current-leadership no-op is one possible
mechanism under its contract. Fence transactions and reservations belonging to
earlier leadership in both serialized and concurrent admission alternatives. Acquiring graph context does not prevent a
subsequent lifecycle change: compare the expected incarnation at admission and
commit, including earlier accepted lifecycle proposals.

These are Brainwire integration requirements, not an alternative implementation
of Raft. The consensus library must control commitment, election, membership and
log reconciliation. In particular, counting copies of an arbitrary old-term
entry is not an adequate substitute for its commitment rules.
See [Raft sections 5 and 8](https://raft.github.io/raft.pdf).

The leader must not first publish an ordinary local commit and then attempt to
replicate it. That would expose a mutation the cluster might never commit. In
cluster mode, all authoritative mutation paths must pass the replicated order.

## What the replicated command contains

The follower should apply decisions made by the leader, rather than execute the
same Cypher text independently. Reexecution can select a different snapshot,
generate different IDs, use a different wall clock or produce different results
from an external embedding service.

A proposed command envelope needs an explicit format version, cluster lineage,
graph incarnation, stable logical column-family identifiers, final mutations
and commit metadata. Where deduplication is promised it also includes the
operation identity, request fingerprint and response metadata needed for retry.
The precise encoding and size limits remain design decisions.

Do not assume a serialized native RocksDB `WriteBatch` is a portable replication
format. Column-family handles or IDs belong to local store instances. A logical
mutation should identify a family such as `node`, `out` or `idx`; each member
resolves that identity to its local handle. Supported encoding versions must
also remain deterministic during a rolling upgrade.

Every authoritative effect belongs in the replicated contract:

| Effect | Why omission matters |
| --- | --- |
| Node record and identity mapping | A replay or failover could duplicate or hide the entity |
| Both edge directions | Traversals could disagree or leave dangling edges |
| Scalar index removals and insertions | An equality lookup could return an old value or miss the new one |
| Ontology and index readiness | Members could interpret the same data differently |
| Allocator reservation | A new leader could reuse an ID already handed out |
| Source cursor and mapping identity | Python could skip data or apply a batch twice |
| Future history, retention and maintenance effects | Members could disagree about what still exists |
| Retry record and application marker | Recovery could repeat a logical operation |
| Graph registry and incarnation changes | A name could refer to different stores on different members |

Local caches, memtable organization, compaction timing and an ephemeral sparse
projection do not need to be byte-identical. They must preserve the agreed
logical state and visibility rules. Replicas can have different SST files while
representing the same graph.

## Four different meanings of progress

| Position | Meaning | What it cannot prove |
| --- | --- | --- |
| Raft log position | A command's place in a particular replicated history | That it has committed or is visible |
| Committed prefix | Consensus has decided this prefix | That this member has applied it |
| Published applied prefix | This member can safely expose the corresponding state | That another member has caught up |
| Current local graph commit sequence | Existing nonempty storage commits in one RocksDB store | Cluster consensus, causal progress or global uniqueness |

The current [local sequence](../commit-state.md) includes ID reservations and is
independent for every graph. A restored copy can repeat the source's numbers.
It must remain explicitly local; it cannot be relabeled as a cluster bookmark.

The Raft log and the RocksDB WAL also have different jobs. The replicated log
orders and retains consensus commands. The local WAL protects local batches
against the failures covered by synchronous storage. Neither name makes the
other layer unnecessary. Whether a later design can reduce duplicate storage
writes requires an explicit recovery proof and measurements.

The proposed ACK requires local application in addition to the required durable
quorum commitment. That makes a successful update immediately readable through
the leader's published state. A committed command whose local application fails
is not an ordinary rolled-back transaction: it may still be recovered and
applied elsewhere.

## Applying to separate RocksDB graph stores

Today each graph is a separate RocksDB. A `WriteBatch` is atomic within its
database; it does not atomically update another database called `system`.
See [RocksDB atomic updates](https://github.com/facebook/rocksdb/wiki/Basic-Operations).

One candidate protocol retains those separate stores and restricts each normal
log command to one graph incarnation. A single instance apply lane processes
entries in order. For a committed entry E:

1. Check E's lineage, graph incarnation and exact ordered identity.
2. In one synchronous graph batch, write all graph effects, a marker for E and
   the required graph-scoped retry result.
3. Synchronously persist the global applied cursor through E in `system`.
4. Publish matching in-memory progress and conflict metadata. Release waiting
   readers and successful commit or deduplication responses.
5. Only then begin applying the next entry.

A crash after step 2 and before step 3 leaves one graph durably ahead of the
published prefix. Recovery reads the retained committed command, recognizes its
exact matching graph marker and completes publication without applying its
effects again. Wrong lineage, mismatched command identity or an unexpectedly
advanced marker is an inconsistency, not a reason to skip by numeric comparison.

There must also be a publication gate for snapshot capture. While steps 2–4 are
in progress, new public transaction snapshots cannot observe the new graph batch
with old global progress. Existing immutable snapshots can keep running. A global gate can delay new
snapshots for other graphs and every read mode too, including stale reads;
staleness permission does not bypass publication safety. Capture
the graph snapshot and its published instance position together, then release
the gate; the query should not hold it for its entire lifetime.

If the global cursor write fails, keep publication closed and stop new apply.
The command is already committed. Recover the member or replace it through a
safe procedure; do not acknowledge a false success or claim definitive rollback.

This is a candidate integration protocol reviewed conceptually. It still needs
a concrete storage format, crash-boundary proof, library integration and tests.
Cross-graph application is intentionally outside this candidate's transaction
boundary. Filesystem graph lifecycle effects need their own replay rules.

### Reasoning table for one committed mutation

Let P be the previously published prefix and E its next committed entry. The
rows assume the candidate single apply lane, atomic graph batch, synchronous
storage and valid retained recovery history. This is a reasoning table, not a
crash test of an implemented replicated engine.

| Interruption point | Durable graph marker | Durable system cursor | Public state before crash | Required recovery action |
| --- | --- | --- | --- | --- |
| Before any graph apply | Prior graph marker | P | P | Establish E's commitment and apply its graph batch |
| Graph batch persisted; system cursor unfinished | E | P | Existing snapshots at P; new capture gated | Match E exactly and finish system publication without repeating effects |
| System cursor persisted; runtime publication unfinished | E | E | Existing snapshots at P; new capture gated | Reconstruct matching runtime state before opening readiness |
| Publication complete; response lost | E | E | E | Resolve the same operation's recorded outcome under its authorization/retention contract |
| Final success delivered | E | E | E | Preserve E through supported normal recovery in the same lineage |

An interrupted or failed sync has an unknown persistence outcome; recovery
examines durable markers rather than assuming that no returned success means
no bytes persisted. A deliberate disaster restore into a new lineage from a
backup older than E can lose E under its separate RPO contract.

Graph markers for other graphs can remain older than P or E; this command did
not target them. No row permits publishing a graph mutation before the protocol
has committed it. If recovery cannot establish the required history or the
marker disagrees with the command, it must fail safely rather than infer success
from a higher numeric counter.

Offline write tools are also authoritative mutation paths. In a future cluster,
stopping one member is not permission to alter its graph files independently
and then rejoin as if those edits had been replicated. Offline repair/import
needs an explicit cluster-aware procedure; today's local maintenance commands
must not silently become a bypass of consensus.

### Bulk loading needs its own durability decision

The approved [design spec](../superpowers/specs/2026-09-24-brainwire-design.md)
section 1.7 proposes online bulk loading with the WAL disabled until a final
flush and sync, and a redo contract after an interrupted load. This is planned
wording, not the current resumable Python importer's ordinary synchronous
transaction path. It cannot silently inherit the candidate per-command apply
protocol above: persisting a global applied cursor past graph effects that can
disappear after a crash would violate that protocol.

With WAL-disabled writes, independent column-family flushes can leave a marker
durable while another family's data is lost. Recovery must not treat that
marker as proof of the entire load without a durable joint cut or an explicit
baseline reconstruction protocol. See
[RocksDB atomic flush](https://github.com/facebook/rocksdb/wiki/Atomic-flush);
that local option does not supply the separate graph/system or cluster protocol.

Future choices include ordinary durable replicated batches, or a separately
specified staged generation that stays outside public reads until its durable
activation. The latter needs graph identity, source cursor, index readiness,
replica installation and interrupted-activation rules; an SST file or alias
swap alone supplies none of those cluster guarantees. An alternative relying
on replay from a durable consensus log is possible only with a demonstrated
recovery contract, including safe applied metadata, snapshot installation and
log retention. Disabling the RocksDB WAL is therefore a design decision to
prove, not an automatic distributed ingestion optimization.

Committing an immutable file's hash or generation name in the consensus log
also does not replicate its contents. Activation must guarantee that the
referenced content remains recoverable after the supported leader/storage
failure, under the chosen artifact durability and retention contract.

Returning only one final bulk ACK does not settle intermediate visibility,
restart behavior or retry identity. The load must explicitly define those
boundaries before distributed performance claims. This session does not amend
the approved spec or implement any of these alternatives.

## Concurrent transactions and IDs

Raft orders commands; it does not by itself validate the conditions under which
the leader produced them. Two transactions might both see that ticket `T-42`
does not exist, validate `MERGE`, and enter the log before either is applied.
Without a proposal-aware conflict rule, ordered application can still publish
incorrect decisions.

The simplest candidate serializes validation through final publication, with a
bounded queue. A more concurrent design reserves keys and range dependencies
against pending proposals. The first is easier to reason about; the second
needs explicit reservation release, leadership fencing and bounded memory.
Operation identities also need pending deduplication/reservation or coalescing:
two retries can arrive in the same leadership before a receipt is persisted.
Reserving data keys alone does not define how that retry is resolved.
Holding today's committer mutex while awaiting Raft can deadlock if application
needs the same mutex. The storage and proposal phases need a deliberate boundary.

Snapshot isolation and serializable transactions are also different contracts.
Ordering final mutation batches does not prevent every anomaly involving reads
from earlier snapshots. The distributed mode must retain or explicitly change
the current transaction isolation contract. It must not advertise serializability
merely because it uses consensus.

Current ID allocation reserves a durable block before handing out its IDs, and
Bolt RUN can return a created node's ID before the explicit transaction commits.
A cluster cannot preserve that promise by allocating IDs only after final COMMIT
without changing what RUN exposes.

Two options require further decision:

- Commit reservations through consensus and durably publish them before handing
  out IDs; mere log receipt by a follower is insufficient. On new leadership, abandon
  unused ranges from the former leader. Gaps are acceptable; reused handed-out
  IDs are not.
- Use a different globally safe allocation scheme or explicitly provisional
  identifiers. Specify what users observe before commit, after abort and after
  failover. This is an API and format decision, not just an allocator optimization.

No-reuse applies within the same recovery lineage and graph incarnation,
including abort, restart and supported failover. Installing a current cluster
snapshot on a follower does not create a new lineage or relax this protection.
A disaster restore from E1/P into E2 may lose reservations newer than P and later
produce numbers seen in E1's discarded future. The restored baseline's existing
IDs and allocator state must still be preserved.

An external physical reference therefore needs lineage, graph incarnation and
internal ID. A tenant/source/type-qualified business key can help external application
reconcile an entity after restore. Neither a bare numeric ID nor the same
business key authorizes automatically replaying an E1 operation into E2.

## Reads and their costs

The proposed API should make its read contract explicit:

| Mode | Required condition | Ticket example |
| --- | --- | --- |
| Strong fresh snapshot | Obtain a safe leadership/read barrier, wait for its published applied prefix, then capture the snapshot | A new read started after the successful resolution sees that update or a later one |
| Causal minimum | Validate a bookmark's lineage and wait for at least that published prefix before snapshot capture | A follower sees my resolution, but may lack newer unrelated commits |
| Possibly stale snapshot | Read a member's safely published local state, with its position identified | A reporting query may see the ticket before resolution |

An old leader's local `leader=true` flag is not enough for a strong read during
a partition. The consensus read barrier must establish current authority.
A bookmark is a minimum progress requirement, not that barrier. The application
must also wait for applied state and capture its snapshot after the barrier.
[OpenRaft's read contract](https://docs.rs/openraft/latest/openraft/docs/protocol/read/index.html)
describes that separation.

A long explicit transaction keeps its initial snapshot. Strong freshness at
BEGIN does not mean every later RUN silently advances to new state. Otherwise,
the existing transaction's repeatable view would change. Timeouts while waiting
for freshness should fail explicitly rather than silently fall back to stale
reads.

Bookmarks need a cluster recovery lineage, graph incarnation and replicated
position. Restoring an older backup creates a new recovery history; a numeric
position from the discarded future must not accidentally become valid again.
The exact bookmark encoding and routing support remain unimplemented.

## Python retries and ingestion

A client can lose the response after the transaction has committed. A timeout
means the outcome is unknown unless the API can subsequently resolve it.
Automatic driver retries do not create general exactly-once execution.

A deduplication contract would assign a stable operation identity before the
first attempt, bind it to a principal, recovery lineage, graph incarnation and request fingerprint,
and persist its outcome with its graph effects. Repeating the same identity and
content returns the recorded outcome after safe publication. Reusing it with
different content fails. Retention must cover the advertised retry window;
expiration must not silently authorize an old non-idempotent write as new.

Deleting a random UUID's deduplication record after a TTL cannot distinguish an
old retry from a genuinely new operation using that ID. Candidate contracts are
a session epoch with ordered operation sequences and one operation in flight,
or a bounded concurrent replay window with a durable retirement floor. The
chosen contract must also prevent reactivation of a retired session epoch.
Result bodies can expire while the knowledge needed to reject reexecution
remains replicated and covered by snapshots. A retry below a retirement floor
can report that its result expired and will not be executed again; it cannot
claim the operation was never applied. These API alternatives remain undecided.

Authorization must still be checked on retries. A credential revoked after the
original operation must not regain access just by asking for its recorded result.
Large query result streams need a separate contract; storing a small commit
receipt is not a promise to reproduce arbitrary streamed records.

The fingerprint describes the original logical request under a specified
encoding/version contract. Newly generated IDs or timestamps from executing a
retry must not redefine its identity. Lookup and pending-operation handling need
to occur at the correct boundary before accepting a second decision. A complete
multi-RUN transaction also needs an explicit request-identity scope; a receipt
for one statement is not automatically a receipt for the entire transaction.

Managed Python transaction callbacks can run again during driver retries; see
[the driver's transaction guidance](https://neo4j.com/docs/python-manual/current/transactions/).
A once-applied graph mutation does not make an email, webhook or external API
call inside that callback execute once. Those external effects require their
own retry/delivery contract. A future outbox could commit delivery intent with
graph changes, but the delivery recipient still needs deduplication or another
explicit effect guarantee. No outbox or general request deduplication is added
by this session.

The existing resumable JSONL importer is a narrower useful foundation: it commits
an immutable source fingerprint, mapping identity, cursor and graph batch in one
transaction. A retry rereads that durable cursor. It does not provide native
DuckLake change capture or a general distributed request-deduplication API.

For future DuckLake ingestion, conceptually distinguish source snapshot/version,
row identity, change ordering, graph transaction boundaries and replicated
progress. A source cursor is not a Raft index. Updating the cursor separately
from the graph would risk either skipping unapplied source data or applying a
batch twice. Changes arriving out of order also need a source-version rule so
an old event cannot overwrite a newer ticket merely because it reached the
cluster later.

### A useful future outcome vocabulary

This is a proposed client contract, not a list of implemented Bolt status codes:

| Observed outcome | Meaning for the logical graph write | Safe client behavior |
| --- | --- | --- |
| Rejected before proposal | This attempt's graph mutation was not accepted | Fix the error or retry under the documented policy; prior unknown attempts remain unknown |
| Connection/response lost after admission | Commit may or may not have occurred | Resolve/retry the same operation identity and context under the chosen contract |
| Committed but local publication failed | The decided mutation is not an aborted transaction | Recover or resolve through another eligible member; do not assume rollback |
| Final commit success | Commitment and local durable publication completed | Carry the issued causal context for later reads |
| Retry result expired/retired | Reexecution is forbidden; result may no longer be retained | Do not replace the operation ID merely to bypass retirement |
| Graph or recovery context mismatch | The request targets a different history/incarnation | Require reconciliation; do not silently reinterpret the old request as new |

An aborted or rejected user transaction can still leave replicated internal ID
reservations and gaps, as today's local allocator already does. Saying its graph
mutation was rejected is not a promise that every internal metadata byte stayed
unchanged. Cancelling a client waiter or closing its socket must not cancel or
skip an already committed command's application.

## Routing, permissions and compatible members

A stable endpoint gives Python a place to connect; it does not prove that every
backend can serve every operation. A future routing contract identifies write
and read candidates, graph context and freshness conditions. A stale route must
produce a safe routing/admission failure, not an ordinary local write on an
isolated former leader. The client refreshes discovery within a bounded retry
policy. Cached route lifetime is not a promise about election duration.

An explicit transaction stays bound to its staged state and connection. A load
balancer cannot send its BEGIN, RUN and COMMIT independently to arbitrary
members. Losing its serving member can require restarting the transaction;
final outcome resolution depends on whether a commit was accepted and whether
its response arrived. Moving an uncommitted transaction snapshot and staged
writes to another server would be a separate protocol.

Bolt specifies authenticated routing-table requests and distinguishes explicit
COMMIT from intermediate transaction messages. See
[the Bolt message specification](https://neo4j.com/docs/bolt/current/bolt/message/).
This reference does not mean Brainwire already implements ROUTE or cluster
bookmarks, nor that a generic Neo4j driver automatically supplies our undecided
operation-identity contract.

Replication addresses must identify the expected member, not an arbitrary
backend behind a shared load balancer. Validate authenticated cluster/member
identity when connecting. A DNS name or valid certificate from a shared trust
root alone does not prove that an endpoint is the configured participant.
Replacing B with D is a membership change, not just pointing B's address at D.
OpenRaft explicitly makes correct peer identity the network adapter's
responsibility in its
[membership documentation](https://docs.rs/openraft/latest/openraft/docs/cluster_control/dynamic_membership/index.html).

Client permissions, peer authentication and transport encryption are different
boundaries. A client service token does not authorize joining the voting group.
Today's file-based client policy does not replicate with graph data. A future
cluster must choose replicated policy or coordinated policy generations with
explicit eligibility rules; failover must not restore a revoked token's access.
The stale-data read mode cannot silently grant permission to use stale security
state. A partitioned member may therefore need to refuse such reads even if its
graph snapshot is otherwise safe. The exact revocation/freshness contract remains
undecided. Do not put raw client credentials in replicated commands or receipts.

Mixed software versions need an explicit command/storage compatibility policy.
A new leader must not emit a format that required members, potential successors
or recovery tooling cannot interpret under that policy. An unsupported committed
command cannot be silently skipped. Feature activation, membership eligibility,
snapshot formats and rollback after an irreversible storage upgrade all need
specified boundaries. Updating container images one at a time does not establish
compatibility by itself.

## Restart and disaster recovery

With disks retained, restarting every container should recover the same member
identities, votes, logs, graph stores and snapshot generation. Startup reconciles
local state; the configured majority establishes safe leadership and commitment.
Only then do strong reads and writes become ready. One remaining voter must not
turn itself into a new one-member cluster to become green.

Missing storage is different from an empty new database. Current development
startup can create the default graph. Cluster restart must refuse a missing or
incompatible expected volume instead of creating a healthy-looking empty member.
Explicit bootstrap creates a new cluster; restart preserves identity;
replacement catches up a new member through the membership contract.

If disks are irrecoverably lost beyond the replica failure budget, external
backups define the recovery point. Restoring a backup is a separate controlled
operation that fences the old history and verifies the recovered generation.
[etcd disaster recovery](https://etcd.io/docs/v3.6/op-guide/recovery/) illustrates
the distinction between member recovery and restoring a new logical cluster.

Restore needs an explicit baseline transition. Validate the complete snapshot
from lineage E1 at prefix P, then install a generation in new lineage E2 with
provenance E1/P and the new consensus numbering/membership established by the
restore procedure. Historical graph markers and deduplication receipts belong
to that baseline. Do not compare them blindly with E2 entries or relabel them
as operations performed in E2. Later snapshots retain the provenance needed
for recovery.

An old E1 request or bookmark fails a lineage mismatch in E2; the client must
not automatically resend it as a new E2 write. The backup may contain its effect
and receipt if they preceded P, or neither if they followed P. Rejecting its
retry prevents silent duplication, but does not resolve every unknown outcome
or recover missing commits. A separate authorized historical-receipt lookup
would be a future API with no permission to repeat the write.

RPO measures the possible data loss relative to the disaster, while RTO measures
how long safe service recovery takes. The proposed target is no lost acknowledged
writes for the supported replica failure model. Backup restore has an explicit
RPO based on completed verified backups and retained logs; no universal zero-loss
promise applies when every copy and backup is destroyed.

## Consistent snapshots of every graph

The current checkpoint captures one graph. Capturing graph A, applying more
entries and then capturing graph B does not produce a snapshot at one global
prefix automatically.

A simple candidate briefly stops application at a published prefix P and
captures all graph checkpoints plus system state under that barrier. Their
per-graph markers may be below P when later entries targeted other graphs; they
need not all equal P. They must collectively represent the instance after P.
Exporting those already captured immutable checkpoints can happen later without
holding the apply barrier for the entire upload.

The manifest needs lineage, snapshot generation, covered log identity, membership,
graph registry/incarnations, storage format versions and checksums. Graph
checkpoints must contain allocators, catalogs, indexes, ingestion cursors and
deduplication state required by their contracts. Future external document blobs
must be included by immutable identity and verified before document service is
ready.

Installation uses a staging generation, validates every required component,
durably publishes the complete generation and then reopens it. Interruption
must expose either the complete old generation or the complete new one. Partial
directory replacement is not a safe installation protocol. Log history can be
discarded only when the published snapshot and retained suffix support recovery.

A portable instance snapshot contains replicated graph/system state, covered
log identity and membership. It must not clone the donor's local operational
member identity or persisted vote into a different recipient. Learner D remains
D when installing a snapshot built by A; the membership and historical log IDs
may legitimately mention A and must retain their meaning. Snapshot receipt does
not promote D to a voter. Local vote changes remain controlled by the protocol,
not overwritten or rewound by the donor's files. OpenRaft separates
[state-machine snapshots](https://docs.rs/openraft/latest/openraft/storage/trait.RaftStateMachine.html)
from [log/vote storage](https://docs.rs/openraft/latest/openraft/storage/trait.RaftLogStorage.html).

Reattaching the current retained volume to the same member, installing a
snapshot into another member and restoring an old backup are distinct actions.
An old backup of one voter's volume can omit later votes/log entries; it must not
be treated as an ordinary safe restart even when its original process is off.
Use the defined replacement or whole-cluster recovery procedure.

A local checkpoint can hard-link SST files and therefore share the source's
physical failure domain. It is not automatically an independent backup.
See [RocksDB checkpoints](https://github.com/facebook/rocksdb/wiki/Checkpoints).

## Object storage and derived search

The proposed initial role for object storage is independent backups, snapshot
artifacts and immutable document blobs. Each voter still has its own durable
local or block-backed filesystem for RocksDB and consensus state. Mounting an
object bucket as a directory does not establish the filesystem semantics the
engine requires; remote live storage needs its own evaluated design.

Three replicas can preserve an accidental deletion just as reliably as an
intentional update. Retained backups and future temporal history solve different
problems from replication. Restore must also coordinate document retention and
permissions so it does not silently resurrect data past an agreed deletion
boundary.

Future ANN and text indexes can be derived asynchronously only with explicit
visibility rules. A graph commit position, each search index's applied position
and its authorization semantics remain distinct. Waiting, an exact bounded
fallback or an explicit not-ready response are candidates. A stale index must
not expose deleted or unauthorized records. Sparse analytical projections also
need a captured graph version; their numeric output need not be stored as the
authoritative graph unless a command explicitly commits it.

## Kubernetes and Docker Swarm responsibilities

The orchestrator starts and places processes. Brainwire's consensus protocol
owns leadership and replicated history. Kubernetes' own etcd or Swarm's manager
Raft does not replicate Brainwire's RocksDB data.

| Concern | Kubernetes concept | Swarm concept | Brainwire responsibility |
| --- | --- | --- | --- |
| Stable member placement and storage | StatefulSet identity and PVCs | Explicit member services and retained-volume placement | Persist and validate member/cluster identities |
| Independent failure domains | Node/zone placement constraints | Placement constraints across nodes | Reject an unsupported claimed topology |
| Process recovery | Startup/liveness/readiness probes | Health and restart behavior | Distinguish recovery, write readiness and read modes |
| Rolling changes | Ordered updates and disruption policy | Stop-first controlled updates | Preserve quorum, compatible formats and safe rollback |
| Missing volume | Storage attachment and scheduling | Volume availability on selected node | Refuse empty automatic restart bootstrap |

[StatefulSets](https://kubernetes.io/docs/concepts/workloads/controllers/statefulset/)
provide identity and storage primitives.
[Swarm services](https://docs.docker.com/engine/swarm/services/) provide placement
and volume mounting. Neither defines the database's write consistency.

A recovering follower can be alive but unready for strong reads. Losing quorum
should not make a liveness probe repeatedly kill otherwise healthy members.
Slow disk sync, snapshot installation and catch-up require startup/recovery
budgets. Orchestrator health must not mistake temporary safe refusal for a
process that needs destruction.

## Sharding and graph locality

Future sharding would split the logical data set between groups. Each shard may
itself use replicated voters. Nine members could mean three replicated shards
with three voters each; it does not mean nine independent writers for the same
unpartitioned graph.

For external application, tenant or organization could be a candidate partition key if the
actual workload supports that locality. Ticket, comments, customer and document
chunks of one tenant could stay near one another. A global user, shared document
or cross-tenant edge complicates that boundary. A large tenant can become a hot
partition; equal node counts do not guarantee equal load.

Traversing three local hops avoids distributed coordination at each hop.
Cross-shard traversals send frontier data over the network, deduplicate visited
nodes, enforce limits and merge results. Mutating an edge whose endpoints belong
to different shards also requires a transaction and mirrored-adjacency contract.
These costs are why sharding cannot be added as a placement flag.

ArangoDB's [SmartGraphs](https://docs.arango.ai/arangodb/stable/graphs/smartgraphs/)
use an explicit attribute to improve graph locality. That is a useful design
reference, not a Brainwire capability or proof that our workload has the same
partitioning properties.

## Decisions before an implementation spike

1. Versioned logical mutation envelope and maximum transaction/log-entry size.
2. ID reservation and pre-COMMIT identifier visibility contract.
3. Proposal-aware conflict validation and bounded admission queue.
4. Per-graph replay markers, global publication protocol and failure behavior.
5. Graph lifecycle and filesystem replay, or an explicitly limited static graph
   registry for the first experiment.
6. Read modes, bookmark lineage and leader/routing errors exposed to Python.
7. Retry identity, request fingerprint, response scope and retention rules.
8. Whole-instance snapshot capture, installation and log-retention boundary.
9. Membership replacement, peer authentication and policy-version behavior.
10. A fault oracle that checks every acknowledged mutation and graph invariant
    on distinct machines after the supported failures.

The next engineering step would be a bounded mutation/apply contract experiment
with crash boundaries, followed by three-member integration. Adding containers
before those contracts are clear would leave the important safety questions
unanswered. This conceptual session does not begin that implementation.
