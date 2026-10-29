//! PackStream v1, the value encoding of Bolt.

use crate::ingress::IngressLease;

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

    /// The JSON value of a null, boolean, number, string, list or map; `None` for
    /// bytes, structures and non-finite floats, which have no JSON form.
    pub fn to_json(&self) -> Option<serde_json::Value> {
        use serde_json::Value as Json;
        Some(match self {
            PackValue::Null => Json::Null,
            PackValue::Bool(b) => Json::Bool(*b),
            PackValue::Int(i) => Json::from(*i),
            PackValue::Float(f) => Json::from(serde_json::Number::from_f64(*f)?),
            PackValue::String(s) => Json::String(s.clone()),
            PackValue::List(items) => Json::Array(
                items
                    .iter()
                    .map(PackValue::to_json)
                    .collect::<Option<_>>()?,
            ),
            PackValue::Map(entries) => Json::Object(
                entries
                    .iter()
                    .map(|(k, v)| Some((k.clone(), v.to_json()?)))
                    .collect::<Option<_>>()?,
            ),
            PackValue::Bytes(_) | PackValue::Struct { .. } => return None,
        })
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
    #[error("encoded value exceeds the PackStream format limit")]
    EncodedLimit,
    #[error("decoded value exceeds its allocation budget")]
    DecodedLimit,
    #[error("decoded value allocation failed")]
    Allocation,
    #[error("shared Bolt input memory budget exhausted")]
    IngressLimit,
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

/// Borrowed payloads and small headers for incremental response encoding.
/// The traversal stack also accommodates response envelopes around decoded data.
pub(crate) enum PackFragment<'a> {
    Header { bytes: [u8; 9], len: usize },
    Payload(&'a [u8]),
}

impl PackFragment<'_> {
    fn header(data: &[u8]) -> Self {
        let mut bytes = [0; 9];
        bytes[..data.len()].copy_from_slice(data);
        Self::Header {
            bytes,
            len: data.len(),
        }
    }

    pub(crate) fn as_slice(&self) -> &[u8] {
        match self {
            Self::Header { bytes, len } => &bytes[..*len],
            Self::Payload(bytes) => bytes,
        }
    }
}

fn size_fragment(
    len: usize,
    tiny: Option<u8>,
    m8: u8,
    m16: u8,
    m32: u8,
) -> Result<PackFragment<'static>, PackError> {
    if let (Some(tiny), true) = (tiny, len < 16) {
        return Ok(PackFragment::header(&[tiny | len as u8]));
    }
    let mut bytes = [0; 9];
    let width = if len <= u8::MAX as usize {
        bytes[..2].copy_from_slice(&[m8, len as u8]);
        2
    } else if len <= u16::MAX as usize {
        bytes[0] = m16;
        bytes[1..3].copy_from_slice(&(len as u16).to_be_bytes());
        3
    } else {
        let len = u32::try_from(len).map_err(|_| PackError::EncodedLimit)?;
        bytes[0] = m32;
        bytes[1..5].copy_from_slice(&len.to_be_bytes());
        5
    };
    Ok(PackFragment::Header { bytes, len: width })
}

/// Leaves room for response envelopes around the maximum decoded value depth.
pub(crate) const MAX_ENCODE_DEPTH: usize = 2 * MAX_DEPTH;

