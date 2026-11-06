//! Brainwire storage core. See docs/superpowers/specs/2026-09-24-brainwire-design.md.

pub mod alloc;
pub mod catalog;
pub mod db;
pub mod graph;
mod relationship_index;
pub mod replicated_apply;
pub mod txn;

#[cfg(test)]
mod smoke {
    #[test]
    fn builds() {}
}
