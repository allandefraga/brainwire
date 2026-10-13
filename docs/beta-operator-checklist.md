# Single-node shadow beta operator checklist

Use this checklist with the [protected quickstart](mvp-quickstart.md),
[October qualification](benchmarks/2026-10-13-beta.md) and
[remaining gates](2026-10-pilot-roadmap.md). It is an operator rehearsal for a
rebuildable shadow graph, not authorization to replace client's production
store. Every live-source/deployment step needs an explicit selected environment.

## Before starting

1. Record the exact native binary or Linux image digest and configuration sources.
   `brainwire config show` reports values and their provenance. Preserve this
   receipt with the chosen ontology and Python mapping contract.
2. Allocate a new data directory/retained volume with exactly one active owner.
   Multiple named graphs share process resources. Do not put two servers against
   one RocksDB directory, or add replicas to manufacture database failover.
3. Generate private service-token files using the quickstart. Keep reader, writer,
   health and optional observer privileges distinct. Memory requires instance
   administration; graph read grants do not imply access to process telemetry.
4. Choose trusted same-host loopback or the verified single-host TLS proxy.
   For Python TLS, verify both CA and hostname with `bolt+s`; do not disable
   verification. Native TUI/probe clients use plaintext on the trusted side.
5. Freeze source IDs, authoritative fields, null/overwrite semantics, ontology and
   source snapshot identity before importing. For DuckLake use an explicit version
   and existing retained local managed files. The bridge provides snapshot ingest,
   not CDC, delete propagation or retention of ticket revisions.
6. Set explicit encoded/decoded/query/cache/memtable/IO limits for the workload.
   The tested quickstart uses encoded 8 MiB, decoded 16 MiB, query/cache 64 MiB,
   memtables 16 MiB and authenticated network IO 300 seconds. These are independent
   accounting limits, not a 144 MiB process RSS ceiling. Container memory and disk
   capacity require their own selected resource envelope.

## While running

- Import through the existing Python/Bolt path. Use bounded batches and the durable
  source digest/mapping cursor; the cursor and each data batch commit together.
  On connection uncertainty, reopen and inspect the durable cursor before resuming.
  Do not assume an absent acknowledgement means a batch failed to commit.
- Python producers use `liveness_check_timeout=30`. The TLS proxy has a 60-second
  inactivity boundary; native complete-request IO defaults to 300 seconds.
  A pool with no liveness check can fail on its first reuse after expiry.
- Inspect `brainwire shell --connect ... --graph ...` and provide the four typed
  Graph selection arguments shown in the quickstart. Use Tab/1–4 and `r` refresh.
  A stale pane preserves its previous sample and displays the error; it is not
  evidence of current readiness. Refresh is synchronous and can block until its
  request deadline.

| Surface | Supported action | Interpretation |
| --- | --- | --- |
| Ontology | Inspect selected types/properties/index declarations and signatures | Server schema snapshot; does not edit ontology |
| Memory | Inspect limit/reserved/peak/rejections with EN/PT explanation | Shared query reservation accounting; excludes total RSS/cache/process RAM |
| Indexes | Inspect declaration/readiness, select/scroll and refresh | Current server snapshot; does not create/rebuild/drop indexes |
| Graph | Select/scroll a typed node's outgoing neighbor sample | At most 20 rows plus further-row marker; not full degree, properties or graph expansion |
| Offline overview/disk | Inspect real named stores and disk metadata after stopping the server | Exclusive local store access; separate from connected workspace |
| Cypher/Python | Individual/batch writes, merge/overwrite and supported queries/analytics | Actual engine APIs; no TUI query editor or administrative mutation workflow |

Record errors, source progress, representative p50/p95/p99, payload/degree shape,
throughput, query reservations, sampled RSS and disk growth. Do not compare a small
scalar result with a full-property read or a cold multi-hop traversal.

## Stop, recover and restore rehearsal

1. Stop ingestion and record its last acknowledged receipt. The server currently
   has no graceful drain or immediate cancellation of blocking engine work.
2. Stop the owned server and wait for its process to exit before issuing offline
   commands. SIGTERM is abrupt; SIGKILL is a separate forced-failure test. Current
   synthetic qualification proves acknowledged indexed writes retained and pending
   transactions discarded on restart; it does not locate an exact COMMIT phase.
3. Run `brainwire db check <graph> --json` with the same stopped-store config.
   Require status `ok` and retain the receipt; never bypass a failed audit because
   a health probe connects successfully. Offline catalog opening discovers named
   stores eagerly: one corrupted neighboring store can block an audit of another.
   Namespace/credential isolation does not imply independent corruption failure
   domains. Preserve the failed store and recover a verified copy rather than
   treating it as an empty database.
4. Create a native checkpoint into a fresh directory with the documented
   [checkpoint commands](checkpoints.md). Preserve ontology, input and cursor
   provenance alongside it. Checkpoints are local store copies, not object-storage
   manifests or coordinated multi-graph/distributed snapshots.
5. Reopen the retained original and independently restore the checkpoint into a
   new named store. Verify expected counts, selective indexed answers, source
   cursor and no-write completed replay. Audit both while stopped.
6. For operator rollback, recover a separately verified checkpoint/build pair.
   Never assume an older binary can reopen a changed on-disk format without an
   upgrade/rollback compatibility check. Current tests are same-candidate restore.

Machine/volume loss, power-loss durability of source filename publication,
whole-cluster loss, quorum replication and cross-version upgrades need separate
acceptance. A process restart on an intact local volume does not prove those cases.

## Release decision

Require the final candidate receipts, successful source/query contract, restore
rehearsal, selected security boundary and measured resource envelope before the
shadow beta. The comprehensive local verifier must reject missing or mismatched
receipts; its PASS means only its declared synthetic scope passed.

Keep production deployment conditional on actual client source/backend access,
queries/SLOs, the selected protected runtime target and upgrade/operator readiness.
Raft, ANN/text search, ticket history/retention, object-storage recovery and fully
connected administrative UI remain separate requested workstreams.
