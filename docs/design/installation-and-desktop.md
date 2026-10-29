# Brainwire installation and desktop delivery

Status: design proposal based on the current repository. Docker Compose and
source builds exist. No Homebrew formula/tap, desktop application, cask or
signed native installer is claimed to be published or tested here.

## Available now

### Docker Compose

The documented local demo is `deploy/compose.demo.yaml`:

```sh
docker compose -p brainwire-demo -f deploy/compose.demo.yaml \
  up --build --wait --wait-timeout 900
```

It builds the local Dockerfile, initializes the synthetic `demo` ontology once,
mounts a retained named volume at `/data`, and publishes Bolt on host loopback
port 7687. `BRAINWIRE_BIND_PORT` selects another host port. It requires a running
Docker Engine and Docker Compose; the first Rust/RocksDB build can take minutes.
`down` retains data; `down --volumes` deletes that demo's volume.

The image entrypoint is `/usr/local/bin/brainwire`; its default command is
`serve`. It runs as UID/GID 10001, sets `BRAINWIRE_SERVER_DATA_DIR=/data` and
listens inside the container on `0.0.0.0:7687`. The official release workflow
currently requests `linux/amd64`; do not claim a native arm64 image without a
new build/test. A release tag alone does not prove the registry image exists;
check its workflow and registry access, then pin a published digest for deployment.

For a Docker run equivalent, build and initialize a fresh synthetic volume:

```sh
docker build -t brainwire:local .
docker volume create brainwire-data
docker run --rm --mount type=volume,src=brainwire-data,dst=/data \
  brainwire:local db create demo --ontology /opt/brainwire/example-schema.json
docker run --detach --name brainwire-local --init --restart unless-stopped \
  --stop-timeout 60 --publish 127.0.0.1:7687:7687 \
  --mount type=volume,src=brainwire-data,dst=/data --memory 2g \
  --env BRAINWIRE_SERVER_DEFAULT_DATABASE=demo \
  --env BRAINWIRE_MEMORY_CACHE=256MiB \
  --env BRAINWIRE_MEMORY_MEMTABLES=32MiB \
  --env BRAINWIRE_MEMORY_QUERY=256MiB \
  brainwire:local serve --allow-unauthenticated-remote
```

This is the existing synthetic, local-only demo override. Real/shared data uses
the authenticated policy profile (`docs/protected-containers.md`) and an encrypted
boundary (`docs/protected-transport.md`), with private files/secrets. The current
native Bolt endpoint is plaintext; the optional Compose proxy exposes strict TLS
on host loopback port 7688 and removes the backend's published plaintext port.
Never describe a Docker network alone as encryption or authentication.

Verification: check `docker exec brainwire-local brainwire probe --mode ready`,
connect a Python client to `bolt://127.0.0.1:7687` selecting `demo`, insert a bounded
synthetic batch, restart the same container/volume, and verify identities,
properties and edges. Stop the server before offline `db check demo`; one process
owns each RocksDB directory. Confirm ownership/permissions for any bind mount.
These are acceptance steps to run, not results measured by this proposal.

### Source build

Use the pinned Rust 1.98.1 toolchain, a native C/C++ compiler, libclang, CMake and
pkg-config (the Docker builder installs Clang/libclang/CMake/pkg-config). Fetch
locked dependencies on the first connected build; later builds may run offline:

```sh
cargo fetch --locked
cargo build --release --locked --offline -p brainwire-server
target/release/brainwire --version
```

The result is `target/release/brainwire`, an administration CLI, Bolt server and
TUI entrypoint. Initialize a named graph with an explicit ontology and data
location, then start `serve` using the same configuration. Native source runs
need no Docker. Run the repository's tests, Clippy, formatting, protected Bolt
client acceptance and restart/integrity checks on each supported OS/architecture
before advertising binary support. See README and `docs/serve.md` for the actual
configuration contract; avoid inventing `cargo install brainwire` availability.

## Future Homebrew delivery

