# Brainwire: launch plan

> **Approach approved on 2026-09-24: Rust + RocksDB (option C).** Launch on
> **2026-10-31**. Rhythm: 45 minutes of work per day. Nothing here becomes code
> before the spec is approved.

Goal: on 2026-10-31 (D38), Brainwire runs in a container and as a three-node
cluster, plugs into the platform through the `GraphRepository` port, holds 5
billion elements with 16 GB of RAM per node, speaks openCypher measured by the
TCK, and has a simple RBAC, client's ACL built in, an animated CLI, graph
algorithms and AI questions.

## 1. What ships at launch

Cluster/HA, RBAC and full openCypher entered the scope at Allan's request on
2026-09-24, with RBAC in its simple version. Each one has a target measured at
the weekly gate.

| Neo4j Enterprise feature | Brainwire on 2026-10-31 | Later |
|---|---|---|
| Multiple databases | `CREATE/DROP DATABASE`, alias with atomic swap | composite databases |
| ACID and WAL | one writer per database, snapshot reads | |
| Cluster and HA | 3 nodes with Raft: the leader writes, followers replicate, automatic failover, routing through Bolt (`neo4j://`) and bookmarks for causal reads | replicas in another region |
| RBAC | simple: users, three fixed roles (`reader`, `editor`, `admin`) and access per database; fine-grained visibility stays with the row ACL | privileges per label, type and property (GRANT/DENY); LDAP/SSO |
| Row-level security | client's ACL (`AclPredicate`), native to the engine (section 5) | |
| Cypher | openCypher measured by the TCK: at least 95% of the scenarios, with the list of what is missing published | 100% of the TCK |
| Online backup | incremental, with a tested restore; in the cluster, taken from a follower | object storage, PITR |
| Constraints | unique key per type; MERGE never duplicates | property existence and type |
| Metrics and queries | Prometheus, slow query log, list and kill queries | |
| Algorithms (GDS) | the subset in section 7 | more algorithms, graph ML |
| Visualization | animated CLI/TUI and website | a screen in the console |

Out of the launch, on purpose: vector index (Qdrant stays), full-text search,
an object storage tier, changefeed, LDAP/SSO and composite databases.

## 2. Approach: language and foundation

The decision: which language and which foundation get the engine ready in time,
written by agents and reviewed by one person.

Bias: models tend to recommend building from scratch and, for systems software,
Rust. That is why the two options the bias disfavors come first, argued in
earnest.

**A. Start from LadybugDB (the MIT fork of Kùzu, in C++).** Kùzu already solved
what costs months: columnar on-disk storage with CSR, a buffer manager, a WAL,
Cypher with a planner and a parallel executor, bulk loading, and a schema
declared per table, which is the same idea as the ontology. The original
repository was archived in October 2025, and LadybugDB carried on with frequent
releases. With that foundation in place, the time would go to Bolt, ACL, events,
CLI and AI.
Against: a native ACL and events require changing a C++ core we did not write;
its Cypher dialect differs from Neo4j's and would need translation in the Bolt
layer; it is embedded, with no server; and its limits become ours, which is the
reason this project exists.

**B. Our own engine in Go on top of Pebble.** Pebble is CockroachDB's storage
engine, which replaced RocksDB with it in 2020, and Dgraph is a graph database
written in Go. Go compiles in seconds, which shortens the agents'
edit-compile-test loop, and within the deadline that loop decides how much gets
done. The code is easy for one person to review, and bubbletea/lipgloss make the
nicest animated TUIs in the terminal.
Against: GC and allocation pressure in the executor weigh on p99; the type system
is weaker for ASTs and query plans; data races only show up at runtime; the
16 GB budget becomes less exact.

