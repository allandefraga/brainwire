# Distributed operation follow-up

`cluster inspect` now contacts all three configured voters concurrently. Each
request retains its own existing five-second deadline, the report remains in
voter-ID order, and each member includes its measured `response_time_ms`, even
when the peer is unavailable or returns invalid status. When all initialized
voters report commit and apply indexes, the report also gives sampled
min/max/spread and per-voter lag behind the highest sampled index. Missing or
incomplete samples yield no lag summary. These independent concurrent samples
are not an atomic snapshot and can differ briefly due to timing; inspection
does not claim quorum or write-readiness.

The overlap regression test synchronizes three synthetic peers and verifies
that each receives a request before any peer is allowed to respond. The native
three-process acceptance then passed all 15 quorum, partition, restart, request
deduplication, ACL and offline-audit checks with 500 input rows. The receipt
records binary identity and confirms that all child processes, fault proxies
and temporary data were removed:
[initial release acceptance receipt](2026-10-14-distributed-ops/acceptance-500.json)
and [replication-progress acceptance receipt](2026-10-14-distributed-ops/acceptance-500-replication-progress.json).
The latter observed all three voters at commit/apply index 512 with zero
sampled lag and passed all 15 checks. A third run kept one follower reachable
for `Status` while a loopback fault policy blocked its consensus traffic; after
three commits on the remaining majority, inspect reported a sampled lag of
three committed and applied indexes. Removing the fault allowed the follower
to catch up before later failover/restart checks. It passed all 16 checks; see
the [live lag acceptance receipt](2026-10-14-distributed-ops/acceptance-500-live-lag.json).
A repeat with the same binary reproduced a three-index gap and full catch-up;
the [latest receipt](2026-10-14-distributed-ops/acceptance-500-live-lag-recovery.json)
also records equal committed/applied indexes across all voters after recovery
and confirms cleanup. A prior [repeat receipt](2026-10-14-distributed-ops/acceptance-500-live-lag-repeat.json)
reproduced the lag independently. The recovery poll requires identical commit
indexes and matching applied indexes across all voters. A further release run
passed the same 16 checks, with the [final repeat receipt](2026-10-14-distributed-ops/acceptance-500-live-lag-final-repeat.json).

Reproduce against a local release binary from the repository root:

```sh
uv run --with neo4j==6.3.0 python crates/server/tests/quorum_replication.py \
  --binary target/release/brainwire --rows 500
```

This ran three local native processes with loopback fault proxies. It does not
qualify Docker, Kubernetes, Swarm or multi-machine routing. Snapshots, log
compaction, dynamic membership and replacement of an erased voter remain
unsupported.
