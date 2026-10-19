# Brainwire: design

> **Complete on 2026-09-25.** Sections 1 to 4 were approved by Allan one by one.
> Section 5 and the self-review fixes (RBAC database access, notes written by
> readers) were decided under Allan's go-ahead to proceed without asking, and
> are listed in the JOURNAL for his review. Plan and scope:
> [PLAN.md](../../../PLAN.md). Progress: [JOURNAL.md](../../../JOURNAL.md).

Sections:

1. Storage
2. Data model and security: ontology, ACL, RBAC, events
3. Query and protocols: Cypher, TCK, Bolt, Arrow Flight, Python SDK
4. Algorithms, AI and CLI
5. Cluster, operations and tests

Product requirements added by Allan on 2026-09-27: rich property data,
Qdrant-style document/vector search and recovery across container/machine
failures. The [data and recovery addendum](../../design/2026-09-27-brainwire-data-and-recovery.md)
records current support, proposed implementation and acceptance criteria.
Vector search and production clustering remain unimplemented; the addendum
does not claim API compatibility with Qdrant or replace the sections below.

## 1. Storage (approved 2026-09-25)

### 1.1 One RocksDB per database

Each database is a RocksDB instance in `data/db/<name>/`. The `system`
database, in `data/system/`, holds the list of databases and aliases, users,
roles and privileges.

Three resources belong to the process, not to the database: a single block
cache (HyperClockCache), a single memtable manager (the memtables of all
databases add up to a ceiling that is charged to the cache) and a single pool of
flush and compaction threads. With that, ten databases fit in the same memory
budget as one.

### 1.2 Column families in each database

Keys are big-endian, so byte order is logical order.

| Family | Key | Value | Use |
|---|---|---|---|
| `meta` | name | versioned schema, dictionaries (strings, principals, connectors), counters | catalog |
| `node` | node id | node row | read properties; scan a label |
| `pk` | `entity_id` · type | node id | MERGE and key seek, with or without a label |
| `out` | source · edge type · target · edge id | ACL set and edge properties | expand outward |
| `in` | target · edge type · source · edge id | empty | expand inward |
| `lbl` | label · node id | empty | labels beyond the primary type |
| `idx` | index · sortable value · node id | empty | exact, range and list indexes |
| `ident` | identifier type · value · node id | empty | find by CPF, e-mail or license plate without knowing the label |
| `event` | stream · entity · (MAX minus time) · seq | ACL set and properties | timeline, newest first |
| `acl` | set id | allow, deny and connectors, as interned ids | ACL sets |

Consequences of the layout:

- The `pk` starts with `entity_id` because the platform looks nodes up without
  a label (`(e {entity_id: $id})`, seven memory-api reads, see
  [research/cypher-corpus.md](../../../research/cypher-corpus.md)). With a label
  it is a get; without one, a short range over the same `entity_id`. The
  `entity_id` is encoded with an escaped terminator (0x00 becomes 0x00 0xFF,
  and the value ends with 0x00 0x01), which keeps byte order and makes the
  encoding prefix-free, so `user:7` does not match `user:70`.
- A node's edges are contiguous in `out` and in `in`. Expanding a node, with or
  without a type, is a range read, and so is `DETACH DELETE`.
- Parallel edges exist, because openCypher requires them: `CREATE` always
  creates. `MERGE` looks up the (source, type, target) prefix before creating,
  so it never duplicates. An edge created by `MERGE` uses id 0, which zstd
  compresses to almost nothing; only a parallel edge gets its own id.
- `MATCH` respects relationship isomorphism (an edge does not appear twice in
  the same pattern), the rule FalkorDB breaks in doc 27.
- An index on a list property stores one entry per item, so
  `WHERE x IN n.emails` is a seek.
- The inverted time in the event key makes "the last 50" a forward read.

### 1.3 IDs

