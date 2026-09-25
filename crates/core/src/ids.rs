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
        assert_eq!(
            NodeId::new(1024, TypeId(1), 0),
            Err(IdError::PartitionOutOfRange)
        );
        assert_eq!(
            NodeId::new(0, TypeId(4096), 0),
            Err(IdError::TypeOutOfRange)
        );
        assert_eq!(
            NodeId::new(0, TypeId(1), 1 << 42),
            Err(IdError::SequenceOutOfRange)
        );
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

    #[test]
    fn edge_id_new_success() {
        let id = EdgeId::new(5, 1000).unwrap();
        assert_eq!(id.0, (5u64 << 54) | 1000);
    }

    #[test]
    fn edge_id_new_partition_out_of_range() {
        assert_eq!(EdgeId::new(1024, 0), Err(IdError::PartitionOutOfRange));
    }

    #[test]
    fn edge_id_new_sequence_out_of_range() {
        assert_eq!(EdgeId::new(0, 1u64 << 54), Err(IdError::SequenceOutOfRange));
    }

    #[test]
    fn edge_id_merged_constant() {
        assert_eq!(EdgeId::MERGED.0, 0);
    }

    #[test]
    fn type_id_constants() {
        assert_eq!(TypeId::UNTYPED.0, 0);
        assert_eq!(TypeId::MAX, (1u16 << 12) - 1);
        assert_eq!(TypeId::MAX, 4095);
    }

    #[test]
    fn parse_element_id_invalid_formats() {
        assert_eq!(NodeId::parse_element_id(""), Err(IdError::BadElementId));
        assert_eq!(NodeId::parse_element_id("n:"), Err(IdError::BadElementId));
        assert_eq!(
            NodeId::parse_element_id("n:not-a-number"),
            Err(IdError::BadElementId)
        );
    }
}
