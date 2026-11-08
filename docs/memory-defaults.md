# Memory defaults in containers

`brainwire config show` and every command that loads server configuration use
host/container-aware defaults for unset memory fields. On Linux, Brainwire reads
`/proc/self/cgroup` and `/proc/self/mountinfo`, checks the process's cgroup and
visible ancestors (`memory.max` in v2, `memory.limit_in_bytes` in v1), and uses
the smallest finite limit or physical host memory, whichever is smaller.
Unlimited, missing or malformed limit files do not supply a usable bound; host
physical memory is the fallback. Ancestors hidden by a cgroup namespace cannot
be inspected. On macOS, physical host memory comes from `hw.memsize`.

| Effective memory | Cache | Memtables (inside cache) | Query reservations | Bolt ingress | Accounted total |
| --- | --- | --- | --- | --- | --- |
| 1 GiB | 256 MiB | 64 MiB | 128 MiB | 128 MiB | 512 MiB |
| 2 GiB | 512 MiB | 128 MiB | 256 MiB | 256 MiB | 1 GiB |
| 4 GiB | 1 GiB | 256 MiB | 512 MiB | 512 MiB | 2 GiB |

Cache uses one quarter of detected memory, query reservations one eighth, and
memtables one sixteenth. **Memtables are charged inside the cache**: do not add
them again when summing the budgets. Bolt ingress defaults to one eighth,
capped at 512 MiB. For the 1/2/4 GiB examples, cache + query + ingress budgets
total half of detected capacity; the ingress cap lowers that fraction on larger
hosts. The remaining capacity is not measured free memory or proven workload
headroom. TOML overrides defaults per field; environment overrides TOML per
field. `config show` identifies the detected source, file or environment variable.
`config init` leaves memory examples commented so they do not disable detection.

Automatic cache sizing rejects a detected limit below 256 MiB rather than
silently allocating a minimum cache above the derived budget. Explicit memory
fields retain their exact configured values, including existing files with
10 GiB cache values; they are never rescaled or rejected solely for exceeding
the detected limit. Operators must review those choices when moving into a
small container. Partial overrides leave other fields at detected defaults;
for example, reducing cache may also require explicitly reducing memtables to
satisfy the existing memtables-inside-cache validation. Existing validation
for a minimum 64 MiB cache and positive query budgets still applies.
If neither host memory nor cgroup bounds can be detected, legacy fixed defaults
remain; inspect `config show` and configure explicit budgets on unsupported hosts.

`memory.limit` (default 90% of the detected capacity) is a **sampled admission
threshold**, not a hard process RSS ceiling. On Linux and macOS, the server
samples RSS at startup and every 100 ms. Crossing 80% of this threshold warns
and requests an allocator purge; at 95%, new RUNs and query-pool growth are
refused. A request may be refused earlier when its shared reservations cannot
fit the sampled headroom. RUN admission also samples RSS on demand and uses
the higher of that sample and the latest periodic sample. On supported RSS
platforms, a failed reading does not refresh the sample: when no successful
reading is available from the past second, new RUNs fail with
`reason=process_monitor` and reservation headroom closes. A fresh reading
reopens admission when the previously observed pressure has cleared. An
already running query can instead receive `reason=process_limit` when its
headroom is unavailable. This does not check every allocation or eliminate
sampling races. On platforms without a supported RSS reader, the server logs
a warning and RSS admission remains best effort.
When RSS falls after a spike, periodic sampling can restore headroom even if
new RUNs arrive during sampling. A higher concurrent RUN observation is retained
for that sampling window.
For cgroup admission, the server reads the inactive file-cache counter on both
sides of the usage reading and discounts the smaller value. If either cache
reading is missing or inconsistent, it discounts no cache. These readings are
not atomic; they reduce one source of undercounting but cannot guarantee a
process-wide ceiling or explain an earlier untraced decision.
See [the design](design/process-memory-ceiling.md).

The component budgets are accounting budgets. Sampling, admission and
allocator decay do **not guarantee a global RSS limit or prevent every OOM**.
RocksDB native allocations, message decoding and encoding, operating system page
cache, connections and other application allocations consume additional memory.
A container's memory limit remains the operating system's enforcement mechanism.
Cgroup discovery tests alone do not certify a workload under a 4 GiB limit;
validate representative workloads and concurrent clients separately.

## Check the container before increasing load

The Compose demo sets a 2 GiB Docker limit, a 256 MiB cache, 32 MiB memtables
inside that cache, and a 256 MiB query pool. Ingress and `memory.limit` still
use detected defaults. Inspect the effective configuration in the running
container rather than assuming a host configuration file was loaded:

