# 20-type ontology stress fixture

This synthetic schema declares 20 node types and all 20 × 20 directed
source/target combinations: 400 typed relationship signatures, including the
20 self-loops and both directions between every distinct pair. Eight
relationship names are distributed across the matrix. This is a UI and schema
catalog stress case, not a claim that each signature has stored instances.

The fixture itself declares schema only. A prebuilt local graph for hands-on
exploration is also available at `data/ontology20_stress` (ignored by Git): it
contains 20 synthetic node records and 400 synthetic edge records. Verify it
with `target/debug/brainwire db stats ontology20_stress --json` and
`target/debug/brainwire db check ontology20_stress --json`.

Create a fresh empty graph from the schema and open its local, read-only
ontology view:

```sh
cargo run --locked --offline -p brainwire-server -- db create ontology20 \
  --ontology examples/complex-ontology-20/ontology.json
cargo run --locked --offline -p brainwire-server -- shell --overview \
  --graph ontology20 --view ontology
```

In the Ontology screen, press `a` to toggle the complete declared-signature
catalog. The list preserves source, relationship name and target; `PgUp` and
`PgDn` page through it. Press `a` again to return to the selected-type diagram
and inspector. The whole catalog is available by scrolling; a terminal cannot
fit all 400 signatures in one frame. Stop any server that has this data
directory open before using the offline inspector.

The fixture is English-first and synthetic. Its property lists and relationship
signatures are declarations; they do not create instance records.

Captured first and last pages from the prebuilt graph's 400-signature TUI
catalog are in `../../docs/design/assets/2026-10-15-ontology-stress/`.
