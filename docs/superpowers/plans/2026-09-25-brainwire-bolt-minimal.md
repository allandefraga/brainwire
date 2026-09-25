# Brainwire minimal Bolt server Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Bolt server that the official Neo4j Python driver 5.x connects to
over `bolt://`, authenticates against, and runs `RETURN 1 AS x` through, both
in auto-commit and in a managed transaction. That closes Gate 1 together with
the storage plan.

**Architecture:** A new crate, `brainwire-bolt`, split into pure parts with no
I/O (chunk framing, PackStream, handshake, message parsing, the session state
machine) and one async part (the TCP server on `tokio`). Query execution sits
behind a `QueryEngine` trait; this plan ships a stub engine that answers
`RETURN <int> [AS <name>]`, and the Cypher plan replaces it.

**Tech Stack:** Rust (stable), `tokio`, `thiserror`; Python 3 with the `neo4j`
driver 5.x or 6.x for the smoke test (checked with 6.3.0).

**Spec:** [2026-09-24-brainwire-design.md](../specs/2026-09-24-brainwire-design.md),
section 3.3, and the protocol notes in
[research/bolt.md](../../../research/bolt.md).

**Depends on:** the storage foundation plan's Task 0 and Task 1 (worktree and
workspace).

## Global Constraints

- Offer Bolt 5.1 to 5.4, never 5.5, without the 5.7 manifest (spec 3.3).
- The HELLO `SUCCESS` carries `server` starting with `Neo4j/`, which driver 5.x
  requires (spec 3.3).
- Classic error format `{code, message}` with Neo4j codes (spec 3.3).
- After a `FAILURE`, answer `IGNORED` until `RESET` (spec 3.3).
- Code, identifiers, comments, commit messages and docs in English; no em
  dashes or en dashes in docs.
- Commits go to `feat/brainwire` only.

## File structure

```
client-brainwire/crates/bolt/        package brainwire-bolt
  Cargo.toml
  src/lib.rs                           module list
  src/chunk.rs                         chunk framing (2-byte sizes, 00 00 end)
  src/packstream.rs                    PackStream v1 values
  src/handshake.rs                     version negotiation
  src/message.rs                       requests and responses
  src/session.rs                       state machine + QueryEngine trait + stub engine
  src/server.rs                        tokio TCP server
  src/bin/brainwire-bolt-dev.rs        dev binary: serve on a port with the stub engine
  tests/driver_smoke.py                the Python driver check
```

## Task 1: Crate and chunk framing

**Files:**
- Create: `crates/bolt/Cargo.toml`, `crates/bolt/src/lib.rs`, `crates/bolt/src/chunk.rs`
- Modify: `client-brainwire/Cargo.toml` (add `"crates/bolt"` to `members` and
  `tokio = { version = "1", features = ["full"] }` to `[workspace.dependencies]`)

**Interfaces:**
- Produces:
  - `pub fn encode_message(payload: &[u8]) -> Vec<u8>` (chunks of at most
    65,535 bytes, then `00 00`)
  - `pub struct ChunkReader` with `ChunkReader::default()`,
    `fn push(&mut self, bytes: &[u8])` and
    `fn next_message(&mut self) -> Option<Vec<u8>>`

- [ ] **Step 1: Write the crate manifest**

`crates/bolt/Cargo.toml`:

```toml
[package]
name = "brainwire-bolt"
edition.workspace = true
version.workspace = true
license.workspace = true
publish.workspace = true

[dependencies]
thiserror.workspace = true
tokio.workspace = true
```

`crates/bolt/src/lib.rs`:

```rust
//! Bolt protocol server for Brainwire (spec 3.3).

pub mod chunk;
```

- [ ] **Step 2: Write the failing tests**

At the bottom of `crates/bolt/src/chunk.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_message_is_one_chunk_and_a_terminator() {
        assert_eq!(encode_message(&[1, 2, 3]), vec![0, 3, 1, 2, 3, 0, 0]);
    }

    #[test]
    fn big_message_is_split_at_65535() {
        let payload = vec![7u8; 70_000];
        let framed = encode_message(&payload);
        assert_eq!(&framed[..2], &[0xFF, 0xFF]);
        let mut reader = ChunkReader::default();
        reader.push(&framed);
        assert_eq!(reader.next_message().unwrap(), payload);
    }

    #[test]
    fn reader_waits_for_the_terminator_and_skips_noop_chunks() {
        let mut reader = ChunkReader::default();
        reader.push(&[0, 0]); // a NOOP chunk between messages
        reader.push(&[0, 2, 9]);
        assert_eq!(reader.next_message(), None);
        reader.push(&[8, 0, 0]);
        assert_eq!(reader.next_message(), Some(vec![9, 8]));
        assert_eq!(reader.next_message(), None);
    }
}
```

- [ ] **Step 3: Run the tests to see them fail**

Run: `cargo test -p brainwire-bolt chunk`
Expected: compile errors, `encode_message` is not defined.

- [ ] **Step 4: Implement**

Top of `crates/bolt/src/chunk.rs`:

```rust
//! Bolt chunk framing: each chunk is a 2-byte big-endian size and its data; a
//! message ends with a zero-size chunk. A zero-size chunk with no data before it
//! is a NOOP (keep-alive) and is skipped.

const MAX_CHUNK: usize = 0xFFFF;

pub fn encode_message(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 4 + 2 * (payload.len() / MAX_CHUNK));
    for chunk in payload.chunks(MAX_CHUNK) {
        out.extend_from_slice(&(chunk.len() as u16).to_be_bytes());
        out.extend_from_slice(chunk);
    }
    out.extend_from_slice(&[0, 0]);
    out
}

#[derive(Default)]
pub struct ChunkReader {
    buffer: Vec<u8>,
    current: Vec<u8>,
}

impl ChunkReader {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    pub fn next_message(&mut self) -> Option<Vec<u8>> {
        loop {
            if self.buffer.len() < 2 {
                return None;
            }
            let size = u16::from_be_bytes([self.buffer[0], self.buffer[1]]) as usize;
            if size == 0 {
                self.buffer.drain(..2);
                if self.current.is_empty() {
                    continue; // NOOP
                }
                return Some(std::mem::take(&mut self.current));
            }
            if self.buffer.len() < 2 + size {
                return None;
            }
            self.current.extend_from_slice(&self.buffer[2..2 + size]);
            self.buffer.drain(..2 + size);
        }
    }
}
```