```sh
docker compose -p brainwire-demo -f deploy/compose.demo.yaml \
  exec brainwire brainwire config show
docker compose -p brainwire-demo -f deploy/compose.demo.yaml logs --tail 100 brainwire
docker stats --no-stream
```

`docker stats` reports container accounting; it is not the same number as
process RSS or live query reservations. Compare it with Brainwire's memory
logs and the detected capacity in `config show`. Explicit environment values
win over automatic sizing: lowering only Docker's limit does not shrink a
previously configured cache. Recreate the container after changing its limits
or environment, then check the configuration again.

For example, to make the process-admission setting explicit in a separate
`docker run` deployment, pass `--memory 2g` and
`--env BRAINWIRE_MEMORY_LIMIT=1536MiB`, alongside your cache, query and ingress
budgets. This lowers the sampled admission threshold; it does not qualify the
workload or reserve that amount of RAM. No single setting fixes every large
batch. Start with bounded batches and a few clients, then measure peak and
idle memory while increasing the representative load.

A `Neo.TransientError.General.MemoryPressure` refusal means reduce concurrent
work or batch size. A missing write acknowledgement is different: reconcile
its outcome before retrying. Container restart after a kernel OOM is recovery,
not evidence that the load is safe. Sustained 2/4/8 GiB production-load
qualification remains open for this beta.

For design context and the limits of comparisons with other graph databases,
see [memory reference notes](design/memory-reference-notes.md).

## Shared Bolt input admission

`memory.ingress` limits the combined assembled-message buffer capacity and
PackStream decoded-storage charges across all connections to this server,
including clients that have not authenticated. It is separate from
`memory.query` and the RocksDB budgets. Unset, it uses one eighth of the detected
memory limit, capped at 512 MiB; without detection it defaults to 256 MiB.
When process memory admission is enabled, positive ingress reservations also
respect the sampled process headroom. If RSS monitoring is stale, growth can
be refused until observations recover. Ingress and query keep their own limits
but share an atomic admission ledger against sampled headroom. Concurrent input
and query reservations cannot each spend the same allowance. Results keep their
credit while retained; release and bounded RESET recovery remain possible when
headroom is zero. Setting `memory.limit = 0` disables this process-headroom
coordination, while component budgets still apply. This does not establish a
global RSS ceiling.
Set `[memory] ingress = "128MiB"` in TOML or
`BRAINWIRE_MEMORY_INGRESS=128MiB` in the environment. `config show` reports its
value and source. Explicit values must be positive and fit the platform address
size. Message size and decoded-storage caps still apply independently.

Admission occurs before growing the assembled-message buffer or allocating
charged PackStream storage. Fragmented input reserves the planned buffer
capacity, including geometric growth slack. During a reallocation it reserves
both the old and replacement capacity, then releases the old charge after
growth succeeds. This can refuse a batch whose logical payload would fit but
whose temporary allocation overlap would not. A request that cannot fit returns
`Neo.TransientError.General.MemoryPressure` with `reason=ingress` and a size
hint before it reaches the engine. A logged-on session stays open: the rest of
the message is skipped, its transaction is rolled back and RESET recovers it;
an unauthenticated connection closes. Reduce batch sizes and use bounded
backoff after a confirmed rejection. This error is different from an unknown
commit outcome; never retry a write blindly after a lost acknowledgement.
Existing sessions can still use bounded recovery messages such as RESET when
headroom falls to zero; new authentication may fail while memory is pressured.

Partial-message accounting is released on disconnect, timeout or rejection.
Rejected input is discarded before waiting for the small failure response.
A valid request keeps its admission through synchronous engine handling, even
if the socket task is cancelled while that worker remains running.

This is a shared accounting ceiling, not a process RSS limit. Socket/read and
framing buffers, allocator size-class rounding and overhead, engine parameter
copies, output encoding, catalog and native RocksDB allocations remain outside
this budget. An input may need both its encoded size and decoded allocation
estimate simultaneously, so a payload smaller than `memory.ingress` may still
be rejected. Validate representative batches and concurrent workloads under
real container limits before production deployment.

## Configured budget diagnostic

`brainwire config show` prints a memory budget summary after the per-field values
and sources. Its fields include `configured_budget_bytes`,
`detected_capacity_bytes`, `capacity_kind`, `capacity_source` and `comparison`.
The sum is cache + query + ingress; memtables are included inside cache and are
not added again. Capacity and its typed host/cgroup source come from the same
discovery used for defaults, even if every memory field is overridden.

