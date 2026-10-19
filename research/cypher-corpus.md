# The platform's Cypher corpus

> Inventory made on 2026-09-24 by a search agent sweeping the monorepo. It sets
> the priorities of Brainwire's parser and executor. Counts are distinct
> templates in the code, not executions: the repo has no traffic telemetry.
> `file:line` references are for commit `5dc9ef10b`.

## Main conclusions

1. **There are two paths, with different dialects.**
   - The internal one (`execute`, `execute_batch`, `execute_pipeline`) goes
     through no grammar at all and carries everything the memory-worker and the
     memory-api write by hand.
   - The governed one (`run_read_cypher`) only accepts the subset of the `lark`
     grammar and injects the ACL predicate on every matched node. The MCP's
     `run_cypher`, the console's `/v1/memory/cypher` and part of the memory-api
     reads go through it.
2. **Almost every write is `UNWIND $rows AS row` over three templates** (node,
   edge, event). Batches have 10,000 rows in the code and 250,000 in the
   starter.
3. **The ACL predicate depends on `any()`/`all()`** over a list with
   `coalesce(prop, [])`, `size()` and `IN $list`. HydraDB and PuppyGraph were
   left out of governed reads because they do not evaluate it.
4. **The predicate's iteration variables (`p`, `c`) have the same names as the
   variables models use most**, `(p:Person)` and `(c:Company)`. The engine must
   get the scope right: a quantifier's list is evaluated outside it. No test in
   the repo proves this semantics.
5. **`get_entity_context` looks up `(e {entity_id: $entity_id})` without a
   label.** In the current backends the index is per label.
6. **Absent from production:** variable-length paths, shortestPath,
   `EXISTS {}`, `CALL` subqueries (outside the wipe), `FOREACH`, `REMOVE`,
   `ON CREATE/ON MATCH SET`, `+=`, comprehensions, map projection, regex and
   date functions. `CONTAINS`/`STARTS WITH`/`ENDS WITH` only appear in the
   console's view builder, which the grammar refuses.

## Files cited

| Alias | Path |
|---|---|
| `common` | `external-core/src/external_core/repository/_cypher_common.py` |
| `neo4j` | `external-core/src/external_core/repository/neo4j.py` |
| `falkordb` | `external-core/src/external_core/repository/falkordb.py` |
| `hydradb` | `external-core/src/external_core/repository/hydradb.py` |
| `visibility` | `external-core/src/external_core/auth/visibility.py` |
| `lark` | `external-core/src/external_core/cypher/cypher_acl.lark` |
| `cypher_acl` | `external-core/src/external_core/cypher/cypher_acl.py` |
| `diagnose` | `external-core/src/external_core/cypher/diagnose.py` |
| `pii` | `external-core/src/external_core/pii.py` |
| `catalog` | `external-core/src/external_core/i18n/catalog.py` |
| `generator` | `external-ai/services/memory-worker/memory_worker/cypher_generator.py` |
| `graph_client` | `external-ai/services/memory-worker/memory_worker/graph_client.py` |
| `layout` | `external-ai/services/memory-worker/memory_worker/layout.py` |
| `pipeline` | `external-ai/services/memory-worker/memory_worker/pipeline.py` |
| `graph.py` | `external-ai/services/memory-api/src/memory_api/db/graph.py` |
| `memory.py` | `external-ai/services/memory-api/src/memory_api/api/v1/endpoints/memory.py` |
| `phantom.py` | `external-ai/services/memory-api/src/memory_api/api/v1/endpoints/phantom.py` |
| `query.py` | `external-ai/services/skills-api/src/skills_api/routers/query.py` |
| `mcp_server` | `external-ai/services/mcp-server/src/mcp_server/server.py` |
| `mcp_generic` | `external-ai/services/mcp-server/src/mcp_server/tools/generic.py` |
| `view_modal` | `external-console-ui/app/(app)/memory/components/toolbar/create-view-modal.tsx` |
| `fixture` | `external-core/tests/fixtures/cypher_sql_refusals.json` |
| `spec_err` | `docs/superpowers/specs/2026-09-15-mcp-tool-error-contract-design.md` |
| `gb/` | `external-brain/tools/graph-bench/` |
| `bench/` | `external-bench/` |

## 1. Clauses

