# Cooperative RUN admission under cgroup pressure

Status: candidate implementation; this does not establish a process RSS ceiling.

## Problem

Query and ingress reservations do not cover all native allocations, existing
results, RocksDB or neighboring processes sharing a cgroup. Admitting another
query when the enforced container hierarchy is almost full can increase OOM risk.

## Alternatives and decision

Combining the two accounting pools alone would not account for new allocations.
An address-space rlimit could fail native mmap/allocation unpredictably and is
not an RSS limit. Instead, add a cooperative admission check for new Bolt RUNs
using current memory consumption at every visible Linux memory-controller
ancestor. Check before cloning the query and parameters and again in the query
worker before converting parameters or executing the query.

Read both limit and usage each time; the controller limit can change. For cgroup
v2 use memory.current and memory.max; for v1 use memory.usage_in_bytes and
memory.limit_in_bytes. At or above 95% of any finite ancestor limit, reject the
RUN with Neo.TransientError.General.MemoryPressure. The error follows existing
transaction cleanup and discards all uncommitted writes in that transaction.
Direct engine callers also receive that rollback before the error returns.
Existing results, COMMIT, ROLLBACK, RESET, DISCARD, PULL and connection cleanup
remain available so pressure does not prevent completing or freeing work.

Discovery reuses the capacity parser's validated visible ancestry, captured
once per process; a cgroup migration needs a process restart to rediscover it.
Use the working set: controller usage minus `inactive_file` (`total_inactive_file`
on v1) from `memory.stat`. Raw usage sits near the limit once files were read or
written, since the kernel reclaims page cache only at the limit: measured on a
2 GiB container with 671 MiB anonymous memory, raw usage stayed at 2047 MiB and
every RUN was refused until the cached file was deleted. A missing `memory.stat`
counts the raw usage (conservative). tmpfs/shmem is not reclaimable and still
counts. A parent
can be pressured by neighbors while the leaf is not. Reading only the smallest
maximum would miss that condition.

## Missing evidence and risks

Absent/malformed controller data, no finite limit, unsupported platforms and
only visible controllers are sampled; hidden ancestors cannot be evaluated.
Admission remains available in that case; existing query and ingress budgets
still apply. The probe is not a safety certificate or a guaranteed OOM barrier.
Queries already in flight, background compaction, native allocations and races
between checks can exceed remaining memory. Cleanup and COMMIT can allocate and
are deliberately not blocked. Synchronous controller reads add work per RUN.

## Verification

Deterministic tests exercise parent pressure with a relaxed child, v1/v2,
dynamic limits, exact thresholds, overflow, missing data, rollback of prior
writes, the second worker check, pressure relief, RESET and draining/committing
existing results. Separate multi-process cgroup stress and 2/4/8 GiB sustained
qualification remain required before any production memory guarantee.
