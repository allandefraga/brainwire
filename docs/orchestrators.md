# Docker, Kubernetes and Swarm test deployments

The templates run one durable server with multiple named graphs. Replication
is not implemented: increasing replicas creates independent stores. Keep one
writer per RocksDB directory. Use a trusted isolated test network because the
development server is unprotected unless the optional
[service-token policy](bolt-auth.md) is configured. Native TLS remains pending;
see [protected overlays](protected-containers.md).

The proposed [distributed deployment and routing design](design/2026-10-12-distributed-deployment.md)
explains future member identities, separate peer/client endpoints, host placement,
bootstrap, load balancing and drain/fencing. It does not change these single-node
templates or provide a runnable replicated cluster.

## Docker Compose

```sh
docker compose -p brainwire-test -f deploy/compose.yaml up --build -d
docker compose -p brainwire-test -f deploy/compose.yaml exec brainwire brainwire probe --mode ready
```

Bolt binds to localhost; `BRAINWIRE_BIND_PORT` changes the host port. The named
`brainwire-test_brainwire-data` volume survives container replacement and
`down`; `down --volumes` destroys it. Compose restarts exited containers; an
unhealthy health check alone does not restart a standalone Docker container.
Compose uses Docker's native init process to forward termination signals to
the server; this avoids PID 1's default signal behavior delaying local stops.

`serve` creates the default named store on first start. Apply its ontology
offline using the same volume:

```sh
docker compose -p brainwire-test -f deploy/compose.yaml stop
docker compose -p brainwire-test -f deploy/compose.yaml run --rm --no-deps brainwire \
  db apply-ontology client_memory /opt/brainwire/example-schema.json
docker compose -p brainwire-test -f deploy/compose.yaml start
```

Use `db create NAME --ontology ...` offline for additional graphs. The bundled
schema is synthetic; mount an appropriate schema for other workloads.

## Kubernetes

Render locally without contacting a cluster:

```sh
kubectl kustomize deploy/kubernetes
```

Provide the image to an isolated test cluster first. `brainwire:local` must be
available on its worker; change the image for other environments. No image
publication or cluster deployment is performed by the templates themselves.

```sh
kubectl apply -n YOUR_TEST_NAMESPACE -k deploy/kubernetes
kubectl rollout status -n YOUR_TEST_NAMESPACE statefulset/brainwire
kubectl port-forward -n YOUR_TEST_NAMESPACE service/brainwire 7687:7687
```

The StatefulSet retains `data-brainwire-0` on deletion/scale-down. The 10 GiB
PVC uses the default StorageClass; choose durable storage and capacity for
larger tests. `ReadWriteOnce` is not replication or universal single-Pod
fencing. Keep replicas at one. RocksDB also rejects concurrent directory
openers. Reattachment to another machine must retain the original volume;
losing a node-local disk loses that member's data.

UID/GID and `fsGroup` are 10001; the volume driver must support this access.
Only `/data` is writable. Startup runs a query-ready probe with a 15-minute
recovery window. Until startup succeeds, readiness and liveness are deferred.
Readiness executes `RETURN 1`; liveness only negotiates a Bolt session, avoiding
storage queries. Tune the window using real recovery measurements. Probes use
500 ms individual network timeouts and a 5 s outer orchestrator timeout.

The memory limit is 2 GiB, cache 256 MiB, memtables 32 MiB inside that cache,
and query setting 256 MiB. The query setting is a conservative per-RUN
allocation guard and a shared concurrent reservation pool. It is not an RSS
ceiling; the default cooperative execution deadline is 30 seconds. See
[its boundaries](query-allocation-guards.md). A 60 s termination
allowance does not promise graceful draining of every transaction. Acknowledged
commits sync the WAL; interrupted uncommitted writes must disappear.

