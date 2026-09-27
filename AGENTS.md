# Brainwire

Brainwire is a standalone graph database repository. Its canonical local path
is `/Users/fraga/Documents/brainwire`, beside `/Users/fraga/Documents/client-platform`.
The old platform feature branch is a historical recovery point, not the active
checkout. Do not edit the old copy or treat the platform's agent routing rules
as instructions for this repository.

## Working agreement

- Keep all work local. Do not push, publish or create a remote PR unless the
  user explicitly asks.
- Read `README.md` and the newest `JOURNAL.md` entry before continuing work.
  Record completed work, verification, decisions and the next step in the
  journal before ending a session.
- Respect the user's requested session duration. The original routine was
  45 minutes; an explicit duration overrides it.
- Code, comments, docs, commit messages and canonical graph/schema identifiers
  are in English. The interface defaults to English, with Portuguese available
  through `--lang pt-BR` (Allan, 2026-09-27). Localization changes display text,
  not stored identifiers or user data. Follow the user's language in conversation.
- Use synthetic data in tests and demos.
- Preserve the selected UI direction. Before UI work, read the Observatory
  design and inspect `docs/design/assets/brainwire-backoffice-user-reference.png`.
  Allan explicitly reconfirmed this composition; the current TUI demo is an
  intermediate step toward it, not a new design reference.
- Preserve existing work. Use `codex/` for new branches unless the user names
  a different branch. Commit only files belonging to the task.

## Plans and verification

The approved design is `docs/superpowers/specs/2026-09-24-brainwire-design.md`.
The current implementation plan is
`docs/superpowers/plans/2026-09-26-brainwire-mvp.md`; the storage foundation
plan also records crash-recovery requirements. Older paths beginning with
`client-brainwire/` refer to this repository root now: drop that prefix.
References to `client-core/` or other platform services refer to the sibling
platform checkout and describe integration targets, not build dependencies.

Use test-first development for engine behavior, followed by task review.
Run the relevant package tests, and consolidate with:

```sh
cargo test --workspace --locked --offline
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
cargo fmt --all -- --check
```

Bolt socket tests need permission to bind localhost. No test server should
remain running after verification. The current MVP is not the final streaming,
memory-budgeted engine; do not claim the five-billion-element target is proven.