**C. Our own engine in Rust on top of RocksDB (approved).** With no GC, the
16 GB budget for 5 billion elements adds up to the byte. The compiler rejects
memory and concurrency errors before any test runs, which makes up for review by
a single person. Enums with exhaustive matching are the natural shape for an AST
and a query plan: a new operator that is not handled does not compile. RocksDB
(Apache 2.0) brings a WAL, compaction, compression, snapshots, checkpoints,
incremental backup, SST ingestion and range deletes, all proven in production
(TiKV uses RocksDB from Rust). In the ecosystem: `tokio` (Bolt), `arrow-flight`
and `parquet` (Python and the lake), `ratatui` (TUI), `openraft` (cluster).
Against: the first RocksDB build takes minutes; the borrow checker fights graph
structures (mitigation: IDs instead of references); and an LSM loses to FalkorDB
on hot traversal, which did 0.4 ms for one hop in doc 27. Brainwire competes on
RAM and scale; on that number it stays behind.

What reverses the decision:

- if by 2026-09-28 the compile-and-test loop takes more than about 2 minutes, or
  the agents spend more time on the compiler than delivering, switch to B while
  the code is small;
- if a spike shows LadybugDB holding 1B elements in 16 GB with its dialect
  covering the platform's corpus, A comes back to the table.

## 3. The ontology becomes the schema

The `graph_mapping.yaml` the platform already has becomes the engine's DDL.
Applying the ontology (`POST /v1/ontology/apply`) now creates types in
Brainwire.

| In the ontology | In Brainwire | Gain |
|---|---|---|
| `nodes[].label` + `entity_id` | node type with a primary key (the MERGE key) | key seek in O(log n) |
| `properties` | typed columns; the type comes from the clean layer | compact rows, no property name stored in each node |
| `er_fields`, `indexed_fields` | exact indexes | MERGE and entity resolution without scans |
| `identifier_columns` | identifier index (type + value) across types | finds a node by CPF, e-mail or license plate without knowing the label (doc 24) |
| `display_tier`, `display_color` | type metadata | leaves each node and gives the CLI its colors |
| `edges[]` (type, from, to) | edge type with an enforced signature | an edge with the wrong endpoint is refused at write time |
| `timeline[]` | per-entity event stream, ordered by time | "the last 50 tickets" becomes a range read |
| `acl_allow`, `acl_deny`, `acl_connector_ids` | one ACL set id per element (section 5) | the lists stop repeating in every node |

A property outside the ontology (a new `SET n.x`) goes to a flexible column and
shows up in a metric, so existing Cypher does not break.

`reset=true` becomes an atomic alias swap: the new graph is built in a new
database while the old one keeps serving, and the alias switches at once.

## 4. Architecture

```
brainwire (one binary)
  server   Bolt :7687 (Cypher, Neo4j drivers, cluster routing)
           Arrow Flight :7688 (Python SDK: bulk, merge, index, delete)
           HTTP :7474 (admin, /metrics, health)
  auth     users, roles and privileges in the system database; JWT from identity
  query    parser, logical plan, operators with ACL and RBAC applied in scan and expand
  graph    ontology schema, row codec, indexes, events, ACL sets
  raft     openraft: the leader replicates each transaction's WriteBatch
  algo     BFS, Dijkstra, PPR, similarity, PageRank, WCC, Louvain
  ai       ask: a loop with the customer's LLM gateway, using the algorithms
  store    one RocksDB per database: nodes, keys, out/in edges, indexes, events
  shell    TUI (ratatui): dashboard, query, explore, ontology, disk, cluster, admin, ask
```

What makes the 16 GB budget work:

- a 64-bit internal ID with bits reserved for a partition, which is the open door
  to partitioning later;
- the ACL as an interned set id, with visibility computed as a bitmap once per
  caller;
- per-type constant metadata kept out of the node, repeated strings in a
  dictionary, zstd with a trained dictionary at the bottom level of the LSM;
- RAM split into a 10 GB block cache (the memtables are charged inside it, which
  leaves about 9 GB for blocks) and about 2 GB for execution, with partitioned
  indexes and filters so they do not need to stay resident.

Compactness target, measured on 2026-10-07: at most half of Neo4j, which used 70
bytes per element in doc 08. At 5 billion that is up to about 175 GB on disk.

Recovery has three layers. The WAL recovers from a crash. The incremental backup
copies only the new files, because SSTs are immutable, and a restore is opening
the copy. In the worst case, the offline import rebuilds the graph from the
lake's clean layer. In the cluster, a new follower receives a checkpoint and then
follows the log.

## 5. ACL, RBAC and cluster

