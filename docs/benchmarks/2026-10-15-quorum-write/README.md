# Local quorum write diagnostic — 2026-10-15

This short synthetic run compares sequential single-row Cypher transactions
with 25-row `UNWIND` transactions against three independent native Brainwire
voters on one host. The harness reuses the qualification test’s framed peer
proxies, fresh provisioning, private authentication and retained-volume restart
pattern. No partition is injected during measurement.

```sh
cargo build --release --locked --offline -p brainwire-server
uv run --no-project --with neo4j==6.3.0 python \
  crates/server/tests/quorum_write_benchmark.py \
  --binary target/release/brainwire --rows 400 --batch-size 25 --repeats 5 \
  > docs/benchmarks/2026-10-15-quorum-write/rows-400-batch-25.json
```

The [canonical raw receipt](rows-400-batch-25.json) binds the candidate binary
SHA-256, platform, driver, exact queries, every transaction timing, convergence
status and all offline audits. Total elapsed time was 4.081 seconds including
startup, provisioning, measurements, restart, validation, audits and cleanup.

| Round | Mode | Transactions | Transaction p50 | Transaction p95 | Rows/s |
| --- | --- | ---: | ---: | ---: | ---: |
| 0 | Single row | 400 | 1.130 ms | 1.370 ms | 850 |
| 0 | Batch of 25 | 16 | 1.570 ms | 1.810 ms | 15,924 |
| 1 | Batch of 25 | 16 | 1.552 ms | 1.752 ms | 15,888 |
| 1 | Single row | 400 | 1.066 ms | 1.427 ms | 885 |
| 2 | Single row | 400 | 1.200 ms | 2.263 ms | 686 |
| 2 | Batch of 25 | 16 | 1.662 ms | 2.602 ms | 14,581 |
| 3 | Batch of 25 | 16 | 1.709 ms | 3.028 ms | 13,768 |
| 3 | Single row | 400 | 1.103 ms | 1.360 ms | 873 |
| 4 | Single row | 400 | 1.066 ms | 1.385 ms | 877 |
| 4 | Batch of 25 | 16 | 1.603 ms | 1.742 ms | 15,485 |

A second complete run produced similar throughput: 826 single rows/s and
14,784 batched rows/s; see the [second raw receipt](rows-400-batch-25-repeat2.json).
Across the two canonical runs, aggregate single-row p50 was 1.104–1.120 ms and
p95 1.614–1.672 ms; batch-of-25 p50 was 1.621–1.647 ms and p95 1.885–2.231 ms.
The first run achieved 827 rows/s for singles and 15,081 rows/s for batches.
Throughput divides total rows by
the sum of measured transaction times; it excludes checks between/after rounds.

With the same 400-row rounds and five repeats, larger batches increased
observed throughput further: batch 100 measured 31,819 rows/s (20 ACK samples,
p50 3.041 ms, p95 3.992 ms); batch 400 measured 41,875 rows/s (5 ACK samples,
p50 9.038 ms, p95 11.315 ms). The [batch-100 receipt](rows-400-batch-100.json)
and [batch-400 receipt](rows-400-batch-400.json) both include the same
all-voter restart and digest checks. These small samples suggest the expected
latency/throughput trade-off; they are not enough to select a production batch
size or establish a stable percentile.

A further run with 1,000 rows per transaction reached 35,409 rows/s but had a
25.028 ms p50 and 39.834 ms p95 (3 ACK samples); 6,000 rows survived restart
and all-voter digest checks. See the [batch-1000 receipt](rows-1000-batch-1000.json).
Compared with batch 400, this larger batch raised single-transaction latency
without improving measured throughput, but the sample is too small to set a
general batch limit.

A longer run then measured 6,000 single-row writes and 60 acknowledged
100-row batches across three alternating rounds. Batches reached 34,106 rows/s
with 2.999 ms p50 and 3.113 ms p95 over 60 samples. Singles reached 937 rows/s
with 0.993 ms p50 and 1.394 ms p95 over 6,000 samples. All 12,000 rows survived
same-volume SIGKILL/restart; three offline audits had identical logical digests.
See the [2,000-row / 100-row-batch receipt](rows-2000-batch-100.json). This is
still an eight-second localhost diagnostic, not an endurance or multi-machine
capacity result.

