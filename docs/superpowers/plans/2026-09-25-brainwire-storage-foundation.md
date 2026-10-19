# Brainwire storage foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Rust workspace with the storage core of Brainwire: order-preserving
keys, node and edge ids, the ontology catalog, the row codec, RocksDB with the
column families of the spec, snapshot-isolated transactions with
first-committer-wins, basic graph operations, and a crash test that proves the
store reopens consistent after `kill -9`.

**Architecture:** Two crates. `brainwire-core` has no I/O: keys, ids, values,
the row codec and the catalog built from the platform's `GraphMappingConfig`.
`brainwire-store` owns RocksDB: one database per directory, shared cache and
memtable budget, an in-house write overlay per transaction, a committer that
validates conflicts, id allocation in blocks, and graph operations on top of
it. Bolt comes in plan 2 and Cypher in plan 3.

**Tech Stack:** Rust (stable), `rocksdb` 0.25 (bundles RocksDB 11.8.1),
`serde` + `serde_json`, `thiserror`, `proptest`, `tempfile`.

**Spec:** [2026-09-24-brainwire-design.md](../specs/2026-09-24-brainwire-design.md),
section 1 (storage) and sections 2.1, 2.4 and 4.4 for the catalog, events and
notes families.

## Global Constraints

- Keys are big-endian, so byte order is logical order (spec 1.2).
- Column families per database: `meta`, `node`, `pk`, `out`, `in`, `lbl`, `idx`,
  `ident`, `event`, `acl`, `note` (spec 1.2 and 4.4).
- Node id: partition (10 bits) · type (12) · sequence (42). Edge id: partition
  (10) · sequence (54). Partition is always 0 in this version (spec 1.3).
- `elementId`: `n:<id>` for a node, `e:<source>:<type>:<target>:<id>` for an
  edge (spec 1.3).
- `pk` key: `entity_id` · type, with the `entity_id` encoded prefix-free so
  `user:7` never matches `user:70` (spec 1.2).
- Snapshot isolation; first committer wins; a conflict is a transient error
  (spec 1.4).
- A crash never reuses an id (spec 1.3).
- Type id 0 is the untyped node (`CREATE ()`); ontology types start at 1; at
  most 4,095 types (spec 1.3).
- Properties named `acl_*` never become catalog columns (spec 2.2).
- Tests, fixtures and benchmarks use synthetic data only.
- Code, identifiers, comments, commit messages and internal docs in English.
  No em dashes or en dashes in docs.
- Commits go to the `feat/brainwire` branch only, never to another feature's
  branch.

## File structure

```
external-brainwire/
  Cargo.toml                         workspace
  rust-toolchain.toml                pinned toolchain
  crates/core/                       package brainwire-core (no I/O)
    Cargo.toml
    src/lib.rs                       module list and re-exports
    src/keys.rs                      order-preserving key encoding + key builders
    src/ids.rs                       TypeId, NodeId, EdgeTypeId, EdgeId, element ids
    src/value.rs                     Value
    src/codec.rs                     row codec (node, edge and event rows)
    src/schema.rs                    GraphMapping (JSON input) and Catalog
    tests/fixtures/graph_mapping.json  synthetic ontology fixture
  crates/store/                      package brainwire-store (RocksDB)
    Cargo.toml
    src/lib.rs
    src/db.rs                        Resources, Database, column families
    src/txn.rs                       Txn (overlay + snapshot) and Committer
    src/alloc.rs                     IdAllocator (blocks persisted before use)
    src/graph.rs                     Graph operations: merge_node, merge_edge, ...
    src/bin/crash_writer.rs          helper binary for the crash test
    tests/crash.rs                   kill -9 test
```

## Task 0: Isolated workspace

The main checkout is on another feature branch with uncommitted work, so this
plan runs in a worktree of `feat/brainwire` (the branch already holds the
docs, commit `d1c50cfab`).

- [ ] **Step 1: Ask Allan for consent and create the worktree**

Follow `superpowers:using-git-worktrees`. With consent:

```bash
cd /Users/fraga/Documents/external-platform
git worktree add .worktrees/brainwire feat/brainwire
cd .worktrees/brainwire/external-brainwire
```

- [ ] **Step 2: Retire the untracked copy in the main checkout**

The same files live untracked in the main checkout. After confirming the
worktree has them (`git -C .worktrees/brainwire log --oneline -1`), move the
untracked copy to the session scratchpad instead of deleting it, and tell Allan
the new path of the folder.

## Task 1: Workspace scaffold

**Files:**
- Create: `external-brainwire/Cargo.toml`, `external-brainwire/rust-toolchain.toml`
- Create: `external-brainwire/crates/core/Cargo.toml`, `crates/core/src/lib.rs`
- Create: `external-brainwire/crates/store/Cargo.toml`, `crates/store/src/lib.rs`
- Create: `external-brainwire/.gitignore`

**Interfaces:**
- Produces: packages `brainwire-core` and `brainwire-store` that build and test.

- [ ] **Step 1: Write the workspace files**

`external-brainwire/Cargo.toml`:

```toml
[workspace]
resolver = "2"
members = ["crates/core", "crates/store"]

[workspace.package]
edition = "2021"
version = "0.1.0"
license = "UNLICENSED"
publish = false

[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "1"
proptest = "1"
tempfile = "3"
rocksdb = "0.25"
```

`external-brainwire/rust-toolchain.toml`:

```toml
[toolchain]
channel = "stable"
components = ["clippy", "rustfmt"]
```

`external-brainwire/.gitignore`:

```
/target
```

`crates/core/Cargo.toml`:

```toml
[package]
name = "brainwire-core"
edition.workspace = true
version.workspace = true
license.workspace = true
publish.workspace = true

[dependencies]
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true

[dev-dependencies]
proptest.workspace = true
```

`crates/store/Cargo.toml`:

```toml
[package]
name = "brainwire-store"
edition.workspace = true
version.workspace = true
license.workspace = true
publish.workspace = true

[dependencies]
brainwire-core = { path = "../core" }
rocksdb.workspace = true
thiserror.workspace = true

[dev-dependencies]
tempfile.workspace = true
```

`crates/core/src/lib.rs` and `crates/store/src/lib.rs` start as:

```rust
//! Brainwire storage core. See docs/superpowers/specs/2026-09-24-brainwire-design.md.

#[cfg(test)]
mod smoke {
    #[test]
    fn builds() {}
}
```

- [ ] **Step 2: Build and test**

Run: `cargo test --workspace` (the first build compiles RocksDB and takes
minutes).
Expected: `test smoke::builds ... ok` twice.

- [ ] **Step 3: Record the build time**

Run: `cargo clean -p brainwire-core && time cargo test --workspace`
Expected: note the incremental time in the JOURNAL. The plan's reversal rule
(PLAN.md, section 2) switches to Go + Pebble if the edit-compile-test loop goes
over about 2 minutes.

- [ ] **Step 4: Commit**

```bash
git add external-brainwire/Cargo.toml external-brainwire/rust-toolchain.toml external-brainwire/.gitignore external-brainwire/crates
git commit -m "chore(brainwire): scaffold the Rust workspace with core and store crates"
```

## Task 2: Order-preserving key encoding

**Files:**
- Create: `crates/core/src/keys.rs`
- Modify: `crates/core/src/lib.rs` (add `pub mod keys;`)

**Interfaces:**
- Produces:
  - `pub fn put_u16(out: &mut Vec<u8>, v: u16)`, `put_u32`, `put_u64`
  - `pub fn put_i64(out: &mut Vec<u8>, v: i64)` (sign bit flipped, big-endian)
  - `pub fn put_bytes(out: &mut Vec<u8>, v: &[u8])` (0x00 escaped as 0x00 0xFF,
    terminated by 0x00 0x01: prefix-free and order-preserving)
  - `pub fn get_u16(input: &mut &[u8]) -> Result<u16, KeyError>`, `get_u32`,
    `get_u64`, `get_i64`, `get_bytes -> Result<Vec<u8>, KeyError>`
  - `pub enum KeyError { Truncated, BadEscape }`

- [ ] **Step 1: Write the failing tests**

At the bottom of `crates/core/src/keys.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn enc_bytes(v: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        put_bytes(&mut out, v);
        out
    }

    #[test]
    fn bytes_are_prefix_free() {
        let a = enc_bytes(b"user:7");
        let b = enc_bytes(b"user:70");
        assert!(!b.starts_with(&a));
    }

    #[test]
    fn bytes_round_trip_with_zero_bytes() {
        let raw = [0u8, 1, 0, 0xFF, 0];
        let enc = enc_bytes(&raw);
        let mut input = enc.as_slice();
        assert_eq!(get_bytes(&mut input).unwrap(), raw.to_vec());
        assert!(input.is_empty());
    }

    #[test]
    fn truncated_input_is_an_error() {
        let mut input: &[u8] = &[0, 1];
        assert_eq!(get_u64(&mut input), Err(KeyError::Truncated));
    }

    proptest! {
        #[test]
        fn u64_order(a: u64, b: u64) {
            let (mut x, mut y) = (Vec::new(), Vec::new());
            put_u64(&mut x, a);
            put_u64(&mut y, b);
            prop_assert_eq!(a.cmp(&b), x.cmp(&y));
        }

        #[test]
        fn i64_order(a: i64, b: i64) {
            let (mut x, mut y) = (Vec::new(), Vec::new());
            put_i64(&mut x, a);
            put_i64(&mut y, b);
            prop_assert_eq!(a.cmp(&b), x.cmp(&y));
        }

        #[test]
        fn bytes_order_and_round_trip(a: Vec<u8>, b: Vec<u8>) {
            let (x, y) = (enc_bytes(&a), enc_bytes(&b));
            prop_assert_eq!(a.cmp(&b), x.cmp(&y));
            let mut input = x.as_slice();
            prop_assert_eq!(get_bytes(&mut input).unwrap(), a);
        }
    }
}
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test -p brainwire-core keys`
Expected: compile errors, `put_u64` and the others are not defined.

- [ ] **Step 3: Implement**

Top of `crates/core/src/keys.rs`:

```rust
//! Order-preserving key encoding. Byte order of an encoded key equals the
//! logical order of its parts (spec 1.2).

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    #[error("key is truncated")]
    Truncated,
    #[error("bad escape sequence in encoded bytes")]
    BadEscape,
}

pub fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

pub fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

pub fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_be_bytes());
}

/// Flips the sign bit so negative numbers sort before positive ones.
pub fn put_i64(out: &mut Vec<u8>, v: i64) {
    put_u64(out, (v as u64) ^ (1 << 63));
}

/// Escapes 0x00 as 0x00 0xFF and terminates with 0x00 0x01. The result is
/// prefix-free and keeps lexicographic order.
pub fn put_bytes(out: &mut Vec<u8>, v: &[u8]) {
    for &b in v {
        out.push(b);
        if b == 0 {
            out.push(0xFF);
        }
    }
    out.extend_from_slice(&[0x00, 0x01]);
}

fn take<const N: usize>(input: &mut &[u8]) -> Result<[u8; N], KeyError> {
    if input.len() < N {
        return Err(KeyError::Truncated);
    }
    let (head, rest) = input.split_at(N);
    *input = rest;
    Ok(head.try_into().expect("length checked"))
}

pub fn get_u16(input: &mut &[u8]) -> Result<u16, KeyError> {
    Ok(u16::from_be_bytes(take::<2>(input)?))
}

pub fn get_u32(input: &mut &[u8]) -> Result<u32, KeyError> {
    Ok(u32::from_be_bytes(take::<4>(input)?))
}

pub fn get_u64(input: &mut &[u8]) -> Result<u64, KeyError> {
    Ok(u64::from_be_bytes(take::<8>(input)?))
}

pub fn get_i64(input: &mut &[u8]) -> Result<i64, KeyError> {
    Ok((get_u64(input)? ^ (1 << 63)) as i64)
}

pub fn get_bytes(input: &mut &[u8]) -> Result<Vec<u8>, KeyError> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < input.len() {
        let b = input[i];
        if b != 0 {
            out.push(b);
            i += 1;
            continue;
        }
        match input.get(i + 1) {
            Some(0xFF) => {
                out.push(0);
                i += 2;
            }
            Some(0x01) => {
                *input = &input[i + 2..];
                return Ok(out);
            }
            Some(_) => return Err(KeyError::BadEscape),
            None => return Err(KeyError::Truncated),
        }
    }
    Err(KeyError::Truncated)
}
```

