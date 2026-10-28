# Bolt transaction timeouts

A positive Bolt `tx_timeout` is a deadline in milliseconds for the entire
transaction, including time between statements and before results are consumed.
The effective duration is the smaller of the requested duration and
`server.query_timeout_ms`. Time starts when the engine accepts BEGIN or the
implicit RUN, before database lookup or replicated admission. Later RUNs do not
restart it. Values must be nonnegative integers; duplicate keys and invalid
values fail with `Neo.ClientError.Request.Invalid` before a transaction starts.

Omitting `tx_timeout`, or sending zero, preserves the existing server policy:
`server.query_timeout_ms` applies separately to each RUN. It does not introduce
an idle transaction timeout for those clients. An explicit transaction sets its
timeout in BEGIN; RUN inside it cannot override that setting.

With the Neo4j Python driver, send the protocol metadata explicitly:

```python
from neo4j import Query

with driver.session(database="demo") as session:
    rows = session.run(Query("MATCH (n:Customer) RETURN n.entity_id LIMIT 10",
                             timeout=0.5)).data()

    with session.begin_transaction(timeout=0.5) as tx:
        rows = tx.run("MATCH (n:Customer) RETURN count(*)").data()
```

`session.run(cypher, timeout=0.5)` passes a Cypher parameter named `timeout`;
it does not set the driver's transaction timeout. Decorator-based managed
transactions also send the timeout with BEGIN.

An expired transaction discards staged writes and its storage snapshot, releases
replicated writer admission, and returns
`Neo.ClientError.Statement.ExecutionTimedOut`. Buffered rows are discarded before
PULL reports the error. Conservative query-memory accounting remains held while
those rows are still owned by the session; it is released after abort, RESET,
LOGOFF or connection cleanup. RESET permits another transaction on the connection.
This does not solve the broader materialized-output or global RSS limits.

Deadlines are cooperative. Parsing, a blocking storage operation, replicated
admission, quorum waits and durable commit cannot currently be interrupted at an
exact wall-clock instant. No +/-100 ms guarantee is established for those paths.
The deadline is checked again after execution/conversion and immediately before
replication submission. A successful durable commit remains successful even if
the deadline passes during the durability call. Once a replicated proposal has
been submitted, an unknown commit outcome retains its existing outcome-unknown
classification; a client must reconcile it before retrying the write.
