# Bolt response encoding diagnostic

Status: synthetic native-loopback diagnostic, not production qualification or
proof that the 5% performance gate is met. Baseline is v0.10.0-beta.1 and the
candidate adds bounded response encoding and conflict-history retention. The
raw receipts contain binary hashes, host details, timing samples, workload and
limits. Both runs verified the returned identities and payload hashes.

Each query returned eight 512 KiB strings. Three rounds with 20 measurements
per binary gave baseline/candidate p50 3.670/3.655 ms and p99 5.458/7.541 ms.
A repeat with five rounds and 100 measurements per binary gave p50
3.934/3.998 ms and p99 11.919/11.041 ms. The individual rounds varied
substantially, host load was nonzero and latency includes Python hashing and
transaction completion. The first sample's p99 is its maximum; these results
do not isolate encoding, demonstrate an RSS improvement or establish a
statistically reliable regression bound. RSS values are sampled, not peaks;
no cgroup/kernel OOM evidence was collected.

Run on an appropriately qualified host with two frozen executables, no builds,
and the Neo4j Python driver installed:

```sh
python scripts/bench_bolt_response.py --baseline /path/to/old/brainwire \
  --candidate /path/to/new/brainwire --output /tmp/brainwire-response.json \
  --rounds 5 --warmup 5 --repeats 100
```

The output must not already exist. Each binary receives a new disposable local
store; no existing graph or container is opened. See
[initial receipt](initial.json) and [repeat receipt](repeat.json).
