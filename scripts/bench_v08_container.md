# v0.8 container baseline (diagnostic, not a published benchmark)

This opt-in harness creates a **new, uniquely labelled Docker volume and server**,
uses an already-built local image, and publishes Bolt only on a random loopback
port. It never mounts an existing graph, deletes existing data, pulls an image,
or builds the server. It refuses an existing output directory. Cleanup checks
its random ownership label before removing its exact server, offline helper and
volume. Docker/global prune is never used. The synthetic graph is deleted after
the diagnostic; receipts remain in the private output directory.

The implementation has unit tests and was exercised against local baseline and
branch images; see `PRODUCTION-v0.8.md` for the measured scope and limitations.
First run the small smoke below, inspect its receipt, then run the million-node workload. The script may
expose existing engine limitations; a failed result is evidence, not a reason
to silently reduce concurrency or change the measured profile.

## Run

From the repository root, use the project's existing Python environment with the
Neo4j driver installed. Neither command installs packages or changes the image:

```sh
.venv/bin/python scripts/bench_v08_container.py \
  --image brainwire:local-v0.7.0-beta.1 \
  --rows 10000 --batch-size 100 --writers 8 --readers 4 \
  --idle-seconds 10 --max-load-seconds 300 \
  --output /private/tmp/brainwire-v08-smoke-01

.venv/bin/python scripts/bench_v08_container.py \
  --image brainwire:local-v0.7.0-beta.1 \
  --rows 1000000 --batch-size 250 --writers 8 --readers 4 \
  --idle-seconds 60 --max-load-seconds 3600 \
  --output /private/tmp/brainwire-v08-baseline-01
```

Each output directory must be new. Use a fresh suffix for each attempt. The
image identity is recorded (immutable ID, available digest and revision label),
but the script cannot prove that image was built from the checkout. Qualify a
freshly built image from the intended source revision before drawing performance
conclusions. No inherited company-specific image labels are copied into results.

Default mode measures **image defaults** inside the 4-GiB limit. A separate run
with `--explicit-budgets` sets cache=512MiB, memtables=64MiB, query=256MiB, and
records that profile. Do not compare or combine these profiles as one result.
Budgets are not total process memory limits.

Docker Desktop needs enough available VM memory and disk. Host power/load are
recorded, but not enforced by this harness. Docker must support cgroup v2 with
`memory.peak`; unavailable counters fail closed. The image must provide `sh`,
`cat`, `/proc/1/status` and the repository CLI. No privileged container is used.

## What is measured and verified

- Exact Docker `Memory=4294967296`, `MemorySwap=4294967296`: a 4-GiB hard cap
  with no additional swap allowance. The actual cgroup `memory.max` is checked.
- Periodic `memory.current`, `memory.peak`, `memory.events`, `memory.stat` (including
  cgroup anonymous memory) and server PID 1 `VmRSS`, `RssAnon`, `RssFile`,
  `RssShmem` where present. Cgroup accounting includes page cache; process RSS is
  reported separately. Every sample keeps its phase and monotonic timestamp.
- Eight disjoint incremental MERGE writers, four concurrent point readers. Each
  committed transaction upserts a batch of nodes with a deterministic 512-byte
  payload plus integer sequence, and edges to one seed. One million rows means
  one million nodes, 999,999 edges, plus storage/index overhead. This star topology
  is explicit and is not representative of arbitrary graph degree distributions.
- Timings for the labelled node count, edge count and unlabelled LIMIT query are
  recorded separately as `query_timings_seconds`, including materialization and
  transaction commit. Cursor pages default to 1,000 rows.
- Per-client operation count and p50/p95/p99 end-to-end latency. Writer operation
  means a whole two-query batch transaction, reader operation one point query.
  Includes driver/network/materialization/commit. No managed transaction retry.
- After all acknowledgements, SIGKILL then restart of the same isolated server and
  volume. Exact node/edge counts, deterministic property samples, unlabelled
  `MATCH (n) ... LIMIT 37`, and a complete 1,000-row keyset traversal are checked.
- Post-load idle samples **before SIGKILL** preserve the original process and its
  retained allocator/native memory. Separately labelled recovered-idle samples
  measure the reopened process. Phase summaries distinguish both from ready/load.
- Dynamic Docker host port is reinspected and the Bolt driver recreated after
  restart; endpoint history and sanitized failure stage/readiness errors are recorded.
- An offline `db check --json` after clean stop.
  Any observed OOM/event or wrong cap fails the diagnostic. Peak remains available
  in pre-restart samples even if the recreated cgroup resets its counters.

`report.json` includes pass/fail, client failure types (without driver error text),
image/container identity, counts, telemetry and gaps. `server.log` contains both
Docker stdout and stderr. A private token policy is generated and removed after
the run. Treat logs as private diagnostic artifacts.

## What it does not establish

This is a single-container failure-recovery and memory diagnostic, **not** a
replication, quorum, network-partition, disk-loss, security, or power-loss test.
SIGKILL is not a disk power-cut. Only fully acknowledged transactions are counted;
there is no kill during writes or uncertain-commit reconciliation in this version.
Readers currently target the seed point; they do not stress full scans/algorithms.

The transaction timeout is sent through `begin_transaction(timeout=...)`, but a
separate adversarial test is still needed to prove the server cancels work on the
deadline. The keyset traversal is separate read transactions on a quiescent graph,
not a snapshot cursor or proof of bounded scan allocations. Idle RSS is recorded,
not asserted to equal cache size: graph metadata, allocator retention and other
pools must be explained separately. No claim of <=4-GiB total working set can be
made from a pass until actual receipts and sample coverage are inspected.

```sh
PYTHONPYCACHEPREFIX=/private/tmp/brainwire-v08-pycache \
  python3 -m unittest scripts.test_bench_v08_container -v
```
