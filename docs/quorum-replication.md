# Static three-voter quorum replication

Brainwire has an opt-in OpenRaft group for the whole database instance. Exactly
three fixed voters each retain an independent RocksDB consensus store and all
named graphs. A majority of two can commit; one remaining voter cannot serve
quorum-authorized transactions. This is replication of the whole instance,
not graph sharding or distributed query execution.

The native operations below are a local three-process pilot. The current
container/Swarm/Kubernetes single-instance manifests do not provision this group.
Use independent persistent volumes when packaging these processes yourself.
Each private peer listener needs a fixed nonzero port matching the intended
network routing. Port `0` is rejected because an ephemeral port cannot be
reliably reached through the configured static peer endpoints.
Advertised peer addresses must be unicast destinations. Wildcard addresses
(`0.0.0.0`, `::`) are for binding listeners. Multicast IPs and the IPv4 limited
broadcast address are also rejected as peer destinations. DNS endpoints are syntax-checked; reachability and DNS
resolution still need verification in the deployment network.
Omitting `serve --cluster-config` keeps standalone mode.

## Provision once, then start

Build the binary from the repository root:

```sh
cargo build --release --locked -p brainwire-server
```

Choose fresh paths and unused ports. The following creates configuration and
private credentials beside three initially empty data roots. Existing
`BRAINWIRE_SERVER_*` and `BRAINWIRE_MEMORY_*` environment overrides take
precedence over TOML; remove conflicting overrides before this example.

```sh
umask 077
bw_demo_root=/tmp/brainwire-quorum-demo
mkdir -m 700 "$bw_demo_root"
python3 -m examples.python.create_service_tokens "$bw_demo_root/auth" --graph tickets --include-observer
openssl rand -hex 32 > "$bw_demo_root/peer-token"

for i in 1 2 3; do
  mkdir "$bw_demo_root/node$i"
  cat > "$bw_demo_root/node$i/server.toml" <<EOF_SERVER
[server]
listen = "127.0.0.1:$((17680+i))"
data_dir = "$bw_demo_root/node$i/data"
default_database = "tickets"
auth_file = "$bw_demo_root/auth/policy.json"
max_message = "8MiB"
max_decoded_message = "16MiB"
io_timeout_ms = 300000
query_timeout_ms = 10000
[memory]
cache = "64MiB"
memtables = "16MiB"
query = "64MiB"
EOF_SERVER
  cat > "$bw_demo_root/node$i/cluster.toml" <<EOF_CLUSTER
node_id = $i
cluster_id = "brainwire-quorum-demo"
listen = "127.0.0.1:$((19000+i))"
token_file = "$bw_demo_root/peer-token"
max_log_bytes = 1073741824
[[peers]]
id = 1
address = "127.0.0.1:19001"
[[peers]]
id = 2
address = "127.0.0.1:19002"
[[peers]]
id = 3
address = "127.0.0.1:19003"
EOF_CLUSTER
  ./target/release/brainwire cluster --cluster-config "$bw_demo_root/node$i/cluster.toml" \
    provision --config "$bw_demo_root/node$i/server.toml"
done
```

Provision is a one-time operation on an empty independent root. It records the
cluster ID, voter ID, fixed peer map and consensus database. Ordinary `serve`
requires this retained identity and consensus store; it never provisions a
missing voter automatically. Do not run standalone `db create` against these roots.
All voters need `server.auth_file`; use the same principal IDs and graph grants
on each replica. The peer token is separate from Bolt service credentials.
On Unix, its file must use mode `0400`, `0600`, `0440` or `0640`; for a local
file, run `chmod 600 "$bw_demo_root/peer-token"`. Group-readable modes are for
mounted secrets: anyone in that group can use this administrative peer token.
File modes do not check ownership, directory access or extended ACLs. Protect the
parent directories and secret mounts as well.

In three terminals, run the following with `i` set to 1, 2 and 3 respectively:

```sh
bw_demo_root=/tmp/brainwire-quorum-demo
i=1
./target/release/brainwire serve --config "$bw_demo_root/node$i/server.toml" \
  --cluster-config "$bw_demo_root/node$i/cluster.toml"
```

Create the graph-name-to-ontology JSON map, then initialize the group and commit
its initial schemas. The registry accepts 1–64 named graphs and at most 1 MiB of
schemas; the serialized replicated command must also fit its 2 MiB limit.

```sh
python3 - <<'PY' > /tmp/brainwire-quorum-demo/schemas.json
import json
from pathlib import Path
print(json.dumps({"tickets": json.loads(Path("examples/source_ingestion/ontology.json").read_text())}))
PY
./target/release/brainwire cluster --cluster-config "$bw_demo_root/node1/cluster.toml" \
  init --schemas "$bw_demo_root/schemas.json" --request-id initial-tickets
```

