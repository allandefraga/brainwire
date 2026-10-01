# Python ingestion and recovery — 2026-10-01

Local macOS development build, Neo4j Python driver 5.28.6 over loopback Bolt,
synthetic Customer records with email, Boolean, two tags and three numeric
embedding values. Two declared scalar indexes: email and active. Temporary
independent graphs; shared cache 64 MiB, write-buffer target 4 MiB, configured
query budget 32 MiB (not enforced). Each batch acknowledges a synchronous WAL
commit. Timings cover parameter generation, driver exchange and writes, excluding
server startup, final checks and restart. These are individual observations, not
a controlled benchmark distribution or production capacity claims.

| Nodes supplied | Batch rows | First ingestion | Idempotent replay |
| ---: | ---: | ---: | ---: |
| 5,000 | 250 | 0.241 s | 0.279 s |
| 50,000 | 250 | 5.880 s | 5.215 s |
| 100,000 | 500 | 12.064 s | 13.943 s |
| 100,000, extended crash case | 500 | 10.838 s | 14.079 s |

A later 5,000-row run alongside the workspace test suite measured 0.897 s / 1.035 s,
illustrating sensitivity to other local work; do not treat one row as an SLA.

All cases verified replay without duplicate nodes, indexed overwrite, NULL
removal, delete, explicit rollback, graph isolation, vector/list round trips,
SIGKILL/restart, and the offline node/primary-key/edge/index audit. One node was
deliberately deleted: the 100K case recovered exactly 99,999 nodes. Duplicate
CREATE within one UNWIND batch left zero partial nodes or index entries.

The extended case also killed the server while a changed indexed email existed
only in an uncommitted transaction. After restart the original email remained
queryable and the uncommitted value was absent. There were 21 SST files before
the first kill and 12 before the second: this workload crossed the memtable/flush
path; it was not only an empty-store handshake. SST count changes reflect engine
background work and are not graph counts. Expected driver connection errors
appear during cleanup after the deliberate kills.

The standalone JSONL importer was executed twice against another isolated graph
with batches of one row, and recovered exactly two nodes after restart.

Reproduce using the built local binary and a Python environment with the driver:

```sh
python crates/server/tests/python_ingestion.py --rows 100000 --batch-size 500
```

Remaining gates: real document/vector sizes, long concurrent ingestion, cold-cache
read distributions, sustained compaction/write amplification, disk-full and
hardware/storage faults, bounded execution memory, resumable index migration,
and replicated failover. SIGKILL recovery of one local RocksDB is not distributed
high availability or protection from loss of the underlying volume.