| Clause | Templates in production | Real example |
|---|---|---|
| MATCH | ~33 | `MATCH (a:{from_label} {entity_id: $from_entity_id})` and `MATCH (b:{to_label} {entity_id: $to_entity_id})` generator:361-362 |
| OPTIONAL MATCH | 2 | `OPTIONAL MATCH (e)-[r{rel_pat}]-(neighbor) WHERE NOT neighbor:Event{acl_n}` graph.py:565-566 |
| WHERE | ~20, plus what the rewriter injects in every governed MATCH | `WHERE NOT n:Event AND n.__schema_marker IS NULL` layout:257-258 |
| WITH | 0 | only in LLM and bench queries: `WITH n.event_id AS id, count(n) AS c WHERE c > 1` |
| UNWIND | 3 | `UNWIND $rows AS row` in every batch (common:320-322); `UNWIND $items AS item MATCH (n) WHERE id(n) = item.id SET n.x = item.x` layout:485-487 |
| RETURN | ~24 | `RETURN type(r) AS rel, labels(n)[0] AS label, count(*) AS c` graph.py:687 |
| RETURN DISTINCT | 0 | the grammar accepts it; 4 of the 74 refused LLM queries |
| ORDER BY | 3 | `ORDER BY ev.timestamp DESC` graph.py:456 |
| SKIP | 0 | the grammar accepts it |
| LIMIT | ~14 | `LIMIT $limit`; an interpolated literal; `LIMIT $_acl_limit` neo4j:675 |
| UNION | 1 | two branches, each with `LIMIT $limit` (graph.py:563-575) |
| CALL {} IN TRANSACTIONS | 1 | `MATCH (n) CALL (n) { DETACH DELETE n } IN TRANSACTIONS OF 10000 ROWS` neo4j:757, in auto-commit |
| CREATE | 1 template, 2 clauses | `CREATE (ev:Event {...})` and `CREATE (e)-[:HAD_EVENT]->(ev)` generator:441-442 |
| MERGE | 9 | `MERGE (n:{label} {entity_id: $entity_id})` generator:243; `MERGE (a)-[r:{rel_type}]->(b)` generator:363; `MERGE (e)-[r:MENTIONED_IN {source_id: $source_id}]->(s)` graph.py:373 |
| ON CREATE / ON MATCH SET | 0 | |
| SET of a property | 9 | `SET n.p = $p, ..., n.acl_allow = $acl_allow, ..., n.updated_at = $updated_at` generator:243 |
| SET +=, SET of a label, REMOVE, DELETE, FOREACH | 0 | a property is removed with `SET x = NULL`, on purpose |
| DETACH DELETE | 5 | `MATCH (n{label_frag}){where_frag} DETACH DELETE n` neo4j:794; the wipe `MATCH (n) DETACH DELETE n` |
| CALL procedure | 5, without YIELD | `CALL db.labels()`, `CALL db.relationshipTypes()`, `CALL db.indexes()` (falkordb) |
| YIELD | 1, in SHOW | `SHOW INDEXES YIELD labelsOrTypes, properties WHERE $lbl IN labelsOrTypes RETURN properties` neo4j:815-816 |

## 2. Functions and operators

| Item | Production | Example or where it appears |
|---|---|---|
| any() | ACL renderer + 1 | `any(p IN coalesce({var}.acl_allow, []) WHERE p IN $acl_principals)` visibility:255; `any(lbl IN labels(n) WHERE lbl IN $entity_types)` graph.py:794 |
| all() | ACL renderer | `all(c IN {conn} WHERE c IN $acl_open_connectors)` visibility:283 |
| size(), coalesce() | ACL renderer | `size(coalesce(n.acl_connector_ids, [])) = 0` |
| labels() | 4 | `labels(n)[0]`; `NOT 'Event' IN labels(n)` |
| type() | 2 | `type(r) AS rel` |
| id() | 3 | `RETURN id(n) AS nid`; `id(a) IN $node_ids`; `WHERE id(n) = item.id` (layout) |
| keys() | 2, internal use | `RETURN keys(n) AS k LIMIT 1` neo4j:845 |
| count() | 2 | `count(*) AS c` |
| IN | 7 + renderer | `ev.event_type IN $event_types`; `n.entity_id IN $node_ids` |
| IS NULL / IS NOT NULL | 5 and 1 | `n.__schema_marker IS NULL` |
| label test in WHERE | 7 | `NOT neighbor:Event` |
| `<>` between nodes | 2 | `neighbor <> e` |
| list index `[0]` | 2 | `labels(n)[0]` |
| elementId, properties, toLower and family, collect, sum/avg/min/max, CASE, STARTS/ENDS WITH, CONTAINS, `=~`, comprehensions, map projection, EXISTS, path functions, shortestPath, dates | 0 | they appear in LLM-written Cypher (section 6) or in tests |

