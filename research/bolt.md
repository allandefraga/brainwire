# Bolt: what the Brainwire server needs to speak

Research from 2026-09-24 for the Brainwire Bolt server (Rust). Target: the official
Neo4j Python driver, package `neo4j` 5.x (currently 5.28.6), with `bolt://`
(single instance) and `neo4j://` (cluster routing).

**How it was done.** The specification comes from https://neo4j.com/docs/bolt/current/,
read in the AsciiDoc files of the `neo4j-drivers/docs-bolt` repository, which is what
the site publishes (individual pages of the site were checked and match). The behavior
of the driver comes from the code at the `5.28.6` and `6.3.1` tags of
`neo4j/neo4j-python-driver`, read through the GitHub API, with no clone and no download.
Everything that says "the driver does X" was read in the 5.28.6 code, unless stated
otherwise. Where the docs and the code diverge, the text says so. Where something was
not found, it says "not found". Recommendations for Brainwire are marked as
**recommendation** and are my inference, not documented fact.

**Driver versions.** PyPI lists 6.3.1 as the most recent version (uploaded on
2026-09-15, requires Python 3.10+). The 5.x line is still maintained: 5.28.6 came out
on the same day. The platform pins `neo4j>=5.0,<6` in `client-core/pyproject.toml`
(measured at 5.28.6), so the real target is 5.28.x. The comment on the pin says that
6.x removed `Node.id` and `Relationship.id`. In the 6.3.1 code, `Entity.id`
still exists, deprecated, and the docstring warns that it may come back empty depending
on the server version. The exact cause of the breakage was not confirmed here. 6.x
appears in this text only where it changes something for the server.

---

## Summary

### Minimum for `bolt://` with RUN/PULL (driver 5.28.6)

1. TCP on 7687. Read `60 60 B0 17` plus 16 more bytes (four proposals). The driver
   sends `00 00 01 FF | 00 08 08 05 | 00 02 04 04 | 00 00 00 03`: manifest v1,
   range 5.8 to 5.0, range 4.4 to 4.2, and 3.0. A server without manifest ignores the
   first proposal and replies with 4 bytes holding an exact version, for example
   `00 00 04 05` for 5.4. Never offer 5.5.
2. Chunking in both directions (u16 big-endian header, end of message
   `00 00`) and complete PackStream v1 for reading (all markers) and writing.
3. HELLO returns `SUCCESS {"server": "Neo4j/<version>", "connection_id": ...}`.
   **The 5.x driver rejects any `server` that does not start with `Neo4j/`**
   (`UnsupportedServerProduct`). In 5.1+ LOGON comes right after, pipelined:
   accept `{"scheme": "basic", ...}` and also `{}` (it is what the driver sends with
   `auth=None`).
4. RUN returns `SUCCESS {"fields": [...]}` (`fields` is mandatory in practice: the
   driver builds the records with it). The driver sends PULL `{"n": 1000}` along with it;
   the server replies with RECORDs and `SUCCESS {"has_more": true}` or the final summary
   (`bookmark`, `type` in `r`/`w`/`rw`/`s`, `t_last`, `db`, `stats`). DISCARD
   `{"n": -1}` is also used.
5. BEGIN, RUN inside a transaction (with `qid`), COMMIT (`{"bookmark": ...}`) and ROLLBACK.
   They are not optional: `driver.execute_query`, which the platform adapter uses,
   opens a managed transaction with BEGIN+RUN+PULL pipelined and then COMMIT.
6. FAILURE followed by IGNORED for everything in the queue until RESET arrives.
   RESET always replies `SUCCESS {}`, aborts the open transaction and returns to
   READY. The driver sends RESET after every FAILURE and when returning a
   "dirty" connection to the pool. GOODBYE closes the socket without a reply.
7. Node, Relationship, UnboundRelationship and Path in the Bolt 5 format, with
   `element_id` (the platform adapter uses `element_id` as `.id`).
8. The right error format for the version: up to 5.6, `{"code", "message"}`; from 5.7
   onward, `neo4j_code`, `gql_status`, `description` and `message` together, or the
   driver loses the code and the message.

### Extra minimum for `neo4j://`

1. Accept `routing` in HELLO (the driver sends `{"address": "<host:port from the URI>",
   ...query string parameters}`).
2. ROUTE (tag `66`, Bolt 4.3+; 4.4+ format with `extra {db, imp_user}`),
   replying `SUCCESS {"rt": {"ttl": <seconds>, "db": <resolved database>,
   "servers": [{"addresses": [...], "role": "ROUTE"}, {..."READ"}, {..."WRITE"}]}}`.
   The driver discards the table if there is not at least one ROUTE and one READ.
   An empty WRITE is tolerated (leader election).
3. When ROUTE comes without `db`, resolve the user's home database (or the server's
   default) and return it in `rt.db`: the driver pins that name and starts
   sending `db` in BEGIN/RUN.
4. Advertised addresses that the client can reach, and `mode: "r"` honored.
5. In a cluster: a follower rejects writes with `Neo.ClientError.Cluster.NotALeader`
   (the driver removes the address from the writers and tries again); a read with a
   bookmark waits for the state to reach the bookmark or fails with
   `Neo.TransientError.Transaction.BookmarkTimeout`.

---

## 1. Handshake and version negotiation

### 1.1 Transport

- TCP, default port 7687. The docs mention WebSocket as a possible transport; the
  Python driver uses a TCP socket.
- Everything that depends on byte order is big-endian.
- There is no formal shutdown: either side can close the TCP connection at any
  time.
- TLS stays outside Bolt: with `bolt+s`/`neo4j+s` (CA certificate) or
  `bolt+ssc`/`neo4j+ssc` (accepts self-signed), the driver opens TLS and does the
  Bolt handshake inside it.

### 1.2 Identification and proposals

1. The client sends `60 60 B0 17` right after connecting. There is no reply.
2. Then it sends exactly four version proposals, each one a u32
   big-endian (16 bytes in total), in order of preference. `00 00 00 00` is
   "no version" and serves as padding.
3. The server replies with 4 bytes holding the chosen version. If none fits,
   it replies `00 00 00 00` and closes the connection.
4. If more than one proposal fits, the first one in the client's order wins.

Proposal formats:

| Format | Layout of the 4 bytes | Example |
|---|---|---|
| Bolt 1 to 3 | the integer is the version | `00 00 00 03` = 3 |
| Bolt 4.0+ | 2 reserved bytes, minor, major | `00 00 01 04` = 4.1 |
| Bolt 4.3+ (range) | 1 reserved byte, how many minors below are also valid, minor, major | `00 02 03 04` = 4.3, 4.2 and 4.1 |

- The range does not cross a major.
- The server's reply is always an exact version in the 4.0 format, with no range.
- The docs recommend that the client explicitly list the versions before
  4.3, because an old server does not understand ranges.

### 1.3 Handshake manifest v1 (new in Bolt 5.7)

- The client replaces one of the four proposals with `00 00 01 FF`: two
  reserved bytes, the manifest version byte (1) and `FF`, which marks "manifest".
- A server that accepts it replies with, in this order:
  1. `00 00 01 FF`;
  2. a VarInt `N`;
  3. `N` 4-byte versions in the 4.3 format (with range);
  4. a VarInt `CAPABILITIES`, a bitmask reserved for vendor amendments to the
     protocol.
- The client replies with the chosen version (4 bytes, 4.0 format, no range) and a
  VarInt with the subset of capabilities it accepted. The server does not reply
  to that, so the client can send the first Bolt message in the same transmission.
- VarInt: 7-bit groups, least significant group first, the high bit of
  each byte as continuation; up to u64. `01` = 1, `7F` = 127,
  `FF 82 71` = 1851775.
