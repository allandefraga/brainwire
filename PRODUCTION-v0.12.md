# v0.12 candidate: qualification status

**Not released.** The v0.12 acceptance gates are still open. The local branch
starts at `v0.11.0-beta.1`; the working tree includes externally supplied
changes that require review. No production readiness claim is implied.

| Area | Current evidence | Remaining gate |
| --- | --- | --- |
| Memory pressure | The existing workspace tests, Clippy and formatting passed. A synthetic local recovery exercise passed. A purge regression failed before its fix and passed after it. | Repeat sustained 2/4/8 GiB runs, qualify process-wide accounting and external cgroup pressure, and measure peak and idle RSS under controlled conditions. Sampled admission is not a hard RSS ceiling. |
| Bolt | Candidate pages entity properties and streams some scalar results. | Verify wire-level backpressure, PULL -1, slow clients, errors, ACL and transaction cleanup. Entity handles and several operators still materialize eagerly. |
| Commit outcome | Existing native recovery exercise reconciled test data after a process kill. | Implement and test an atomic standalone request receipt/status; the existing exercise is not a status API or power-loss test. |
| RocksDB | The write buffer manager already shares the block cache. | Qualify compaction and external memory pressure with the requested 4 GiB workload. |
| Query execution | Existing guards reject oversized operations; eager operators remain. | Finish lazy parallel entity reads; prove ORDER BY, aggregation and DISTINCT spill or refuse with the required contract. |
| Performance and Cypher | Earlier and external measurements are diagnostic, not reproduced release results. | Run the requested differential suite and performance targets; remaining compatibility gaps are open. |
| Multi-architecture image | The current published release is amd64 only. | Verify CI, the release workflow and amd64/arm64 manifests and digests before claiming publication. |

External workload reports cited in candidate design notes were not independently
repeated in this session. They do not qualify the release or prove behavior on a
real ingestion source. The release workflow link and image digests will be added
only after a successful publication; no v0.12 tag or image is claimed here.
