# Offline native graph checkpoints

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
