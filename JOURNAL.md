# Brainwire journal

One 45-minute session per day, started when Allan asks. This file is the memory
between sessions: each session starts with no context other than this file and
the README.

Internal documents are in English (Allan, 2026-09-24). The conversation with
Allan stays in Portuguese.

## How a session starts

1. Read the [README.md](README.md) and the newest entry below.
2. Run `date` and note the start time. At 40 minutes, start nothing new; at
   45, wrap up.
3. Do the **Next step** of the newest entry. If it depends on a decision from
   Allan, ask first, one question at a time, and meanwhile move forward on
   whatever does not depend on it (research, corpus, drafts of later sections).

## How a session ends

1. A new entry **at the top** of the list, with: done, decided, next step,
   blockers.
2. Commits go to `feat/brainwire`, or to a sub-branch of it that merges back
   (plan 2 uses `feat/brainwire-bolt`), never to another feature's branch, and
   nothing is pushed unless Allan asks. The code lives in the worktree
   `.worktrees/brainwire`; `client-brainwire` in the main checkout is a
   symlink to it, so no `git clean` there.

## Phases

1. Brainstorming (requirements, approaches, design section by section).
2. Spec in `docs/superpowers/specs/`, reviewed by Allan.
3. Implementation plan broken into tasks (skill `superpowers:writing-plans`).
4. Implementation: tasks from the plan, with TDD, several per session through
   parallel agents.

No line of code before the end of phase 2.

## Entries

### 2026-09-25, 14:45 (second session: the code starts)

- **Done:** Rust installed with Allan's permission (rustup, stable 1.98.1,
  minimal profile with clippy and rustfmt, about 470 MB; the shell PATH was not
  changed, so run `source "$HOME/.cargo/env"` first). Worktree
  `.worktrees/brainwire` on `feat/brainwire`; the untracked copy of this folder
  in the main checkout went to the session scratchpad and the old path is now a
  symlink to the worktree. Plan 2 runs in a second worktree,
  `.worktrees/brainwire-bolt`, on `feat/brainwire-bolt`. Plan 1: Tasks 0 to 4
  done and reviewed (workspace, keys, ids, values and the row codec; 32 tests).
  Plan 2: all six tasks done and reviewed (chunk framing, PackStream,
  negotiation, messages, session, TCP server; 31 tests). The official Neo4j
  Python driver 6.3.0 connects to `brainwire-bolt-dev`, negotiates Bolt 5.4 and
  runs `RETURN 1 AS x` in auto-commit and in a managed transaction; 2,000 round
  trips in one session took 0.087 ms each (release build, stub engine).
- **Measured:** the first full build takes 1m57s (RocksDB compiled from
  source) and the incremental `cargo test` 0.70s, far below the 2-minute
  reversal rule of the plan: Rust stays.
- **Found by the task reviews and fixed:** two crash paths on hostile input
  (PackStream and the row codec decoded nested values without a depth limit,
  so a stream of nested lists aborted the process; both now stop at 64
  levels); `NodeId::type_range` panicked on an invalid type and now returns a
  `Result`; decoders and error variants without tests; the server's message
  cap moved to the spec's 256 MiB (the first version used 1 MiB, which would
  refuse the platform's 200 MB batches); a malformed message now closes the
  connection.
- **Decided under Allan's go-ahead (to review):** plan 2 on its own branch,
  merged back into `feat/brainwire` later; plan 2 Tasks 1 to 4 in one dispatch
  with one review; the driver check uses the neo4j 6.3.0 already in
  `client-core/.venv`, so nothing was installed; nesting limit of 64 levels
  on the wire and on disk; `type_range` returns a `Result` (plan 3 uses it
  that way); `Cargo.lock` is committed right after the merge; the message cap
  is configurable through `serve_with_limit`. Every ruling, with what it costs
  if wrong, is in the ledgers
  (`.worktrees/brainwire/.superpowers/sdd/<plan>/progress.md`, git-ignored).
- **Next step:** final whole-branch review of plan 2 on the most capable
  model, then merge `feat/brainwire-bolt` into `feat/brainwire` and commit
  `Cargo.lock`; then plan 1 from Task 5 (schema and catalog) to Task 10 (crash
  test), with `superpowers:subagent-driven-development`.
- **Open for plan 3 (from the reviews):** `ChunkReader` copies each message
  twice, while spec 3.3 asks for decoding straight from the chunks;
  auto-commit PULL does not return a bookmark yet; the server has no logging,
  read timeout or connection cap; LOGOFF is not handled.
- **Blockers:** none.

### 2026-09-25, 11:48 (session)

