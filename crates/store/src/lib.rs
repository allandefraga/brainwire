//! Brainwire storage core. See docs/superpowers/specs/2026-09-24-brainwire-design.md.

pub mod alloc;
pub mod db;
pub mod graph;
pub mod txn;

#[cfg(test)]
mod smoke {
    #[test]
    fn builds() {}
}
