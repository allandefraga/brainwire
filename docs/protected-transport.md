# Protected Bolt transport for a single-host pilot

Python commands below use the environment from [Python setup](python-package.md).

Brainwire currently serves plaintext Bolt. The optional Compose TLS overlay
terminates TLS in HAProxy and forwards Bolt to Brainwire over the project's
private container network. Bolt authentication and per-graph grants still run
inside Brainwire. This is a qualified client-to-proxy path, not native server
TLS, replication, or protection of traffic between separate machines.

Only `127.0.0.1:7688` is published by default. The overlay removes the base
plaintext database host port. Do not expose the backend separately or attach
untrusted containers to the project network. Keep the loopback binding for a
local pilot; a remote deployment needs an explicitly reviewed ingress and
network boundary.

## Start with certificates and file credentials

Use Docker Compose 2.24.4 or newer; this profile was qualified with 5.4.0.
The [`!reset` merge tag](https://docs.docker.com/reference/compose-file/merge/)
removes the base host port.
Use a server certificate whose Subject Alternative Name matches the client's
hostname. `BRAINWIRE_TLS_PEM_FILE` must contain the certificate chain followed
by the private key. Keep the key private and readable by the proxy process.
The pinned proxy image runs as its `haproxy` user; on a Linux host, arrange
private file ownership/read access for UID/GID 99 as well. The PEM and
configuration are mounted read-only. There are no credentials in YAML.

Build the selected local image first using the [container guide](container.md).
Prepare a server auth policy and health client JSON using the
[auth profile](bolt-auth.md), with grants for the Compose default graph
`default`. The native quickstart's temporary data directory is separate
from Compose storage; initialize the actual selected Compose volume before serving. File-backed Compose
secrets retain host permissions; arrange ownership/read access for Brainwire
UID 10001 without making real credentials world-readable.

```sh
export BRAINWIRE_IMAGE=brainwire:local
export BRAINWIRE_PILOT_PROJECT=brainwire-pilot-YOUR_UNIQUE_SUFFIX
export BRAINWIRE_AUTH_POLICY_FILE=/absolute/private/policy.json
export BRAINWIRE_HEALTH_AUTH_FILE=/absolute/private/health.json
export BRAINWIRE_TLS_PEM_FILE=/absolute/private/server.pem
export BRAINWIRE_TLS_IMAGE=haproxy:3.2.24-alpine@sha256:1aca32ee932b96d2a75f1882909325839dd23c138ba9c6fd26e4d88179f30f13
export BRAINWIRE_TLS_PORT=7688

docker compose -p "$BRAINWIRE_PILOT_PROJECT" -f deploy/compose.yaml -f deploy/compose.auth.yaml -f deploy/compose.tls.yaml config
# Fresh project/volume only; the bundled Customer ontology is a synthetic example.
docker compose -p "$BRAINWIRE_PILOT_PROJECT" -f deploy/compose.yaml -f deploy/compose.auth.yaml -f deploy/compose.tls.yaml run --rm --no-deps brainwire db create default --ontology /opt/brainwire/example-schema.json
docker compose -p "$BRAINWIRE_PILOT_PROJECT" -f deploy/compose.yaml -f deploy/compose.auth.yaml -f deploy/compose.tls.yaml up -d --no-build
```

Replace the project suffix once and keep that same project name for restart.
For an existing retained graph, skip creation; do not initialize a different
project/volume and interpret that as recovery. Use a mounted appropriate ontology
instead of the bundled example for a real source mapping. These commands do not
mount or reuse the native quickstart's store.

Review rendered configuration before starting: `brainwire` must have no
published ports; `tls` must publish only the loopback TLS port. The inherited
Brainwire health probe checks local authenticated Bolt. HAProxy waits for that
service to become healthy and performs TCP backend checks, which are not a
replacement for an authenticated application check through TLS.

HAProxy accepts TLS 1.2 and 1.3, limits connections to 128, and has a 5-second
backend connect timeout and 60-second client/server inactivity timeouts.
An inactive explicit transaction can therefore lose its connection. These are
proxy inactivity limits, not query execution deadlines or graceful drain.
Docker DNS resolves the single backend; this does not distribute writes across
multiple database owners. See the [HAProxy 3.2 configuration manual](https://docs.haproxy.org/3.2/configuration.html).

## Python with strict certificate checks

Use `bolt+s`, which requires certificate validation. The qualification uses a
private CA bundle through Python's `SSL_CERT_FILE`; the fixture proves both
trust-chain and hostname rejection. A public certificate trusted by Python's
normal trust store does not need that variable. Do not use `bolt+ssc` to bypass
verification.

```sh
export SSL_CERT_FILE=/absolute/path/private-ca.crt
```

```python
import json
from pathlib import Path
from brainwire import connect

credentials = json.loads(Path('/absolute/private/writer.json').read_text())
with connect(
    'bolt+s://localhost:7688', "default",
    auth=(credentials['principal'], credentials['credentials']),
    connection_timeout=5,
    liveness_check_timeout=30,
    max_transaction_retry_time=0,
) as graph:
    driver = graph.driver
    with driver.session(database='default') as session:
        print(session.run('RETURN 1 AS ok').single()['ok'])
```

The example checks pooled sockets idle for 30 seconds before reuse. This is
below the proxy's 60-second idle timeout and the native server's 300-second IO
default. A query that runs longer than the proxy's inactivity limit can lose
its connection even while the engine continues working. If no acknowledgement
arrives, treat the mutation outcome as unknown and reconcile durable IDs or
checkpoints before retrying. Proxy expiry is not native query cancellation or
graceful drain.

The native TUI/probe currently connects to plaintext Bolt and does not validate
this TLS endpoint. Run it on the trusted database side with file credentials;
do not route it through this proxy until native TLS client support exists.

## Reproduce the synthetic qualification

Build the selected database image first. The harness generates a one-day local
CA, localhost server certificate, unrelated CA and random credentials inside a
private temporary directory. Synthetic credential and one-day PEM mounts use
mode 0444 inside the mode 0700 temporary directory so image UIDs 10001 and 99 can read them on Linux. This
fixture-only choice is not production permission advice. It starts
only a unique `brainwire-tls-*` project, then removes its own containers, volume,
network and temporary certificates even after failure.

```sh
python3 crates/server/tests/compose_tls.py --self-check
.venv-brainwire/bin/python crates/server/tests/compose_tls.py --image brainwire:local
```

Add `--idle-pool` to either runtime command to warm a one-connection pool,
wait 65 real seconds, and require its first reuse to succeed with
`liveness_check_timeout=30`. The fixture observes the old frontend socket
expiring and a different peer socket appearing through the owned proxy's Linux
TCP tables; certificate verification remains enabled. This optional case adds
about one minute and is not run by ordinary fixture invocations.

Runtime checks include strict CA/hostname success; wrong CA, hostname and
plaintext rejection; TLS 1.2/1.3 success and TLS 1.1 protocol-version rejection;
anonymous/wrong-token rejection; reader write denial;
1,000 indexed writes; explicit rollback; backend SIGKILL/start with committed
data/index recovery; and an offline graph audit. JSON receipts identify both
actual image IDs, the pinned proxy digest, driver version and cleanup result.
Receipts also record the Python and SSL-library versions. In one local beta.2
run, Python 3.14 with OpenSSL 3.5 passed these checks, while Python 3.9.6 with
LibreSSL 2.8.3 could not complete TLS readiness; the cause remains unverified.
The readiness timeout preserves the last connection error for diagnosis.

The final candidate [driver 5 receipt](../benchmarks/archive/2026-10-13-beta/tls-driver5.json)
and [driver 6 receipt](../benchmarks/archive/2026-10-13-beta/tls-driver6.json) passed these
checks on image `brainwire:beta-20261013`
(`sha256:d08c2e5af98ea237b568b86bf58af99af7d80ae5425ad7aaecdc37f620d821d3`).
Their idle-pool pauses were 65.807 and 65.811 seconds; both observed a different
TLS frontend peer and passed their first reuse with liveness set to 30 seconds.
Both receipts also record successful graph/index audits and owned-resource cleanup.

Certificate rotation, remote ingress, Kubernetes/Swarm TLS, mTLS, native TUI TLS
and encryption of the backend connection remain separate qualification work.
The harness uses synthetic data and does not certify a production deployment.
