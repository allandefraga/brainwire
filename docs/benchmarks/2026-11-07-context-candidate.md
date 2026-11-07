# Context candidate: local memory and recovery diagnostics

These are synthetic checks of a local v0.17.0-beta.1 candidate image
(`sha256:5cef6e0d2b18da02e0d3e394845fdc37a29b30acca50d19227dcf3c031fbe5d3`).
The host was shared with other containers and short integration checks; these
are not performance comparisons or production service-level objectives.

- [512 MiB pressure receipt](2026-11-07-context-pressure-512.json): cgroup-v2
  pressure was injected with tmpfs. Admission rejected a new RUN under
  pressure, and the recorded cgroup counters reported no OOM. This is a short
  synthetic check, not a hard RSS ceiling.
- [One-hour mixed-load receipt](2026-11-07-context-mixed-1h.json): one local
  standalone container with 2 CPUs and a 2 GiB cgroup limit handled concurrent
  reads and writes on 100,000 nodes and 100,000 relationships. Every full
  one-minute window recorded successful reads and writes. After acknowledged
  writes stopped, the runner verified all 100,000 rows, sent SIGKILL, restarted
  with the same volume and verified every row again. It observed no OOM event.

The mixed run sampled at ten-second intervals: the highest sampled cgroup
`memory.current` was 553,312,256 bytes, the highest reported cgroup
`memory.peak` was 562,425,856 bytes, and the highest sampled process RSS was
461,041,664 bytes. Sampling can miss RSS peaks. The test does not simulate a lost
commit acknowledgement, disk loss, power loss, quorum or machine failover.
It ran concurrently with the separate pressure check and isolated application
smokes on the shared host. Sustained representative workloads and 2/4/8 GiB
qualification remain open.

To repeat the checks with a locally built candidate image and the Neo4j Bolt
Python driver installed:

```sh
python crates/server/tests/cgroup_pressure_smoke.py --image YOUR_IMAGE \
  --memory-mib 512 --output /tmp/pressure-receipt.json
python scripts/bench_mixed_recovery.py --image YOUR_IMAGE --rows 100000 \
  --batch 250 --seconds 3600 --deadline 4200 --memory 2 \
  --sample-seconds 10 --pause 0.02 --output /tmp/mixed-receipt.json
```

Both runners create and remove their own disposable Docker resources. They
require Docker and a Linux cgroup-v2 environment; do not run them against a
production graph.