- Example from the docs: client `00 00 01 FF 00 00 04 04 00 00 00 03 00 00 00 02`;
  server `00 00 01 FF`, `02`, `00 02 08 05` (5.8 to 5.6), `00 04 04 04`
  (4.4 to 4.0), `09` (bits 0 and 3); client `00 00 07 05` (5.7) and `08`.
- With the manifest, the HELLO SUCCESS gains `protocol_version` (e.g. `"5.7"`). The
  5.28.6 driver does not read this field.

### 1.4 What the Python driver does in the handshake

Driver 5.28.6 (`_async/io/_bolt.py` and `_async/io/_bolt_socket.py`):

- Always sends `60 60 B0 17` and then
  `00 00 01 FF  00 08 08 05  00 02 04 04  00 00 00 03`: manifest v1, 5.8 to 5.0,
  4.4 to 4.2, 3.0.
- Versions with a registered implementation: 3.0, 4.2, 4.3, 4.4 and 5.0 to 5.8. The
  4.0 and 4.1 classes exist but do not register (`SKIP_REGISTRATION`).
- Classic reply: the driver reads only the last two bytes (minor, major) and
  ignores the first two.
- Manifest reply: reads `N`, the ranges and the capabilities; picks the **highest**
  version it knows within any offered range; sends that version and
  capabilities `00`.
- If the reply is `HTTP`, it reports a wrong port (`ServiceUnavailable`). If it is a
  manifest with a version other than 1, `BoltProtocolError`. If the agreed version
  has no implementation (including `00 00 00 00`), it closes and raises
  `BoltHandshakeError`.

Driver 6.3.1: sends the same 16 bytes. It can speak 6.0 and 6.1, but those versions
only show up via the manifest, because they are not in the classic proposals. With no
version in common, it raises `UnsupportedServerProduct`, which in 6.x became a subclass of
`ConfigurationError`.

### 1.5 Neo4j vs Bolt (table from the docs)

| Neo4j | Negotiated Bolt 5.x |
|---|---|
| 5.0 to 5.4 | 5.0 |
| 5.5 to 5.6 | 5.0 and 5.1 |
| 5.7 to 5.8 | 5.0 to 5.2 |
| 5.9 to 5.12 | 5.0 to 5.3 |
| 5.13 to 5.22 | 5.0 to 5.4 |
| 5.23 to 5.25 | 5.0 to 5.4 and 5.6 |
| 5.26 onward | 5.0 to 5.4 and 5.6 to 5.8 |
| 2025.10 onward | also Bolt 6.0 |

- **Bolt 5.5 existed in some drivers, has a flawed design and no
  Neo4j server negotiates it.** The Python driver has the 5.5 class, so a server that
  offers 5.5 may end up in it.
- Neo4j 4.4 onward speaks 4.0 to 4.4 (the earlier ones marked as removable).

### 1.6 What changed in each 5.x version

| Version | Change |
|---|---|
| 5.0 | `element_id` in Node, Relationship and UnboundRelationship; new DateTime (`49`) and DateTimeZoneId (`69`), with seconds in UTC, replace the legacy ones (`46`/`66`). Messages identical to those of 4.4. |
| 5.1 | HELLO no longer carries authentication; LOGON, LOGOFF and the AUTHENTICATION state are introduced. The CONNECTED state becomes NEGOTIATION. |
| 5.2 | HELLO, BEGIN and RUN accept `notifications_minimum_severity` and `notifications_disabled_categories`. |
| 5.3 | `bolt_agent` in HELLO. |
| 5.4 | TELEMETRY message and `telemetry.enabled` hint. |
| 5.5 | Not used. |
| 5.6 | `statuses` replaces `notifications` in the PULL/DISCARD SUCCESS; `notifications_disabled_categories` becomes `notifications_disabled_classifications`. |
| 5.7 | Handshake manifest v1; `protocol_version` in the HELLO SUCCESS; FAILURE gains `gql_status`, `description`, `diagnostic_record` and `cause`, and `code` is renamed `neo4j_code`. |
| 5.8 | `ssr.enabled` hint; `advertised_address` in the LOGON SUCCESS; the SUCCESS of BEGIN and of autocommit RUN carries the resolved home database in `db`. |
| 6.0 | Vector (`56`) and UnsupportedType (`3F`) structures; the `message` text in FAILURE no longer has a stable format. |

Finding outside the docs: in the 6.3.1 driver, the Bolt 6.1 class switches the codec to
PackStream v2, which adds a UUID type with marker `E0`. 6.1 and
PackStream v2: not found in the official docs.

### 1.7 Minimum version for the 5.x driver to connect

- Technically, 3.0 connects: 5.28.6 still offers 3.0 and 4.2 to 4.4.
- With Bolt 3 there is no `db` (the driver rejects `database=` with `ConfigurationError`)
  and routing uses the procedure `dbms.cluster.routing.getRoutingTable`.
- From 4.0 to 4.2 routing goes through the procedure
  `dbms.routing.getRoutingTable($context, $database)`; ROUTE only exists from
  4.3 onward, and `rt.db` (home database resolution) from 4.4 onward.
- Native `element_id` requires 5.0. With Bolt 4.x the 5.x driver fabricates
  `element_id = str(id)`.
- **Recommendation:** implement the 5.1 to 5.4 range first, with the classic
  handshake. That is where LOGON (provided for in the plan) and TELEMETRY come in, and
  errors stay in the simple format. Also accept 5.0 if it comes cheap (authentication
  inside HELLO). 5.6 to 5.8 and the manifest are left for when the GQL error
  format exists. Since the 6.x driver sends the same classic proposals, a
  5.x server without manifest also serves 6.x.

---

## 2. PackStream (version 1)

### 2.1 Types and markers

| Type | Markers | Note |
|---|---|---|
| Null | `C0` | |
| Boolean | `C2` false, `C3` true | |
| Integer | `00`..`7F` (0 to 127), `F0`..`FF` (-16 to -1), `C8`+i8, `C9`+i16, `CA`+i32, `CB`+i64 | always signed, 64 bits at most; the docs recommend the shortest form |
| Float | `C1` + 8 bytes IEEE 754 double | there is no 32-bit float and no unsigned integer |
| Bytes | `CC`+u8, `CD`+u16, `CE`+u32 | no short form; maximum 2^31-1 |
| String | `80`..`8F` (0 to 15 bytes), `D0`+u8, `D1`+u16, `D2`+u32 | size in UTF-8 bytes, not in characters |
| List | `90`..`9F` (0 to 15 items), `D4`+u8, `D5`+u16, `D6`+u32 | heterogeneous; size in items |
| Dictionary | `A0`..`AF` (0 to 15 pairs), `D8`+u8, `D9`+u16, `DA`+u32 | key always String; repeated key: the last one wins |
| Structure | `B0`..`BF` (0 to 15 fields) + tag byte (0 to 127) | the semantics come from the Bolt version |

- Sizes are unsigned u8, u16 or u32, but the ceiling is 2^31-1 in bytes,
  items or pairs.
- The docs say that reserved markers must not be used and that receiving them is an
  error. From the table, what is left is `C4`..`C7`, `CF`, `D3`, `D7`, `DB`..`DF` and
  `E0`..`EF` (list derived by me; the docs do not enumerate them). `E0` is the UUID of
  PackStream v2 (Bolt 6.1, only in the 6.x driver).
- Examples from the docs: 42 = `2A`; 1.23 = `C1 3F F3 AE 14 7A E1 47 AE`;
  `"A"` = `81 41`; `{}` = `A0`; `[1, 2, 3]` = `93 01 02 03`.
- Message tags and value tags are separate spaces: the top-level structure of the
  chunk is a message, the nested ones are values. TELEMETRY (`54`) and Time (`54`),
  ROUTE (`66`) and Legacy DateTimeZoneId (`66`), PULL (`3F`) and UnsupportedType
  (`3F`) have the same byte.