A repeat-length run used ten alternating rounds of 1,000 single writes and ten
100-row batches each. It measured 10,000 transactions per mode: 841 single
rows/s (p50/p95 1.050/1.708 ms) and 32,090 batched rows/s (p50/p95
3.073/3.748 ms over 100 ACKs). All 20,000 rows survived same-volume restart
and all three offline logical digests matched. The full diagnostic took 14.5 s;
see the [10-round receipt](rows-1000-batch-100-repeats-10.json). A 34 ms
single-write maximum shows occasional host/queueing outliers even though p95
was 1.7 ms. In this comparison, batching delivered about 38× aggregate row
throughput, at the cost of a 3.75 ms p95 acknowledgment for each 100-row
transaction. This remains a short one-host run. A higher run using `--rows 2000 --batch-size
100 --repeats 10` **did not complete**: a single-row `CREATE` received
`CommitOutcomeUnknown` after the server-side 10-second `raft.client_write`
wait. The precise repeat/offset, cluster status and durable request lookup were
not captured; the harness deleted its scratch logs/state during cleanup. The
[failed diagnostic receipt](rows-2000-batch-100-repeats-10-failure.json) records
the known request ID and limits. This does not prove data loss or a rejected
write. The preceding acknowledged workload of 10,000 single writes plus 100
100-row batches passed; the larger run's outcome remains unresolved. The next
step is to preserve per-voter status/log evidence and query that request ID on
timeout before retrying larger loads. Do not lengthen the timeout or change WAL
sync to hide this latency.

Each mode writes 400 nodes per round. A persistent authenticated Bolt session
sends one request at a time and consumes the result before the next request.
The timer includes the client, Bolt, Cypher, quorum and durable acknowledgment
path; it excludes fixture preparation and post-run checking. Only a read warms
the connection before the first round. The two modes alternate order within the
same growing graph; each uses unique IDs and equally sized title strings.

After all 2,080 write transactions, all voters agree on committed/applied
indexes. All three processes are killed, then restarted with the same data and
configuration without reprovisioning. The restarted leader returns exactly all
4,000 acknowledged IDs and titles. All voters catch up again; after shutdown,
their offline integrity audits pass and their logical graph SHA-256 digests
agree. The owned child processes, proxies and temporary directories are removed.

This provides a reproducible reason to prefer batching for this bounded write
shape. It does not establish sustained capacity, production latency, physical
power-loss durability, multi-machine networking, concurrent clients or behavior
under failure during load. With 16 batch transactions per round, each round’s
nearest-rank p95 is its largest sample. The aggregate 80-sample p95 is still a
short diagnostic observation, not an SLO. Candidate durability settings are
retained; no WAL or synchronous-write setting is disabled. This script makes no
production-code change and does not isolate fsync cost from replication,
parsing, indexes or the driver.

Host variability matters: a preceding 400-row/five-round run observed only
173–310 single rows/s and 1,934–12,983 batched rows/s. No environment isolation
or host-idle guarantee was applied. An [initial smaller smoke receipt](rows-100-batch-25.json)
is also retained; its four batches per round make its p95 the largest sample.

Verification: the script’s `--self-check`, Python bytecode compilation, rejection
of `python -O` (assertions are required), and complete bounded loopback run pass.
The self-check also verifies that failed process waits retain ownership and
cleanup attempts later resources after an earlier failure. The existing
acceptance test remains unchanged.

## Failure evidence capture and bounded reproduction