`pii_token('email', 'x')` becomes a hex literal before it reaches the engine
(pii:609-661); the engine never sees that function.

## 3. Path patterns

- Variable length: 0 in production (the grammar only accepts fixed length). The
  LLM tried `MATCH (cf)-[*1..2]->(node)` once.
- Direction: `->` is the rule; undirected `-[r]-` in 5 templates; `<-` only in
  the graph-bench and in 3 LLM queries.
- Several types `[:A|B]`: 1 site (graph.py:560), with types coming from the LLM
  planner.
- Untyped relationship `-[]-`: graph.py:698.
- Inline properties on a node: ~17 templates, with a label and **without a
  label** (`(e {entity_id: $entity_id})` in graph.py:371, 453, 564, 570, 578,
  685, 698).
- Two-hop chain: `(e {...})-[{rel_pat}]-(mid)-[r2]-(neighbor)` graph.py:570.
- Comma-separated patterns: 2.

## 4. Schema, introspection and procedures

- Index: `CREATE INDEX {name} IF NOT EXISTS FOR (n:{lbl}) ON (n.{p})`
  (neo4j:721-724), named `external_<label>_<sha1>_<prop>`; an error containing
  "already", "exist" or "equivalent" is swallowed as a rerun.
- Who creates indexes: the pipeline, on every run, for `entity_id` of each
  label, for `Event`, for the match_fields and the indexed_fields
  (pipeline:1270-1285).
- Listing indexes: `SHOW INDEXES YIELD labelsOrTypes, properties WHERE $lbl IN
  labelsOrTypes RETURN properties` (neo4j:814-818).
- `CALL db.labels()` and `CALL db.relationshipTypes()` on every `get_schema`,
  with no cache, together with `keys(n)` and SHOW INDEXES per label. The code
  calls `get_schema` "the most-called tool there is" (query.py:153-158).
- Signature samples: `MATCH (a)-[r:{t}]->(b) RETURN labels(a) AS la,
  labels(b) AS lb LIMIT 200` per type (common:157-158) and
  `MATCH (:{a})-[:{t}]->(:{b}) RETURN 1 AS ok LIMIT 1` (common:182).
- Health: `RETURN 1`.
- Not used: CREATE CONSTRAINT, db.schema.*, db.propertyKeys, dbms.*, apoc.

## 5. Reserved ACL parameters

- The predicate comes from `AclPredicate.to_cypher` (visibility:237-284) and the
  parameters from `bound_acl_params` (visibility:286-301): `acl_principals`
  whenever there is a principal; `acl_connectors` with the entry axis on and a
  non-empty list; `acl_open_connectors` with a non-empty open set. All of them
  are lists of lowercase strings.
- The MCP token always turns the entry axis on.
- `bind_acl_params` refuses caller parameters prefixed `acl_` or `_acl` and
  overwrites them with the server's values (common:433-455).
- The rewriter injects the predicate on every node of every MATCH and OPTIONAL
  MATCH of every UNION branch; an anonymous node gets the variable
  `_acl_anon_N`.
- `get_graph` and `get_neighborhood_schema` inject the predicate by hand
  through the raw path (graph.py:530-539, 669-678) and the endpoint checks again
  in Postgres; `get_timeline` carries no predicate (the root is checked in
  Postgres).

A real example of the shadowing, the rewriter's output for
`MATCH (p:Person)-[:TRABALHA_EM]->(:Company) RETURN p.name`:

```
MATCH (p:Person)-[:TRABALHA_EM]->(_acl_anon_0:Company) WHERE (any(p IN coalesce(p.acl_allow, []) WHERE p IN $acl_principals) AND NOT any(p IN coalesce(p.acl_deny, []) WHERE p IN $acl_principals)) AND (...) RETURN p.name
```

The post-filter (common:330-431) drops the whole row if any node reachable in
any cell is invisible, judging each node by `acl.is_visible` over the returned
`acl_*` properties; then it removes every `acl_*` key. Relationships are not
judged, and the relationships of a Path are not visited.

**Consequences for Brainwire:**