- [ ] **Step 5: Run the tests to see them pass**

Run: `cargo test -p brainwire-bolt chunk`
Expected: the three `chunk::tests` pass.

- [ ] **Step 6: Commit**

```bash
git add client-brainwire/Cargo.toml client-brainwire/crates/bolt
git commit -m "feat(brainwire): add the bolt crate with chunk framing"
```

## Task 2: PackStream v1

**Files:**
- Create: `crates/bolt/src/packstream.rs`
- Modify: `crates/bolt/src/lib.rs` (add `pub mod packstream;`)

**Interfaces:**
- Produces:
  - `pub enum PackValue { Null, Bool(bool), Int(i64), Float(f64), Bytes(Vec<u8>), String(String), List(Vec<PackValue>), Map(Vec<(String, PackValue)>), Struct { tag: u8, fields: Vec<PackValue> } }`
    (derives `Debug, Clone, PartialEq`; `Map` keeps insertion order)
  - `PackValue::get(&self, key: &str) -> Option<&PackValue>` for maps
  - `pub fn pack(value: &PackValue, out: &mut Vec<u8>)`
  - `pub fn unpack(input: &mut &[u8]) -> Result<PackValue, PackError>`
  - `pub enum PackError { Truncated, UnknownMarker(u8), BadUtf8, BadMapKey }`

Markers: `C0` null, `C2`/`C3` false/true, `C1` float64 (8 bytes big-endian);
integers as tiny (-16..=127, one byte), `C8` i8, `C9` i16, `CA` i32, `CB` i64,
always the smallest form; strings `80..8F` (length 0 to 15), `D0` u8 length,
`D1` u16, `D2` u32; lists `90..9F`, `D4`, `D5`, `D6`; maps `A0..AF`, `D8`, `D9`,
`DA`; structs `B0..BF` (0 to 15 fields) followed by the tag byte; bytes `CC`,
`CD`, `CE`.

- [ ] **Step 1: Write the failing tests**

At the bottom of `crates/bolt/src/packstream.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn bytes_of(v: &PackValue) -> Vec<u8> {
        let mut out = Vec::new();
        pack(v, &mut out);
        out
    }

    fn round_trip(v: PackValue) {
        let bytes = bytes_of(&v);
        let mut input = bytes.as_slice();
        assert_eq!(unpack(&mut input).unwrap(), v);
        assert!(input.is_empty());
    }

    #[test]
    fn known_encodings() {
        assert_eq!(bytes_of(&PackValue::Null), vec![0xC0]);
        assert_eq!(bytes_of(&PackValue::Bool(true)), vec![0xC3]);
        assert_eq!(bytes_of(&PackValue::Int(1)), vec![0x01]);
        assert_eq!(bytes_of(&PackValue::Int(-16)), vec![0xF0]);
        assert_eq!(bytes_of(&PackValue::Int(-17)), vec![0xC8, 0xEF]);
        assert_eq!(bytes_of(&PackValue::Int(128)), vec![0xC9, 0x00, 0x80]);
        assert_eq!(bytes_of(&PackValue::String("a".into())), vec![0x81, b'a']);
        assert_eq!(
            bytes_of(&PackValue::Struct { tag: 0x70, fields: vec![PackValue::Map(vec![])] }),
            vec![0xB1, 0x70, 0xA0]
        );
    }

    #[test]
    fn round_trips_every_kind_and_size() {
        for i in [0, 127, -16, -17, -128, 32_767, -32_768, 2_147_483_647, i64::MAX, i64::MIN] {
            round_trip(PackValue::Int(i));
        }
        round_trip(PackValue::Float(1.5));
        round_trip(PackValue::Bytes(vec![1, 2, 3]));
        round_trip(PackValue::String("x".repeat(16)));
        round_trip(PackValue::String("x".repeat(300)));
        round_trip(PackValue::String("x".repeat(70_000)));
        round_trip(PackValue::List((0..20).map(PackValue::Int).collect()));
        round_trip(PackValue::Map((0..20).map(|i| (format!("k{i}"), PackValue::Int(i))).collect()));
        round_trip(PackValue::Struct { tag: 0x4E, fields: vec![PackValue::Int(1), PackValue::List(vec![])] });
    }

    #[test]
    fn truncated_and_unknown() {
        let mut input: &[u8] = &[0x81];
        assert_eq!(unpack(&mut input), Err(PackError::Truncated));
        let mut input: &[u8] = &[0xE0];
        assert_eq!(unpack(&mut input), Err(PackError::UnknownMarker(0xE0)));
    }

    #[test]
    fn map_get() {
        let m = PackValue::Map(vec![("a".into(), PackValue::Int(1))]);
        assert_eq!(m.get("a"), Some(&PackValue::Int(1)));
        assert_eq!(m.get("b"), None);
    }
}
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test -p brainwire-bolt packstream`
Expected: compile errors, `PackValue` is not defined.

- [ ] **Step 3: Implement**

Top of `crates/bolt/src/packstream.rs`:

