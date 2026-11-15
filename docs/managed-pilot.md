# Plan a managed Brainwire pilot

Use four weeks to qualify a small, invite-only pilot. Start with one standalone
Brainwire instance per customer, a pinned container image, and a graph the
customer can rebuild from its source. Offer deployment, backup and recovery,
and help with the first workload. Agree on capacity limits, maintenance
windows, and support hours with each participant before onboarding.

Brainwire already provides [protected container profiles](protected-containers.md),
[Bolt authentication and TLS](protected-transport.md), [health probes](serve.md#health-checks),
and [graph checkpoints](checkpoints.md). The pilot still needs a tested
provisioning and recovery procedure, alert routing, and a measured workload
envelope on the infrastructure you choose. A standalone instance has downtime
during upgrades and outages. Keep [experimental Raft](quorum-replication.md)
out of the pilot's availability and recovery commitments.

## Week 1 · Provision one isolated instance

- Pick one deployment target. Use the protected Compose profile on a dedicated
  host or the [protected Kubernetes profile](orchestrators.md) with a separate
  namespace, workload, PVC and network policy for each customer. Check that your platform
  enforces the network policy. Give each customer an exclusive persistent data
  volume, independent backup credentials and storage, and a scoped Bolt endpoint.
- Record the image digest, graph name, volume, resource limits and owner. Keep
  provisioning configuration repeatable and store secrets separately. Initialize
  the default graph on the empty volume before starting the server. Grant the
  readiness principal access to that graph, scope application credentials to
  the workload, and keep instance administration separate. Verify encryption
  and server identity from the actual client.
- Use a synthetic graph to check that the client can write and query, and that
  invalid credentials, untrusted certificates and access from another
  customer's network or credentials fail. Recreate the workload, keep its
  volume, and confirm the data and indexes reopen.

## Week 2 · Verify backups and failure recovery

- Set a checkpoint schedule and retention policy. Copy each checkpoint to
  independent storage, record its commit sequence, and verify the copy. Back
  up the protected deployment configuration separately and keep access to the
  policy, private material and backups scoped to approved operators. Review
  the [object storage helper's limits](object-storage-backup.md) if you use it.
- Restore a checkpoint into a fresh volume with compatible software. Run
  `brainwire db check` offline, then compare representative queries, index
  lookups and authorized and denied reads with the original instance. Record
  how old the recovered data is and how long the restore takes.
- Kill the process during a write in the test environment. Confirm that
  acknowledged writes survive and that an interrupted commit can be
  reconciled by source identity before replay. Exercise volume loss through
  the restore procedure. [Durable commit state](commit-state.md)

## Week 3 · Measure capacity and rehearse an upgrade

- Run representative reads and imports concurrently with the intended resource
  limits. Record query and write latency, errors and memory refusals,
  process and container memory, disk growth, and restarts. Publish the tested
  envelope for resource limits, batch sizes and concurrent clients. Use
  [memory settings](memory-defaults.md) to tune the workload, and monitor
  memory and disk from the host or container platform.
- Alert an operator on readiness failures, restarts, sustained memory pressure,
  low disk space and overdue or failed backups. Include a synthetic authenticated
  query from the client path. Exercise the alerts and keep secrets, query
  parameters and customer data out of shared diagnostics.
- Rehearse upgrades from the previous pinned image against a restored copy.
  Verify readiness, representative reads and writes, permissions and indexes.
  For the real upgrade, schedule a write pause, record and verify the final
  backup, and keep it until validation passes. If the new image fails, restore
  into a fresh volume for the previous image. Confirm how writes accepted
  after cutover would be recovered before reopening writes.

## Week 4 · Onboard one customer

- Start with a bounded, rebuildable workload. Run the
  [application pilot](application-pilot.md) with the customer's data shape and
  client before moving the full workload. Agree on the backup schedule and
  measured recovery expectations. Define how the customer can export its
  graph and how its data and backups are retired at the end of the pilot.
- Proceed only after a second operator can repeat provisioning, verify
  isolation, recover from backup, and rehearse an upgrade using the same image
  and infrastructure proposed for the pilot. Keep the results alongside the
  runbook. Fix any failed check before expanding the pilot.
- Offer scheduled maintenance and a named support contact. Use the pilot
  results to decide which recovery and availability commitments can be offered
  later. Track larger failure domains separately in the
  [production readiness roadmap](production-readiness-roadmap.md).