The harness now captures evidence **before** scratch state is removed when any
timed write raises. It records the repeat, mode, row offset, rows in the failed
transaction, the failing call's elapsed time and the error type/code/message;
the request ID is parsed from the message when present. It samples `status`
from all three voters (a silent voter is recorded as an error) before and after
a quorum-authoritative `cluster request --id <id>` lookup, retried for up to
30 s while no leader answers. The lookup state is `Applied`, `absent` (only
when the CLI succeeded and returned `Lookup: null`), `other` or `unavailable`;
an exhausted wait is never reported as absent. Each voter's last 64 KiB of
`server.log` is included with tokens redacted. `--failure-receipt PATH` writes
this JSON (default: stderr). The failed write is not retried; timeouts, WAL/sync
settings and verification are unchanged. `--self-check` covers lookup retry,
log tail bounds/redaction and the failure record with fakes, and a scratch
fault-injection run (driver error forced at one request) exercised the real
path against live voters and found `Lookup: null` for an unsent request.

Release binary SHA-256 `8662a232...` (a rebuild that only moves the SIGTERM
handler), Python driver 6.3.0, macOS arm64, host load average 16–20 throughout
(`uptime` 1-minute value before each run: 16.5 sanity, 19.5, 17.3, 16.4).
[Sanity run](rows-400-batch-25-failure-capture-sanity.json): `--rows 400
--batch-size 25 --repeats 3` PASS in 3.5 s, 590 single rows/s (p50/p95
1.387/2.519 ms, 1,200 samples), 10,119 batch rows/s (1.998/6.182 ms, 48
samples); 2,400 rows survived restart.

Three `--rows 2000 --batch-size 100 --repeats 10` attempts did **not**
reproduce `CommitOutcomeUnknown`. All three failed earlier, in a single-row
`CREATE`, with `DatabaseUnavailable: leader/quorum unavailable` after 23–28 ms
(wall 21, 6 and 5 s): [attempt 1](rows-2000-batch-100-repeats-10-attempt1-failure.json)
at repeat 4 single offset 1615 (8 rounds had completed),
[attempt 2](rows-2000-batch-100-repeats-10-attempt2-failure.json) at repeat 0
offset 1229 and [attempt 3](rows-2000-batch-100-repeats-10-attempt3-failure.json)
at repeat 0 offset 530. This message comes from the pre-write quorum read
barrier (`ensure_linearizable` in `begin`), before a command is proposed, and
carries no request ID, so no lookup was possible (`unavailable`: no ID). At
failure time every voter answered, all were in term 1 with voter 1 as leader,
`running` true and no issue; in attempts 2 and 3 followers trailed the leader
by one log entry. Server logs contained only the startup line. Cause not
established; these are loopback observations on a loaded host, not a regression
claim against earlier passing runs (those used a previous binary and a different
host load). Not tested: idle host, other binaries, multiple clients, the
original 10-second `raft.client_write` timeout path, multi-machine behavior.

## Root cause: one TCP connection per peer RPC (fixed)

A fourth attempt on the same binary `8662a232...` reproduced the original
failure: [`CommitOutcomeUnknown`](rows-2000-batch-100-repeats-10-reproduced-failure.json)
at repeat 6, single mode, offset 55, after 3.2 s. The quorum lookup returned
`absent`, so the uncertain write had not committed and a same-ID retry was safe.
Voter status showed term 4 with the leader moved from voter 1 to voter 2.
Host-wide TIME_WAIT sockets, sampled every second, rose from 11 to **58,046** in
about 26 s ([samples](time-wait-before-after.json)): ephemeral ports ran out,
peer `connect()` failed, openraft's heartbeat round for `ensure_linearizable`
got no quorum (`QuorumNotEnough` → `leader/quorum unavailable`), heartbeats
failed and leadership changed. Two sources opened a connection per frame:
Brainwire's peer client (`network.rs` connected for every AppendEntries/Vote)
and this harness's Python fault proxy (one upstream connection per frame).
Their separate shares were not measured.

Fix: the peer client keeps up to 8 idle authenticated connections per peer
(reused only within 10 s) and the peer server answers successive frames per
connection (first frame within 5 s, later frames within 30 s, sender pinned per
connection, mTLS check per frame). If a reused connection fails, the client
drops that peer's idle connections and retries once on a new one; only
AppendEntries and Vote use the pool, and Raft tolerates their repetition. The
same retry keeps one-request peers (an older binary, a proxy) working. The
proxy now forwards successive frames over one upstream connection.

