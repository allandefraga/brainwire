# Standalone durable transaction request identity

Status: an experimental storage prototype and opt-in authenticated standalone
server integration are implemented. They are disabled by default. See the
[experimental operator guide](../standalone-requests.md). Upgrade from existing
graphs, supported old-binary downgrade, ENOSPC/power loss and restore/fork remain
qualification gates; no production or automatic logical retry guarantee is
provided. The scope is one protected server and explicit write transactions.

## Implemented storage-only prototype

The opt-in Rust storage methods are hidden from generated documentation.
The ordinary server and offline tools without explicit opt-in cannot open this
format; `db check --config` supports the experimental setting, while checkpoint
creation is refused at the shared storage entry point. The experimental
server path requires explicit `server.request_ledger = true`, authentication and
a new dedicated volume:

- `Database::create_with_request_ledger` requires a nonexistent directory and
  creates a dedicated `request_ledger_v1` column family. It persists a random
  UUID incarnation, fixed quotas and an entry count synchronously. Creation
  interrupted before the header persists fails closed on reopen; it does not
  regenerate identity. Legacy activation is deliberately unsupported.
- `Database::open_with_request_ledger` requires that family and validates the
  header and every bounded record against the durable count, quotas and commit
  sequence. Ordinary `Database::open` rejects the extra family, as does the
  previous eleven-family RocksDB open policy in a regression test. An actual
  supported old-binary downgrade rehearsal is still required before enablement.
- `Txn::commit_identified` hashes the **exact staged mutations**, namespaces a
  bounded scope/seed by incarnation, and checks duplicates against the current
  RocksDB view under the committer lock. Graph mutations, the ordinary commit
  sequence, ledger record and quota counter share one synchronous WriteBatch.
  Matching duplicates return `AlreadyCommitted`; divergent mutations return
  `PayloadMismatch`. No-op identified commits persist a ledger record and
  advance the sequence once. Ordinary transactions retain their previous path.
- An ambiguous RocksDB write error quarantines the opt-in database. Subsequent
  commits, replication preparation (including empty preparation), request
  lookups and checkpoints fail until exclusive close/reopen;
  `commit_sequence` reports untracked rather than a possibly stale RAM value.
  A fault-injection test persists the batch but suppresses its successful result,
  verifies quarantine, and recovers the matching receipt and graph after reopen.
  This test supplements pre/post-commit process kills; it is not disk-full or
  power-loss qualification. Low-level graph reads and consumption of an ID
  already reserved in a cached block can continue; they cannot be durably
  committed while quarantined. Ordinary non-ledger stores keep their prior policy.
- The exact-mutation fingerprint is deliberately conservative: a replay that
  allocates different internal IDs can be rejected as a mismatch even when
  application intent is similar. It is not a public operation fingerprint or
  automatic callback replay protocol.
- `request_receipt` performs a fresh trusted-storage lookup. It accepts a scope
  only from trusted Rust callers; authentication, graph grants and principal
  incarnation must be supplied by future server integration. Absence proves
  neither rollback nor permission to retry. No code currently exposes it to
  untrusted clients as arbitrary scope input. The experimental engine supplies
  the authenticated principal ID and selected graph after checking grants.
- Canonical entry bytes are bounded by persisted entry/byte quotas, with no TTL
  or automatic eviction. These counts exclude RocksDB format overhead, physical
  disk exhaustion and process memory. Runtime quota editing is not supported.

Focused tests cover concurrent duplicate attempts, payload mismatch, no-op
records, quota rejection, MVCC conflict, invalid identities, retained-volume
reopen, corrupt/missing headers, storage write refusal and process kill before
or after commit. The kill test does not inject a crash inside RocksDB's WAL
write, simulate power loss, or qualify checkpoints/restore/fork semantics.
Copying an opt-in volume preserves its incarnation; do not treat independent
clones or restored histories as safe retry targets. The server integration
remains disabled until the remaining gates below are proved.

## v0.12 integration and remaining gates

The experimental engine accepts an explicit request ID only with the opt-in
setting, an authenticated graph writer and no static/delegated visibility.
It attaches an immutable identified-write capability before RUN and rejects all
CALL procedures across every UNION branch before executing any clause.
BEGIN checks a fresh receipt; COMMIT still performs the authoritative duplicate
check under the store committer lock. `AlreadyCommitted` remains an error.