Add `pub mod keys;` to `crates/core/src/lib.rs`.

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p brainwire-core keys`
Expected: all `keys::tests` pass, including the three property tests.

- [ ] **Step 5: Commit**

```bash
git add external-brainwire/crates/core
git commit -m "feat(brainwire): add order-preserving key encoding"
```

## Task 3: Node and edge ids

**Files:**
- Create: `crates/core/src/ids.rs`
- Modify: `crates/core/src/lib.rs` (add `pub mod ids;`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub struct TypeId(pub u16)` with `TypeId::UNTYPED = TypeId(0)` and
    `TypeId::MAX = 4095`
  - `pub struct EdgeTypeId(pub u16)`
  - `pub struct NodeId(pub u64)` with `NodeId::new(partition: u16, ty: TypeId,
    seq: u64) -> Result<NodeId, IdError>`, `.partition() -> u16`,
    `.type_id() -> TypeId`, `.seq() -> u64`, `.element_id() -> String`,
    `NodeId::parse_element_id(&str) -> Result<NodeId, IdError>`,
    `NodeId::type_range(partition: u16, ty: TypeId) -> (NodeId, NodeId)`
    (inclusive bounds of a label scan)
  - `pub struct EdgeId(pub u64)` with `EdgeId::new(partition: u16, seq: u64)`,
    `EdgeId::MERGED = EdgeId(0)`
  - `pub fn edge_element_id(src: NodeId, ty: EdgeTypeId, dst: NodeId, id: EdgeId) -> String`
  - `pub enum IdError { PartitionOutOfRange, TypeOutOfRange, SequenceOutOfRange, BadElementId }`

- [ ] **Step 1: Write the failing tests**

At the bottom of `crates/core/src/ids.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_round_trip() {
        let id = NodeId::new(3, TypeId(42), 123_456).unwrap();
        assert_eq!(id.partition(), 3);
        assert_eq!(id.type_id(), TypeId(42));
        assert_eq!(id.seq(), 123_456);
    }

    #[test]
    fn bounds_are_enforced() {
        assert_eq!(NodeId::new(1024, TypeId(1), 0), Err(IdError::PartitionOutOfRange));
        assert_eq!(NodeId::new(0, TypeId(4096), 0), Err(IdError::TypeOutOfRange));
        assert_eq!(NodeId::new(0, TypeId(1), 1 << 42), Err(IdError::SequenceOutOfRange));
    }

    #[test]
    fn a_label_scan_is_one_contiguous_range() {
        let (lo, hi) = NodeId::type_range(0, TypeId(7));
        let inside = NodeId::new(0, TypeId(7), 99).unwrap();
        let other = NodeId::new(0, TypeId(8), 0).unwrap();
        assert!(lo.0 <= inside.0 && inside.0 <= hi.0);
        assert!(other.0 > hi.0);
        assert!(lo.0.to_be_bytes() <= inside.0.to_be_bytes());
    }

    #[test]
    fn element_ids_round_trip() {
        let id = NodeId::new(0, TypeId(5), 77).unwrap();
        assert_eq!(NodeId::parse_element_id(&id.element_id()).unwrap(), id);
        assert_eq!(NodeId::parse_element_id("x:1"), Err(IdError::BadElementId));
        let src = NodeId::new(0, TypeId(1), 1).unwrap();
        let dst = NodeId::new(0, TypeId(2), 2).unwrap();
        let e = edge_element_id(src, EdgeTypeId(9), dst, EdgeId::MERGED);
        assert_eq!(e, format!("e:{}:9:{}:0", src.0, dst.0));
    }
}
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test -p brainwire-core ids`
Expected: compile errors, `NodeId` is not defined.

- [ ] **Step 3: Implement**

Top of `crates/core/src/ids.rs`:

```rust
//! Node and edge ids (spec 1.3). Node: partition (10) · type (12) · seq (42).
//! Edge: partition (10) · seq (54).

const PARTITION_BITS: u32 = 10;
const TYPE_BITS: u32 = 12;
const NODE_SEQ_BITS: u32 = 42;
const EDGE_SEQ_BITS: u32 = 54;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    #[error("partition out of range")]
    PartitionOutOfRange,
    #[error("type id out of range")]
    TypeOutOfRange,
    #[error("sequence out of range")]
    SequenceOutOfRange,
    #[error("malformed element id")]
    BadElementId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TypeId(pub u16);

impl TypeId {
    pub const UNTYPED: TypeId = TypeId(0);
    pub const MAX: u16 = (1 << TYPE_BITS) - 1;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EdgeTypeId(pub u16);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub u64);

impl NodeId {
    pub fn new(partition: u16, ty: TypeId, seq: u64) -> Result<NodeId, IdError> {
        if u32::from(partition) >= 1 << PARTITION_BITS {
            return Err(IdError::PartitionOutOfRange);
        }
        if ty.0 > TypeId::MAX {
            return Err(IdError::TypeOutOfRange);
        }
        if seq >= 1 << NODE_SEQ_BITS {
            return Err(IdError::SequenceOutOfRange);
        }
        Ok(NodeId(
            (u64::from(partition) << (TYPE_BITS + NODE_SEQ_BITS))
                | (u64::from(ty.0) << NODE_SEQ_BITS)
                | seq,
        ))
    }

    pub fn partition(self) -> u16 {
        (self.0 >> (TYPE_BITS + NODE_SEQ_BITS)) as u16
    }

    pub fn type_id(self) -> TypeId {
        TypeId(((self.0 >> NODE_SEQ_BITS) & u64::from(TypeId::MAX)) as u16)
    }

    pub fn seq(self) -> u64 {
        self.0 & ((1 << NODE_SEQ_BITS) - 1)
    }

    /// Inclusive bounds of every id of one type in one partition.
    pub fn type_range(partition: u16, ty: TypeId) -> (NodeId, NodeId) {
        let lo = NodeId::new(partition, ty, 0).expect("valid bounds");
        let hi = NodeId::new(partition, ty, (1 << NODE_SEQ_BITS) - 1).expect("valid bounds");
        (lo, hi)
    }

    pub fn element_id(self) -> String {
        format!("n:{}", self.0)
    }

    pub fn parse_element_id(s: &str) -> Result<NodeId, IdError> {
        s.strip_prefix("n:")
            .and_then(|n| n.parse::<u64>().ok())
            .map(NodeId)
            .ok_or(IdError::BadElementId)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EdgeId(pub u64);

impl EdgeId {
    /// Id of an edge created by MERGE: it is identified by (source, type, target).
    pub const MERGED: EdgeId = EdgeId(0);

    pub fn new(partition: u16, seq: u64) -> Result<EdgeId, IdError> {
        if u32::from(partition) >= 1 << PARTITION_BITS {
            return Err(IdError::PartitionOutOfRange);
        }
        if seq >= 1 << EDGE_SEQ_BITS {
            return Err(IdError::SequenceOutOfRange);
        }
        Ok(EdgeId((u64::from(partition) << EDGE_SEQ_BITS) | seq))
    }
}

pub fn edge_element_id(src: NodeId, ty: EdgeTypeId, dst: NodeId, id: EdgeId) -> String {
    format!("e:{}:{}:{}:{}", src.0, ty.0, dst.0, id.0)
}
```

Add `pub mod ids;` to `crates/core/src/lib.rs`.

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p brainwire-core ids`
Expected: the four `ids::tests` pass.

- [ ] **Step 5: Commit**

```bash
git add external-brainwire/crates/core
git commit -m "feat(brainwire): add node and edge ids with element ids"
```

## Task 4: Values and the row codec

**Files:**
- Create: `crates/core/src/value.rs`, `crates/core/src/codec.rs`
- Modify: `crates/core/src/lib.rs` (add `pub mod value;` and `pub mod codec;`)

**Interfaces:**
- Produces:
  - `pub enum Value { Null, Bool(bool), Int(i64), Float(f64), Str(String), List(Vec<Value>), Map(BTreeMap<String, Value>) }`
    (derives `Debug, Clone, PartialEq`)
  - `pub struct Row { pub schema_version: u32, pub acl_set: u32, pub columns: BTreeMap<u16, Value>, pub extra: BTreeMap<String, Value> }`
    (derives `Debug, Clone, PartialEq, Default`); `columns` is keyed by column
    id, `extra` holds properties outside the ontology (the flexible map)
  - `pub fn encode_row(row: &Row) -> Vec<u8>`
  - `pub fn decode_row(bytes: &[u8]) -> Result<Row, CodecError>`
  - `pub enum CodecError { Truncated, UnknownTag(u8), BadUtf8, VarintOverflow }`

Encoding: LEB128 varints; `Int` as zigzag varint; `Float` as 8 bytes
little-endian; strings as varint length + UTF-8. Value tags: 0 Null, 1 false,
2 true, 3 Int, 4 Float, 5 Str, 6 List, 7 Map.

- [ ] **Step 1: Write the failing tests**

At the bottom of `crates/core/src/codec.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn arb_value() -> impl Strategy<Value = crate::value::Value> {
        let leaf = prop_oneof![
            Just(crate::value::Value::Null),
            any::<bool>().prop_map(crate::value::Value::Bool),
            any::<i64>().prop_map(crate::value::Value::Int),
            any::<f64>()
                .prop_filter("NaN never equals itself", |f| !f.is_nan())
                .prop_map(crate::value::Value::Float),
            ".{0,16}".prop_map(crate::value::Value::Str),
        ];
        leaf.prop_recursive(3, 32, 4, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..4).prop_map(crate::value::Value::List),
                prop::collection::btree_map(".{0,8}", inner, 0..4)
                    .prop_map(crate::value::Value::Map),
            ]
        })
    }

    #[test]
    fn empty_row_round_trips() {
        let row = Row::default();
        assert_eq!(decode_row(&encode_row(&row)).unwrap(), row);
    }

    #[test]
    fn unknown_tag_is_an_error() {
        // schema_version 1, acl_set 0, one column: id 0 with tag 99.
        let bytes = [1u8, 0, 1, 0, 99];
        assert_eq!(decode_row(&bytes), Err(CodecError::UnknownTag(99)));
    }

    #[test]
    fn truncated_row_is_an_error() {
        let mut row = Row::default();
        row.columns.insert(3, crate::value::Value::Str("hello".into()));
        let bytes = encode_row(&row);
        assert_eq!(decode_row(&bytes[..bytes.len() - 2]), Err(CodecError::Truncated));
    }

    proptest! {
        #[test]
        fn rows_round_trip(
            schema_version: u32,
            acl_set: u32,
            columns in prop::collection::btree_map(any::<u16>(), arb_value(), 0..8),
            extra in prop::collection::btree_map(".{0,8}", arb_value(), 0..4),
        ) {
            let row = Row { schema_version, acl_set, columns, extra };
            prop_assert_eq!(decode_row(&encode_row(&row)).unwrap(), row);
        }
    }
}
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test -p brainwire-core codec`
Expected: compile errors, `Row` is not defined.

- [ ] **Step 3: Implement**

`crates/core/src/value.rs`:

```rust
//! Property values. Temporal types, duration and point arrive with Cypher (plan 2).

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Value>),
    Map(BTreeMap<String, Value>),
}
```

Top of `crates/core/src/codec.rs`:

```rust
//! Row codec for node, edge and event rows (spec 1.2). Columns are keyed by
//! column id, so no property name is stored per row; properties outside the
//! ontology go to the `extra` map.

