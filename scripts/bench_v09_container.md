# Four-label container memory diagnostic

This opt-in harness creates a new random Docker volume and container for each
invocation, uses an already-built local image, binds Bolt to a random loopback
port and deletes only resources carrying its exact random ownership label.
It never attaches to an existing graph, pulls/builds an image, changes another
container, or performs a global prune. Receipts remain in a new private output
directory; existing output directories are refused. Generated writer/reader tokens and their policy are
private to that directory and deleted during cleanup. The ordinary writer has
a write grant; the separate reader has a read grant and is a visibility delegate
only when ACL stamping is selected. Readiness uses the ordinary writer pool.

**Live compatibility and the requested production criteria have not yet been
qualified.** Unit tests cover fixture counts, reader shapes and telemetry/OOM
attribution. First run the small smoke, inspect its report, then run the full
workload separately for 2, 4 and 8 GiB. Run without builds/tests in parallel,
on AC power with low power mode off. The harness refuses an unqualified power
receipt. It records host load and `pmset -g therm` availability at start/end.

```sh
.venv/bin/python scripts/bench_v09_container.py \
  --image YOUR_LOCAL_IMAGE --memory-gib 4 --rows 1000 --batch-size 100 \
  --neighborhood-rows 1000 --soak-seconds 5 --idle-seconds 3 --stamp-acl \
  --output /private/tmp/brainwire-v09-smoke-01

.venv/bin/python scripts/bench_v09_container.py \
  --image YOUR_LOCAL_IMAGE --memory-gib 4 --stamp-acl \
  --output /private/tmp/brainwire-v09-before-4g-01
```

Repeat the second command with `--memory-gib 2` and `8`, each with a new output
path. Default workload: 1M nodes per label, eight writers, four mixed readers,
30 minutes of post-load read soak, 60 seconds idle, cursor pages of 200.
Loading has a separate one-hour deadline. The 30-minute default is read soak
*after* loading; load duration is recorded separately. `--rows`, reader width
and timings may be reduced for smoke tests; those runs are never full
qualification. Docker Desktop's VM must have enough available memory/disk.
New receipts record batch size, neighborhood width, sampling interval, reader
pause and query timeout. Earlier receipts may lack those fields; do not assume
matching conditions from a comparison of the fields they did record.

## Dataset and operations

The ontology/property shapes follow `crates/cypher/benches/baseline.rs`:
Usuario, Carro, Endereco, Casa and the four TEM_CARRO, TEM_ENDERECO, TEM_CASA,
CASA_FICA_EM relationships. At 1M rows, this is 4M nodes and 4M edges. Each
identity has one node per label and one edge per type. There are no extra
Pessoa nodes or broad-degree hubs. Eight writers own disjoint identity ranges;
each transaction atomically upserts all four labels and edge types for its batch.
This loading order differs from the older sequential per-label embedded
benchmark; do not compare its ingestion times as the same workload.

Four concurrent readers repeatedly perform a full Usuario scan, brand
aggregation over relationships, a bounded broad neighborhood returning full
node values (default first100K identities), and an ordered Usuario keyset page.
Each scan, aggregation and neighborhood read checks cardinality in the same
transaction snapshot; row identities, relationship endpoints and cursor ordering
are validated too. These checks add a second query to each such read operation
and are included in its latency. These deliberately stress materialization. Each role records latency, row count
and memory-denial codes. Only known memory errors are accepted as backpressure;
other failures fail the diagnostic. A memory denial is still reported and is
not counted as a successful query.

With `--stamp-acl`, every new node receives an ACL allowing one synthetic
principal in the same writer transaction. All readers pass delegated principal
visibility. This checks the restricted visibility path but makes every node
visible; it does not qualify mixed allow/deny or revocation semantics. Without
that flag the report explicitly records an unrestricted cursor. A complete
quiescent cursor verifies every expected ID after load/read soak; mixed cursor
pages during writes are not snapshot pagination.

After readers stop, the original process stays alive for idle memory samples,
then exact label/edge counts, unlabelled LIMIT and full cursor results are
verified. Finally, a clean stop and offline `db check` must pass. This harness
currently does not inject a crash; existing crash tests are separate.

## Memory and OOM evidence

The container uses `--memory=N --memory-swap=N` (no extra swap allowance).
Telemetry reads cgroup-v2 memory.current, memory.peak, memory.max, memory.events,
memory.stat and PID1 VmRSS/RssAnon/RssFile. Cgroup peak includes file cache;
sampled process RSS is not a continuous RSS high-water mark. The live graph
process may die between samples, so a missing final sample fails that run.

A separate Docker event stream records oom/die events, and the receipt includes
RestartCount and container state. An observed cgroup OOM or Docker oom event
fails the diagnostic even when OOMKilled=false. The harness attempts an
unprivileged dmesg probe and records availability, but cannot read the Docker
host kernel log on many installations. It **never treats OOMKilled=false as
proof of no kernel OOM**, and `kernel_log_verified` remains false. Independent
host kernel evidence is still required for the user's full acceptance gate.
No privileged container or host-kernel configuration change is used.

Image IDs/digests/revision labels are recorded; the harness cannot prove an
image was built from a specific checkout. Build from the intended revision
before measuring, and retain the build/source receipt separately. Results do
not certify a process-wide RSS ceiling, memory release policy, streaming,
TLS, replication, power-loss recovery or production readiness.

```sh
PYTHONPYCACHEPREFIX=/private/tmp/brainwire-v09-pycache \
  python3 -m unittest scripts.test_bench_v09_container -v
```
