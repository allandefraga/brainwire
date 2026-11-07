# Brainwire

Brainwire is a standalone graph database repository. Its canonical local path
is `/Users/fraga/Documents/brainwire`; it has no required sibling checkout.

## Working agreement

- For completed changes, keep `main` current, update the README and relevant
  documentation in simple language, and create and push a new release tag after
  the required checks pass. Allan requested this as the default on 2026-11-02;
  explicit later instructions can override it. Do not publish unfinished or
  failing work. Remote PRs still require an explicit request.
- Keep this project independent and organization-agnostic. Do not add customer
  or organization names, domains, credentials or private integration details to
  the repository. Use synthetic, generic examples and audit before release.
- Read `README.md` and the newest `JOURNAL.md` entry before continuing work.
  Record completed work, verification, decisions and the next step in the
  journal before ending a session.
- Respect the user's requested session duration. The original routine was
  45 minutes; an explicit duration overrides it.
  Through October 31, 2026, the current planned cadence is **20 minutes per day**,
  following `docs/2026-10-pilot-roadmap.md`. A later explicit duration still wins.
  This plan does not create scheduled/automatic runs.
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

The original approved design and MVP plan are summarized in
`docs/superpowers/specs/2026-09-24-brainwire-design.md` and
`docs/superpowers/plans/2026-09-26-brainwire-mvp.md`; their full historical text
remains in Git history. Use `docs/production-readiness-roadmap.md` and the
latest journal entry for current work. Older paths beginning with `brainwire/`
refer to this repository root now.

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