### ACL: the client model, evaluated inside the engine

Brainwire implements the `AclPredicate` rule
(`client-core/src/client_core/auth/visibility.py`, ADR-016 and ADR-017)
without changing anything in it:

- each element carries `acl_allow`, `acl_deny` and `acl_connector_ids`, stamped
  at ingestion as today;
- the caller arrives with the JWT principals (`org:`, `role:`, `user:<email>`,
  `team:`, `group:<connector>:<id>`, `user:<connector>:<id>`,
  `connector:<uuid>`), the admitted connectors (the entry axis) and the
  connectors proven open;
- the rule per object: `ENTRY and (OPEN or row ACL)`; the row ACL is "some
  principal in allow and none in deny"; with no provenance, entry is exempt but
  the row ACL is mandatory (in strict mode, invisible); `allow_all` only for the
  service principal; with no identity, nothing.

What changes is where the rule runs. Today it becomes Cypher text (`any(p IN
coalesce(n.acl_allow, []) ...)`) evaluated node by node, and FalkorDB still needs
a post-filter. In Brainwire, each distinct combination of (allow, deny,
connectors) becomes an interned set with an id, and each element stores only that
id. Per caller, the engine computes the bitmap of visible sets once and tests one
bit per node, edge or event, inside scan and expand. Three consequences:

- counts, aggregations and paths through an invisible node stop leaking, because
  the invisible node does not exist for that query;
- the cost of the ACL stops growing with the size of the lists;
- the `acl_*` names stay out of every result (IR5).

Both of today's paths keep working. `neo4j.py` keeps sending the predicate and
the reserved parameters (`$acl_principals`, `$acl_connectors`,
`$acl_open_connectors`); the engine recognizes them in a service session and
applies the native rule, and refuses those parameters from a user session.
Direct connections (CLI, BI) log in with the client-identity JWT, and Brainwire
compiles the predicate from the claims, like `compile_read_filter`.

Verification: a differential test against the Python `is_visible`, which is the
reference implementation. Cases generated in bulk (lists, entry axis, open
connectors, strict mode) must give the same result in both.

### RBAC

Simple version, decided on 2026-09-24. Users and roles live in the `system`
database. There are three fixed roles: `reader` reads, `editor` reads and
writes, `admin` does everything, including managing databases and users. Each
role applies to the databases the user has access to. Commands, in Neo4j syntax:
`CREATE USER`, `DROP USER`, `GRANT ROLE`, `REVOKE ROLE`, `SHOW USERS`, `SHOW
ROLES`, plus `ALTER USER <name> SET DATABASES [...]` for the list of databases
a user can access (all by default). The roles in the client JWT map to these three through configuration.
There are no privileges per label, type or property: the row ACL decides what each
person sees, and RBAC decides only what they can do (read, write, administer),
like the platform's role and capability policy.

### Cluster

