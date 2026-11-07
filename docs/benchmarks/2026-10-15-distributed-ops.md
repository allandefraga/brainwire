# Distributed log-cap recovery qualification

Python commands below use the environment from [Python setup](../python-package.md).
These commands use the current checkout's Python dependencies. They do not
recreate the historical dependency environment; original versions remain in the receipts.

The quorum acceptance now has an opt-in `--log-cap-recovery` scenario. It runs
the existing four-row, three-voter loopback acceptance with a 1 MiB retained-log
limit, appends bounded synthetic writes with stable request IDs, and stops only
after the next proposal cannot fit even by its raw property bytes on a majority.
It then restarts the same voters and data directories with a 1 GiB configured
limit, resolves the uncertain commit through quorum-backed request lookup,
retries only after authoritative absence, and audits all three graph stores.

The frozen release binary (`8008c6218a44d678512566c9e26c3bb4741aa306da61c584fd8428ab85ff8572`)
passed the new opt-in scenario with 17 checks. Sixteen explicit writes were
acknowledged. Before the final 16,402-byte raw property proposal, every voter
reported 13,462 bytes remaining. At that exact boundary, a leader readiness
probe still passed its Raft quorum-read barrier, and a fresh status sample
showed the same capacity bound. The following client write received
`CommitOutcomeUnknown`. An immediate observation-only readiness probe on the
rebuilt candidate returned `fatal_storage_error` (exit 1). The status API
reports that stable category without including the underlying message or path;
after restart, quorum lookup established that request had not committed, and
retrying its same request ID applied it exactly once. Replaying an already
committed request was rejected before the callback ran. All three voters
recovered at commit/apply index 45, and their offline logical graph digests
matched. The [four-row opt-in receipt](../../benchmarks/archive/2026-10-15-distributed-ops/log-cap-recovery-four-row.json)
records the per-write capacity samples and cleanup of all processes, proxies and
temporary data.

The ordinary 500-row release acceptance still passes all 16 checks with the
default 1 GiB cap; see its [regression receipt](../../benchmarks/archive/2026-10-15-distributed-ops/default-acceptance-500.json).
Three additional consecutive runs of the original four-row cap-recovery
scenario passed all 17 checks. After adding the fatal-category field, the rebuilt
candidate passed the scenario again and reported `fatal_storage_error` after the
uncertain write. The ordinary 500-row acceptance also passed on that rebuilt
candidate; both final receipts use the same binary hash.
Reproduce both from the repository root:

```sh
.venv-brainwire/bin/python crates/server/tests/quorum_replication.py \
  --binary target/release/brainwire --rows 500
.venv-brainwire/bin/python crates/server/tests/quorum_replication.py \
  --binary target/release/brainwire --log-cap-recovery
```

## Recovery behavior exercised

On an uncertain commit, the test keeps the same request ID and voter data
directories. It stops the processes, raises the configured cap on all voters,
restarts the same identities, then performs quorum-authoritative request lookup.
It retries the uncertain operation only after lookup reports absence, and checks
that the same request ID applies exactly once. A request already reported as
applied is rejected on replay before the callback executes. The general client
procedure and request-key format are documented in
[`Python writes and uncertain commits`](../quorum-replication.md#python-writes-and-uncertain-commits).

This sequence is qualified only in the local acceptance harness. It does not
qualify a production rolling restart, capacity changes in a live three-machine
cluster, or the behavior under disk-full conditions outside the configured
retained-log bound.

This is a bounded local test of three native processes on loopback. It confirms
retained-volume recovery after a configured small cap, not actual exhaustion
under a 1 GiB production-sized cap, a machine loss, container orchestration,
snapshots, log compaction or multi-machine deployment. The passing readiness
probe demonstrates that Raft quorum-read readiness does not promise capacity
for a future write. In this run, readiness failed immediately after the
uncertain write because the local node reported a storage fatal error; recovery
after raising the cap was separately verified.