### 2.2 Chunking (Bolt layer, right below the messages)

- Each message goes in one or more chunks: a u16 big-endian header with the
  size (up to 65535), then the data.
- The message ends with `00 00`. A chunk never carries two messages.
- Since 4.1, an empty chunk (NOOP) serves as a keep-alive; since 4.3 it can be sent
  in any state while the server is processing.
- The 5.28.6 driver splits into 16 KiB chunks and skips NOOPs.
- Computed examples: `SUCCESS {}` = `00 03 B1 70 A0 00 00`;
  `RECORD [1]` = `00 04 B1 71 91 01 00 00`;
  `SUCCESS {"server": "Neo4j/5.26.0"}` =
  `00 17 B1 70 A1 86 73 65 72 76 65 72 8C 4E 65 6F 34 6A 2F 35 2E 32 36 2E 30 00 00`.

### 2.3 Value structures (Bolt 5.x)

| Structure | Tag | Fields | Note |
|---|---|---|---|
| Node | `4E` (`B4 4E`) | `id` Integer, `labels` List<String>, `properties` Dict, `element_id` String | 3 fields before 5.0 |
| Relationship | `52` (`B8 52`) | `id`, `startNodeId`, `endNodeId` (Integer), `type` String, `properties` Dict, `element_id`, `start_node_element_id`, `end_node_element_id` (String) | 5 fields before 5.0 |
| UnboundRelationship | `72` (`B4 72`) | `id`, `type`, `properties`, `element_id` | no endpoints; used inside Path; 3 fields before 5.0 |
| Path | `50` (`B3 50`) | `nodes` List<Node>, `rels` List<UnboundRelationship>, `indices` List<Integer> | index rule below |
| Date | `44` (`B1 44`) | `days` | days since 1970-01-01 |
| Time | `54` (`B2 54`) | `nanoseconds`, `tz_offset_seconds` | nanoseconds since local midnight |
| LocalTime | `74` (`B1 74`) | `nanoseconds` | |
| DateTime | `49` (`B3 49`) | `seconds`, `nanoseconds`, `tz_offset_seconds` | `seconds` = epoch in UTC |
| DateTimeZoneId | `69` (`B3 69`) | `seconds`, `nanoseconds`, `tz_id` | `seconds` = epoch in UTC; `tz_id` from the tz database |
| LocalDateTime | `64` (`B2 64`) | `seconds`, `nanoseconds` | local clock counted from the epoch |
| Duration | `45` (`B4 45`) | `months`, `days`, `seconds`, `nanoseconds` | can be negative |
| Point2D | `58` (`B3 58`) | `srid`, `x`, `y` (Float) | |
| Point3D | `59` (`B4 59`) | `srid`, `x`, `y`, `z` | |
| Legacy DateTime | `46` | `seconds` (UTC plus the offset), `nanoseconds`, `tz_offset_seconds` | only up to 4.4; forbidden in 5.x |
| Legacy DateTimeZoneId | `66` | `seconds` (local time), `nanoseconds`, `tz_id` | only up to 4.4 |
| Vector | `56` (`B2 56`) | `type_marker` Bytes, `data` Bytes | 6.0 |
| UnsupportedType | `3F` (`B4 3F`) | `name`, `minimum_protocol_major`, `minimum_protocol_minor`, `extra {message}` | 6.0 |

- `nanoseconds` ranges from 0 to 999,999,999.
- Example of the DateTime difference: `1970-01-01T02:15:00.000000042+01:00` is
  `seconds=4500, nanoseconds=42, tz_offset_seconds=3600` in Bolt 5. In the
  legacy structure it would be `seconds=8100`, with the offset added.
- Path: the first node of `nodes` opens the path and does not appear in `indices`.
  `indices` has an even length. The 1st, 3rd, 5th... positions point to `rels`
  starting at 1; a negative value means a relationship traversed in reverse;
  zero never appears. The 2nd, 4th... positions point to `nodes` starting at 0.

### 2.4 `element_id` in Bolt 5

- What changed: Node, Relationship and UnboundRelationship gained an `element_id`
  String, and Relationship gained `start_node_element_id` and
  `end_node_element_id`. The `id` Integer field stays in the structure.
- Semantics per the Cypher Manual: `elementId()` returns a String unique across nodes and
  relationships of all databases in the DBMS, but only within a transaction; outside
  of it there is no mapping guarantee. `id()` is deprecated and is unique only per
  type and database. Neo4j reuses the IDs of deleted elements.
- The 5.28.6 driver stores the result's nodes and relationships keyed by `element_id`.
  If it is absent (Bolt 4.x), it uses `str(id)`. The legacy `id` is also stored.
- The platform adapter (`client-core/.../repository/neo4j.py`) exposes
  `element_id` as `.id` of `_Node` and `_Relationship`, and memory-api deduplicates
  by that value. So `element_id` needs to be stable at least within a
  result.
- **Recommendation:** also send a real integer `id`, because the spec
  defines the field as Integer.

### 2.5 Parameters that the 5.28.6 driver sends

- `date` becomes Date. `time` without a time zone becomes LocalTime; with a time zone, Time.
- `datetime` without a time zone becomes LocalDateTime (`64`); with a named time zone (pytz or
  zoneinfo), DateTimeZoneId (`69`); with a fixed offset, DateTime (`49`).
- `timedelta` and `Duration` become Duration.
- Points become Point2D/Point3D with SRID 7203/9157 (Cartesian) or 4326/4979
  (WGS-84).
- `bytes` becomes Bytes; `list`/`tuple`, List; `dict`, Dictionary.

---

## 3. State machine and messages

### 3.1 General rules

- Each request gets zero or more detail messages (RECORD only) and
  exactly one summary message (SUCCESS, IGNORED or FAILURE).
- The client can send several requests in a pipeline. After a FAILURE,
  the server answers IGNORED to everything until the client acknowledges the failure with RESET.
- A message that is unexpected for the state is a protocol error, fatal: the server goes
  to DEFUNCT and closes the connection.
- Signals: `<INTERRUPT>` jumps the queue (RESET triggers it), and `<DISCONNECT>` comes from
  GOODBYE.
- **Recommendation:** always send SUCCESS with a dictionary, even an empty one
  (`B1 70 A0`). The docs show `SUCCESS` without metadata for ROLLBACK, but the message
  is defined with a Dictionary field. The driver accepts both forms.

### 3.2 States (5.1+)

| State | Meaning |
|---|---|
| NEGOTIATION | handshake completed; accepts only HELLO (was called CONNECTED up to 5.0) |
| AUTHENTICATION | after HELLO or LOGOFF; accepts only LOGON |
| READY | accepts RUN, BEGIN, ROUTE, TELEMETRY, LOGOFF, RESET, GOODBYE |
| STREAMING | autocommit with an open result |
| TX_READY | explicit transaction open, no pending result |
| TX_STREAMING | explicit transaction with one or more open results |
| FAILED | failure awaiting RESET |
| INTERRUPTED | between the arrival of `<INTERRUPT>` and the processing of RESET |
| DEFUNCT | dead connection (terminal) |

Up to 5.0, HELLO carries the authentication and goes straight from CONNECTED to READY.

### 3.3 Transitions (appendix "version 5.1+")

