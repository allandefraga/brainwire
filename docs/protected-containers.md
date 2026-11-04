# Protected single-instance container profiles

The base manifests omit credentials and now fail closed on their non-loopback
listeners. These overlays mount a server verifier policy and a separate
read-only health credential. Generate these
with [service-token setup](bolt-auth.md), granting the health principal the
configured **default database**. Policy/token updates require a server restart;
secret-file projection alone does not reload Brainwire's in-memory policy.

The auth overlays supply no native Bolt TLS, graph replication, quorum durability
or machine-loss failover. A separate [single-host TLS proxy overlay](protected-transport.md)
now qualifies Python client-to-proxy encryption; its isolated bridge backend remains
plaintext. The server's container listener binds all interfaces. Keep it inside an
appropriate trusted/encrypted transport boundary: Kubernetes networking is
unencrypted by default, and NetworkPolicy filters traffic rather than encrypting
it. [Kubernetes multi-tenancy documentation](https://kubernetes.io/docs/concepts/security/multi-tenancy/).

## Docker Compose

```sh
export BRAINWIRE_IMAGE=brainwire:local
export BRAINWIRE_AUTH_POLICY_FILE=/secure/path/policy.json
export BRAINWIRE_HEALTH_AUTH_FILE=/secure/path/health.json
# Use the same graph name as the health principal's grants/default database.
docker compose -f deploy/compose.yaml -f deploy/compose.auth.yaml config
docker compose -f deploy/compose.yaml -f deploy/compose.auth.yaml up -d --no-build
```

The inherited host port stays on loopback and the named data volume is retained.
The existing default graph is `default`; generate health grants for that
name, or deliberately change both the server default and grants together.

Compose file-backed secrets use file mounts. Do not assume uid/gid/mode attributes
will remap permissions: verify the deployed Compose implementation and provision
source ownership/mode so **UID 10001** can read without exposing real client tokens
to other host users. Do not use the runtime test's world-readable synthetic fixture
mode for real credentials. [Docker Compose secrets](https://docs.docker.com/compose/how-tos/use-secrets/),
[official implementation issue](https://github.com/docker/compose/issues/9648).

## Fresh Docker setup on a Linux host

This recipe is for a **rootful Linux Docker daemon without user-namespace
remapping**, in a dedicated root shell, from a checkout of the desired release.
It is a reviewed setup recipe, not a record that these exact commands were
executed on your host. First copy the versioned `BRAINWIRE_IMAGE` export from the
[README](../README.md); log in to GHCR if the image is private. Keep the same
shell for the following steps.

Use fresh paths and a new Compose project. Do not reuse another deployment's
credentials or data volume. `/root` and `/var/lib` below must be trusted,
root-owned host directories. The generator refuses an existing directory;
`mkdir` also refuses an existing runtime directory, rather than replacing its
credentials.

```sh
set -eu
: "${BRAINWIRE_IMAGE:?Copy the exact versioned image export from the README first}"
export BRAINWIRE_AUTH_SOURCE=/root/brainwire-auth-01
export BRAINWIRE_AUTH_RUNTIME=/var/lib/brainwire-auth-01
export BRAINWIRE_AUTH_POLICY_FILE="$BRAINWIRE_AUTH_RUNTIME/policy.json"
export BRAINWIRE_HEALTH_AUTH_FILE="$BRAINWIRE_AUTH_RUNTIME/health.json"
bwcompose() {
  docker compose -p brainwire-protected \
    -f deploy/compose.yaml -f deploy/compose.auth.yaml "$@"
}
# Fail before creating credentials if this project or volume is already in use.
docker info >/dev/null
BRAINWIRE_EXISTING_CONTAINERS=$(bwcompose ps --all --quiet)
BRAINWIRE_EXISTING_VOLUMES=$(docker volume ls --quiet --filter 'name=^brainwire-protected_brainwire-data$')
if [ -n "$BRAINWIRE_EXISTING_CONTAINERS" ] || [ -n "$BRAINWIRE_EXISTING_VOLUMES" ]; then
  printf "%s\n" "Project or data volume already exists; choose a new deployment instead." >&2
  exit 1
fi
umask 077
python3 -m examples.python.create_service_tokens "$BRAINWIRE_AUTH_SOURCE" --graph default
mkdir -m 0750 "$BRAINWIRE_AUTH_RUNTIME"
chown root:10001 "$BRAINWIRE_AUTH_RUNTIME"
install -o root -g 10001 -m 0640 "$BRAINWIRE_AUTH_SOURCE/policy.json" "$BRAINWIRE_AUTH_RUNTIME/policy.json"
install -o root -g 10001 -m 0640 "$BRAINWIRE_AUTH_SOURCE/health.json" "$BRAINWIRE_AUTH_RUNTIME/health.json"
```

The source directory is 0700, and its original files remain 0600. It contains
separate `writer.json`, `reader.json` and `health.json` tokens plus the verifier
policy. No instance administrator is created. Only the policy and health token
are copied to the 0750 runtime directory; those copies are root-owned, group
10001, mode 0640. Do not commit either directory or paste file contents into a
terminal, issue or chat. If setup fails partway, keep the partial directory
private and choose new paths; do not delete or overwrite credentials in use.

The preflight refuses existing project containers or a same-named data volume.
It is not a lock: do not run this recipe concurrently with another deployment
using the same project or volume name. Docker command failures abort the setup.
Pull the exact image, then check the actual Compose secret mounts as the
container user without printing either file:

```sh
bwcompose config --quiet
bwcompose pull
bwcompose run --rm --no-deps --pull never --entrypoint /bin/sh brainwire -ec \
  'test -r /run/secrets/brainwire-policy.json && test -r /run/secrets/brainwire-health-auth.json'
```

Initialize the `default` graph before starting the server. This project uses
volume `brainwire-protected_brainwire-data`. The one-off command uses the same
Compose service, image, UID and volume as the server. The volume must still be
new: any existing `default` directory aborts initialization. Never run offline
graph commands while a server has that volume open.

```sh
bwcompose run --rm --no-deps --pull never --entrypoint /bin/sh brainwire -ec '
  test ! -e /data/default || {
    printf "%s\n" "Graph path already exists; initialization refused." >&2
    exit 1
  }
  brainwire db create default --ontology /opt/brainwire/example-schema.json
'
bwcompose up --no-build --wait --wait-timeout 900
bwcompose exec -T brainwire brainwire probe --mode ready
```

The bundled ontology is synthetic; replace it with your reviewed ontology when
initializing a new real graph. This command does not apply a new ontology to an
existing graph. All three generated identities are granted exactly `default`,
matching the server and health probe. The application should receive only
`writer.json` for writes or `reader.json` for reads, through its own protected
secret mechanism. It must not receive the server policy or health token. For a
host Python application, set `BRAINWIRE_BOLT_AUTH_FILE` to its securely installed
writer/reader file and connect to `bolt://127.0.0.1:7687`; see the
[Python connection example](bolt-auth.md#python-health-checks-and-connected-inspection).
Do not make the root-only source directory world-readable just to let an app
load a token. Provision a separate owner-readable copy for that app.

`bwcompose down` retains the volume; `down --volumes` deletes its graph data.
On later starts, reuse the same project, runtime paths and image variable, and
run `bwcompose up --no-build --wait --wait-timeout 900`; do not regenerate tokens.
Changing the policy requires a coordinated server restart and client refresh.
Back up and verify the volume before upgrades.

Docker Desktop on macOS and rootless/remapped Linux use different host-file
ownership translation. Do not apply the Linux `root:10001` commands blindly on
those systems. Use the existing protected profile with an appropriate secret
provisioner and verify readability as UID 10001 without making credentials
world-readable. A mount that cannot be read should fail startup; do not bypass
authentication to make it start. These profiles still require the transport and
production-qualification checks described above.

## Docker Swarm

Use the existing [Swarm retained-volume runbook](orchestrators.md#docker-swarm) for the required
owner node/volume. Create secrets separately, then render both files together:

```sh
docker secret create brainwire-auth-policy /secure/path/policy.json
docker secret create brainwire-health-auth /secure/path/health.json
export BRAINWIRE_SWARM_NODE_ID=<retained-volume-owner-node-id>
export BRAINWIRE_IMAGE=<image-present-on-that-node>
docker stack config -c deploy/swarm.yaml -c deploy/swarm.auth.yaml
# Actual deployment is a separate environment-specific operation.
```

The protected overlay requests external secrets, mounted as UID/GID 10001 with
mode 0400. The base remains **one replica**, pinned to the retained-volume owner,
with stop-first updates. Two replicas would be independent stores; Swarm's own
manager consensus does not replicate Brainwire graph writes. Swarm's secret
transport/storage protections do not encrypt application Bolt traffic.
[Docker Swarm secrets](https://docs.docker.com/engine/swarm/secrets/).

Versioned secret names and a controlled service restart are needed for rotation;
do not update a client token without making the server load its matching policy.
Keep a working health credential during the change and test readiness afterward.
Recreate Python drivers/client processes so their cached auth tuples use the new
client secret; updating a mounted file alone does not refresh an existing pool.

## Kubernetes

The separate `deploy/kubernetes-auth` kustomization uses the existing StatefulSet
base, preserving its single replica and PVC retention. Create Secret `brainwire-auth`
with keys `policy.json` and `health.json` in the same namespace as the StatefulSet:

```sh
kubectl create secret generic brainwire-auth \
  --from-file=policy.json=/secure/path/policy.json \
  --from-file=health.json=/secure/path/health.json
kubectl kustomize deploy/kubernetes-auth
# Apply only to an explicitly chosen test environment with the required PVC/storage class.
```

The overlay mounts the Secret read-only with mode 0440; the base's `fsGroup: 10001`
makes it readable to the non-root process. Missing Secret/keys block mounting;
invalid policy content fails server startup. The inherited readiness check uses
the mounted health credential and reads the default graph.

Projected Secret files update eventually; Kubernetes does not automatically
restart Brainwire or reload its policy when a Secret changes. Coordinate a rollout
and verify old credentials are rejected afterward. Recreate Python drivers/client
processes when their credentials change. This mount uses no subPath.
[Kubernetes Secret behavior](https://kubernetes.io/docs/concepts/configuration/secret/).

A StatefulSet gives stable identities and storage attachment, not graph replication.
The application must implement replication itself; the current Brainwire profile
requires retained storage and a single active owner.
[StatefulSet documentation](https://kubernetes.io/docs/concepts/workloads/controllers/statefulset/).

## Local verification

```sh
python3 deploy/check_auth_profiles.py
uv run --no-project --with neo4j==6.3.0 python crates/server/tests/compose_auth.py \
  --image brainwire:auth-20261012
```

The render checker invokes Compose/stack config and kustomize without contacting a
cluster or changing daemon mode. The runtime test creates only its own temporary
Compose project/volume and synthetic credential files, exercises denial, indexed
writes, authenticated health, SIGKILL/recreation and offline audit, then removes
that project. It does not initialize Swarm or change an existing cluster.
See [actual evidence](benchmarks/2026-10-12-auth.md) for executed versus unrun checks.