Acceptance: write synthetic indexed data via Python/Bolt, replace the Pod,
wait for startup/readiness, then verify counts, index lookups and absence of
uncommitted writes. Fence an unreachable old writer before force-deleting its
Pod. This session checks the local manifest render and strict Kubernetes 1.36
schema validation (both resources valid); live-cluster acceptance is provided by the isolated kind script below.
The [session report](benchmarks/2026-10-03-guards-indexes-checkpoints-recovery.md)
records the actual runtime result separately from manifest validation.

## Docker Swarm

Supply `BRAINWIRE_SWARM_NODE_ID` to pin the writer to the machine owning the
retained volume. Create `brainwire-swarm-data` on that machine and provide the
image there. Equal local volume names on different machines are different data.

```sh
export BRAINWIRE_SWARM_NODE_ID=YOUR_STORAGE_NODE_ID
export BRAINWIRE_IMAGE=YOUR_TEST_IMAGE
docker stack config --compose-file deploy/swarm.yaml
docker stack deploy --compose-file deploy/swarm.yaml brainwire-test
```

Bolt is available only on the stack's overlay network, with no published host
port. Updates and rollbacks use `stop-first` so tasks do not overlap on the
volume. If the pinned node is unavailable, the task stays pending instead of
silently starting an empty member elsewhere. Moving it requires reattaching
the retained volume and fencing the old writer first. Swarm's control-plane
quorum does not replicate Brainwire's graph.

## Checks

```sh
docker compose -f deploy/compose.yaml config --quiet
BRAINWIRE_SWARM_NODE_ID=validation-only docker stack config -c deploy/swarm.yaml
kubectl kustomize deploy/kubernetes
python crates/server/tests/container_restart.py --image brainwire:local --rows 10000 --cycles 5
python crates/server/tests/compose_restart.py --image brainwire:local
python crates/server/tests/kubernetes_restart.py --image brainwire:local --rows 10000
```

The acceptance script waits for a query-ready probe and kills its own uniquely
named container during an uncommitted indexed overwrite. It checks acknowledged
data and current/stale index keys after restart, runs an offline `db check`,
then removes its own resources. Requires the Neo4j Python driver and a current
built image. The Compose acceptance also initializes the ontology offline,
force-recreates the service after SIGKILL, verifies an indexed read and audits
the retained volume, cleaning its own project afterward. Manifest parsing and
schema validation are separate from running-cluster acceptance.

Sources: [StatefulSets](https://kubernetes.io/docs/concepts/workloads/controllers/statefulset/),
[health probes](https://kubernetes.io/docs/tasks/configure-pod-container/configure-liveness-readiness-startup-probes/),
[Swarm placement/storage](https://docs.docker.com/engine/swarm/services/),
[update/rollback order](https://docs.docker.com/reference/compose-file/deploy/).

The Kubernetes script requires `kind`, Docker, kubectl and the Neo4j driver. It
checks that the local image exists before allocating resources, creates a unique
single-node cluster with a private kubeconfig, loads that
local image and applies the actual StatefulSet template. It initializes ontology
with an offline Job, ingests bounded Python batches, interrupts an uncommitted
indexed overwrite by stopping the container with zero timeout, and recreates
the Pod. Both recoveries must retain the PVC, string/numeric lookups and committed
counts. It then audits the store, creates/audits a native checkpoint and queries
the copy over Bolt. Its cluster is deleted in `finally`, including on failure;
it does not deploy to an existing context. Its acceptance scope covers process/Pod
recovery on retained storage, not machine loss, distributed replication or HA.

The first October 3 attempt timed out during kind bootstrap. The second-session
retry passed Kubernetes 1.36.4 retained-PVC acceptance with 10K nodes: container
interruption, Pod recreation, index checks, offline audit, native checkpoint and
queries against the reopened copy. See the [current session report](benchmarks/2026-10-03-streaming-pool-deadlines.md)
for the exact local image and raw evidence. This is single-node retained-storage
recovery, not replication, machine loss or HA. The harness checks image presence
before creating a unique cluster; `--node-image` selects another compatible image.
