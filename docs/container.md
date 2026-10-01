# Single-node test container

The local image packages the **single-node** Bolt server. It is a test
deployment step toward Kubernetes and Docker Swarm; it does not implement
replication, failover, authentication or TLS. Use it only on a trusted test
network. See [server limits](serve.md).

Build and initialize a named Docker volume:

```sh
docker build -t brainwire:local .
docker volume create brainwire-data
docker run --rm --mount type=volume,src=brainwire-data,dst=/data \
  brainwire:local db create client_memory \
  --ontology /opt/brainwire/example-schema.json
```

Run the server, then connect a Bolt driver to `bolt://127.0.0.1:7687`:

```sh
docker run --name brainwire-local --rm \
  --publish 127.0.0.1:7687:7687 \
  --mount type=volume,src=brainwire-data,dst=/data \
  --memory 2g \
  --env BRAINWIRE_MEMORY_CACHE=256MiB \
  --env BRAINWIRE_MEMORY_MEMTABLES=32MiB \
  --env BRAINWIRE_MEMORY_QUERY=256MiB \
  brainwire:local
```

The container runs as UID/GID 10001 and expects its volume to be writable by
that user. The image does not declare an anonymous `VOLUME`: every deployment
must explicitly name or mount its durable storage. A fresh local volume is a
fresh database. Do not treat a container restart on a different machine with a
new local volume as recovery of the old member.

For a restart check, write synthetic data, kill the container, then start a new
container with the **same** `brainwire-data` volume and read it back. The
[driver acceptance script](../crates/server/tests/mvp_demo.py) has `write` and
`read` modes for this purpose. The repeatable
[container restart test](../crates/server/tests/container_restart.py) creates
its own temporary volume, writes 10,000 synthetic Customers, performs five
kill/recreate cycles and removes its test resources:

```sh
python crates/server/tests/container_restart.py
```

This tests process/container recovery, not machine loss. The example ontology
is bundled only to make local test setup reproducible; use an appropriate
ontology for real graphs.

Before Kubernetes or Docker Swarm deployment, add a recovery-aware health
endpoint and explicit readiness/liveness behavior. Package stable member
identities and durable per-member volumes; run the failure matrix in the
[data and recovery design](design/2026-09-27-brainwire-data-and-recovery.md).
On Docker Swarm, service placement must keep each member bound to the correct
storage, or use a storage system that can reattach that member's volume. On
Kubernetes, a StatefulSet and persistent volume claims are the intended first
single-node pattern. Multi-machine high availability requires Brainwire's own
replication protocol and quorum tests in addition to either orchestrator.
