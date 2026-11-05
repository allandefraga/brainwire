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
3. **Reservation checks sampled headroom.** With `memory.limit` enabled, query
   and Bolt ingress retain their component limits and share an atomic ledger
   against sampled process headroom. Growth must fit both the component budget
   and the common total before allocation. If the component refuses growth,
   the tentative common credit is released. Workers transfer existing credits;
   results keep credit after their transaction ends and release it when dropped.
   Already resident reservations may be counted twice, so admission is
   deliberately conservative. Both use the
   [memory refusal contract](memory-error-contract.md).
4. **Visibility and return.** Crossing 80% and 95% (upwards, with hysteresis)
   logs one line with RSS, limit, cache, memtables and query reservations; at
   80% the sampler also asks jemalloc to purge every arena immediately.

## Not covered

Admission checks apply at reservation points, so an allocation that has no
reservation (RocksDB compaction buffers, socket buffers) can still pass it
between samples. The kernel stays the last line; the 5% margin and the purge
do not guarantee avoidance of that path. Memory added by other processes in the same
cgroup is handled by separate cgroup admission.

## Scope of the shared ledger

The shared ledger closes double spending between query and ingress reservations.
It is connected before accepting clients when `memory.limit` is enabled.
Zero headroom prevents growth but permits releasing credit and bounded RESET.
It still uses sampled RSS and does not account for every RocksDB, allocator or
socket allocation. It does not guarantee a hard process-wide RSS ceiling.
