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

/// Flips the sign bit back so negative numbers sort before positive ones.
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

    #[test]
    fn u16_round_trip() {
        let value = 42u16;
        let mut out = Vec::new();
        put_u16(&mut out, value);
        let mut input = out.as_slice();
        assert_eq!(get_u16(&mut input).unwrap(), value);
        assert!(input.is_empty());
    }

    #[test]
    fn u32_round_trip() {
        let value = 12345u32;
        let mut out = Vec::new();
        put_u32(&mut out, value);
        let mut input = out.as_slice();
        assert_eq!(get_u32(&mut input).unwrap(), value);
        assert!(input.is_empty());
    }

    #[test]
    fn u64_round_trip() {
        let value = 9876543210u64;
        let mut out = Vec::new();
        put_u64(&mut out, value);
        let mut input = out.as_slice();
        assert_eq!(get_u64(&mut input).unwrap(), value);
        assert!(input.is_empty());
    }

    #[test]
    fn i64_round_trip() {
        let value = -42i64;
        let mut out = Vec::new();
        put_i64(&mut out, value);
        let mut input = out.as_slice();
        assert_eq!(get_i64(&mut input).unwrap(), value);
        assert!(input.is_empty());
    }

    proptest! {
        #[test]
        fn u64_order(a: u64, b: u64) {
            let (mut x, mut y) = (Vec::new(), Vec::new());
            put_u64(&mut x, a);
            put_u64(&mut y, b);
            prop_assert_eq!(a.cmp(&b), x.cmp(&y));
            // Also verify round-trip for both values
            let mut input = x.as_slice();
            prop_assert_eq!(get_u64(&mut input).unwrap(), a);
            prop_assert!(input.is_empty());
            let mut input = y.as_slice();
            prop_assert_eq!(get_u64(&mut input).unwrap(), b);
            prop_assert!(input.is_empty());
        }

        #[test]
        fn i64_order(a: i64, b: i64) {
            let (mut x, mut y) = (Vec::new(), Vec::new());
            put_i64(&mut x, a);
            put_i64(&mut y, b);
            prop_assert_eq!(a.cmp(&b), x.cmp(&y));
            // Also verify round-trip for both values
            let mut input = x.as_slice();
            prop_assert_eq!(get_i64(&mut input).unwrap(), a);
            prop_assert!(input.is_empty());
            let mut input = y.as_slice();
            prop_assert_eq!(get_i64(&mut input).unwrap(), b);
            prop_assert!(input.is_empty());
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