use std::collections::BTreeMap;

use crate::value::Value;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Row {
    pub schema_version: u32,
    pub acl_set: u32,
    pub columns: BTreeMap<u16, Value>,
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CodecError {
    #[error("row is truncated")]
    Truncated,
    #[error("unknown value tag {0}")]
    UnknownTag(u8),
    #[error("invalid UTF-8 in a string")]
    BadUtf8,
    #[error("varint overflow")]
    VarintOverflow,
}

const T_NULL: u8 = 0;
const T_FALSE: u8 = 1;
const T_TRUE: u8 = 2;
const T_INT: u8 = 3;
const T_FLOAT: u8 = 4;
const T_STR: u8 = 5;
const T_LIST: u8 = 6;
const T_MAP: u8 = 7;

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn get_varint(input: &mut &[u8]) -> Result<u64, CodecError> {
    let mut result = 0u64;
    for shift in (0..64).step_by(7) {
        let (&byte, rest) = input.split_first().ok_or(CodecError::Truncated)?;
        *input = rest;
        result |= u64::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return Ok(result);
        }
    }
    Err(CodecError::VarintOverflow)
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    put_varint(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

fn get_str(input: &mut &[u8]) -> Result<String, CodecError> {
    let len = get_varint(input)? as usize;
    if input.len() < len {
        return Err(CodecError::Truncated);
    }
    let (head, rest) = input.split_at(len);
    *input = rest;
    String::from_utf8(head.to_vec()).map_err(|_| CodecError::BadUtf8)
}

fn put_value(out: &mut Vec<u8>, v: &Value) {
    match v {
        Value::Null => out.push(T_NULL),
        Value::Bool(false) => out.push(T_FALSE),
        Value::Bool(true) => out.push(T_TRUE),
        Value::Int(i) => {
            out.push(T_INT);
            put_varint(out, ((i << 1) ^ (i >> 63)) as u64);
        }
        Value::Float(f) => {
            out.push(T_FLOAT);
            out.extend_from_slice(&f.to_le_bytes());
        }
        Value::Str(s) => {
            out.push(T_STR);
            put_str(out, s);
        }
        Value::List(items) => {
            out.push(T_LIST);
            put_varint(out, items.len() as u64);
            for item in items {
                put_value(out, item);
            }
        }
        Value::Map(map) => {
            out.push(T_MAP);
            put_varint(out, map.len() as u64);
            for (k, item) in map {
                put_str(out, k);
                put_value(out, item);
            }
        }
    }
}

fn get_value(input: &mut &[u8]) -> Result<Value, CodecError> {
    let (&tag, rest) = input.split_first().ok_or(CodecError::Truncated)?;
    *input = rest;
    Ok(match tag {
        T_NULL => Value::Null,
        T_FALSE => Value::Bool(false),
        T_TRUE => Value::Bool(true),
        T_INT => {
            let z = get_varint(input)?;
            Value::Int(((z >> 1) as i64) ^ -((z & 1) as i64))
        }
        T_FLOAT => {
            if input.len() < 8 {
                return Err(CodecError::Truncated);
            }
            let (head, rest) = input.split_at(8);
            *input = rest;
            Value::Float(f64::from_le_bytes(head.try_into().expect("length checked")))
        }
        T_STR => Value::Str(get_str(input)?),
        T_LIST => {
            let n = get_varint(input)?;
            let mut items = Vec::new();
            for _ in 0..n {
                items.push(get_value(input)?);
            }
            Value::List(items)
        }
        T_MAP => {
            let n = get_varint(input)?;
            let mut map = BTreeMap::new();
            for _ in 0..n {
                let k = get_str(input)?;
                map.insert(k, get_value(input)?);
            }
            Value::Map(map)
        }
        other => return Err(CodecError::UnknownTag(other)),
    })
}

pub fn encode_row(row: &Row) -> Vec<u8> {
    let mut out = Vec::new();
    put_varint(&mut out, u64::from(row.schema_version));
    put_varint(&mut out, u64::from(row.acl_set));
    put_varint(&mut out, row.columns.len() as u64);
    for (id, v) in &row.columns {
        put_varint(&mut out, u64::from(*id));
        put_value(&mut out, v);
    }
    put_varint(&mut out, row.extra.len() as u64);
    for (k, v) in &row.extra {
        put_str(&mut out, k);
        put_value(&mut out, v);
    }
    out
}

pub fn decode_row(bytes: &[u8]) -> Result<Row, CodecError> {
    let mut input = bytes;
    let schema_version = get_varint(&mut input)? as u32;
    let acl_set = get_varint(&mut input)? as u32;
    let mut columns = BTreeMap::new();
    for _ in 0..get_varint(&mut input)? {
        let id = get_varint(&mut input)? as u16;
        columns.insert(id, get_value(&mut input)?);
    }
    let mut extra = BTreeMap::new();
    for _ in 0..get_varint(&mut input)? {
        let k = get_str(&mut input)?;
        extra.insert(k, get_value(&mut input)?);
    }
    Ok(Row { schema_version, acl_set, columns, extra })
}
```

Add `pub mod value;` and `pub mod codec;` to `crates/core/src/lib.rs`.

Note for the `unknown_tag_is_an_error` test: the bytes are schema_version 1,
acl_set 0, one column, column id 0, then tag 99, which matches the encoding
above.

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p brainwire-core codec`
Expected: `empty_row_round_trips`, `unknown_tag_is_an_error`,
`truncated_row_is_an_error` and `rows_round_trip` pass.

- [ ] **Step 5: Commit**

```bash
git add external-brainwire/crates/core
git commit -m "feat(brainwire): add values and the row codec"
```

## Task 5: Catalog from the ontology

**Files:**
- Create: `crates/core/src/schema.rs`
- Create: `crates/core/tests/fixtures/graph_mapping.json`
- Modify: `crates/core/src/ids.rs` (add `serde::Serialize, serde::Deserialize`
  to the derives of `TypeId` and `EdgeTypeId`)
- Modify: `crates/core/src/lib.rs` (add `pub mod schema;`)

**Interfaces:**
- Consumes: `TypeId`, `EdgeTypeId` (Task 3).
- Produces:
  - `pub struct GraphMapping { pub version: String, pub nodes: Vec<NodeSpec>, pub edges: Vec<EdgeSpec>, pub timeline: Vec<TimelineSpec> }`
    deserialized from the JSON of the platform's `GraphMappingConfig`
    (`model_dump_json(by_alias=True)`, so an edge's endpoints are `from` and `to`)
  - `pub struct Catalog { pub version: u32, pub node_types: Vec<NodeType>, pub edge_types: Vec<EdgeType>, pub streams: Vec<Stream> }`
    (serde Serialize/Deserialize, stored as JSON in `meta`)
  - `pub struct NodeType { pub id: TypeId, pub label: String, pub columns: Vec<Column>, pub indexed: Vec<String>, pub identifiers: Vec<(String, String)>, pub display_color: Option<String> }`
    with `fn column(&self, name: &str) -> Option<u16>`
  - `pub struct EdgeType { pub id: EdgeTypeId, pub name: String, pub signatures: Vec<(TypeId, TypeId)>, pub columns: Vec<Column> }`
  - `pub struct Stream { pub id: u16, pub event_type: String, pub owner: TypeId, pub timestamp_field: String, pub columns: Vec<Column> }`
  - `pub struct Column { pub id: u16, pub name: String }`
  - `Catalog::apply(mapping: &GraphMapping, previous: Option<&Catalog>) -> Result<Catalog, SchemaError>`,
    `Catalog::node_type(&self, label: &str) -> Option<&NodeType>`,
    `Catalog::edge_type(&self, name: &str) -> Option<&EdgeType>`
  - `pub enum SchemaError { UnknownLabel(String), DuplicateLabel(String), TooManyTypes, BadEntityJoin(String) }`

Rules: type ids start at 1 (0 is untyped); ids of labels, edge types, streams
and columns stay stable across applications and new ones are appended; columns
are never removed, so old rows stay decodable; properties whose name starts
with `acl_` never become columns; `indexed` is `er_fields` followed by
`indexed_fields`, without duplicates.

- [ ] **Step 1: Write the fixture**

`crates/core/tests/fixtures/graph_mapping.json` (synthetic):

```json
{
  "version": "1",
  "nodes": [
    {
      "label": "Customer",
      "properties": ["name", "email", "phone", "cpf", "segment", "acl_allow"],
      "er_fields": ["email", "phone"],
      "indexed_fields": ["segment", "email"],
      "identifier_columns": {"email": "email", "cpf": "cpf"},
      "display_color": "#3B82F6"
    },
    {
      "label": "Company",
      "properties": ["name", "cnpj", "industry"],
      "indexed_fields": ["industry"],
      "identifier_columns": {"cnpj": "cnpj"}
    },
    {"label": "Segment", "properties": ["name"]}
  ],
  "edges": [
    {"type": "WORKS_AT", "from": {"label": "Customer"}, "to": {"label": "Company"}, "properties": ["role"]},
    {"type": "IN_SEGMENT", "from": {"label": "Company"}, "to": {"label": "Segment"}},
    {"type": "IN_SEGMENT", "from": {"label": "Customer"}, "to": {"label": "Segment"}}
  ],
  "timeline": [
    {
      "event_type": "interaction",
      "entity_join": "customer_email -> Customer.email",
      "timestamp_field": "created_at",
      "properties": ["ticket_id", "channel", "status"]
    }
  ]
}
```

- [ ] **Step 2: Write the failing tests**

At the bottom of `crates/core/src/schema.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> GraphMapping {
        serde_json::from_str(include_str!("../tests/fixtures/graph_mapping.json")).unwrap()
    }

    #[test]
    fn builds_the_catalog() {
        let cat = Catalog::apply(&fixture(), None).unwrap();
        assert_eq!(cat.version, 1);
        let customer = cat.node_type("Customer").unwrap();
        assert_eq!(customer.id, TypeId(1));
        assert_eq!(cat.node_type("Company").unwrap().id, TypeId(2));
        assert!(customer.column("acl_allow").is_none());
        assert_eq!(customer.indexed, vec!["email", "phone", "segment"]);
        assert_eq!(customer.identifiers.len(), 2);
        let in_segment = cat.edge_type("IN_SEGMENT").unwrap();
        assert_eq!(in_segment.signatures, vec![(TypeId(2), TypeId(3)), (TypeId(1), TypeId(3))]);
        assert_eq!(cat.streams[0].owner, TypeId(1));
    }

    #[test]
    fn ids_are_stable_across_applications() {
        let first = Catalog::apply(&fixture(), None).unwrap();
        let mut next = fixture();
        next.nodes.insert(
            0,
            serde_json::from_str(r#"{"label": "Product", "properties": ["sku"]}"#).unwrap(),
        );
        next.nodes[1].properties.insert(0, "nickname".into());
        let second = Catalog::apply(&next, Some(&first)).unwrap();
        assert_eq!(second.version, 2);
        assert_eq!(second.node_type("Customer").unwrap().id, TypeId(1));
        assert_eq!(second.node_type("Product").unwrap().id, TypeId(4));
        let customer = second.node_type("Customer").unwrap();
        assert_eq!(customer.column("name"), first.node_type("Customer").unwrap().column("name"));
        assert!(customer.column("nickname").is_some());
    }

    #[test]
    fn unknown_edge_label_is_an_error() {
        let mut m = fixture();
        m.edges[0].to.label = "Nope".into();
        assert_eq!(Catalog::apply(&m, None), Err(SchemaError::UnknownLabel("Nope".into())));
    }

    #[test]
    fn bad_entity_join_is_an_error() {
        let mut m = fixture();
        m.timeline[0].entity_join = "customer_email".into();
        assert!(matches!(Catalog::apply(&m, None), Err(SchemaError::BadEntityJoin(_))));
    }

    #[test]
    fn catalog_round_trips_as_json() {
        let cat = Catalog::apply(&fixture(), None).unwrap();
        let back: Catalog = serde_json::from_str(&serde_json::to_string(&cat).unwrap()).unwrap();
        assert_eq!(back, cat);
    }
}
```

- [ ] **Step 3: Run the tests to see them fail**

Run: `cargo test -p brainwire-core schema`
Expected: compile errors, `Catalog` is not defined.

- [ ] **Step 4: Implement**

Top of `crates/core/src/schema.rs`:

```rust
//! The ontology becomes the schema (spec 2.1). Input: the platform's
//! GraphMappingConfig as JSON. Output: a versioned catalog with stable ids.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ids::{EdgeTypeId, TypeId};

#[derive(Debug, Clone, Deserialize)]
pub struct GraphMapping {
    pub version: String,
    pub nodes: Vec<NodeSpec>,
    #[serde(default)]
    pub edges: Vec<EdgeSpec>,
    #[serde(default)]
    pub timeline: Vec<TimelineSpec>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NodeSpec {
    pub label: String,
    #[serde(default)]
    pub properties: Vec<String>,
    #[serde(default)]
    pub er_fields: Vec<String>,
    #[serde(default)]
    pub indexed_fields: Vec<String>,
    #[serde(default)]
    pub identifier_columns: BTreeMap<String, String>,
    #[serde(default)]
    pub display_color: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EdgeSpec {
    #[serde(rename = "type")]
    pub name: String,
    pub from: EndpointSpec,
    pub to: EndpointSpec,
    #[serde(default)]
    pub properties: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EndpointSpec {
    pub label: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TimelineSpec {
    pub event_type: String,
    pub entity_join: String,
    pub timestamp_field: String,
    #[serde(default)]
    pub properties: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    pub id: u16,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeType {
    pub id: TypeId,
    pub label: String,
    pub columns: Vec<Column>,
    pub indexed: Vec<String>,
    pub identifiers: Vec<(String, String)>,
    pub display_color: Option<String>,
}

impl NodeType {
    pub fn column(&self, name: &str) -> Option<u16> {
        self.columns.iter().find(|c| c.name == name).map(|c| c.id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdgeType {
    pub id: EdgeTypeId,
    pub name: String,
    pub signatures: Vec<(TypeId, TypeId)>,
    pub columns: Vec<Column>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stream {
    pub id: u16,
    pub event_type: String,
    pub owner: TypeId,
    pub timestamp_field: String,
    pub columns: Vec<Column>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    pub version: u32,
    pub node_types: Vec<NodeType>,
    pub edge_types: Vec<EdgeType>,
    pub streams: Vec<Stream>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SchemaError {
    #[error("unknown label {0}")]
    UnknownLabel(String),
    #[error("label {0} is declared twice")]
    DuplicateLabel(String),
    #[error("more than 4095 node types")]
    TooManyTypes,
    #[error("entity_join must look like 'column -> Label.property': {0}")]
    BadEntityJoin(String),
}

fn is_reserved(name: &str) -> bool {
    name.to_ascii_lowercase().starts_with("acl_")
}

/// Keeps the previous columns (so old rows decode) and appends new names.
fn merge_columns(previous: &[Column], names: &[String]) -> Vec<Column> {
    let mut cols = previous.to_vec();
    for name in names.iter().filter(|n| !is_reserved(n)) {
        if !cols.iter().any(|c| &c.name == name) {
            let id = cols.iter().map(|c| c.id + 1).max().unwrap_or(0);
            cols.push(Column { id, name: name.clone() });
        }
    }
    cols
}

fn owner_label(entity_join: &str) -> Result<String, SchemaError> {
    let bad = || SchemaError::BadEntityJoin(entity_join.to_string());
    let (_, target) = entity_join.split_once("->").ok_or_else(bad)?;
    let (label, _) = target.trim().split_once('.').ok_or_else(bad)?;
    if label.is_empty() {
        return Err(bad());
    }
    Ok(label.to_string())
}

impl Catalog {
    pub fn node_type(&self, label: &str) -> Option<&NodeType> {
        self.node_types.iter().find(|t| t.label == label)
    }

    pub fn edge_type(&self, name: &str) -> Option<&EdgeType> {
        self.edge_types.iter().find(|t| t.name == name)
    }

    pub fn apply(mapping: &GraphMapping, previous: Option<&Catalog>) -> Result<Catalog, SchemaError> {
        let empty = Catalog { version: 0, node_types: vec![], edge_types: vec![], streams: vec![] };
        let prev = previous.unwrap_or(&empty);

        let mut node_types = prev.node_types.clone();
        let mut seen = Vec::new();
        for spec in &mapping.nodes {
            if seen.contains(&spec.label) {
                return Err(SchemaError::DuplicateLabel(spec.label.clone()));
            }
            seen.push(spec.label.clone());
            let mut indexed: Vec<String> = Vec::new();
            for f in spec.er_fields.iter().chain(&spec.indexed_fields) {
                if !indexed.contains(f) && !is_reserved(f) {
                    indexed.push(f.clone());
                }
            }
            let identifiers: Vec<(String, String)> =
                spec.identifier_columns.iter().map(|(c, t)| (c.clone(), t.clone())).collect();
            match node_types.iter_mut().find(|t| t.label == spec.label) {
                Some(t) => {
                    t.columns = merge_columns(&t.columns, &spec.properties);
                    t.indexed = indexed;
                    t.identifiers = identifiers;
                    t.display_color = spec.display_color.clone();
                }
                None => {
                    let next = node_types.iter().map(|t| t.id.0 + 1).max().unwrap_or(1);
                    if next > TypeId::MAX {
                        return Err(SchemaError::TooManyTypes);
                    }
                    node_types.push(NodeType {
                        id: TypeId(next),
                        label: spec.label.clone(),
                        columns: merge_columns(&[], &spec.properties),
                        indexed,
                        identifiers,
                        display_color: spec.display_color.clone(),
                    });
                }
            }
        }
        let type_of = |label: &str| {
            node_types
                .iter()
                .find(|t| t.label == label)
                .map(|t| t.id)
                .ok_or_else(|| SchemaError::UnknownLabel(label.to_string()))
        };

        let mut edge_types = prev.edge_types.clone();
        for spec in &mapping.edges {
            let sig = (type_of(&spec.from.label)?, type_of(&spec.to.label)?);
            match edge_types.iter_mut().find(|t| t.name == spec.name) {
                Some(t) => {
                    if !t.signatures.contains(&sig) {
                        t.signatures.push(sig);
                    }
                    t.columns = merge_columns(&t.columns, &spec.properties);
                }
                None => {
                    let next = edge_types.iter().map(|t| t.id.0 + 1).max().unwrap_or(1);
                    edge_types.push(EdgeType {
                        id: EdgeTypeId(next),
                        name: spec.name.clone(),
                        signatures: vec![sig],
                        columns: merge_columns(&[], &spec.properties),
                    });
                }
            }
        }

        let mut streams = prev.streams.clone();
        for spec in &mapping.timeline {
            let owner = type_of(&owner_label(&spec.entity_join)?)?;
            match streams.iter_mut().find(|s| s.event_type == spec.event_type && s.owner == owner) {
                Some(s) => s.columns = merge_columns(&s.columns, &spec.properties),
                None => {
                    let next = streams.iter().map(|s| s.id + 1).max().unwrap_or(1);
                    streams.push(Stream {
                        id: next,
                        event_type: spec.event_type.clone(),
                        owner,
                        timestamp_field: spec.timestamp_field.clone(),
                        columns: merge_columns(&[], &spec.properties),
                    });
                }
            }
        }

        Ok(Catalog { version: prev.version + 1, node_types, edge_types, streams })
    }
}
```

In `crates/core/src/ids.rs`, change the derives of `TypeId` and `EdgeTypeId` to:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
```

Add `pub mod schema;` to `crates/core/src/lib.rs`.

- [ ] **Step 5: Run the tests to see them pass**

Run: `cargo test -p brainwire-core schema`
Expected: the five `schema::tests` pass.

- [ ] **Step 6: Commit**

```bash
git add external-brainwire/crates/core
git commit -m "feat(brainwire): build the catalog from the platform ontology"
```

## Task 6: RocksDB database with the column families

**Files:**
- Create: `crates/store/src/db.rs`
- Modify: `crates/store/src/lib.rs` (add `pub mod db;`)

**Interfaces:**
- Produces:
  - `pub const COLUMN_FAMILIES: [&str; 11]`
  - `pub struct StoreConfig { pub cache_bytes: usize, pub memtable_bytes: usize, pub id_block: u64 }`
    with `StoreConfig::for_tests()` (64 MB cache, 16 MB memtables, id block 4)
  - `pub struct Resources` (one per process: the shared cache and memtable
    manager) with `Resources::new(cfg: StoreConfig) -> Resources`
  - `pub struct Database` with `Database::open(path: &Path, res: &Resources) -> Result<Database, StoreError>`
    and `pub(crate) fn cf(&self, name: &'static str) -> Result<&rocksdb::ColumnFamily, StoreError>`
  - `pub enum StoreError { Rocks(rocksdb::Error), MissingColumnFamily(&'static str), Codec(CodecError), Key(KeyError), Catalog(String), Corrupt(String) }`

This task sets only what the spec fixes for every family (shared cache, 16 KB
blocks, LZ4 on upper levels and ZSTD at the bottom, one memtable budget). The
per-family filters and prefix extractors of spec 1.6 belong to the scale plan,
where they are measured; without a prefix extractor every iterator is total
order, so no read here depends on them. If a `rocksdb` 0.25 function name
differs from the code below, use the equivalent from docs.rs/rocksdb/0.25 and
keep the behavior.

- [ ] **Step 1: Write the failing tests**

At the bottom of `crates/store/src/db.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_every_column_family() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        drop(Database::open(dir.path(), &res).unwrap());
        let names = rocksdb::DB::list_cf(&rocksdb::Options::default(), dir.path()).unwrap();
        for cf in COLUMN_FAMILIES {
            assert!(names.iter().any(|n| n == cf), "missing {cf}");
        }
    }

    #[test]
    fn data_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        {
            let db = Database::open(dir.path(), &res).unwrap();
            db.db.put_cf(db.cf("node").unwrap(), b"k", b"v").unwrap();
        }
        let db = Database::open(dir.path(), &res).unwrap();
        assert_eq!(db.db.get_cf(db.cf("node").unwrap(), b"k").unwrap(), Some(b"v".to_vec()));
    }

    #[test]
    fn two_databases_share_one_resources() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let res = Resources::new(StoreConfig::for_tests());
        let da = Database::open(a.path(), &res).unwrap();
        let db = Database::open(b.path(), &res).unwrap();
        da.db.put_cf(da.cf("meta").unwrap(), b"x", b"1").unwrap();
        assert_eq!(db.db.get_cf(db.cf("meta").unwrap(), b"x").unwrap(), None);
    }
}
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test -p brainwire-store db`
Expected: compile errors, `Database` is not defined.

- [ ] **Step 3: Implement**

Top of `crates/store/src/db.rs`:

```rust
//! One RocksDB per database, with process-wide cache and memtable budget
//! (spec 1.1, 1.2, 1.5).

use std::path::Path;

use brainwire_core::codec::CodecError;
use brainwire_core::keys::KeyError;
use rocksdb::{BlockBasedOptions, Cache, ColumnFamilyDescriptor, DBCompressionType, Options, WriteBufferManager, DB};

pub const COLUMN_FAMILIES: [&str; 11] =
    ["meta", "node", "pk", "out", "in", "lbl", "idx", "ident", "event", "acl", "note"];

#[derive(Debug, Clone, Copy)]
pub struct StoreConfig {
    pub cache_bytes: usize,
    pub memtable_bytes: usize,
    pub id_block: u64,
}

impl StoreConfig {
    pub fn for_tests() -> StoreConfig {
        StoreConfig { cache_bytes: 64 << 20, memtable_bytes: 16 << 20, id_block: 4 }
    }
}

/// Shared by every database of the process, so ten databases fit the budget of one.
pub struct Resources {
    pub(crate) config: StoreConfig,
    cache: Cache,
    write_buffers: WriteBufferManager,
}

impl Resources {
    pub fn new(config: StoreConfig) -> Resources {
        let cache = Cache::new_hyper_clock_cache(config.cache_bytes, 0);
        // allow_stall = false: one full family must not stall the other databases (spec 1.5).
        let write_buffers =
            WriteBufferManager::new_write_buffer_manager_with_cache(config.memtable_bytes, false, cache.clone());
        Resources { config, cache, write_buffers }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Rocks(#[from] rocksdb::Error),
    #[error("column family {0} is missing")]
    MissingColumnFamily(&'static str),
    #[error(transparent)]
    Codec(#[from] CodecError),
    #[error(transparent)]
    Key(#[from] KeyError),
    #[error("catalog: {0}")]
    Catalog(String),
    #[error("corrupt data: {0}")]
    Corrupt(String),
}

pub struct Database {
    pub(crate) db: DB,
}

fn family_options(res: &Resources) -> Options {
    let mut block = BlockBasedOptions::default();
    block.set_block_cache(&res.cache);
    block.set_block_size(16 * 1024);
    let mut opts = Options::default();
    opts.set_block_based_table_factory(&block);
    opts.set_compression_type(DBCompressionType::Lz4);
    opts.set_bottommost_compression_type(DBCompressionType::Zstd);
    opts
}

impl Database {
    pub fn open(path: &Path, res: &Resources) -> Result<Database, StoreError> {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);
        opts.set_write_buffer_manager(&res.write_buffers);
        let families = COLUMN_FAMILIES
            .iter()
            .map(|name| ColumnFamilyDescriptor::new(*name, family_options(res)));
        let db = DB::open_cf_descriptors(&opts, path, families)?;
        Ok(Database { db })
    }

    pub(crate) fn cf(&self, name: &'static str) -> Result<&rocksdb::ColumnFamily, StoreError> {
        self.db.cf_handle(name).ok_or(StoreError::MissingColumnFamily(name))
    }
}
```

Add `pub mod db;` to `crates/store/src/lib.rs`.

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p brainwire-store db`
Expected: the three `db::tests` pass.

- [ ] **Step 5: Commit**

```bash
git add external-brainwire/crates/store
git commit -m "feat(brainwire): open one RocksDB per database with shared resources"
```

## Task 7: Transactions with first-committer-wins

**Files:**
- Create: `crates/store/src/txn.rs`
- Modify: `crates/store/src/db.rs` (add the `committer` field and `begin`)
- Modify: `crates/store/src/lib.rs` (add `pub mod txn;`)

**Interfaces:**
- Consumes: `Database`, `StoreError` (Task 6).
- Produces:
  - `pub enum Cf { Meta, Node, Pk, Out, In, Lbl, Idx, Ident, Event, Acl, Note }`
    with `fn name(self) -> &'static str`
  - `Database::begin(&self) -> Txn<'_>`
  - `Txn::get(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError>`
    (overlay first, then the snapshot)
  - `Txn::get_for_update(&mut self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError>`
    (same read, and the key becomes a dependency checked at commit)
  - `Txn::put(&mut self, cf: Cf, key: Vec<u8>, value: Vec<u8>)`,
    `Txn::delete(&mut self, cf: Cf, key: Vec<u8>)`
  - `Txn::scan_prefix(&self, cf: Cf, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StoreError>`
    (snapshot merged with the overlay, deletes applied, key order)
  - `Txn::commit(self) -> Result<u64, TxnError>` returning the commit sequence
  - `Txn::database(&self) -> &Database`
  - `pub enum TxnError { Conflict, Store(StoreError) }`

Rules (spec 1.4): reads come from the snapshot taken at `begin`; a commit fails
with `Conflict` when any key it wrote or depended on was written by a
transaction that committed after this one began; the batch is written with the
WAL synced; a dropped transaction leaves no trace.

- [ ] **Step 1: Write the failing tests**

At the bottom of `crates/store/src/txn.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, Resources, StoreConfig};

    fn open() -> (tempfile::TempDir, Resources, Database) {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        let db = Database::open(dir.path(), &res).unwrap();
        (dir, res, db)
    }

    #[test]
    fn reads_its_own_writes() {
        let (_d, _r, db) = open();
        let mut t = db.begin();
        t.put(Cf::Node, b"a".to_vec(), b"1".to_vec());
        assert_eq!(t.get(Cf::Node, b"a").unwrap(), Some(b"1".to_vec()));
    }

    #[test]
    fn reads_come_from_the_snapshot() {
        let (_d, _r, db) = open();
        let reader = db.begin();
        let mut writer = db.begin();
        writer.put(Cf::Node, b"a".to_vec(), b"1".to_vec());
        writer.commit().unwrap();
        assert_eq!(reader.get(Cf::Node, b"a").unwrap(), None);
        assert_eq!(db.begin().get(Cf::Node, b"a").unwrap(), Some(b"1".to_vec()));
    }

    #[test]
    fn first_committer_wins_on_a_dependency() {
        let (_d, _r, db) = open();
        let mut t1 = db.begin();
        let mut t2 = db.begin();
        assert_eq!(t1.get_for_update(Cf::Pk, b"user:7").unwrap(), None);
        assert_eq!(t2.get_for_update(Cf::Pk, b"user:7").unwrap(), None);
        t1.put(Cf::Pk, b"user:7".to_vec(), b"id1".to_vec());
        t2.put(Cf::Pk, b"user:7".to_vec(), b"id2".to_vec());
        t1.commit().unwrap();
        assert!(matches!(t2.commit(), Err(TxnError::Conflict)));
        assert_eq!(db.begin().get(Cf::Pk, b"user:7").unwrap(), Some(b"id1".to_vec()));
    }

    #[test]
    fn disjoint_writes_both_commit() {
        let (_d, _r, db) = open();
        let mut t1 = db.begin();
        let mut t2 = db.begin();
        t1.put(Cf::Node, b"a".to_vec(), b"1".to_vec());
        t2.put(Cf::Node, b"b".to_vec(), b"2".to_vec());
        t1.commit().unwrap();
        t2.commit().unwrap();
    }

    #[test]
    fn scan_merges_the_overlay_and_applies_deletes() {
        let (_d, _r, db) = open();
        let mut seed = db.begin();
        seed.put(Cf::Out, b"p:1".to_vec(), b"x".to_vec());
        seed.put(Cf::Out, b"p:2".to_vec(), b"y".to_vec());
        seed.put(Cf::Out, b"q:1".to_vec(), b"z".to_vec());
        seed.commit().unwrap();
        let mut t = db.begin();
        t.delete(Cf::Out, b"p:1".to_vec());
        t.put(Cf::Out, b"p:3".to_vec(), b"w".to_vec());
        let got = t.scan_prefix(Cf::Out, b"p:").unwrap();
        assert_eq!(got, vec![(b"p:2".to_vec(), b"y".to_vec()), (b"p:3".to_vec(), b"w".to_vec())]);
    }

    #[test]
    fn a_dropped_transaction_leaves_no_trace() {
        let (_d, _r, db) = open();
        let mut t = db.begin();
        t.put(Cf::Node, b"a".to_vec(), b"1".to_vec());
        drop(t);
        assert_eq!(db.begin().get(Cf::Node, b"a").unwrap(), None);
        let mut again = db.begin();
        again.put(Cf::Node, b"a".to_vec(), b"2".to_vec());
        again.commit().unwrap();
    }
}
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test -p brainwire-store txn`
Expected: compile errors, `Cf` and `begin` are not defined.

- [ ] **Step 3: Implement**

Top of `crates/store/src/txn.rs`:

```rust
//! Snapshot-isolated transactions with an in-house write overlay and a
//! committer that lets the first committer win (spec 1.4). The crate does not
//! expose WriteBatchWithIndex, hence the overlay.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Mutex;

use crate::db::{Database, StoreError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Cf {
    Meta,
    Node,
    Pk,
    Out,
    In,
    Lbl,
    Idx,
    Ident,
    Event,
    Acl,
    Note,
}

impl Cf {
    pub fn name(self) -> &'static str {
        match self {
            Cf::Meta => "meta",
            Cf::Node => "node",
            Cf::Pk => "pk",
            Cf::Out => "out",
            Cf::In => "in",
            Cf::Lbl => "lbl",
            Cf::Idx => "idx",
            Cf::Ident => "ident",
            Cf::Event => "event",
            Cf::Acl => "acl",
            Cf::Note => "note",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TxnError {
    #[error("conflict with a transaction that committed first")]
    Conflict,
    #[error(transparent)]
    Store(#[from] StoreError),
}

type Key = (Cf, Vec<u8>);

#[derive(Default)]
pub(crate) struct Committer {
    state: Mutex<CommitState>,
}

#[derive(Default)]
struct CommitState {
    seq: u64,
    last_write: HashMap<Key, u64>,
    active: BTreeMap<u64, usize>,
}

impl CommitState {
    fn release(&mut self, start: u64) {
        if let Some(n) = self.active.get_mut(&start) {
            *n -= 1;
            if *n == 0 {
                self.active.remove(&start);
            }
        }
        // A key last written at or before the oldest active start cannot conflict any more.
        if self.last_write.len() > 100_000 {
            let horizon = self.active.keys().next().copied().unwrap_or(self.seq);
            self.last_write.retain(|_, s| *s > horizon);
        }
    }
}

pub struct Txn<'db> {
    db: &'db Database,
    snapshot: rocksdb::Snapshot<'db>,
    start_seq: u64,
    writes: BTreeMap<Key, Option<Vec<u8>>>,
    depends: BTreeSet<Key>,
    done: bool,
}

impl Database {
    pub fn begin(&self) -> Txn<'_> {
        let mut st = self.committer.state.lock().expect("committer lock");
        let start_seq = st.seq;
        *st.active.entry(start_seq).or_insert(0) += 1;
        // Taken under the lock, so no commit lands between the sequence and the snapshot.
        let snapshot = self.db.snapshot();
        drop(st);
        Txn { db: self, snapshot, start_seq, writes: BTreeMap::new(), depends: BTreeSet::new(), done: false }
    }
}

impl<'db> Txn<'db> {
    pub fn database(&self) -> &'db Database {
        self.db
    }

    pub fn get(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        if let Some(v) = self.writes.get(&(cf, key.to_vec())) {
            return Ok(v.clone());
        }
        Ok(self.snapshot.get_cf(self.db.cf(cf.name())?, key)?)
    }

    pub fn get_for_update(&mut self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        self.depends.insert((cf, key.to_vec()));
        self.get(cf, key)
    }

    pub fn put(&mut self, cf: Cf, key: Vec<u8>, value: Vec<u8>) {
        self.writes.insert((cf, key), Some(value));
    }

    pub fn delete(&mut self, cf: Cf, key: Vec<u8>) {
        self.writes.insert((cf, key), None);
    }

    pub fn scan_prefix(&self, cf: Cf, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StoreError> {
        let mut merged: BTreeMap<Vec<u8>, Option<Vec<u8>>> = BTreeMap::new();
        let handle = self.db.cf(cf.name())?;
        let mode = rocksdb::IteratorMode::From(prefix, rocksdb::Direction::Forward);
        for item in self.snapshot.iterator_cf(handle, mode) {
            let (k, v) = item?;
            if !k.starts_with(prefix) {
                break;
            }
            merged.insert(k.to_vec(), Some(v.to_vec()));
        }
        for ((c, k), v) in self.writes.range((cf, prefix.to_vec())..) {
            if *c != cf || !k.starts_with(prefix) {
                break;
            }
            merged.insert(k.clone(), v.clone());
        }
        Ok(merged.into_iter().filter_map(|(k, v)| v.map(|v| (k, v))).collect())
    }

    pub fn commit(mut self) -> Result<u64, TxnError> {
        let db = self.db;
        let mut st = db.committer.state.lock().expect("committer lock");
        let start = self.start_seq;
        let conflict = self
            .writes
            .keys()
            .chain(self.depends.iter())
            .any(|k| st.last_write.get(k).is_some_and(|&s| s > start));
        if conflict {
            st.release(start);
            self.done = true;
            return Err(TxnError::Conflict);
        }
        if !self.writes.is_empty() {
            let mut batch = rocksdb::WriteBatch::default();
            for ((cf, key), value) in &self.writes {
                let handle = db.cf(cf.name())?;
                match value {
                    Some(v) => batch.put_cf(handle, key, v),
                    None => batch.delete_cf(handle, key),
                }
            }
            let mut opts = rocksdb::WriteOptions::default();
            opts.set_sync(true);
            db.db.write_opt(batch, &opts).map_err(StoreError::from)?;
            st.seq += 1;
            let seq = st.seq;
            for key in self.writes.keys() {
                st.last_write.insert(key.clone(), seq);
            }
        }
        let seq = st.seq;
        st.release(start);
        self.done = true;
        Ok(seq)
    }
}

impl Drop for Txn<'_> {
    fn drop(&mut self) {
        if !self.done {
            if let Ok(mut st) = self.db.committer.state.lock() {
                st.release(self.start_seq);
            }
        }
    }
}
```

In `crates/store/src/db.rs`, the struct and `open` become:

```rust
pub struct Database {
    pub(crate) db: DB,
    pub(crate) committer: crate::txn::Committer,
}
```

and the last line of `Database::open` becomes
`Ok(Database { db, committer: crate::txn::Committer::default() })`.

Add `pub mod txn;` to `crates/store/src/lib.rs`.

Note: an early return through `?` inside `commit` drops the lock guard first
and the transaction afterwards, so `Drop` can take the lock again without a
deadlock.

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p brainwire-store`
Expected: the `db::tests` and the six `txn::tests` pass.

- [ ] **Step 5: Commit**

```bash
git add external-brainwire/crates/store
git commit -m "feat(brainwire): add snapshot transactions where the first committer wins"
```

## Task 8: Id allocation in blocks

**Files:**
- Create: `crates/store/src/alloc.rs`
- Modify: `crates/store/src/db.rs` (add the `alloc` and `id_block` fields)
- Modify: `crates/store/src/lib.rs` (add `pub mod alloc;`)

**Interfaces:**
- Consumes: `Database::begin`, `Txn::get_for_update`, `Txn::commit`, `Cf::Meta`
  (Task 7); `NodeId`, `EdgeId`, `TypeId` (Task 3).
- Produces:
  - `Database::next_node_id(&self, ty: TypeId) -> Result<NodeId, StoreError>`
  - `Database::next_edge_id(&self) -> Result<EdgeId, StoreError>` (starts at 1,
    since 0 is `EdgeId::MERGED`)
  - counters in `meta` under `seq/node/<type as u16 BE>` and `seq/edge`,
    holding the end of the last reserved block as u64 BE

Rule (spec 1.3): a block's end is committed before any id in it is handed out,
so a crash skips the rest of the block and never reuses an id.

- [ ] **Step 1: Write the failing tests**

At the bottom of `crates/store/src/alloc.rs`:

```rust
#[cfg(test)]
mod tests {
    use brainwire_core::ids::{EdgeId, NodeId, TypeId};

    use crate::db::{Database, Resources, StoreConfig};

    #[test]
    fn ids_grow_and_never_repeat_across_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        let db = Database::open(dir.path(), &res).unwrap();
        let ids: Vec<NodeId> = (0..10).map(|_| db.next_node_id(TypeId(1)).unwrap()).collect();
        assert!(ids.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn a_reopen_never_reuses_ids() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        let first = Database::open(dir.path(), &res).unwrap().next_node_id(TypeId(1)).unwrap();
        let second = Database::open(dir.path(), &res).unwrap().next_node_id(TypeId(1)).unwrap();
        assert!(second.seq() >= first.seq() + StoreConfig::for_tests().id_block);
    }

    #[test]
    fn edge_ids_start_at_one() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        let db = Database::open(dir.path(), &res).unwrap();
        assert_eq!(db.next_edge_id().unwrap(), EdgeId::new(0, 1).unwrap());
    }

    #[test]
    fn concurrent_allocation_is_unique() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        let db = Database::open(dir.path(), &res).unwrap();
        let mut all: Vec<NodeId> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..4)
                .map(|_| s.spawn(|| (0..50).map(|_| db.next_node_id(TypeId(2)).unwrap()).collect::<Vec<_>>()))
                .collect();
            handles.into_iter().flat_map(|h| h.join().unwrap()).collect()
        });
        all.sort();
        all.dedup();
        assert_eq!(all.len(), 200);
    }
}
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test -p brainwire-store alloc`
Expected: compile errors, `next_node_id` is not defined.

- [ ] **Step 3: Implement**

Top of `crates/store/src/alloc.rs`:

```rust
//! Id allocation in blocks. The end of a block is committed before any id in
//! it is used, so a crash never reuses an id (spec 1.3).

