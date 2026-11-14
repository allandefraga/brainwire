# Protected Bolt transport

Use a service-token policy and verify the server certificate for shared
connections. Brainwire can terminate TLS on its Bolt listener. The Compose
HAProxy overlay is another option for a single-host pilot. Both use the
[Python setup](python-package.md) for the client examples below.

## Enable native Bolt TLS

Prepare a certificate chain in PEM format, with the server certificate first,
and a separate PEM file containing the matching private key. Keep the key
private and readable by the server process. The certificate's Subject
Alternative Name (SAN) must cover each DNS name or IP that clients use. For a
local probe that connects to `127.0.0.1`, include `IP:127.0.0.1` in the SAN.
Configure a [service-token policy](bolt-auth.md) for the graph before sharing it.

```sh
export BRAINWIRE_SERVER_AUTH_FILE=/absolute/private/policy.json
export BRAINWIRE_SERVER_TLS_CERT_FILE=/absolute/private/server-chain.pem
export BRAINWIRE_SERVER_TLS_KEY_FILE=/absolute/private/server-key.pem
brainwire serve
```

The equivalent TOML options are `server.tls_cert_file` and
`server.tls_key_file`. Set both together. The configured listener accepts TLS
1.2 and 1.3 and starts Bolt negotiation after successful TLS setup. With no
certificate and key configured, the listener serves plaintext. Rotate the
certificate or key by replacing the files and restarting the server. Raft peer
TLS has its own configuration.

