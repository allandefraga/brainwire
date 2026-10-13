//! Row codec for node, edge and event rows (spec 1.2). Columns are keyed by
//! column id, so no property name is stored per row; properties outside the
//! ontology go to the `extra` map.

use std::collections::BTreeMap;

use crate::value::Value;

pub const MAX_DEPTH: usize = 64;

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
    #[error("value nested deeper than 64 levels")]
    TooDeep,
    #[error("row allocation estimate exceeds addressable capacity")]
    AllocationOverflow,
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
    get_value_at(input, 0)
}

fn get_value_at(input: &mut &[u8], depth: usize) -> Result<Value, CodecError> {
    if depth > MAX_DEPTH {
        return Err(CodecError::TooDeep);
    }
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
                items.push(get_value_at(input, depth + 1)?);
            }
            Value::List(items)
        }
        T_MAP => {
            let n = get_varint(input)?;
            let mut map = BTreeMap::new();
            for _ in 0..n {
                let k = get_str(input)?;
                map.insert(k, get_value_at(input, depth + 1)?);
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
    Ok(Row {
        schema_version,
        acl_set,
        columns,
        extra,
    })
}

fn estimate_add(total: &mut usize, bytes: usize) -> Result<(), CodecError> {
    *total = total
        .checked_add(bytes)
        .ok_or(CodecError::AllocationOverflow)?;
    Ok(())
}
fn estimate_take<'a>(input: &mut &'a [u8], len: usize) -> Result<&'a [u8], CodecError> {
    if input.len() < len {
        return Err(CodecError::Truncated);
    }
    let (head, rest) = input.split_at(len);
    *input = rest;
    Ok(head)
}
fn estimate_string(input: &mut &[u8], total: &mut usize) -> Result<(), CodecError> {
    let len = usize::try_from(get_varint(input)?).map_err(|_| CodecError::AllocationOverflow)?;
    let raw = estimate_take(input, len)?;
    std::str::from_utf8(raw).map_err(|_| CodecError::BadUtf8)?;
    estimate_add(total, len)
}
fn estimate_count(input: &mut &[u8], minimum: usize) -> Result<usize, CodecError> {
    let count = usize::try_from(get_varint(input)?).map_err(|_| CodecError::AllocationOverflow)?;
    if count
        .checked_mul(minimum)
        .is_none_or(|bytes| bytes > input.len())
    {
        return Err(CodecError::Truncated);
    }
    Ok(count)
}
fn estimate_map(total: &mut usize, count: usize) -> Result<(), CodecError> {
    if count != 0 {
        estimate_add(total, 1024)?;
        estimate_add(
            total,
            count
                .checked_mul(256)
                .ok_or(CodecError::AllocationOverflow)?,
        )?;
    }
    Ok(())
}
fn estimate_value(input: &mut &[u8], depth: usize, total: &mut usize) -> Result<(), CodecError> {
    if depth > MAX_DEPTH {
        return Err(CodecError::TooDeep);
    }
    let tag = estimate_take(input, 1)?[0];
    match tag {
        T_NULL | T_FALSE | T_TRUE => {}
        T_INT => {
            get_varint(input)?;
        }
        T_FLOAT => {
            estimate_take(input, 8)?;
        }
        T_STR => estimate_string(input, total)?,
        T_LIST => {
            let count = estimate_count(input, 1)?;
            if count != 0 {
                let slots = count
                    .checked_mul(2)
                    .and_then(|n| n.checked_add(4))
                    .ok_or(CodecError::AllocationOverflow)?;
                estimate_add(
                    total,
                    slots
                        .checked_mul(std::mem::size_of::<Value>())
                        .ok_or(CodecError::AllocationOverflow)?,
                )?;
            }
            for _ in 0..count {
                estimate_value(input, depth + 1, total)?;
            }
        }
        T_MAP => {
            let count = estimate_count(input, 2)?;
            estimate_map(total, count)?;
            for _ in 0..count {
                estimate_string(input, total)?;
                estimate_value(input, depth + 1, total)?;
            }
        }
        other => return Err(CodecError::UnknownTag(other)),
    }
    Ok(())
}

