# Memory defaults in containers

`brainwire config show` and every command that loads server configuration use
host/container-aware defaults for unset memory fields. On Linux, Brainwire reads
`/proc/self/cgroup` and `/proc/self/mountinfo`, checks the process's cgroup and
visible ancestors (`memory.max` in v2, `memory.limit_in_bytes` in v1), and uses
the smallest finite limit or physical host memory, whichever is smaller.
Unlimited, missing or malformed limit files do not supply a usable bound; host
physical memory is the fallback. Ancestors hidden by a cgroup namespace cannot
be inspected. On macOS, physical host memory comes from `hw.memsize`.

| Effective memory | Cache | Memtables (inside cache) | Query reservations |
| --- | --- | --- | --- |
| 1 GiB | 256 MiB | 64 MiB | 128 MiB |
| 2 GiB | 512 MiB | 128 MiB | 256 MiB |
| 4 GiB | 1 GiB | 256 MiB | 512 MiB |

Cache uses one quarter of detected memory, query reservations one eighth, and
memtables one sixteenth. **Memtables are charged inside the cache**: do not add
them again when summing the budgets. Defaults leave five eighths for other
allocations. TOML overrides defaults per field; environment overrides TOML per
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