use std::collections::HashMap;
use std::sync::Mutex;

use brainwire_core::ids::{EdgeId, NodeId, TypeId};

use crate::db::{Database, StoreError};
use crate::txn::{Cf, TxnError};

#[derive(Default)]
pub(crate) struct IdAllocator {
    blocks: Mutex<HashMap<Vec<u8>, Block>>,
}

#[derive(Clone, Copy)]
struct Block {
    next: u64,
    end: u64,
}

const EDGE_COUNTER_KEY: &[u8] = b"seq/edge";

pub(crate) fn node_counter_key(ty: TypeId) -> Vec<u8> {
    let mut key = b"seq/node/".to_vec();
    key.extend_from_slice(&ty.0.to_be_bytes());
    key
}

impl Database {
    pub fn next_node_id(&self, ty: TypeId) -> Result<NodeId, StoreError> {
        let seq = self.next_seq(node_counter_key(ty), 0)?;
        NodeId::new(0, ty, seq).map_err(|e| StoreError::Corrupt(e.to_string()))
    }

    pub fn next_edge_id(&self) -> Result<EdgeId, StoreError> {
        let seq = self.next_seq(EDGE_COUNTER_KEY.to_vec(), 1)?;
        EdgeId::new(0, seq).map_err(|e| StoreError::Corrupt(e.to_string()))
    }

