# Relationship property index design

See [Index relationship properties](../relationship-property-indexes.md)
for activation and Cypher examples.

## Storage and format

An opted-in standalone store creates a separate RocksDB column family
named `relationship_index_v1`. Older writers refuse a graph with this
column family. New binaries detect it when reopening the graph, including
from a checkpoint. Dropping declarations keeps the column family. Quorum
apply, prepare and server startup reject the indexed format.

A forward key contains the type, property name, encoded scalar and full
physical outgoing edge key. Parallel relationships retain distinct entries
even when their property values match. A reverse key maps a property and
edge back to its forward key for updates and deletes. These entries are
independent of relationship identity and keyed `MERGE`.

## Population and writes

The catalog and a durable build job are committed with the declaration.
The worker scans outgoing keys with a durable cursor, at most 1,024
physical keys per transaction. It limits estimated retained entry bytes
per batch and estimated allocation to decode an indexed field to 4 MiB
each. An oversized existing field moves the declaration to a durable failed
state. A separate drop job cleans up old entries in bounded batches.

Writes maintain forward and reverse keys in the same transaction as edge
adjacency. This covers ordinary and keyed `MERGE`, property changes, edge
deletion and both detach deletion paths. The builder records an outgoing
row dependency before indexing it. Writers track reverse keys, including
absent keys, so a builder that commits first can conflict with a concurrent
delete or property removal. A small definition record per type rejects
stale catalog writes. The final build batch publishes readiness atomically.
The offline consistency check verifies the stored key pairs and checks
coverage of representable values in ready indexes.

## Candidate selection

The executor checks for a ready index at an eligible one-type expansion
with a property equality or range conjunct. It probes conservatively and
reevaluates the full predicate, pattern and endpoint visibility. If the
plan uses partitioned anchor scans or the probe cannot safely represent
the predicate, the executor keeps its ordinary access path.

Strings use an escaped, order-preserving encoding of up to 64 KiB of
source bytes. Numeric keys use an ordered float encoding, and converting an
integer to float is monotone. Long strings and neighboring large integers
can share a key. Inclusive byte bounds retain those candidates even for a
strict Cypher inequality, and the exact predicate removes false matches.

Focused tests cover nonfinal backfill interleavings, checkpoint and reopen,
endpoint ACLs, boundary values and compatibility with legacy index
inventory clients.
