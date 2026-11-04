# Native graph checkpoints

A checkpoint copies one committed RocksDB graph, including its ontology,
primary/scalar indexes and allocator high-water marks, into a new directory.
It uses RocksDB's native checkpoint API rather than copying a running directory
with ordinary filesystem tools. Stop the server before using the offline CLI.

```sh
brainwire db checkpoint research --destination ./data/research-copy
brainwire db check research-copy --json
brainwire cypher --database ./data/research-copy --query 'MATCH (n:Customer) RETURN count(*)'
```

The destination's parent must already exist. Existing destinations, including
symlinks, are refused; a destination inside the source is refused. There is no
`--force`. A sibling directory under `server.data_dir` becomes another named
graph on the next server start. A checkpoint outside that root remains an
independent embedded store until explicitly arranged as a named graph.

Committed records are included; pending writes are excluded. Later writes to
the source do not change the checkpoint. The native operation may flush data
and hard-link immutable SST files on the same filesystem, while metadata is
copied. Treat the result as its own complete directory: do not edit SST files.
The CLI receipt includes `commit_sequence` as a decimal string and
`commit_sequence_tracked`. They describe the captured store, not a later live
position. The store library exposes `Database::checkpoint` for an already-open
instance and `Database::checkpoint_with_sequence` when the receipt is needed.
Checkpoint creation holds the commit lock, so new transactions and commits wait
until capture completes. A reopened checkpoint starts from the captured counter;
source and copy can subsequently produce the same numbers independently.
See [durable commit state](commit-state.md) for scope and upgrade constraints.

This is a local single-graph snapshot, not multi-graph atomicity, off-machine
backup or object-storage durability. It has no scheduled retention, upload or
restore manager. Protect and validate a copy on independent storage before
relying on it against machine/disk loss. Tests reopen a checkpoint, audit it,
read its indexes and write new nodes without reusing an existing identity.

## Online checkpoints over Bolt

A running server can create the same native checkpoint without stopping,
when the operator configures a checkpoint root outside `server.data_dir`:

```toml
[server]
data_dir = "/data"
checkpoint_dir = "/backups"   # or BRAINWIRE_SERVER_CHECKPOINT_DIR
```

```cypher
CALL db.checkpoint('nightly-2026-10-23')
YIELD name, path, sequence, tracked
RETURN name, path, sequence, tracked
```

The checkpoint is written to `<checkpoint_dir>/<name>` for the graph selected
by the session. The name is 1-128 ASCII letters, digits, `-`, `_` or `.`, and
cannot start with `.`; callers never supply a path. The root must already
exist, and an existing name is refused. `sequence` and `tracked` have the same
meaning as the offline receipt.

Rules and limits:

- Unset `checkpoint_dir` (the default) disables the procedure.
- It requires an `instance_admin` principal; graph writers are refused.
- It must be the first clause, with no writes or other procedures in the same
  transaction. It is a side effect, not transactional: rolling back afterwards
  does not delete the directory.
- It captures committed data only. Commits on that graph wait while the
  checkpoint is created; reads continue.
- It is refused in replicated serving and for principals with node visibility
  restrictions. One replica's copy is not a cluster backup.
- The startup check that `checkpoint_dir` is outside `data_dir` compares paths
  lexically; it does not resolve symlinks.

Restore is offline: stop the server, copy or move the checkpoint directory into
`server.data_dir` under an unused graph name (or point `--database` at it), run
`brainwire db check <name> --json`, then start the server. Copy checkpoints to
independent storage yourself; the root is usually on the same volume, where
SST files may be hard links of the live store.

## Copy to independent storage from Python

`GraphClient.checkpoint(name)` in the [Python client](python-client.md) runs
the online procedure and returns a receipt with the graph, path, commit
sequence and ontology version. `examples/python/checkpoint_backup.py` then
copies the checkpoint directory to another location with a SHA-256 manifest:

```python
import json
receipt = GraphClient(driver, "research").checkpoint("nightly-2026-10-23")
open("receipt.json", "w").write(json.dumps(receipt))
```

```sh
python -m examples.python.checkpoint_backup copy \
  /backups/nightly-2026-10-23 /mnt/offsite/nightly-2026-10-23 --receipt receipt.json
python -m examples.python.checkpoint_backup verify /mnt/offsite/nightly-2026-10-23
```

The copy runs on a host that can read the checkpoint root; the server keeps
serving because a created checkpoint is never modified. It writes to
`DEST.partial`, flushes every file and directory, then renames, so an
interrupted copy never looks complete. Existing destinations, symlinks inside
the checkpoint and a receipt whose name differs from the source directory are
refused. `verify` rehashes every file and reports missing, extra or changed
files; it exits 1 on any problem. The manifest file
(`brainwire-manifest.json`) is not part of the store: remove it, or copy the
directory without it, when restoring into `server.data_dir`.

Only the manifest at the backup root is reserved; a file with the same name in
a subdirectory is part of the inventory. Verification rejects malformed,
duplicate or unsafe inventory entries and requires `CURRENT`. Copying an
existing manifested backup first verifies it. Checksums detect accidental
changes but do not authenticate the backup or replace an offline `db check`.
The destination must be outside the source checkpoint, including through
directory aliases. Keep the checkpoint immutable and use trusted parent
directories; the helper does not defend against concurrent path replacement.

Concurrent copies to the same destination do not remove one another's staging
directory. A failure while syncing the destination's parent after rename can
still leave a completed directory whose durability is uncertain; inspect and
verify it before deciding how to retry. Only one operator should own a backup
destination, and the filesystem must provide the expected rename and fsync
semantics.

The ontology version is read in the same session just before the checkpoint.
An online catalog change that commits between the two reads can leave the
recorded version one behind the checkpoint's catalog. The
manifest is a local file inventory, not an object-storage upload, encryption
or retention policy.

Synthetic measurement (2026-10-23, debug build in the `rust:1.98.1-bookworm`
container, local disk, one run): 100,000 `Customer` nodes loaded in 3.9 s;
online checkpoint 0.24 s; copy of 9 files / 3.1 MB 0.02 s; verify < 0.01 s;
restore by copying into `data_dir` plus a full `db check` 2.9 s. The restored
graph had 100,000 nodes; the live graph had 100,001 after a later write. These
are local observations, not a benchmark or SLA.