The status endpoint recognizes an isolated `CALL db.requestStatus(seed)` from its
AST. It accepts no principal argument and requires a fresh autocommit transaction.
The engine supplies the principal and selected graph from trusted session state;
static/delegated visibility is denied. The status view is current, not the
transaction's older storage snapshot. It returns no query text or fingerprints.
The narrow first endpoint intentionally refuses composition and explicit BEGIN.

The current experiment does not activate legacy stores, expose CLI export/restore,
permit online checkpoints, provide principal-incarnation lifecycle management,
or implement a semantic operation fingerprint and automatic callback replay.
Before broader enablement, qualify real supported old binaries, existing-volume
migration, power loss/disk-full and checkpoint/restore lineage. A reused static
principal ID inherits its prior namespace; operators must use a new ID for a
new identity. Copying a volume does not create an independent safe retry history.

## Problem and observed call paths

The current Bolt parser accepts `tx_metadata.brainwire_request_id` as a bounded
ASCII identifier (`crates/bolt/src/session.rs::durable_request_id`). The server rejects an explicit ID outside replicated serving unless the new
experimental standalone opt-in is active (`engine.rs`, `Command::Begin`). Replicated serving namespaces
the seed by cluster, graph and caller, performs a quorum barrier and looks up
previous outcomes (`crates/server/src/cluster.rs::Replica::{request_key,begin}`).
Its commit path can report an unknown outcome and directs operators to the
cluster request lookup.

Standalone `Command::Commit` calls `QueryTransaction::commit`, which eventually
calls `crates/store/src/txn.rs::Txn::commit`. That function holds the per-store
committer lock, checks conflicts, and writes graph changes plus
`commit/sequence` using one synchronous RocksDB `WriteBatch`. It updates the
in-memory sequence only after the write succeeds. Read-only commits do not
advance the counter. The allocator may separately persist reserved ID blocks;
those gaps are valid and cannot identify a particular user transaction.

`docs/commit-state.md` correctly says the sequence is not a request outcome.
`crates/server/tests/uncertain_commit.py` demonstrates reconciliation of owned
synthetic graph state after COMMIT is sent and its acknowledgment is not read.
It does not supply arbitrary client transaction status or simulate power loss.

## Decision: a committed-outcome ledger, not callback replay

Add an opt-in durable request identity to standalone explicit writes. The first
increment is a committed-outcome lookup plus at-most-one committed write for a
scoped ID. A repeated ID reports a stable already-committed error; the server
does not rerun callbacks or fabricate earlier result rows.

Persist only a successful committed outcome. Do not persist `pending` as a
separate pre-commit transaction: this creates orphan pending records and an
unnecessary recovery protocol. An active request may be registered in bounded
process memory for observability, but this is advisory and disappears on crash.
Do not advertise automatic exactly-once application behavior: a source event can
still be sent with different IDs or have side effects outside the graph.

## Identity, authorization and information boundaries

- Namespace by durable store incarnation, authenticated principal identity and
  the caller's bounded request seed. Graph names alone are insufficient: a
  deleted graph recreated under the same name is a different incarnation.
- Introduce a durable store UUID before enabling the feature on a legacy graph.
  Create it once under exclusive store ownership with synchronous persistence;
  an invalid existing identity fails closed. This metadata migration needs a
  separate compatibility test and must not silently regenerate on corruption.
- Use an unambiguous versioned binary encoding of namespace components, then
  hash for bounded key length. Hashing does not replace authorization.
- The regular status procedure accepts only a seed, never an arbitrary
  principal or fully namespaced ledger key. The server supplies principal and
  selected graph from trusted session context. Check graph grants before any
  lookup. Do not return raw query text, parameters, row values or payload hashes.
- A caller sees only its own requests. A cross-principal operator inspection
  path, if added, requires a separate instance-admin capability and auditing.
  Do not silently make every graph reader able to enumerate writer activity.
- Require authenticated identity for durable request metadata initially. The
  current shared unauthenticated `standalone` caller scope is unsuitable for
  tenant isolation. An explicit development-only mode could be designed later.
- Principal deletion/recreation needs a durable principal incarnation, or an
  explicit administrative rule that a reused principal ID inherits its former
  request namespace. Current static policy IDs do not prove that distinction.
  Token rotation for the same principal must preserve its namespace.
- Restricted/delegated sessions must not gain ledger lookup merely because
  they can see some nodes. Initial API should deny request writes and lookup
  for restricted visibility unless a dedicated ownership rule is specified.

## Proposed durable format and storage operation

