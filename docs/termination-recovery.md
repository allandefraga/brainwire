# Native termination, drain and recovery qualification

Python commands below use the environment from [Python setup](python-package.md).

`brainwire serve` handles SIGTERM and SIGINT with a bounded drain:

1. The listener closes at once. New connections are refused; a connection
   queued in the backlog at that instant can see a reset instead.
2. Every session that is idle at a message boundary closes: before or during
   authentication, `READY`, or `FAILED`. A request read from an idle session
   after the drain starts (for example a `BEGIN` pipelined behind a `COMMIT`)
   is not executed. It receives
   `Neo.TransientError.General.DatabaseUnavailable` and the session closes.
3. Explicit transactions and open result streams continue. Their `RUN`,
   `PULL`/`DISCARD`, `COMMIT` and `ROLLBACK` run normally; the session closes
   as soon as it returns to an idle state.
4. After `server.shutdown_grace_ms` (`BRAINWIRE_SERVER_SHUTDOWN_GRACE_MS`,
   default 8,000 ms, range 0–3,600,000), sessions still open are abandoned.
   Open transactions that never reach commit are dropped. A write that already
   reached commit can have an unknown outcome for the client. The process exits
   with code 1 and reports how many sessions were abandoned. A complete drain
   exits with code 0.
5. After the drain, the process gives the async runtime up to one second to stop
   and waits up to five more seconds for store handles to be released before
   closing RocksDB on the main thread. If a
   running engine call still holds them, it exits immediately with code 1
   without running destructors. Acknowledged commits are already synced to the
   WAL in both cases (graph commits and consensus metadata write with RocksDB
   `sync=true`: `crates/store/src/txn.rs`, `crates/replication/src/storage.rs`).

The handlers are installed once the stores are open. A signal while RocksDB or
consensus state is still opening stops the process immediately, as before; that
path is covered by crash recovery.

Allow the drain time plus the runtime and store shutdown time in your
supervisor stop budget. The shipped Compose,
Swarm and Kubernetes profiles allow 60 seconds, and the image starts the binary
in exec form, so the signal reaches it directly (Compose also adds an init
process). The Compose fixture `crates/server/tests/compose_restart.py` checks it
in a Linux container: a transaction committed during the drain survives restart
and the container exits 0, also when only idle sessions are open. Raise the grace only together with the orchestrator's own stop
timeout. A further SIGTERM does not shorten the
drain; SIGKILL remains the forced stop and is covered by crash recovery.

When the server closes a session while the client still has unread bytes in
flight (for example a new request pipelined behind a refused one), the operating
system can answer with a reset, and the client may see a connection error
instead of the refusal. Treat that as an unknown outcome, as for any lost
connection; the refused request itself was not executed.

What the drain does **not** do: it does not interrupt an engine call that is
already running (the cooperative `server.query_timeout_ms` still applies), it
does not hand leadership over in a quorum cluster, and it is not a
continued-availability or failover mechanism. A client that loses its
connection before an acknowledgement must still treat the outcome as unknown.

## Executable qualification

The fixture runs an authenticated loopback server on owned synthetic data, with
a 3,000 ms grace, and checks five stops:

1. **Commit during drain.** An explicit transaction is open with a pending
   CREATE. SIGTERM is sent; the fixture waits until new connections are
   refused, then runs an indexed overwrite in the same transaction and commits.
   Exit code 0 before the grace. After restart, the CREATE and the overwritten
   index value are present and the old index value is gone.
2. **Abandoned transaction.** An explicit transaction with a pending CREATE and
   indexed overwrite is left open. Exit code 1 after the grace; after restart,
   all 1,000 acknowledged baseline rows and old index values remain and the
   pending changes are absent.
3. **Unacknowledged RUN.** A raw-Bolt autocommit batch of 200 CREATEs is sent
   without reading any reply, then SIGTERM. Accepted outcomes are zero or all 200
   nodes, with node lookups and scalar indexes agreeing, and exit code 0 or 1.
4. **Stalled reader.** A reader requests all 1,000 records of 4 KiB, sees the
   first RECORD and stops reading. The stream holds the drain until the grace;
   exit code 1. Restart preserves the committed graph.
5. **Idle pool only.** With only an idle authenticated driver connection, the
   process exits 0 within milliseconds.

It then runs the offline integrity audit and removes its processes, store,
logs and credentials.

```sh
cargo build --release --locked --offline
python3 crates/server/tests/termination_recovery.py --self-check
.venv-brainwire/bin/python crates/server/tests/termination_recovery.py --binary target/release/brainwire
```

The quorum fixture (`crates/server/tests/quorum_replication.py`) now ends with
SIGTERM to each voter in turn, requires exit code 0, then compares offline
audits and logical digests across voters.

The [October 15 graceful-drain report](benchmarks/2026-10-15-graceful-drain.md)
and the [October 13 abrupt-termination receipt](../benchmarks/archive/2026-10-13-beta/termination.json)
record earlier binaries. The [uncertain-commit report](benchmarks/2026-11-14-uncertain-commit.md)
records later process kills with unacknowledged writes. Re-run the fixture against
the exact candidate you plan to operate.

## Application and supervisor boundary

Keep writes idempotent: check durable application IDs or checkpoints after a
lost connection and use the resumable ingestion contract, rather than blindly
retrying a non-idempotent CREATE. The drain reduces unknown outcomes during
planned stops; it does not provide exactly-once delivery.

Run only one store owner. Configure the supervisor to restart the process with
its persistent data mounted, and gate readiness on authenticated Bolt recovery.
The listener closes as the drain starts, so readiness probes fail during it.

Still separate work: interrupting running engine calls, an exact COMMIT-phase
fault hook, large-volume memory limits, orchestrator termination on a real
Kubernetes/Swarm target, and replicated failover.