```rust
//! PackStream v1, the value encoding of Bolt.

#[derive(Debug, Clone, PartialEq)]
pub enum PackValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Bytes(Vec<u8>),
    String(String),
    List(Vec<PackValue>),
    Map(Vec<(String, PackValue)>),
    Struct { tag: u8, fields: Vec<PackValue> },
}

impl PackValue {
    pub fn get(&self, key: &str) -> Option<&PackValue> {
        match self {
            PackValue::Map(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PackError {
    #[error("value is truncated")]
    Truncated,
    #[error("unknown marker {0:#04x}")]
    UnknownMarker(u8),
    #[error("invalid UTF-8 in a string")]
    BadUtf8,
    #[error("map key is not a string")]
    BadMapKey,
}

fn pack_size(out: &mut Vec<u8>, len: usize, tiny: u8, m8: Option<u8>, m16: u8, m32: u8) {
    if len < 16 {
        out.push(tiny | len as u8);
    } else if let (Some(m8), true) = (m8, len <= u8::MAX as usize) {
        out.push(m8);
        out.push(len as u8);
    } else if len <= u16::MAX as usize {
        out.push(m16);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(m32);
        out.extend_from_slice(&(len as u32).to_be_bytes());
    }
}

pub fn pack(value: &PackValue, out: &mut Vec<u8>) {
    match value {
        PackValue::Null => out.push(0xC0),
        PackValue::Bool(false) => out.push(0xC2),
        PackValue::Bool(true) => out.push(0xC3),
        PackValue::Int(i) => {
            let i = *i;
            if (-16..=127).contains(&i) {
                out.push(i as i8 as u8);
            } else if (i8::MIN as i64..=i8::MAX as i64).contains(&i) {
                out.push(0xC8);
                out.push(i as i8 as u8);
            } else if (i16::MIN as i64..=i16::MAX as i64).contains(&i) {
                out.push(0xC9);
                out.extend_from_slice(&(i as i16).to_be_bytes());
            } else if (i32::MIN as i64..=i32::MAX as i64).contains(&i) {
                out.push(0xCA);
                out.extend_from_slice(&(i as i32).to_be_bytes());
            } else {
                out.push(0xCB);
                out.extend_from_slice(&i.to_be_bytes());
            }
        }
        PackValue::Float(f) => {
            out.push(0xC1);
            out.extend_from_slice(&f.to_be_bytes());
        }
        PackValue::Bytes(b) => {
            // Bytes have no tiny form.
            if b.len() <= u8::MAX as usize {
                out.push(0xCC);
                out.push(b.len() as u8);
            } else if b.len() <= u16::MAX as usize {
                out.push(0xCD);
                out.extend_from_slice(&(b.len() as u16).to_be_bytes());
            } else {
                out.push(0xCE);
                out.extend_from_slice(&(b.len() as u32).to_be_bytes());
            }
            out.extend_from_slice(b);
        }
        PackValue::String(s) => {
            pack_size(out, s.len(), 0x80, Some(0xD0), 0xD1, 0xD2);
            out.extend_from_slice(s.as_bytes());
        }
        PackValue::List(items) => {
            pack_size(out, items.len(), 0x90, Some(0xD4), 0xD5, 0xD6);
            for item in items {
                pack(item, out);
            }
        }
        PackValue::Map(entries) => {
            pack_size(out, entries.len(), 0xA0, Some(0xD8), 0xD9, 0xDA);
            for (k, v) in entries {
                pack(&PackValue::String(k.clone()), out);
                pack(v, out);
            }
        }
        PackValue::Struct { tag, fields } => {
            out.push(0xB0 | fields.len() as u8);
            out.push(*tag);
            for f in fields {
                pack(f, out);
            }
        }
    }
}

fn take<'a>(input: &mut &'a [u8], n: usize) -> Result<&'a [u8], PackError> {
    if input.len() < n {
        return Err(PackError::Truncated);
    }
    let (head, rest) = input.split_at(n);
    *input = rest;
    Ok(head)
}

fn be_len(input: &mut &[u8], width: usize) -> Result<usize, PackError> {
    let raw = take(input, width)?;
    Ok(raw.iter().fold(0usize, |acc, b| (acc << 8) | *b as usize))
}

fn string_of(input: &mut &[u8], len: usize) -> Result<String, PackError> {
    String::from_utf8(take(input, len)?.to_vec()).map_err(|_| PackError::BadUtf8)
}

fn list_of(input: &mut &[u8], len: usize) -> Result<PackValue, PackError> {
    let mut items = Vec::with_capacity(len.min(1024));
    for _ in 0..len {
        items.push(unpack(input)?);
    }
    Ok(PackValue::List(items))
}

fn map_of(input: &mut &[u8], len: usize) -> Result<PackValue, PackError> {
    let mut entries = Vec::with_capacity(len.min(1024));
    for _ in 0..len {
        let key = match unpack(input)? {
            PackValue::String(s) => s,
            _ => return Err(PackError::BadMapKey),
        };
        entries.push((key, unpack(input)?));
    }
    Ok(PackValue::Map(entries))
}

pub fn unpack(input: &mut &[u8]) -> Result<PackValue, PackError> {
    let marker = take(input, 1)?[0];
    Ok(match marker {
        0x00..=0x7F => PackValue::Int(marker as i64),
        0xF0..=0xFF => PackValue::Int(marker as i8 as i64),
        0x80..=0x8F => PackValue::String(string_of(input, (marker & 0x0F) as usize)?),
        0x90..=0x9F => list_of(input, (marker & 0x0F) as usize)?,
        0xA0..=0xAF => map_of(input, (marker & 0x0F) as usize)?,
        0xB0..=0xBF => {
            let tag = take(input, 1)?[0];
            let mut fields = Vec::new();
            for _ in 0..(marker & 0x0F) {
                fields.push(unpack(input)?);
            }
            PackValue::Struct { tag, fields }
        }
        0xC0 => PackValue::Null,
        0xC1 => PackValue::Float(f64::from_be_bytes(take(input, 8)?.try_into().expect("8 bytes"))),
        0xC2 => PackValue::Bool(false),
        0xC3 => PackValue::Bool(true),
        0xC8 => PackValue::Int(take(input, 1)?[0] as i8 as i64),
        0xC9 => PackValue::Int(i16::from_be_bytes(take(input, 2)?.try_into().expect("2 bytes")) as i64),
        0xCA => PackValue::Int(i32::from_be_bytes(take(input, 4)?.try_into().expect("4 bytes")) as i64),
        0xCB => PackValue::Int(i64::from_be_bytes(take(input, 8)?.try_into().expect("8 bytes"))),
        0xCC | 0xCD | 0xCE => {
            let width = 1 << (marker - 0xCC);
            let len = be_len(input, width)?;
            PackValue::Bytes(take(input, len)?.to_vec())
        }
        0xD0 | 0xD1 | 0xD2 => {
            let len = be_len(input, 1 << (marker - 0xD0))?;
            PackValue::String(string_of(input, len)?)
        }
        0xD4 | 0xD5 | 0xD6 => {
            let len = be_len(input, 1 << (marker - 0xD4))?;
            list_of(input, len)?
        }
        0xD8 | 0xD9 | 0xDA => {
            let len = be_len(input, 1 << (marker - 0xD8))?;
            map_of(input, len)?
        }
        other => return Err(PackError::UnknownMarker(other)),
    })
}
```

