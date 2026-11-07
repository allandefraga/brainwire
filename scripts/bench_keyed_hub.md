# Keyed relationship hub diagnostic

This runner measures `MERGE` for many distinct keyed relationships between
the same two nodes. It uses a locally available image, one authenticated
standalone container, two CPUs and a configurable memory limit. It creates
its own UUID-labeled volume and removes only resources carrying that label.
It refuses an existing report path and never opens an existing graph.

With Python and the `neo4j` driver installed, start with a small smoke:

```sh
python scripts/bench_keyed_hub.py --image YOUR_LOCAL_IMAGE \
  --rows 20 --batch 5 --payload-bytes 256 \
  --output /tmp/brainwire-keyed-hub-smoke.json
```

Then repeat on each image you want to compare, using new output paths:

```sh
python scripts/bench_keyed_hub.py --image YOUR_LOCAL_IMAGE \
  --rows 2000 --batch 100 --payload-bytes 4096 \
  --output /tmp/brainwire-keyed-hub.json
```

Run serially on the same host without concurrent builds. The phases insert
half the events, replay that half, append the rest, and replay all events.
Each batch records its latency. Each phase validates every key, ordinal and
payload and records a deterministic content hash. The graph is killed with
SIGKILL, restarted, checked again, stopped and audited offline.

Use `--indexed` to explicitly enable the standalone relationship-index disk
format and build an `EVENT.event_key` property index before the measured
phases. The runner waits for `scalar_range_ready` and records setup time
separately. A shared 60-second setup deadline starts before the DDL; each DDL
or status query receives its remaining time as a Bolt transaction timeout.
The outer benchmark alarm also covers transport stalls. Omit the flag for the baseline; record it in both receipts when
comparing indexed behavior across versions. The temporary graph is created
only for this test, so this option never upgrades an existing volume.

Add `--extra-labels 100` to declare 100 unused node types without adding
records or relationship types. This varies catalog size while retaining the
same event data. Compare runs with identical image, indexing mode and workload
settings to investigate catalog overhead. The receipt records declared type
counts and input ontology JSON bytes; that size is not a measurement of the
persisted catalog or its decode allocation. Empty types are a synthetic probe,
not a representative broad ontology.

The report includes external process-RSS snapshots and cgroup usage, peak
and OOM counters. These phase samples are not continuous RSS measurements;
the cgroup peak includes file cache and Docker observers. Validation scans
also affect memory and cache state. Lowering `--query-mib` can cause either
the writer or the verification scan to be refused; the failure receipt names
the active phase and retains completed batches.

The fixture has synthetic fixed-width payloads and one sequential connection.
Its timings are not production SLOs, a process RSS ceiling, sustained memory
qualification, or distributed failure-recovery evidence. A failed run still
attempts owned-resource cleanup and preserves its report.
