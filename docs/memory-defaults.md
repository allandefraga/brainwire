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