| State | Message | Response | New state |
|---|---|---|---|
| NEGOTIATION | HELLO | SUCCESS / FAILURE | AUTHENTICATION / DEFUNCT |
| AUTHENTICATION | LOGON | SUCCESS / FAILURE | READY / DEFUNCT |
| READY | RUN | SUCCESS / FAILURE | STREAMING / FAILED |
| READY | BEGIN | SUCCESS / FAILURE | TX_READY / FAILED |
| READY | ROUTE | SUCCESS / FAILURE | READY / FAILED |
| READY | TELEMETRY (5.4+) | SUCCESS / FAILURE | READY / FAILED |
| READY | LOGOFF | SUCCESS / FAILURE | AUTHENTICATION / FAILED (see note) |
| STREAMING | PULL or DISCARD | SUCCESS `has_more: true` | STREAMING |
| STREAMING | PULL or DISCARD | SUCCESS `has_more: false` or `{}` | READY |
| STREAMING | PULL or DISCARD | FAILURE | FAILED |
| TX_READY | RUN | SUCCESS `{qid}` / FAILURE | TX_STREAMING / FAILED |
| TX_READY | COMMIT or ROLLBACK | SUCCESS / FAILURE | READY / FAILED |
| TX_STREAMING | RUN | SUCCESS `{qid}` / FAILURE | TX_STREAMING / FAILED |
| TX_STREAMING | PULL or DISCARD | `has_more: true` | TX_STREAMING |
| TX_STREAMING | PULL or DISCARD | `has_more: false` | TX_READY, or TX_STREAMING if there is still an open result |
| FAILED | RUN, PULL, DISCARD, BEGIN, COMMIT, ROLLBACK, ROUTE, LOGOFF | IGNORED | FAILED |
| INTERRUPTED | any except RESET | IGNORED | INTERRUPTED |
| READY, STREAMING, TX_READY, TX_STREAMING, FAILED or INTERRUPTED | RESET | (`<INTERRUPT>`) | INTERRUPTED; when RESET is processed, SUCCESS and READY (or FAILURE and DEFUNCT) |
| any | GOODBYE | none | DEFUNCT |

- COMMIT is not accepted in TX_STREAMING. The driver discards the pending
  results (DISCARD) before COMMIT and ROLLBACK.
- Note: on FAILURE for LOGOFF the docs are inconsistent. The appendix table leads to
  FAILED; the message text calls for FAILURE and closing the connection, and the client
  must treat it as DEFUNCT.

### 3.4 Tags

| Message | Direction | Tag | Header | Fields | Since |
|---|---|---|---|---|---|
| HELLO | C to S | `01` | `B1 01` | `extra` Dict | 3 |
| LOGON | C to S | `6A` | `B1 6A` | `auth` Dict | 5.1 |
| LOGOFF | C to S | `6B` | `B0 6B` | none | 5.1 |
| TELEMETRY | C to S | `54` | `B1 54` | `api` Integer | 5.4 |
| GOODBYE | C to S | `02` | `B0 02` | none | 3 |
| RESET | C to S | `0F` | `B0 0F` | none | 1 |
| RUN | C to S | `10` | `B3 10` | `query` String, `parameters` Dict, `extra` Dict | 1 (`extra` since 3) |
| DISCARD | C to S | `2F` | `B1 2F` | `extra` Dict (`n`, `qid`) | fields since 4.0 |
| PULL | C to S | `3F` | `B1 3F` | `extra` Dict (`n`, `qid`) | fields since 4.0 |
| BEGIN | C to S | `11` | `B1 11` | `extra` Dict | 3 |
| COMMIT | C to S | `12` | `B0 12` | none | 3 |
| ROLLBACK | C to S | `13` | `B0 13` | none | 3 |
| ROUTE | C to S | `66` | `B3 66` | `routing` Dict, `bookmarks` List<String>, `extra` Dict | 4.3 (`extra` since 4.4) |
| SUCCESS | S to C | `70` | `B1 70` | `metadata` Dict | |
| RECORD | S to C | `71` | `B1 71` | `data` List | |
| IGNORED | S to C | `7E` | `B0 7E` | none | |
| FAILURE | S to C | `7F` | `B1 7F` | `metadata` Dict | |

INIT, ACK_FAILURE, PULL_ALL and DISCARD_ALL are from old versions and do not appear
in 5.x.

### 3.5 Message by message

Legend: "spec" is what the docs define; "driver" is what 5.28.6 sends or reads.

#### HELLO (`01`)

- **State:** only in NEGOTIATION. In any other state it is a violation and closes the connection.
- **`extra` fields (spec):**
  - `user_agent` String, in the format "Name/Version".
  - Up to 5.0, also `scheme`, `principal`, `credentials` and the other
    authentication fields.
  - `routing` Dict (4.1+). Absent or `null`: no routing. `{}`: route.
    With content: route with that context (the keys of the URI query string
    plus `address`).
  - `patch_bolt` List (4.3 and 4.4 only, patch `"utc"`).
  - `notifications_minimum_severity` String (5.2+; `"OFF"` turns everything off).
  - `notifications_disabled_categories` List (5.2 to 5.4).
  - `notifications_disabled_classifications` List (5.6+).
  - `bolt_agent` Dict (5.3+, required by the spec): `product` required;
    `platform`, `language` and `language_details` optional.
- **Driver:** always sends `user_agent`, in the format
  `neo4j-python/5.28.6 Python/<version> (<platform>)`. Sends `routing` only with
  `neo4j://`, with `address` and the URI parameters. `bolt_agent` is sent from
  5.3 on, with `product` `neo4j-python/5.28.6`. The notification fields are only sent if the
  user configures them. In 5.1+ it sends LOGON right after it, in the same write, and only then
  reads the two responses.
- **SUCCESS (spec):**
  - `server` String, e.g. `"Neo4j/5.26.0"`.
  - `connection_id` String, e.g. `"bolt-61"`.
  - `hints` Dict (4.3+).
  - `protocol_version` String (5.7+, only with manifest).
  - `patch_bolt` (4.3 and 4.4).
- **Defined hints:**
  - `connection.recv_timeout_seconds` Integer (4.3+): the driver starts using it
    as the socket read timeout. Whoever advertises it must send NOOP during a
    long query, or the driver drops the connection.
  - `telemetry.enabled` Boolean (5.4+): without it, the driver does not send TELEMETRY.
  - `ssr.enabled` Boolean (5.8+).
- **Driver reads:** `hints`; stores the rest in `server_info`; requires `server`
  starting with `Neo4j/` (section 6.5). `connection_id` is optional (it becomes
  `"<unknown id>"` in the logs).
- **FAILURE:** the server may send metadata and must close the connection
  right after. The driver kills the connection.

#### LOGON (`6A`, 5.1+)

- **State:** AUTHENTICATION.
- **Fields (spec):** `auth` Dict with `scheme` (`none`, `basic`, `bearer`,
  `kerberos`, depending on the server) and the scheme fields: `basic` uses
  `principal` and `credentials`; `bearer`, only `credentials`.
- **Driver:** sends the dictionary of the `Auth` object: `scheme`, `principal`,
  `credentials` and, if present, `realm` and `parameters`. With `auth=None` it sends
  `{}`, without `scheme`.
- **SUCCESS (spec):** `{}`; `advertised_address` (5.8+); `credentials_expired`
  Boolean (absent counts as false). Driver 5.28.6 reads neither of them.
- **FAILURE:** the server closes the connection. The driver kills the connection and raises the
  error, for example `AuthError` for `Neo.ClientError.Security.Unauthorized`.

#### LOGOFF (`6B`, 5.1+)

- **State:** only in READY. No fields.
- **Response:** SUCCESS leads to AUTHENTICATION. Outside READY, FAILURE and close
  (see note in 3.3).
- **Driver:** uses it when it needs to switch credentials on the same connection
  (per-session auth, token rotation). Sends LOGOFF and LOGON in a pipeline. With
  `bolt://` and a fixed credential, it does not appear.

#### TELEMETRY (`54`, 5.4+)

- **State:** only in READY.
- **Field:** `api` Integer: 0 managed transaction, 1 explicit, 2 implicit,
  3 `execute_query()`. The information goes to the server metrics.
