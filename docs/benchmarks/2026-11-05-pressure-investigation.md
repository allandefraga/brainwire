# Pressure-admission investigation: beta.12 candidate

This is a short synthetic diagnostic in a 512 MiB Docker cgroup, not a process
RSS ceiling or sustained-capacity qualification. The tested local image ID was
`sha256:ea9136ef1fc6b3c3b1f4a0bb7f4a18a110403ea3dcc3d1e2607adb792a8634f8`;
it was built before the optional decision trace and release commit; it is not
a published digest.

## Original result: FAIL

The standard smoke admitted a RUN when separate external samples immediately
before and after it estimated the working set at 512,614,400 and 515,383,296
bytes. The 95% threshold was approximately 510,027,366 bytes. The receipt did
not capture the reading used by the server at its decision. No cgroup OOM was
observed, and owned test resources were removed. Sampling is sequential and the
receipt does not prove the working set was above the threshold continuously.

This failed result is retained. Its cause is **not established**.

## Follow-up observations

In a separate low-load experiment without the database, a persistent shell
using built-in reads was compared with an independent `docker exec cat` read.
Across 20 pairs, the latter exceeded the larger neighboring shell reading by a
median 468,992 bytes and a maximum 9,302,016 bytes. Different sample times and
processes limit the comparison; it does not establish the cause of the failed
RUN. It shows that the observer can perturb a small threshold margin.

Three separate repeats on the same local image used a persistent observer.
Both a transaction RUN and an independent RUN were refused with structured
`MemoryPressure` / `cgroup` / rollback evidence; the graph recovered and an
offline audit passed. One repeat disabled healthchecks; another kept the normal
healthcheck but ended before it ran. In the third, a normal healthcheck ran
during a pressure hold: 91 observer samples estimated 512,831,488 to
516,624,384 bytes, with a maximum observed sampling gap of 63 ms. Subsequent
rejected RUNs logged decision-time effective values of 512,962,560 and
522,133,504 bytes. No OOM was observed, and owned resources were removed.

These follow-ups show correct refusal at the captured decisions. They do not
reconstruct the original missing decision reading, prove uninterrupted pressure
between samples, or establish why the original RUN was admitted. The persistent
observer itself consumes memory, and `memory.current` and `memory.stat` are
read sequentially. Kernel logs on the Docker host were not independently
verified. Continue investigating the original failure and qualify representative
workloads before relying on admission for production safety.

After implementing the opt-in observer below, three additional serial runs
passed: beta.12 with the observer, beta.11 with the observer, and beta.12 with
the standard method. Each observed two cgroup refusals with rollback, followed
by recovery and a successful offline audit. The default-mode repeat observed a
healthcheck; the two short opt-in runs ended before their first healthcheck.
These repeats do not invalidate the original FAIL or identify its cause.

Finally, [three tests of the later traced candidate](2026-11-05-pressure-trace-015b12.json)
passed using the same image: standard sampling with trace, persistent sampling
with trace, and standard sampling without trace. The two traced runs logged
the server refusing the transaction and independent RUN at effective cgroup
readings above the threshold. The control emitted no trace. Trace timestamps
follow the reading, and the host and VM clocks may differ. These new results
do not reconstruct or explain the original failed decision.

For a follow-up diagnostic, the smoke offers an optional persistent observer:

```sh
python crates/server/tests/cgroup_pressure_smoke.py \
  --image YOUR_LOCAL_IMAGE --output /tmp/brainwire-pressure-receipt.json \
  --persistent-observer
```

Use a new output path. The optional mode uses one owned shell for measurements;
it keeps the same limits and pass/fail rules as the standard smoke. It still
has observer overhead and sampling gaps. An observer failure is inconclusive;
an observed OOM or failed cleanup remains a failure. The standard mode stays
the default. Run only against a disposable local candidate on a Linux Docker
host with cgroup v2 and the Python Bolt driver installed.
Add `--trace-admission` to record up to 64 server-side decisions using the
readings already made by the guard. It is disabled by default and records no
query, identity or filesystem path. Synchronous logging can affect timing and
memory, so use it as a diagnostic rather than a neutral measurement.
