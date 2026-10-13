# Experimental deterministic per-store apply

`brainwire_store::replicated_apply` is an internal Rust foundation, disconnected
from the server and CLI. It applies already-materialized column-family/key/value
mutations, not Cypher commands. It is not a replicated database: there is no
network protocol, durable consensus log, leader election, quorum acknowledgement,
normal-commit capture or global multi-graph/registry transaction.

A version-1 `Batch` has an ordered log index and unique concrete mutations. A
mutation's CF is one of the eleven existing families, its key is nonempty, and
`None` means delete while `Some(empty)` preserves an empty value. The canonical
binary encoding is `BWAP`, version byte, big-endian index/count, then entries
sorted by CF name and key with length-prefixed CF/key and tagged optional value.
SHA-256 binds the complete encoding, including its index. Decoding rejects
truncation, trailing bytes, unsupported versions, invalid operations and
noncanonical order; announced lengths/counts are checked before copying.

Default bounds are 100,000 mutations, 64 MiB canonical bytes, 64 KiB keys and
16 MiB values. These do not define RSS or total process memory. The caller already
owns the input mutations; canonicalization, transaction overlays and RocksDB can
use additional memory. CF bytes are trusted engine-compatible materialized data;
this layer does not validate every graph/schema/index codec invariant.

`apply` starts at index 1 and accepts only the next index. The applied index and
digest are stored under a reserved `meta` ledger in the same existing synchronous
RocksDB transaction as all mutations. The latest identical entry replays without
any write or local commit-sequence advance. Divergent duplicates, older entries,
gaps, zero/exhausted indices and corrupt ledgers fail explicitly. Conflicts and
store failures propagate to the caller; there is no hidden retry or acknowledgement
from another machine. Mutation batches cannot target the ledger prefix or the
local `commit/sequence` marker.

The ledger is per store and records only the latest entry, not full log history.
A native checkpoint carries the ledger with its data; restoration retains its
next-index boundary. The local durable commit sequence is independent of the
log index. Ordinary `Txn` APIs remain trusted and can bypass this module; they
are not a replica-role authorization gate.

Apply-only stores must not perform independent writes or allocate graph IDs.
Copying persistent allocator counters does not synchronize in-memory allocator
caches or make follower promotion safe. Future consensus integration must supply
role/fencing rules, normal-commit capture, log persistence, membership, snapshot
installation and compatible codec versions before this becomes replication.

```sh
cargo test -p brainwire-store --test replicated_apply --locked --offline
cargo test -p brainwire-store --lib replicated_apply --locked --offline
```

Tests apply the same committed synthetic catalog/node/primary-key/index/
auxiliary identity/edge/ACL/source-cursor bytes to three independent stores. They compare logical
content excluding local sequence/apply metadata, audit graph invariants, restore
checkpoints, continue the ledger, reopen stores and verify latest replay,
divergence/gaps/older entries, encoding and allocation boundaries. A real
read-only RocksDB commit failure leaves both data and applied progress unchanged.
The identity CF has no serving codec yet; its extra fixture mapping is explicitly
auxiliary bytes, not a qualified identifier-search feature. All stores are
isolated temporary fixtures; no active deployment is modified.
