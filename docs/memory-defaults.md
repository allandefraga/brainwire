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
fit the sampled headroom. See [the design](design/process-memory-ceiling.md).

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

## Shared Bolt input admission

`memory.ingress` limits the combined assembled-message buffer capacity and
PackStream decoded-storage charges across all connections to this server,
including clients that have not authenticated. It is separate from
`memory.query` and the RocksDB budgets. Unset, it uses one eighth of the detected
memory limit, capped at 512 MiB; without detection it defaults to 256 MiB.
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

The synthetic 512 MiB admission smoke compares the same working-set estimate
as the server and records raw usage separately for the injection safety limit.
It only accepts a structured cgroup refusal as a confirmed cgroup rejection;
missing or different evidence is inconclusive. The previous failed run remains
unexplained, and this test does not qualify sustained memory use or an RSS cap.

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

New Bolt RUNs are checked before copying query parameters and again before
execution. If the working set (controller usage minus the inactive file cache
in `memory.stat`, which the kernel reclaims before it OOMs) reaches 95% of any
finite visible cgroup ancestor limit, the RUN fails with
`Neo.TransientError.General.MemoryPressure`; uncommitted writes in its
transaction are rolled back. Reduce concurrent work and reconcile application
state before retrying. Existing PULL/DISCARD results, COMMIT, ROLLBACK, RESET
and connection cleanup remain available under pressure.

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