Python's `bolt+s://` validates certificates against its trust store. For a
private CA, use the [explicit CA example](#python-with-strict-certificate-checks)
with the native listener's host and port, such as `bolt://localhost:7687` for a
certificate with `DNS:localhost`. Keep hostname or IP verification enabled.

The native probe and terminal take numeric `IP:PORT` addresses. Provide a PEM
CA bundle with `BRAINWIRE_BOLT_TLS_CA_FILE`. The IP must appear as an IP SAN in
the certificate. The client checks the chain against the supplied bundle and
never falls back to plaintext after a failed TLS handshake.

```sh
export BRAINWIRE_BOLT_TLS_CA_FILE=/absolute/private/ca.pem
export BRAINWIRE_BOLT_AUTH_FILE=/absolute/private/health.json
brainwire probe --mode ready --address 127.0.0.1:7687
```

For `brainwire shell`, use a credential with the appropriate graph read grants
and the same CA setting. With containers, mount certificate files and the CA
bundle read-only and set the probe's CA variable in the container environment.
The default image and Compose probes connect to `127.0.0.1:7687`. If the
certificate lacks that IP SAN, configure a probe address whose IP is covered.
`handshake` only verifies the endpoint and Bolt negotiation. `ready` also
needs a valid credential on a protected server and makes a query.

The connection limit still applies while TLS negotiates. TLS uses memory in
addition to the `memory.ingress` budget, which accounts for Bolt request
storage. TLS authenticates the server only. Client authentication uses Bolt
service tokens.

## Run native Bolt TLS with Compose

Use a pinned Brainwire image that includes native Bolt TLS. To build one from a
checkout that includes this feature, use `docker build -t brainwire:native-tls .`
from the repository root. The overlay requires an explicit image in
`BRAINWIRE_NATIVE_TLS_IMAGE`. Keep the existing Compose project name to reuse
its volume. Use a certificate with `IP:127.0.0.1` for the inherited readiness
probe and a SAN for any separate address your application uses.

```sh
export BRAINWIRE_NATIVE_TLS_IMAGE=brainwire:native-tls
export BRAINWIRE_PILOT_PROJECT=brainwire-pilot-YOUR_UNIQUE_SUFFIX
export BRAINWIRE_AUTH_POLICY_FILE=/absolute/private/policy.json
export BRAINWIRE_HEALTH_AUTH_FILE=/absolute/private/health.json
export BRAINWIRE_NATIVE_TLS_CHAIN_FILE=/absolute/private/server-chain.pem
export BRAINWIRE_NATIVE_TLS_KEY_FILE=/absolute/private/server-key.pem
export BRAINWIRE_NATIVE_TLS_CA_FILE=/absolute/private/ca.pem

docker compose -p "$BRAINWIRE_PILOT_PROJECT" \
  -f deploy/compose.yaml -f deploy/compose.auth.yaml \
  -f deploy/compose.native-tls.yaml config

# Run only for a fresh volume. Use your ontology for real data.
docker compose -p "$BRAINWIRE_PILOT_PROJECT" \
  -f deploy/compose.yaml -f deploy/compose.auth.yaml \
  -f deploy/compose.native-tls.yaml run --rm --no-deps brainwire \
  db create default --ontology /opt/brainwire/example-schema.json

docker compose -p "$BRAINWIRE_PILOT_PROJECT" \
  -f deploy/compose.yaml -f deploy/compose.auth.yaml \
  -f deploy/compose.native-tls.yaml up --no-build --wait --wait-timeout 900
```

Compose mounts the files as secrets. Keep the private key and tokens private
and readable by Brainwire's container UID 10001. The health principal needs
read access to the default database. The service publishes Bolt on host
loopback. Set `BRAINWIRE_BIND_PORT` to choose a different host port.

Verify the actual endpoint from outside the container before connecting your
application. A healthy status alone may come from an older image running
plaintext. With a TLS-capable `brainwire` client on the host, the first command
below must succeed with the expected CA and IP SAN. The second must fail
because it tries plaintext Bolt negotiation without credentials. Change the
port in both commands if you set `BRAINWIRE_BIND_PORT`.

```sh
BRAINWIRE_BOLT_TLS_CA_FILE="$BRAINWIRE_NATIVE_TLS_CA_FILE" \
  BRAINWIRE_BOLT_AUTH_FILE="$BRAINWIRE_HEALTH_AUTH_FILE" \
  brainwire probe --mode ready --address 127.0.0.1:7687

env -u BRAINWIRE_BOLT_TLS_CA_FILE \
  brainwire probe --mode handshake --address 127.0.0.1:7687
```

## Use the Compose TLS overlay

The optional overlay terminates TLS in HAProxy. It forwards plaintext Bolt to
Brainwire over the project's private container network. Brainwire checks Bolt
credentials and graph grants. Use the overlay with a plaintext Brainwire
backend. The archived test receipts below cover the client-to-proxy path on a
single host.

Only `127.0.0.1:7688` is published by default. The overlay removes the base
plaintext database host port. Do not expose the backend separately or attach
untrusted containers to the project network. Keep the loopback binding for a
local pilot; a remote deployment needs an explicitly reviewed ingress and
network boundary.

## Start with certificates and file credentials

Use Docker Compose 2.24.4 or newer; this profile was qualified with 5.4.0.
The [`!reset` merge tag](https://docs.docker.com/reference/compose-file/merge/)
removes the base host port.
Use a server certificate with a SAN matching the address clients connect to.
`BRAINWIRE_TLS_PEM_FILE` must contain the certificate chain followed by the
private key. Keep the key private and readable by the proxy process.
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

Use `bolt+s`, which requires certificate validation. A public certificate
trusted by Python's normal trust store needs no extra CA configuration. For a
private CA, `SSL_CERT_FILE` works with some Python/OpenSSL builds, but not all:
the system Python 3.9/LibreSSL tested on macOS did not load it. If in doubt,
use the explicit CA configuration below. Do not use `bolt+ssc` to bypass
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

For portable explicit trust of a private CA, use the plain `bolt://` scheme
with encryption enabled and `TrustCustomCAs`. The driver checks both the CA
chain and hostname; the hostname must appear in the server certificate.

```python
from neo4j import TrustCustomCAs
from brainwire import connect

with connect(
    'bolt://localhost:7688', 'default',
    auth=(credentials['principal'], credentials['credentials']),
    encrypted=True,
    trusted_certificates=TrustCustomCAs('/absolute/path/private-ca.crt'),
    connection_timeout=5,
    liveness_check_timeout=30,
    max_transaction_retry_time=0,
) as graph:
    print(graph.run_cypher('RETURN 1 AS ok'))
```

The example checks pooled sockets idle for 30 seconds before reuse. This is
below the proxy's 60-second idle timeout and the native server's 300-second IO
default. A query that runs longer than the proxy's inactivity limit can lose
its connection even while the engine continues working. If no acknowledgement
arrives, treat the mutation outcome as unknown and reconcile durable IDs or
checkpoints before retrying. Proxy expiry is not native query cancellation or
graceful drain.

The native TUI and probe can connect to the TLS frontend when its certificate
includes the frontend IP as an IP SAN. Set `BRAINWIRE_BOLT_TLS_CA_FILE` for the
frontend CA bundle. With a DNS-only certificate, use a client that connects
by the matching DNS name.

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

If the local Python runtime does not load `SSL_CERT_FILE`, add `--custom-ca`
to exercise explicit CA loading with the same strict hostname checks.

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

The proxy harness uses synthetic data. Its coverage is limited to the
single-host client-to-proxy setup described above. Remote ingress,
Kubernetes/Swarm TLS and backend encryption require separate tests.