`init` initializes the exact configured voters, then proposes Bootstrap. Keep
its request ID if the reply is lost. Graph creation happens only through committed
commands. The registry and each graph's original ontology are immutable identities;
this increment has no graph drop or ontology migration operation.

## Status, graph creation and request outcomes

```sh
./target/release/brainwire cluster --cluster-config "$bw_demo_root/node1/cluster.toml" status
./target/release/brainwire cluster --cluster-config "$bw_demo_root/node2/cluster.toml" status
./target/release/brainwire cluster --cluster-config "$bw_demo_root/node3/cluster.toml" status
./target/release/brainwire cluster --cluster-config "$bw_demo_root/node1/cluster.toml" \
  create --graph research --ontology examples/source_ingestion/ontology.json --request-id create-research
./target/release/brainwire cluster --cluster-config "$bw_demo_root/node1/cluster.toml" \
  request --id initial-tickets
```

`cluster inspect` samples every configured voter directly in one command:

```sh
./target/release/brainwire cluster --cluster-config "$bw_demo_root/node1/cluster.toml" inspect
```

Its JSON includes individual statuses and per-peer `response_time_ms` elapsed RPC
attempt times (round-trip time when the peer responds), `complete`, `all_initialized`,
`retained_log_cap_agreement` and `configured_log_cap_agreement`. Unavailable or
invalid peers remain explicit entries; incomplete samples report unknown cap
agreement as `null`. Incomplete/running-false samples or differing capacities
exit nonzero after printing the diagnostic report. An uninitialized group can
produce complete samples with `all_initialized=false`. All three peer requests run
concurrently, each with its own bounded request deadline; a slow or unavailable
voter does not postpone contacting the others. Report entries remain ordered by
voter ID regardless of response order. Independent status samples do not
prove quorum or readiness for the next write; transaction barriers remain authoritative.
When all three initialized voters report committed and applied indexes,
`replication_progress` shows the sampled min/max/spread and each voter's lag
behind the highest sampled index. These requests run concurrently but are not
an atomic snapshot, so transient differences can reflect timing; missing indexes
or incomplete membership make the progress summary `null`.

For example, a healthy three-voter sample can report:

```json
{
  "replication_progress": {
    "basis": "concurrent independent samples; not an atomic snapshot or readiness proof",
    "committed_index": {"min": 512, "max": 512, "spread": 0},
    "applied_index": {"min": 512, "max": 512, "spread": 0}
  },
  "members": [
    {"node_id": 1, "committed_index_lag": 0, "applied_index_lag": 0},
    {"node_id": 2, "committed_index_lag": 0, "applied_index_lag": 0},
    {"node_id": 3, "committed_index_lag": 0, "applied_index_lag": 0}
  ]
}
```

Treat a nonzero spread as a diagnostic signal, not a routing decision: repeat
the inspection to see whether the follower catches up. Use the local readiness
probe on the intended write leader for the actual quorum read barrier; it is
the transaction barrier, not the sampled indexes, that decides whether the
leader can serve a linearizable operation.

Status is local diagnostic telemetry: leader, term, membership, last log,
committed/applied progress, retained log bytes, configured byte cap, remaining
log bytes and running state. A stopped Raft node also reports the stable
`running_issue` category `storage_error`, `panic` or `stopped`; the response never
includes the underlying fatal message or storage path. Older voters without
this optional field remain readable. Status is not a quorum health proof. Graph
creation follows the leader; accessing a new graph also requires explicit
grants in the Bolt policy on every voter and a server
restart to reload that policy. Private cluster controls use the peer secret,
not the Bolt reader token; protect access as administrative authority.

Raft-specific local probes keep followers alive without routing user traffic to them:

```sh
./target/release/brainwire cluster --cluster-config "$bw_demo_root/node1/cluster.toml" probe --kind liveness
./target/release/brainwire cluster --cluster-config "$bw_demo_root/node1/cluster.toml" probe --kind readiness
```

Liveness samples only the addressed member's identity and running state; followers
and uninitialized members can be live. Readiness additionally requires that this
member is the initialized local leader and completes a quorum read barrier. It
never follows a leader redirect, so a follower cannot be marked ready via another
member. Failure prints a redacted diagnostic JSON and exits nonzero.
Readiness is a momentary Raft-only sample, not a test of graph availability, Bolt
permissions or future write capacity. Leadership can change immediately afterward.
For a stopped node, readiness maps the same categories to `fatal_storage_error`,
`raft_panicked` or `raft_stopped` while keeping details redacted.
Allow up to 5 seconds for liveness and 10 seconds for readiness; configure an
external probe timeout accordingly. Current standalone manifests are unchanged
and are not qualified three-voter deployment profiles.