- **Response:** SUCCESS and the state does not change. An invalid value or the wrong state gives
  FAILURE and FAILED.
- **Driver:** only sends it if the `telemetry.enabled` hint is true and the user has not
  turned it off. It goes in a pipeline before BEGIN or RUN. The spec says the
  server must accept TELEMETRY even if the client ignores the hint.

#### RUN (`10`)

- **State:** READY (autocommit) or TX_READY/TX_STREAMING (transaction). In FAILED
  or INTERRUPTED, IGNORED.
- **Fields:** `query` String and `parameters` Dict.
- **`extra` in autocommit** (in an explicit transaction it comes empty; the one that carries it is
  BEGIN):

  | Field | Type | Default (spec) | Since |
  |---|---|---|---|
  | `bookmarks` | List<String> | `[]` | 3 |
  | `tx_timeout` | Integer (ms) | timeout configured on the server | 3 |
  | `tx_metadata` | Dict | null | 3 |
  | `mode` | `"r"` or `"w"` | `"w"` | 3 |
  | `db` | String | null; null and `""` mean the default database configured on the server | 4.0 |
  | `imp_user` | String | null (no impersonation) | 4.4 |
  | `notifications_minimum_severity` | String | null (uses the one from HELLO) | 5.2 |
  | `notifications_disabled_classifications` | List<String> | null | 5.6 |
  | `notifications_disabled_categories` | List<String> | null | 5.2 to 5.4 |

- **Driver:** sends `mode: "r"` only for reads (absent means write). Sends `db`
  only if the database is set, `bookmarks` only if the list is not empty, and
  `tx_metadata`, `tx_timeout` (converted to ms) and `imp_user` only if
  configured. Sends PULL `{"n": <fetch_size>}` (default 1000) in the same write.
- **SUCCESS (spec):**
  - `fields` List<String>.
  - `t_first` Integer (ms until the first record).
  - `qid` Integer (4.0+, only in an explicit transaction).
  - `db` String (5.8+, only when the client did not send `db`: it is the user's
    resolved home database).
- **Driver reads:** `fields`, which is required in practice (the keys of the
  `Record`); `qid`; `db`, which reports the resolved database.

#### PULL (`3F`) and DISCARD (`2F`)

- **Fields:** `extra` with `n` Integer (required, no default; `-1` = all) and
  `qid` Integer (only in a transaction; `-1` = the last open result; default -1).
- **Detail:** PULL returns zero or more `RECORD [values]`; DISCARD returns
  nothing.
- **SUCCESS with more records:** `{"has_more": true}`. Absent counts as
  false.
- **SUCCESS at the end of the result:**

  | Field | Type | Note |
  |---|---|---|
  | `bookmark` | String | only in autocommit |
  | `db` | String | database where it ran (4.0+) |
  | `type` | String | `"r"`, `"w"`, `"rw"` or `"s"` (schema) |
  | `t_last` | Integer | ms until the last record is consumed |
  | `t_first` | Integer | also listed here by the docs |
  | `stats` | Dict | counters |
  | `plan`, `profile` | Dict | EXPLAIN/PROFILE |
  | `notifications` | List<Dict> | up to 5.4 |
  | `statuses` | List<Dict> | 5.6+, GQL status objects |

- **Driver:** sends PULL `{"n": 1000}` and repeats while it gets
  `has_more: true`. On `consume()`, sends DISCARD `{"n": -1}`. Only sends `qid`
  when the result is not the last one opened in the transaction.
- **Driver reads:** if `type` comes with a value outside `r`/`w`/`rw`/`s`, it raises
  `BoltProtocolError`. Reads `stats` through the keys `nodes-created`,
  `nodes-deleted`, `relationships-created`, `relationships-deleted`,
  `properties-set`, `labels-added`, `labels-removed`, `indexes-added`,
  `indexes-removed`, `constraints-added`, `constraints-removed`,
  `system-updates`, `contains-updates` and `contains-system-updates`. Without
  `statuses` or `notifications`, the driver synthesizes the status.
- **Transaction with several results:** from Bolt 4.0 on, the driver does not
  exhaust the previous result before the next RUN. The server then needs to
  keep several results open in the same transaction, each with its own `qid`, and
  serve PULL/DISCARD by `qid`. Without `qid` in the RUN SUCCESS, the driver uses -1
  for all of them and pulls from the wrong result.

#### BEGIN (`11`)

- **State:** READY.
- **`extra` fields:** the same as the autocommit RUN (`bookmarks`, `tx_timeout`,
  `tx_metadata`, `mode`, `db`, `imp_user` and the notification ones), with the same
  defaults.
- **SUCCESS:** `{}`; from 5.8 on, `db` when the client did not send `db`.
- **Driver:** same field policy as RUN. In `execute_query` it sends BEGIN,
  RUN and PULL in the same write (pipelined BEGIN). If BEGIN fails, the server
  must answer IGNORED to the RUN and the PULL.

#### COMMIT (`12`) and ROLLBACK (`13`)

- **State:** TX_READY. No fields.
- **SUCCESS for COMMIT:** `bookmark` String.
- **SUCCESS for ROLLBACK:** `{}`.
- **Driver:** reads `bookmark` and `db` from the COMMIT SUCCESS. Does not send ROLLBACK if the
  connection has already been reset, because RESET already undid the transaction.

#### RESET (`0F`)

- **Semantics:** jumps the queue, interrupts the work in progress, makes whatever was
  in the queue ahead of it receive IGNORED, and leaves the connection as it was right after
  a successful HELLO+LOGON. In practice, it aborts the open transaction.
- **Response:** `SUCCESS {}` and READY. FAILURE only if the connection has not reached
  READY yet, and then the server closes it.
- **Driver:** sends RESET inside the handling of every ordinary FAILURE, before
  raising the exception, and when returning to the pool a connection that is not clean.
  If RESET gets FAILURE, IGNORED or RECORD, the driver discards the connection.

#### GOODBYE (`02`)

- No fields and no response. The server closes the socket and interrupts whatever
  is running. The driver sends it when closing connections.

#### ROUTE (`66`, 4.3+)

- Details in section 4. Only after authentication and outside a transaction (READY).
  The state does not change.

#### FAILURE (`7F`) and IGNORED (`7E`)

- Format and codes in section 6. IGNORED has no fields.

### 3.6 Sequences that driver 5.28.6 generates

```
# connection (bolt://, Bolt 5.4, server without manifest)
C: 60 60 B0 17  00 00 01 FF  00 08 08 05  00 02 04 04  00 00 00 03
S: 00 00 04 05
C: HELLO {"user_agent": "neo4j-python/5.28.6 ...", "bolt_agent": {...}}
C: LOGON {"scheme": "basic", "principal": "neo4j", "credentials": "..."}
S: SUCCESS {"server": "Neo4j/5.26.0", "connection_id": "bolt-1"}
S: SUCCESS {}

# session.run (autocommit, session opened with database="neo4j")
C: RUN "MATCH (n) RETURN n" {} {"db": "neo4j"}
C: PULL {"n": 1000}
S: SUCCESS {"fields": ["n"], "t_first": 1}
S: RECORD [...]  (up to 1000)
S: SUCCESS {"has_more": true}
C: PULL {"n": 1000}
S: RECORD [...]
S: SUCCESS {"bookmark": "...", "type": "r", "t_last": 3, "db": "neo4j"}

# execute_query (managed transaction)
C: BEGIN {"db": "neo4j", "bookmarks": ["..."]}
C: RUN "CREATE (n:X) RETURN n" {} {}
C: PULL {"n": 1000}
S: SUCCESS {}
S: SUCCESS {"fields": ["n"], "qid": 0}
S: RECORD [...]
S: SUCCESS {"type": "w", "t_last": 2, "db": "neo4j"}
C: COMMIT
S: SUCCESS {"bookmark": "..."}

# failure (Bolt 5.4, error format prior to 5.7)
C: RUN "RETRUN 1" {} {}
C: PULL {"n": 1000}
S: FAILURE {"code": "Neo.ClientError.Statement.SyntaxError", "message": "..."}
S: IGNORED
C: RESET
S: SUCCESS {}

# closing
C: GOODBYE
```