    fn next_seq(&self, key: Vec<u8>, first: u64) -> Result<u64, StoreError> {
        let mut blocks = self.alloc.blocks.lock().expect("allocator lock");
        let exhausted = blocks.get(&key).map_or(true, |b| b.next >= b.end);
        if exhausted {
            let block = self.reserve(&key, first)?;
            blocks.insert(key.clone(), block);
        }
        let block = blocks.get_mut(&key).expect("block present");
        let seq = block.next;
        block.next += 1;
        Ok(seq)
    }

    fn reserve(&self, key: &[u8], first: u64) -> Result<Block, StoreError> {
        loop {
            let mut txn = self.begin();
            let current = match txn.get_for_update(Cf::Meta, key)? {
                Some(v) => u64::from_be_bytes(
                    v.as_slice().try_into().map_err(|_| StoreError::Corrupt("id counter".into()))?,
                ),
                None => first,
            };
            let end = current + self.id_block;
            txn.put(Cf::Meta, key.to_vec(), end.to_be_bytes().to_vec());
            match txn.commit() {
                Ok(_) => return Ok(Block { next: current, end }),
                Err(TxnError::Conflict) => continue,
                Err(TxnError::Store(e)) => return Err(e),
            }
        }
    }
}
```

In `crates/store/src/db.rs`, the struct becomes:

```rust
pub struct Database {
    pub(crate) db: DB,
    pub(crate) committer: crate::txn::Committer,
    pub(crate) alloc: crate::alloc::IdAllocator,
    pub(crate) id_block: u64,
}
```

and the last line of `Database::open` becomes:

```rust
Ok(Database {
    db,
    committer: crate::txn::Committer::default(),
    alloc: crate::alloc::IdAllocator::default(),
    id_block: res.config.id_block,
})
```

Add `pub mod alloc;` to `crates/store/src/lib.rs`. Lock order is allocator
first, committer second, and the committer never takes the allocator lock, so
there is no deadlock.

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p brainwire-store`
Expected: every test so far passes, including the four `alloc::tests`.