Request lookup follows the leader and uses a quorum barrier before returning a
cached reply or absence. An unavailable quorum gives an error rather than an
authoritative “not found.” An identical committed command returns its original
reply; reusing its request ID for different content returns Conflict.

## Python writes and uncertain commits

Connect directly to the leader's Bolt endpoint using the existing Python driver
and [file credentials](bolt-auth.md). There is no Neo4j routing discovery or
automatic follower forwarding. Recheck cluster status after a leader change and
reconnect to that leader. Every transaction begins after a leader quorum read
barrier; an existing transaction reads its own snapshot. Followers and isolated
leaders reject new transactions rather than serve stale follower reads.

Use explicit transaction metadata for a stable write identity. The Python driver
encodes `metadata` as Bolt `tx_metadata`. Generate the seed once per logical
operation and retain it in your application before attempting the transaction:

```sh
export BRAINWIRE_BOLT_AUTH_FILE="$bw_demo_root/auth/writer.json"
```

```python
import hashlib
import json
import uuid
from neo4j import GraphDatabase
from examples.python.connection import auth_from_env

seed = "ticket-create-" + uuid.uuid4().hex  # Persist once; reuse after an uncertain outcome.
request_key = hashlib.sha256(json.dumps(
    ["brainwire-quorum-demo", "tickets", "writer", seed],
    ensure_ascii=False, separators=(",", ":"),
).encode()).hexdigest()
print("Durable request key:", request_key)

# Replace the endpoint with the leader reported by cluster status.
with GraphDatabase.driver("bolt://127.0.0.1:17681", auth=auth_from_env(),
                          liveness_check_timeout=30) as driver:
    with driver.session(database="tickets") as session:
        with session.begin_transaction(metadata={"brainwire_request_id": seed}) as tx:
            tx.run("CREATE (t:Ticket {entity_id:$id, subject:$subject})",
                   id="synthetic-ticket-1", subject="Quorum pilot").consume()
            tx.commit()
```

The effective request key is SHA-256 of the compact UTF-8 JSON array
`[cluster_id, graph_name, authenticated_principal_id, seed]`. The example's
principal is `writer`; use your actual authenticated ID. Seeds accept 1–128 ASCII
letters/digits or the punctuation `-`, `_`, `:`, `.`; spaces are rejected. Control `init`/`create` request IDs are used directly.

After `CommitOutcomeUnknown`, inspect the effective key with
`cluster request --id YOUR_PRINTED_KEY`. `Lookup` containing `Applied` means the
write committed; a repeat BEGIN with the same key reports
`RequestAlreadyCommitted` before rerunning any Cypher. `Conflict` means that
request was rejected. A quorum-authoritative absence permits retrying the same
logical operation with the retained seed. Absence means not applied at that read
point; a pending proposal can still commit. Never blindly assign a new seed after
a lost acknowledgement: that would be a different operation. Default automatic
UUIDs do not provide an application-retained retry identity.

Replicated physical write batches are bounded below 1 MiB (256 bytes reserved
for internal metadata), and their JSON command encoding is capped at 2 MiB;
encoding can reject a batch that fits the first limit. Use bounded imports and
reduce batch size on an argument-limit error. There is one admitted writable
transaction per leader process, held until commit/rollback; close transactions
promptly. Read [connection limits](connection-limits.md) for client deadlines.

## Peer TLS, retained volumes and recovery boundaries

A voter refuses to open a store with unreadable persisted vote metadata. The
original bytes are retained; this does not repair corruption or detect every
semantically invalid or missing vote.

Accepted and outbound Raft peer sockets enable `TCP_NODELAY` to reduce delays
between small frame writes. This does not eliminate network delays or guarantee
quorum availability.

The Raft log rejects a nonzero first index and leader-identity regressions
before writing the batch, including a lower node ID in the same term. Recovery
rejects an existing log that violates this ordering rather than repairing it.
Commit markers must also match a retained log entry and cannot move backwards
or be cleared after a commit; invalid updates are rejected before persistence.
Persisted votes likewise cannot regress under the configured OpenRaft vote
ordering; repeated votes remain valid. This protects new writes and does not
repair an already damaged vote.
Before applying a batch to the graph, a voter also validates every entry's
sequence, leader order and command shape. A malformed later entry therefore
does not apply an earlier entry in that batch. An application or I/O failure
after validation can still interrupt application; recovery and the durable
request ledger remain necessary. This is not batch-wide transactional atomicity.
Snapshots and log purging are not implemented; these checks do not establish
multi-machine failover readiness or repair an already corrupt log.

