# Beta.13 pressure diagnostic

These short synthetic 512 MiB cgroup-v2 tests used local candidate image
`sha256:ee467c466e3cdb62982e8381d3a10fe7c195c8481970ce04268641f698656855`
on a shared Docker Desktop host. They do not qualify a production workload or
establish a process-wide RSS ceiling.

One standard-observer run returned **FAIL** under the earlier harness rule: the
external samples before and after an accepted RUN reported 514,531,328 and
515,039,232 bytes, above the 510,027,366-byte threshold. The trace included two
nearby server decisions at 508,170,240 bytes, below the threshold. Host and VM
clocks are not proven aligned and the trace has no request ID. Transient
`docker exec` observers can raise the cgroup's usage; this is consistent with
but does not prove the cause of the discrepancy. The original receipt remains
FAIL; the pressure at the RUN decision is **inconclusive**, not a passing test.
It cannot explain the separate [beta.12 incident](2026-11-05-pressure-investigation.md).

A repeated standard run with trace passed; server traces captured refusals at
512,241,664 and 512,331,776 bytes. A persistent-observer run with trace passed
with refusals at 511,868,928 and 512,675,840 bytes. Each verified the transaction
rollback, recovery, offline audit, no observed OOM and owned-resource cleanup.
A persistent-observer control without trace also passed. Sampling and logging
can affect timing and do not establish continuous pressure.

The updated harness reports an admitted RUN with only high external bracketing
samples as **INCONCLUSIVE**: those samples cannot prove what the server observed
at its decision. It still reports OOM, data/integrity errors and cleanup failures
as failures. Inconclusive is not a pass, and historic receipts are unchanged.
