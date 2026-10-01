//! Typed operations for the canonical, guest-resident dictionary allocation.
//!
//! A bucket is addressed by an authenticated dictionary layout and an index.
//! No raw pointer or nested projection is exposed to source lowering. The
//! checker and backend must both enforce the bucket state before touching a
//! key or value token.

use super::{LoadTransfer, StoreTransfer, ValueId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictionaryHeaderField {
    Count,
    Capacity,
    Head,
    Tail,
    Tombstones,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictionarySlotField {
    State,
    Hash,
    Next,
    Prev,
    Key,
    Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictionaryRehashTransfer {
    /// Retain every full-row managed child; source remains live.
    CloneShared,
    /// Transfer every full-row child and consume the source allocation.
    MoveUnique,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DictionaryOperation {
    /// Allocate a fresh unique table and initialize every bucket to empty.
    /// The requested capacity is rounded to a power of two, with minimum 16.
    Initialize { destination: ValueId, layout: crate::layout::LayoutId, capacity: ValueId },
    ReadHeader { destination: ValueId, dict: ValueId, field: DictionaryHeaderField },
    ReadSlot {
        destination: ValueId,
        dict: ValueId,
        bucket: ValueId,
        field: DictionarySlotField,
        transfer: LoadTransfer,
    },
    /// Scan 16 control bytes from `bucket`, wrapping at capacity. Bit zero of
    /// each Size result describes `bucket`. A candidate still requires its
    /// complete stored hash and key comparison before it is a match.
    ProbeGroup {
        matching: ValueId,
        empty: ValueId,
        deleted: ValueId,
        /// Offset of the earliest candidate, empty, or deleted control byte;
        /// 16 means the whole group can be skipped.
        first_event: ValueId,
        dict: ValueId,
        bucket: ValueId,
        fingerprint: ValueId,
    },
    InsertSlot {
        dict: ValueId,
        bucket: ValueId,
        key: ValueId,
        value: ValueId,
        hash: ValueId,
        key_transfer: StoreTransfer,
        value_transfer: StoreTransfer,
    },
    ReplaceValue {
        displaced: ValueId,
        dict: ValueId,
        bucket: ValueId,
        value: ValueId,
        transfer: StoreTransfer,
    },
    RemoveSlot { removed_value: ValueId, dict: ValueId, bucket: ValueId },
    /// Copy the exact bucket layout and order, retaining each full-row child.
    /// A lookup result from the source remains valid in the destination.
    CloneExact { destination: ValueId, source: ValueId, capacity: ValueId },
    Rehash {
        destination: ValueId,
        source: ValueId,
        capacity: ValueId,
        transfer: DictionaryRehashTransfer,
    },
}

impl DictionaryOperation {
    pub fn destinations(&self) -> Vec<ValueId> {
        match self {
            Self::Initialize { destination, .. }
            | Self::ReadHeader { destination, .. }
            | Self::ReadSlot { destination, .. }
            | Self::CloneExact { destination, .. }
            | Self::Rehash { destination, .. } => vec![*destination],
            Self::ProbeGroup { matching, empty, deleted, first_event, .. } =>
                vec![*matching, *empty, *deleted, *first_event],
            Self::ReplaceValue { displaced, .. } => vec![*displaced],
            Self::RemoveSlot { removed_value, .. } => vec![*removed_value],
            Self::InsertSlot { .. } => Vec::new(),
        }
    }

    pub fn operands(&self) -> Vec<ValueId> {
        match self {
            Self::Initialize { capacity, .. } => vec![*capacity],
            Self::ReadHeader { dict, .. } => vec![*dict],
            Self::ReadSlot { dict, bucket, .. }
            | Self::RemoveSlot { dict, bucket, .. } => vec![*dict, *bucket],
            Self::ProbeGroup { dict, bucket, fingerprint, .. } =>
                vec![*dict, *bucket, *fingerprint],
            Self::InsertSlot { dict, bucket, key, value, hash, .. } =>
                vec![*dict, *bucket, *key, *value, *hash],
            Self::ReplaceValue { dict, bucket, value, .. } => vec![*dict, *bucket, *value],
            Self::CloneExact { source, capacity, .. } => vec![*source, *capacity],
            Self::Rehash { source, capacity, .. } => vec![*source, *capacity],
        }
    }
}
