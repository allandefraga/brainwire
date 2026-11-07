# Mixed write/read and restart diagnostic

This opt-in runner uses synthetic data in a new authenticated standalone graph.
It creates its own labeled container and volume, runs one writer and one reader,
and samples cgroup memory and process RSS. It never opens an existing graph.

Install the Python Neo4j driver in your environment and make the image available
locally. The runner does not pull images. For a short check:

```sh
python scripts/bench_mixed_recovery.py --image YOUR_CANDIDATE_IMAGE \
  --rows 1000 --batch 100 --seconds 10 --deadline 300 \
  --output /tmp/brainwire-mixed-smoke.json
```

For a 30-minute evaluation with 100,000 nodes and relationships, in a 2 GiB
container with two CPUs:

```sh
python scripts/bench_mixed_recovery.py --image YOUR_CANDIDATE_IMAGE \
  --rows 100000 --seconds 1800 --deadline 2400 --memory 2 \
  --output /tmp/brainwire-mixed-receipt.json
```

Use a fresh output path in a directory Docker can mount. Memory options are 2,
3, 4 or 8 GiB; swap is disabled for the container. The default payload is 256
bytes per node, and the maximum workload duration is one hour. The host must
have enough free memory for the chosen cap alongside its other services.

The writer commits bounded property updates to nodes and relationships and
creates/deletes a temporary node within each transaction. A concurrent reader
checks counts and bounded records. Once the threads stop, the runner checks
every expected node and edge revision, kills the process, starts it again,
repeats the exact check and runs the offline integrity audit. A commit with an
uncertain result fails the diagnostic; it is never automatically replayed.

The JSON receipt records memory samples, completed work, refusal counters,
checks and cleanup. Receipts with `runner_schema: 2` also record every workload
setting and progress windows. By default each full 60-second window must contain
at least one confirmed write batch and one successful reader iteration. Adjust
`--progress-window`, `--minimum-writes-per-window` and
`--minimum-reads-per-window` to choose an explicit diagnostic threshold.
Expected memory refusals can occur, but they do not count as successful work.

Window assignment uses the timestamp when an operation completes, captured
under a lock. The final partial window is recorded without a requirement;
if the entire run is shorter than one window, that sole window requires the
configured minimum. An operation started before the requested end may complete
during the bounded shutdown wait; it is recorded separately as
`writes_after_requested_end` or `reads_after_requested_end` in the final window
and can satisfy its minimum. `last_completion_s` shows the observed completion
time. No operation started after the requested duration qualifies. Windows
without enough completions fail this diagnostic threshold; that observation
alone does not prove that the database was unavailable.

A passing receipt still requires exact final data. Older receipts without
progress windows only show aggregate completed work; they do not establish
sustained progress throughout the requested interval.
Failures preserve a failing receipt and remove resources carrying that run's
UUID label. SIGINT and SIGTERM follow the same cleanup path. Killing the runner
itself with SIGKILL prevents cleanup; the receipt's run identifier can be used
to locate its Docker resources by the `brainwire.benchmark.run` label. Tokens
are generated in memory; the temporary policy file contains only their hash.

These are local diagnostics. Sampled RSS and cgroup peaks do not prove a global
RSS ceiling or absence of leaks. The crash happens after acknowledged writes
and quiescent verification; lost acknowledgements, disk loss, power loss,
replication, network partitions and multi-machine failover are not exercised.
Kernel logs are not independently checked. Host load and observer processes
can affect memory and timings.

Window checks require a minimum number of completions in each required window;
they do not bound the latency of individual requests or the longest gap between
successes. The query timeout is cooperative on the server and is not a
preemptive socket deadline. The runner's overall deadline is a separate bound.
