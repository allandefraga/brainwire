# Isolated Swarm restart acceptance

`crates/server/tests/swarm_restart.py` rehearses a protected, single-owner
Brainwire service using the actual `deploy/swarm.yaml` and
`deploy/swarm.auth.yaml`. It creates a disposable Docker-in-Docker daemon rather
than initializing Swarm on Docker Desktop's existing daemon. The host Swarm must
start inactive, and its local state, node ID and control availability must remain
unchanged after cleanup.

This is a runtime rehearsal for one Swarm engine and one retained local volume.
It does not qualify replication, movement between machines, disk-loss recovery,
quorum, distributed failover or a production Swarm cluster.

## Run

Use a locally built, frozen candidate image. The script never builds, pushes or
chooses a Brainwire candidate. Its standard-library self-check needs neither
Docker nor the Neo4j package:

```sh
python3 crates/server/tests/swarm_restart.py --self-check
```

After the candidate image is available locally:

```sh
uv run --no-project --with neo4j==6.3.0 python \
  crates/server/tests/swarm_restart.py \
  --image brainwire:YOUR-FROZEN-CANDIDATE --rows 1000
```

The runtime requires Docker and permission to launch a privileged container.
It pulls `docker:29.7.2-dind`, records its resolved image ID and repository digests,
and transfers the existing Brainwire image with `docker save`, `docker cp` and
an inner `docker load`. The host and inner Brainwire image IDs must match.
It publishes only a random loopback Bolt port; neither a host Docker socket nor
a Docker daemon port is exposed to the outer container.

## Checks

- The effective Swarm service has one replica, a constraint to the inner node's
  exact ID, stop-first updates and the retained external data volume.
- External service-token secrets use the repository authentication overlay.
  Private temporary credentials are never printed. The health principal makes
  the inherited readiness probe succeed; the reader cannot mutate data.
- Bounded batches commit synthetic indexed Customer records. An explicit
  transaction leaves a CREATE and indexed overwrite pending while a separate
  reader continues to see only committed data.
- SIGKILL removes the current task. Swarm schedules a replacement on the same
  owner and volume. Acknowledged records and old index values survive; pending
  records and the pending index update disappear.
- Scaling the service to zero removes the owner before offline access. The
  native audit passes, a new `/data/restored` checkpoint is created, and that
  restored graph passes an independent audit and count query. The original
  protected service resumes and still contains its acknowledged records.
- The owned outer daemon and its two named data/certificate volumes are removed
  in `finally`, and host Swarm state is checked again. No user stack or volume is
  selected for removal.

The receipt records image identities, manifest hashes, task IDs, the inner owner,
record count, native audits and cleanup. `result: PASS` is emitted only after the
runtime checks and cleanup complete. A self-check PASS is explicitly marked
`runtime_exercised: false` and cannot substitute for runtime acceptance.

## Boundary

Docker-in-Docker runs privileged and shares the Docker host's kernel. On Docker
Desktop that is the Linux VM kernel; this is not a security sandbox or evidence
that arbitrary untrusted images are safe. Use the known official daemon image
and a trusted local Brainwire candidate.

The published Bolt endpoint is loopback plaintext with service-token auth; this
rehearsal does not test TLS. The checkpoint remains on the same local volume and
uses the same candidate version. A lost owner or lost disk therefore needs a
separate backup/restore plan. The native engine uses abrupt-stop recovery here;
a task restart is not a claim that active queries drain gracefully.
