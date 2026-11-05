# Admission overhead: exploratory comparison

Two short synthetic 2 GiB container comparisons used the same four concurrent
read types, 4,000 nodes, 4,000 relationships and 45 seconds of reads per run.
The versions were run in order beta.12, candidate beta.13, candidate beta.13,
beta.12. The first candidate had higher p95 latencies than the first baseline;
the second candidate had lower p95 latencies than the second baseline. For
example scan p95 values in that order were 256.881, 313.669, 122.081 and
356.468 ms. All four runs passed their counts and audit checks.

These measurements are **inconclusive** for regression: compilation, other
Docker workloads and an integration test overlapped the runs on a shared host.
They neither establish comparable throughput nor prove a performance change.
A separate headroom-only microbenchmark observed extra mutex and clock overhead,
but does not represent end-to-end query performance. Repeat on an isolated host
with randomized alternating runs and the application's own workload before
setting latency or throughput expectations.
