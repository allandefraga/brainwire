# Relationship count benchmark

Run the same synthetic graph and query pairs against two local Brainwire
images. Requires Docker with Linux containers and cgroup v2, Python on macOS
or Linux, and the Neo4j Python driver. Pull or build each image first. The
script pins the locally resolved image ID for both database setup and serving.

Run a smoke test with a locally available image:

```sh
python3 scripts/bench_relationship_types.py --image brainwire:local \
  --labels 10 --rows 30 --samples 3 --memory 2 \
  --deadline-probe --restart --output /tmp/relationship-smoke.json
```

Use `--output` to name a new JSON file in a writable directory that Docker can
bind mount. The benchmark creates a UUID-labeled container and volume, binds
Bolt to a random localhost port, and generates an ephemeral authentication
token. Cleanup targets that run's label. The report records the label suffix
in `run` for inspection if Docker cleanup fails.

For a full comparison, run each image sequentially on the same host with
`--labels 100 --rows 20000 --batch 500 --samples 9 --memory 8 --query-timeout 0`
and distinct output files. This creates 2 million nodes and 2.2 million
relationships across 110 types, with a 2-CPU container cap. The benchmark
verifies total counts, then measures 9 warm Bolt round trips per query shape
on each of 3 types. It records the raw timings, median, nearest-rank p95,
cgroup memory and OOM counters. A p95 of 9 samples is the largest sample.
`--restart` checks the three selected counts after reopening the volume.
`--deadline-probe` records the result of a separate 1-second query attempt
outside the timed samples.

Keep host activity comparable between runs. This workload measures synthetic
count queries with one client. It does not measure full pipeline, ACL, or
memory admission behavior.
