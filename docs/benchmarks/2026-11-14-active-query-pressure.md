# Active transaction under cgroup pressure

A local A/B diagnostic exercised one explicit transaction in two images built before and after the query-growth admission change. Both used the same synthetic dataset, Docker daemon, 512 MiB cgroup-v2 limit, and a private tmpfs filled to induce pressure. The process-RSS admission threshold was disabled for this diagnostic to isolate the cgroup path.

| Local build | COMMIT under sampled pressure | Recovery |
| --- | --- | --- |
| Before the query-growth guard | Committed | Data and index checks passed |
| With the guard | Refused a 127,343-byte query reservation and rolled back | Full transaction retry passed after pressure was relieved |

The [baseline receipt](2026-11-14-active-commit-baseline.json) and [candidate receipt](2026-11-14-active-commit-candidate.json) identify the immutable image IDs, runner and helper hashes, limits, cgroup samples, audit and cleanup. The [first setup attempt](2026-11-14-active-commit-setup-failure.json) failed because the test supplied an invalid zero-size value; it did not run the pressure scenario. The runner was corrected before the reported A/B run.

These are local candidate images, not verified registry images. The test checks a short synthetic episode, one transaction and one container. External samples bracket COMMIT; they do not reveal the exact reading used by the sampler. This is not a global RSS guarantee, a sustained capacity test, or evidence of distributed failover.

Run the diagnostic only on a dedicated local Docker daemon with the Python Bolt driver installed and two prebuilt images. It fills a temporary filesystem inside a disposable container. Each invocation requires a new output path and cleans only its own labelled resources:

```sh
python crates/server/tests/cgroup_active_commit.py --docker-host unix:///path/to/local/docker.sock \
  --image YOUR_BASELINE_IMAGE --expect commit --output baseline.json
python crates/server/tests/cgroup_active_commit.py --docker-host unix:///path/to/local/docker.sock \
  --image YOUR_CANDIDATE_IMAGE --expect rollback --output candidate.json
```
