# Quorum operations and Python qualification — October 14, 2026

This local candidate builds on `404133a`. It adds retained-log capacity diagnostics,
three-member inspection, local Raft liveness/readiness, explicit Python transaction
identities and atomic node/ACL upserts. No target deployment or publication occurred.

## Verified scope

The final workspace run passed **623 Rust tests** and **31 Python tests**. Clippy
with warnings denied, formatting and diff checks passed. Astra reviewed the changes
and found no source-level blockers. The capacity boundary test proves atomic
rejection, exact replay at capacity, lower-cap startup refusal and increased-cap
reopen without deleting retained data. This is not a real cluster exhaustion test.

The native fault harness checks three independent process/store pairs and two
named graphs: debug with 200 synthetic nodes, release with 500. Each run has 15
checks, including the prior leader-loss/partition/majority-loss/restart cases,
capacity arithmetic, complete and incomplete inspection, follower liveness versus
leader readiness, no-quorum readiness failure, explicit Python lost-ACK resolution,
atomic node/ACL success and exact permission-denial rollback. Final logical digests
match across all eleven graph column families on all members. Owned directories,
proxies and processes are removed after each run.

The optimized binary is `target/quorum-20261014-release/brainwire`, SHA-256
`04a18d258f7f4811dd573a9dbc8ff84c6be14e03f62c6200f9cf7336cc5ad7e1`.
The debug binary is `target/quorum-20261014-final/brainwire`, SHA-256
`9ec94f02db0dcff7ecb5af68ef62f21263c5d335ff31e6f023fa06b711e8429b`.
The [evidence directory](2026-10-14-quorum-capacity/) binds binaries, Cargo/Rust/
Python sources, check logs and actual native receipts. Its verifier also rejects
six deliberately false readiness/cleanup/capacity receipts in memory.

```sh
python3 docs/benchmarks/2026-10-14-quorum-capacity/verify.py
uv run --with neo4j==6.3.0 python crates/server/tests/quorum_replication.py \
  --binary target/quorum-20261014-release/brainwire --rows 500
```

## Operational meaning

`cluster inspect` samples all three voters and reports missing members and cap
mismatch with nonzero exit. It does not prove quorum. `cluster probe --kind
liveness` accepts running followers; `--kind readiness` requires the local leader
and an actual quorum read barrier without forwarding. A probe is a momentary
Raft sample, not a graph/authentication check or a promise about the next write.
See the [operations guide](../quorum-replication.md).

Python `write_transaction` retains explicit request identity and does not retry
uncertain writes automatically. `upsert_node_with_acl` commits one node patch and
private ACL together; it requires trusted graph-write plus administration grants.
See the [Python guide](../python-client.md). Actual client mapping and DuckLake
ACL integration remain pending; the source importer does not stamp ACL automatically.

No snapshot transfer, log compaction, erased-voter replacement, dynamic membership,
sharding or multi-machine deployment was added. These native fault runs are not
throughput/SLO benchmarks or physical-machine-loss evidence. The optimized build
alone is not a production-readiness claim. Earlier October 13 qualification stays
bound to its original checkout and binaries; its source verifier should refuse a
changed checkout rather than silently credit old evidence to this candidate.
