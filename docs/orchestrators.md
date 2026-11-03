# Docker, Kubernetes and Swarm test deployments

The templates run one durable server with multiple named graphs. These
templates do not enable replication: increasing replicas creates independent stores.
The [experimental quorum profile](quorum-replication.md) has a separate deployment
path. Keep one writer per RocksDB directory. Non-loopback listeners now require
a valid auth policy. Use the protected [Compose, Kubernetes or Swarm profile](protected-containers.md)
for real data and shared access. The unauthenticated opt-in is only for isolated
synthetic development. Native TLS remains unavailable; see [transport guidance](protected-transport.md).

The proposed [distributed deployment and routing design](design/2026-10-12-distributed-deployment.md)
explains future member identities, separate peer/client endpoints, host placement,
bootstrap, load balancing and drain/fencing. It does not change these single-node
templates or provide a runnable replicated cluster.

## Docker Compose

Run these commands from the repository root with Docker Engine running. For a
ready-to-query synthetic ontology, use the [demo quickstart](container.md#fastest-local-start).
For a protected `default` graph, first create a server policy and health
credential as described in [service-token setup](bolt-auth.md), then set their
file paths and start both Compose files:

```sh
export BRAINWIRE_AUTH_POLICY_FILE=/secure/path/policy.json
export BRAINWIRE_HEALTH_AUTH_FILE=/secure/path/health.json
docker compose -p brainwire-test -f deploy/compose.yaml -f deploy/compose.auth.yaml up --build --wait --wait-timeout 900
docker compose -p brainwire-test -f deploy/compose.yaml -f deploy/compose.auth.yaml exec brainwire brainwire probe --mode ready
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
docker compose -p brainwire-test -f deploy/compose.yaml -f deploy/compose.auth.yaml stop
docker compose -p brainwire-test -f deploy/compose.yaml -f deploy/compose.auth.yaml run --rm --no-deps brainwire \
  db apply-ontology default /opt/brainwire/example-schema.json
docker compose -p brainwire-test -f deploy/compose.yaml -f deploy/compose.auth.yaml start
```

Use `db create NAME --ontology ...` offline for additional graphs. The bundled
schema is synthetic; mount an appropriate schema for other workloads.

## Kubernetes

### Local laptop quickstart with kind

Install Docker, `kubectl` and `kind` first, and run from the repository root.
This creates a dedicated single-node cluster named `brainwire-demo` and a private
kubeconfig, so commands do not use your existing cluster context. Choose another
cluster name if `brainwire-demo` already exists; do not delete an existing cluster
to reuse its name. This local example uses an auth-enabled server and reads the
policy/health JSON files created in [service-token setup](bolt-auth.md). The
local image is loaded directly, without a registry:

```sh
docker build -t brainwire:local .
BRAINWIRE_KUBE_DIR="$(mktemp -d)"
BRAINWIRE_KUBECONFIG="$BRAINWIRE_KUBE_DIR/config"
kind create cluster --name brainwire-demo --kubeconfig "$BRAINWIRE_KUBECONFIG" --wait 120s
kind load docker-image brainwire:local --name brainwire-demo
kubectl --kubeconfig "$BRAINWIRE_KUBECONFIG" --context kind-brainwire-demo create namespace brainwire-demo
kubectl --kubeconfig "$BRAINWIRE_KUBECONFIG" --context kind-brainwire-demo -n brainwire-demo create secret generic brainwire-auth \
  --from-file=policy.json=/secure/path/policy.json --from-file=health.json=/secure/path/health.json
kubectl --kubeconfig "$BRAINWIRE_KUBECONFIG" --context kind-brainwire-demo -n brainwire-demo apply -k deploy/kubernetes-auth
kubectl --kubeconfig "$BRAINWIRE_KUBECONFIG" --context kind-brainwire-demo -n brainwire-demo rollout status statefulset/brainwire --timeout=900s
kubectl --kubeconfig "$BRAINWIRE_KUBECONFIG" --context kind-brainwire-demo -n brainwire-demo exec brainwire-0 -- brainwire probe --mode ready
kubectl --kubeconfig "$BRAINWIRE_KUBECONFIG" --context kind-brainwire-demo -n brainwire-demo port-forward service/brainwire 17687:7687
```

Run each step only after the previous one succeeds. Keep the last command running;
clients connect to `bolt://127.0.0.1:17687`, database `default`. This starts an
empty graph. The Docker demo's Python Customer example needs its bundled ontology
and database `demo`; it cannot be copied unchanged into this empty graph. The
[kind acceptance test](#checks) initializes the ontology offline and exercises
Python writes and recovery end to end.

Stop port-forwarding with Ctrl-C. In the same shell, inspect or restart the Pod:

```sh
kubectl --kubeconfig "$BRAINWIRE_KUBECONFIG" --context kind-brainwire-demo -n brainwire-demo get pods,pvc
kubectl --kubeconfig "$BRAINWIRE_KUBECONFIG" --context kind-brainwire-demo -n brainwire-demo logs brainwire-0
kubectl --kubeconfig "$BRAINWIRE_KUBECONFIG" --context kind-brainwire-demo -n brainwire-demo delete pod brainwire-0
kubectl --kubeconfig "$BRAINWIRE_KUBECONFIG" --context kind-brainwire-demo -n brainwire-demo rollout status statefulset/brainwire --timeout=900s
```

The recreated Pod uses the same retained PVC. Deleting this disposable kind
cluster permanently deletes its local graph data, including retained PVCs:

```sh
kind delete cluster --name brainwire-demo --kubeconfig "$BRAINWIRE_KUBECONFIG"
rm -- "$BRAINWIRE_KUBECONFIG"
rmdir -- "$BRAINWIRE_KUBE_DIR"
```

### Existing isolated cluster

Render locally without contacting a cluster. The base manifest is a building
block without authentication; use an auth-enabled overlay to serve it:

```sh
kubectl kustomize deploy/kubernetes
```

The base manifest uses `brainwire:local` for local image builds. Create the
`brainwire-auth` server/health secret described in [protected Kubernetes setup](protected-containers.md).
For a private GHCR package, also create a namespace-scoped image pull secret with a GitHub token
that has `read:packages` access. This prompt keeps the token out of shell
history, command arguments, and repository files:

```sh
kubectl create namespace YOUR_TEST_NAMESPACE
kubectl create secret generic brainwire-auth -n YOUR_TEST_NAMESPACE \
  --from-file=policy.json=/secure/path/policy.json \
  --from-file=health.json=/secure/path/health.json
python3 - YOUR_TEST_NAMESPACE <<'PY'
import base64
import getpass
import json
import subprocess
import sys

username = input("GitHub username: ")
token = getpass.getpass("GitHub token (read:packages): ")
auth = base64.b64encode(f"{username}:{token}".encode()).decode()
docker_config = json.dumps({"auths": {"ghcr.io": {"auth": auth}}})
secret = {
    "apiVersion": "v1",
    "kind": "Secret",
    "metadata": {"name": "ghcr-pull", "namespace": sys.argv[1]},
    "type": "kubernetes.io/dockerconfigjson",
    "data": {".dockerconfigjson": base64.b64encode(docker_config.encode()).decode()},
}
subprocess.run(
    ["kubectl", "apply", "-f", "-"],
    input=json.dumps(secret), text=True, check=True,
)
PY
kubectl kustomize deploy/kubernetes-public-release
kubectl apply -n YOUR_TEST_NAMESPACE -k deploy/kubernetes-public-release
# For a private GHCR package, use the overlay that also references ghcr-pull:
kubectl kustomize deploy/kubernetes-release
kubectl apply -n YOUR_TEST_NAMESPACE -k deploy/kubernetes-release
```

Both versioned overlays pin `ghcr.io/allandefraga/brainwire:v0.15.0-beta.3`. <!-- brainwire-release-version --> The local
release helper updates both tags with the Cargo workspace version. The private
overlay references `ghcr-pull`; logging in on your workstation alone does not
authenticate cluster nodes. The public overlay needs no image-pull secret. For
immutable deployments, replace the tag with the published image digest. Applying
the manifest starts a service in the cluster; it does not create or qualify a
cluster for you.

```sh
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
[its boundaries](query-allocation-guards.md). On SIGTERM the server
drains for `server.shutdown_grace_ms` (default 8 s, inside the 60 s allowance);
transactions still open after it are abandoned, so draining every transaction is
not promised. Acknowledged
commits sync the WAL; interrupted uncommitted writes must disappear.

Acceptance: write synthetic indexed data via Python/Bolt, replace the Pod,
wait for startup/readiness, then verify counts, index lookups and absence of
uncommitted writes. Fence an unreachable old writer before force-deleting its
Pod. Local manifest rendering and schema validation do not exercise a running
cluster; live-cluster acceptance is provided by the isolated kind script below.
The [session report](benchmarks/2026-10-03-guards-indexes-checkpoints-recovery.md)
records the actual runtime result separately from manifest validation.

## Docker Swarm

Supply `BRAINWIRE_SWARM_NODE_ID` to pin the writer to the machine owning the
retained volume. Create `brainwire-swarm-data` on that machine and provide the
image there. Equal local volume names on different machines are different data.
Create the external auth secrets from the server policy and health credential
described in [protected Swarm setup](protected-containers.md):

```sh
export BRAINWIRE_SWARM_NODE_ID=YOUR_STORAGE_NODE_ID
export BRAINWIRE_IMAGE=ghcr.io/allandefraga/brainwire:v0.15.0-beta.3 # brainwire-release-version
docker secret create brainwire-auth-policy /secure/path/policy.json
docker secret create brainwire-health-auth /secure/path/health.json
docker stack config -c deploy/swarm.yaml -c deploy/swarm.auth.yaml
docker stack deploy --with-registry-auth -c deploy/swarm.yaml -c deploy/swarm.auth.yaml brainwire-test
```

Log in to GHCR on the manager first. `--with-registry-auth` forwards those
credentials to workers so private image pulls succeed; use a token with
`read:packages` permission. Keep the token out of stack files and Git.

Bolt is available only on the stack's overlay network, with no published host
port. Updates and rollbacks use `stop-first` so tasks do not overlap on the
volume. If the pinned node is unavailable, the task stays pending instead of
silently starting an empty member elsewhere. Moving it requires reattaching
the retained volume and fencing the old writer first. Swarm's control-plane
quorum does not replicate Brainwire's graph.

## Checks

```sh
docker compose -f deploy/compose.yaml -f deploy/compose.auth.yaml config --quiet
BRAINWIRE_SWARM_NODE_ID=validation-only docker stack config -c deploy/swarm.yaml -c deploy/swarm.auth.yaml
kubectl kustomize deploy/kubernetes
kubectl kustomize deploy/kubernetes-auth
kubectl kustomize deploy/kubernetes-release
kubectl kustomize deploy/kubernetes-public-release
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
