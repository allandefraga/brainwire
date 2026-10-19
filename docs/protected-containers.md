# Protected single-instance container profiles

The existing development manifests remain unchanged. Optional overlays mount a
server verifier policy and a separate read-only health credential. Generate these
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

## Docker Swarm

Use the existing [Swarm retained-volume runbook](container.md) for the required
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
