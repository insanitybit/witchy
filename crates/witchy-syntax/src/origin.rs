//! Persistent source provenance for generated AST nodes (RFC-0080).
//!
//! The runtime AST stays representation-neutral: origins live in this side
//! table and address nodes structurally. IDs and paths are compiler-owned typed
//! values; rendered source is never used as identity.

#[cfg_attr(not(target_arch = "wasm32"), derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePosition {
    /// One-based source line. Zero means the producer had no location.
    pub line: u32,
    /// One-based source column. Zero means the producer had no column.
    pub column: u32,
}

#[cfg_attr(not(target_arch = "wasm32"), derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSpan {
    pub module: String,
    pub start: SourcePosition,
    pub end: SourcePosition,
}

impl SourceSpan {
    pub fn line(module: impl Into<String>, line: u32) -> Self {
        Self {
            module: module.into(),
            start: SourcePosition { line, column: if line == 0 { 0 } else { 1 } },
            end: SourcePosition { line, column: if line == 0 { 0 } else { 1 } },
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxCategory {
    Item,
    Expr,
    Type,
    Pattern,
    Statement,
    Block,
}

/// One syntax hole crossed while producing a generated node. Outer holes come
/// first, making nested expansion traces deterministic without message parsing.
#[cfg_attr(not(target_arch = "wasm32"), derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoleOrigin {
    pub category: SyntaxCategory,
    pub definition: SourceSpan,
    pub invocation: SourceSpan,
}

#[cfg_attr(not(target_arch = "wasm32"), derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpansionOrigin {
    pub definition: SourceSpan,
    pub invocation: SourceSpan,
    pub hole_ancestry: Vec<HoleOrigin>,
}

#[cfg_attr(not(target_arch = "wasm32"), derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GeneratedNodeId {
    /// The source module performing expansion, not a source-text fingerprint.
    pub module: String,
    /// Allocation order within that module's deterministic expansion walk.
    pub ordinal: u32,
}

/// Structural address in the expanded AST. `path` is empty for an item root;
/// nested-node producers append deterministic child ordinals as they walk.
#[cfg_attr(not(target_arch = "wasm32"), derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedNodePath {
    pub item: u32,
    pub path: Vec<u32>,
    pub category: SyntaxCategory,
}

#[cfg_attr(not(target_arch = "wasm32"), derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedNodeOrigin {
    pub id: GeneratedNodeId,
    pub node: GeneratedNodePath,
    pub origin: ExpansionOrigin,
}

#[cfg_attr(not(target_arch = "wasm32"), derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OriginTable {
    nodes: Vec<GeneratedNodeOrigin>,
    next_ordinals: std::collections::BTreeMap<String, u32>,
}

impl OriginTable {
    pub fn nodes(&self) -> &[GeneratedNodeOrigin] {
        &self.nodes
    }

    pub fn origin_for_item(&self, item: usize) -> Option<&GeneratedNodeOrigin> {
        self.nodes.iter().find(|entry| {
            entry.node.item as usize == item
                && entry.node.path.is_empty()
                && entry.node.category == SyntaxCategory::Item
        })
    }

    pub fn record_item(
        &mut self,
        module: &str,
        item: usize,
        origin: ExpansionOrigin,
    ) -> GeneratedNodeId {
        self.record(
            module,
            GeneratedNodePath {
                item: item_index(item),
                path: Vec::new(),
                category: SyntaxCategory::Item,
            },
            origin,
        )
    }

    pub fn record(
        &mut self,
        module: &str,
        node: GeneratedNodePath,
        origin: ExpansionOrigin,
    ) -> GeneratedNodeId {
        let id = self.allocate_id(module);
        self.nodes.push(GeneratedNodeOrigin { id: id.clone(), node, origin });
        id
    }

    fn allocate_id(&mut self, module: &str) -> GeneratedNodeId {
        let next = self.next_ordinals.entry(module.to_string()).or_default();
        let id = GeneratedNodeId { module: module.to_string(), ordinal: *next };
        *next = (*next).checked_add(1).expect("generated-origin ordinal overflow");
        id
    }

    /// Append another table after its item indices have moved by `item_offset`.
    /// IDs are deterministically rebased into this table's allocation order.
    pub fn append_shifted(&mut self, mut other: OriginTable, item_offset: usize) {
        let item_offset = item_index(item_offset);
        for entry in &mut other.nodes {
            entry.node.item = entry
                .node
                .item
                .checked_add(item_offset)
                .expect("generated-origin item index overflow");
            let module = entry.id.module.clone();
            entry.id = self.allocate_id(&module);
        }
        self.nodes.append(&mut other.nodes);
    }

    /// Remap item addresses after an AST pass. One input may expand to several
    /// outputs; the first keeps its ID and additional outputs receive new IDs.
    pub fn remap_items(&mut self, module: &str, mapping: &[Vec<usize>]) {
        let old = std::mem::take(&mut self.nodes);
        let mut remapped = Vec::new();
        for entry in old {
            let Some(targets) = mapping.get(entry.node.item as usize) else { continue };
            for (copy, target) in targets.iter().enumerate() {
                let mut moved = entry.clone();
                moved.node.item = item_index(*target);
                if copy > 0 {
                    moved.id = self.allocate_id(module);
                }
                remapped.push(moved);
            }
        }
        self.nodes = remapped;
    }

    pub fn retain_items(&mut self, module: &str, retained: &[bool]) {
        let mut mapping = vec![Vec::new(); retained.len()];
        let mut next = 0usize;
        for (old, keep) in retained.iter().copied().enumerate() {
            if keep {
                mapping[old].push(next);
                next += 1;
            }
        }
        self.remap_items(module, &mapping);
    }
}

fn item_index(item: usize) -> u32 {
    u32::try_from(item).expect("generated-origin item index overflow")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace(module: &str, definition: u32, invocation: u32) -> ExpansionOrigin {
        ExpansionOrigin {
            definition: SourceSpan::line(module, definition),
            invocation: SourceSpan::line(module, invocation),
            hole_ancestry: vec![HoleOrigin {
                category: SyntaxCategory::Expr,
                definition: SourceSpan::line(module, definition + 1),
                invocation: SourceSpan::line(module, invocation + 1),
            }],
        }
    }

    #[test]
    fn remapping_is_typed_deterministic_and_preserves_hole_ancestry() {
        let mut table = OriginTable::default();
        table.record_item("main", 1, trace("main", 3, 8));
        table.remap_items("main", &[vec![], vec![2, 3]]);

        assert_eq!(table.nodes.len(), 2);
        assert_eq!(table.nodes[0].node.item, 2);
        assert_eq!(table.nodes[1].node.item, 3);
        assert_ne!(table.nodes[0].id, table.nodes[1].id);
        assert_eq!(table.nodes[0].origin.hole_ancestry.len(), 1);
        assert_eq!(table.nodes[0].origin.definition.start.line, 3);
        assert_eq!(table.nodes[0].origin.invocation.start.line, 8);
    }

    #[test]
    fn appending_batches_allocates_unique_ids_without_source_identity() {
        let mut first = OriginTable::default();
        first.record_item("main", 0, trace("main", 2, 5));
        let mut second = OriginTable::default();
        second.record_item("main", 0, trace("main", 9, 12));
        first.append_shifted(second, 4);

        assert_eq!(first.nodes[0].id.ordinal, 0);
        assert_eq!(first.nodes[1].id.ordinal, 1);
        assert_eq!(first.nodes[1].node.item, 4);
    }
}