Add `pub mod packstream;` to `crates/bolt/src/lib.rs`.

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p brainwire-bolt packstream`
Expected: the four `packstream::tests` pass.

- [ ] **Step 5: Commit**

```bash
git add client-brainwire/crates/bolt
git commit -m "feat(brainwire): add PackStream v1 encoding"
```

## Task 3: Version negotiation

**Files:**
- Create: `crates/bolt/src/handshake.rs`
- Modify: `crates/bolt/src/lib.rs` (add `pub mod handshake;`)

**Interfaces:**
- Produces:
  - `pub const MAGIC: [u8; 4] = [0x60, 0x60, 0xB0, 0x17]`
  - `pub fn negotiate(proposals: &[u8; 16]) -> Option<(u8, u8)>` returning
    `(major, minor)`
  - `pub fn response(version: Option<(u8, u8)>) -> [u8; 4]`

A proposal is four bytes `[0, range, minor, major]`: it accepts `major.minor`
down to `major.(minor - range)`. A proposal with major `0xFF` is the 5.7
manifest, which this server does not offer. Supported versions, best first:
5.4, 5.3, 5.2, 5.1. Never 5.5.

- [ ] **Step 1: Write the failing tests**

At the bottom of `crates/bolt/src/handshake.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_5_28_gets_5_4() {
        // What driver 5.28.6 sends after the magic: manifest, 5.8..5.0, 4.4..4.2, 3.0.
        let p = [0x00, 0x00, 0x01, 0xFF, 0x00, 0x08, 0x08, 0x05, 0x00, 0x02, 0x04, 0x04, 0x00, 0x00, 0x00, 0x03];
        assert_eq!(negotiate(&p), Some((5, 4)));
        assert_eq!(response(Some((5, 4))), [0x00, 0x00, 0x04, 0x05]);
    }

    #[test]
    fn exact_5_2_is_accepted_and_5_5_is_not() {
        let mut p = [0u8; 16];
        p[..4].copy_from_slice(&[0, 0, 2, 5]);
        assert_eq!(negotiate(&p), Some((5, 2)));
        p[..4].copy_from_slice(&[0, 0, 5, 5]);
        assert_eq!(negotiate(&p), None);
        assert_eq!(response(None), [0, 0, 0, 0]);
    }

    #[test]
    fn bolt_4_only_is_refused() {
        let mut p = [0u8; 16];
        p[..4].copy_from_slice(&[0, 2, 4, 4]);
        assert_eq!(negotiate(&p), None);
    }
}
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test -p brainwire-bolt handshake`
Expected: compile errors, `negotiate` is not defined.

- [ ] **Step 3: Implement**

Top of `crates/bolt/src/handshake.rs`:

```rust
//! Bolt version negotiation (research/bolt.md, section 1).

pub const MAGIC: [u8; 4] = [0x60, 0x60, 0xB0, 0x17];

const SUPPORTED: [(u8, u8); 4] = [(5, 4), (5, 3), (5, 2), (5, 1)];

pub fn negotiate(proposals: &[u8; 16]) -> Option<(u8, u8)> {
    for p in proposals.chunks(4) {
        let (range, minor, major) = (p[1], p[2], p[3]);
        if major == 0xFF {
            continue; // 5.7 manifest proposal, not offered
        }
        let lowest = minor.saturating_sub(range);
        if let Some(v) = SUPPORTED.iter().find(|(ma, mi)| *ma == major && *mi <= minor && *mi >= lowest) {
            return Some(*v);
        }
    }
    None
}

pub fn response(version: Option<(u8, u8)>) -> [u8; 4] {
    match version {
        Some((major, minor)) => [0, 0, minor, major],
        None => [0, 0, 0, 0],
    }
}
```

Add `pub mod handshake;` to `crates/bolt/src/lib.rs`.

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p brainwire-bolt handshake`
Expected: the three `handshake::tests` pass.

- [ ] **Step 5: Commit**

```bash
git add client-brainwire/crates/bolt
git commit -m "feat(brainwire): negotiate Bolt 5.1 to 5.4"
```

## Task 4: Requests and responses

**Files:**
- Create: `crates/bolt/src/message.rs`
- Modify: `crates/bolt/src/lib.rs` (add `pub mod message;`)

**Interfaces:**
- Consumes: `PackValue` (Task 2).
- Produces:
  - `pub enum Request { Hello { extra: PackValue }, Logon { auth: PackValue }, Logoff, Telemetry, Run { query: String, params: PackValue, extra: PackValue }, Pull { extra: PackValue }, Discard { extra: PackValue }, Begin { extra: PackValue }, Commit, Rollback, Reset, Goodbye, Route }`
  - `pub fn parse_request(value: PackValue) -> Result<Request, MessageError>`
  - `pub enum Response { Success(Vec<(String, PackValue)>), Record(Vec<PackValue>), Ignored, Failure { code: String, message: String } }`
    with `fn to_value(&self) -> PackValue`
  - `pub enum MessageError { NotAStruct, UnknownTag(u8), BadFields(u8) }`

Tags: HELLO `01`, GOODBYE `02`, RESET `0F`, RUN `10`, BEGIN `11`, COMMIT `12`,
ROLLBACK `13`, DISCARD `2F`, PULL `3F`, TELEMETRY `54`, ROUTE `66`, LOGON `6A`,
LOGOFF `6B`; SUCCESS `70`, RECORD `71`, IGNORED `7E`, FAILURE `7F`.

- [ ] **Step 1: Write the failing tests**

