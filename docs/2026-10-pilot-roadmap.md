# October 2026 client pilot proposal

Allan's October 3 target: a usable independent graph database by October 31,
with Docker/Kubernetes, direct Python access, individual and batch writes,
multiple independent graphs, a documentation site and a polished TUI.
This is a proposed pilot scope, not a promise of complete Cypher or general
production readiness. Keep implementation and artifacts local until publication
or deployment is explicitly requested.

## Weekly acceptance

| Target | Deliverable | Evidence required |
| --- | --- | --- |
| October 10 | Reproducible single-writer Docker/Compose/Kubernetes deployment | Build a pinned image; test acknowledged writes, forced exit, container/Pod replacement on retained storage, index audit and checkpoint restore; document what happens when storage itself is lost |
| October 17 | Python and query path suitable for the first integration | Runnable individual/UNWIND batch examples, idempotent replay with stable entity_id, multiple graph isolation, rollback/conflict handling, bounded result delivery and representative latency/memory measurements |
| October 24 | Connected TUI and accurate docs site | Select a named graph; inspect bounded neighborhoods, ontology/index status and live memory; run bounded read queries; show understandable errors; preserve green Observatory identity and EN/PT-BR; documentation examples execute against the release candidate |
| October 31 | client pilot candidate | Run a representative synthetic workload and a sustained mixed-load test; inject failures during ingestion; restore a checkpoint; package a reproducible local deployment and record known limits, resource sizing and rollback procedure |

Dependencies matter: live TUI queries need bounded server results, and deployment
acceptance needs the final image. Move optional visual polish before moving a
correctness or recovery gate. If a gate fails, narrow the pilot or move the date;
do not label an unverified path supported.

## What usable means

- The database runs independently of client-platform. Python selects a named
  graph and can create/update one entity or submit bounded batches through Cypher.
- Repeating a batch with stable identities has a documented result. Partial
  failures, conflict retries and outcome-unknown connection failures are distinct.
- Confirmed commits survive tested process/container/Pod failures while the same
  durable storage remains available. Uncommitted work disappears. Graph/index
  audits and restoring a native checkpoint are executable procedures.
- The TUI presents real selected-graph data and identifies configured budgets,
  estimates and measured usage. It does not present synthetic previews as live
  data. Start connected operations with bounded reads; administration retains
  explicit graph scope and safe offline CLI paths where needed.
- The docs site includes quickstart, Python examples, the supported Cypher subset,
  ontology and index behavior, memory semantics, deployment, backup/restore and
  troubleshooting. The existing untracked `docs-site/` is a concept draft with
  future syntax; reconcile it with actual capabilities before publication.
- The pilot has recorded acceptance results, known limits and an operator who
  can diagnose, restart and restore it using only the documentation.

## Separate releases or explicit scope decisions

Kubernetes scheduling is not graph replication. This pilot assumes one writer
per RocksDB directory. Multi-machine replication, automatic failover, quorum,
fencing and recovery after losing the only disk require another implementation
and failure campaign. Do not increase replicas over independent stores and call
it a distributed database.

Complete Cypher, vector ANN/full-text indexes, temporal ticket revisions,
automatic retention/ontology cleanup and DuckLake/Arrow incremental ingestion
are separate capabilities, not implied by the pilot label. For exposure beyond
a trusted isolated environment, authentication, authorization, transport security,
upgrade compatibility and recovery objectives also need explicit acceptance.

The [October 3 measurements](benchmarks/2026-10-03-streaming-pool-deadlines.md)
record the current baseline. One million synthetic nodes and short concurrency
samples establish useful checks, not sustained production capacity.

## November extension, proposed October 3

Allan also asked what could be available by November 30 beyond MCP. Target a
closed beta only if the October pilot gates pass and development continues at a
similar cadence. This is a prioritization proposal, not an approved promise to
complete every item by that date.

1. Incremental ingestion, prioritizing DuckLake/DuckDB/Parquet: durable source
   checkpoints, bounded batches, resumability, stable identities and idempotent
   replay. Measure Arrow against the existing Python/Bolt path before adopting
   a separate transport.
2. Query maturity: bounded result delivery, cancellation, improved access-path
   selection and sustained mixed read/write workloads. Publish latency, memory,
   failure and recovery results with reproducible datasets.
3. Operational boundaries for external users: authentication, per-graph access,
   transport security, live metrics, backup/restore and documented upgrades.
4. A connected TUI and documentation covering supported operations accurately,
   including understandable errors and the distinction between configured
   budgets, estimates and measured process usage.
5. A first ticket-history/retention slice after the foundations above: explicit
   revision semantics, bounded retention and auditable maintenance. Ontology
   changes must not silently delete previously stored records.

MCP should adapt the same supported APIs and permission model, with bounded
results and explicit mutation scope. It does not replace the Python driver or
create a second storage/query implementation. Vector search and multi-machine
replication remain explicit competing priorities; do not promise both plus all
beta gates in the same month. Prefer measured client requirements when choosing
which large capability follows reliable ingestion and operation.

## December extension, proposed October 3

Allan asked what could follow by December 31. Aim for a first commercially
supported release with a narrow published scope, conditional on the pilot and
closed beta demonstrating stability. A self-hosted database plus support has a
smaller operational scope than simultaneously building a fully managed SaaS.

The desired release includes independent Docker/Compose/Kubernetes installation,
multiple graphs, Python and MCP over the same APIs, individual/batch writes,
incremental client ingestion, measured query/index behavior, basic history and
retention, a connected polished TUI, versioned documentation, authentication,
graph isolation, metrics, backup/restore and a documented upgrade path.

The commercial gate is evidence: representative sustained workloads, fault
injection, successful restore and upgrade/recovery procedures, and resolution
of beta correctness issues. Record supported volumes and limits from actual
tests rather than promising generic billion-element capacity. Do not sell
replication or automatic failover as supported just because Kubernetes packaging
exists. A first distributed implementation could be explored, but it needs its
own acceptance campaign before inclusion in the supported offer.

Proposed progression: October pilot, November closed beta, December scoped
commercial release. Stability determines each transition; dates alone do not.
