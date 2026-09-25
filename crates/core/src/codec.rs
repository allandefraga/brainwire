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
    Ok(Row {
        schema_version,
        acl_set,
        columns,
        extra,
    })
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