One Raft group (openraft) per instance. Each committed transaction becomes a log
entry holding the WriteBatch the leader already built, and the followers apply
the same WriteBatch, so the state is identical by construction (TiKV's design).
The Raft snapshot is a RocksDB checkpoint. The Bolt bookmark is the log index: a
read with a bookmark waits for the follower to reach that index, which gives
causal reads without reading from the leader. Drivers using `neo4j://` receive
the routing table, with writes on the leader and reads on the followers.

Acceptance: a fault test that kills the leader, partitions the network and fills
the disk checks that no committed write was lost. That test is ready before the
cluster code.

## 6. Python

Proposed SDK API, in the style of the port:

```python
import brainwire as bw

db = bw.connect("grpc://localhost:7688", database="client_memory")
db.apply_ontology("graph_mapping.yaml")

with db.bulk():  # WAL off during the load and a flush at the end, like the port's bulk_load
    db.nodes("Customer").merge(customers, key="entity_id")
    db.edges("PURCHASED").merge(purchases, src="customer_id", dst="product_id")
    db.events("interaction").append(tickets, entity="customer_email", ts="created_at")

db.create_index("Customer", "email")
db.nodes("Customer").delete(where="n.connector_id = $c", params={"c": connector})

top = db.cypher("MATCH (c:Customer)-[:PURCHASED]->(p) RETURN p LIMIT 10").to_pandas()
db.algo.similar(node, via=["IN_SEGMENT", "SELLS_TO"], top=10)
db.ask(node, "quais são os concorrentes?")  # "who are the competitors?"
```

`customers`, `purchases` and `tickets` can be pandas, polars, pyarrow or a
DuckDB relation: anything that exposes Arrow goes over without a copy. The
batch's `acl_*` columns become ACL sets at write time. The memory-worker already
reads the clean layer through DuckDB, so the new path takes `.arrow()` straight
to `merge`, without generating Cypher row by row. A
`client_core/repository/brainwire.py` adapter implements the port on top of the
SDK; until it exists, the current `neo4j.py` talks to Brainwire over Bolt.

## 7. Algorithms and AI

Local ones, starting from a node, straight on disk: depth (k-hop, BFS), shortest
path, Dijkstra weighted by a property, PPR (proximity) and similarity (Jaccard,
overlap, Adamic-Adar) over the chosen edge types. Global ones, on an in-memory
projection bounded by the budget: PageRank, WCC and Louvain.

They are all Cypher procedures (`CALL bw.path.dijkstra(...)`) and respect the
caller's ACL and RBAC. The MCP and the memory-api reach them through
`run_cypher`, with no new API, once the platform's governed read grammar accepts
`CALL bw.*` (it refuses `CALL` today; a small change in client-core).

`bw.ask(node, question)` runs an LLM loop with these procedures as tools and
returns the answer together with the evidence (nodes, paths, scores). For "quais
são os concorrentes?" ("who are the competitors?") on a `Company`, it reads the
ontology, picks the edges that define competition (same segment, shared
customers), runs `bw.similar` and explains the ranking through the paths. When
implementing, load the `claude-api` skill before writing the loop.

Compliance: `ask` calls the LLM gateway configured in the customer's
environment, and Brainwire's telemetry carries only `client_*` counts. Tests,
benchmarks and demos use the graph-bench synthetic generator.

## 8. CLI

`brainwire shell` opens the TUI. Everything it does also exists as a command.

| Screen | What it shows |
|---|---|
| Dashboard | queries/s, p50/p95, cache, WAL and compaction, memory against the 16 GB, all live |
| Query | Cypher editor, results, plan and timing; running queries, with a kill option |
| Explore | a node's neighborhood, animated, expansion by key, the Dijkstra path lit up, PPR heat |
| Ontology | types and edges in an animated graph with the ontology's colors, count per type, indexes |
| Disk | size per database and per column family, bytes per element, LSM levels |
| Cluster | nodes, leader, replication lag, failover happening on screen |
| Admin | databases and aliases, users and roles, backup and restore, indexes, purge |
| Ask | a question about a node, with the evidence drawn on the graph |

The opening shows the wiring taking shape. In terminals with an image protocol
(kitty, iTerm2), the ontology also comes out as a real image.

## 9. Calendar

D1 = 2026-09-24 and D38 = 2026-10-31. Each week closes with a measured gate. A
failed gate stops the visual lanes and puts everyone on the engine.

### Week 1 (2026-09-24 to 2026-09-30): spec and foundation

- Spec section by section, with approval; implementation plan (skill
  `superpowers:writing-plans`).
- Dedicated branch, Rust workspace, CI and Docker image.
- Storage (databases, keys, ontology catalog, codec, IDs, transactions,
  snapshots) and Bolt (handshake, HELLO/LOGON, RUN/PULL, BEGIN/COMMIT).
- TCK harness running, even with almost everything red.
- **Gate 1 (2026-09-30):** `kill -9` in the middle of a write and the database
  reopens consistent; the sample `graph_mapping.yaml` becomes a schema; the
  Python driver connects.

### Week 2 (2026-10-01 to 2026-10-07): Cypher, ACL and the platform

- openCypher parser; executor starting from the platform's corpus (code,
  `cypher_generator`, `neo4j.py`, `run_cypher` logs).
- Reads, writes and batched `UNWIND`; native ACL with the differential test
  against `is_visible`; introspection.
