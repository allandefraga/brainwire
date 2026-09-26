# Brainwire journal

One 45-minute session per day, started when Allan asks. This file is the memory
between sessions: each session starts with no context other than this file and
the README.

Internal documents are in English (Allan, 2026-09-24). The conversation follows
Allan's current language.

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
2. Brainwire now lives in its standalone repository at
   `/Users/fraga/Documents/brainwire`, on `codex/brainwire`. Nothing is pushed
   unless Allan asks. The old platform branch and its worktrees are historical
   recovery points. Older entries below retain their original paths and commit
   IDs; standalone history has rewritten IDs after subtree extraction.

## Phases

1. Brainstorming (requirements, approaches, design section by section).
2. Spec in `docs/superpowers/specs/`, reviewed by Allan.
3. Implementation plan broken into tasks (skill `superpowers:writing-plans`).
4. Implementation: tasks from the plan, with TDD, several per session through
   parallel agents.

No line of code before the end of phase 2.

## Entries

### 2026-09-26, visual identity selected; continue tomorrow

- **Decision:** Allan explicitly chose the latest green v2 identity: electric
  green `#57FF48`, near-black `#070B09`, restrained cyan, and the interwoven
  B/W graph logo. The [Observatory design](docs/design/2026-09-26-brainwire-observatory.md#2-selected-visual-identity)
  identifies the canonical logo, memory and backoffice references. Earlier
  color variants are historical explorations. Production assets should refine
  this choice, not reopen the visual direction.
- **Scope:** visual identity approval does not imply the proposed backend
  contracts or TUI features are implemented. README and design status updated.
- **Next session:** Allan asked to stop and continue tomorrow. Resume the
  executor (MVP Task 4), then real Bolt transactions and serve (Task 5), with
  the selected identity preserved for the TUI/backoffice work. No automation
  or scheduled run requested.
- **Verification:** documentation whitespace checked; local changes only.

### 2026-09-26, UI planning after the engine session

- **Requested:** plan the generated graph-centric terminal concept. Allan
  chose both graph exploration and Cypher in the first prototype, then added
  memory usage/budget visibility, explanations for memory variables, a logo,
  and a backoffice to inspect and understand database files. After trying
  magenta/cyan, he preferred stronger green with restrained supporting colors.
- **Delivered:** [Observatory design proposal](docs/design/2026-09-26-brainwire-observatory.md),
  local concept assets and image-generation prompts. The plan includes Graph,
  Query, Memory and Disk & Files, contextual F1 help, file-purpose inspection,
  a shared config explanation catalog, client/server boundaries, staged
  acceptance and a green visual direction. Images are synthetic concepts;
  no UI code or engine changes in this planning session.
- **Findings:** configured memory budgets are not allocated RAM. Memtables
  are charged within the shared cache budget. Cache accounting, process RSS
  and container usage are overlapping scopes and must not be summed. Query
  memory enforcement is not implemented; global versus per-query admission
  needs an explicit contract. A connected graph explorer also needs bounded
  neighborhood/edge results beyond the current MVP node/scalar output plan.
- **Next:** continue the existing engine MVP with Task 4 (executor), then
  Task 5 (real Bolt transaction lifecycle and serve). Use this design to brief
  the integrated TUI prototype and telemetry/storage inspection work. Before
  shipping live backoffice features, implement the listed backend contracts.
- **Verification:** local design links, PNG signatures and selected-image
  copy hashes checked; Git whitespace checks pass. No code tests rerun for
  this documentation and concept-art-only change.
- **Local only:** design and documentation changes are kept in this standalone
  repository; no remote, publication or push.

### 2026-09-26, 17:48 (Codex, one-hour local session)

- **Scope:** Allan requested one hour of work and explicitly prohibited pushes.
  During the session he requested moving Brainwire into an independent sibling
  repository at `/Users/fraga/Documents/brainwire`. No remote PR or push.
- **Done:** re-ran the merged workspace's 177 tests; the scoped review
  of `711a3e3` found all three graph fixes addressed and no new blocking issue.
  Merged the existing local Cypher branch (`d51482ffd`) and closed its second
  depth-limit fix round (`dd054f759`): expression depth now follows the actual
  subtree, including mixed operators and wrappers. All 41 parser tests pass;
  the scoped review is clean. MVP Task 3 is complete (`a6a741050`): snapshot
  range scans and label scans, including maximum ID bounds and transaction
  overlay behavior; 70 store tests, clippy and fmt pass, review clean. Added an
  independent regression for non-null SET after property promotion. Storage
  Task 10 is complete (`a0165496d`): five SIGKILL/reopen cycles verify every
  acknowledged Customer, Company and WORKS_AT edge; focused test and review
  pass. This proves process-crash recovery, not hardware power-loss durability.
  Corrected the README's obsolete brainstorming status.
- **Relocation complete:** extracted 54 Brainwire-only commits, moved all 50
  working source/documentation files and the build cache, preserved task
  ledgers, and removed the main platform checkout's untracked shortcut.
  SHA-256 verification matched every moved source/documentation file. The old
  feature branch/worktree remains a clean historical recovery checkpoint.
  The standalone repo has no remote
  and its Git objects have no dependency on the platform object store.
  Rewritten equivalents: parser fix `73cb05c`, label scans `2c52501`, crash
  test `1315687` (the original IDs remain above for historical context).
- **Verification after relocation:** all 225 workspace tests pass (44 Bolt,
  46 core, 41 parser, 23 config, 70 store, 1 crash integration test), as do
  workspace clippy with warnings denied and formatting. `brainwire config
  show` works from the new root. No server was left running.
- **Next step:** MVP Task 4 (executor), then Task 5 (`brainwire serve` and the
  driver demo). The executor agent had only read its brief when relocation was
  requested; no executor code or tests were written. Resume in the standalone
  repository. The session stops at 18:48 CEST.
- **Integration finding:** the current Bolt `QueryEngine` exposes only `run`;
  BEGIN, COMMIT and ROLLBACK still operate only on session state. Task 5 must
  add real engine transaction ownership/hooks before accepting durable writes
  inside explicit transactions. Wiring per-query commits into that stub would
  incorrectly acknowledge rollback after persisting the write.
  Updated the active MVP plan with standalone paths and branch, completed
  task status, and required transaction lifecycle acceptance checks.
- **Decisions:** the user's one-hour request replaces the usual 45-minute
  limit for this session. Parser work continues in the main Brainwire worktree
  after a local merge; the old parser checkout is preserved. Scratch reports
  from `/tmp/brainwire-20260926` are preserved under the standalone repository's
  ignored `.superpowers/sdd/session-2026-09-26/`; this journal records the
  durable handoff. The crash test verifies every acknowledged write, not only
  the most recent one. Scan APIs still materialize vectors per the MVP
  contract; streaming and enforcement of execution memory limits remain ahead.

### 2026-09-26, 11:03 (MVP push, interrupted at about 12:15)

- **Done:** the MVP plan (config file and CLI, a Cypher subset, the executor
  on the store, `brainwire serve`) written and committed
  ([plan](docs/superpowers/plans/2026-09-26-brainwire-mvp.md)). Merged into
  `feat/brainwire`: the Bolt server (final review plus one fix wave,
  re-reviewed clean), and the config file with `config init` and `config show`
  (unknown keys are refused; `BRAINWIRE_<SECTION>_<KEY>` overrides; memory
  defaults 10 GiB cache with 1 GiB of memtables inside it and 2 GiB for
  queries). `Cargo.lock` is committed. Storage Tasks 7 (transactions, bounded
  conflict history, read-for-update proven by tests, poison recovery) and 8
  (id blocks, poison recovery that never reuses an id) are complete and
  reviewed. Task 9 (graph operations) is implemented and reviewed. The Cypher
  parser (Task 2 of the MVP plan) is implemented and reviewed once. The
  workspace had 164 tests green at the last merge.
- **Found by the reviews:** a dangling edge when an edge commits before a
  concurrent DETACH DELETE of its node (fix: prefix dependencies in the
  committer); SET NULL not clearing a property promoted to a column; an
  invariant check blind to dangling edges; long AND/OR chains that crash the
  parser (bounded, but the bound had a bypass that the second fix closes);
  a typo in a config key passing silently.
- **Interrupted:** the API session limit stopped two implementers mid-work.
  At 16:07 Allan asked for a little more, and the Task 9 fix was finished and
  committed (`711a3e3`: DETACH DELETE conflicts with a concurrent edge in both
  orders, promotion clears the old value, the invariant check sees dangling
  edges; 177 workspace tests green); its scoped re-review is still to do.
  `.worktrees/brainwire-cypher` is clean; its fix round 2 had not started.
- **Next step:** the scoped re-review of the Task 9 fix, MVP Task 3 in
  `.worktrees/brainwire`, and MVP Task 2 fix round 2 in
  `.worktrees/brainwire-cypher` (resume the implementers from the ledgers);
  review, merge the parser, then MVP Task 4
  (executor) and Task 5 (`brainwire serve` and the driver demo: create, restart,
  read from disk).
- **Blockers:** none.

### 2026-09-26, 08:13

- **Done:** plan 1 Task 5 (catalog from the ontology) implemented on the
  temporary branch `feat/brainwire-catalog` (worktree
  `.worktrees/brainwire-catalog`), with two review fixes: id arithmetic is
  checked (`IdSpaceExhausted` instead of a silent wrap that would give two
  columns the same id), and entries that share a label merge into one type, as
  the platform's `GraphMappingConfig` allows; spec 2.1 now says so. A demo over
  the synthetic fixture showed ids staying stable when the ontology changes.
  Task 6 (RocksDB with the eleven column families and a shared cache and
  memtable budget) done and reviewed on `feat/brainwire`. Task 7 (snapshot
  transactions, first committer wins) implemented, 20 store tests. The final
  review of plan 2 (most capable model) asked for fixes before the merge.
- **Found by the final review of plan 2:** a client could reach READY without
  HELLO or LOGON through RESET (the plan's own state table was wrong against
  research/bolt.md 3.1, 3.5 and 6.6), and one `accept()` error, such as running
  out of file descriptors, ended the process. Both are in a fix wave. Parked as
  blockers for plan 3 and for any client outside this machine: the "message
  too large" error never reaches the driver, which retries for up to 30 s, and
  decoding multiplies memory by up to about 33 before authentication.
- **Decided under Allan's go-ahead (to review):** Task 5 ran in parallel on its
  own branch; shared labels merge instead of failing (union of properties,
  first `display_color`, `DuplicateLabel` removed); the plan 2 fix wave covers
  the two Important findings the reviewer asked for before the merge, and the
  rest waits for plan 3. The ledgers hold each ruling with its cost.
- **In flight at the end of the session:** the plan 2 fix wave, the re-review
  of Task 5's last fix, and the review of Task 7 (most capable model, because
  it is the concurrency code).
- **After the session:** the plan 2 fix wave came back done (driver check
  ok), and the Task 7 review asked for three fixes: the conflict history grows
  without limit while any transaction is open, no test proves the
  read-for-update dependency, and a poisoned lock panics. The rulings for fix
  round 1 are in the plan 1 ledger.
- **Next step:** finish those loops from the ledgers (Task 7 fix round 1, the
  scoped re-review of the plan 2 fix wave); re-review the plan
  2 fix wave, merge `feat/brainwire-bolt` and `feat/brainwire-catalog` into
  `feat/brainwire`, and commit `Cargo.lock`; then Task 8 (id blocks), Task 9
  (graph operations) and Task 10 (the kill -9 crash test), which closes Gate 1
  (due 2026-09-30).
- **Driver 5.x:** production pins the Python driver to `neo4j>=5.0,<6`
  (`client-core/pyproject.toml`). With Allan's OK, neo4j 5.28.6 went into a
  temporary venv in the session scratchpad, and the driver check and the demo
  pass with it too (Bolt 5.4, 0.083 ms per round trip). Rerun it after the fix
  wave.
- **Blockers:** none.

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
