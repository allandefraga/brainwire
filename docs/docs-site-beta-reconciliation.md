# Documentation-site beta reconciliation

> **Historical note (October 13):** this records a review of an earlier draft,
> not the current site's feature status. Several statements below are now
> outdated, including the claim that online `CREATE INDEX` is unsupported. For
> current instructions, see the [documentation site](../docs-site/index.html),
> [index guide](indexes.md), and [Cypher guide](local-cypher.md).

The pre-existing untracked `docs-site/` draft was inspected on October 13 without
editing or publishing it. Its visual concept can be retained, but the following
claims and samples need reconciliation before it becomes an executable beta guide.
This queue belongs to the October 25 documentation session; it does not mark the
site as beta ready.

| Draft section | Current issue | Supported replacement / source |
| --- | --- | --- |
| `index.html` lines 30–38, hero and feature cards | Documents, embeddings, full-text/vector search and replicas presented as available | State local single-node MVP. Property lists/maps can store data, but dedicated text/vector search is pending. Link [server scope](serve.md) and [indexes](indexes.md) |
| Line 47, index examples | Invented range/text/vector index syntax | Show ontology-declared scalar equality and `CALL db.indexes()`; no supported Cypher `CREATE INDEX`. Link [index guide](indexes.md) |
| Lines 48–49, functions/search | Unsupported `text.contains`, `vector.distance`, `document.chunks`, `vector.search`; `graph.degree` shown as scalar | Show `size`, `coalesce`, `toLower`, `toUpper`, and actual `CALL graph.*` procedures with fixtures from [local Cypher](local-cypher.md) / [analytics](sparse-analytics.md). Mark search vision pending |
| Lines 50–51, storage/recovery | Object-storage snapshots/replicas/startup invariant verification overclaimed | Explain one active owner of a retained volume, explicit offline `db check`, local checkpoints. Readiness is a query probe, not an integrity audit. Link [checkpoints](checkpoints.md) / [orchestrators](orchestrators.md) |
| Line 25, localization | Portuguese described as future without surface distinction | TUI already supports `--lang pt-BR`; site translation remains separate work |
| Lines 42–45, examples | No setup/fixture; unavailable search API | Provide copyable protected quickstart and tested Customer/Ticket queries; verify every command against a new disposable graph |
| Branding | Generic glyph/favicon omit chosen identity | Integrate selected green B/W identity. Current raster is a design reference; vet a small vector/favicon separately |
| Static behavior | Two files, section links only, no real search; clipboard has no failure feedback | Label static local preview, add accessible copy success/failure status, version/status and links to supported docs |

The draft has no package manager/build dependency. A local preview is simply:

```sh
python3 -m http.server 8000 --directory docs-site
```

Google Fonts are currently an external runtime import with system fallbacks.
Choose whether the release preview should remain entirely offline. Do not add an
unnecessary framework merely to correct content and make these links/examples work.
Publication/DNS remain a separate user-authorized step.

Acceptance for this session: every claimed available API has an executable fixture
or current engine documentation source; every future capability is identified as
planned; supported English/Portuguese surfaces are named precisely; all links and
copy controls work locally; screenshots show the actual current candidate.