enum EncodeFrame<'a> {
    Value(&'a PackValue, usize),
    String(&'a str, usize),
    Payload(&'a [u8]),
    Values(std::slice::Iter<'a, PackValue>, usize),
    Entries(std::slice::Iter<'a, (String, PackValue)>, usize),
}

pub(crate) struct PackFragments<'a> {
    stack: Vec<EncodeFrame<'a>>,
}

impl<'a> PackFragments<'a> {
    pub(crate) fn new(value: &'a PackValue) -> Result<Self, PackError> {
        let mut encoder = Self { stack: Vec::new() };
        encoder.push(EncodeFrame::Value(value, 0))?;
        Ok(encoder)
    }

    fn push(&mut self, frame: EncodeFrame<'a>) -> Result<(), PackError> {
        self.stack
            .try_reserve(1)
            .map_err(|_| PackError::Allocation)?;
        self.stack.push(frame);
        Ok(())
    }

    pub(crate) fn rewind(&mut self, value: &'a PackValue) {
        self.stack.clear();
        self.stack.push(EncodeFrame::Value(value, 0));
    }

    fn fragment(&mut self) -> Result<Option<PackFragment<'a>>, PackError> {
        loop {
            let Some(frame) = self.stack.pop() else {
                return Ok(None);
            };
            match frame {
                EncodeFrame::Payload(bytes) => return Ok(Some(PackFragment::Payload(bytes))),
                EncodeFrame::String(value, depth) => {
                    if depth > MAX_ENCODE_DEPTH {
                        return Err(PackError::EncodedLimit);
                    }
                    self.push(EncodeFrame::Payload(value.as_bytes()))?;
                    return size_fragment(value.len(), Some(0x80), 0xD0, 0xD1, 0xD2).map(Some);
                }
                EncodeFrame::Values(mut values, depth) => {
                    if let Some(value) = values.next() {
                        self.push(EncodeFrame::Values(values, depth))?;
                        self.push(EncodeFrame::Value(value, depth))?;
                    }
                }
                EncodeFrame::Entries(mut entries, depth) => {
                    if let Some((key, value)) = entries.next() {
                        self.push(EncodeFrame::Entries(entries, depth))?;
                        self.push(EncodeFrame::Value(value, depth))?;
                        self.push(EncodeFrame::String(key, depth))?;
                    }
                }
                EncodeFrame::Value(value, depth) => {
                    if depth > MAX_ENCODE_DEPTH {
                        return Err(PackError::EncodedLimit);
                    }
                    let header = match value {
                        PackValue::Null => PackFragment::header(&[0xC0]),
                        PackValue::Bool(false) => PackFragment::header(&[0xC2]),
                        PackValue::Bool(true) => PackFragment::header(&[0xC3]),
                        PackValue::Int(value) => {
                            let (marker, width) = if (-16..=127).contains(value) {
                                (0, 1)
                            } else if i8::try_from(*value).is_ok() {
                                (0xC8, 1)
                            } else if i16::try_from(*value).is_ok() {
                                (0xC9, 2)
                            } else if i32::try_from(*value).is_ok() {
                                (0xCA, 4)
                            } else {
                                (0xCB, 8)
                            };
                            let raw = value.to_be_bytes();
                            if marker == 0 {
                                PackFragment::header(&raw[7..])
                            } else {
                                let mut bytes = [0; 9];
                                bytes[0] = marker;
                                bytes[1..=width].copy_from_slice(&raw[8 - width..]);
                                PackFragment::Header {
                                    bytes,
                                    len: width + 1,
                                }
                            }
                        }
                        PackValue::Float(value) => {
                            let mut bytes = [0; 9];
                            bytes[0] = 0xC1;
                            bytes[1..].copy_from_slice(&value.to_be_bytes());
                            PackFragment::Header { bytes, len: 9 }
                        }
                        PackValue::Bytes(bytes) => {
                            self.push(EncodeFrame::Payload(bytes))?;
                            return size_fragment(bytes.len(), None, 0xCC, 0xCD, 0xCE).map(Some);
                        }
                        PackValue::String(value) => {
                            self.push(EncodeFrame::String(value, depth))?;
                            continue;
                        }
                        PackValue::List(values) => {
                            self.push(EncodeFrame::Values(values.iter(), depth + 1))?;
                            return size_fragment(values.len(), Some(0x90), 0xD4, 0xD5, 0xD6)
                                .map(Some);
                        }
                        PackValue::Map(entries) => {
                            self.push(EncodeFrame::Entries(entries.iter(), depth + 1))?;
                            return size_fragment(entries.len(), Some(0xA0), 0xD8, 0xD9, 0xDA)
                                .map(Some);
                        }
                        PackValue::Struct { tag, fields } => {
                            if fields.len() > 15 {
                                return Err(PackError::EncodedLimit);
                            }
                            self.push(EncodeFrame::Values(fields.iter(), depth + 1))?;
                            PackFragment::header(&[0xB0 | fields.len() as u8, *tag])
                        }
                    };
                    return Ok(Some(header));
                }
            }
        }
    }
}

impl<'a> Iterator for PackFragments<'a> {
    type Item = Result<PackFragment<'a>, PackError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.fragment() {
            Ok(fragment) => fragment.map(Ok),
            Err(error) => {
                self.stack.clear();
                Some(Err(error))
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

struct DecodeBudget<'a> {
    remaining: usize,
    ingress: Option<&'a mut IngressLease>,
}

fn charge_storage<T>(len: usize, remaining: &mut DecodeBudget<'_>) -> Result<(), PackError> {
    let bytes = len
        .checked_mul(std::mem::size_of::<T>())
        .ok_or(PackError::DecodedLimit)?;
    let next = remaining
        .remaining
        .checked_sub(bytes)
        .ok_or(PackError::DecodedLimit)?;
    if let Some(lease) = remaining.ingress.as_deref_mut() {
        lease.grow(bytes).map_err(|_| PackError::IngressLimit)?;
    }
    remaining.remaining = next;
    Ok(())
}

fn reserve<T>(len: usize, remaining: &mut DecodeBudget<'_>) -> Result<Vec<T>, PackError> {
    charge_storage::<T>(len, remaining)?;
    let mut values = Vec::new();
    values
        .try_reserve_exact(len)
        .map_err(|_| PackError::Allocation)?;
    Ok(values)
}

fn bytes_of_input(
    input: &mut &[u8],
    len: usize,
    remaining: &mut DecodeBudget<'_>,
) -> Result<Vec<u8>, PackError> {
    let bytes = take(input, len)?;
    let mut owned = reserve(len, remaining)?;
    owned.extend_from_slice(bytes);
    Ok(owned)
}

fn string_of(
    input: &mut &[u8],
    len: usize,
    remaining: &mut DecodeBudget<'_>,
) -> Result<String, PackError> {
    String::from_utf8(bytes_of_input(input, len, remaining)?).map_err(|_| PackError::BadUtf8)
}

fn values_of(
    input: &mut &[u8],
    len: usize,
    depth: usize,
    remaining: &mut DecodeBudget<'_>,
) -> Result<Vec<PackValue>, PackError> {
    // Every encoded value needs at least one marker: reject hostile lengths before allocating.
    if input.len() < len {
        return Err(PackError::Truncated);
    }
    let mut items = reserve(len, remaining)?;
    for _ in 0..len {
        items.push(unpack_at(input, depth + 1, remaining)?);
    }
    Ok(items)
}

fn list_of(
    input: &mut &[u8],
    len: usize,
    depth: usize,
    remaining: &mut DecodeBudget<'_>,
) -> Result<PackValue, PackError> {
    Ok(PackValue::List(values_of(input, len, depth, remaining)?))
}

fn map_of(
    input: &mut &[u8],
    len: usize,
    depth: usize,
    remaining: &mut DecodeBudget<'_>,
) -> Result<PackValue, PackError> {
    if input.len() < len.checked_mul(2).ok_or(PackError::Truncated)? {
        return Err(PackError::Truncated);
    }
    let mut entries = reserve(len, remaining)?;
    for _ in 0..len {
        let key = match unpack_at(input, depth + 1, remaining)? {
            PackValue::String(s) => s,
            _ => return Err(PackError::BadMapKey),
        };
        entries.push((key, unpack_at(input, depth + 1, remaining)?));
    }
    Ok(PackValue::Map(entries))
}

/// Decode trusted input without an allocation quota. Network callers use [`unpack_with_limit`].
pub fn unpack(input: &mut &[u8]) -> Result<PackValue, PackError> {
    unpack_with_limit(input, usize::MAX)
}

/// Decode with a shared quota for owned payloads and container backing storage.
/// The root value lives on the stack; nested values are charged by their parent.
/// Allocator overhead, the encoded input and subsequent request processing are excluded.
pub fn unpack_with_limit(
    input: &mut &[u8],
    max_decoded_bytes: usize,
) -> Result<PackValue, PackError> {
    unpack_at(
        input,
        0,
        &mut DecodeBudget {
            remaining: max_decoded_bytes,
            ingress: None,
        },
    )
}

pub(crate) fn unpack_admitted(
    input: &mut &[u8],
    max_decoded_bytes: usize,
    ingress: &mut IngressLease,
) -> Result<PackValue, PackError> {
    unpack_at(
        input,
        0,
        &mut DecodeBudget {
            remaining: max_decoded_bytes,
            ingress: Some(ingress),
        },
    )
}

fn unpack_at(
    input: &mut &[u8],
    depth: usize,
    remaining: &mut DecodeBudget<'_>,
) -> Result<PackValue, PackError> {
    if depth > MAX_DEPTH {
        return Err(PackError::TooDeep);
    }
    let marker = take(input, 1)?[0];
    Ok(match marker {
        0x00..=0x7F => PackValue::Int(marker as i64),
        0xF0..=0xFF => PackValue::Int(marker as i8 as i64),
        0x80..=0x8F => PackValue::String(string_of(input, (marker & 0x0F) as usize, remaining)?),
        0x90..=0x9F => list_of(input, (marker & 0x0F) as usize, depth, remaining)?,
        0xA0..=0xAF => map_of(input, (marker & 0x0F) as usize, depth, remaining)?,
        0xB0..=0xBF => {
            let tag = take(input, 1)?[0];
            let fields = values_of(input, (marker & 0x0F) as usize, depth, remaining)?;
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
            PackValue::Bytes(bytes_of_input(input, len, remaining)?)
        }
        0xD0..=0xD2 => {
            let len = be_len(input, 1 << (marker - 0xD0))?;
            PackValue::String(string_of(input, len, remaining)?)
        }
        0xD4..=0xD6 => {
            let len = be_len(input, 1 << (marker - 0xD4))?;
            list_of(input, len, depth, remaining)?
        }
        0xD8..=0xDA => {
            let len = be_len(input, 1 << (marker - 0xD8))?;
            map_of(input, len, depth, remaining)?
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
    fn response_fragments_match_pack_for_all_markers_and_borrow_payloads() {
        let mut values = vec![
            PackValue::Null,
            PackValue::Bool(false),
            PackValue::Bool(true),
        ];
        for value in [
            i64::MIN,
            i32::MIN as i64 - 1,
            i32::MIN as i64,
            -32769,
            -32768,
            -129,
            -128,
            -17,
            -16,
            0,
            127,
            128,
            255,
            256,
            32767,
            32768,
            i32::MAX as i64,
            i32::MAX as i64 + 1,
            i64::MAX,
        ] {
            values.push(PackValue::Int(value));
        }
        for value in [-0.0, 1.5, f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
            values.push(PackValue::Float(value));
        }
        for len in [0, 1, 15, 16, 255, 256, 65535, 65536] {
            values.push(PackValue::String("x".repeat(len)));
            values.push(PackValue::Bytes(vec![3; len]));
            values.push(PackValue::List(vec![PackValue::Null; len]));
            values.push(PackValue::Map(
                (0..len).map(|_| ("k".into(), PackValue::Null)).collect(),
            ));
        }
        values.push(PackValue::Struct {
            tag: 0x71,
            fields: vec![PackValue::List(values[..25].to_vec())],
        });
        for value in values {
            let mut expected = Vec::new();
            pack(&value, &mut expected);
            let mut actual = Vec::new();
            for fragment in PackFragments::new(&value).unwrap() {
                actual.extend_from_slice(fragment.unwrap().as_slice());
            }
            assert_eq!(actual, expected);
        }
        let value = PackValue::Map(vec![(
            "borrowed key".into(),
            PackValue::String("borrowed value".into()),
        )]);
        let PackValue::Map(entries) = &value else {
            unreachable!()
        };
        let PackValue::String(text) = &entries[0].1 else {
            unreachable!()
        };
        let pointers: Vec<_> = PackFragments::new(&value)
            .unwrap()
            .filter_map(|fragment| match fragment.unwrap() {
                PackFragment::Payload(bytes) => Some(bytes.as_ptr()),
                _ => None,
            })
            .collect();
        assert_eq!(pointers, [entries[0].0.as_ptr(), text.as_ptr()]);
    }

    #[test]
    fn response_fragment_stack_is_depth_bounded_and_errors_are_terminal() {
        let mut nested = PackValue::Null;
        for _ in 0..MAX_ENCODE_DEPTH {
            nested = PackValue::Map(vec![("k".into(), nested)]);
        }
        let mut fragments = PackFragments::new(&nested).unwrap();
        let mut peak_stack = 0;
        while let Some(fragment) = fragments.next() {
            fragment.unwrap();
            peak_stack = peak_stack.max(fragments.stack.len());
        }
        assert!(peak_stack <= 2 * MAX_ENCODE_DEPTH + 3);
        let too_deep = PackValue::List(vec![nested]);
        let mut fragments = PackFragments::new(&too_deep).unwrap();
        let error = fragments.find_map(Result::err);
        assert_eq!(error, Some(PackError::EncodedLimit));
        assert!(fragments.next().is_none());
        let invalid_struct = PackValue::Struct {
            tag: 0x71,
            fields: vec![PackValue::Null; 16],
        };
        assert!(matches!(
            PackFragments::new(&invalid_struct).unwrap().next(),
            Some(Err(PackError::EncodedLimit))
        ));
        if usize::BITS > 32 {
            assert!(matches!(
                size_fragment(usize::MAX, Some(0x80), 0xD0, 0xD1, 0xD2),
                Err(PackError::EncodedLimit)
            ));
        }
    }

    #[test]
    fn decoded_storage_uses_shared_admission_before_allocation() {
        use crate::ingress::IngressPool;
        let value = PackValue::List(vec![PackValue::String("abc".into())]);
        let mut encoded = Vec::new();
        pack(&value, &mut encoded);
        let required = std::mem::size_of::<PackValue>() + 3;
        let pool = IngressPool::new(required);
        let mut a = pool.lease();
        let mut b = pool.lease();
        assert_eq!(
            unpack_admitted(&mut encoded.as_slice(), usize::MAX, &mut a).unwrap(),
            value
        );
        assert_eq!(pool.reserved(), required);
        assert_eq!(
            unpack_admitted(&mut encoded.as_slice(), usize::MAX, &mut b),
            Err(PackError::IngressLimit)
        );
        drop(a);
        assert_eq!(pool.reserved(), 0);
        assert_eq!(
            unpack_admitted(&mut encoded.as_slice(), usize::MAX, &mut b).unwrap(),
            value
        );
        drop(b);
        assert_eq!(pool.reserved(), 0);
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
        let mut input: &[u8] = &[0xA1, 0x01, 0xc0];
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
    #[test]
    fn decoded_scalar_list_budget_has_an_exact_boundary() {
        let value = PackValue::List(vec![PackValue::Null; 1000]);
        let bytes = bytes_of(&value);
        let budget = 1000 * std::mem::size_of::<PackValue>();
        assert!(bytes.len() < budget / 10);
        assert_eq!(
            unpack_with_limit(&mut bytes.as_slice(), budget - 1),
            Err(PackError::DecodedLimit)
        );
        assert_eq!(
            unpack_with_limit(&mut bytes.as_slice(), budget).unwrap(),
            value
        );
    }

    #[test]
    fn decoded_nested_storage_and_payloads_share_one_budget() {
        let value = PackValue::Struct {
            tag: 1,
            fields: vec![PackValue::Map(vec![(
                "key".into(),
                PackValue::List(vec![
                    PackValue::String("hello".into()),
                    PackValue::Bytes(vec![1, 2, 3, 4]),
                ]),
            )])],
        };
        let budget = 3 * std::mem::size_of::<PackValue>()
            + std::mem::size_of::<(String, PackValue)>()
            + 3
            + 5
            + 4;
        let bytes = bytes_of(&value);
        assert_eq!(
            unpack_with_limit(&mut bytes.as_slice(), budget - 1),
            Err(PackError::DecodedLimit)
        );
        assert_eq!(
            unpack_with_limit(&mut bytes.as_slice(), budget).unwrap(),
            value
        );
        assert_eq!(unpack_with_limit(&mut &[0xc0][..], 0), Ok(PackValue::Null));
    }

    #[test]
    fn truncated_container_announcements_are_rejected_before_budget_or_allocation() {
        for bytes in [
            vec![0xd6, 0xff, 0xff, 0xff, 0xff],
            vec![0xda, 0xff, 0xff, 0xff, 0xff],
            vec![0xb1, 1],
        ] {
            assert_eq!(
                unpack_with_limit(&mut bytes.as_slice(), 0),
                Err(PackError::Truncated)
            );
        }
        let mut remaining = DecodeBudget {
            remaining: 10,
            ingress: None,
        };
        assert_eq!(
            charge_storage::<PackValue>(usize::MAX, &mut remaining),
            Err(PackError::DecodedLimit)
        );
        assert_eq!(remaining.remaining, 10);
    }
}