- [ ] **Step 5: Commit**

```bash
git add external-brainwire/crates/store
git commit -m "feat(brainwire): allocate ids in blocks that survive crashes"
```

## Task 9: Catalog persistence and graph operations

**Files:**
- Create: `crates/store/src/graph.rs`
- Modify: `crates/store/Cargo.toml` (add `serde_json.workspace = true` to
  `[dependencies]`)
- Modify: `crates/store/src/lib.rs` (add `pub mod graph;`)

**Interfaces:**
- Consumes: everything above.
- Produces:
  - `Database::apply_ontology(&self, mapping: &GraphMapping) -> Result<Catalog, GraphError>`
    (stores the catalog as JSON in `meta` under `catalog`)
  - `Database::catalog(&self) -> Result<Option<Catalog>, StoreError>`
  - `pub struct NodeRecord { pub id: NodeId, pub entity_id: String, pub row: Row }`
  - `pub struct EdgeRef { pub edge_type: EdgeTypeId, pub other: NodeId, pub id: EdgeId }`
  - `pub fn merge_node(txn: &mut Txn, catalog: &Catalog, label: &str, entity_id: &str, props: &BTreeMap<String, Value>) -> Result<NodeId, GraphError>`
  - `pub fn get_node(txn: &Txn, id: NodeId) -> Result<Option<NodeRecord>, GraphError>`
  - `pub fn find_by_entity_id(txn: &Txn, entity_id: &str, label: Option<TypeId>) -> Result<Vec<NodeId>, GraphError>`
  - `pub fn merge_edge(txn: &mut Txn, catalog: &Catalog, src: NodeId, edge_type: &str, dst: NodeId, props: &BTreeMap<String, Value>) -> Result<(), GraphError>`
  - `pub fn out_edges(txn: &Txn, src: NodeId) -> Result<Vec<EdgeRef>, GraphError>`,
    `pub fn in_edges(txn: &Txn, dst: NodeId) -> Result<Vec<EdgeRef>, GraphError>`
  - `pub fn detach_delete(txn: &mut Txn, id: NodeId) -> Result<(), GraphError>`
  - `pub fn check_invariants(db: &Database) -> Result<(), String>`
  - `pub enum GraphError { UnknownLabel(String), UnknownEdgeType(String), BadSignature(String), ReservedProperty(String), NodeNotFound(NodeId), Store(StoreError), Txn(TxnError) }`

Encodings: `node` key = node id (u64 BE); node value = entity_id length (u32
BE) + entity_id + encoded row. `pk` key = `put_bytes(entity_id)` +
`put_u16(type)`, value = node id (u64 BE). `out` key = source · edge type ·
target · edge id, value = encoded row; `in` key = target · edge type · source ·
edge id, value empty. `SET x = NULL` (a `Value::Null` in `props`) removes the
property. Properties named `acl_*` are refused until the ACL plan lands.
Secondary indexes (`idx`, `ident`) come with the Cypher plan.

- [ ] **Step 1: Write the failing tests**

