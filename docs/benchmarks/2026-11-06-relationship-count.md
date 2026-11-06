# Relationship type count: synthetic wide graph

A local Bolt diagnostic created 100 labels with 20,000 nodes each and 110
relationship types with 20,000 relationships each: 2 million nodes and
2.2 million relationships. It compared the published v0.16.0-beta.1 image
with a locally built beta.2 candidate. Both ran with 8 GiB, two CPUs, a warm
sequential client and nine measured samples after two warmups on a shared host.
They used separate generated volumes; this is not a production benchmark.

| Query | Baseline median | Candidate median |
| --- | ---: | ---: |
| `MATCH ()-[r:R000_A]->() RETURN count(r)` | 4,108 ms | 10.31 ms |
| `MATCH ()-[r:R050_A]->() RETURN count(r)` | 4,429 ms | 9.98 ms |
| `MATCH ()-[r:R009_B]->() RETURN count(r)` | 6,161 ms | 9.49 ms |

Every count was 20,000. The candidate's `R050_A` query also passed a 1-second
query timeout and the three counts survived restart. The candidate visits
adjacency ranges belonging to labels in the relationship type's signatures;
it does **not** provide a count store or constant-time counting. Runtime grows
with eligible adjacency volume. These fixtures do not qualify ACL performance,
concurrent workloads, latency SLOs or another database's performance.

[Baseline receipt](2026-11-06-wide-baseline.json) ·
[beta.2 candidate receipt](2026-11-06-wide-beta2-candidate.json) ·
[earlier scan-only receipt](2026-11-06-wide-scan-candidate.json) ·
[first candidate attempt](2026-11-06-wide-restart-initial.json)

The first candidate attempt recorded correct counts but failed when the test
reused the old dynamically assigned host port after restart. The runner was
corrected to resolve the port again; the original failure is retained. Both
containers and their volumes were removed by the harness.