Start with a project-owned tap containing a formula for the CLI/server, pinned
to a source release plus checksum. A tap is a separate delivery repository;
its name and commands should be announced only after it exists. The formula
must declare build dependencies, install the `brainwire` binary, and run a
synthetic create/query/reopen check. Keep user data outside the versioned Cellar;
upgrades/uninstall must not delete or silently migrate volumes. Add bottles for
macOS arm64/x86_64 and supported Linux targets only after native CI builds/tests.
Add an opt-in `brew services` definition later with explicit config/data paths,
loopback default and safe shutdown; installation must not expose a server.

Homebrew distinguishes formulae, taps, bottles and casks. A desktop cask would
install a separately produced app artifact; it is not the CLI formula. Follow the
[Formula Cookbook](https://docs.brew.sh/Formula-Cookbook) and
[Cask Cookbook](https://docs.brew.sh/Cask-Cookbook) when implementing. Test the
actual tap/formula with install, audit, test, upgrade and uninstall in clean
machines, preserving synthetic data across upgrade.

## Future Electron desktop client

First release: a read-only client for an already-running Brainwire server. The
Electron app is not a replacement database engine, a rendered terminal capture,
or an existing TUI screenshot presented as a working desktop interface. Build
actual bounded views for named-graph connection, schema, neighbor inspection,
index status and operational data the selected principal may read. Label stale
or unavailable data. Existing TUI layouts may inform the design, but capability
claims must match actual APIs.

Keep the server external initially. The Node main/utility side owns a verified
Bolt client connection and bounded pool; renderer code receives only typed,
size-limited results through a narrow preload/IPC interface. A candidate JS Bolt
driver must pass Brainwire protocol/auth/error compatibility tests before use;
Python driver coverage does not prove JS driver coverage. Require a server-side
read grant and deny arbitrary write/admin actions; a client-side read toggle is
not authorization. Metadata such as memory may require separate admin permission
and should be unavailable to an ordinary reader.

Store credentials through an OS-backed credential facility after testing its
platform behavior; keep them out of renderer/localStorage/logs and release
artifacts. Validate server URI, explicit graph choice and TLS chain/hostname for
remote connections. Local development may explicitly connect to loopback Bolt
7687; the existing protected proxy recipe uses 7688. The client should create no
inbound listener. A future managed local server launcher needs separate process,
private data/config, authenticated loopback, port-conflict handling, version
compatibility, exclusive RocksDB ownership and graceful shutdown; never attach
to or kill an unrelated process just because it owns the requested port.

Use `nodeIntegration: false`, `contextIsolation: true`, renderer `sandbox: true`,
a restrictive CSP and packaged UI content. Validate IPC sender and arguments,
allow only explicit operations, deny unwanted navigation/windows/permissions,
and treat graph strings/links as untrusted data. Never expose generic Node,
filesystem, shell or IPC primitives to the renderer. Keep Electron current and
review its [security guidance](https://www.electronjs.org/docs/latest/tutorial/security/)
and [sandbox model](https://www.electronjs.org/docs/latest/tutorial/sandbox).

Package and test each target architecture separately. A macOS app release needs
signed artifacts, notarization/stapling and clean-machine Gatekeeper checks;
keep signing credentials in release secrets. Produce checksums and a documented
update/rollback policy. Add a cask only after the signed release artifact and
stable download URL exist. Windows/Linux packaging and signing have their own
qualification tasks; no universal installer is currently available.

Desktop acceptance: connect with valid/invalid credentials and certificates;
prove read-only grants reject writes; bound large graph/result responses; test
cancel, timeout, disconnect/reconnect and stale snapshots; render hostile graph
strings safely; verify IPC denial and credential redaction; upgrade/uninstall
without deleting database files. Ship screenshots only from the actual built
client and identify synthetic datasets.

## Suggested order and availability gate

1. Improve the current Compose/source quickstart and run its real acceptance.
2. Publish reproducible native release binaries and a tested CLI tap/formula.
3. Build the bounded read-only Electron client against verified Bolt APIs.
4. Sign/notarize the desktop release, then publish an optional cask.
5. Consider managed local server lifecycle only after server stability and
   upgrade/restore qualification pass.

This document creates no package, installer, server or desktop application.