At the bottom of `crates/bolt/src/message.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn s(tag: u8, fields: Vec<PackValue>) -> PackValue {
        PackValue::Struct { tag, fields }
    }

    #[test]
    fn parses_run_with_its_three_fields() {
        let req = parse_request(s(
            0x10,
            vec![PackValue::String("RETURN 1".into()), PackValue::Map(vec![]), PackValue::Map(vec![])],
        ))
        .unwrap();
        assert!(matches!(req, Request::Run { query, .. } if query == "RETURN 1"));
    }

    #[test]
    fn parses_the_simple_messages() {
        assert!(matches!(parse_request(s(0x0F, vec![])).unwrap(), Request::Reset));
        assert!(matches!(parse_request(s(0x12, vec![])).unwrap(), Request::Commit));
        assert!(matches!(parse_request(s(0x02, vec![])).unwrap(), Request::Goodbye));
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(parse_request(PackValue::Int(1)).unwrap_err(), MessageError::NotAStruct);
        assert_eq!(parse_request(s(0x99, vec![])).unwrap_err(), MessageError::UnknownTag(0x99));
        assert_eq!(parse_request(s(0x10, vec![])).unwrap_err(), MessageError::BadFields(0x10));
    }

    #[test]
    fn responses_become_structs() {
        let failure = Response::Failure { code: "Neo.ClientError.Statement.SyntaxError".into(), message: "m".into() }.to_value();
        assert_eq!(
            failure,
            s(0x7F, vec![PackValue::Map(vec![
                ("code".into(), PackValue::String("Neo.ClientError.Statement.SyntaxError".into())),
                ("message".into(), PackValue::String("m".into())),
            ])])
        );
        assert_eq!(Response::Ignored.to_value(), s(0x7E, vec![]));
    }
}
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test -p brainwire-bolt message`
Expected: compile errors, `Request` is not defined.

- [ ] **Step 3: Implement**

Top of `crates/bolt/src/message.rs`:

```rust
//! Bolt 5.x requests and responses (research/bolt.md, section 3).

use crate::packstream::PackValue;

#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Hello { extra: PackValue },
    Logon { auth: PackValue },
    Logoff,
    Telemetry,
    Run { query: String, params: PackValue, extra: PackValue },
    Pull { extra: PackValue },
    Discard { extra: PackValue },
    Begin { extra: PackValue },
    Commit,
    Rollback,
    Reset,
    Goodbye,
    Route,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MessageError {
    #[error("not a message structure")]
    NotAStruct,
    #[error("unknown message tag {0:#04x}")]
    UnknownTag(u8),
    #[error("bad fields for message {0:#04x}")]
    BadFields(u8),
}

fn map_field(fields: &mut Vec<PackValue>, tag: u8) -> Result<PackValue, MessageError> {
    match fields.pop() {
        Some(v @ PackValue::Map(_)) => Ok(v),
        _ => Err(MessageError::BadFields(tag)),
    }
}

pub fn parse_request(value: PackValue) -> Result<Request, MessageError> {
    let PackValue::Struct { tag, mut fields } = value else {
        return Err(MessageError::NotAStruct);
    };
    let expect = |n: usize, fields: &Vec<PackValue>| if fields.len() == n { Ok(()) } else { Err(MessageError::BadFields(tag)) };
    Ok(match tag {
        0x01 => {
            expect(1, &fields)?;
            Request::Hello { extra: map_field(&mut fields, tag)? }
        }
        0x02 => Request::Goodbye,
        0x0F => Request::Reset,
        0x10 => {
            expect(3, &fields)?;
            let extra = map_field(&mut fields, tag)?;
            let params = map_field(&mut fields, tag)?;
            let query = match fields.pop() {
                Some(PackValue::String(q)) => q,
                _ => return Err(MessageError::BadFields(tag)),
            };
            Request::Run { query, params, extra }
        }
        0x11 => {
            expect(1, &fields)?;
            Request::Begin { extra: map_field(&mut fields, tag)? }
        }
        0x12 => Request::Commit,
        0x13 => Request::Rollback,
        0x2F => {
            expect(1, &fields)?;
            Request::Discard { extra: map_field(&mut fields, tag)? }
        }
        0x3F => {
            expect(1, &fields)?;
            Request::Pull { extra: map_field(&mut fields, tag)? }
        }
        0x54 => Request::Telemetry,
        0x66 => Request::Route,
        0x6A => {
            expect(1, &fields)?;
            Request::Logon { auth: map_field(&mut fields, tag)? }
        }
        0x6B => Request::Logoff,
        other => return Err(MessageError::UnknownTag(other)),
    })
}

#[derive(Debug, Clone, PartialEq)]
pub enum Response {
    Success(Vec<(String, PackValue)>),
    Record(Vec<PackValue>),
    Ignored,
    Failure { code: String, message: String },
}

impl Response {
    pub fn to_value(&self) -> PackValue {
        match self {
            Response::Success(meta) => PackValue::Struct { tag: 0x70, fields: vec![PackValue::Map(meta.clone())] },
            Response::Record(values) => PackValue::Struct { tag: 0x71, fields: vec![PackValue::List(values.clone())] },
            Response::Ignored => PackValue::Struct { tag: 0x7E, fields: vec![] },
            Response::Failure { code, message } => PackValue::Struct {
                tag: 0x7F,
                fields: vec![PackValue::Map(vec![
                    ("code".into(), PackValue::String(code.clone())),
                    ("message".into(), PackValue::String(message.clone())),
                ])],
            },
        }
    }
}
```

