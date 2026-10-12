# Explainable knowledge-source ranking

`knowledge.rank` is a read-only Cypher procedure inspired by the source-ranking
idea described in OntoRank. It is Brainwire's explicit heuristic, not Databricks'
algorithm or an implementation of Genie. It ranks competing sources for an exact
business term, optionally within one domain, and returns each signal's contribution.

## Try it locally

Build `cargo build --locked --offline -p brainwire-server --bin brainwire`.
Use a fresh, synthetic database directory, with no server holding it open:

```sh
export BRAINWIRE_MEMORY_CACHE=64MiB
export BRAINWIRE_MEMORY_MEMTABLES=16MiB
./target/debug/brainwire cypher --database ./data/knowledge-demo \
  --ontology examples/knowledge/schema.json \
  --query "CREATE (s:KnowledgeSource {entity_id:'finance-approved', term:'revenue', domain:'finance', definition:'Recognized revenue', authority:1, authorRelevance:1, certified:true, freshness:1}), (p:KnowledgeSource {entity_id:'popular-sheet', term:'revenue', domain:'finance', definition:'Sales spreadsheet estimate', usage:1})"

./target/debug/brainwire cypher --database ./data/knowledge-demo \
  --query "CALL knowledge.rank({label:'KnowledgeSource', term:'revenue', domain:'finance', topK:5}) YIELD node, score, contributions RETURN node.entity_id, node.definition, score, contributions"
```

`finance-approved` scores approximately `0.90`; `popular-sheet` scores `0.10`.
The five numeric contributions sum to the score. Python can use the same query
through the existing Neo4j driver/Bolt path, replacing the options literal with
`$options` and passing a Python dictionary.

## Policy v1

| Stored property | Input | Maximum contribution |
|---|---|---:|
| `authority` | Number from 0 to 1 | 0.30 |
| `authorRelevance` | Number from 0 to 1 | 0.20 |
| `usage` | Normalized usage evidence from 0 to 1 | 0.10 |
| `certified` | Boolean | 0.25 |
| `freshness` | Number from 0 to 1 | 0.15 |

These weights are a transparent initial product policy, not learned or calibrated
probabilities. Missing/null evidence contributes zero; known signals are not
renormalized. Invalid types, nonfinite numbers or values outside the range fail
the query, even for a candidate that would not make the top K. A failure aborts
prior uncommitted writes in the same transaction.

Ingestion supplies and normalizes these properties. `freshness` does not decay
automatically, `usage` is not a raw count, and certification is a stored flag,
not a verified approval workflow. Higher scores do not establish factual truth.
The server supports optional [named-graph service-token grants](bolt-auth.md),
but no per-source ACL. `domain` is an exact-match relevance filter, **not access
control**. A reader can rank all sources in its allowed graph; use separate graphs
when records require separate permissions. Native TLS remains pending.

## Query and resource contract

- One options map is required: `label` and `term` are nonempty strings;
  optional `domain` is a nonempty string. Matching is exact and case-sensitive.
- `topK` defaults to 10, range 1–1,000. `maxCandidates` defaults to 10,000,
  range 1–1,000,000. Unknown options are rejected.
- Results descend by score; ties use ascending internal node ID. `YIELD`
  exposes `node`, `score` and `contributions`; aliases, post-ranking `WHERE`
  and `RETURN ... LIMIT` use the existing procedure pipeline. A post-ranking
  filter can reduce the result count; it does not backfill lower-ranked sources.
- A ready scalar equality index on `term` streams candidate IDs. Without one,
  the procedure streams the selected label. It reads each candidate once and
  applies term/domain filters before scoring. `maxCandidates` counts visited
  IDs **before** these filters: all matching-term IDs with the index, all label
  IDs without it. Exceeding it fails, never returns a partial ranking.
- Ranking uses O(C log K) time and O(K) retained candidate memory; node decoding
  and selected output properties add temporary/output memory. No adjacency
  matrix or graph-wide PageRank projection is built. Existing conservative
  query guards, shared reservations and cooperative deadlines still apply;
  these are not hard RSS limits. `LIMIT 0` does not skip ranking validation.
- Reads use the transaction snapshot plus its staged writes. Committed changes
  to signals or term indexes affect subsequent transactions immediately;
  rankings are computed on demand and are not stored in a second cache.

## Relation to PageRank

`graph.pagerank` measures structural importance by propagating scores along
directed edges. `knowledge.rank` evaluates explicit source evidence. Neither
automatically proves truth, and this first source-ranking policy does not combine
the two. Automatic provenance extraction, learned weights, usage collection,
time decay and per-source permission-aware ranking remain future work.
Optional named-graph grants already apply to the procedure.
