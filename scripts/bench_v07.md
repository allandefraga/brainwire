# Reproduce the embedded v0.7 diagnostics

`bench_v07.py` records the existing `baseline` executable's independent
**load → read → write → consistency-check** phases. The default load has four
labels with one million nodes each and four relationship types with one million
edges each: **4M nodes and 4M edges**. The write phase adds separate `Pessoa`
nodes and tests the selected session counts.

Build the benchmark before measuring, then use the executable path printed by
Cargo, rather than invoking a Cargo wrapper during the run:

```sh
cargo bench --locked --offline -p brainwire-cypher --bench baseline --no-run
python3 scripts/bench_v07.py \
  --baseline /absolute/path/to/target/release/deps/baseline-HASH \
  --source-revision EXACT_GIT_COMMIT \
  --output /private/tmp/brainwire-v07-before-001
```

The output directory must be new. Its `data/bench` graph is disposable synthetic
data. The script retains that data, JSONL measurements, stderr logs, and
`receipt.json`; it never loads an existing graph or connects to a server. A
nonzero command exit, invalid/empty output, a watchdog stop, or a consistency
check without `status: ok` makes the run fail. Reuse the same executable and
settings for a repeated diagnostic, and use a new output directory each time.

The receipt records the executable SHA-256 and size, caller-supplied source
revision, host load before/after, command arguments, elapsed times and sampled
RSS for each benchmark process. The benchmark has no version command; the hash
identifies the tested artifact. The caller must verify the executable was built
from the supplied revision. Credentials and environment variables are not
written to the receipt.

On macOS, execution requires AC power from `pmset -g batt` and active
`lowpowermode 0` from `pmset -g`. Inactive battery/AC profiles do not qualify or
disqualify a run; their raw `pmset -g custom` output is retained as evidence. An
unverifiable host is rejected unless `--allow-unqualified-host` is supplied; such
a run remains explicitly unqualified. Stop unrelated builds/tests before running
measurements. The script records load averages but cannot establish an idle host.

The default RSS watchdog stops only a process group created by this script when
a sampled RSS value exceeds 4 GiB. It is **not an enforced memory ceiling**:
sampling can miss short peaks and overshoot the threshold. `--rss-watchdog-bytes
0` disables that stop. `rss_peak_bytes: null` means RSS was unavailable, not zero.
RSS includes the direct process and its threads; subprocess RSS is excluded.

## What this does not qualify

This harness is an embedded diagnostic foundation. It does not measure the full
requested simultaneous load/read workload, Bolt transfer, delegated/restricted
ACL reads, a 200-row ACL cursor, or an enforced 4 GiB server memory budget. The
existing read phase uses a 1,000-row cursor and may report extrapolated time.
Its cache/memtable/query accounting settings are fixed in `baseline.rs`.
No performance target is declared passed by this harness. Keep these receipts
separate from a future full server acceptance run.

For a fast harness smoke test, use small positive values (for example
`--rows-per-label 10 --write-rows 10 --sessions 1 --load-sessions 1`). Small data
can legitimately return empty lookup results because several benchmark queries
refer to the million-row fixture's fixed IDs. Unit tests use a fake executable
and do not claim engine performance:

```sh
python3 -m unittest scripts.test_bench_v07 -v
```