At the bottom of `crates/store/src/graph.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Resources, StoreConfig};

    fn setup() -> (tempfile::TempDir, Resources, Database, Catalog) {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        let db = Database::open(dir.path(), &res).unwrap();
        let mapping: GraphMapping =
            serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json")).unwrap();
        let cat = db.apply_ontology(&mapping).unwrap();
        (dir, res, db, cat)
    }

    fn props(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    #[test]
    fn merge_node_is_idempotent_and_null_removes() {
        let (_d, _r, db, cat) = setup();
        let mut t = db.begin();
        let a = merge_node(&mut t, &cat, "Customer", "user:7", &props(&[("name", Value::Str("Ana".into()))])).unwrap();
        let b = merge_node(&mut t, &cat, "Customer", "user:7", &props(&[("segment", Value::Str("smb".into())), ("name", Value::Null)])).unwrap();
        t.commit().unwrap();
        assert_eq!(a, b);
        let node = get_node(&db.begin(), a).unwrap().unwrap();
        let customer = cat.node_type("Customer").unwrap();
        assert_eq!(node.row.columns.get(&customer.column("segment").unwrap()), Some(&Value::Str("smb".into())));
        assert!(node.row.columns.get(&customer.column("name").unwrap()).is_none());
    }

    #[test]
    fn finds_by_entity_id_with_and_without_label() {
        let (_d, _r, db, cat) = setup();
        let mut t = db.begin();
        merge_node(&mut t, &cat, "Customer", "user:7", &BTreeMap::new()).unwrap();
        merge_node(&mut t, &cat, "Company", "user:7", &BTreeMap::new()).unwrap();
        merge_node(&mut t, &cat, "Customer", "user:70", &BTreeMap::new()).unwrap();
        t.commit().unwrap();
        let r = db.begin();
        assert_eq!(find_by_entity_id(&r, "user:7", None).unwrap().len(), 2);
        let customer = cat.node_type("Customer").unwrap().id;
        assert_eq!(find_by_entity_id(&r, "user:7", Some(customer)).unwrap().len(), 1);
    }

    #[test]
    fn merge_edge_never_duplicates_and_mirrors() {
        let (_d, _r, db, cat) = setup();
        let mut t = db.begin();
        let c = merge_node(&mut t, &cat, "Customer", "c:1", &BTreeMap::new()).unwrap();
        let k = merge_node(&mut t, &cat, "Company", "k:1", &BTreeMap::new()).unwrap();
        merge_edge(&mut t, &cat, c, "WORKS_AT", k, &props(&[("role", Value::Str("cto".into()))])).unwrap();
        merge_edge(&mut t, &cat, c, "WORKS_AT", k, &BTreeMap::new()).unwrap();
        t.commit().unwrap();
        let r = db.begin();
        assert_eq!(out_edges(&r, c).unwrap().len(), 1);
        let back = in_edges(&r, k).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].other, c);
    }

    #[test]
    fn a_wrong_signature_is_refused() {
        let (_d, _r, db, cat) = setup();
        let mut t = db.begin();
        let c = merge_node(&mut t, &cat, "Customer", "c:1", &BTreeMap::new()).unwrap();
        let k = merge_node(&mut t, &cat, "Company", "k:1", &BTreeMap::new()).unwrap();
        assert!(matches!(
            merge_edge(&mut t, &cat, k, "WORKS_AT", c, &BTreeMap::new()),
            Err(GraphError::BadSignature(_))
        ));
    }

    #[test]
    fn detach_delete_removes_node_key_and_both_mirrors() {
        let (_d, _r, db, cat) = setup();
        let mut t = db.begin();
        let c = merge_node(&mut t, &cat, "Customer", "c:1", &BTreeMap::new()).unwrap();
        let k = merge_node(&mut t, &cat, "Company", "k:1", &BTreeMap::new()).unwrap();
        merge_edge(&mut t, &cat, c, "WORKS_AT", k, &BTreeMap::new()).unwrap();
        t.commit().unwrap();
        let mut t = db.begin();
        detach_delete(&mut t, c).unwrap();
        t.commit().unwrap();
        let r = db.begin();
        assert!(get_node(&r, c).unwrap().is_none());
        assert!(find_by_entity_id(&r, "c:1", None).unwrap().is_empty());
        assert!(in_edges(&r, k).unwrap().is_empty());
        check_invariants(&db).unwrap();
    }

    #[test]
    fn acl_properties_are_refused_for_now() {
        let (_d, _r, db, cat) = setup();
        let mut t = db.begin();
        let r = merge_node(&mut t, &cat, "Customer", "c:1", &props(&[("acl_allow", Value::Null)]));
        assert!(matches!(r, Err(GraphError::ReservedProperty(_))));
    }

    #[test]
    fn the_catalog_persists() {
        let (dir, res, db, _cat) = setup();
        let mapping: GraphMapping =
            serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json")).unwrap();
        assert_eq!(db.apply_ontology(&mapping).unwrap().version, 2);
        drop(db);
        let db = Database::open(dir.path(), &res).unwrap();
        assert_eq!(db.catalog().unwrap().unwrap().version, 2);
    }
}
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test -p brainwire-store graph`
Expected: compile errors, `merge_node` is not defined.

- [ ] **Step 3: Implement**

Top of `crates/store/src/graph.rs`:

```rust
//! Graph operations on a transaction (spec 1.2): nodes by primary key, edges
//! mirrored in `out` and `in`, detach delete, and the invariant check used by
//! the crash test.

use std::collections::{BTreeMap, HashSet};

use brainwire_core::codec::{decode_row, encode_row, Row};
use brainwire_core::ids::{EdgeId, EdgeTypeId, NodeId, TypeId};
use brainwire_core::keys::{get_bytes, get_u16, get_u64, put_bytes, put_u16, put_u64};
use brainwire_core::schema::{Catalog, Column, GraphMapping};
use brainwire_core::value::Value;

use crate::db::{Database, StoreError};
use crate::txn::{Cf, Txn, TxnError};

const CATALOG_KEY: &[u8] = b"catalog";

#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("unknown label {0}")]
    UnknownLabel(String),
    #[error("unknown edge type {0}")]
    UnknownEdgeType(String),
    #[error("edge type {0} does not connect these labels")]
    BadSignature(String),
    #[error("property {0} is reserved")]
    ReservedProperty(String),
    #[error("node {0:?} not found")]
    NodeNotFound(NodeId),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Txn(#[from] TxnError),
}

pub struct NodeRecord {
    pub id: NodeId,
    pub entity_id: String,
    pub row: Row,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EdgeRef {
    pub edge_type: EdgeTypeId,
    pub other: NodeId,
    pub id: EdgeId,
}

fn catalog_error(e: impl std::fmt::Display) -> StoreError {
    StoreError::Catalog(e.to_string())
}

impl Database {
    pub fn catalog(&self) -> Result<Option<Catalog>, StoreError> {
        match self.begin().get(Cf::Meta, CATALOG_KEY)? {
            Some(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(catalog_error),
            None => Ok(None),
        }
    }

    pub fn apply_ontology(&self, mapping: &GraphMapping) -> Result<Catalog, GraphError> {
        let mut txn = self.begin();
        let previous: Option<Catalog> = match txn.get_for_update(Cf::Meta, CATALOG_KEY)? {
            Some(bytes) => Some(serde_json::from_slice(&bytes).map_err(catalog_error)?),
            None => None,
        };
        let next = Catalog::apply(mapping, previous.as_ref()).map_err(catalog_error)?;
        txn.put(Cf::Meta, CATALOG_KEY.to_vec(), serde_json::to_vec(&next).map_err(catalog_error)?);
        txn.commit()?;
        Ok(next)
    }
}

fn node_key(id: NodeId) -> Vec<u8> {
    id.0.to_be_bytes().to_vec()
}

fn pk_prefix(entity_id: &str) -> Vec<u8> {
    let mut key = Vec::new();
    put_bytes(&mut key, entity_id.as_bytes());
    key
}

fn pk_key(entity_id: &str, ty: TypeId) -> Vec<u8> {
    let mut key = pk_prefix(entity_id);
    put_u16(&mut key, ty.0);
    key
}

fn edge_key(a: NodeId, ty: EdgeTypeId, b: NodeId, id: EdgeId) -> Vec<u8> {
    let mut key = Vec::with_capacity(26);
    put_u64(&mut key, a.0);
    put_u16(&mut key, ty.0);
    put_u64(&mut key, b.0);
    put_u64(&mut key, id.0);
    key
}

fn decode_edge_key(key: &[u8]) -> Result<(NodeId, EdgeTypeId, NodeId, EdgeId), StoreError> {
    let mut input = key;
    let a = get_u64(&mut input)?;
    let ty = get_u16(&mut input)?;
    let b = get_u64(&mut input)?;
    let id = get_u64(&mut input)?;
    Ok((NodeId(a), EdgeTypeId(ty), NodeId(b), EdgeId(id)))
}

fn node_id_value(bytes: &[u8]) -> Result<NodeId, StoreError> {
    let raw: [u8; 8] = bytes.try_into().map_err(|_| StoreError::Corrupt("node id value".into()))?;
    Ok(NodeId(u64::from_be_bytes(raw)))
}

fn encode_node(entity_id: &str, row: &Row) -> Vec<u8> {
    let mut out = (entity_id.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(entity_id.as_bytes());
    out.extend_from_slice(&encode_row(row));
    out
}

fn decode_node(bytes: &[u8]) -> Result<(String, Row), StoreError> {
    let corrupt = || StoreError::Corrupt("node value".into());
    let len: [u8; 4] = bytes.get(..4).ok_or_else(corrupt)?.try_into().map_err(|_| corrupt())?;
    let n = u32::from_be_bytes(len) as usize;
    let rest = &bytes[4..];
    let raw = rest.get(..n).ok_or_else(corrupt)?;
    let entity_id = String::from_utf8(raw.to_vec()).map_err(|_| corrupt())?;
    Ok((entity_id, decode_row(&rest[n..])?))
}

fn check_props(props: &BTreeMap<String, Value>) -> Result<(), GraphError> {
    match props.keys().find(|k| k.to_ascii_lowercase().starts_with("acl_")) {
        Some(k) => Err(GraphError::ReservedProperty(k.clone())),
        None => Ok(()),
    }
}

fn apply_props(row: &mut Row, columns: &[Column], props: &BTreeMap<String, Value>) {
    for (name, value) in props {
        match columns.iter().find(|c| &c.name == name).map(|c| c.id) {
            Some(id) if *value == Value::Null => {
                row.columns.remove(&id);
            }
            Some(id) => {
                row.columns.insert(id, value.clone());
            }
            None if *value == Value::Null => {
                row.extra.remove(name);
            }
            None => {
                row.extra.insert(name.clone(), value.clone());
            }
        }
    }
}

fn read_node(txn: &Txn, id: NodeId) -> Result<Option<(String, Row)>, GraphError> {
    match txn.get(Cf::Node, &node_key(id))? {
        Some(bytes) => Ok(Some(decode_node(&bytes)?)),
        None => Ok(None),
    }
}

pub fn merge_node(
    txn: &mut Txn,
    catalog: &Catalog,
    label: &str,
    entity_id: &str,
    props: &BTreeMap<String, Value>,
) -> Result<NodeId, GraphError> {
    check_props(props)?;
    let ty = catalog.node_type(label).ok_or_else(|| GraphError::UnknownLabel(label.to_string()))?;
    let pk = pk_key(entity_id, ty.id);
    let (id, mut row) = match txn.get_for_update(Cf::Pk, &pk)? {
        Some(bytes) => {
            let id = node_id_value(&bytes)?;
            let (_, row) = read_node(txn, id)?.ok_or(GraphError::NodeNotFound(id))?;
            (id, row)
        }
        None => {
            let id = txn.database().next_node_id(ty.id)?;
            txn.put(Cf::Pk, pk, id.0.to_be_bytes().to_vec());
            (id, Row::default())
        }
    };
    row.schema_version = catalog.version;
    apply_props(&mut row, &ty.columns, props);
    txn.put(Cf::Node, node_key(id), encode_node(entity_id, &row));
    Ok(id)
}

pub fn get_node(txn: &Txn, id: NodeId) -> Result<Option<NodeRecord>, GraphError> {
    Ok(read_node(txn, id)?.map(|(entity_id, row)| NodeRecord { id, entity_id, row }))
}

pub fn find_by_entity_id(txn: &Txn, entity_id: &str, label: Option<TypeId>) -> Result<Vec<NodeId>, GraphError> {
    let mut found = Vec::new();
    for (key, value) in txn.scan_prefix(Cf::Pk, &pk_prefix(entity_id))? {
        let mut input = &key[key.len() - 2..];
        let ty = TypeId(get_u16(&mut input).map_err(StoreError::from)?);
        if label.map_or(true, |l| l == ty) {
            found.push(node_id_value(&value)?);
        }
    }
    Ok(found)
}

pub fn merge_edge(
    txn: &mut Txn,
    catalog: &Catalog,
    src: NodeId,
    edge_type: &str,
    dst: NodeId,
    props: &BTreeMap<String, Value>,
) -> Result<(), GraphError> {
    check_props(props)?;
    let et = catalog
        .edge_type(edge_type)
        .ok_or_else(|| GraphError::UnknownEdgeType(edge_type.to_string()))?;
    if !et.signatures.contains(&(src.type_id(), dst.type_id())) {
        return Err(GraphError::BadSignature(edge_type.to_string()));
    }
    for id in [src, dst] {
        if read_node(txn, id)?.is_none() {
            return Err(GraphError::NodeNotFound(id));
        }
    }
    let out = edge_key(src, et.id, dst, EdgeId::MERGED);
    let mut row = match txn.get_for_update(Cf::Out, &out)? {
        Some(bytes) => decode_row(&bytes).map_err(StoreError::from)?,
        None => Row::default(),
    };
    row.schema_version = catalog.version;
    apply_props(&mut row, &et.columns, props);
    txn.put(Cf::Out, out, encode_row(&row));
    txn.put(Cf::In, edge_key(dst, et.id, src, EdgeId::MERGED), Vec::new());
    Ok(())
}

fn edges(txn: &Txn, cf: Cf, node: NodeId) -> Result<Vec<EdgeRef>, GraphError> {
    let mut found = Vec::new();
    for (key, _) in txn.scan_prefix(cf, &node.0.to_be_bytes())? {
        let (_, edge_type, other, id) = decode_edge_key(&key)?;
        found.push(EdgeRef { edge_type, other, id });
    }
    Ok(found)
}

pub fn out_edges(txn: &Txn, src: NodeId) -> Result<Vec<EdgeRef>, GraphError> {
    edges(txn, Cf::Out, src)
}

pub fn in_edges(txn: &Txn, dst: NodeId) -> Result<Vec<EdgeRef>, GraphError> {
    edges(txn, Cf::In, dst)
}

pub fn detach_delete(txn: &mut Txn, id: NodeId) -> Result<(), GraphError> {
    let (entity_id, _) = read_node(txn, id)?.ok_or(GraphError::NodeNotFound(id))?;
    for e in out_edges(txn, id)? {
        txn.delete(Cf::Out, edge_key(id, e.edge_type, e.other, e.id));
        txn.delete(Cf::In, edge_key(e.other, e.edge_type, id, e.id));
    }
    for e in in_edges(txn, id)? {
        txn.delete(Cf::In, edge_key(id, e.edge_type, e.other, e.id));
        txn.delete(Cf::Out, edge_key(e.other, e.edge_type, id, e.id));
    }
    txn.delete(Cf::Pk, pk_key(&entity_id, id.type_id()));
    txn.delete(Cf::Node, node_key(id));
    Ok(())
}

/// Checks the invariants the crash test relies on: pk and node agree both
/// ways, node ids are unique, `out` and `in` mirror each other, and every id
/// counter is above the largest id in use.
pub fn check_invariants(db: &Database) -> Result<(), String> {
    let txn = db.begin();
    let err = |e: &dyn std::fmt::Display| e.to_string();
    let mut seen = HashSet::new();
    for (key, value) in txn.scan_prefix(Cf::Pk, &[]).map_err(|e| err(&e))? {
        let mut input = key.as_slice();
        let entity_id = String::from_utf8(get_bytes(&mut input).map_err(|e| err(&e))?).map_err(|e| err(&e))?;
        let ty = TypeId(get_u16(&mut input).map_err(|e| err(&e))?);
        let id = node_id_value(&value).map_err(|e| err(&e))?;
        if !seen.insert(id) {
            return Err(format!("node id {id:?} is used by two pk entries"));
        }
        let (stored, _) = read_node(&txn, id).map_err(|e| err(&e))?.ok_or(format!("pk {entity_id} points to missing node"))?;
        if stored != entity_id || id.type_id() != ty {
            return Err(format!("pk {entity_id} and node {id:?} disagree"));
        }
    }
    for (key, value) in txn.scan_prefix(Cf::Node, &[]).map_err(|e| err(&e))? {
        let id = node_id_value(&key).map_err(|e| err(&e))?;
        let (entity_id, _) = decode_node(&value).map_err(|e| err(&e))?;
        let pk = txn.get(Cf::Pk, &pk_key(&entity_id, id.type_id())).map_err(|e| err(&e))?;
        if pk.as_deref() != Some(&id.0.to_be_bytes()[..]) {
            return Err(format!("node {id:?} has no matching pk"));
        }
        let counter = txn
            .get(Cf::Meta, &crate::alloc::node_counter_key(id.type_id()))
            .map_err(|e| err(&e))?
            .ok_or(format!("no id counter for type {:?}", id.type_id()))?;
        let end = u64::from_be_bytes(counter.as_slice().try_into().map_err(|_| "bad counter".to_string())?);
        if id.seq() >= end {
            return Err(format!("node {id:?} is above its counter {end}"));
        }
    }
    for (cf, mirror) in [(Cf::Out, Cf::In), (Cf::In, Cf::Out)] {
        for (key, _) in txn.scan_prefix(cf, &[]).map_err(|e| err(&e))? {
            let (a, ty, b, id) = decode_edge_key(&key).map_err(|e| err(&e))?;
            if txn.get(mirror, &edge_key(b, ty, a, id)).map_err(|e| err(&e))?.is_none() {
                return Err(format!("edge {a:?} {ty:?} {b:?} has no mirror in {}", mirror.name()));
            }
        }
    }
    Ok(())
}
```

