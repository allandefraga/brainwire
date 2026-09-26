//! `ByteSize`: a byte count parsed from and displayed as a human-readable
//! string ("64KiB", "1GB"), for the memory and message-size config keys.

use std::fmt;
use std::str::FromStr;

use serde::de::{self, Deserialize, Deserializer};

/// A byte count. `FromStr` accepts a plain integer or an integer followed by
/// an optional space and a unit: binary (`KiB`, `MiB`, `GiB`, `TiB`, base
/// 1024, case-insensitive) or decimal (`KB`, `MB`, `GB`, `TB`, base 1000).
/// `Display` prints the largest binary unit that divides the value exactly,
/// falling back to plain bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ByteSize(pub u64);

/// Binary units, largest first, checked in this order by both `FromStr` and
/// `Display` so the largest exact unit always wins.
const BINARY_UNITS: &[(&str, u64)] = &[
    ("TiB", 1u64 << 40),
    ("GiB", 1u64 << 30),
    ("MiB", 1u64 << 20),
    ("KiB", 1u64 << 10),
];

/// Decimal units, largest first.
const DECIMAL_UNITS: &[(&str, u64)] = &[
    ("TB", 1_000_000_000_000),
    ("GB", 1_000_000_000),
    ("MB", 1_000_000),
    ("KB", 1_000),
];

/// A `ByteSize` string did not parse: no recognized unit, no digits, or the
/// value overflows `u64` once the unit is applied.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid byte size {0:?}: expected a plain integer or a number with a unit (KiB, MiB, GiB, TiB, KB, MB, GB, TB)")]
pub struct ParseByteSizeError(pub String);

impl FromStr for ByteSize {
    type Err = ParseByteSizeError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let trimmed = input.trim();
        let split_at = trimmed
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(trimmed.len());
        let (digits, rest) = trimmed.split_at(split_at);
        if digits.is_empty() {
            return Err(ParseByteSizeError(input.to_string()));
        }
        let value: u64 = digits
            .parse()
            .map_err(|_| ParseByteSizeError(input.to_string()))?;

        let unit = rest.trim();
        if unit.is_empty() {
            return Ok(ByteSize(value));
        }

        BINARY_UNITS
            .iter()
            .chain(DECIMAL_UNITS.iter())
            .find(|(name, _)| unit.eq_ignore_ascii_case(name))
            .and_then(|(_, multiplier)| value.checked_mul(*multiplier))
            .map(ByteSize)
            .ok_or_else(|| ParseByteSizeError(input.to_string()))
    }
}

impl fmt::Display for ByteSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes = self.0;
        for (name, multiplier) in BINARY_UNITS {
            if bytes != 0 && bytes.is_multiple_of(*multiplier) {
                return write!(f, "{} {}", bytes / multiplier, name);
            }
        }
        write!(f, "{bytes} B")
    }
}

impl<'de> Deserialize<'de> for ByteSize {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ByteSizeVisitor;

        impl de::Visitor<'_> for ByteSizeVisitor {
            type Value = ByteSize;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a byte size, as an integer or a string like \"64KiB\"")
            }

            fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(ByteSize(v))
            }

            fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                u64::try_from(v)
                    .map(ByteSize)
                    .map_err(|_| E::custom(format!("byte size cannot be negative: {v}")))
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                v.parse::<ByteSize>().map_err(|e| E::custom(e.to_string()))
            }
        }

        deserializer.deserialize_any(ByteSizeVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize)]
    struct Wrapper {
        size: ByteSize,
    }

    #[test]
    fn parses_binary_and_decimal_units() {
        assert_eq!("123".parse::<ByteSize>().unwrap().0, 123);
        assert_eq!("64KiB".parse::<ByteSize>().unwrap().0, 64 << 10);
        assert_eq!("512 mib".parse::<ByteSize>().unwrap().0, 512 << 20);
        assert_eq!("10GiB".parse::<ByteSize>().unwrap().0, 10 << 30);
        assert_eq!("1GB".parse::<ByteSize>().unwrap().0, 1_000_000_000);
        assert!("ten".parse::<ByteSize>().is_err());
        assert!("10XB".parse::<ByteSize>().is_err());
        assert!("-1GiB".parse::<ByteSize>().is_err());
    }

    #[test]
    fn displays_the_largest_exact_binary_unit() {
        assert_eq!(ByteSize(10 << 30).to_string(), "10 GiB");
        assert_eq!(ByteSize(1536 << 20).to_string(), "1536 MiB");
        assert_eq!(ByteSize(123).to_string(), "123 B");
    }

    #[test]
    fn empty_and_blank_strings_do_not_parse() {
        assert!("".parse::<ByteSize>().is_err());
        assert!("   ".parse::<ByteSize>().is_err());
        assert!("GiB".parse::<ByteSize>().is_err());
    }

    #[test]
    fn overflow_is_an_error_not_a_panic() {
        assert!("99999999999999TiB".parse::<ByteSize>().is_err());
        assert!("999999999999999999999".parse::<ByteSize>().is_err());
    }

    #[test]
    fn zero_bytes_displays_as_bytes() {
        assert_eq!(ByteSize(0).to_string(), "0 B");
    }

    #[test]
    fn deserializes_from_toml_string_or_integer() {
        let from_string: Wrapper = toml::from_str("size = \"64KiB\"").unwrap();
        assert_eq!(from_string.size.0, 64 << 10);

        let from_int: Wrapper = toml::from_str("size = 12345").unwrap();
        assert_eq!(from_int.size.0, 12345);
    }

    #[test]
    fn deserialize_rejects_bad_input_without_panicking() {
        assert!(toml::from_str::<Wrapper>("size = -5").is_err());
        assert!(toml::from_str::<Wrapper>("size = \"lots\"").is_err());
        assert!(toml::from_str::<Wrapper>("size = true").is_err());
    }
}
