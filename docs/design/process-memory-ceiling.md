# Process memory admission

Status: experimental beta; cooperative admission with sampled memory evidence.
It does not establish a hard RSS ceiling or production qualification.

## Problem

The memory budgets (`cache`, `query`, `ingress`) are accounting pools. Nothing
looked at the process's real resident memory, so the allocator's fragmentation,
glibc's per-thread arenas and memory freed but never returned to the kernel
stayed invisible. External reports described 3.62 GiB idle RSS after a load in
a 4 GiB container and kernel OOM kills at 4 and 8 GiB with `OOMKilled=false`.
These observations were not independently reproduced during beta review; see
the [qualification report](../../PRODUCTION-v0.12.md).

## Decision

1. **jemalloc is the process allocator** on non-MSVC targets, unprefixed where
   supported so RocksDB's C++ allocations use it too. At startup the server
   attempts to enable jemalloc's `background_thread` where supported, applying
   its dirty/muzzy decay (10 s defaults) while idle. This permits returning
   unused pages; peak and idle RSS still need workload-specific measurement.
2. **`memory.limit`** (`BRAINWIRE_MEMORY_LIMIT`): an advisory threshold for cooperative admission, not a hard process ceiling. Default:
   90% of the detected cgroup/host capacity; `0` disables it. A sampler thread
   reads process RSS every 100 ms using `/proc/self/statm` on Linux and
   `proc_pid_rusage` on macOS. Other supported targets use jemalloc
   `stats.resident`, which is only a partial allocator estimate; sampling may
   also be unavailable.
3. **Reservation checks sampled headroom.** Let
   `headroom = max(0, 95% of memory.limit - sampled RSS)`. Query-pool growth
   atomically requires `all live reserved bytes + extra <= min(pool limit,
   headroom)`. This compares the shared total, so concurrent reservations cannot
   each spend the same headroom. Reservations already resident in RSS are
   deliberately counted twice; this conservative rule can refuse early.
   A RUN is refused at admission when sampled RSS is already at or above 95%.
   Bolt ingress also checks its own aggregate reservations against sampled
   headroom before positive buffer growth. Ingress and query reservations do
   not share a single atomic pool; they can both observe the same headroom.
   Both use the [memory refusal contract](memory-error-contract.md).
4. **Visibility and return.** Crossing 80% and 95% (upwards, with hysteresis)
   logs one line with RSS, limit, cache, memtables and query reservations; at
   80% the sampler also asks jemalloc to purge every arena immediately.

## Not covered

Admission checks apply at reservation points, so an allocation that has no
reservation (RocksDB compaction buffers, socket buffers) can still pass it
between samples. The kernel stays the last line; the 5% margin and the purge
do not guarantee avoidance of that path. Memory added by other processes in the same
cgroup is handled by separate cgroup admission.

## Next step: coordinate query and ingress reservations

The two pools currently compare their totals independently with the same
sampled headroom. A query and incoming request can therefore both reserve the
same allowance before RSS is sampled again. A future shared ledger should give
both pools one atomic total while retaining each pool's own limit. Each growth
must acquire both credits before allocating; if either acquisition fails, it
must release the other. Worker and result ownership transfers must transfer
credits without reserving or releasing them twice. RESET and release must still
work when headroom is zero.

A deterministic concurrency test should allow only one of two simultaneous
60-byte reservations against 100 bytes of shared headroom while the winner
keeps its credit. Additional tests
must cover pool-limit failure, cancellation, result lifetime beyond a
transaction, worker credit transfers and zero outstanding credits after cleanup.
Even this change would not account for every RocksDB, allocator or socket
allocation, and sampling races would remain; it would not by itself guarantee a
hard RSS ceiling.
