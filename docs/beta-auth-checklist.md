# Protected Bolt path: implementation and remaining acceptance

Updated October 12, 2026. Optional service-token authentication and named-graph
read/write authorization are implemented. The original October 10 checklist
below remains the acceptance contract, rather than a claim of full tenant security.

## Current boundary

`server.auth_file` selects a fail-closed policy. Without it, development mode
still uses `AuthMode::AllowAll` and warns at startup. A protected session binds
its principal to exact graph grants before resolving the default or explicit
store. Cypher authorizes the whole parsed statement before execution; process-wide
`db.memory` additionally requires `instance_admin`.

Python, the connected TUI and native probes can load one credentials file through
`BRAINWIRE_BOLT_AUTH_FILE`. Credentials are not command-line arguments. Local
filesystem administration remains a separate authority and does not acquire Bolt
permissions merely because network authentication is configured.

The protected Compose profile passed a real Linux container test including an
authenticated health check, denied requests and retained-volume SIGKILL recovery.
Swarm and Kubernetes protected profiles passed local rendering checks only; their
cluster behavior remains unverified in this session. All profiles remain single
instance. There is no native TLS or replication. Shared use requires trusted
transport or a separately configured encrypted tunnel; NetworkPolicy and secret
mounts do not encrypt Bolt traffic.

See [policy and connection guide](bolt-auth.md), [container profiles](protected-containers.md)
and [October 12 receipts](benchmarks/2026-10-12-auth.md).

## Implemented October 12

- Bounded, strict policy loading before opening stores/listening; invalid explicit
  policy fails closed. High-entropy tokens use SHA-256 verifiers; diagnostics redact
  credentials. This is not a password, JWT or general identity-provider system.
- Per-connection identity, exact named-graph read/write grants, instance telemetry
  permission, whole-AST Cypher authorization and explicit procedure allowlisting.
- LOGOFF/re-auth cleanup, transaction scope pinning, unsupported impersonation
  rejection and RESET rules; tests cover the lifecycle and engine boundary.
- Real neo4j Python drivers 5.28.6 and 6.3.0, shared native client credentials,
  connected ontology fetch, restart revocation and offline integrity audit.
- Optional Compose/Swarm/Kubernetes secret overlays with documented ownership,
  rotation/restart, health credentials and retained-storage boundaries.

## Remaining release evidence

- Validate the intended encrypted transport configuration and its failure behavior.
- Execute protected Swarm and Kubernetes profiles in separately authorized test
  clusters, including secret rotation, placement, retained storage and probes.
- Review actual external application principal/graph policy and workload with the operator;
  synthetic fixtures do not approve access to production data.
- Extend sustained mixed-load/recovery measurements to the proposed supported
  resource profile and data shape. A short local run is not certification.
- Native TLS, online policy reload, per-record ACLs and replicated failover remain
  separate future work. Restart is currently required for policy changes.

## Original implementation checklist (now implemented)

1. Consult Astra on the authentication state machine and config boundary before
   implementation. Extend the existing session/server policy; do not create a
   second query or storage engine.
2. Load policy through `crates/server/src/config.rs` and `serve.rs`, replacing the
   hardcoded AllowAll session construction. Never expose credentials through
   derived Debug, `config show`, source diagnostics or error messages.
3. Bind an authenticated principal to each session. Check the resolved graph
   before `engine.begin` for both implicit RUN and explicit BEGIN. An omitted
   database selects the configured default and still needs authorization.
4. Classify operations in `crates/cypher/src/exec.rs` before mutations, carrying
   scope through `crates/server/src/engine.rs`. Checking only RUN or graph name
   cannot enforce read-only access. Authorize CALL explicitly; `db.memory`
   currently exposes process-wide telemetry even when selected through one graph.
5. Define RESET/LOGOFF lifecycle and transport protection before shared use.
   The current parsed LOGOFF has no explicit session state-machine handler.

## Required denial and lifecycle tests

- Missing/invalid credentials, HELLO-only queries and LOGOFF then RUN fail;
  RESET cannot bypass failed authentication.
- A graph-A principal cannot select B through RUN, BEGIN or a default database
  outside its scope. BEGIN A then RUN B fails and aborts the transaction.
- An explicit transaction remains pinned to its authorized graph when later RUN
  omits `db`; RESET releases the transaction without losing valid identity/scope.
- Read-only scope rejects CREATE/SET/DELETE, including UNWIND batches, before
  graph/index/checkpoint mutation. Include node/relationship MERGE and DETACH
  DELETE, plus a query combining an allowed read with a prohibited mutation or
  procedure; reject before execution or ID allocation. Unknown and future admin
  procedures cannot inherit unrestricted CALL permission.
- LOGOFF/re-auth during Streaming and TxStreaming cannot disclose queued rows
  through PULL or implicitly commit old writes through DISCARD. COMMIT, RESET
  and failed LOGON cannot recover a revoked principal's scope.
- Invalid/empty configured auth fails startup closed rather than falling back
  to AllowAll. Reject malformed/unsupported credential schemes and unsupported
  impersonation metadata.
- Denied requests disclose no data or commit bookmark and release resources.
  Verify both supported driver versions: the current Bolt 5.1+ state machine
  expects HELLO followed by LOGON; driver major version does not define that
  protocol lifecycle distinction.
- Config/error diagnostics redact secrets. Document encrypted transport and the
  separate local filesystem/admin authority; do not imply container scheduling
  provides tenant security.

Primary code locations: Bolt session/message/server, server config/serve/engine,
Cypher executor/procedures and their existing tests. The optional protected path
now satisfies the local token/graph authorization slice. Encrypted transport and target-cluster acceptance remain October blockers
for shared deployment; synthetic ingestion alone does not satisfy them.
