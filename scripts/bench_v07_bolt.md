# Measure delegated reads over Bolt

This is a **sequential read diagnostic** against an existing, frozen synthetic
fixture. It measures delegated `allow_all` and restricted visibility using the
same service principal. It never loads or modifies graph data, starts/stops a
server, or retries a transaction. It is not the complete concurrent ingestion
and read qualification requested for v0.7.

Prepare a disposable isolated server first, with a read-only service principal
configured as `visibility_delegate: true`. Stamp node ACLs through a separate
trusted writer before freezing the dataset. Keep other clients from changing
this fixture while measuring. The benchmark rejects incomplete setup and uses
explicit Bolt READ transactions with `brainwire_visibility` metadata on every
query and every cursor page. The driver's managed retry mechanism is not used.

The manifest defines the actual schema and expected results. It does not assume
that a particular benchmark dataset exists. For example, a small fixture whose
schema and counts really match this file could use:

```json
{
  "synthetic": true,
  "disposable": true,
  "frozen": true,
  "dataset_id": "synthetic-fixture-001",
  "server_id": "isolated-fixture-server",
  "database": "bench",
  "schema_version": 1,
  "source_revision": "REPLACE_WITH_EXACT_40_CHARACTER_COMMIT",
  "server_binary_sha256": "REPLACE_WITH_EXACT_64_CHARACTER_BINARY_SHA256",
  "schema": {
    "source_label": "Person",
    "target_label": "Car",
    "relationship_type": "OWNS",
    "group_property": "brand",
    "cursor_label": "Person"
  },
  "top_n": 5,
  "expected": {
    "allow_all": {
      "count_pairs": 3,
      "cursor_rows": 3,
      "top_n": [{"group_value": "A", "count": 2}, {"group_value": "B", "count": 1}]
    },
    "restricted": {
      "count_pairs": 1,
      "cursor_rows": 1,
      "top_n": [{"group_value": "A", "count": 1}]
    }
  }
}
```

Replace the identity placeholders and all schema/count values with the fixture's
actual values. The manifest parser requires complete lowercase hexadecimal
identities. The schema version is the nonnegative integer catalog version returned over
Bolt by `CALL db.schema()`, not the string version in an input ontology mapping.
The remote schema version and expected count/top-N/cursor results
are checked; this cannot independently verify the remote process's binary hash.
A wrong declared expected result makes the run fail rather than producing a
successful timing. Group values must be non-null strings, with descending count
and ascending group value for ties. The cursor label must have nonempty string
`entity_id` values; the walk verifies strict order and duplicate avoidance.

Install the same pinned driver used by the repository examples, then provide
connection authentication and restricted claims through the environment:

```sh
python3 -m pip install neo4j==6.3.0
# Set these privately in your shell; do not save credentials in the manifest.
# BRAINWIRE_BENCH_URI=bolt://127.0.0.1:7687
# BRAINWIRE_BENCH_PRINCIPAL=<delegated-reader>
# BRAINWIRE_BENCH_TOKEN=<private-service-token>
# BRAINWIRE_BENCH_RESTRICTED_VISIBILITY=<private-JSON-policy>
export BRAINWIRE_BENCH_CONFIRM_SYNTHETIC=synthetic-fixture-001
python3 scripts/bench_v07_bolt.py \
  --manifest /private/path/synthetic-fixture.json \
  --output /private/tmp/brainwire-v07-bolt-before-001
```

All five environment variables are required. Restricted visibility requires
nonempty principals and disallows `allow_all`/`deny_all` shortcuts; the server
still validates the full policy. Only direct `bolt://` and verified `bolt+s://`
URIs are accepted; use `bolt://` only on a trusted local network. URI-embedded
credentials and self-signed TLS bypass schemes are refused. No connection is
made by unit tests. Run `python3 -m unittest scripts.test_bench_v07_bolt -v` for
the fake-driver harness checks.

The output directory must be new. `receipt.json` records the manifest hash,
caller-supplied server identity, observed schema, exact generated queries,
count/top-N results, one warmup and three timed attempts per query, and every
cursor page's start ID, row count, last ID and elapsed time. Cursor pages default
to **200**; pages are separate read transactions. Raw credentials and visibility
claims are omitted; hashes identify the two visibility policies. Each mode's
`allow_all-cursor.jsonl` or `restricted-cursor.jsonl` is an append-only page
journal, flushed after each successful page. The complete receipt is written at
query/mode boundaries and finalization, avoiding a full rewrite on every page.
`query_seconds` is the sum of page-query timings; `seconds` is wall time including
validation and journaling, and `page_journal_seconds` reports the page-log cost.
Interrupted/failed or
incomplete walks cannot report success, and no million-row extrapolation is
reported as a completed measurement.

On macOS, AC power and the active low-power mode being off must be confirmed
with `pmset -g batt` and `pmset -g`; inactive battery profile settings do not
disqualify an AC-powered run. Other/unverifiable
hosts require explicit `--allow-unqualified-host`, and retain that unqualified
status in their receipts. Host load before/after is recorded. Stop unrelated
builds, tests and workloads before measuring; the helper cannot prove an idle
host. The cursor has an overall default 60-second loop deadline and a per-query
60-second timeout; a page already running can exceed the loop deadline until
that query returns/times out. Set these explicitly when a slow baseline must be
observed for longer.

This diagnostic does not measure un-delegated reads, write throughput, server
RSS, cgroup memory, hard RSS enforcement, or concurrent load/read traffic. Its
latencies include the Python driver, Bolt transfer and transaction commit.
Retain source/build/config provenance and run `db check` separately with the
server stopped after the full workload. Do not treat these timings alone as
production qualification or an acceptance of any v0.7 target.
