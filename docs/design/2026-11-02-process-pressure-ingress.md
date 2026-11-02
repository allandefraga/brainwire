# Process-pressure admission before Bolt decoding

Status: design only. The process-pressure gate described here is not implemented.

## Problem

The engine checks process/cgroup pressure after Bolt has assembled and decoded a
request. The shared ingress quota bounds accounted input allocations, but is
independent of the process guard. A process already near its configured limit
can therefore admit more input before refusing RUN.

The first change in this session validates zero-field RESET and ROLLBACK. Their
canonical PackStream payloads are `B0 0F` and `B0 13`. Additional fields are invalid.
This is a protocol validation fix, not an ingress-pressure gate.

## Required invariants

- Keep the existing ingress quota and atomic lease accounting. A pressure callback
  is an additional cooperative filter, not a combined RSS reservation pool.
- Consult pressure before positive allocation growth during both framing and
  decoding. Zero-byte growth must remain successful so zero-field controls can
  decode without allocation under continuous pressure.
- Preserve structured refusal reasons and size hints: process pressure must not
  falsely report that a still-available ingress quota is exhausted.
- Preserve the input lease through blocking worker completion. Do not release its
  accounting while an engine can still reference the decoded request.
- Permit the first legitimate RESET/ROLLBACK under pressure, not only a second
  attempt after an input refusal.
- A recovery exception must be canonical and bounded. A proposed implementation
  may reserve a two-byte scratch allowance up front for authenticated connections
  and accept only prefixes of the two canonical payloads until the terminator.
  Do not execute a prefix. A third byte, extra chunk data, nonzero field count,
  truncated payload, or wrong tag must not obtain an exemption.
  The two-byte allowance describes only control payload storage. It does not
  include connection/session objects, socket buffers, the read scratch buffer,
  `ChunkReader::buffer`, allocator overhead, failure responses or response framing.
  Inventory these allocations and their connection-count bounds before claiming
  an upper bound on recovery memory; the payload allowance is not that bound.
- Preserve protocol state and authorization checks after recognizing a control.
  In particular RESET before authentication must not establish an authenticated
  session. PULL, DISCARD, COMMIT and RUN receive no generic control exemption.
- On refusal, discard the entire rejected message through its terminator before
  reading the next message; never repeatedly report failure without consuming
  the offending input. Connection IO deadlines continue to bound incomplete
  messages and discard work.

## Delivery slices and tests

1. Implement and test exact control framing separately from process wiring.
   Exercise the first control while a competing lease fills the quota, controls
   split across chunks/reads, forged prefixes with trailing data, and malformed
   zero-field messages. Assert no engine call for a forged control.
2. Add typed process-refusal propagation through ingress, framing and decode.
   Inject deterministic pressure changes. Test positive growth rejected before
   allocation, zero growth allowed, pressure rising during decode, held-lease
   release, and subsequent retry after pressure falls.
3. Wire the configured process guard into the listener. Test continuous pressure
   with an open transaction, RESET cleanup from TX_STREAMING and ROLLBACK cleanup
   from TX_READY, no RUN bypass via small frames, authentication boundaries, and
   independent connection recovery. Inventory allocations outside ingress growth
   (connection/session creation, receive buffers, protocol decoding stack, error
   construction, output and framing) and identify which are quota-controlled,
   connection-count bounded, or remain best-effort.

Run the existing ingress, parser, state-machine, shutdown and pressure tests
alongside the new deterministic cases. Review the full ownership/error paths
before enabling the gate. Neither this design nor its eventual implementation
establishes a hard process RSS ceiling.
