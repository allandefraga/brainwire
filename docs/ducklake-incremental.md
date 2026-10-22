# Incremental graph updates from pinned DuckLake snapshots

`examples/source_ingestion/ducklake_incremental.py` compares two explicit versions of a
local SQLite-backed DuckLake source. It emits authoritative upserts and physical
deletions using the existing external application ticket/comment mapping and stable scoped
entity IDs. It reads both complete snapshots; this is a snapshot-difference
bridge, not native DuckLake CDC or a live source subscription.

The source must retain both catalog versions and their managed data files until
export finishes. Only cached DuckLake/SQLite extensions are loaded; the example
never INSTALLs extensions. Postgres catalogs, object-storage deployment and the
actual external application source are not qualified by these synthetic tests.

## Immutable artifact contract

Every JSONL row carries `contract_id=external-ducklake-diff-v1`, a caller-stable
`source_id`, `source_identity`, ordered `from_version`/`to_version`, canonical
logical digests for both source snapshots and the expected change count.
`source_identity` binds the absolute catalog path, lexical data path and exact
trusted SQL. Keep that identity and the query's logical scope stable across
versions. Renaming a catalog or changing SQL requires an explicit new baseline;
this example does not infer source identity across moves.

A begin boundary precedes all changes, followed by this deterministic order:

1. Removed comments, then removed tickets.
2. Inserted/changed tickets, then inserted/changed comments.
3. An end boundary, even for an empty difference.

Upserts contain a complete normalized snapshot row: null authoritatively removes
properties. Deletes contain the record type and scoped tenant/connector/external
identity. The source is rejected before publication if IDs repeat, comments have
missing ticket parents or an existing comment changes its parent. A scratch
SQLite index holds the complete comparison on disk; Python does not retain both
snapshots as dictionaries.

Exports cap each snapshot at 100,000 rows by default and maximum, each source at
1 GiB, the changes artifact at 1 GiB and individual lines at 64 KiB. Fetches use
250 rows. Function arguments can lower these bounds. These bound files/fetched
rows, not DuckDB SQL-engine memory, individual source values before serialization
or total scratch-disk overhead. Metadata makes change rows larger than their
snapshot rows; near-limit rows can be refused rather than partially published.

The destination must not exist. Only after reading and validating both pinned
snapshots does the exporter fsync a private mode-0600 file and link it exclusively
into place. Publication refuses overwrite. Directory fsync and power-loss
publication guarantees are not provided; an uncatchable process death can leave
a private temporary file. The JSON receipt records artifact bytes/SHA-256,
source versions/digests, counts and DuckDB/DuckLake versions.

## Register a verified baseline, then ingest

First export the old pinned version using the existing snapshot bridge and
import it completely through `ingest_snapshot`. Registration requires the actual
baseline file and its existing `ImportCheckpoint`: SHA-256, mapping contract
`external-zendesk-snapshot-v1` and durable byte offset must exactly match the
complete file. Its canonical normalized contents must also match the delta's
from-snapshot digest. Registration cannot merely assert that a baseline was
applied.

```sh
uv run --no-project --with duckdb==1.5.5 python -m examples.source_ingestion.ducklake_incremental export \
  --catalog /absolute/catalog.sqlite --data-path /absolute/managed-data \
  --query-file examples/source_ingestion/export_ducklake_snapshot.sql \
  --source-id tenant-zendesk --from-version 12 --to-version 18 \
  --output /absolute/private/changes-12-18.jsonl

export BRAINWIRE_BOLT_AUTH_FILE=/absolute/private/writer.json
uv run --no-project --with neo4j==6.3.0 python -m examples.source_ingestion.ducklake_incremental register \
  /absolute/private/changes-12-18.jsonl --database support-demo \
  --baseline-file /absolute/private/snapshot-12.jsonl --baseline-source-id baseline-12
uv run --no-project --with neo4j==6.3.0 python -m examples.source_ingestion.ducklake_incremental ingest \
  /absolute/private/changes-12-18.jsonl --database support-demo --batch-size 250 \
  --max-batches 4
```

`--max-batches` stops between committed batches and prints the durable
byte cursor as `paused`. Rerun the same immutable artifact to resume; omit the
limit to finish. `complete` means the cursor reached the artifact size. The
source head remains pinned to the artifact while it is partial, so a competing
or out-of-order artifact is still rejected.

The SQL uses `source_lake.main` and must select the complete source scope, with
all tickets before comments, as required by the existing exporter. The baseline
uses the same scope; a selective query can only synchronize that selected view.
See [snapshot ingestion](source-ingestion.md) for creating the graph and loading
the first version. The module also exposes `export_incremental`,
`initialize_source` and `ingest_incremental` for Python callers.

## Durable order, replay and deletion boundary

A separate source head in `ImportCheckpoint` stores current version, logical
snapshot digest, identity and an active-artifact digest. Every mutation batch
checks that head. BEGIN claims the artifact; END advances the version in the
same graph transaction as its durable byte cursor. Competing/incomplete or
out-of-order artifacts are refused. A completed artifact replays as a no-op via
its already-complete cursor, even if later versions have subsequently advanced.
Empty deltas still advance the source head through their two boundaries.

The adapter reuses `resumable_customers.ingest` for immutable-file hashing and
atomic data/cursor writes; authoritative upserts reuse `ingest_snapshot.write_batch`.
A failed batch rolls back its changes and cursor. A lost acknowledgement after
commit is resolved by reading the durable cursor again. Import is batch-atomic,
not atomic across the entire source transition: readers can observe partial
progress between committed batches, while `active_artifact` identifies the
unfinished transition. Finish/resume that artifact before starting another.

Run exactly one importer per source and serialize overlapping tenant/connector
scopes. This example has no source lease or distributed scheduler. Other writers
must not alter the source head or mutate source-owned nodes independently; the
baseline checkpoint proves completion, not that unrelated writers preserved its
contents indefinitely.

Deletes use typed `MATCH ... DETACH DELETE`, which removes every incident edge
on the deleted source-owned node. Use an exclusively owned shadow graph or an
explicitly accepted ownership boundary. This is physical deletion, not ticket
history, retention, tombstone preservation or selective edge ownership.

## Synthetic qualification

```sh
uv run --no-project --with duckdb==1.5.5 --with neo4j==6.3.0 \
  python -m unittest examples.source_ingestion.test_ducklake_incremental -v
```

Tests use cached real DuckLake managed Parquet snapshots and private authenticated
native server processes. They cover ordered inserts/updates/deletions, null
removal and scalar-index changes, edge-mirror audit after deletion, replay,
rollback/resume after staged mutations, a lost committed acknowledgement, empty
differences, wrong/incomplete baselines, stale heads, competing artifacts,
malformed files, duplicate IDs, reparenting/dangling comments and export bounds.
Missing optional dependencies/extensions or a native binary cause the relevant
tests to skip; a skipped run is not runtime qualification. Set
`BRAINWIRE_INCREMENTAL_BINARY` to select the candidate executable. All fixture
servers, credentials, stores and files are owned temporary resources.