Add `pub mod message;` to `crates/bolt/src/lib.rs`.

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p brainwire-bolt message`
Expected: the four `message::tests` pass.

- [ ] **Step 5: Commit**

```bash
git add client-brainwire/crates/bolt
git commit -m "feat(brainwire): parse Bolt requests and encode responses"
```

## Task 5: Session state machine and the stub engine

**Files:**
- Create: `crates/bolt/src/session.rs`
- Modify: `crates/bolt/src/lib.rs` (add `pub mod session;`)

**Interfaces:**
- Consumes: `Request`, `Response` (Task 4); `PackValue` (Task 2).
- Produces:
  - `pub struct QueryResult { pub fields: Vec<String>, pub rows: Vec<Vec<PackValue>> }`
  - `pub struct EngineError { pub code: String, pub message: String }`
  - `pub trait QueryEngine { fn run(&mut self, db: Option<&str>, query: &str, params: &PackValue) -> Result<QueryResult, EngineError>; }`
  - `pub struct StubEngine;` answering `RETURN <int> [AS <name>]`
  - `pub enum State { Connected, Authentication, Ready, Streaming, TxReady, TxStreaming, Failed, Defunct }`
  - `pub enum AuthMode { AllowAll }` (development only; real authentication
    arrives with the security plan)
  - `pub struct Session<E: QueryEngine>` with `Session::new(engine: E, auth: AuthMode)`,
    `fn state(&self) -> State` and `fn handle(&mut self, req: Request) -> Vec<Response>`

Behavior, from the 5.1+ state machine in research/bolt.md, section 3:

| State | Request | Responses | Next state |
|---|---|---|---|
| Connected | HELLO | SUCCESS `{server: "Neo4j/5.26.0", connection_id, hints}` | Authentication |
| Authentication | LOGON | SUCCESS | Ready |
| Ready | RUN | SUCCESS `{fields, t_first}` or FAILURE | Streaming or Failed |
| Streaming | PULL `{n}` | n RECORDs, then SUCCESS `{has_more: true}` or `{type: "r", t_last}` | Streaming or Ready |
| Streaming | DISCARD | SUCCESS `{type: "r"}` | Ready |
| Ready | BEGIN | SUCCESS | TxReady |
| TxReady | RUN | SUCCESS `{fields, t_first, qid}` or FAILURE | TxStreaming or Failed |
| TxStreaming | PULL | as above | TxStreaming or TxReady |
| TxReady | COMMIT | SUCCESS `{bookmark: "bw:<db>:<n>"}` | Ready |
| TxReady | ROLLBACK | SUCCESS | Ready |
| any but Defunct | RESET | SUCCESS | Ready |
| any | GOODBYE | nothing | Defunct |
| Ready, TxReady | TELEMETRY | SUCCESS | same |
| Failed | anything else | IGNORED | Failed |
| any | ROUTE | FAILURE (routing arrives with the cluster plan; use `bolt://`) | Failed |
| any | out of place | FAILURE `Neo.ClientError.Request.Invalid` | Failed |

`n = -1` in PULL means all remaining rows.

- [ ] **Step 1: Write the failing tests**

At the bottom of `crates/bolt/src/session.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, PackValue)]) -> PackValue {
        PackValue::Map(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
    }

    fn run(q: &str) -> Request {
        Request::Run { query: q.into(), params: map(&[]), extra: map(&[]) }
    }

    fn pull(n: i64) -> Request {
        Request::Pull { extra: map(&[("n", PackValue::Int(n))]) }
    }

    fn ready<E: QueryEngine>(engine: E) -> Session<E> {
        let mut s = Session::new(engine, AuthMode::AllowAll);
        let hello = s.handle(Request::Hello { extra: map(&[]) });
        match &hello[0] {
            Response::Success(meta) => {
                let server = meta.iter().find(|(k, _)| k == "server").map(|(_, v)| v.clone());
                assert!(matches!(server, Some(PackValue::String(ref v)) if v.starts_with("Neo4j/")));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(s.handle(Request::Logon { auth: map(&[]) }).len(), 1);
        assert_eq!(s.state(), State::Ready);
        s
    }

    struct ThreeRows;
    impl QueryEngine for ThreeRows {
        fn run(&mut self, _: Option<&str>, _: &str, _: &PackValue) -> Result<QueryResult, EngineError> {
            Ok(QueryResult { fields: vec!["i".into()], rows: (0..3).map(|i| vec![PackValue::Int(i)]).collect() })
        }
    }

    #[test]
    fn auto_commit_run_and_pull() {
        let mut s = ready(StubEngine);
        let r = s.handle(run("RETURN 1 AS x"));
        assert!(matches!(&r[0], Response::Success(meta) if meta.iter().any(|(k, v)| k == "fields" && *v == PackValue::List(vec![PackValue::String("x".into())]))));
        let r = s.handle(pull(1000));
        assert_eq!(r[0], Response::Record(vec![PackValue::Int(1)]));
        assert!(matches!(&r[1], Response::Success(_)));
        assert_eq!(s.state(), State::Ready);
    }

    #[test]
    fn explicit_transaction_returns_a_bookmark() {
        let mut s = ready(StubEngine);
        s.handle(Request::Begin { extra: map(&[]) });
        assert_eq!(s.state(), State::TxReady);
        s.handle(run("RETURN 2"));
        assert_eq!(s.state(), State::TxStreaming);
        s.handle(pull(-1));
        assert_eq!(s.state(), State::TxReady);
        let r = s.handle(Request::Commit);
        assert!(matches!(&r[0], Response::Success(meta) if meta.iter().any(|(k, _)| k == "bookmark")));
        assert_eq!(s.state(), State::Ready);
    }

    #[test]
    fn failure_then_ignored_until_reset() {
        let mut s = ready(StubEngine);
        let r = s.handle(run("MATCH (n) RETURN n"));
        assert!(matches!(&r[0], Response::Failure { code, .. } if code == "Neo.ClientError.Statement.SyntaxError"));
        assert_eq!(s.handle(pull(10)), vec![Response::Ignored]);
        assert!(matches!(s.handle(Request::Reset)[0], Response::Success(_)));
        assert_eq!(s.state(), State::Ready);
    }

    #[test]
    fn pull_in_batches_reports_has_more() {
        let mut s = ready(ThreeRows);
        s.handle(run("anything"));
        let r = s.handle(pull(2));
        assert_eq!(r.len(), 3);
        assert!(matches!(&r[2], Response::Success(meta) if meta.iter().any(|(k, v)| k == "has_more" && *v == PackValue::Bool(true))));
        let r = s.handle(pull(2));
        assert_eq!(r.len(), 2);
        assert_eq!(s.state(), State::Ready);
    }

    #[test]
    fn goodbye_answers_nothing() {
        let mut s = ready(StubEngine);
        assert!(s.handle(Request::Goodbye).is_empty());
        assert_eq!(s.state(), State::Defunct);
    }
}
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test -p brainwire-bolt session`
Expected: compile errors, `Session` is not defined.

- [ ] **Step 3: Implement**

Top of `crates/bolt/src/session.rs`:

