# Memory reference notes

Brainwire's beta.14 memory-admission changes were designed independently after
reviewing the following public source snapshots. These are design references,
not imported source code or evidence of a hard RSS limit.

- [Memgraph memory tracker](https://github.com/memgraph/memgraph/blob/bcec383fe10370abfaf86a48f6d34dc1011bab89/src/utils/memory_tracker.hpp)
  uses tracked allocations and parent counters, with refusal limited to scopes
  that can safely handle it. Its Community source is under BSL; check its license
  before reusing code. Metrics distinguish tracked allocations from RSS.
- [FalkorDB query runtime](https://github.com/FalkorDB/FalkorDB/blob/5d98077aec45537b86d92a87f37003008897c160/graph/src/runtime/runtime.rs)
  checks a per-query memory budget, while its [thread pool](https://github.com/FalkorDB/FalkorDB/blob/5d98077aec45537b86d92a87f37003008897c160/graph/src/threadpool.rs)
  bounds queued queries. Its server source is under SSPL; check its license
  before reusing code. A per-query budget is not an RSS limit.

The Brainwire changes in this release reserve known output structures and
high-degree deletion staging before allocation, and check sampled process
headroom before growing its existing Bolt ingress pool. Query and ingress pools
remain independent and do not cover all native allocations. The container limit
can still kill the process. Test memory pressure, rollback, other clients and
recovery on the intended workload; the failure envelope is not yet qualified.