Store ledger records in an opt-in required column family, proposed name
`request_ledger_v1`, with versioned namespace/seed keys. Keep the store UUID in
reserved `meta` metadata. The new column family is also a proposed physical
format gate against legacy writable opens; the evidence and acceptance test are
specified below. A new metadata key alone cannot provide that gate because old
binaries ignore it. A bounded ledger record contains format version, canonical
request fingerprint, store UUID, committed local sequence, and fixed-size flags.
Do not store result sets or unbounded error messages. Reserve ledger access and
the identity metadata against ordinary graph, catalog, replication-import and
raw trusted mutations.

Add a trusted storage commit operation accepting a scoped request identity and
fingerprint. Under the same committer lock used by `Txn::commit`:

1. Validate identity, version and limits before building a batch.
2. Inspect the latest committed ledger record directly, not only the
   transaction's older snapshot. Existing `Txn::get` is a snapshot read and
   does not record a dependency; `get_for_update` does record one. Using an
   ordinary `get` followed by `put` without a final synchronized check is unsafe.
3. If an outcome exists, distinguish matching and mismatched fingerprint and
   return a stable already-committed or request-conflict error. Apply no graph
   mutations and do not increment the sequence.
4. Run ordinary MVCC conflict checks. Check sequence exhaustion and ledger
   quota. Quota admission must be atomic under this lock too.
5. Build one RocksDB batch containing graph/index/ACL/source-cursor mutations,
   the next sequence and the ledger record. Use the existing synchronous WAL
   policy; there is no durable successful state without the matching graph data.
6. After a successful write, publish in-memory bookkeeping and return a small
   acknowledgment including the seed and durable local sequence. Keep existing
   reservation/error cleanup on every exit.

An identified write transaction whose logical operations produce no graph
mutations must still define an outcome. Recommended: commit a ledger-only batch
and advance the storage sequence once. It remains a storage-commit counter.
Do not allow identified read-only transactions in the first version. Reuse
existing transaction cancellation and query-budget checks; ledger bytes count
against the configured transaction/store budgets.

## Reject external and non-transactional side effects

An identified transaction may commit only effects participating in its atomic
RocksDB batch. It must reject procedures with filesystem, network or other
independent effects before invoking them. In particular,
`crates/cypher/src/exec.rs::checkpoint_preconditions` currently rejects
`db.checkpoint` for replicated/restricted execution but does not know about an
identified standalone transaction. Propagate an explicit identified-transaction
capability into the execution context and reject `db.checkpoint` there before
checkpoint creation. A procedure being the first clause is insufficient: its
files can exist even if COMMIT never reaches the ledger.

Use a fail-closed classification of procedure effects: bounded transactional
reads and mutations explicitly supported by this contract may run; admin,
external-I/O and unknown/new effect classes cannot run in an identified
transaction until their atomic coupling is specified. Inspect all callers and
future nested/subquery paths, not just a textual search for a CALL name. Existing
pending graph changes must roll back on this rejection. Catalog/index worker
operations, graph create/drop, imports and backup hooks need explicit
classification; this document does not assume they are batch-atomic merely
because Cypher can invoke them.

Required regression: an authenticated identified BEGIN followed by
`CALL db.checkpoint(...)` fails before creating a destination directory, creates
no outcome record and discards previous graph writes. Repeat for rollback,
COMMIT/no-ack and every supported clause/subquery placement. Verify ordinary
non-identified checkpoint behavior remains supported under its current rules.

## Fingerprints and payload mismatch

Two different notions must remain explicit:

- A request fingerprint identifies the ordered logical operation transcript:
  a version marker followed by length-delimited Cypher text and typed canonical
  parameters for each RUN, plus semantic options. Compute it incrementally on
  the server to avoid retaining a second transcript. Preserve type distinctions,
  integer width/value, float bits, null versus missing, list ordering and map
  keys; reject duplicate map keys or canonically define them. Include effective
  relevant execution/security modes. Never hash ad-hoc JSON concatenation.
- A mutation digest identifies the concrete prepared storage effects. A retry
  may allocate different internal IDs or observe another snapshot, so mutation
  equality is not generally equivalent to a repeated user request.

Do not claim matching payload detection at BEGIN when only an ID has arrived.
At BEGIN, an existing scoped ID can immediately return already-committed without
executing. The stored transcript fingerprint allows detecting different payloads
at a racing COMMIT or in a future explicit verification API. If callers need
mismatch classification before running, a separately specified bounded command
API can submit the complete canonical command up front. A caller-supplied hash
alone is not proof that executed statements match it.