Plaintext peer transport is permitted only on loopback. Non-loopback listener
or peer addresses require mutual TLS, a shared private peer token, and a CA trusted
by all voters. Add this to each cluster TOML using that voter's own certificate:

```toml
[tls]
cert = "/run/secrets/voter1-chain.pem"
key = "/run/secrets/voter1-key.pem"
ca = "/run/secrets/peer-ca.pem"
```

Certificates must support server and client authentication. Each certificate's
SAN must match its configured peer hostname/IP: receivers verify the presented
certificate against the authenticated sender's fixed peer address, and clients
verify the destination address. Use distinct stable peer names. The shared token
file accepts 32–256 ASCII token characters; keep keys and tokens private. This
secures the peer plane. Native Bolt is still plaintext; client ingress needs the
separate [protected transport](protected-transport.md) boundary.

Voters reuse authenticated peer connections for Raft replication and votes
(up to 8 idle per peer, reused within 10 s; a server keeps an authenticated
connection open 30 s for its next request). Each request is still authenticated
and, with TLS, certificate-checked. Earlier builds opened one connection per
RPC; sustained writes then exhausted local ports and broke quorum reads
([diagnosis](benchmarks/2026-10-15-quorum-write/README.md)). A voter that still
closes after one request keeps working through a single fresh-connection retry.

Mount and retain the **whole** `server.data_dir` for each voter: its identity file,
consensus RocksDB, committed graph registry, every graph RocksDB and application
ledgers. Give each voter its own volume; never share a RocksDB directory. Restart
with the same complete retained volume, cluster/node IDs and peer map. A missing
identity, consensus store, committed registry or graph/catalog fails startup.
Stopping a container or losing a process is different from losing its voter disk.

This increment has no snapshot transfer, log purge/compaction, replacement voter,
dynamic membership, readdressing or automatic rebuild from partial/lost storage.
Never re-provision a wiped voter under its old ID, copy another voter's directory,
or roll an individual disk back to an old snapshot. Restoring a temporarily
unmounted original volume is the supported restart case; full disk-loss recovery
needs a separate designed and qualified procedure.

Retained encoded log bytes default to 1 GiB per voter; `max_log_bytes` accepts
1 MiB–64 GiB. Approaching the cap requires capacity planning: exhaustion fails
closed and does not discard history. `cluster status` reports `log_bytes`,
`max_log_bytes` and `log_remaining_bytes` separately for the queried voter.
These are retained encoded entry bytes, not disk usage, RocksDB allocation or RSS;
headroom is diagnostic, not a reservation for the next transaction. Compare all
three members: their local configured caps may differ.

If the cap is exhausted, preserve every original voter volume. Stop ingestion,
inspect each member, increase `max_log_bytes` within its supported range on each
voter, and restart using the same complete volumes and identities. Keep a majority
available where possible; after recovery verify membership, committed/applied
progress and catchup before resuming ingestion. A Raft storage error can stop a
member until restart. Increasing the cap permits retaining more history; it does
not reclaim disk, implement snapshots or replace a lost voter. Never delete Raft
files to make space. Lowering the cap below retained bytes refuses startup.

Query allocation budgets are not a promise
about replication/RocksDB process RSS.

SIGTERM drains a voter's client sessions for `server.shutdown_grace_ms`, then
stops it; it does not transfer leadership first, so the remaining voters elect a
new leader as after a process loss. Stop **all three servers** before an offline
audit, then run for each voter:

```sh
for i in 1 2 3; do
  ./target/release/brainwire cluster --cluster-config "$bw_demo_root/node$i/cluster.toml" \
    audit --config "$bw_demo_root/node$i/server.toml"
done
```

Audit reports per-graph logical digests and apply revisions. Compare replicas
once acknowledged writes have caught up; an audit alone does not prove safe
replacement of a lost voter. The local synthetic qualification harness is
[`quorum_replication.py`](../crates/server/tests/quorum_replication.py), covering
majority failover, partition refusal, retained-disk restart and request outcomes.
The [October 14 distributed-operations follow-up](benchmarks/2026-10-14-distributed-ops.md)
records the parallel-inspection change and the latest local release acceptance.
The [October 15 log-cap recovery qualification](benchmarks/2026-10-15-distributed-ops.md)
adds an opt-in small-cap test that resolves an uncertain commit against the
same retained voter volumes after the configured limit is raised. It is not
evidence that the 1 GiB default has been exhausted or that multi-machine
recovery is qualified.