- **Done:** the spec moved to English and was completed
  ([spec](docs/superpowers/specs/2026-09-24-brainwire-design.md)); Allan
  approved sections 1 to 4 one by one; implementation plan 1, the storage
  foundation, written in 11 tasks with tests first
  ([plan 1](docs/superpowers/plans/2026-09-25-brainwire-storage-foundation.md));
  implementation plan 2, the minimal Bolt server, written in 6 tasks
  ([plan 2](docs/superpowers/plans/2026-09-25-brainwire-bolt-minimal.md)); the
  docs committed to the local branch `feat/brainwire` (no push), written without
  switching branches, so the PII branch was not touched. All three research notes
  are now in English (translated in parts after two earlier rounds of
  translation agents stalled; headings and line counts checked against the
  originals).
- **Decided by Allan:** an event inherits the visibility of its owning entity
  unless it comes with its own ACL; "see comments" in the CLI means people's
  annotations on nodes and types.
- **Decided under Allan's go-ahead to proceed without asking (to review):**
  section 5 of the spec (cluster, operations, tests) as written; each RBAC user
  has one role and a list of databases, set with `ALTER USER ... SET
  DATABASES`; a `reader` can add notes; the `pk` encoding uses an escaped
  terminator instead of a length prefix; ids come in blocks whose end is
  committed before use; one implementation plan per gate (plan 1 storage,
  plan 2 Bolt, plan 3 Cypher and ACL).
- **Next step:** Allan reviews the go-ahead decisions above and plans 1 and 2.
  Then execute plan 1 and plan 2 with `superpowers:subagent-driven-development`,
  starting with Task 0 of plan 1.
- **Blockers:** the Rust toolchain is not installed on this machine, and
  installing it (rustup from the official source) is a download that needs
  Allan's permission; consent for the worktree of `feat/brainwire`; the Python
  `neo4j` package for plan 2's driver check, if it is missing.

### 2026-09-24, 22:00 (wrap-up)

- **Done:** rhythm corrected; scope and date confirmed; internal documents moved
  to English, with `PLANO.md` renamed to [PLAN.md](PLAN.md) and `DIARIO.md` to
  this file.
- **Decided:** 45 minutes of work per day, not per hour, with the session
  starting when Allan calls it (the hourly loop from 21:00 was a
  misunderstanding and is cancelled). Scope: everything stays, and so does the
  2026-10-31 launch date, with a simple RBAC (three fixed roles and access per
  database; fine-grained visibility stays with the ACL). Versioning and the
  release flow are parked for later.
- **Next step:** with Allan's approval of spec section 1, present section 2
  (already drafted). Without it, draft section 3 (query and protocols) from
  `research/bolt.md` and `research/cypher-corpus.md`.
- **Blockers:** approval of section 1; the decision on whether an event
  inherits the visibility of its owning entity (spec section 2.2).

### 2026-09-24, 21:00 (first scheduled hour)

- **Done:** section 1 of the spec written and presented
  ([spec](docs/superpowers/specs/2026-09-24-brainwire-design.md)); the
  platform's Cypher inventoried
  ([research/cypher-corpus.md](research/cypher-corpus.md)), with the `pk`
  changed to start with `entity_id`; draft of section 2 (data model and
  security) in the spec, not yet presented; two platform problems found along
  the way became separate tasks (scope of the ACL predicate's variables; five
  inconsistencies in the Cypher path). Bolt research done
  ([research/bolt.md](research/bolt.md)): target Bolt 5.1 to 5.4 without the
  manifest, and the HELLO `server` value must start with `Neo4j/` or driver 5.x
  refuses the connection. RocksDB research done
  ([research/rocksdb.md](research/rocksdb.md)) and already applied to section
  1: a 10 GB cache with the memtables inside it, a filter chosen per column
  family (bloom on everything would cost 12.5 GB), an in-house overlay instead
  of WriteBatchWithIndex, which the crate does not expose, and the RAM peak of
  the import with a ZSTD dictionary.
- **Decided:** nothing new from Allan in this hour.
- **Next step:** superseded by the 22:00 entry.
- **Blockers:** approval of section 1.

### 2026-09-24, session 0

- **Done:** requirements round of the brainstorming; name; folder; proposal of
  the plan (scope, approach, ontology as schema, ACL, RBAC, cluster, Python
  SDK, algorithms and AI, CLI, calendar with gates).
- **Decided:** see the requirements table in the README. The name is
  Brainwire, with "brain" in it at Allan's request (Engrama and Brainfold were
  dropped). Approach approved: Rust + RocksDB. Launch on 2026-10-31, with
  cluster/HA, RBAC and full openCypher in scope. The ACL follows the client
  model (`AclPredicate`), native to the engine. No commit until the spec.
- **Next step:** the spec in five sections, each approved before the next:
  (1) storage; (2) data model and security: ontology, ACL, RBAC, events;
  (3) query, protocols and SDK; (4) algorithms, AI and CLI; (5) cluster,
  operations and tests.
- **Blockers:** the open items listed in the plan.