- **Gate 2 (2026-10-07):** `test_graph_repository_contract.py` green through
  `neo4j.py`; the memory-worker ingests a synthetic connector end to end; first
  measurement of bytes per element; TCK measured.

### Week 3 (2026-10-08 to 2026-10-14): scale, Python, algorithms, AI

- Arrow Flight and the Python SDK; offline import; native events.
- The algorithms in section 7 and `bw.ask`.
- **Gate 3 (2026-10-14):** graph-bench at 20M with the ACL on, in the same
  table as Neo4j and FalkorDB (docs 08 and 27); 100M with the container limited
  to 16 GB; TCK at 60% or more.

### Week 4 (2026-10-15 to 2026-10-21): cluster, RBAC, operations

- Raft with 3 nodes, routing and bookmarks; RBAC; backup and restore; LGPD
  purge; metrics; TUI.
- **Gate 4 (2026-10-21):** cluster fault test with no committed write lost;
  RBAC suite green (a role without permission is refused); TCK at 85% or more.

### Week 5 (2026-10-22 to 2026-10-28): 5 billion and hardening

- Load 5B on a VM with local NVMe, container limited to 16 GB.
- Fault tests at scale, TCK up to 95%, complete TUI.
- **Gate 5 (2026-10-28):** 5B loaded with a p50/p95 table; failover at scale;
  TCK at 95% or more.

### Launch (2026-10-29 to 2026-10-31)

- Website, documentation, image, the starter's compose with `GRAPH_BACKEND`
  pointing at Brainwire, release notes.
- **Launch (2026-10-31):** end-to-end demo: a 3-node cluster in the compose,
  `brainwire shell`, "quais são os concorrentes?" on a synthetic `Company`, live
  failover on the cluster screen.

## 10. Rhythm, risks and cut order

Rhythm: 45 minutes of work per day, about 28 hours of sessions until
2026-10-31. With agents in parallel, each session closes 3 to 4 tasks. By the
estimate of 2026-09-24, that covers the single-node engine, the SDK, the ACL,
the algorithms, `ask` and the CLI, but not the cluster, the full RBAC and the
TCK at 95% within the same deadline. **Allan's decision on 2026-09-24: keep the
date and the whole scope, with a simple RBAC.** The weekly gates measure whether
it fits; if one slips, the cut order below applies. Each session starts from the
JOURNAL and ends with a new entry in it.

Main risks:

- the cluster is the riskiest part: consensus comes ready from openraft, but the
  integration, the snapshots and the routing are only proven by fault tests;
- full openCypher is large; the TCK measures progress every week, and the most
  obscure part (temporal types, durations) is left for the end;
- RBAC and ACL are two layers that must compose without opening visibility; the
  suite covers the combinations;
- Cypher semantics with silent differences (MERGE, NULL, relationship
  isomorphism); mitigation: TCK and the contract suite;
- cold latency on network disks (EBS gp3 at baseline: about 30 s for an
  expansion over 93 thousand nodes); the 5B proof uses local NVMe, and the
  website says so.

Cut order, if a gate slips: Louvain; website polish; TUI screens beyond
dashboard, query, explore and cluster; the TCK target from 95% to 90%, with the
list published; the 5B proof becomes a measured 1B plus extrapolation. Cluster,
RBAC, ACL, crash recovery and backup are not cut: if one of them does not close,
the date moves, and that decision is Allan's.

## 11. Open items

1. ~~Approve the approach.~~ Approved: Rust + RocksDB.
2. ~~"See comments" in the CLI.~~ Decided on 2026-09-25: people's annotations
   on nodes and types (spec, section 4.4).
3. Which VMs run the 5B proof and the cluster test.
4. Where the Python SDK lives: in this folder or in `client-sdk/`.
5. Brainwire's license for distribution in BYOC.
6. Who the launch is for: internal, customers or public. If public, where the
   website lives (its own page or `client-website-v2`).
7. Versioning and release flow: parked by Allan on 2026-09-24, to define later.

---

Provenance: AI-assisted draft (Claude Opus 5.5, 2026-09-24). Anti-trendslop
protocol applied. Tensions identified: build vs. start from an existing engine;
Rust vs. Go. Final decision: Allan.
