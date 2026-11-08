# Recovery and memory checks: v0.18 candidate

These are small synthetic checks on a shared development host. They are not
capacity benchmarks or production guarantees.

- [Interrupted commit](2026-11-08-uncertain-commit-v018b1.json): a protected
  native single-node server received COMMIT without the client consuming its
  response, followed by SIGKILL and restart. Across three cycles the batch was
  absent once and present twice. Reconciliation checked the complete node and
  relationship sets, indexed values and existing acknowledged data. The test
  does not pinpoint the signal within fsync or simulate power/disk loss.
- [512 MiB pressure](2026-11-08-pressure512-v018b1.json) and
  [2 GiB pressure](2026-11-08-pressure2g-v018b1.json): separate Linux cgroup-v2
  containers with no swap filled temporary memory to trigger RUN admission.
  The server rejected new work, recovered after pressure was removed and
  reported no cgroup OOM event. Both used a persistent sampling observer and
  bounded admission tracing, which can affect timing.

The pressure checks used a local candidate image. The interrupted-commit test
used a locally built debug binary, not the image. Other host services were
running. None of these checks establishes a process-wide RSS ceiling, tests
sustained production-scale 2/4/8 GiB workloads, or qualifies multi-machine
failover. A lost response remains an unknown outcome to be reconciled, not a
reason for an automatic blind retry.

Repeat the checks using a local image, an isolated disposable Docker environment,
and the Python Bolt driver:

```sh
python crates/server/tests/uncertain_commit.py --binary /path/to/brainwire --rows 100
python crates/server/tests/cgroup_pressure_smoke.py --image YOUR_IMAGE \
  --memory-mib 512 --persistent-observer --trace-admission --output /tmp/pressure-512.json
python crates/server/tests/cgroup_pressure_smoke.py --image YOUR_IMAGE \
  --memory-mib 2048 --persistent-observer --trace-admission --output /tmp/pressure-2g.json
```

The runners create and remove their own temporary data and resources. Do not
run against a production graph.
