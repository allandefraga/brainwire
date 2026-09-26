# Brainwire integrated terminal demo

User-authorized implementation slice, 2026-09-26, approximately 30 minutes.
Design: [Observatory](../../design/2026-09-26-brainwire-observatory.md).
Use subagent-driven development with one implementer, task review and final
review. All work is local in `/Users/fraga/Documents/brainwire`; no push.

## Global constraints

- Deliver a real interactive Rust terminal program in the chosen electric
  green identity, not another generated screenshot. Preserve existing config
  commands. Engine MVP Task 4/5 remain separate work; do not claim they are done.
- Demo uses one bounded synthetic graph and clearly identified illustrative
  memory/file data. Never inspect or mutate a real database in this slice.
- Choose Ratatui/Crossterm versions compatible with the existing toolchain;
  update the lockfile normally. No remote service or account is required.
- English source/comments; core user-facing UI text catalog in pt-BR (default)
  and English, selectable by a command option. No secrets or real customer data.
- Use the existing workspace and branch. Do not modify store, Cypher or Bolt.

## Task 1: Integrated shell demo

**Files:** create `crates/tui/` (`brainwire-tui`), its app, view, demo and text
modules as needed; add workspace membership/dependencies; integrate a `Shell`
subcommand in `crates/server/src/main.rs` and its package manifest. Tests belong
to the new crate and server CLI integration where appropriate. README/journal
updates are owned by the controller, not the implementer.

**Command contract:**

- `brainwire shell --demo` launches the interactive TUI.
- A bare `brainwire shell` clearly explains that the live backend is not yet
  connected and asks the caller to use `--demo`; do not silently enter demo.
- `--lang pt-BR|en` chooses text, default pt-BR.
- `--no-anim` and `NO_COLOR` are honored. Static deterministic layout is fine.
- `--snapshot --view graph|query|memory|disk --width 160 --height 48` renders
  the same widgets once through a TestBackend to plain text without requiring
  a TTY. Validate sizes (80..240 columns, 24..80 rows) rather than allocate
  arbitrary buffers. Require `--demo` here too.
- Interactive mode refuses non-terminal input/output with an actionable error
  before enabling raw mode. Restore terminal state on normal/error exit.

**UI contract:**

1. Green `#57FF48`, background `#070B09`, quiet cyan details, readable pale
   text, thin borders. Header BRAINWIRE / graph observatory, persistent
   DEMO/SYNTHETIC indication and selected view. Footer always shows key help.
2. Four working views: Graph, Query, Memory, Disk & Files. Left navigation;
   main area plus inspector or contextual explanation; fit at 120x36 and
   degrade to one useful main panel at 80x24 without panic or illegible overlap.
3. Graph: visually interesting bounded graph (roughly 30-60 nodes in clustered
   deterministic positions), label colors, real edges from the same fixture,
   selectable node, highlighted incident edges and inspector properties.
   Selection and any example query results must refer to the same fixture.
4. Query: editable text, an example read scenario, F5 to run that clearly
   labeled guided scenario and show corresponding fixture results. Full Cypher
   execution is not available: edits outside supported scenarios return an
   explicit unsupported-demo message, never fabricated successful execution.
   Support at least a Customer list and Customer-WORKS_AT-Company read; show
   example selection help. Do not build a second general query executor.
5. Memory: show simulated container limit, usage and remaining headroom; cache,
   memtables and query budgets; usage bars; explanations of each selected
   variable. The memtable budget is included in the cache pool; no double
   counting. Indicate configured query budget is not enforced by this demo.
   Values, scope and explanations must be internally consistent.
6. Disk: selectable illustrative CURRENT, MANIFEST, numbered .log WAL, .sst,
   OPTIONS, LOG and LOCK rows with sizes and explanation of each type. Selection
   updates the inspector. No arbitrary filesystem access and no delete action.
7. F1 toggles contextual help; Tab cycles useful focus; 1/2/3/4 selects views
   outside editor text mode; arrows move selection; Enter inspects; F5 runs
   a guided query; Esc leaves editing/help before quitting; q quits only when
   not editing. Editor handles Unicode insertion/backspace safely and pastes
   within a bounded length. Keep event handling responsive and bounded.
8. Explanation text distinguishes budget from measured usage and physical
   container scope from RSS. No fake OS measurement, future API or algorithm
   described as implemented. Every view remains labeled demo.

**Acceptance and verification:**

- Meaningful state tests: view/focus transitions; typing shortcut characters
  stays in editor; Unicode edit/backspace and length bound; selected node/file
  explanation; supported query yields actual fixture values; changed unknown
  query yields unsupported; both languages and no-color modes.
- Render all views with TestBackend at 160x48, 120x36 and 80x24, asserting
  essential content and no panic. Graph selection must be visible.
- Command snapshot works without TTY; non-TTY interactive attempt fails
  cleanly; normal PTY launch/navigation/quit restores the terminal.
- Run package tests, clippy with warnings denied and fmt. Controller will run
  full workspace checks and independently exercise the binary in a PTY.
- Implementer commits only owned files, writes the report with commands,
  results and limitations, and does not spawn subagents. Aim to hand off by
  18:03 UTC so controller can review and verify within the session.
