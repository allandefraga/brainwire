# Linux cgroup pressure admission: synthetic smoke

This is a narrow functional test, not an RSS ceiling, OOM guarantee, throughput
benchmark or qualification of 2/4/8 GiB workloads. It runs one Linux container
on Docker Desktop with an isolated graph and deliberately increases cgroup usage
using a temporary tmpfs file. Cgroup usage includes file cache and is not RSS.

The candidate image was built from the local worktree (which contained
uncommitted changes), on Docker 29.7.2 Linux/arm64 under macOS arm64. Image ID:
`sha256:3bd48e84a47a23f7f25e1596e94c3e9ba3d1ebe5bfe6cfa3086e8a21741da9e8`.
It is a local test image, not a published release or a source commit identifier.
The container limit was 512 MiB with no swap.

The [first attempt](initial.json) recorded a failed second assertion after the
first transaction was correctly rejected. Its last pressure sample was only
0.69 MiB above the boundary. Rollback and temporary probe processes can change
usage around the threshold; the attempt did not establish a guard bypass. The
[repeat](repeat.json) used a 96% injection target and samples around both RUNs.

In that repeat, both an existing transaction and a new session received
`Neo.TransientError.General.MemoryPressure`. After the owned tmpfs file was
removed, normal queries resumed; the previously acknowledged node remained,
and neither the pending nor rejected node was present. An offline `db check`
returned `ok`. Collected cgroup counters showed no OOM, Docker reported
`OOMKilled=false`, and no Docker `oom` event was collected. Host kernel logs
were not independently verified. These observations apply only to this run.

## Reproduce

Build a candidate image separately. Then, with Docker and Python 3.12 plus the
Python Bolt driver available, run:

```sh
python crates/server/tests/cgroup_pressure_smoke.py \
  --image YOUR_LOCAL_IMAGE --output /tmp/brainwire-pressure-new.json
```

For a larger isolated smoke, add `--memory-mib 2048` or `--memory-mib 3072`.
These tests deliberately fill tmpfs near the selected cgroup limit: verify Docker
host memory is available and run them serially. They do not qualify a 2/3 GiB
production workload.

Use a new output path. The script creates uniquely labelled temporary resources,
uses a random loopback port, cleans up only its own container and volume, and
stops if the measurement or injection is unsafe or unavailable. It may return
`INCONCLUSIVE` if pressure drops around a query. Review raw receipts alongside
the [memory guide](../../memory-defaults.md) and the
[production roadmap](../../production-readiness-roadmap.md).