- Through an unchanged `neo4j.py`, every node returned to a service session must
  carry the `acl_*` properties, or the post-filter discards every row of a
  caller that is not allow_all. The engine materializes them from the ACL set.
- Scalars and aggregates are only protected by the predicate in the MATCH; the
  post-filter does not judge scalars. Brainwire's native ACL closes that gap.

## 6. LLM-written Cypher

- Only the MCP `run_cypher` tool receives free Cypher (mcp_server:629-648
  through query.py:386-477 and `run_read_cypher`).
- The instruction to the model is the tool description (catalog:426-441 in PT,
  1301-1316 in EN): call `get_schema` first, prefer indexed properties, and the
  accepted subset (fixed-length MATCH/OPTIONAL MATCH/WITH/RETURN, WHERE,
  aggregates, ORDER BY/SKIP/LIMIT, UNION; no CONTAINS and family, no UNWIND,
  CALL, subqueries, variable-length paths, properties()/keys(), no mutation),
  with a ceiling of 200 rows.
- The `get_schema` description says the graph is "(FalkorDB)", whatever the
  backend.
- Measured in run g4: 175 `run_cypher` calls, 104 (59%) without a usable
  result; of the 49 refusals, 12 were for a function and 37 for syntax (19 for
  CONTAINS/STARTS WITH/ENDS WITH, 3 for `=~`, 3 for LIKE, 1 for GROUP BY).

Constructs in the sample of 74 refused LLM queries (fixture):

| Construct | Queries |
|---|---|
| inline property in the pattern | 30 |
| CONTAINS | 29 |
| lookup by entity_id | 28 |
| properties() | 17 |
| count() | 16 |
| LIMIT | 16 |
| undirected pattern | 14 |
| toLower | 12 |
| ORDER BY | 11 |
| trailing `;` | 9 |
| literal `\n` | 8 |
| coalesce | 8 |
| type() | 7 |
| pattern inside WHERE (`NOT (c)--(:Ticket)`) | 5 |
| RETURN DISTINCT, WITH | 4 each |
| OPTIONAL MATCH, collect(DISTINCT), `<-`, `=~`, LIKE, `x.*` | 3 each |
| ENDS WITH | 2 |
| STARTS WITH, `size((c)--())`, EXISTS {}, variable-length path, slice `[0..5]`, GROUP BY, toDate | 1 each |

## 7. Batched writes

- The generator produces one (cypher, params) pair per row; the pipeline flushes
  every `_batch_size` rows (10,000 in the code, 250,000 in the starter).
- Edges and events (they start with MATCH) go to `execute_pipeline`; node MERGE
  goes to `execute_batch` (graph_client:99-111, 199-214).
- The UNWIND fold (common:288-322) groups consecutive statements with identical
  text, replaces `$name` with `row.name` outside literals and sends
  `{"rows": [...]}`. A group of one statement is not folded, and if folding
  fails it falls back to statement by statement: the engine must accept both
  shapes.
- A 250,000-row batch goes over 200 MB in a single parameter (an estimate from
  about 1 KB per node).

What reaches the engine through `neo4j.py`:

```
UNWIND $rows AS row
MERGE (n:Customer {entity_id: row.entity_id})
SET n.name = row.name, n.source = row.source, n.acl_connector_ids = row.acl_connector_ids, n.acl_source_table = row.acl_source_table, n.acl_allow = row.acl_allow, n.acl_deny = row.acl_deny, n.connector_id = row.connector_id, n.display_name = row.display_name, n.display_tier = row.display_tier, n.display_color = row.display_color, n.updated_at = row.updated_at

UNWIND $rows AS row
MATCH (a:Person {entity_id: row.from_entity_id})
MATCH (b:Company {entity_id: row.to_entity_id})
MERGE (a)-[r:WORKS_AT]->(b)
SET r.role = row.role, r.last_activity_at = row.last_activity_at

UNWIND $rows AS row
MATCH (e:Customer {entity_id: row.entity_id})
CREATE (ev:Event {event_id: row.event_id, type: row.event_type, timestamp: row.ev_timestamp})
CREATE (e)-[:HAD_EVENT]->(ev)
```

Semantics the batch assumes:

- a MERGE inside the UNWIND sees the earlier rows of the same batch;
- an edge only exists if both MATCHes find their nodes, otherwise it vanishes
  silently;
- `SET` with NULL removes the property;
- the event CREATE is not idempotent.

