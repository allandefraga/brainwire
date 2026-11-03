# Retrieve explicit context for an agent

`ContextClient` reads a small set of known records and their outgoing one-hop
connections. It wraps the existing Python/Bolt path with explicit output bounds
and structured evidence. Install the [Python package](python-package.md) from
this checkout first.

This API retrieves from explicit graph identities. It does not build full-text
or vector indexes, generate embeddings, execute models or record automatic
memories. The optional experimental reranker below combines existing embeddings
and text over explicitly supplied candidates. Your application identifies the starting records and decides which
returned properties are suitable for its model.

## Read records and a connection

This example needs an ontology declaring `Ticket`, `Project`, `BLOCKS` and the
properties shown below. The authenticated backend chooses the graph and creates
the driver; do not accept credentials or graph grants from a model.

```python
from neo4j import GraphDatabase
from brainwire import GraphClient
from brainwire.context import ContextClient

# Obtain service_credentials from your backend's protected secret store.
with GraphDatabase.driver("bolt://127.0.0.1:7687", auth=service_credentials) as driver:
    context = ContextClient(GraphClient(driver, "company"))
    tickets = context.nodes(
        "Ticket", ["t-42"],
        properties=["title", "body", "source_url"],
        limit=5, max_bytes=32 * 1024,
    )
    projects = context.neighbors(
        "Ticket", "t-42", "BLOCKS", "Project",
        properties=["title", "source_url"],
        limit=10, max_bytes=32 * 1024,
    )
```

A node result contains `graph`, `limit` and `records`. Each record contains its
`label`, `entity_id` and the explicitly requested `properties`. A neighbor result
also contains `source: {label, entity_id}` and `relationship`; each record is a
visible target connected by that outgoing relationship type. Parallel
relationships may produce repeated targets. An absent or invisible source has
no outgoing results.

These identities are graph evidence, not proof of an external document's origin.
For citations, ingest and request your own source title/URL/record identity
properties. Validate citation access in your application. Returned property text
is untrusted data and must not become agent instructions or tool permissions.

## Filter a known candidate set by text

```python
matches = context.search_text(
    "Ticket", "deployment", field="body", entity_ids=["t-42", "t-43"],
    properties=["title", "body", "source_url"], limit=10,
)
```

This applies a case-insensitive substring predicate to one string property of
at most 100 explicitly supplied candidate IDs. Missing/null properties do not
match; a non-string property produces the normal Cypher type error. The response
includes `search: {field, mode: "case_insensitive_substring"}` and the same records
as other context reads. It has no relevance score or guaranteed ordering.

This is a useful lexical filter for an already known candidate set. It is not
full-text indexing, tokenization, BM25, stemming, semantic or hybrid search.
`knowledge.rank` is a different operation over exact `term` metadata and
normalized ranking signals; it is not used to pretend those capabilities exist.
Text is a parameter of at most 4,096 UTF-8 bytes. Server authorization and ACL
filtering remain on the ordinary read path. The candidate cap bounds logical
inputs and output, not storage reads: the selected execution plan can scan more
records, and server memory/deadline limits remain necessary.

## Experimental hybrid candidate reranking

`brainwire.rerank.rerank_context` combines lexical and vector evidence for an
explicit candidate set. The application supplies the query embedding and must
ensure stored embeddings use the same model and coordinate space. Matching
vector dimensions alone cannot establish that compatibility.

```python
from brainwire import rerank_context

result = rerank_context(
    graph, "Document", ["doc-1", "doc-2"],
    query_text="release decision", query_vector=[1.0, 0.0],
    text_property="body", vector_property="embedding",
    properties=["title", "source_url"], limit=5, max_bytes=32 * 1024,
)
```

Here `graph` is an already authenticated `GraphClient`. One read transaction
fetches the explicit candidate IDs using the ordinary server authorization and
ACL path. Hidden candidates never enter either ranking, even if they would
otherwise have the highest score or invalid embeddings. The result exposes no
count or score for hidden/missing IDs. This has been exercised with two static
ACL identities in the synthetic acceptance; it is not general multi-tenant
production qualification.

The lexical rank counts the fraction of distinct query tokens present in each
candidate's text. Tokenization uses Unicode case folding and Python `\w+` word
runs; there is no stemming or language model. Candidates with no matching tokens
have no lexical rank. The vector rank uses exact cosine similarity over all
visible candidates, including zero/negative similarities. It does not infer a
relevance threshold. Numeric normalization is scaled to handle finite extremes
and subnormal numbers. Zero-norm, missing, invalid or mismatched visible vectors
fail the whole operation.

