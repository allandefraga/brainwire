//! Brainwire storage core. See docs/superpowers/specs/2026-09-24-brainwire-design.md.

pub mod codec;
pub mod ids;
pub mod keys;
pub mod value;

#[cfg(test)]
mod smoke {
    #[test]
    fn builds() {}
}
