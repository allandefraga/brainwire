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
from brainwire import connect
from brainwire.context import ContextClient

# Obtain service_credentials from your backend's protected secret store.
with connect("bolt://127.0.0.1:7687", "company", auth=service_credentials) as graph:
    context = ContextClient(graph)
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

## A context workflow

An application can use these helpers together:

1. Explore **declared types** with `schema_paths` to see which connections the
   ontology allows.
2. Ask its configured search providers for candidate IDs, then use
   `search_context` to retrieve and rank only records visible to the caller.
3. Use `trace_paths` to sample **stored connections** between known visible
   records, and return source references alongside the evidence.

The calls have separate snapshots. Schema routes, record paths and reported
sources each have different meanings; none is a guarantee that an agent's
answer is correct. Let the application decide how to use this untrusted context.

To try the workflow against the local demo, first write the records in the
[quickstart](../README.md#write-and-query-from-python), then run:

```sh
.venv-brainwire/bin/python -m examples.python.agent_context_demo
```

The example reads existing records and uses a fixed demo ID as its provider;
it does not perform text or vector search.

## Explore the ontology before searching

`schema_paths` helps an application find the relationships the ontology
allows between two labels. It uses one authenticated catalog read; it does
not query stored nodes or create Cypher.

```python
from brainwire import schema_paths

paths = schema_paths(graph, "Person", "Project", max_hops=3, max_paths=10)
for path in paths["paths"]:
    print([rel["name"] for rel in path["relationships"]])
```

Every result is marked `schema_only`: a declared path does not prove records
exist or are visible to the caller. Catalog metadata is available to graph
readers and is not filtered by node ACL; do not put secrets in descriptions or
treat them as trusted instructions. Walks are directed and can repeat labels,
stop on reaching the target and are bounded to four hops, 20 results and 8,192
extensions. Equal source and target returns a zero-hop path. `truncated` means
an additional path was found beyond the requested limit; exceeding the work
budget raises `ContextLimitError`, not a partial answer. `max_bytes` bounds
the encoded result, not memory used by the driver when fetching the catalog.

## Trace visible connections

`trace_paths` reads a small sample of paths between two known records. It follows
outgoing relationships by default:

```python
from brainwire import trace_paths

result = trace_paths(graph, ("Person", "person-1"), ("Project", "project-1"),
                     max_hops=3, limit=3)
for path in result["paths"]:
    print(path["nodes"], path["relationships"])
```

Use `direction="incoming"` to follow relationships toward their sources, or
`direction="both"` to follow a connection regardless of its stored direction:

```python
result = trace_paths(graph, ("Document", "doc-1"), ("Person", "person-1"),
                     relationship="AUTHORED", direction="incoming", max_hops=1)
```

The response records the requested `direction`. Node identities are ordered
from the supplied source to the supplied target. In `both` mode, a path can
mix outgoing and incoming steps; its node order does not describe each
relationship's stored source/target. Per-edge orientation is not included.

The graph's normal read grants and node visibility apply at each hop. Paths
contain node identities and relationship types, not source citations or edge
properties. They are unordered samples, not necessarily shortest or exhaustive.
An empty result does not distinguish hidden, missing or disconnected records.
`max_hops` is at most four and `limit` at most five. These bounds constrain
returned paths, not traversal work; configure server query budgets and timeouts.
The helper's own timeout is cooperative and cannot interrupt blocked Bolt I/O.
`max_bytes` bounds encoded output after driver decoding, not process RSS.
Separate calls do not share a snapshot; timing and resource errors can depend on
graph topology. This API does not establish timing non-interference or provide
historical `AS OF` reads.

## Search with configured providers

`search_context` combines ranked candidate IDs from application-configured
providers. It reads the candidates through the caller's graph credentials,
removes hidden or missing records, then ranks the visible records and returns
their reported sources. Providers can represent lexical, vector or other
search systems; the application supplies and configures them.

```python
from brainwire import search_context

# These functions are implemented by your application. They return IDs for the
# supplied graph and label, ranked by the external source.
def lexical_search(query, *, graph_name, label, limit, timeout):
    return my_lexical_index.search_ids(query, graph_name, label, limit, timeout)

def vector_search(query, *, graph_name, label, limit, timeout):
    return my_vector_index.search_ids(query, graph_name, label, limit, timeout)

result = search_context(
    graph, "Document", "release blocker",
    providers={"lexical": lexical_search, "vector": vector_search},
    source_fields={"title": "title", "url": "source_url"},
    properties=["title", "body"], limit=10,
)
for record in result["records"]:
    print(record["properties"], record["citation_id"], record["matches"])
```

Each callback returns a list or tuple of at most 100 IDs scoped to the supplied
graph and label. There can be at most four providers and 100 distinct candidates
in total. Duplicate IDs within a provider are ignored. Provider failures abort
the search; no partial context is returned. The result is bounded by `max_bytes`
and includes only sources for returned records. Citations identify current
records, not immutable revisions or independently verified sources.

Providers are trusted application code, not a sandbox. They may use network
services and can receive the query. Configure their credentials and access
separately. The deadline is cooperative: a blocked callback or Bolt request
cannot be forcibly interrupted by this helper. External indexes and the graph
do not share a snapshot, so candidates can be stale. This does not create
native ANN/BM25 indexes, generate embeddings, guarantee recall or authorize
agent actions. Returned text remains untrusted input for a model.
The intermediate graph read is capped at 1 MiB; the driver may still decode
an individual property before it can be checked. These are not RSS limits.

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
