# Durable local commit state

Every nonempty storage transaction writes its next commit sequence in the same
synchronous RocksDB batch as its records and index changes. Reopening the store
loads this marker before accepting transactions. Read-only commits and rejected
writes do not create a new position. This is a foundation for later history and
replication work; it does not add either feature by itself.

## Inspect a graph

While the server is running, send this query through Python/Bolt. Embedded
queries against the same store require the server to be stopped:

```cypher
CALL db.commitState() YIELD sequence, tracked, scope
RETURN sequence, tracked, scope
```

The single row uses a decimal **string** for `sequence`, a Boolean `tracked`, and
`scope: 'store_snapshot'`. Strings preserve the full unsigned 64-bit counter in
Cypher and JSON without overflowing signed integers or losing JavaScript numeric
precision. Convert explicitly if your application needs arithmetic.

An explicit transaction reports the position captured when it began, even if
another transaction has since committed. Pending graph changes do not advance
this snapshot's position. Start a new transaction to inspect newer commits.
The procedure supports the usual YIELD aliases, filters and projection.

Stop the server before inspecting the latest local position with the offline CLI:

```sh
brainwire db commit-state research --json
# {"database":"research","sequence":"42","tracked":true,"scope":"store_latest"}
```

This command reads metadata without counting nodes or scanning index records.
The offline TUI's pipeline explains the same latest position in English or
Portuguese:

```sh
brainwire shell --overview --estimated --graph research --view query
# Add --lang pt-BR for Portuguese.
```

`db overview --json` includes each graph's `commit_state` with the same string,
tracked flag and local scope. The TUI is an offline snapshot, not a live counter. See [all six actual captures](tui-commit-state.md).
The checkpoint CLI also includes the position captured in its receipt; see
[checkpoints](checkpoints.md).

## What the number means

- It counts nonempty **storage commits**, including ontology changes and durable
  ID-block reservations. It does not count nodes, user queries or imported rows.
- A user transaction that rolls back can still leave a higher latest position
  because internal ID reservations committed independently. Its graph writes
  remain absent. Aborted IDs may leave gaps.
- Each graph has its own counter. Two independent stores, or a checkpoint and
  its source after capture, may legitimately have the same position.
- `tracked: false, sequence: '0'` means no marker exists. This includes a new
  empty graph and legacy stores whose earlier commits were never counted.
  Their first new nonempty commit establishes position 1; historical commit
  counts cannot be reconstructed from it.
- It is neither an as-of query argument nor an ingestion source offset, causal
  Bolt bookmark, cluster term/index or globally unique commit ID. Production
  Bolt still returns no fabricated causal bookmark.

For resumable ingestion, commit the source cursor and its batch of graph writes
in the same transaction. Reading this counter alone cannot determine which
source events have already been imported.

## Failure and upgrade behavior

The versioned marker occupies the reserved metadata key `commit/sequence`.
User transactions cannot overwrite or delete it. Unsupported marker versions,
malformed encodings and exhausted counters fail closed. Disjoint writers do not
implicitly depend on this shared key, so adding the marker does not force them
to conflict with each other. Explicit dependencies on that metadata key still
observe changes.

The in-memory position advances only after the synchronous batch succeeds.
A process kill after an acknowledged commit retains both its data and its
position, provided the filesystem/device honors RocksDB's synchronous WAL
contract. A lost client acknowledgement can still have an unknown outcome;
use deterministic retries and source cursors rather than assuming rollback.
Process-kill tests do not prove machine-power-loss or disk-loss durability.

Forward opening legacy stores is supported. **Writable downgrade is not
supported**: older binaries do not maintain the marker and can leave it stale.
Use a separate backup/test copy when evaluating an older binary. Restoring a
checkpoint restores its captured position; it does not continue a discarded
future branch of the counter.
