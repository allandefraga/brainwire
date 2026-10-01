# Real offline TUI capture — 2026-10-01

These PNGs render actual ANSI output from `brainwire shell --overview --estimated`
in a 120×30 pseudo-terminal, with Tab navigating all four panels. The matching
text files preserve the terminal cells. Rendering used pyte 0.8.2 and Pillow,
Menlo at 18 px, 11×24 px cells, with the emitted foreground/background colors.
They are not generated concept artwork.

The isolated synthetic fixture contained two named RocksDB databases:
`knowledge` (empty) and `python-customers` (10,000 Customer nodes, no edges).
Customer properties email and active were indexed, giving 20,000 index entries.
The fixture used `examples/python/schema.json`, identity-only MERGE and SET with
parameterized UNWIND. Its offline `db check` passed before capture. The temporary
database was removed after capturing; no user dataset was replaced.

Configured shared budgets were cache 64 MiB, memtables 4 MiB and query 32 MiB.
Memory counters belong to the newly opened offline inspection process. The
query budget is explicitly shown as unenforced. The retained local 100K/1M
staging dataset is a different fixture and is not depicted here.

Panels:
- `graph`: graph selection, counts and ontology.
- `pipeline`: current index readiness and the explained Python write path.
- `memory`: shared budgets, accounting and scope explanations.
- `disk`: per-column-family RocksDB metrics; not a physical-file browser.
