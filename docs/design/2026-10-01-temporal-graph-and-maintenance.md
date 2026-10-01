# Temporal graph, weights and maintenance contract

This design extends the [data and recovery design](2026-09-27-brainwire-data-and-recovery.md)
without claiming that the features are implemented. The current catalog has
versioned ontology definitions and timeline stream metadata, but graph writes
store only the latest node and edge rows. There is no queryable entity revision
history, temporal index, retention worker or ontology-driven garbage collector.
`DETACH DELETE` removes the current node and its mirrored edges. Reapplying an
ontology retains prior types and columns so old rows remain decodable; it does
not remove data whose declaration disappeared.

## Separate clocks and versions

- **Catalog version** describes the schema used to decode a row. It is not the
  history of a ticket or other entity.
- **Entity revision** increases monotonically for one stable node ID whenever
  its meaningful properties or state change. A revision records the complete
  post-change image initially; delta compression can follow measurement.
- **Recorded time** is when Brainwire committed a revision. It is assigned by
  the database, with a durable monotonic commit sequence to break clock ties
  across process restarts. The current in-memory conflict sequence is not that
  durable sequence.
- **Valid time** is when the fact applied in the source domain. A late Jira
  update can be recorded today while saying a ticket entered `In Progress`
  yesterday. Valid time comes from the writer and must never overwrite
  recorded time. An absent valid time defaults to recorded time.

The default `MATCH` sees the current row. A separate history operation returns
ordered revisions with revision ID, recorded time, valid interval, source and
the changed properties. An as-of query must state whether it means *known at*
(recorded time) or *true at* (valid time); otherwise historical answers are
ambiguous. Backdated corrections may split valid intervals. All revisions and
the current row must commit atomically, so crash recovery cannot expose a new
current value without its history. Idempotent retries use a source event ID.

For a Ticket, the ontology can request `history.enabled: true` and a retention
period such as `P6M` for revisions. The current ticket remains while it is
active; expiring an old revision must not remove the current ticket. Retention
starts at recorded time and is evaluated in UTC. Calendar months have variable
length, so the policy must define calendar arithmetic explicitly. Legal holds,
backup retention and deletion requests override ordinary expiration according
to the applicable policy. Historical edge changes require their own revisions
if an as-of traversal is promised; node history alone cannot reconstruct an
old graph neighborhood.

A possible future mapping extension (not implemented by the current catalog) is:

```json
{
  "version": "3",
  "nodes": [{
    "label": "Ticket",
    "history": { "enabled": true, "retention": "P6M" },
    "weights": [{ "property": "priority_score", "meaning": "importance", "default": 1.0 }]
  }],
  "edges": [{
    "type": "BLOCKS",
    "from": { "label": "Ticket" },
    "to": { "label": "Ticket" },
    "history": { "enabled": true, "retention": "P6M" },
    "weights": [{ "property": "impact", "meaning": "strength", "default": 1.0 }]
  }]
}
```

The first implementation needs explicit validation of the retention format,
weight units and allowed values. A missing policy means no automatic history
or expiry. Today Serde may ignore those proposed fields; until the policy
parser exists, the CLI must not suggest that such a declaration is active.
The future parser must reject unknown policy fields rather than silently
creating an unprotected graph.

## Weights

An edge already supports a numeric `weight` property in ephemeral sparse CSR
projections and weighted PageRank. Missing values default to 1; explicit
weights must be finite, nonnegative and representable as `f32`. This is an
algorithm input, not a universal edge weight. Different algorithms may use
different properties or normalization, and a distance cost must not silently
be treated as a PageRank strength.

A node weight should likewise be an ordinary typed numeric property selected
by a particular algorithm or ranking query, such as Ticket priority or
Company importance. The ontology can declare its meaning, units, default and
allowed range. It must not add a mandatory weight to every node. Keep raw
source values separate from derived scores; record the score algorithm and
version if scores are persisted. A temporal query uses the weight at the
chosen entity revision, never today's weight on an older graph snapshot.

## Ontology lifecycle and maintenance

Removing a label, edge signature, property, index or retention policy from an
ontology must not immediately delete stored data. The first transition is
`active -> deprecated`: new writes may be refused according to policy, while
old rows, revisions and catalog versions remain readable. A maintenance plan
then reports affected current nodes, edges, revisions, index entries and bytes
as estimates. It also reports incoming references, pending legal holds and
whether backups still contain the data. Execution requires an explicit,
auditable job scoped to a type and a catalog version, never an implicit side
effect of `apply-ontology`.

Jobs are resumable and bounded: persist a job ID, policy snapshot, key cursor,
counts and errors; process small transactional batches; throttle against
normal queries and compaction pressure; and expose dry-run, progress, pause and
resume. A crash can replay a batch without double-deleting. Node cleanup
removes current row, primary and secondary keys, both mirrors of each edge,
history and event records according to policy. Edge-type cleanup removes both
mirrors and historical edge revisions. Every batch checks graph invariants.
Retention jobs may expire old revisions while keeping current entities.

Logical deletion is not proof of physical erasure in an LSM store. A purge
request tracks tombstones, compaction, snapshots, WAL, checkpoints, replicas,
object storage and backups before making any physical-erasure claim. Backup
retention and legal obligations need their own policy. An operator should be
able to inspect why a record was retained or removed.

## Implementation order and acceptance gates

1. Add explicit, validated ontology policy fields for node/edge history,
   retention and weight semantics. Keep existing catalogs readable through
   serde defaults. Reject conflicting policies for duplicate node labels.
2. Add an append-only revision family and a per-entity revision counter. Commit
   revision, current row and index changes in one transaction. Test update,
   delete, rollback, concurrent writers, backdated events and kill/reopen.
3. Expose history and as-of reads with a clear clock choice. Add an indexed
   access path by node ID and time; test equal timestamps and time boundaries.
   A historical traversal is not supported until edge revisions are indexed.
4. Add an offline dry-run maintenance planner, then resumable online jobs for
   retention and deprecated ontology types. Test stop/restart in every phase,
   full graph invariants and exact retained/deleted sets before enabling
   automated schedules.
5. Measure write amplification, revision growth and compaction impact on a
   large ticket workload. Retention must be tested with clock control and real
   calendar boundaries, not a short sleep-based TTL test.

The physical secondary index and streaming-query work in the
[query-scale design](2026-10-01-secondary-index.md) remain prerequisites for
large historical scans and bounded maintenance jobs.