/// Nonallocating preflight of owned decoded Row storage. Strings, list growth
/// and conservative BTreeMap backing storage are charged using checked sums.
/// Input bytes, allocator metadata/reallocation peaks and later clones are excluded.
/// Like decode_row, trailing bytes after a complete row are ignored.
pub fn estimate_row_allocation(bytes: &[u8]) -> Result<usize, CodecError> {
    let mut input = bytes;
    let mut total = std::mem::size_of::<Row>();
    get_varint(&mut input)?;
    get_varint(&mut input)?;
    let columns = estimate_count(&mut input, 2)?;
    estimate_map(&mut total, columns)?;
    for _ in 0..columns {
        get_varint(&mut input)?;
        estimate_value(&mut input, 0, &mut total)?;
    }
    let extra = estimate_count(&mut input, 2)?;
    estimate_map(&mut total, extra)?;
    for _ in 0..extra {
        estimate_string(&mut input, &mut total)?;
        estimate_value(&mut input, 0, &mut total)?;
    }
    Ok(total)
}

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
        row.columns
            .insert(3, crate::value::Value::Str("hello".into()));
        let bytes = encode_row(&row);
        assert_eq!(
            decode_row(&bytes[..bytes.len() - 2]),
            Err(CodecError::Truncated)
        );
    }

    #[test]
    fn max_depth_64_nested_lists_round_trip() {
        // Build a row with 64 nested single-element lists ending in Null.
        let mut value = Value::Null;
        for _ in 0..MAX_DEPTH {
            value = Value::List(vec![value]);
        }
        let mut row = Row::default();
        row.columns.insert(0, value);
        let encoded = encode_row(&row);
        let decoded = decode_row(&encoded);
        assert!(decoded.is_ok(), "64-level nesting should succeed");
        assert_eq!(decoded.unwrap(), row);
    }

    #[test]
    fn depth_65_returns_too_deep() {
        // Build a row with 65 nested single-element lists ending in Null.
        let mut value = Value::Null;
        for _ in 0..=MAX_DEPTH {
            value = Value::List(vec![value]);
        }
        let mut row = Row::default();
        row.columns.insert(0, value);
        let encoded = encode_row(&row);
        let decoded = decode_row(&encoded);
        assert_eq!(decoded, Err(CodecError::TooDeep));
    }

    #[test]
    fn extreme_nesting_100k_levels_does_not_crash() {
        // Build bytes for a row with 100,000 nested single-element lists.
        // Manually construct: schema_version=1, acl_set=0, 1 column (id=0),
        // then 100,000 nested single-element lists all containing Null.
        // Each list: tag(1 byte) + count(1 byte, value 1) + element.
        // We don't recursively construct the Value; we craft the bytes directly.
        let mut bytes = Vec::new();
        put_varint(&mut bytes, 1u64); // schema_version = 1
        put_varint(&mut bytes, 0u64); // acl_set = 0
        put_varint(&mut bytes, 1u64); // 1 column
        put_varint(&mut bytes, 0u64); // column id = 0

        // Manually write 100,000 nested list tags + counts
        for _ in 0..100_000 {
            bytes.push(6u8); // T_LIST tag
            bytes.push(1u8); // list has 1 element (encoded as varint = 0x01)
        }
        bytes.push(0u8); // T_NULL (innermost value)
        put_varint(&mut bytes, 0u64); // 0 extra fields

        let result = decode_row(&bytes);
        // Must not panic; must return TooDeep, never stack overflow crash.
        assert_eq!(result, Err(CodecError::TooDeep));
    }

    #[test]
    fn invalid_utf8_in_string_returns_bad_utf8() {
        // Manually construct a row with a string containing invalid UTF-8.
        // Tag 5 (T_STR), varint length 2, then 0xFF 0xFE (invalid UTF-8).
        let mut bytes = Vec::new();
        put_varint(&mut bytes, 1u64); // schema_version = 1
        put_varint(&mut bytes, 0u64); // acl_set = 0
        put_varint(&mut bytes, 1u64); // 1 column
        put_varint(&mut bytes, 0u64); // column id = 0
        bytes.push(5u8); // T_STR tag
        put_varint(&mut bytes, 2u64); // string length = 2
        bytes.extend_from_slice(&[0xFF, 0xFE]); // invalid UTF-8
        put_varint(&mut bytes, 0u64); // 0 extra fields

        let result = decode_row(&bytes);
        assert_eq!(result, Err(CodecError::BadUtf8));
    }

    #[test]
    fn varint_11_continuation_bytes_returns_overflow() {
        // Manually construct a row with an 11-byte varint (overflow).
        // Tag 3 (T_INT), then 11 continuation bytes (0x80 repeated, then 0x01).
        let mut bytes = Vec::new();
        put_varint(&mut bytes, 1u64); // schema_version = 1
        put_varint(&mut bytes, 0u64); // acl_set = 0
        put_varint(&mut bytes, 1u64); // 1 column
        put_varint(&mut bytes, 0u64); // column id = 0
        bytes.push(3u8); // T_INT tag
        bytes.extend_from_slice(&[0x80u8; 11]); // 11 continuation bytes
        put_varint(&mut bytes, 0u64); // 0 extra fields

        let result = decode_row(&bytes);
        assert_eq!(result, Err(CodecError::VarintOverflow));
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
    #[test]
    fn allocation_preflight_covers_null_lists_nested_singletons_maps_and_long_strings() {
        let mut row = Row::default();
        row.extra
            .insert("values".into(), Value::List(vec![Value::Null; 4097]));
        let bytes = encode_row(&row);
        let estimate = estimate_row_allocation(&bytes).unwrap();
        assert!(estimate > bytes.len() * 4);
        let decoded = decode_row(&bytes).unwrap();
        let Value::List(items) = &decoded.extra["values"] else {
            panic!()
        };
        assert!(estimate >= items.capacity() * std::mem::size_of::<Value>());
        let mut nested = Value::Str("x".repeat(200_000));
        for _ in 0..64 {
            nested = Value::List(vec![nested]);
        }
        row.extra.insert("nested".into(), nested);
        row.columns
            .insert(1, Value::Map(BTreeMap::from([("key".into(), Value::Null)])));
        assert!(estimate_row_allocation(&encode_row(&row)).unwrap() > 200_000 + estimate);
        assert!(
            estimate_row_allocation(&encode_row(&Row::default())).unwrap()
                >= std::mem::size_of::<Row>()
        );
    }
    #[test]
    fn allocation_preflight_matches_decoder_errors_without_allocating_announced_storage() {
        for bytes in [
            vec![1, 0, 1, 0, 99],
            vec![1, 0, 1, 0, 5, 1, 255],
            vec![
                1, 0, 1, 0, 6, 255, 255, 255, 255, 255, 255, 255, 255, 255, 1,
            ],
        ] {
            assert_eq!(
                estimate_row_allocation(&bytes).unwrap_err(),
                decode_row(&bytes).unwrap_err()
            );
        }
        let mut bytes = vec![1, 0, 1, 0];
        for _ in 0..65 {
            bytes.extend([T_LIST, 1]);
        }
        bytes.extend([T_NULL, 0]);
        assert_eq!(estimate_row_allocation(&bytes), Err(CodecError::TooDeep));
    }
    fn decoded_value_backing(value: &Value) -> usize {
        match value {
            Value::Str(value) => value.capacity(),
            Value::List(items) => {
                items.capacity() * std::mem::size_of::<Value>()
                    + items.iter().map(decoded_value_backing).sum::<usize>()
            }
            Value::Map(items) => {
                items.len() * std::mem::size_of::<(String, Value)>()
                    + items
                        .iter()
                        .map(|(key, value)| key.capacity() + decoded_value_backing(value))
                        .sum::<usize>()
            }
            _ => 0,
        }
    }
    proptest! {
        #[test]
        fn allocation_preflight_covers_decoded_recursive_value_backing(value in arb_value()) {
            let row=Row{extra:BTreeMap::from([("value".into(),value)]),..Row::default()};
            let bytes=encode_row(&row);let decoded=decode_row(&bytes).unwrap();
            let estimate=estimate_row_allocation(&bytes).unwrap();
            let lower_bound=std::mem::size_of::<Row>()+decoded.extra.iter().map(|(key,value)|key.capacity()+decoded_value_backing(value)).sum::<usize>();
            prop_assert!(estimate>=lower_bound);
        }
    }
}