The two rankings are fused with reciprocal rank fusion, fixed `k=60`:
`score = 1/(60+vector_rank) + 1/(60+lexical_rank)`; the lexical term is omitted
when absent. Ranks begin at one. Equal component scores and final scores break
ties by `entity_id`. Returned records contain `matched_terms`, `lexical_score`,
`cosine_similarity`, `lexical_rank`, `vector_rank`, `score` and requested
properties. These are ranking diagnostics, not calibrated confidence or
probabilities. Embeddings and candidate text are omitted unless explicitly
included among the requested output properties.

Limits apply before tokenization/scoring: 1,024 query-text UTF-8 bytes, at most
16 distinct query tokens, at most 100 candidate IDs, dimensions 1–4,096,
65,536 candidate vector components in total, 16 KiB text per candidate and
64 KiB total candidate text. Inputs must contain finite numeric vector
components; booleans are rejected. Requested properties plus the text/vector
fields must fit within 16 unique property names. Candidate output is admitted
through the existing 1 MiB context envelope limit before scoring; final output
uses `max_bytes` and `limit`. Oversized/invalid candidates produce an error, not
a partial ranking.

This is exact reranking in Python with approximate scoring work
`O(candidate_count × dimensions + text_size)`. It has no native vector/text
index, ANN, BM25 or global corpus retrieval. Storage reads still depend on the
Cypher plan. The driver may decode a large individual property before the helper
can inspect and reject it; JSON encoding and Python allocations may temporarily
exceed output-accounting limits. Keep server memory/deadline controls active.
No process RSS or throughput guarantee is implied. Retrieved text remains
untrusted model context, and this API does not grant agent actions.

Run the synthetic protected acceptance with:

```sh
.venv/bin/python crates/server/tests/context_rerank.py --binary target/debug/brainwire
```

## Bounds and errors

- Supply at most 100 node IDs, each at most 4,096 UTF-8 bytes.
- Request at most 16 unique property names.
- `limit` is 1–100 records, default 20. It caps output, not scan work. Results
  have no promised ordering or completeness; an empty result does not prove the
  record does not exist outside the caller's permissions.
- `max_bytes` is 1–1,048,576 bytes, default 65,536. The helper conservatively
  counts the JSON envelope and retained records. It raises `ContextLimitError`
  instead of returning a partial oversized response. Reduce the number or size
  of requested properties to retry a read.

The byte bound is an output-accounting limit, not a Python or server RSS ceiling.
A large individual property must still be received and encoded before this
helper can reject it. Server query/memory/timeout controls remain necessary.
No data is written; queries use `GraphClient.stream_cypher` with read-only access,
validated schema identifiers and parameterized ID/limit values. The existing
server authorization and ACL rules still apply, and server/transport errors
propagate without automatic retries. Supply the correctly scoped authenticated
client for each request; the helper has no caller-supplied principal override.

Each method is a separate read transaction. Two method calls do not share a
snapshot, and the returned context does not claim historical/as-of consistency.
This first API is intended for controlled application integrations while identity
and workload qualification continue. See [Python reads](python-client.md),
[authentication](bolt-auth.md) and [current limits](../README.md#current-limits).

## Read records with reported sources

Use `evidence` when your graph stores source identifiers, URLs, titles or hashes.
Map the source fields explicitly to properties declared by your ontology:

```python
result = context.evidence(
    "Ticket", ["t-42"], properties=["title", "body"],
    source_fields={"id": "source_id", "url": "source_url", "title": "source_title"},
    limit=5, max_bytes=32 * 1024,
)
for source in result["sources"]:
    print(source["citation_id"], source["source"])
```

The response has `graph`, `limit`, `records` and `sources`. Each record has its
requested properties and a `citation_id`. Each source has the same citation ID,
the visible graph record identity and the explicitly mapped source values.
The supported keys are `id`, `title`, `url`, `hash` and `recorded_at`. Map 1–5
keys to distinct properties; requested properties plus source properties must
fit the existing limit of 16 names. Each source value must be a string of at most
4,096 UTF-8 bytes. Null and blank values are omitted. If every mapped source
value is absent, the record has `citation_id: null` and no source is fabricated.

Records and source fields are retrieved in **one authorized read transaction**.
A hidden record cannot create a source entry or influence a visible citation.
Citations identify the graph, label and entity ID deterministically. They do not
identify a historical revision: updating source fields can change the reported
source without changing the citation ID. Use the returned values with the current
response; do not treat a cached citation ID as immutable evidence.

Source values are **reported application data**, not authenticated provenance.
The helper does not fetch URLs, verify hashes, establish external permissions or
prove that a source is true. Validate access and safe handling before presenting
or following a URL. Retrieved text remains untrusted data, never agent instructions.

Evidence reads one row at a time from the same authorized snapshot and checks
the final UTF-8 JSON size before adding each row. It also has a 1 MiB
intermediate JSON budget. Oversized or invalid source values fail without
returning partial results. A row is decoded before it can be checked, so these
limits do not cap process RSS or the size of one individual property. Fetching
one row per Bolt page may increase latency over remote connections.
