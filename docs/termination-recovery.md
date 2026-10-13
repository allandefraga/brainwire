# Native termination and recovery qualification

Brainwire currently has no native SIGTERM drain protocol. The operating system
terminates the process abruptly. A supervisor can start it again against the
same exclusive RocksDB data directory; this is restart recovery, not continued
availability during termination or distributed failover.

The executable fixture checks three boundaries on an isolated, authenticated
loopback server:

1. **Acknowledged writes and an open transaction.** It commits 1,000 Customers
   with scalar email indexes, then opens an explicit transaction containing a
   CREATE and an overwrite of an existing indexed value. Both transaction
   statements finish and their pending results are visible to that transaction.
   A separate reader sees committed data only. SIGTERM occurs before COMMIT.
   After restart, all acknowledged baseline rows and old index values remain;
   pending nodes and new index values are absent.
2. **RUN submitted without acknowledgement.** It sends one batch mutation over
   authenticated Bolt, then signals the process without reading RUN/SUCCESS or
   a commit acknowledgement. The acceptable outcome is either zero or all 200
   new Customers, with node lookups and scalar indexes agreeing. Submission does
   not prove the server entered execution. This case does not locate a kill
   inside COMMIT or establish a deterministic commit outcome.
3. **A reader stops during PULL.** It imports a 4 KiB tag payload on each baseline
   Customer, requests all 1,000 records with a small socket receive buffer,
   observes the first RECORD and stops reading. The receipt records the requested
   and actual OS socket buffer; operating systems can increase it. It signals
   the process and verifies committed graph/index data after restart. Observing the first record
   proves the response started; it does not prove the writer was blocked at the
   exact signal instant or establish a peak RSS bound.

Each SIGTERM exit is required within five seconds. Receipts record exit codes,
exit times, authenticated readiness after restart, binary SHA-256, driver and
host versions, uncertain outcome and final offline graph/index audit. A negative
signal exit code is expected for abrupt termination. These elapsed observations
are fixture results, not recovery-time service-level guarantees.

## Run the owned synthetic fixture

Build the selected release binary first, then run from the repository root:

```sh
python3 crates/server/tests/termination_recovery.py --self-check
uv run --no-project --with neo4j==6.3.0 python crates/server/tests/termination_recovery.py --binary target/release/brainwire
```

The fixture creates a private temporary directory, mode 0600 random credentials,
a new graph and loopback port. It signals only its own child processes and
removes its temporary store, logs and sockets in cleanup, including after a
failed assertion. It does not use an existing graph or deployment. JSON results
are written to stdout; driver diagnostics can appear on stderr when the server
terminates a live connection.

The final candidate [receipt](benchmarks/2026-10-13-beta/termination.json) passed
on native binary SHA-256
`1042c946cb85d6c6d5b234db85163f3478ab505f20b4fb896e3b8e515d963150`.
It observed no committed mutations for the unacknowledged batch, signal exit code
`-15`, a first PULL record of 4,103 bytes and a socket receive buffer increased by
the OS from the requested 1,024 to 322,386 bytes. The offline audit and owned
cleanup passed. This evidence retains the phase and abrupt-termination limits
described above.

## Application and supervisor boundary

A client that loses its connection before an acknowledgement must treat the
operation's outcome as unknown. Check durable application IDs/checkpoints after
reconnecting and use the existing resumable ingestion contract rather than
blindly retrying a non-idempotent CREATE. The fixture accepts both atomic outcomes
and verifies consistency; it does not promise exactly-once delivery.

Run only one store owner. Configure a supervisor or orchestrator to restart the
process with its persistent data mounted, and gate readiness on authenticated
Bolt recovery. [The protected MVP quickstart](mvp-quickstart.md) describes local
startup and audit; [protected transport](protected-transport.md) describes the
optional single-host TLS edge.

Graceful stop of admission, draining/cancelling active queries, explicit commit
phase fault injection, large-volume memory limits, Kubernetes/Swarm termination
and replicated failover remain separate work. This fixture qualifies the native
single-node process boundary with synthetic data.