```rust
//! The Bolt 5.1+ server state machine, independent of the network.

use std::collections::VecDeque;

use crate::message::{Request, Response};
use crate::packstream::PackValue;

pub struct QueryResult {
    pub fields: Vec<String>,
    pub rows: Vec<Vec<PackValue>>,
}

pub struct EngineError {
    pub code: String,
    pub message: String,
}

pub trait QueryEngine {
    fn run(&mut self, db: Option<&str>, query: &str, params: &PackValue) -> Result<QueryResult, EngineError>;
}

/// Answers `RETURN <integer> [AS <name>]` only. The Cypher plan replaces it.
pub struct StubEngine;

impl QueryEngine for StubEngine {
    fn run(&mut self, _db: Option<&str>, query: &str, _params: &PackValue) -> Result<QueryResult, EngineError> {
        let refuse = || EngineError {
            code: "Neo.ClientError.Statement.SyntaxError".into(),
            message: "the stub engine only answers RETURN <integer> [AS <name>]".into(),
        };
        let words: Vec<&str> = query.split_whitespace().collect();
        let (value, name) = match words.as_slice() {
            [r, v] if r.eq_ignore_ascii_case("RETURN") => (*v, *v),
            [r, v, a, n] if r.eq_ignore_ascii_case("RETURN") && a.eq_ignore_ascii_case("AS") => (*v, *n),
            _ => return Err(refuse()),
        };
        let int: i64 = value.parse().map_err(|_| refuse())?;
        Ok(QueryResult { fields: vec![name.to_string()], rows: vec![vec![PackValue::Int(int)]] })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Connected,
    Authentication,
    Ready,
    Streaming,
    TxReady,
    TxStreaming,
    Failed,
    Defunct,
}

pub enum AuthMode {
    /// Development only; real authentication arrives with the security plan.
    AllowAll,
}

pub struct Session<E: QueryEngine> {
    engine: E,
    state: State,
    rows: VecDeque<Vec<PackValue>>,
    db: Option<String>,
    commits: u64,
    _auth: AuthMode,
}

fn meta(pairs: Vec<(&str, PackValue)>) -> Response {
    Response::Success(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn invalid(message: &str) -> Response {
    Response::Failure { code: "Neo.ClientError.Request.Invalid".into(), message: message.into() }
}

impl<E: QueryEngine> Session<E> {
    pub fn new(engine: E, auth: AuthMode) -> Session<E> {
        Session { engine, state: State::Connected, rows: VecDeque::new(), db: None, commits: 0, _auth: auth }
    }

    pub fn state(&self) -> State {
        self.state
    }

    fn fail(&mut self, response: Response) -> Vec<Response> {
        self.state = State::Failed;
        self.rows.clear();
        vec![response]
    }

    fn start_query(&mut self, query: &str, params: &PackValue, in_tx: bool) -> Vec<Response> {
        match self.engine.run(self.db.as_deref(), query, params) {
            Ok(result) => {
                self.rows = result.rows.into();
                self.state = if in_tx { State::TxStreaming } else { State::Streaming };
                let fields = PackValue::List(result.fields.into_iter().map(PackValue::String).collect());
                let mut pairs = vec![("fields", fields), ("t_first", PackValue::Int(0))];
                if in_tx {
                    pairs.push(("qid", PackValue::Int(0)));
                }
                vec![meta(pairs)]
            }
            Err(e) => self.fail(Response::Failure { code: e.code, message: e.message }),
        }
    }

    fn pull(&mut self, extra: &PackValue, discard: bool) -> Vec<Response> {
        let n = match extra.get("n") {
            Some(PackValue::Int(n)) if *n >= 0 && !discard => *n as usize,
            _ if discard => 0,
            _ => usize::MAX,
        };
        let mut out = Vec::new();
        if discard {
            self.rows.clear();
        }
        while out.len() < n {
            match self.rows.pop_front() {
                Some(row) => out.push(Response::Record(row)),
                None => break,
            }
        }
        let in_tx = self.state == State::TxStreaming;
        if self.rows.is_empty() {
            self.state = if in_tx { State::TxReady } else { State::Ready };
            out.push(meta(vec![("type", PackValue::String("r".into())), ("t_last", PackValue::Int(0))]));
        } else {
            out.push(meta(vec![("has_more", PackValue::Bool(true))]));
        }
        out
    }

    pub fn handle(&mut self, req: Request) -> Vec<Response> {
        use State::*;
        match (self.state, req) {
            (Defunct, _) => vec![],
            (_, Request::Goodbye) => {
                self.state = Defunct;
                vec![]
            }
            (_, Request::Reset) => {
                self.rows.clear();
                self.state = Ready;
                vec![meta(vec![])]
            }
            (Failed, _) => vec![Response::Ignored],
            (_, Request::Route) => self.fail(invalid("routing arrives with the cluster plan; use bolt://")),
            (Connected, Request::Hello { .. }) => {
                self.state = Authentication;
                vec![meta(vec![
                    ("server", PackValue::String("Neo4j/5.26.0".into())),
                    ("connection_id", PackValue::String("bw-1".into())),
                    ("hints", PackValue::Map(vec![(
                        "brainwire.version".into(),
                        PackValue::String(env!("CARGO_PKG_VERSION").into()),
                    )])),
                ])]
            }
            (Authentication, Request::Logon { .. }) => {
                self.state = Ready;
                vec![meta(vec![])]
            }
            (Ready | TxReady, Request::Telemetry) => vec![meta(vec![])],
            (Ready, Request::Run { query, params, extra }) => {
                self.db = match extra.get("db") {
                    Some(PackValue::String(db)) => Some(db.clone()),
                    _ => None,
                };
                self.start_query(&query, &params, false)
            }
            (Ready, Request::Begin { extra }) => {
                self.db = match extra.get("db") {
                    Some(PackValue::String(db)) => Some(db.clone()),
                    _ => None,
                };
                self.state = TxReady;
                vec![meta(vec![])]
            }
            (TxReady, Request::Run { query, params, .. }) => self.start_query(&query, &params, true),
            (Streaming | TxStreaming, Request::Pull { extra }) => self.pull(&extra, false),
            (Streaming | TxStreaming, Request::Discard { extra }) => self.pull(&extra, true),
            (TxReady, Request::Commit) => {
                self.commits += 1;
                self.state = Ready;
                let db = self.db.clone().unwrap_or_else(|| "default".into());
                vec![meta(vec![("bookmark", PackValue::String(format!("bw:{db}:{}", self.commits)))])]
            }
            (TxReady, Request::Rollback) => {
                self.state = Ready;
                vec![meta(vec![])]
            }
            (_, _) => self.fail(invalid("message not allowed in the current state")),
        }
    }
}
```