Add `pub mod graph;` to `crates/store/src/lib.rs`, and in
`crates/store/Cargo.toml` add `serde_json.workspace = true` under
`[dependencies]`. In `crates/store/src/alloc.rs`, `node_counter_key` is already
`pub(crate)`, which `check_invariants` needs.

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p brainwire-store`
Expected: every test passes, including the seven `graph::tests`.

- [ ] **Step 5: Commit**

```bash
git add external-brainwire/crates/store
git commit -m "feat(brainwire): add catalog persistence and graph operations"
```

## Task 10: Crash test with kill -9

**Files:**
- Create: `crates/store/src/bin/crash_writer.rs`
- Create: `crates/store/tests/crash.rs`

**Interfaces:**
- Consumes: `Database`, `Resources`, `StoreConfig`, `merge_node`, `merge_edge`,
  `find_by_entity_id`, `check_invariants`.
- Produces: the Gate 1 evidence that the store reopens consistent after
  `kill -9` and loses no acknowledged write.

- [ ] **Step 1: Write the writer binary**

`crates/store/src/bin/crash_writer.rs`:

```rust
//! Writes forever, printing the number of each acknowledged commit. The crash
//! test kills it with SIGKILL at a random point.

use std::collections::BTreeMap;
use std::io::Write;

use brainwire_core::schema::GraphMapping;
use brainwire_core::value::Value;
use brainwire_store::db::{Database, Resources, StoreConfig};
use brainwire_store::graph::{merge_edge, merge_node};

fn main() {
    let path = std::env::args().nth(1).expect("database path");
    let start: u64 = std::env::args().nth(2).expect("first number").parse().expect("a number");
    let res = Resources::new(StoreConfig::for_tests());
    let db = Database::open(std::path::Path::new(&path), &res).expect("open");
    let mapping: GraphMapping =
        serde_json::from_str(include_str!("../../../core/tests/fixtures/graph_mapping.json")).expect("fixture");
    let catalog = db.apply_ontology(&mapping).expect("ontology");
    let mut out = std::io::stdout();
    for i in start.. {
        let mut txn = db.begin();
        let mut props = BTreeMap::new();
        props.insert("name".to_string(), Value::Str(format!("customer {i}")));
        let c = merge_node(&mut txn, &catalog, "Customer", &format!("c:{i}"), &props).expect("customer");
        let k = merge_node(&mut txn, &catalog, "Company", &format!("k:{}", i % 100), &BTreeMap::new()).expect("company");
        merge_edge(&mut txn, &catalog, c, "WORKS_AT", k, &BTreeMap::new()).expect("edge");
        txn.commit().expect("commit");
        writeln!(out, "{i}").expect("stdout");
        out.flush().expect("flush");
    }
}
```

- [ ] **Step 2: Write the test**

`crates/store/tests/crash.rs`:

```rust
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use brainwire_store::db::{Database, Resources, StoreConfig};
use brainwire_store::graph::{check_invariants, find_by_entity_id};

#[test]
fn survives_kill_minus_nine() {
    let dir = tempfile::tempdir().unwrap();
    let mut next = 0u64;
    for round in 0..5u64 {
        let mut child = Command::new(env!("CARGO_BIN_EXE_crash_writer"))
            .arg(dir.path())
            .arg(next.to_string())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut last = 0u64;
        for _ in 0..(150 + round * 37) {
            last = lines.next().unwrap().unwrap().parse().unwrap();
        }
        child.kill().unwrap(); // SIGKILL on Unix
        child.wait().unwrap();

        let res = Resources::new(StoreConfig::for_tests());
        let db = Database::open(dir.path(), &res).unwrap();
        check_invariants(&db).unwrap();
        let txn = db.begin();
        assert_eq!(
            find_by_entity_id(&txn, &format!("c:{last}"), None).unwrap().len(),
            1,
            "acknowledged commit {last} was lost in round {round}"
        );
        next = last + 1;
    }
}
```

- [ ] **Step 3: Run it**

Run: `cargo test -p brainwire-store --test crash`
Expected: PASS. If it fails, the message names the broken invariant or the
lost commit; debug with `superpowers:systematic-debugging` before changing
anything.

- [ ] **Step 4: Run the whole workspace**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all -- --check`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add external-brainwire/crates/store
git commit -m "test(brainwire): prove the store survives kill -9 without losing commits"
```

## What comes next

- Plan 2: Bolt server (handshake, PackStream, HELLO/LOGON, RUN/PULL,
  BEGIN/COMMIT) so the Python driver connects, which closes Gate 1.
- Plan 3: Cypher front end and executor over this store, native ACL, secondary
  indexes, introspection, and the contract suite (Gate 2).

Spec items this plan leaves for later plans, on purpose:

- group commit, one WriteBatch and one fsync for several transactions (spec
  1.4); this plan writes one batch per transaction, which is correct and slower;
- per-family filters and prefix extractors (spec 1.6), which the scale plan
  measures before setting;
- typed columns fed by the clean layer (spec 2.1); this plan stores tagged
  values, which keeps rows free of property names;
- bulk loading, online and offline (spec 1.7);
- events and notes (spec 2.4 and 4.4); their column families already exist;
- the per-query memory ceiling (spec 1.5), which belongs to the executor;
- LGPD purge and backup (spec 1.6 and 5.2).