For a concurrent duplicate, the server computes each completed transcript. The
final lock check returns already-committed for equal fingerprints and conflict
for unequal fingerprints, regardless of internal generated IDs. Never return
previous rows as a substitute for evaluating a retried, snapshot-dependent
Cypher query. Phase one provides status and deduplication, not transparent replay.

## Lookup contract and uncertain outcomes

A proposed procedure is `CALL db.requestStatus($seed)` scoped to the current
graph and trusted caller. Return at most one fixed-size row:

- `committed`: durable ledger entry exists; return request seed, store UUID and
  sequence as a decimal string. This proves that request's graph batch committed
  in this store history, not that the application consumed its acknowledgment.
- `not_recorded`: no durable outcome is present in the selected current history.
  This is not `aborted`, not permission to blindly retry, and not proof an older
  in-flight request cannot still commit. A concurrent transaction may be active.
- Invalid/unauthorized/corrupt metadata fails with distinct stable errors and
  no existence information before authorization.

Run lookup against a fresh storage view. A status lookup inside an older explicit
snapshot cannot be advertised as latest. Prefer a standalone procedure execution
or force a new read transaction. After a server crash and exclusive retained
volume reopen, no old process remains active, but a client must still distinguish
restore, graph recreation and namespace changes before acting on `not_recorded`.

Proposed error names are placeholders pending the shared public error spec:
`RequestAlreadyCommitted`, `RequestPayloadMismatch`, `RequestLedgerFull`,
`RequestLedgerCorrupt`. Transport loss after COMMIT submission remains an unknown
outcome until reconciliation. A RocksDB write error must not be classified as
safe-to-retry solely from the function returning an error: document which errors
establish non-application; otherwise close/poison and reconcile after recovery.

## At-most-one argument and crash cases

Assumptions: one process has exclusive ownership of the store; all identified
commits use this operation; metadata cannot be modified through other paths;
RocksDB provides atomic recovered batches and the storage stack honors its sync
contract. These are assumptions to test, not a formal verification result.

For one scope/seed, all final checks are serialized under one committer lock.
The first successful synchronous batch contains both its graph effects and
ledger record. Every later operation sees that record under the lock and applies
no graph effects. A crash before durability recovers neither; a crash after
durability recovers both. A lost acknowledgment changes client knowledge only.
A failed MVCC check writes neither. Ledger quota rejection writes neither.
Do not use a separate metadata database or asynchronous ledger flush: that breaks
the coupling. Allocator reservations can survive independently and leave gaps;
they are not committed user effects.

## Bounds, retention and deletion

- Cap request ID length, record size, total ledger entries/bytes per graph and
  concurrent requests per principal. Use fixed-bounded values and measured
  overhead. On full ledger, reject identified writes before mutation instead of
  evicting records silently. The exact quotas need workload measurements.
- Phase one retains outcomes for the store incarnation with no automatic TTL.
  It can fail closed at the cap. This is operable only with documented quota
  inspection and explicit maintenance; it is not an infinite-storage design.
- Silent TTL deletion would allow an old retry to commit twice. Safe retention
  needs a future explicit epoch/fencing protocol: permanently reject old epochs,
  wait for in-flight operations, then remove their detailed entries while keeping
  a compact durable rejection watermark. Wall-clock expiry alone is insufficient.
- Deleting nodes/edges does not delete their transaction outcome. Dropping a graph
  deletes data and ledger together and a new graph gets a new incarnation.
  Clients must not replay old events automatically into a recreated graph.
- Checkpoints must contain the ledger. Restoring a checkpoint restores only that
  history; requests committed after it may be absent. Do not silently reuse an
  old causal identity. Define clone/restore incarnation and source reconciliation
  before offering transparent replay across restoration. This is an open design
  dependency, not solved by copying the UUID byte-for-byte.
- Outcome records contain linkable identifiers; privacy erasure requires a
  separate documented policy covering replicas/checkpoints/backups. Removing a
  ledger row alone neither erases graph data nor preserves dedupe guarantees.

## Compatibility and implementation sequence

1. Add and prove the physical format activation gate, reserved store identity
   and bounded ledger storage APIs; preserve unchanged legacy graphs/commits
   without request IDs. Do not enable the API until downgrade rejection is
   demonstrated using the actual supported old binaries.
