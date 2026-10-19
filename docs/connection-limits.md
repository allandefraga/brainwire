# Request decoding, idle connections and Python pools

Brainwire independently limits encoded Bolt request payloads and PackStream
owned decoded storage. `server.max_message` bounds payload bytes; chunk framing
and NOOPs do not consume that payload budget. `server.max_decoded_message`
bounds owned strings/bytes and container backing storage during decoding before
request dispatch. A compact NULL list can cost substantially more decoded
storage than its wire representation. Invalid, trailing or over-budget decoded
messages are fatal `Neo.ClientError.Request.Invalid` failures and close the
session, including pending transaction cleanup.

The decoded default is 64 MiB per request. It is independent of the encoded
256 MiB default and the query memory pool; it does not define RSS or an aggregate
server memory ceiling. Up to 64 admitted connections can each have their own
request allocation allowance, plus encoded buffers, query data and storage
caches. Choose smaller message/batch limits for a constrained pilot.

```sh
export BRAINWIRE_SERVER_MAX_MESSAGE=8MiB
export BRAINWIRE_SERVER_MAX_DECODED_MESSAGE=64MiB
export BRAINWIRE_SERVER_IO_TIMEOUT_MS=300000
```

## Complete IO budgets

Authenticated connections have an absolute complete-request read budget and a
separate complete-response write budget. The default is 300,000 milliseconds
(300 seconds), configurable from 1 to 3,600,000 milliseconds. NOOPs and partial
bytes cannot renew the read budget. Successfully processed complete requests
and response delivery begin the next request budget. An idle explicit
transaction loses its connection and its pending changes roll back.

Handshake/authentication use their separate ten-second IO budgets. Synchronous
parsing and engine work can postpone enforcement; the IO timer is not a query
execution deadline, cancellation protocol or total connection lifetime limit.
Responses are encoded one frame at a time, but session results can still be
materialized in memory before network writes. The timeout does not prove an
aggregate result-memory or peak RSS bound.

## Python connection pools

A pooled socket can expire while the application is idle. Configure the official
Neo4j Python driver to check connections idle for at least 30 seconds before
reuse. This is below the optional TLS proxy's 60-second inactivity timeout and
the native server's 300-second IO default. For a shorter configured timeout,
choose a liveness threshold below the earliest idle boundary in that path:

```python
from neo4j import GraphDatabase

with GraphDatabase.driver(
    'bolt://127.0.0.1:7687',
    auth=('reader', token),
    liveness_check_timeout=30,
    connection_timeout=5,
    max_transaction_retry_time=0,
) as driver:
    with driver.session(database='default') as session:
        print(session.run('RETURN 1 AS ok').single()['ok'])
```

For a protected TLS edge use `bolt+s` with strict CA/hostname validation as
shown in [protected transport](protected-transport.md). Liveness checks concern
connection reuse; they do not make writes idempotent or guarantee transparent
retry of every failure. Without that setting, the fixture records whether the
first reuse succeeds or fails, then explicitly opens a new connection after an
accepted connection error. It does not claim automatic recovery by default.

## Reproduce the owned qualification

Build a binary containing these connection settings before running. The short
fixture uses a fresh authenticated server with a 64 KiB decoded quota, 8 MiB
encoded quota and 200 ms IO budget. An 8,313-byte RUN containing 8,192 NULLs is
rejected before its CREATE; valid connections remain usable. NOOP and partial
byte drips expire; an idle explicit transaction's node/index changes disappear;
complete requests renew the budget; pool reuse with liveness succeeds. That
short fixture uses `liveness_check_timeout=1` and a 1.2-second pool pause, so the
liveness check runs after its accelerated 200 ms server expiry. The one-second
fixture value is separate from the ordinary 30-second configuration above.

```sh
python3 crates/server/tests/connection_limits.py --self-check
uv run --no-project --with neo4j==5.28.6 python crates/server/tests/connection_limits.py --binary target/release/brainwire
uv run --no-project --with neo4j==6.3.0 python crates/server/tests/connection_limits.py --binary target/release/brainwire
```

The optional default test starts another fresh server **without** overriding
`BRAINWIRE_SERVER_IO_TIMEOUT_MS`. It waits about 301 seconds, observes the raw
authenticated socket closing around the default 300-second boundary, and checks
pool reuse with and without liveness. It does not accelerate or mock the timer.

```sh
uv run --no-project --with neo4j==6.3.0 python crates/server/tests/connection_limits.py --binary target/release/brainwire --default-idle
```

The final candidate's [driver 5 receipt](benchmarks/2026-10-13-beta/connection-limits-driver5.json)
and [driver 6 receipt](benchmarks/2026-10-13-beta/connection-limits-driver6.json)
record unoverridden native expiry at 300.003 and 300.002 seconds respectively.
Both first reuses passed with fixture liveness enabled; both default pools failed
their first reuse and succeeded after an explicit new connection. Those receipts
qualify the real default timer, not the accelerated short cases. Binary SHA-256
was `1042c946cb85d6c6d5b234db85163f3478ab505f20b4fb896e3b8e515d963150`.

Receipts include binary SHA-256, driver/OS, observed expiry times, pool behavior,
final offline graph/index audit and cleanup result. The harness uses random
mode-0600 credentials in a private temporary directory, signals only its own
server and removes all owned sockets/store/logs after completion or failure.
No existing graph or deployment is accessed. See also
[termination recovery](termination-recovery.md).
