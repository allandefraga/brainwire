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

These are accounting budgets, **not a hard process RSS limit or an OOM guarantee**.
RocksDB native allocations, message decoding and encoding, operating system page
cache, connections and other application allocations consume additional memory.
A container's memory limit remains the operating system's enforcement mechanism.
Cgroup discovery tests alone do not certify a workload under a 4 GiB limit;
validate representative workloads and concurrent clients separately.

## Shared Bolt input admission

`memory.ingress` limits the combined logical assembled message bytes and
PackStream decoded-storage charges across all connections to this server,
including clients that have not authenticated. It is separate from
`memory.query` and the RocksDB budgets. Unset, it uses one eighth of the detected
memory limit, capped at 512 MiB; without detection it defaults to 256 MiB.
Set `[memory] ingress = "128MiB"` in TOML or
`BRAINWIRE_MEMORY_INGRESS=128MiB` in the environment. `config show` reports its
value and source. Explicit values must be positive and fit the platform address
size. Message size and decoded-storage caps still apply independently.

Admission occurs before copying an assembled chunk or allocating charged
PackStream storage. A request that cannot fit returns
`Neo.TransientError.General.IngressMemoryPoolExhausted` and its connection
closes before the request reaches the engine. Reduce batch sizes and use bounded
backoff after a confirmed rejection. This error is different from an unknown
commit outcome; never retry a write blindly after a lost acknowledgement.

Partial-message accounting is released on disconnect, timeout or rejection.
Rejected input is discarded before waiting for the small failure response.
A valid request keeps its admission through synchronous engine handling, even
if the socket task is cancelled while that worker remains running.

This is a shared accounting ceiling, not a process RSS limit. Socket/read and
framing buffers, vector capacity slack, allocator overhead, engine parameter
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

New Bolt RUNs are checked before copying query parameters and again before
execution. If raw controller usage reaches 95% of any finite visible cgroup
ancestor limit, the RUN fails with
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
