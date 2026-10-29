# Bounded Bolt response encoding

Status: candidate implementation; not a process RSS or production guarantee.

## Problem

The Bolt server currently serializes each response into a full PackStream `Vec`
before chunking and writing it. A large record can therefore create an
additional response-sized allocation outside query reservations. Concurrent
connections multiply this cost, and a slow socket prolongs it.

## Alternatives

1. Keep the vector and charge an estimated encoded size before allocating it.
   This controls admitted bytes only where estimates and all allocations match;
   a large redundant copy remains.
2. Encode incrementally into bounded Bolt frames and wait for the socket before
   producing the next frame. This removes the response-sized encoding vector;
   query values themselves remain materialized and charged separately.

## Decision and boundaries

Prefer incremental response framing if it preserves PackStream byte order,
Bolt framing and error/timeout handling. Retain the existing result reservation
until the write finishes or is cancelled. Do not claim executor streaming,
RSS enforcement, rejection-before-query, bounded kernel socket memory, or
bounded RocksDB/native allocations from this change.

## Evidence required

Compare encoded messages with the existing encoder across scalar values,
length boundaries, nested lists/maps/structures, large strings and bytes.
Exercise partial writes, write errors and cancellation. Bound scratch space
independently of payload size. Test the server integration and existing Bolt
client acceptance, then measure latency and memory on a reproducible workload
without concurrent builds before making a performance claim. Investigate any
measured regression greater than 5%; no comparison means no performance claim.
