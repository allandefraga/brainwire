# v0.16 beta integration diagnostics

The beta.16 candidate passed two synthetic tests using disposable local data.
These are regression checks, not production qualification or a benchmark
against another database.

- An external application writer with a modified adapter declared a keyed type
  online. It created distinct events for the same endpoints, replayed a batch,
  updated and deleted individual events, refused a key change, and recovered
  after process interruption. One interruption occurred after a committed
  prefix but before advancing its checkpoint. The source's timeline compiler
  and real source data were not tested. See the [sanitized receipt](2026-11-05-beta16-integration.json).
- A separate authenticated standalone server with the experimental request
  ledger verified identified node and keyed-edge batches, duplicate-ID refusal,
  bounded input, rollback before commit, restart, and durable status for
  acknowledged writes. An interactive 80×24 PTY also verified the TUI status inspector against this server. See the [receipt](2026-11-05-beta16-identified.json).

A separate ordinary-transaction diagnostic submitted COMMIT without reading its
acknowledgement, killed the process, and reconciled graph and index state after
restart. It did not test the identified-batch API's lost-acknowledgement path.
None of these tests proves exactly-once delivery, power-loss recovery or
multi-machine failover.

A separate [keyed hub diagnostic](2026-11-05-beta16-keyed-hub.json) used
10,000 events on the same endpoint pair with batches of 50, including a
restart and replay. It passed integrity checks in 156 seconds on the shared
host. Batch times increased as the hub grew, consistent with the current
pair scan; this synthetic result is not a throughput guarantee.

A separate [200,000-node bulk diagnostic](2026-11-05-beta16-bulk200k.json)
on the final candidate exercised 20 labels, unkeyed relationship batches,
property changes, deletions and forced restart. A sequential warm point-read
sample includes Python/Bolt round trips. The fixture is synthetic and
compressible, and the shared host makes its timings unsuitable as an SLO.
