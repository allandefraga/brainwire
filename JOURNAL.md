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
2. **No commit until the spec is approved** (decision of 2026-09-24). The
   folder is untracked in the main checkout, so no `git clean` there. After
   the spec, commits go to a dedicated branch, never to another feature's
   branch.

## Phases

1. Brainstorming (requirements, approaches, design section by section).
2. Spec in `docs/superpowers/specs/`, reviewed by Allan.
3. Implementation plan broken into tasks (skill `superpowers:writing-plans`).
4. Implementation: tasks from the plan, with TDD, several per session through
   parallel agents.

No line of code before the end of phase 2.

## Entries

### 2026-09-25, 11:48 (session)

- **Done:** the spec moved to English and was completed
  ([spec](docs/superpowers/specs/2026-09-24-brainwire-design.md)); Allan
  approved sections 1 to 4 one by one; implementation plan 1, the storage
  foundation, written in 11 tasks with tests first
  ([plan 1](docs/superpowers/plans/2026-09-25-brainwire-storage-foundation.md));
  the docs committed to the local branch `feat/brainwire` (commit `d1c50cfab`,
  no push), written without switching branches, so the PII branch was not
  touched. The three research notes are still in Portuguese: two rounds of
  translation agents stalled without writing anything, and the files are
  intact.
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
- **Next step:** Allan reviews the go-ahead decisions above and plan 1. Then
  execute plan 1 with `superpowers:subagent-driven-development`, starting with
  its Task 0 (a worktree of `feat/brainwire`, which needs Allan's consent).
  Translate the research notes in small chunks.
- **Blockers:** consent for the worktree.

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
