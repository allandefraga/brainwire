//! PackStream v1, the value encoding of Bolt.

/// Maximum nesting depth `unpack` will follow before returning
/// [`PackError::TooDeep`]. Bounds the recursion so adversarial input (e.g. a
/// long run of tiny-list headers) cannot overflow the stack.
pub const MAX_DEPTH: usize = 64;

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
    /// Looks a key up in a `Map`. PackStream does not forbid duplicate keys,
    /// and the spec (research/bolt.md 2.1) says the *last* one wins, so this
    /// searches from the end rather than returning the first match.
    pub fn get(&self, key: &str) -> Option<&PackValue> {
        match self {
            PackValue::Map(entries) => entries.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v),
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
    #[error("value nested deeper than {} levels", MAX_DEPTH)]
    TooDeep,
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

fn list_of(input: &mut &[u8], len: usize, depth: usize) -> Result<PackValue, PackError> {
    let mut items = Vec::with_capacity(len.min(1024));
    for _ in 0..len {
        items.push(unpack_at(input, depth + 1)?);
    }
    Ok(PackValue::List(items))
}

fn map_of(input: &mut &[u8], len: usize, depth: usize) -> Result<PackValue, PackError> {
    let mut entries = Vec::with_capacity(len.min(1024));
    for _ in 0..len {
        let key = match unpack_at(input, depth + 1)? {
            PackValue::String(s) => s,
            _ => return Err(PackError::BadMapKey),
        };
        entries.push((key, unpack_at(input, depth + 1)?));
    }
    Ok(PackValue::Map(entries))
}

pub fn unpack(input: &mut &[u8]) -> Result<PackValue, PackError> {
    unpack_at(input, 0)
}

fn unpack_at(input: &mut &[u8], depth: usize) -> Result<PackValue, PackError> {
    if depth > MAX_DEPTH {
        return Err(PackError::TooDeep);
    }
    let marker = take(input, 1)?[0];
    Ok(match marker {
        0x00..=0x7F => PackValue::Int(marker as i64),
        0xF0..=0xFF => PackValue::Int(marker as i8 as i64),
        0x80..=0x8F => PackValue::String(string_of(input, (marker & 0x0F) as usize)?),
        0x90..=0x9F => list_of(input, (marker & 0x0F) as usize, depth)?,
        0xA0..=0xAF => map_of(input, (marker & 0x0F) as usize, depth)?,
        0xB0..=0xBF => {
            let tag = take(input, 1)?[0];
            let mut fields = Vec::new();
            for _ in 0..(marker & 0x0F) {
                fields.push(unpack_at(input, depth + 1)?);
            }
            PackValue::Struct { tag, fields }
        }
        0xC0 => PackValue::Null,
        0xC1 => PackValue::Float(f64::from_be_bytes(
            take(input, 8)?.try_into().expect("8 bytes"),
        )),
        0xC2 => PackValue::Bool(false),
        0xC3 => PackValue::Bool(true),
        0xC8 => PackValue::Int(take(input, 1)?[0] as i8 as i64),
        0xC9 => {
            PackValue::Int(i16::from_be_bytes(take(input, 2)?.try_into().expect("2 bytes")) as i64)
        }
        0xCA => {
            PackValue::Int(i32::from_be_bytes(take(input, 4)?.try_into().expect("4 bytes")) as i64)
        }
        0xCB => PackValue::Int(i64::from_be_bytes(
            take(input, 8)?.try_into().expect("8 bytes"),
        )),
        0xCC..=0xCE => {
            let width = 1 << (marker - 0xCC);
            let len = be_len(input, width)?;
            PackValue::Bytes(take(input, len)?.to_vec())
        }
        0xD0..=0xD2 => {
            let len = be_len(input, 1 << (marker - 0xD0))?;
            PackValue::String(string_of(input, len)?)
        }
        0xD4..=0xD6 => {
            let len = be_len(input, 1 << (marker - 0xD4))?;
            list_of(input, len, depth)?
        }
        0xD8..=0xDA => {
            let len = be_len(input, 1 << (marker - 0xD8))?;
            map_of(input, len, depth)?
        }
        other => return Err(PackError::UnknownMarker(other)),
    })
}

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
            bytes_of(&PackValue::Struct {
                tag: 0x70,
                fields: vec![PackValue::Map(vec![])]
            }),
            vec![0xB1, 0x70, 0xA0]
        );
    }

    #[test]
    fn round_trips_every_kind_and_size() {
        for i in [
            0,
            127,
            -16,
            -17,
            -128,
            32_767,
            -32_768,
            2_147_483_647,
            i64::MAX,
            i64::MIN,
        ] {
            round_trip(PackValue::Int(i));
        }
        round_trip(PackValue::Float(1.5));
        round_trip(PackValue::Bytes(vec![1, 2, 3]));
        round_trip(PackValue::String("x".repeat(16)));
        round_trip(PackValue::String("x".repeat(300)));
        round_trip(PackValue::String("x".repeat(70_000)));
        round_trip(PackValue::List((0..20).map(PackValue::Int).collect()));
        round_trip(PackValue::Map(
            (0..20)
                .map(|i| (format!("k{i}"), PackValue::Int(i)))
                .collect(),
        ));
        round_trip(PackValue::Struct {
            tag: 0x4E,
            fields: vec![PackValue::Int(1), PackValue::List(vec![])],
        });
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

    // M1 (review finding, 2026-09-25): PackStream says the *last* entry with
    // a given key wins (research/bolt.md 2.1), but `get` returned the first
    // match. A BEGIN with `{db: "first", db: "last"}` produced the bookmark
    // `bw:first:1` instead of `bw:last:1`.
    #[test]
    fn map_get_returns_the_last_matching_key_not_the_first() {
        let m = PackValue::Map(vec![
            ("db".to_string(), PackValue::String("first".into())),
            ("db".to_string(), PackValue::String("last".into())),
        ]);
        assert_eq!(m.get("db"), Some(&PackValue::String("last".into())));
    }

    // M7 (review finding, 2026-09-25): the TooDeep message hardcoded "64"
    // instead of reading MAX_DEPTH, so the two could drift silently. This
    // pins the rendered text; changing MAX_DEPTH must change the value below
    // as a reminder to check the message still reads correctly.
    #[test]
    fn too_deep_message_is_built_from_max_depth_and_reads_the_same_as_before() {
        assert_eq!(MAX_DEPTH, 64);
        assert_eq!(
            PackError::TooDeep.to_string(),
            "value nested deeper than 64 levels"
        );
    }

    // Missing-test gap flagged by the review: BadUtf8 and BadMapKey had no
    // coverage through `unpack`.
    #[test]
    fn invalid_utf8_in_a_tiny_string_is_rejected() {
        // 0x81 = tiny string, length 1; 0xFF is never valid UTF-8 (standalone
        // continuation byte with no leading byte).
        let mut input: &[u8] = &[0x81, 0xFF];
        assert_eq!(unpack(&mut input), Err(PackError::BadUtf8));
    }

    #[test]
    fn a_non_string_map_key_is_rejected() {
        // 0xA1 = tiny map, 1 entry; 0x01 = Int(1) where a String key is
        // required.
        let mut input: &[u8] = &[0xA1, 0x01];
        assert_eq!(unpack(&mut input), Err(PackError::BadMapKey));
    }

    #[test]
    fn max_depth_nesting_decodes() {
        let mut bytes = vec![0x91u8; MAX_DEPTH];
        bytes.push(0x01);
        let mut input = bytes.as_slice();
        assert!(unpack(&mut input).is_ok());
    }

    #[test]
    fn depth_past_max_is_too_deep() {
        let mut bytes = vec![0x91u8; MAX_DEPTH + 1];
        bytes.push(0x01);
        let mut input = bytes.as_slice();
        assert_eq!(unpack(&mut input), Err(PackError::TooDeep));
    }

    #[test]
    fn deeply_nested_adversarial_input_returns_too_deep() {
        let bytes = vec![0x91u8; 100_000];
        let mut input = bytes.as_slice();
        assert_eq!(unpack(&mut input), Err(PackError::TooDeep));
    }
}
