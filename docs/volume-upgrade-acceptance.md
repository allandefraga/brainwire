# Populated volume upgrade acceptance

`crates/server/tests/volume_upgrade.py` rehearses a native single-node upgrade
with two explicitly supplied Brainwire executables. It always creates a new
private temporary store and synthetic credentials. It does not accept an
existing data directory or connect to an existing server.

The old binary creates a graph with two node labels, a typed relationship,
scalar index declarations and individual allow lists. Exact properties,
relationship weights, index lookups, catalog contents, ACL metadata and
restricted-reader visibility are checked before a clean shutdown. The new
binary audits and opens the same populated store. It verifies the original
state, changes indexed values, adds a node and relationship, changes ACLs,
then restarts and repeats the exact checks and offline `db check`.

The harness forbids using the old binary after the first new-binary access.
This is forward compatibility testing, not a downgrade test. The two supplied
executables are copied into the owned temporary directory before use, and the
receipt identifies their SHA-256 hashes and reported versions. Revision fields
are caller-supplied provenance: the harness cannot prove a binary's source.

## Run

Build both binaries before running the test. Keep an immutable copy of each
binary rather than repeatedly referring to a shared Cargo output path:

```sh
.venv/bin/python crates/server/tests/volume_upgrade.py \
  --old-binary /absolute/path/to/previous-brainwire \
  --new-binary /absolute/path/to/candidate-brainwire \
  --old-revision PREVIOUS_SOURCE_COMMIT \
  --new-revision CANDIDATE_SOURCE_COMMIT \
  --rows 32
```

The live test needs the existing Python Bolt driver and permission to
bind a loopback listener. The pure fixture check needs only Python:

```sh
python3 crates/server/tests/volume_upgrade.py --self-check
```

The self-check deliberately corrupts expected node, edge and ACL results and
confirms exact reconciliation rejects them. It does not open a database.

## CI integration

The CI workflow uses the preceding mainline release tag as the old source and
the checked-out candidate as the new source. It retains a JSON receipt as a
workflow artifact when the test reaches the reporting stage. Fetch sufficient Git history/tags, resolve both to
exact commit IDs, and archive the old source into a fresh temporary directory.
Build its locked workspace, copy the old executable to an immutable path, then
build the candidate and copy its executable to a different path. Pass these
paths and commit IDs to the command above. Preserve the JSON receipt as an
artifact and fail the job on a nonzero exit.

The native/dependency build may be reused through the CI Cargo cache; do not
reuse another job's writable target directory concurrently. Building an old
source can replace `target/debug/brainwire`, so copy it before rebuilding the
candidate. Archive timestamps can predate cached workspace artifacts: force
rebuild the workspace crates in an owned CI target directory after switching
source, retaining only the native/dependency cache. Record the resolved source
commit and actual executable hash; a version string alone does not establish
provenance. A single ordinary candidate binary passed in both positions is a
useful harness smoke test but does not qualify an upgrade between releases.

Run the gate on every proposed release and on changes to storage formats,
catalogs, indexes, ACL encoding or migration code. Record the support window:
passing from the preceding release does not establish compatibility with all
older versions. This script uses no Docker daemon and needs no registry token;
the versioned image still needs its separate retained-volume acceptance test.

## Limits

This fixture is bounded (4–1,000 customer records) and tests clean native
single-node upgrades. It does not demonstrate large-volume migration speed,
crash during migration, rollback, power-loss durability, container recreation,
cross-platform storage portability or replicated-cluster rolling upgrades.
Back up an actual volume and verify restoration before an operational upgrade.