2. Connect authenticated standalone explicit BEGIN/COMMIT and add scoped fresh
   lookup. Keep existing replicated behavior separate until a shared contract
   and migration are tested. No new fabricated Bolt causal bookmark.
3. Document the commit-uncertainty contract, then expose typed Python errors and
   explicit status/reconciliation methods without managed callback replay.
4. Qualify upgrades, checkpoint/restore semantics and quota maintenance before
   extending retention, aliases or automatic retry.

### Physical format activation and downgrade gate

A format version stored only in a new `meta` key cannot force an old binary to
refuse the volume. The observed old `Database::open` opens exactly the eleven
`COLUMN_FAMILIES` through `DB::open_cf_descriptors`; it neither enumerates nor
interprets a new ledger-format key. Do not describe such a key as a downgrade
barrier.

Proposed enforceable gate: activating request identity adds the required
`request_ledger_v1` column family. RocksDB writable open is expected to reject
an old descriptor set that omits an existing non-default column family. This is
a design hypothesis until the acceptance harness proves it with the actual
previous release binaries and pinned RocksDB versions. The new binary must
recognize this optional extension explicitly; it must not silently add it on
every legacy graph open. Unknown required column families/versions fail closed.

Activation is a forward-only opt-in format migration under exclusive store
ownership, before accepting identified writes. Persist the format gate and
store identity durably, verify them after reopen, then enable the API. A crash
between column-family creation and identity initialization can leave a gated
but incomplete migration: the new binary must finish a deterministic idempotent
migration or refuse activation with an explicit repair path. It must never
regenerate a corrupt existing UUID or accept identified writes in an unverified
state. Column-family creation durability is not assumed from a unit test; qualify
its actual crash behavior. Preserve the pre-activation backup separately.

Downgrade acceptance must first prove an old binary can open/write a disposable
pre-activation fixture; after activation with the candidate and clean closure,
that same old binary must refuse normal writable open and preserve logical
state. Test every supported release and official open path, plus a checkpoint
copy and interrupted activation. An unrelated configuration/permission failure
is not proof of format rejection. Never run the old binary against user data.
If any supported binary can writable-open the activated store, activation remains
blocked; do not fall back to a metadata-only claim. Document writable downgrade
as unsupported and require restore of an independent pre-activation backup.

Keep the ledger column family in the same RocksDB database so the commit batch
remains atomic across graph and ledger families. Update checkpoint/export/audit
and new-binary descriptor recognition together. Existing `replicated-apply`
family validation must explicitly prohibit importing standalone ledger records
into a replica until a shared migration contract is designed. Raw embedded users
need the same trusted API; otherwise their bypasses invalidate cross-API
at-most-one claims. A malicious administrator who manually deletes the ledger
family can invalidate guarantees; this gate protects supported binary operation,
not arbitrary disk tampering.

## Required tests before implementation can be accepted

- Same scoped seed: one committed write; repeat and concurrent races cannot
  duplicate nodes/edges/index/ACL/cursor effects. Same seed in separate graphs or
  principals is independent; unauthorized lookup reveals no existence.
- Equal transcript versus changed statement/order/typed parameter has the
  documented already-committed/mismatch behavior. Generated IDs never redefine
  semantic request identity. Large transcripts do not accumulate in memory.
- Crash before sync, after sync and before response yields complete or absent
  graph-plus-ledger; every acknowledged request survives retained-volume restart.
  Inject actual disk/fsync faults separately from process SIGKILL.
- Conflicts, failed statements, rollback, deadline, cancellation and disconnect
  produce no partial graph/ledger pair; a cancelled worker retaining execution
  cannot lose its memory/accounting/admission ownership early.
- Identified transactions reject `db.checkpoint` and all unsupported external
  side effects before invocation; no destination file or independent action
  survives a denied or rolled-back attempt. Every supported procedure execution
  path observes the same effect capability.
- Physical activation rejects old writable opens for the correct format reason,
  survives/rejects interrupted activation deterministically, and preserves the
  required family in checkpoints. A metadata key alone is not a passing gate.
- Ledger limit exhaustion, corrupt version/length/digest, sequence exhaustion and
  attempted reserved-prefix writes fail closed. Concurrent quota boundary tests
  prove no over-admission; existing untagged writes remain compatible.
- Checkpoint, clone/restore, graph drop/recreate and principal token rotation
  have explicit identity/outcome behavior. Previous-version populated-volume
  upgrade preserves all graph metadata and new ledger safety.

None of these new tests or guarantees has been implemented by this document.