With Bolt 5.4 and the `telemetry.enabled` hint, `TELEMETRY <api>` goes in before RUN
or BEGIN.

---

## 4. Routing

### 4.1 `bolt://` and `neo4j://` in the driver

- **`bolt://`, `bolt+s://`, `bolt+ssc://`:** direct driver, with a pool of a
  single address. HELLO goes without `routing` and there is no ROUTE. Parameters in the
  URI query string produce a deprecation warning and are ignored. Without
  `database=`, no `db` is sent and the server uses the default.
- **`neo4j://`, `neo4j+s://`, `neo4j+ssc://`:** routing driver. The
  routing context is the URI query string (each key only once, non-empty
  value) plus `address` with the host:port of the URI. The `address` key is
  reserved. This context goes in HELLO (`routing`) and in ROUTE.
- The driver 5 docs say that with `neo4j://` the driver gets a routing table
  even without a cluster; whoever wants a specific machine uses `bolt://`.

### 4.2 When the driver asks for the table

- There is one table per database. The first one starts out with the URI address as the only
  router.
- When acquiring a connection for a mode (read or write), the table must be
  "fresh": not expired (updated less than `ttl` seconds ago), with at least
  one router and with at least one server in the requested role (READ for reads,
  WRITE for writes). Otherwise the driver refreshes it.
- Attempt order: if the last table came without a writer (or does not exist),
  first the initial URI address, then the known routers.
  Otherwise, the known routers first and the initial address last.
  Each router is resolved through DNS and all IPs are tried.
- If no router gives a valid table, it raises `ServiceUnavailable("Unable to
  retrieve routing information")`.
- An unused table is deleted after `ttl` + 30 s.
- Session without `database=`: before the first transaction the driver sends ROUTE without
  `db`. The `rt.db` of the response becomes the session's pinned database, which from then on goes
  in `db` on BEGIN/RUN. Without `rt.db`, according to the code the table is stored under the
  null key and the transactions proceed without `db`, with no name resolution.
- Bolt 5.8: if all the connections in the pool advertise `ssr.enabled: true`, the
  driver may skip this ROUTE and use the cached home database,
  relying on the `db` in the BEGIN/RUN SUCCESS and on server-side routing.
  **Recommendation:** do not advertise `ssr.enabled` without having server-side
  routing.

### 4.3 ROUTE: request and response