Final binary `9a09b8c1...` (adds a 60 s maximum pooled-connection age, a pool
purge on client timeout and a body deadline for idle-connection frames, after
review), same host and command: **PASS**, 20,000 single writes at 1,517 rows/s
(p50 0.607 ms, p95 0.941 ms, max 7.5 ms) and 200 batches at 35,770 rows/s (p50
2.651 ms); all 40,000 rows verified after SIGKILL restart
([receipt](rows-2000-batch-100-repeats-10-connection-reuse-final.json)). Host
TIME_WAIT was 9 before the run and peaked at 63 during it
([samples](time-wait-before-after.json)). The intermediate binary `6f9c102f...`
passed the same run twice (1,523 / 1,569 single rows/s;
[run 1](rows-2000-batch-100-repeats-10-connection-reuse.json),
[run 2](rows-2000-batch-100-repeats-10-connection-reuse-repeat.json)). The full
`quorum_replication.py` acceptance (partitions, restarts, digests, ACL reader)
passed at 200 rows and three times at 500 rows across both binaries
([final receipt](quorum-acceptance-rows-500-connection-reuse.json)).
Earlier throughput figures in this file used per-frame connections
in both the proxy and the peer client, so they are not a like-for-like baseline.
Loopback, one client, three processes on one host; not multi-machine or
endurance evidence.

## Six-minute soak on the fixed binary

`--rows 2000 --batch-size 100 --repeats 200` on `9a09b8c1...` (the repeat bound
is now 300, under the 1 GiB retained-log cap; final verification checks an exact
count and then every row in blocks of 2,000 IDs, because one unbounded read of a
large run is refused by the 64 MiB query guard): **PASS** in 352 s. 400,000
single writes at 1,299 rows/s (p50 0.589 ms, p95 1.015 ms, **max 1.82 s**, one
outlier not diagnosed) and 4,000 batches at 31,780 rows/s (p50 2.702 ms, max
48 ms); all 800,000 rows verified after SIGKILL restart; matching offline audits.
Term stayed 1 with no leader change; 404,002 log entries, 450 MB retained. Host
TIME_WAIT never exceeded 87 and voter RSS stayed between 35 and 135 MB in 5 s
samples ([receipt](soak-rows-2000-batch-100-repeats-200.json), per-transaction
samples removed). One client, loopback, one host; not an endurance or
multi-machine qualification, and retained-log growth still requires snapshots
before longer runs.

A repeat of the same soak on the same binary **failed** after 105 s
([receipt](soak-repeat-election-failure.json)): at repeat 71 a single write got
`leader/quorum unavailable` because leadership had moved from voter 1 to voter 3
(term 2) and this harness keeps writing to the first leader. A follower must
have missed heartbeats for at least the 500 ms election timeout, which matches
the 0.6–1.8 s write pauses in the passing soak (all in repeats 150–197). RocksDB
is not the cause on the evidence collected: the voters' graph and consensus LOG
files reported zero write-stall delays or stops, and flushes took about 6 ms.
The host was busy (load average 6.5–19 during the session; `mediaanalysisd`,
the Docker VM with 33 containers and `BTLEServer` near or above one core each),
but host contention is not established as the cause. The binary has no tracing,
so the paused component (leader core, follower apply, fsync or scheduler) is
unknown. Both soak results stand: sustained writes no longer exhaust ports, and
a remaining sub-2 s pause can still cost a leader election on this host.

A follow-up probe on the same host (one Python writer, 1 KiB append + `fsync`
in a loop for 60 s, no Brainwire running) measured p50 0.015 ms and p99.9
0.06 ms, but 79 calls above 100 ms and a maximum of 530 ms. Storage sync pauses
of this size can exceed the 500 ms election timeout, so they are a plausible
cause of the pauses and the election; the voters themselves were not
instrumented, so this remains a hypothesis.