Add `pub mod session;` to `crates/bolt/src/lib.rs`.

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p brainwire-bolt session`
Expected: the five `session::tests` pass.

- [ ] **Step 5: Commit**

```bash
git add client-brainwire/crates/bolt
git commit -m "feat(brainwire): add the Bolt session state machine with a stub engine"
```

## Task 6: TCP server and the Python driver check

**Files:**
- Create: `crates/bolt/src/server.rs`, `crates/bolt/src/bin/brainwire-bolt-dev.rs`,
  `crates/bolt/tests/driver_smoke.py`
- Modify: `crates/bolt/src/lib.rs` (add `pub mod server;`)

**Interfaces:**
- Consumes: everything above.
- Produces:
  - `pub async fn serve<E, F>(listener: tokio::net::TcpListener, make_engine: F) -> std::io::Result<()>`
    where `E: QueryEngine + Send + 'static` and `F: Fn() -> E + Send + Sync + 'static`
  - the `brainwire-bolt-dev` binary: `brainwire-bolt-dev [addr]`, stub engine,
    no authentication, for development only

- [ ] **Step 1: Implement the server**

`crates/bolt/src/server.rs`:

```rust
//! Accepts TCP connections, negotiates the version, then feeds messages to a
//! Session and writes its responses back.

use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::chunk::{encode_message, ChunkReader};
use crate::handshake::{negotiate, response, MAGIC};
use crate::message::{parse_request, Response};
use crate::packstream::{pack, unpack};
use crate::session::{AuthMode, QueryEngine, Session, State};

pub async fn serve<E, F>(listener: TcpListener, make_engine: F) -> std::io::Result<()>
where
    E: QueryEngine + Send + 'static,
    F: Fn() -> E + Send + Sync + 'static,
{
    let make_engine = Arc::new(make_engine);
    loop {
        let (stream, _) = listener.accept().await?;
        let make = Arc::clone(&make_engine);
        tokio::spawn(async move {
            let _ = handle_connection(stream, make()).await;
        });
    }
}

async fn handle_connection<E: QueryEngine>(mut stream: TcpStream, engine: E) -> std::io::Result<()> {
    let mut hello = [0u8; 20];
    stream.read_exact(&mut hello).await?;
    if hello[..4] != MAGIC {
        return Ok(());
    }
    let proposals: [u8; 16] = hello[4..].try_into().expect("16 bytes");
    let version = negotiate(&proposals);
    stream.write_all(&response(version)).await?;
    if version.is_none() {
        return Ok(());
    }
    let mut session = Session::new(engine, AuthMode::AllowAll);
    let mut reader = ChunkReader::default();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        while let Some(payload) = reader.next_message() {
            let mut input = payload.as_slice();
            let responses = match unpack(&mut input).ok().and_then(|v| parse_request(v).ok()) {
                Some(req) => session.handle(req),
                None => vec![Response::Failure {
                    code: "Neo.ClientError.Request.Invalid".into(),
                    message: "malformed message".into(),
                }],
            };
            let mut out = Vec::new();
            for r in responses {
                let mut body = Vec::new();
                pack(&r.to_value(), &mut body);
                out.extend_from_slice(&encode_message(&body));
            }
            if !out.is_empty() {
                stream.write_all(&out).await?;
            }
            if session.state() == State::Defunct {
                return Ok(());
            }
        }
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        reader.push(&buf[..n]);
    }
}
```

`crates/bolt/src/bin/brainwire-bolt-dev.rs`:

```rust
//! Development server: stub engine, no authentication. Never expose it.

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let addr = std::env::args().nth(1).unwrap_or_else(|| "127.0.0.1:7687".to_string());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    eprintln!("brainwire-bolt-dev listening on {addr} (stub engine, no authentication)");
    brainwire_bolt::server::serve(listener, || brainwire_bolt::session::StubEngine).await
}
```

Add `pub mod server;` to `crates/bolt/src/lib.rs`.

- [ ] **Step 2: Write the driver check**

`crates/bolt/tests/driver_smoke.py`:

```python
"""Checks that the official Neo4j Python driver 5.x talks to brainwire-bolt-dev."""

import sys

from neo4j import GraphDatabase

uri = sys.argv[1] if len(sys.argv) > 1 else "bolt://127.0.0.1:7687"
with GraphDatabase.driver(uri, auth=("neo4j", "dev")) as driver:
    driver.verify_connectivity()
    records, _summary, keys = driver.execute_query("RETURN 1 AS x")
    assert keys == ["x"], keys
    assert records[0]["x"] == 1, records
    with driver.session() as session:
        assert session.run("RETURN 2 AS y").single()["y"] == 2
print("driver smoke: ok")
```

- [ ] **Step 3: Run the server and the check**

The check needs the `neo4j` Python package 5.x. First run
`python3 -c "import neo4j, sys; print(neo4j.__version__)"`; if it is missing,
ask Allan before installing anything (installing downloads a package).

```bash
cargo build -p brainwire-bolt --bin brainwire-bolt-dev
./target/debug/brainwire-bolt-dev 127.0.0.1:17687 &
python3 crates/bolt/tests/driver_smoke.py bolt://127.0.0.1:17687
kill %1
```

Expected: `driver smoke: ok`. If the driver refuses the connection, compare the
exchange with research/bolt.md, sections 1 and 3, before changing code (skill
`superpowers:systematic-debugging`).

- [ ] **Step 4: Run the whole workspace**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all -- --check`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add client-brainwire/crates/bolt
git commit -m "feat(brainwire): serve Bolt over TCP and pass the Python driver check"
```

## What comes next

With the storage plan and this one done, Gate 1 closes: crash consistency, the
ontology as schema, and the Python driver connected. Plan 3 (Cypher, native
ACL, indexes, introspection, contract suite) replaces the stub engine with the
real one over the store. Authentication (`basic` against `system`, `bearer`
with the client-identity JWT) arrives with the security work of plan 3; until
then only the development binary exists, and it must never be exposed.
