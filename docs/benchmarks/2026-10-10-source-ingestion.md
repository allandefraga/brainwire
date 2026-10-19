# external application Ticket/Comment snapshot acceptance — October 10

The [shadow adapter](../source-ingestion.md) imports fixed-schema synthetic
Zendesk-shaped Ticket and TicketComment records through the existing Python/Bolt
transaction engine. It adds source-qualified identities, HAS_COMMENT parent
validation and a mapping contract checked alongside immutable source SHA/cursor.
No storage format or Rust engine behavior is changed.

## Recorded runs

| Run | Tickets | Comments | HAS_COMMENT edges | Import seconds | Input records/s |
| --- | ---: | ---: | ---: | ---: | ---: |
| Driver 5.28.6, first basic acceptance | 10,000 | 20,000 | 20,000 | 2.454 | 12,227 |
| Driver 5.28.6, expanded acceptance | 100,000 | 200,000 | 200,000 | 25.101 | 11,952 |
| Driver 6.3.0, expanded + partial-resume acceptance | 1,000 | 2,000 | 2,000 | 0.277 | 10,832 |

Raw receipts: [10K](2026-10-10-source-ingestion/10000.json),
[100K](2026-10-10-source-ingestion/100000.json),
[driver 6](2026-10-10-source-ingestion/driver6-1000.json).
The initial smaller run preceded the extra overwrite/parent/guard cases;
each receipt lists the cases actually executed. Later source additions do not
retroactively add tests to the earlier run.

The 100K-ticket run has **300K business nodes** plus import metadata; it does not
have 1M edges. Its input is 260,304,455 JSONL bytes. Each comment has 1024 ASCII
characters of highly compressible synthetic text; tickets are smaller. Two
scalar indexes per business type are active. This is not 1KiB for every node,
a vector benchmark or a realistic document-compression profile.

Method: canonical release binary built with locked offline dependencies on
Apple M5 / 32GiB macOS ARM64. Receipts record the binary SHA256. One importer,
250 input rows / <=1MiB source bytes per batch / <=64KiB per line; 64MiB RocksDB
cache, 16MiB memtable budget, 64MiB query guard. Timing excludes fixture generation,
server startup, later query validation, crash and restore. It includes source
hashing, Python conversion, Bolt and graph/cursor commits. Throughput divides
business input records by elapsed import time; it is not total graph mutations/s.

The host was not isolated: a user virtualization process was observed around
253% CPU during the session and was left untouched. This is a single short local
run at each size, not a pinned 4-vCPU/8GiB profile, a sustained benchmark, p95
latency measurement or production guarantee. No peak RSS/allocation measurement
is claimed. Authentication, replication, history, CDC and ANN remain unsupported.

## Correctness and recovery

Expanded runs verify:

- Exact Ticket/Comment/edge and indexed status/priority counts with provider IDs
  deliberately reused across two synthetic tenants.
- Finished-source replay has no new commit; a different mapping at EOF is rejected.
- A separate graph proves authoritative SET, null removal, old scalar-index cleanup,
  duplicate-edge replay, missing-ticket and existing/within-batch parent rejection.
- A callback failure after node/edge staging rolls back the whole current batch and
  its checkpoint. Allocator reservation counters may legitimately survive rollback.
- SIGKILL while a bounded property update and source-cursor rewind are pending;
  reopening preserves the previous data/cursor/counter and replay remains a no-op.
- Consistent native checkpoint restore into a fresh graph, exact receipt/counter,
  replay against restored progress and full graph/scalar-index audits.

The driver-6 run also rejects changed content for a reused source ID and resumes
a partial source after a wrapper deliberately drops the receipt **after the real
first batch commits**. The final node/edge counts show no duplicate replay.
The [existing Customer acceptance](2026-10-10-source-ingestion/legacy-customers.json)
also passes with 1000 records after the shared-helper extension, including
before-commit rollback, lost acknowledgement/SIGKILL and completed CLI replay.
Thirteen importer and three adapter algorithm tests pass; 438 Rust workspace tests,
workspace all-targets Clippy and rustfmt pass.

A separate [old-catalog check](2026-10-10-source-ingestion/old-catalog.json) uses an actual
Customer ontology without `contract_id` and an existing checkpoint lacking that
property. Resume, completed replay, a new default source and offline index audit
pass through real Bolt. Reproduce from the repository root:

```sh
PYTHONPATH=. uv run --no-project --with neo4j==5.28.6 python \
  docs/benchmarks/2026-10-10-source-ingestion/old-catalog-acceptance.py
```

[Validation inventory](2026-10-10-source-ingestion/validation.json) records final source
hashes and checks. Earlier benchmark receipts retain their original case lists.
The [final 1K check](2026-10-10-source-ingestion/final-1000.json) reruns the complete
driver-6 harness after adding explicit success/index-scope assertions for all
three offline audits; it is a verification rerun, not a new throughput target.

## Query guard finding

An initial 100K-ticket harness tried one eager SET over all 33,334 open tickets
and hit `MemoryLimitExceeded` at the unchanged 64MiB query guard. This is an
explicit supported-limit finding, not hidden by increasing the budget. The expanded
harness records that rejection, verifies transaction rollback and unchanged data,
then uses a primary-key bounded pending write for the crash test. Large imports
remain bounded batches; a successful import does not imply unlimited mass updates.

## Reproduce

```sh
cargo build --release --locked --offline -p brainwire-server --bin brainwire
uv run --no-project --with neo4j==5.28.6 python \
  docs/benchmarks/2026-10-10-source-ingestion/acceptance.py --tickets 100000
uv run --no-project --with neo4j==6.3.0 python \
  docs/benchmarks/2026-10-10-source-ingestion/acceptance.py --tickets 1000
```

The harness creates only temporary synthetic input/stores, stops every server it
starts and removes data on success or failure. Defunct-connection logs during the
intentional SIGKILL are expected; they do not replace result assertions. New
snapshot IDs must be applied in order without overlapping writers for a given
tenant/connector; this example has no source-version arbitration or edge repair.