The comparison is `within_detected_capacity`, `exceeds_detected_capacity`,
`unknown` when capacity was not discovered, or `sum_overflow` when adding the
configured budgets exceeds the u64 byte representation. Unknown capacity and
arithmetic overflow are not rendered as a safe zero-byte budget. Physical host
capacity is not a per-process limit; a cgroup limit can also cover other
processes. Hidden ancestors and changes after configuration load remain outside
this snapshot.

`serve` warns if the configured sum exceeds detected capacity or the sum
overflows, including when capacity is unknown. Explicit overrides remain unchanged. The
warning does not reject startup or block offline `db check`, checkpoint or
restore workflows. Unknown capacity alone does not produce a warning.
`within_detected_capacity` compares only three configured budgets; it does not
include every allocation, prove an RSS ceiling or establish workload headroom.

When detected host and cgroup capacities are identical, the current minimum
selection may report the host source. This diagnostic is advisory; do not use
its displayed source as an enforcement decision without separately retaining
and checking the cgroup limit.

## Admission under Linux cgroup pressure

Cypher list quantifiers, comprehensions and `reduce` account for temporary
binding copies before evaluating each item. Temporary accounting resets per
iteration while retained results remain charged; the shared reservation can
remain at its high-water mark until the query or transaction finishes.
Quantifiers also check deadlines during iteration. These measures do not create
a process-wide RSS ceiling.

`replace()` accounts for the actual UTF-8 output size before constructing it;
an absent pattern no longer reserves a hypothetical expanded string. A large
real expansion can still be refused. Timeout checks remain cooperative.
`split()` also accounts for each output segment before allocating its result;
it scans the input twice and may refuse large output under the query budget.
`size()`, `isEmpty()` and `exists()` inspect already-evaluated arguments
without an additional deep copy. Argument evaluation itself can still copy data.
Projection and aggregate `DISTINCT` admit retained key payloads before cloning
them; grouped projections account for both retained key copies, and `UNION`
accounts for the copies used to deduplicate branches. These are conservative
query-accounting checks, not a complete allocator or RSS limit. Output list
conversion now reserves exact result capacity before allocation. For scalar
lists this can temporarily hold a second buffer rather than reusing the input
buffer; that overlap is charged to the query budget. `UNION ALL`
does not create that deduplication set.

A fresh RUN RSS reading that exceeds the current sample also reduces the
shared reservation headroom until a later sampler reading confirms relief. A
sampler read already in progress cannot replace an intervening higher reading.
This narrows one sampling window; it is not a process-wide memory ceiling.

The synthetic admission smoke supports 512 MiB, 2 GiB and 3 GiB limits. It compares the same working-set estimate
as the server and records raw usage separately for the injection safety limit.
It only accepts a structured cgroup refusal as a confirmed cgroup rejection;
missing or different evidence is inconclusive. The previous failed run remains
unexplained, and this test does not qualify sustained memory use or an RSS cap.
The [v0.14.0-beta.4 local candidate receipt](benchmarks/2026-11-03-cgroup-014b4.json)
records two cgroup refusals with rollback and a successful offline graph audit on
Linux arm64. It identifies the local image, not the published image digest.
The [v0.15.0-beta.5 local candidate receipt](benchmarks/2026-11-04-cgroup-015b5.json)
records a repeat of this synthetic smoke on a locally built image; it does not
establish sustained-load behavior or explain the earlier failure.
The [beta.9 candidate receipt](benchmarks/2026-11-04-cgroup-015b9.json) records
another passing run with the same scope and limitations.
The [beta.11 candidate summary](benchmarks/2026-11-04-cgroup-015b11.json)
records a fresh synthetic pressure rejection, rollback and recovery.
The [beta.12 investigation](benchmarks/2026-11-05-pressure-investigation.md)
records a failed smoke, controlled repeats and the optional decision trace;
the original failure remains unexplained.
In a separate [small 30-minute read soak](benchmarks/2026-11-04-soak-015b9.json),
the median process RSS rose by about 53 MiB between the first and last
five-minute windows. This observation needs investigation; it does not prove
a leak or qualify sustained production memory use.