| Element | Format (64 bits) |
|---|---|
| Node | partition (10) · type (12) · sequence (42) |
| Edge | partition (10) · sequence (54) |

The type inside the node id lets the `node` key serve both purposes: a seek by
id is a get, and a label scan is a range. That allows up to 4,096 types per
database and 4.4 trillion nodes per type in each partition. A node without a
label (`CREATE ()`) uses type 0, and labels added later go to `lbl`.

The partition is always 0 in this version. It is the open door to partitioning a
graph across machines later without rewriting ids.

The Bolt 5 `elementId` is a string that describes the position: `n:<id>` for a
node and `e:<source>:<type>:<target>:<id>` for an edge. Finding an element by
`elementId` needs no extra index.

Ids are allocated in blocks: the end of a block is committed to `meta` before
any id in it is handed out, so a crash skips the rest of the block and never
reuses an id.

### 1.4 Transactions

- Each transaction reads from a RocksDB snapshot taken at its start (snapshot
  isolation) and accumulates its writes in its own overlay, an ordered map per
  column family that the transaction's reads merge with the snapshot. The
  `rocksdb` crate does not expose WriteBatchWithIndex, and OptimisticTransactionDB
  refuses range deletes and keeps up to 1.25 GiB of history per database; the
  overlay in Rust avoids both and needs no FFI.
- One committer per database checks for conflicts: if another transaction
  committed a write first to a key this one wrote or depended on (the absence of
  a `pk` that a `MERGE` read, for example), this one aborts. First committer
  wins.
- The conflict comes out as a transient error (`Neo.TransientError.Transaction.*`),
  which the Neo4j drivers already retry on their own in managed transactions.
- The committer groups the validated transactions and writes one WriteBatch per
  group, with a WAL fsync, and acknowledges them all together.
- The validated WriteBatch is the unit Raft replicates in the cluster (section
  5): the same code applies it, with or without a cluster.

### 1.5 Memory: 16 GB per node

| Part | Size | Note |
|---|---|---|
| Block cache | ~10 GB | the memtables are charged inside it, leaving ~9 GB for blocks and partitioned indexes and filters (top level pinned, 16 KB blocks) |
| Memtables | ~1 GB, inside the cache | total across all databases, with `allow_stall = false` so one full family does not stall writes to the other databases |
| Execution | ~2 GB | a ceiling per query; a query that goes over fails with a clear error |
| System and headroom | ~4 GB | page cache, connections, threads |

The acceptance test runs with the container limited to 16 GB (`--memory=16g`).

### 1.6 Compression, compaction and deletion

- LZ4 on the upper levels and ZSTD with a trained dictionary on the last one.
- A filter chosen per family, because a 10-bit bloom on ~10 billion keys would
  cost 12.5 GB: an 8-byte prefix (the source) in `out` and `in`; a 10-byte
  prefix (stream and entity) in `event`; the whole key only in `pk`; `node` with
  no filter on the last level (`optimize_filters_for_hits`), because a read by
  id almost always hits. The total stays between 2.3 and 5.4 GB, inside the
  cache.
- Deletion writes a tombstone. Whole ranges (a node's edges, an entity's events,
  a database) use range deletes.
- Physical purge for LGPD: a forced compaction over the affected ranges, then a
  check of open snapshots, obsolete files, WAL, checkpoints and backups. Only
  then does the engine report that the data no longer exists on disk. TTL alone
  does not guarantee a deadline. Backups have their own retention, documented
  alongside.

### 1.7 Bulk loading

- Online: Arrow batches from the SDK become large WriteBatches. Inside `bulk()`
  the WAL is off, with a flush and sync at the end. A crash in the middle of a
  `bulk()` means redoing the load, and the API says so.
- Offline: external sort per family, SSTs written directly and ingested into the
  last level of a new database, with no compaction. Then an alias swap. Through
  the memtables, loading 5 billion would write ~11.6 TB to disk because of write
  amplification; through ingestion, each byte is written once.
