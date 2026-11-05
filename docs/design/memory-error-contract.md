# Memory refusal and commit outcome contract

Status: implemented in the working tree; standalone request lookup is available
only through the experimental opt-in mode described below.

## One memory refusal

Every memory refusal uses one code, `Neo.TransientError.General.MemoryPressure`,
whatever refused: the per-statement allocation guard, the shared query pool, the
process ceiling, the cgroup admission or the shared Bolt input pool. It
replaces `MemoryLimitExceeded`, `QueryMemoryPoolExhausted` and
`IngressMemoryPoolExhausted`. Guarantee: **nothing of the transaction was
applied** (an explicit transaction is rolled back with the failed statement),
so the caller may retry it. The message ends with a machine-readable hint:

```
... [brainwire.memory reason=<reason> requested_bytes=<n> available_bytes=<m> rolled_back=true]
```

| reason | what refused | what to do |
|---|---|---|
| `statement_limit` | one statement needs more than `memory.query` | split the statement; `available_bytes` is the per-statement limit |
| `query_pool` | concurrent queries hold the shared pool | retry later or smaller |
| `process_limit` | process headroom is insufficient or its sample expired during execution | back off; investigate server memory and monitoring |
| `process_monitor` | a new RUN cannot get a recent process RSS sample | retry later; investigate process memory monitoring |
| `cgroup` | container working set at 95% | back off; the whole server is busy |
| `ingress` | shared Bolt input pool | send a smaller message or retry later |

Neo4j drivers retry transient errors inside managed transactions; that is safe
here because of the rollback guarantee. A client that wants to size its next
batch reads `requested_bytes` and `available_bytes`.

## Blocking query operators

`ORDER BY` without a bounded result, `DISTINCT`, grouped aggregation and
`collect()` retain intermediate state. They do not spill to disk. If that state
exceeds `memory.query`, the query fails with `MemoryPressure` and
`reason=statement_limit`; the whole explicit transaction is rolled back.
A bounded top-k or a single `count(*)` can succeed on the same input.

The synthetic Bolt regression in `crates/server/tests/blocking_memory.rs`
uses a 64 KiB query budget. It checks read-only refusals, rollback of a pending
property overwrite, ignored COMMIT after failure, recovery with RESET, shared
reservation release and a final `db check`. This verifies the query allocation
contract for those plans; it does not qualify process RSS or large production
workloads.

## Session after a refusal

An input refusal on a logged-on connection no longer closes it. The reader
skips the rest of the refused message without copying it, the session fails
like any statement (its transaction is rolled back) and answers the pipelined
PULL with IGNORED. One small recovery message (<= 1 KiB, e.g. RESET) is
admitted even if the pool is still full. Unauthenticated connections still close.

## Commit outcome

A failed COMMIT now says which case it is:

- refused before the write (conflict, deadline, validation): the existing code,
  with `[brainwire.commit outcome=rolled_back]`;
- the storage write itself failed: `Neo.ClientError.Transaction.CommitOutcomeUnknown`
  with `[brainwire.commit outcome=unknown]` instead of a generic
  `Neo.DatabaseError`. Do not retry blindly; reconcile by key.

An experimental standalone lookup is available on new dedicated volumes with
`server.request_ledger = true`, authenticated identity and explicit identified
writes. See [standalone request outcomes](../standalone-requests.md) for its
fresh-autocommit-only syntax and remaining format/restore gates. The default
mode has no per-request ledger: reconcile an unknown outcome by reading the
owned application keys. `not_recorded` in the experiment is not proof of rollback
and never authorizes a blind retry. Replicated serving retains its separate
request ledger and contract.