Row values: str, int, float, bool, None and lists of str. Datetime, Decimal and
UUID arrive as strings or floats.

## 8. What `neo4j.py` requires from Bolt

- A driver with `notifications_disabled_classifications=["UNRECOGNIZED"]` and
  `verify_connectivity()`; schemes bolt, bolt+s, bolt+ssc, neo4j, neo4j+s,
  neo4j+ssc.
- Every query goes through `execute_query` (a managed transaction, write route
  by default, reads included), with a `database_` that may be None.
- The wipe needs RUN in auto-commit (IN TRANSACTIONS does not run inside an
  explicit transaction).
- Values: Node with labels, element_id and properties; Relationship with type,
  start_node, end_node and element_id; Path. `element_id` stable within one
  result; `id()` returns an integer.
- Errors: a driver failure and a transient error stop the batch; "unsupported
  syntax" is recognized by text ("invalidinput", "syntaxerror",
  "unsupportedadministrationcommand", "notsupported"); a duplicate index by
  "already", "exist" or "equivalent". Neo4j-style codes keep this behavior.
- The skills-api builds a new driver and runs `verify_connectivity` on every
  `get_schema` and `run_cypher` request.

## 9. Inconsistencies found in the platform

- The console's view builder offers CONTAINS, STARTS WITH and ENDS WITH, which
  the grammar refuses; the preview returns 400.
- The `/v1/memory/cypher` error message says UNWIND is accepted
  (graph.py:1009-1010); the grammar refuses it.
- The `get_schema` description says the graph is FalkorDB, whatever the
  backend.
- The event is written with `type` (generator:412), but the timeline filters and
  reads `event_type` (graph.py:448, 468).
- The skills-api mutation regex lacks the lookbehind and the literal masking of
  the core (query.py:94-97 against common:49-68).

## 10. The 20 constructs by priority

1. `UNWIND $rows AS row` with a large list of maps and `row.field` access.
2. `MERGE (n:L {entity_id: row.entity_id})` with an index lookup, seeing the
   batch's earlier rows.
3. `SET n.p = expr, ...` with more than 10 items, lists, and `= NULL` removing.
4. `MATCH (a:L {entity_id: row.x})` with an index inside the UNWIND, in two
   consecutive MATCHes.
5. `MERGE (a)-[r:T]->(b)` between bound variables, with `SET r.p` and inline
   properties.
6. `CREATE (ev:L {map})` and `CREATE (e)-[:T]->(ev)`.
7. The ACL predicate (`any()`/`all()` over `coalesce(n.list, [])`, `IN`,
   `size()`, NOT/AND/OR) with the right variable scope.
8. A node with an inline map, with or without a label: `(e {entity_id: $id})`
   needs a global lookup by `entity_id`.
9. Fixed-length relationships: `->`, `-[r]-`, `:A|B`, `-[]-`, two-hop chains.
10. `OPTIONAL MATCH ... WHERE`, with NULL when absent.
11. Predicates: `NOT n:L`, IS NULL, IS NOT NULL, `IN $list`, comparisons on
    strings and numbers, `<>` between nodes.
12. RETURN of whole Nodes, Relationships and Paths, with `acl_*` for service
    sessions, aliases and constants (`RETURN 1`).
13. `count(*)` with implicit grouping.
14. `ORDER BY alias [DESC]` and `LIMIT` with a parameter or a literal.
15. `UNION` with a LIMIT per branch.
16. `labels()`, `type()`, `id()`, `keys()` and index `[0]`.
17. `CREATE INDEX name IF NOT EXISTS FOR (n:L) ON (n.p)`.
18. `CALL db.labels()`, `CALL db.relationshipTypes()`, `SHOW INDEXES YIELD ...
    WHERE ... RETURN`.
19. `MATCH ... DETACH DELETE n` and `CALL (n) { DETACH DELETE n } IN TRANSACTIONS
    OF 10000 ROWS` in auto-commit.
20. What models use and the grammar accepts: `WITH` with WHERE after
    aggregating, `RETURN DISTINCT`, `collect(DISTINCT x)`, `size()`, `toLower`,
    `coalesce` with a string default, `min`/`max`.

Outside the list, with measured demand: CONTAINS and family (19 of the 37
syntax refusals in g4) and `properties()` (12 refusals, blocked on purpose by
IR5). The graph-bench gate also needs `<-`, four-hop chains and
`UNWIND ... CREATE ... RETURN count(b)`.
