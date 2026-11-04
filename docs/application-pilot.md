# Pilot Brainwire with an application

Start with a graph you can rebuild. This checklist helps test an application
against a protected instance before moving any critical workload. It does not
certify production readiness or multi-machine availability.

## 1. Choose the contract

- Pin the [release image and digest](container.md) and record the source
  version, graph name, storage location and resource limits.
- Define node labels, properties, relationship types and stable source IDs in
  an [ontology](ontology-descriptions.md). Check the
  [Cypher subset](local-cypher.md) used by the application.
- Decide which system owns each property and how missing rows and properties
  are reconciled. Use bounded batches and test insert, update, deletion and
  replay with the [Python client](python-client.md).
- For a lakehouse source, the [DuckLake bridge](ducklake-incremental.md) compares
  pinned snapshots and can reconcile missing keys. It is not native CDC; test
  source retention, checkpoint recovery and deletion scope with your own data.
- For distinct events between the same endpoints, choose event nodes or an
  appropriate keyed relationship type before loading the graph. Existing types
  need migration to use keyed relationships; see the [timeline guide](temporal-data.md).

## 2. Start and protect the database

Use the [Docker demo](container.md) with synthetic data to check connectivity.
For shared environments use an [authenticated profile](protected-containers.md),
least-privilege credentials and a [verified encrypted boundary](protected-transport.md).
The demo is unauthenticated. Standard Docker, Kubernetes and Swarm profiles
run one instance; adding pods does not enable replication.

Give that instance a durable, exclusive volume and configure memory and disk
headroom based on your measurements. The [memory settings](memory-defaults.md)
describe what the budgets cover. They are not a process-wide RSS guarantee.
Use an [independent checkpoint](checkpoints.md) and verify a restore before
depending on the data.

## 3. Exercise the application

A first 30-minute trial can cover the steps below with a small dataset. Record
each result and repeat with representative volume and failure scenarios before
increasing the workload. The trial is a compatibility check, not a production
qualification.

1. Load a small representative dataset and compare exact node, relationship,
   property and permission results with the source.
2. Replay the same batch, update and delete records, and confirm that unrelated
   records are preserved. Test errors and size limits with the real client.
3. Restart the instance with the same volume and compare acknowledged records,
   indexes and access rules. Restore a checkpoint into a separate volume and
   compare again.
4. If a connection fails after COMMIT was sent, treat the outcome as unknown.
   Reconcile by stable source identity and checkpoint; do not assume rollback.
   The experimental quorum path also offers a
   [retained request identity](quorum-replication.md) with its own limitations.
5. Run representative ingestion and queries under the intended memory limit,
   collecting latency, errors, process and cgroup memory, disk use and restart
   information. A short synthetic smoke is not a capacity qualification.

Keep a rebuild and rollback path. Test the actual three-voter deployment,
independent volumes, partitions and machine loss separately before depending on
failover. Do not expose credentials, private data or query parameters in shared
diagnostics.