- **Fields (4.4+):**
  - `routing` Dict: the context, with `address`.
  - `bookmarks` List<String>.
  - `extra` Dict: `db` (null = default database configured on the server) and
    `imp_user` (impersonated user, to resolve that user's home database).
- **In 4.3** the third field was `db` String instead of `extra`.
- **Driver:** sends the routing context (or `{}`), the session's bookmark list
  and an `extra` with `db`/`imp_user` only when set.
- **SUCCESS:**
  `{"rt": {"ttl": Integer, "db": String, "servers": [{"addresses": [String], "role": "ROUTE" | "READ" | "WRITE"}, ...]}}`
  - `ttl` in seconds.
  - `db` (4.4+) is the database the table refers to.
  - The docs say that `servers` has three entries, one per role.
- **Driver reads:** `rt.servers`, `rt.ttl` and `rt.db`.
  - Missing `rt` or `servers`/`ttl` break the parsing.
  - An address is `host:port` (default port 7687).
  - The same address can appear in more than one role.
  - A table with zero ROUTE or zero READ is discarded as a failure of that
    router. Zero WRITE is accepted, and writes wait for the next refresh.
- **Example from the docs:** ttl 1000, db `"foo"`, one ROUTE, two READ, two WRITE.

### 4.4 Server selection and reaction to failures

- **Reads** (`execute_read`, `routing_=READ`) go to a READ. **Writes**
  (the default for `session.run` and `execute_query`) go to a WRITE.
- Among the candidates, the driver picks the one with the fewest connections in use; a tie is
  broken at random.
- **Connection failure** (ServiceUnavailable or SessionExpired) deactivates the
  address: it is removed from all tables and the idle connections to it are closed.
  After refreshing the table, the driver closes connections to servers that no
  longer appear in any table.
- **`Neo.ClientError.Cluster.NotALeader` and
  `Neo.ClientError.General.ForbiddenOnReadOnlyDatabase`:** the address is removed only from the
  WRITE list of that database, and the error is retryable.
- **`Neo.TransientError.General.DatabaseUnavailable`:** deactivates the whole
  address, and the error is retryable.

### 4.5 Operation in Neo4j (reference)

- `dbms.routing_ttl` defines how long the caller keeps the table (default
  5 min, minimum 1 s).
- Normally there is one writer per database. The other servers are readers, and
  all the ones hosting the database appear as routers.
- Server-side routing (`dbms.routing.enabled`, default true)
  internally redirects a write that landed on a member that is not the leader. It only
  works with `neo4j://`.
- The address that goes in the table is the advertised one (`server.bolt.advertised_address`).

**Recommendation for Brainwire:**

- **Single instance with `neo4j://`:** its own advertised address in all three
  roles, with a `ttl` of 300 s and the user's home database in `rt.db`.
- **Raft cluster:** the leader in WRITE, the followers (and optionally the leader) in
  READ, all of them in ROUTE.
- **Follower that receives a write:** FAILURE `Neo.ClientError.Cluster.NotALeader`.

---

## 5. Bookmarks

### 5.1 What the docs say

- A bookmark is a token that represents a database state. With bookmarks in the
  request, the server does not run the transaction before those states exist.
- Within a session the chaining is automatic.
- To chain different sessions: gather the `last_bookmarks()` into a
  `Bookmarks` object and pass it when creating the new session, or share a bookmark
  manager.
- `execute_query` uses `driver.execute_query_bookmark_manager` by default, and
  consecutive queries see the previous ones.
- Cost: the query waits for the state to propagate in the cluster, so the docs
  recommend bookmarks only when causal consistency is needed.
- In Neo4j, only a server that has already processed the marked transaction runs
  the client's next transaction.

### 5.2 How the 5.28.6 driver propagates them

- **Format:** a bookmark is an opaque String. The driver never interprets it.
- **Where it goes:**
  - in `bookmarks` of BEGIN (explicit or managed transaction);
  - in the autocommit RUN;
  - in ROUTE.
  In all three cases, only if the list is not empty.
- **Where it comes from:**
  - `bookmark` of the COMMIT SUCCESS;
  - `bookmark` of the last SUCCESS (`has_more` false) of PULL/DISCARD in
    autocommit.
- **In the session:** the new bookmark replaces the previous ones, and the session
  then sends only that one. If the server does not return a bookmark, the session
  keeps what it had.
- **In the bookmark manager:** on each update it removes the bookmarks that the
  transaction used and adds the new ones. Since driver 5.3 the manager does not
  separate bookmarks by database, so a list can mix bookmarks from different
  databases.
- **In ROUTE:** a comment in the driver code explains that the bookmarks
  ensure that a newly created database already exists before asking for its table.

### 5.3 What the server must do

- **Before starting the transaction**, ensure that the local state includes everything
  each bookmark represents. If it does not get there in time, respond with
  `Neo.TransientError.Transaction.BookmarkTimeout`: it is transient, so the
  driver's managed transaction tries again.
- **Bookmark that cannot be interpreted:**
  `Neo.ClientError.Transaction.InvalidBookmark`.
- **Mixing databases:** the description of
  `Neo.ClientError.Transaction.InvalidBookmarkMixture` says that mixing
  bookmarks generated by different databases is forbidden. Since the driver's
  bookmark manager mixes them (section 5.2), official docs explaining how
  Neo4j 5 handles this mixing in practice were **not found**.
- **Response:** return a new bookmark on every COMMIT and at the end of every
  autocommit.
- **Timeout:** Neo4j has the dynamic setting
  `db.transaction.bookmark_ready_timeout`. Its description and default value were
  **not found** in the pages consulted.

**Recommendation for Brainwire (consistent with PLAN.md):**

- **Bookmark content:** identifies the database and the Raft log index
  applied at the commit.
- **Single instance:** every valid bookmark is already satisfied on arrival.
- **Follower:** waits for the applied index to reach the bookmark's, with a timeout.
- **List with several databases:** accept it and wait for each one. Refusing
  would break `execute_query` across databases.

---

## 6. Errors

### 6.1 FAILURE format

- **Bolt 1 to 5.6:** `{"code": "Neo.<Classification>.<Category>.<Title>",
  "message": "..."}`.
- **Bolt 5.7+:**
  - `neo4j_code` String (the old `code`);
  - `message` String;
  - `gql_status` String (the stable marker, preferred over `neo4j_code`);
  - `description` String (describes the `gql_status`);
  - `diagnostic_record` Dict, with `OPERATION` (default `""`), `OPERATION_CODE`
    (default `"0"`), `CURRENT_SCHEMA` (default `"/"`) and `_classification` (e.g.
    `"CLIENT_ERROR"`), omitted when it is the default, and it may have extra entries;
  - `cause` optional Dict, with the same shape and nestable.
- **Bolt 6.0:** the text of `message` is no longer stable; the stable marker is
  `gql_status`.
- **Driver classifications:** `CLIENT_ERROR`, `DATABASE_ERROR`,
  `TRANSIENT_ERROR` and `UNKNOWN`.

### 6.2 How the 5.28.6 driver reads the FAILURE

- **Up to 5.6:** reads `code` and `message`. Without `code`, it uses
  `Neo.DatabaseError.General.UnknownError`.
- **5.7 onward:**
  - reads `neo4j_code`, not `code`;
  - requires `gql_status`, `description` and `message` together, and if one is missing,
    replaces all three with generic values (status `50N42` and the message "An
    unknown error occurred");
  - without `neo4j_code`, the error becomes `Neo.DatabaseError.General.UnknownError`.
  A server that negotiates 5.7+ and sends the old format turns every error into
  an unknown DatabaseError, not retryable and without a message.
- **The class comes from the code:** the driver splits the code into four parts at
  the dots. The second part (`ClientError`, `TransientError`, `DatabaseError`)
  picks the class; a code in another format becomes DatabaseError.
- **Even in 5.7+, classification and retry come from `neo4j_code`**, not
  from `gql_status` or from `_classification`.
- **Effect on the platform:** the Neo4j adapter decides the fallback of
  `CALL { ... } IN TRANSACTIONS` by looking for markers like `syntaxerror` in the
  code and in the message. Codes and messages faithful to Neo4j's are not
  cosmetic.

### 6.3 Retry and routing

The managed transaction (`execute_read`, `execute_write`, `execute_query`) tries
again when `is_retryable()` is true, with exponential backoff, up to
`max_transaction_retry_time` (default 30 s). Autocommit (`session.run`) is never
retried.

| Code or situation | Class in the driver | Retries? | Effect on routing |
|---|---|---|---|
| `Neo.TransientError.*` (general rule) | TransientError | yes | none |
| `Neo.TransientError.General.DatabaseUnavailable` | DatabaseUnavailable | yes | deactivates the address |
| `Neo.ClientError.Cluster.NotALeader` | NotALeader (subclass of TransientError) | yes | removes the address from WRITE for that database |
| `Neo.ClientError.General.ForbiddenOnReadOnlyDatabase` | ForbiddenOnReadOnlyDatabase (subclass of TransientError) | yes | removes the address from WRITE |
| `Neo.ClientError.Security.AuthorizationExpired` | reclassified as TransientError | yes | marks all connections to that address for re-authentication |
| `Neo.TransientError.Transaction.Terminated` and `...LockClientStopped` | rewritten as `Neo.ClientError.Transaction.*` | no | none |
| `Neo.ClientError.Security.*` handled by the auth manager (`AuthManagers.basic`: `Unauthorized`; `AuthManagers.bearer`: `TokenExpired` and `Unauthorized`; a fixed credential handles nothing) | AuthError/TokenExpired | yes, if the manager handles it | none |
| other `Neo.ClientError.*` (syntax, constraint, type...) | ClientError and subclasses | no | none |
| `Neo.DatabaseError.*` | DatabaseError | no | none |
| connection drop, no server for the mode | ServiceUnavailable, SessionExpired | yes | deactivates the address |
| drop during COMMIT | IncompleteCommit | no | none |

ClientError subclasses mapped by code:

- `CypherSyntaxError`: `Statement.SyntaxError`, `Statement.InvalidSyntax`.
- `CypherTypeError`: `Statement.TypeError`, `Statement.InvalidType`,
  `Procedure.TypeError`.
- `ConstraintError`: `Schema.ConstraintValidationFailed`,
  `Schema.ConstraintViolation` and the `Statement` equivalents.
- `Forbidden`: `Security.Forbidden`, `General.ReadOnly` and others.
- `AuthError`: `Security.Unauthorized`, `Security.AuthorizationFailed`.
- `TokenExpired`: `Security.TokenExpired`.

### 6.4 Errors during discovery (ROUTE)

- **Fail immediately,** without trying another router:
  - `Neo.ClientError.Database.DatabaseNotFound`;
  - `Neo.ClientError.Transaction.InvalidBookmark`;
  - `Neo.ClientError.Transaction.InvalidBookmarkMixture`;
  - `Neo.ClientError.Statement.TypeError`;
  - `Neo.ClientError.Statement.ArgumentError`;
  - `Neo.ClientError.Request.Invalid`;
  - any `Neo.ClientError.Security.*`, except `AuthorizationExpired`.
- **Any other Neo4jError,** ServiceUnavailable or SessionExpired in ROUTE:
  the driver moves on to the next router.
- The Neo4j code list also has `Neo.ClientError.Cluster.Routing`
  (could not route the request). The 5.28.6 driver does not handle this code in
  any special way.

### 6.5 `server` that does not start with "Neo4j/"

- **Driver 5.x:**
  - At the end of `hello()`, after the HELLO and LOGON responses, the driver calls
    `check_supported_server_product`, and the test is
    `if not agent.startswith("Neo4j/")`.
  - If it fails, it raises `neo4j.exceptions.UnsupportedServerProduct(agent)` and
    kills the connection.
  - In 5.28.6 the exception inherits directly from `Exception`, so retries do not
    catch it, and `verify_connectivity`, sessions and `execute_query` all fail.
  - Without the `server` field, the driver compares the string `"None"` and fails the same way.
- **Driver 6.x:** the check was removed in PR #1208, which lifts the restrictions on the
  agent string (merged on 2025-06-12, 6.0 line). In 6.x, `UnsupportedServerProduct`
  came to mean "no Bolt version in common".
- **Market practice:** Memgraph sends by default
  `Neo4j/v5.11.0 compatible graph database server - Memgraph` (section 7).
- **Recommendation for Brainwire:** something like
  `Neo4j/5.26.0 compatible (Brainwire/0.1.0)`. The `Neo4j/` prefix is what
  matters to the 5.x driver.

### 6.6 Other failures that drop the connection

- **FAILURE on HELLO or LOGON:** the server closes and the driver kills the connection.
- **FAILURE on RESET:** the driver discards the connection.
- **Unexpected message:** protocol error and DEFUNCT.

---

## 7. Memgraph and Amazon Neptune speaking Bolt

### 7.1 Memgraph (public documentation)

- **Bolt versions:** 1 (since Memgraph 0.8.0), 4.0 and 4.1 (since 1.2.0), 4.3
  (since 2.1.0) and 5.2 (since 2.9.0).
- **Server name:** comes from the `--bolt-server-name-for-init` flag. The default is
  `Neo4j/v5.11.0 compatible graph database server - Memgraph`. The Python client
  docs say that, on Memgraph before 2.11, it must be changed to `Neo4j/5.2.0` for the
  Neo4j driver to work.
- **Routing (`neo4j://`):** only in high availability (Enterprise), talking
  to the coordinators.
  - ROUTE goes to any coordinator. The table has writers = the current MAIN,
    readers = the replicas (and optionally the MAIN), routers = the coordinators.
  - The TTL is fixed at 5 minutes, and some topology changes stay invisible
    until it expires.
  - Configuring the cluster requires `bolt://`, not `neo4j://`.
  - A write to a replica fails.
- **Multiple databases:** multi-tenancy has been Enterprise since 2.10. The docs recommend
  passing `database_="memgraph"` in queries. Impersonation is also Enterprise.
- **`element_id`:** returns the node's internal ID, which is not the ID defined by the
  user.
- **Bookmarks:** **not found** in the Memgraph docs.

### 7.2 Amazon Neptune (public documentation)

- **Scheme and endpoints:** the docs say to connect with the `bolt` scheme and do not
  mention `neo4j://`. With several instances, AWS recommends two drivers: one
  for the writer and one for the read replicas (or one per custom
  endpoint).
- **Transport:** TCP only, no WebSocket. You cannot put an Application Load
  Balancer in front. The port is the cluster's (8182 in the examples), and the examples
  use encryption (`encrypted=True`, `bolt+s`).
- **Version:** the server picks the highest suitable Bolt version among 1, 2, 3 and
  4.0. **My inference:** given the proposals of the 5.28.6 Python driver, this
  results in Bolt 3.
- **Connections:**
  - up to 1,000 simultaneous Bolt connections (512 on t3.medium and t4g.medium);
  - a connection idle for 20 minutes is closed;
  - with IAM, the connection drops a little over 10 days after it is opened.
- **Authentication:**
  - without IAM, credentials are ignored (you can use `AuthTokens.none()`);
  - with IAM, the token is a SigV4 signature placed in `credentials` of the
    `basic` scheme, with a dummy principal and realm `realm`;
  - the signature expires in about 5 minutes, which used to break drivers without
    token renewal.
- **Transactions and errors (deviations from the Bolt spec):**
  - one transaction per connection;
  - a new query is discarded if the previous result was not consumed, and to
    discard the previous one the client must send RESET;
  - an exception inside a transaction makes the server roll back the transaction and close the
    connection.
- **Bookmarks:** they appear in the list of Neo4j features missing from Neptune.
  To read your own write, AWS says to read through the cluster endpoint (replicas
  are eventually consistent, usually under 100 ms).
- **Multiple databases:** one graph per cluster.
- **IDs:** node and relationship IDs are strings, and `id()` returns a string.
- **Procedures:** there is no `CALL`. **My inference:** this rules out procedure-based
  routing in Bolt 3 and 4.x.

---

## Gaps (what was not found)

- Official documentation for Bolt 6.1 and PackStream v2. They only exist in the
  6.3.1 driver code.
- Description and default value of `db.transaction.bookmark_ready_timeout`.
- How Neo4j 5 handles bookmarks from different databases in the same list, which is
  what the driver's bookmark manager produces.
- Format of Neo4j bookmarks (the docs treat them as opaque).
- What Memgraph documents about bookmarks.
- The `server` string that Neptune announces.

---

## 8. Sources

Bolt specification (site and source):

- https://neo4j.com/docs/bolt/current/
- https://neo4j.com/docs/bolt/current/bolt/
- https://neo4j.com/docs/bolt/current/bolt/handshake/
- https://neo4j.com/docs/bolt/current/bolt/message/
- https://neo4j.com/docs/bolt/current/bolt/server-state/
- https://neo4j.com/docs/bolt/current/bolt/structure-semantics/
- https://neo4j.com/docs/bolt/current/packstream/
- https://neo4j.com/docs/bolt/current/bolt-compatibility/
- https://neo4j.com/docs/bolt/current/appendix/version-5/
- https://neo4j.com/docs/bolt/current/appendix/connection-hints/
- https://neo4j.com/docs/bolt/current/appendix/message-exchange-examples/
- https://github.com/neo4j-drivers/docs-bolt (AsciiDoc source in `modules/ROOT/pages/`)

Python driver (code read at tag 5.28.6, unless otherwise noted):

- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_async/io/_bolt.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_async/io/_bolt_socket.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_async/io/_bolt3.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_async/io/_bolt4.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_async/io/_bolt5.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_async/io/_common.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_async/io/_pool.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_routing.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/exceptions.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/api.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_async/driver.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_async/work/workspace.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_async/work/session.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_async/work/transaction.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_async/work/result.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_async/bookmark_manager.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_async/auth_management.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_auth_management.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_codec/hydration/v1/hydration_handler.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_codec/hydration/v2/hydration_handler.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_work/summary.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_conf.py
- https://github.com/neo4j/neo4j-python-driver/blob/5.28.6/src/neo4j/_meta.py
- https://github.com/neo4j/neo4j-python-driver/blob/6.3.1/src/neo4j/_async/io/_bolt.py
- https://github.com/neo4j/neo4j-python-driver/blob/6.3.1/src/neo4j/_async/io/_bolt6.py
- https://github.com/neo4j/neo4j-python-driver/blob/6.3.1/src/neo4j/_codec/packstream/v2/__init__.py
- https://github.com/neo4j/neo4j-python-driver/blob/6.x/CHANGELOG.md
- https://github.com/neo4j/neo4j-python-driver/pull/1208
- https://github.com/neo4j/neo4j-python-driver/pull/1164
- https://pypi.org/project/neo4j/

Neo4j manuals:

- https://neo4j.com/docs/python-manual/5/bookmarks/
- https://neo4j.com/docs/python-manual/5/connect-advanced/
- https://neo4j.com/docs/python-manual/current/bookmarks/
- https://neo4j.com/docs/status-codes/current/errors/all-errors/
- https://neo4j.com/docs/operations-manual/current/clustering/introduction/
- https://neo4j.com/docs/operations-manual/current/clustering/setup/routing/
- https://neo4j.com/docs/operations-manual/current/configuration/configuration-settings/
- https://neo4j.com/docs/operations-manual/current/configuration/dynamic-settings/
- https://neo4j.com/docs/cypher-manual/current/functions/scalar/#functions-elementid

Memgraph:

- https://memgraph.com/docs/client-libraries
- https://memgraph.com/docs/client-libraries/python
- https://memgraph.com/docs/database-management/configuration
- https://memgraph.com/docs/clustering/high-availability/querying-the-cluster-in-high-availability

Amazon Neptune:

- https://docs.aws.amazon.com/neptune/latest/userguide/access-graph-opencypher-bolt.html
- https://docs.aws.amazon.com/neptune/latest/userguide/migration-compatibility.html
- https://docs.aws.amazon.com/neptune/latest/userguide/feature-opencypher-compliance.html

In this repository:

- `client-core/pyproject.toml` (extra `neo4j`: `neo4j>=5.0,<6`)
- `client-core/src/client_core/repository/neo4j.py` (use of `element_id`,
  `execute_query` and the error code)
- `client-brainwire/PLAN.md` (bookmark = Raft log index)