Decision logs include the selected cgroup's raw usage, inactive file cache,
effective working-set estimate and limit when the cgroup check refuses a RUN. If any examined cgroup has raw usage
at least 95% but subtracting inactive file cache permits admission, a separate
`cgroup_allowed_after_inactive_file` diagnostic records the first such cgroup's
inputs, independently of which cgroup has the largest effective usage ratio.
No admission diagnostic is emitted if another cgroup rejects the RUN or required
inputs are unavailable. Each stream
logs at counts 1, 2, 4, 8 and so on. Only numerical memory fields are logged;
query text and credentials are not included. This reuses the admission reads
without additional filesystem I/O. Values come from sequential reads at that
check, not one atomic kernel snapshot. They explain the cgroup check, not whether
later authorization, query or storage work succeeds. Missing/unreadable required limit or usage
inputs remain unavailable and do not manufacture an admission sample. Missing
inactive-file data retains the existing conservative fallback of zero reclaimable bytes. These
logs do not diagnose older receipts retroactively or establish a global RSS cap.

For a short controlled investigation, set `BRAINWIRE_TRACE_CGROUP_ADMISSION=1`
before starting the server. It logs at most 64 pressure-guard decisions using
the readings already taken, without query text, identities or filesystem paths.
An allowed pressure-guard decision does not mean the query or its authorization
succeeded. This opt-in writes synchronously to stderr, which can perturb timing
and memory; leave it off for normal operation. It is not a continuous trace or
an atomic measurement.

New Bolt RUNs are checked before copying query parameters and again before
execution. If the working set (controller usage minus the inactive file cache
in `memory.stat`, which may be reclaimable) reaches 95% of any
finite visible cgroup ancestor admission limit, the RUN fails with
`Neo.TransientError.General.MemoryPressure`; uncommitted writes in its
transaction are rolled back. Reduce concurrent work and reconcile application
state before retrying. Existing PULL/DISCARD results, COMMIT, ROLLBACK, RESET
and connection cleanup remain available under pressure.
Their own execution and reservation limits still apply, including the staged
commit guard described below.

On cgroup v2, the admission limit is the lower finite value of `memory.high`
and `memory.max` at each visible ancestor. If `memory.high` is missing,
unreadable, malformed or `max`, the check falls back to `memory.max`. Both
values are reread on each check. The diagnostic identifies whether `High` or
`Max` supplied the selected limit. This does not prevent kernel throttling:
the kernel uses its own accounting, samples are not atomic, and in-flight work
continues. Startup budget sizing still uses capacity, not `memory.high`.

The process shares one discovered controller ancestry. Limits and current usage
are reread on each check, but a later cgroup migration needs restart to
rediscover paths. Linux v1/v2 are supported; on other platforms or when visible controller data
is missing or malformed, pressure monitoring is unavailable and the existing
reservation controls remain active. Only visible controllers are sampled;
hidden ancestors cannot be evaluated. Unavailable data never means zero
usage or a proven safe amount of free memory. Controller usage includes memory
other than process RSS and may include neighboring processes.

This is cooperative query admission. In-flight work, native allocation,
compaction and timing races still make a global RSS/OOM guarantee unproven.
See [the admission design](design/cgroup-run-admission.md).

Startup prints a sampled admission status and the shared `denied_runs` and
`unavailable_samples` counters. Later diagnostic messages are emitted only at
power-of-two counter values to avoid one log per rejected request. A startup
probe counts as one sample; reading the counter snapshot does not sample again.
These counters are not an HTTP or Prometheus endpoint. Raw controller usage
includes reclaimable file cache: conservative admission can keep rejecting RUNs
until usage falls, even when some pages could be reclaimed. Unknown monitoring
fails open for admission while existing query/input reservations remain active.

## Cypher temporary values

The `reverse`, `head`, `last`, `tail` and `valueType` functions avoid an
extra deep copy of their already-evaluated argument. Some inputs and results
still need to be materialized, and normal query reservations continue to
apply. This improvement is not a process-wide RSS ceiling.

## Staged writes at commit

Before a Cypher write transaction commits, it reserves a conservative estimate
for materializing its staged overlay into the storage batch. Replication
preparation uses the same estimate. A transaction admitted during `RUN` can
therefore still be refused at commit by the query limit or shared pool; that
refusal happens before writing and discards its staged changes. Read-only
completion does not need this write-buffer reservation.
Internal ID ranges durably reserved during `RUN` can remain allocated after a
refusal; the refused commit does not publish its graph records or request receipt.

The estimate is three times the accounted overlay and dependency bytes. It is
a reservation floor, not a measurement of every native allocation. Existing
transaction high-water credit can cover it without new growth; pressure is
sampled when reservation growth needs admission, not on every commit. Live
result owners can retain their credits after transaction completion. These
controls do not guarantee a process RSS ceiling or preempt storage I/O.