- The SstFileWriter with a ZSTD dictionary holds the whole file uncompressed in
  memory until it closes, and the crate does not expose the limit. The SST size
  and the number of parallel writers set the load's RAM peak (four writers of
  256 MB, for example, stay around 1 GB).
- The gaps in the `rocksdb` 0.25 crate and the way around each one are in
  [research/rocksdb.md](../../../research/rocksdb.md): the `raw-ptr` feature
  gives the C API pointer, to call whatever is missing through FFI.

## 2. Data model and security (approved 2026-09-25)

### 2.1 Schema from the ontology

The memory-api's `POST /v1/ontology/apply` now sends the `GraphMappingConfig`
(the same YAML the memory-worker's Pydantic model validates) to Brainwire, which
applies it as DDL. Each application creates a new schema version. Old rows stay
readable because they carry the version they were written with; a new column
rewrites nothing.

| In the ontology | In Brainwire |
|---|---|
| `nodes[].label` | node type (the 12 bits in the id) |
| `entity_id` | the `pk` key |
| `properties` | typed columns; the type comes from the clean layer through the catalog |
| `er_fields`, `indexed_fields` | indexes in `idx` |
| `identifier_columns` | entries in `ident` (identifier type + value) |
| `display_tier`, `display_color` | type metadata, outside the node |
| `edges[]` | edge type with a (from, to) signature |
| `timeline[]` | event stream per entity type |

Rules that keep all of Cypher valid:

- Properties are dynamic in openCypher. A value that does not match the column's
  type goes to that row's flexible map, and a metric counts how often that
  happens. The common case stays compact and no case breaks.
- A node with a label outside the ontology (the whole TCK, ad hoc queries) uses
  type 0, with labels in `lbl` and properties in the flexible map.
- The edge signature applies to ontology types: writing `WORKS_AT` between
  labels the ontology does not declare returns an error. A type outside the
  ontology has no signature. Each type can be marked lenient.
- `SET x = NULL` removes the property, as the platform already assumes.
- Several `nodes[]` entries may share a label when one entity is fed by several
  connectors (Person from HubSpot, Zendesk and Jira). They become one node type:
  the columns are the union of their properties in declaration order, indexes and
  identifiers are unions, and the first `display_color` declared wins, as in the
  platform's `presentation_is_resolved_per_label`. The memory-worker's validators
  already reject the harmful cases (sources whose identifiers cannot converge),
  so Brainwire merges instead of refusing (decided 2026-09-26).

### 2.2 ACL: the external application model, inside the engine

Storage:

- Principals and connectors become 32-bit ids in the `meta` dictionaries.
- Each distinct combination of (allow, deny, connectors) becomes a set in `acl`,
  and the element stores only the set id.
- On write, `SET n.acl_allow = ...`, `n.acl_deny` and `n.acl_connector_ids`
  (coming from the `cypher_generator` over Bolt) and the `acl_*` columns of an
  Arrow batch are intercepted and become the set id. `acl_source_table` stays as
  a hidden property, outside the rule.

Reads:

- The rule is `AclPredicate.is_visible`, unchanged: `ENTRY and (OPEN or row
  ACL)`, with the entry exemption for objects without provenance and the strict
  mode.
- Per session, the engine builds the bitmap of visible sets from inverted
  indexes (principal to sets in allow, principal to sets in deny, connector to
  sets), caches it by the predicate's hash and extends it when new sets appear,
  since set ids only grow.
- An invisible node does not exist for the query: it does not enter counts,
  aggregations or paths. An edge is visible when both endpoints are visible and
  its own set, if any, also allows it.

Sessions:

- **Service** (today's `neo4j.py`, authenticated with the service principal):
  the reserved parameters (`$acl_principals`, `$acl_connectors`,
  `$acl_open_connectors`) become the query's native predicate. The predicate
  text the adapter injects is still evaluated, over virtual `acl_*` properties,
  and gives the same result. Returned nodes carry the virtual `acl_*`
  properties, because the adapter's post-filter depends on them to keep the row,
  and the adapter strips them before answering.
- **User** (CLI, BI, any direct connection): logs in with the
  external-identity JWT, validated through the JWKS. Brainwire derives the
  principals like `_principals_for` and the entry axis like
  `entry_connectors`. The set of open connectors stays empty until the
  memory-api publishes it through the admin API, which falls on the safe side:
  row ACL everywhere. An `acl_*` parameter from a user session is refused, and
  `acl_*` never shows up in results, `keys()` or `properties()` (IR5).

Verification: a differential test against the Python `is_visible`. A generator
creates lists, entry axes, open sets and strict mode in bulk, and the engine and
Python must agree on every case.

**Event visibility (decided by Allan on 2026-09-25):** an event inherits the
visibility of its owning entity, unless it comes stamped with its own ACL.
Today an event is born without `acl_allow` and is invisible in governed reads;
the timeline only works because `get_timeline` reads through the raw path and
checks the root in Postgres. With inheritance, governed reads return events to
whoever can see the entity, which matches what `get_timeline` already does.

### 2.3 RBAC

Simple version, decided by Allan on 2026-09-24.

- Users and roles in `system`. Three fixed roles: `reader` (reads), `editor`
  (reads and writes), `admin` (everything, including databases and users).
- Each user has one role and a list of databases it can access, all of them by
  default. The role applies to every database on that list.
- Commands in Neo4j syntax where one exists: `CREATE USER`, `DROP USER`,
  `GRANT ROLE`, `REVOKE ROLE`, `SHOW USERS`, `SHOW ROLES`; the database list is
  set with `ALTER USER <name> SET DATABASES [<db>, ...]`, a Brainwire extension.
- The roles in the external application JWT (`role:admin`, `role:consumer` and the others)
  map to the three through configuration.
- RBAC decides what a person can do (read, write, administer); what they see
  stays with the row ACL. A graph write from a `reader` is refused before it
  runs. Notes (section 4.4) are annotations, not graph data, so a `reader` can
  add them.

### 2.4 Events

- Each `timeline[]` item becomes a stream (event type, entity label), stored in
  `event` newest first.
- Compatibility: `(e)-[:HAD_EVENT]->(ev:Event)` keeps working. The event shows
  up as a virtual node labeled `Event` and the edge as a virtual edge; an
  `ORDER BY ev.timestamp DESC LIMIT n` becomes a range read.
- Writes: the pair `CREATE (ev:Event {...})` + `CREATE (e)-[:HAD_EVENT]->(ev)`
  in the same statement, which is what the generator emits, goes straight to
  `e`'s stream. An `Event` created any other way becomes a regular node:
  correct, just less compact.
- The SDK offers an optional `append` that is idempotent by `event_id`, so
  reprocessing a batch does not duplicate events as it does today.

### 2.5 Identifiers

`identifier_columns` feeds `ident`: (identifier type, normalized value) points
to the node, in any label. It is the lookup doc 24 asks for. With PII
tokenization on, the engine only sees tokens (`pii_token()` is resolved before
reaching it), and equality still works.

## 3. Query and protocols (approved 2026-09-25)

Inputs: [research/cypher-corpus.md](../../../research/cypher-corpus.md) (what
the platform sends today) and [research/bolt.md](../../../research/bolt.md)
(what the Neo4j drivers require).

### 3.1 Cypher front end

- **Parser:** hand-written recursive descent, with Pratt parsing for
  expressions, following the public openCypher grammar plus the Neo4j 5 forms
  the platform uses (`CALL (n) { ... } IN TRANSACTIONS OF n ROWS`, `SHOW INDEXES
  YIELD ... WHERE ... RETURN`, `CREATE INDEX name IF NOT EXISTS FOR (n:L) ON
  (n.p)`, and the database and user administration commands). Hand-written
  rather than generated, because the platform's LLM tools depend on clear error
  messages that name the construct and its position.
- **Semantic analysis:** variable scopes, types and aggregation grouping. The
  list of a quantifier is evaluated in the outer scope, so the ACL predicate
  `any(p IN coalesce(p.acl_allow, []) WHERE p IN $acl_principals)` works when
  the user's own node is also called `p`; a dedicated test covers it.
- **Planner:** rule based, with simple statistics kept as counters in `meta`
  (nodes per type, average degree per edge type). The anchor is chosen in this
  order: `pk` seek, `ident`/`idx` seek, label scan, full scan. Filters are pushed
  down, the ACL check included. `ORDER BY` with `LIMIT` becomes a top-k.
  `OPTIONAL MATCH` is a left outer apply. Variable-length expansion enforces
  relationship isomorphism. `MERGE` records the `pk` read as a dependency of the
  transaction, so two concurrent MERGEs of the same key cannot both create
  (section 1.4).
- **Executor:** pull based, in batches of about 1,024 rows, one thread per
  query, many queries in parallel. Each query has a memory ceiling inside the
  2 GB execution budget; going over ends the query with an explicit error.
- **Values:** the openCypher type system (null, boolean, 64-bit integer,
  float, string, list, map, node, relationship, path, temporal types, duration,
  point), with three-valued logic and the TCK's ordering and equality rules.

### 3.2 TCK

- The openCypher TCK scenarios (Gherkin, Apache 2.0) are vendored into the repo
  with attribution and run in process by a Rust harness: set up the graph, run
  the query, compare rows, side effects and expected errors.
- Output: the pass rate per feature, reported at every gate, plus a committed
  list of passing scenarios. CI fails when a scenario that passed stops passing.

### 3.3 Bolt server

- `tokio`, optional TLS (`rustls`), PackStream v1, chunked transport.
- Versions offered: 5.1 to 5.4, without the 5.7 manifest. Never 5.5, which some
  drivers know but no Neo4j server negotiates. Staying at or below 5.6 also
  keeps the classic `{code, message}` error format.
- HELLO answers with `server` set to
  `Neo4j/5.26.0 compatible graph database server - Brainwire`, the form
  Memgraph uses (decided with Allan on 2026-09-26). The `Neo4j/` prefix is a
  hard requirement of driver 5.x (it raises `UnsupportedServerProduct`
  otherwise; driver 6.x dropped the check), and the rest says the server is
  not Neo4j itself, whose name is a trademark of Neo4j, Inc. The Brainwire
  version goes in the `hints` and in `CALL dbms.components()`.
- LOGON accepts three schemes: `basic` (users in `system`, argon2 hashes),
  `bearer` (the external-identity JWT, validated by JWKS) and `none` only when
  enabled for local development. A token carrying the service capability opens
  a service session (section 2.2).
- Messages: RUN, PULL and DISCARD with `qid`; BEGIN, COMMIT and ROLLBACK; RESET;
  GOODBYE; TELEMETRY accepted and ignored; ROUTE for `neo4j://`. After a
  FAILURE, IGNORED until RESET, as the state machine requires.
- The database comes from `db` in BEGIN or RUN, with a configurable default
  (`default`); administration runs against `system`.
- Bookmark: `bw:<database>:<commit index>`. On a single node the index is the
  commit sequence; in the cluster, the Raft index.
- Errors use the Neo4j codes the platform already recognizes by text:
  `Neo.ClientError.Statement.SyntaxError` for unsupported syntax,
  `Neo.ClientError.Schema.EquivalentSchemaRuleAlreadyExists` for a repeated
  index, `Neo.TransientError.Transaction.*` for commit conflicts,
  `Neo.ClientError.Cluster.NotALeader` for a write on a follower and
  `Neo.TransientError.Transaction.BookmarkTimeout` for a bookmark the follower
  has not reached.
- Large parameters: the platform's 250,000-row batches reach about 200 MB in a
  single parameter. The decoder builds values straight from the chunks, without
  a second copy, and the limit per message is configurable (256 MB by default),
  with a clear error above it.
- `CALL { ... } IN TRANSACTIONS` only runs in auto-commit, like Neo4j, which is
  how the wipe path already sends it.

### 3.4 Compatibility surface for the platform

What the corpus shows the platform calls, all of it in the first Cypher
milestone: `UNWIND $rows AS row` folded batches and the same statements unfolded;
`MERGE` of nodes and edges with long `SET` lists, `SET x = NULL` removing;
`CREATE` of events; the ACL predicate with `any`, `all`, `size`, `coalesce` and
`IN`; label-less `(e {entity_id: $id})`; fixed-length patterns in both
directions, `[:A|B]`, two-hop chains; `OPTIONAL MATCH`; `UNION`; `count(*)`,
`ORDER BY`, `LIMIT` with a parameter; `labels()`, `type()`, `id()`, `keys()`,
list indexing; `CALL db.labels()`, `CALL db.relationshipTypes()`, `SHOW INDEXES`,
`CREATE INDEX ... IF NOT EXISTS`; `DETACH DELETE` and the wipe; `RETURN 1` for
health.

### 3.5 Arrow Flight and the Python SDK

- Flight service on `:7688` (`arrow-flight` over gRPC), with the same
  authentication as Bolt:
  - `DoPut` streams record batches for `merge_nodes`, `merge_edges`,
    `append_events` and `delete`, through the committer or inside `bulk()`;
  - `DoGet` runs Cypher and streams the result as Arrow;
  - `DoAction` covers administration: `apply_ontology`, `create_index`,
    databases, `bulk` begin and end, backup.
- The `brainwire` Python package wraps `pyarrow.flight` with the API in the plan
  (section 6). Anything that exposes Arrow (pandas, polars, pyarrow, DuckDB
  relations) goes in without a copy, and results come back with `to_arrow()`,
  `to_pandas()` and `to_polars()`.
- The platform adapter `external_core/repository/brainwire.py` implements the
  port: governed reads send the caller's `AclPredicate` as session context and
  need no post-filter, `merge_node` and bulk writes go through Flight, and the
  Cypher write hatches (`execute`, `execute_batch`, `execute_pipeline`) go
  through Bolt. Until it lands, `neo4j.py` talks to Brainwire over Bolt with no
  change.

## 4. Algorithms, AI and CLI (approved 2026-09-25)

### 4.1 Algorithms

Local algorithms start from a node and run on storage, through the same scan and
expand operators, so the caller's ACL and RBAC apply to every step:

| Procedure | What it returns |
|---|---|
| `bw.path.bfs(start, depth, types)` | the k-hop neighborhood, with the depth of each node |
| `bw.path.shortest(a, b, types)` | the unweighted shortest path (bidirectional BFS) |
| `bw.path.dijkstra(a, b, weight, types)` | the shortest path weighted by a non-negative property |
| `bw.ppr(start, types, top)` | proximity scores by personalized PageRank (random walks with restart) |
| `bw.similar(node, via, metric, top)` | nodes of the same label ranked by shared neighbors over the `via` edge types (Jaccard, overlap, Adamic-Adar) |
| `bw.common_neighbors(a, b, types)` | the shared neighbors and their count |

Each call has a limit on visited nodes and on time; hitting one returns the
partial result marked as partial.

Global algorithms run on a named projection: `bw.project(name, labels, types,
weight)` builds a compact in-memory CSR of the visible subgraph.

- `bw.project.estimate(...)` gives the size before building, and a projection
  that does not fit the execution budget is refused. In 2 GB that is roughly
  200 to 300 million edges, so on a 5 billion element graph the global
  algorithms run on subgraphs chosen by label and edge type.
- A projection stores the hash of its creator's ACL predicate and serves only
  that same predicate, so nobody reads through someone else's projection.
- On a projection: `bw.pagerank`, `bw.wcc` and `bw.louvain`.

All of them are Cypher procedures (`CALL bw.... YIELD ...`). The platform's
governed read grammar refuses `CALL` today, so the MCP's `run_cypher` needs one
small change in external-core: accept `CALL bw.*`, which Brainwire marks as
read-only.

### 4.2 AI: `bw.ask`

- Entry points: `CALL bw.ask($node, $question) YIELD answer, evidence`,
  `brainwire ask <node> "<question>"` and `db.ask(node, question)` in the SDK.
- The LLM gets a fixed set of read-only tools: `describe_ontology` (types,
  edges, descriptions, counts), `get_node`, `neighbors`, `similar`,
  `shortest_path`, `ppr` and a governed read-only `cypher`. All of them run
  under the caller's ACL and RBAC, so the model never sees what the caller
  cannot.
- The answer comes with its evidence (node ids, paths, scores) and the trace of
  tool calls. Every id the answer cites must come from a tool result; an id that
  did not is dropped.
- Limits per question: number of tool calls, tokens and time.
- The model is reached through the LLM gateway configured in the customer's
  environment (the platform's litellm), with the credential read from the secret
  store. The model is configurable; the `claude-api` skill is loaded before the
  loop is written.
- Telemetry carries counts and latency only, never the question, the answer or
  graph content. With PII tokenization on, the model sees tokens, since
  Brainwire does not detokenize.

Example, "quais são os concorrentes?" ("who are the competitors?") on a
`Company`: `describe_ontology` shows the edge types around `Company`; the model
calls `similar(company, via=[IN_SEGMENT, SELLS_TO], metric=jaccard, top=10)`,
checks shared customers with `neighbors`, and answers with the ranking and the
paths behind each name.

### 4.3 CLI and TUI

- One binary, `brainwire`, with the subcommands `server`, `shell` (the TUI),
  `cypher` (one query), `admin` (databases, aliases, users, backup, restore,
  purge, indexes), `import` and `ask`.
- The TUI (`ratatui` + `crossterm`) is a client: it reads queries over Bolt and
  metrics and disk data over the admin HTTP API, so it works against a remote
  server.
- Screens and motion:
  - opening: the wiring forming, particles connecting on a braille canvas;
  - Dashboard: charts and sparklines refreshed every second;
  - Explore: an animated force layout around the chosen node, rings by depth,
    the Dijkstra path lit, PPR as heat;
  - Ontology: types with their `display_color`, typed links, counts, animated
    layout;
  - Cluster: nodes as boxes, the leader highlighted, lag bars, and the failover
    animated when the leader changes;
  - Query, Disk, Admin and Ask as listed in the plan.
- Terminals with an image protocol (kitty, iTerm2) also get a high-resolution
  render of the ontology and the neighborhood; the others fall back to braille.
- `--no-anim` turns motion off, and `NO_COLOR` is respected.
- User-facing CLI and TUI strings support English by default and Portuguese
  as an option. Internal code, canonical graph/schema identifiers and technical
  documentation are English; localization does not rewrite stored identifiers
  or user data (language decision updated by Allan, 2026-09-27).

### 4.4 Annotations

"See comments" in the CLI means annotations: people leave notes on a node or on
an ontology type (decided by Allan on 2026-09-25).

- Storage: a `note` column family per database, keyed by target (node id or
  type) · time · note id, holding the author's principal, the text and the
  timestamps.
- Visibility: a note is visible to whoever can see its target, like events.
  Anyone who can see the target can add a note, a `reader` included; only the
  author or an `admin` deletes it.
- Access: `CALL bw.notes(target)` to read and `CALL bw.note.add(target, text)`
  to write, the second one outside the read-only governed path.
- In the TUI, Explore and Ontology show a note count on each node or type and a
  notes panel, with a key to add one.
- Notes are free text written by people and may carry personal data: the LGPD
  purge covers them, by author and by target.

## 5. Cluster, operations and tests (decided 2026-09-25 under Allan's go-ahead, to review)

### 5.1 Cluster

- `openraft`, one Raft group per instance covering every database, with 3 or 5
  voters. Membership is static at launch; a failed node is replaced by a new one
  that receives a snapshot.
- A log entry is (database, WriteBatch, commit metadata). The leader's committer
  validates and proposes it; once a majority has persisted it, every node
  applies the same WriteBatch, and the client gets its acknowledgment.
- The snapshot is a RocksDB checkpoint of every database plus `system` and the
  last applied index. The Raft log lives in its own store on each node, truncated
  after a snapshot.
- A read on a follower with a bookmark waits until the applied index reaches it,
  or fails with `BookmarkTimeout`. Without a bookmark it may be stale, and the
  replication lag is a metric.
- ROUTE answers with the leader as writer, the followers as readers and every
  node as router, with a TTL of about 10 s. A write that reaches a follower gets
  `NotALeader`, and the driver routes again.
- Target: writes resume within 10 s of losing the leader.

### 5.2 Operations

- **Backup:** `brainwire admin backup --db <name> --to <dir or s3://...>`,
  incremental through RocksDB's BackupEngine, consistent across column families;
  in the cluster it runs on a follower. **Restore** goes into a new database,
  followed by an alias swap.
- **Purge (LGPD):** `brainwire admin purge --db <name> --entity <id>` (or
  `--connector <uuid>`, or by the author of notes): deletion, forced compaction
  and the checks from section 1.6, then a report saying the data is physically
  gone and listing the backups that still hold it and when they expire.
- **Offline import:** `brainwire import` reads the clean layer's Parquet (or CSV)
  and builds the database through SST ingestion (section 1.7), followed by an
  alias swap.
- **Metrics:** Prometheus at `/metrics`, under `external_brainwire_*`: queries,
  latency histograms, cache hit rate, bytes per element, compaction backlog, WAL
  fsync latency, Raft lag, ACL cache hits. Counts and statistics only.
- **Logs:** structured JSON, like `external_core.logging`. The slow query log
  keeps the query text and leaves the parameters out by default, since they may
  carry personal data.
- **Health:** `/health` for liveness and `/ready` for readiness (databases open,
  Raft role known).
- **Configuration:** one TOML file with environment overrides. Secrets come from
  the environment or the secret store, never from the file.
- **Container:** a distroless image running as non-root, data in `/data`, a
  service in the starter's compose, and `GRAPH_BACKEND` pointing at Brainwire
  (through `neo4j.py` until the native adapter lands).

### 5.3 Tests

- Unit and property tests per crate: codecs, and key encodings that must keep
  byte order equal to logical order.
- The ACL differential test: cases generated in Python and evaluated by
  `is_visible` become fixtures that the engine must match one by one.
- The TCK harness (section 3.2) and the contract suite
  (`external-core/tests/test_graph_repository_contract.py`) against Brainwire,
  in CI.
- Crash test: `kill -9` in a loop during writes, then invariants checked (every
  `pk` points to an existing node, `out` and `in` mirror each other, counters stay
  above the largest id).
- Cluster fault test: 3 nodes in compose, with the leader killed, the network
  partitioned and a disk filled, while a workload writes unique ids and records
  every acknowledgment. After healing, every acknowledged write must be there.
- Benchmarks: the graph-bench suite with the ACL on at 20M, 100M, 1B and 5B,
  plus ingestion rate, bytes per element and RSS under the 16 GB limit, recorded
  at every gate.
- CI: build, clippy, fmt, tests, TCK and contract suite on every change under
  `external-brainwire/`; benchmarks run on the benchmark VM.
