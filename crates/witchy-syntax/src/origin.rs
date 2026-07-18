//! Persistent source provenance for generated AST nodes (RFC-0080).
//!
//! The runtime AST stays representation-neutral: origins live in this side
//! table and address nodes structurally. IDs and paths are compiler-owned typed
//! values; rendered source is never used as identity.

use crate::ast::{Block, Expr, Function, Item, MatchArm, Param, Pattern, Stmt, Type};

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

    /// Return the persistent expansion origin for one structural node address.
    /// A generated node's source provenance is intentionally independent of the
    /// AST representation, so diagnostics and tooling can retain this lookup
    /// across linking and lowerings that leave the address intact.
    pub fn origin_for_node(&self, node: &GeneratedNodePath) -> Option<&GeneratedNodeOrigin> {
        self.nodes.iter().find(|entry| &entry.node == node)
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

    /// Record one generated item and every nested expression, type, pattern,
    /// statement, and block it owns. Child paths are DFS ordinals, not source
    /// offsets: they remain deterministic for compiler-owned AST payloads and
    /// never depend on formatting.
    pub fn record_item_tree(
        &mut self,
        module: &str,
        item_position: usize,
        item: &Item,
        origin: ExpansionOrigin,
    ) -> GeneratedNodeId {
        let item_number = item_index(item_position);
        let id = self.record(
            module,
            GeneratedNodePath { item: item_number, path: Vec::new(), category: SyntaxCategory::Item },
            origin.clone(),
        );
        let mut children = 0;
        record_item_children(self, module, item_number, item, &mut children, &origin);
        id
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

fn child_path(parent: &[u32], next: &mut u32) -> Vec<u32> {
    let mut path = parent.to_vec();
    path.push(*next);
    *next = next.checked_add(1).expect("generated-origin child ordinal overflow");
    path
}

fn record_node(
    table: &mut OriginTable,
    module: &str,
    item: u32,
    path: Vec<u32>,
    category: SyntaxCategory,
    origin: &ExpansionOrigin,
) {
    table.record(module, GeneratedNodePath { item, path, category }, origin.clone());
}

fn record_item_children(
    table: &mut OriginTable,
    module: &str,
    item_index: u32,
    item: &Item,
    next: &mut u32,
    origin: &ExpansionOrigin,
) {
    match item {
        Item::Function(function) => record_function(table, module, item_index, &[], function, next, origin),
        Item::Type(def) => {
            for variant in &def.variants {
                for ty in &variant.fields {
                    let path = child_path(&[], next);
                    record_type(table, module, item_index, &path, ty, origin);
                }
            }
        }
        Item::Trait(def) => {
            for method in &def.methods {
                for param in &method.params {
                    let path = child_path(&[], next);
                    record_param(table, module, item_index, &path, param, origin);
                }
                if let Some(ret) = &method.ret {
                    let path = child_path(&[], next);
                    record_type(table, module, item_index, &path, ret, origin);
                }
                if let Some(body) = &method.default {
                    let path = child_path(&[], next);
                    record_block(table, module, item_index, &path, body, origin);
                }
            }
        }
        Item::Impl(def) => {
            for ty in def.trait_args.iter().chain(def.target_args.iter()) {
                let path = child_path(&[], next);
                record_type(table, module, item_index, &path, ty, origin);
            }
            for (_, _, args) in &def.bounds {
                for ty in args {
                    let path = child_path(&[], next);
                    record_type(table, module, item_index, &path, ty, origin);
                }
            }
            for method in &def.methods {
                let path = child_path(&[], next);
                record_function(table, module, item_index, &path, method, next, origin);
            }
        }
        Item::Const { value, .. } => {
            let path = child_path(&[], next);
            record_expr(table, module, item_index, &path, value, origin);
        }
        Item::TypeAlias { ty, .. } => {
            let path = child_path(&[], next);
            record_type(table, module, item_index, &path, ty, origin);
        }
        Item::Comptime(block) => {
            let path = child_path(&[], next);
            record_block(table, module, item_index, &path, block, origin);
        }
    }
}

fn record_function(table: &mut OriginTable, module: &str, item: u32, path: &[u32], function: &Function, next: &mut u32, origin: &ExpansionOrigin) {
    for param in &function.params {
        let child = child_path(path, next);
        record_param(table, module, item, &child, param, origin);
    }
    if let Some(ret) = &function.ret {
        let child = child_path(path, next);
        record_type(table, module, item, &child, ret, origin);
    }
    for (_, _, args) in &function.bounds {
        for ty in args {
            let child = child_path(path, next);
            record_type(table, module, item, &child, ty, origin);
        }
    }
    let child = child_path(path, next);
    record_block(table, module, item, &child, &function.body, origin);
}

fn record_param(table: &mut OriginTable, module: &str, item: u32, path: &[u32], param: &Param, origin: &ExpansionOrigin) {
    if let Some(ty) = &param.ty {
        let mut next = 0;
        let child = child_path(path, &mut next);
        record_type(table, module, item, &child, ty, origin);
    }
    if let Some(default) = &param.default {
        let mut next = 1;
        let child = child_path(path, &mut next);
        record_expr(table, module, item, &child, default, origin);
    }
}

fn record_type(table: &mut OriginTable, module: &str, item: u32, path: &[u32], ty: &Type, origin: &ExpansionOrigin) {
    record_node(table, module, item, path.to_vec(), SyntaxCategory::Type, origin);
    let mut next = 0;
    match ty {
        Type::Named(_, args) | Type::Dyn(_, args) | Type::Tuple(args) => {
            for ty in args { let child = child_path(path, &mut next); record_type(table, module, item, &child, ty, origin); }
        }
        Type::Fn(params, ret, _) => {
            for ty in params { let child = child_path(path, &mut next); record_type(table, module, item, &child, ty, origin); }
            let child = child_path(path, &mut next); record_type(table, module, item, &child, ret, origin);
        }
        Type::Qualified(_, inner) => { let child = child_path(path, &mut next); record_type(table, module, item, &child, inner, origin); }
    }
}

fn record_block(table: &mut OriginTable, module: &str, item: u32, path: &[u32], block: &Block, origin: &ExpansionOrigin) {
    record_node(table, module, item, path.to_vec(), SyntaxCategory::Block, origin);
    let mut next = 0;
    for stmt in &block.stmts { let child = child_path(path, &mut next); record_stmt(table, module, item, &child, stmt, origin); }
}

fn record_stmt(table: &mut OriginTable, module: &str, item: u32, path: &[u32], stmt: &Stmt, origin: &ExpansionOrigin) {
    record_node(table, module, item, path.to_vec(), SyntaxCategory::Statement, origin);
    let mut next = 0;
    match stmt {
        Stmt::Let { ty, value, .. } => {
            if let Some(ty) = ty { let child = child_path(path, &mut next); record_type(table, module, item, &child, ty, origin); }
            let child = child_path(path, &mut next); record_expr(table, module, item, &child, value, origin);
        }
        Stmt::Assign { value, .. } | Stmt::Yield(value) | Stmt::Expr(value) => { let child = child_path(path, &mut next); record_expr(table, module, item, &child, value, origin); }
        Stmt::LetPattern { pattern, value } => { let child = child_path(path, &mut next); record_pattern(table, module, item, &child, pattern, origin); let child = child_path(path, &mut next); record_expr(table, module, item, &child, value, origin); }
        Stmt::Return(Some(value)) => { let child = child_path(path, &mut next); record_expr(table, module, item, &child, value, origin); }
        Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
    }
}

fn record_expr(table: &mut OriginTable, module: &str, item: u32, path: &[u32], expr: &Expr, origin: &ExpansionOrigin) {
    record_node(table, module, item, path.to_vec(), SyntaxCategory::Expr, origin);
    let mut next = 0;
    match expr {
        Expr::List(values) | Expr::Tuple(values) => for value in values { record_expr_child(table, module, item, path, &mut next, value, origin); },
        Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => for arg in args { record_expr_child(table, module, item, path, &mut next, arg, origin); },
        Expr::LabeledCall { args, .. } => for (_, arg) in args { record_expr_child(table, module, item, path, &mut next, arg, origin); },
        Expr::MethodCall { receiver, args, .. } => { record_expr_child(table, module, item, path, &mut next, receiver, origin); for arg in args { record_expr_child(table, module, item, path, &mut next, arg, origin); } }
        Expr::Apply { func, args } => { record_expr_child(table, module, item, path, &mut next, func, origin); for arg in args { record_expr_child(table, module, item, path, &mut next, arg, origin); } }
        Expr::Unary { expr, .. } | Expr::Try(expr) => record_expr_child(table, module, item, path, &mut next, expr, origin),
        Expr::Field { base, .. } => record_expr_child(table, module, item, path, &mut next, base, origin),
        Expr::Lambda { params, body, ret } => { for param in params { let child = child_path(path, &mut next); record_param(table, module, item, &child, param, origin); } if let Some(ret) = ret { let child = child_path(path, &mut next); record_type(table, module, item, &child, ret, origin); } let child = child_path(path, &mut next); record_block(table, module, item, &child, body, origin); }
        Expr::RecordUpdate { base, fields, .. } => { record_expr_child(table, module, item, path, &mut next, base, origin); for (_, value) in fields { record_expr_child(table, module, item, path, &mut next, value, origin); } }
        Expr::Record { fields, spread, .. } => { for (_, value) in fields { record_expr_child(table, module, item, path, &mut next, value, origin); } if let Some(spread) = spread { record_expr_child(table, module, item, path, &mut next, spread, origin); } }
        Expr::As { expr, ty } | Expr::ExistentialPack { expr, ty, .. } => { record_expr_child(table, module, item, path, &mut next, expr, origin); let child = child_path(path, &mut next); record_type(table, module, item, &child, ty, origin); }
        Expr::ExistentialCall { receiver, args, ty, result, .. } => {
            record_expr_child(table, module, item, path, &mut next, receiver, origin);
            for arg in args {
                record_expr_child(table, module, item, path, &mut next, arg, origin);
            }
            let child = child_path(path, &mut next);
            record_type(table, module, item, &child, ty, origin);
            let child = child_path(path, &mut next);
            record_type(table, module, item, &child, result, origin);
        }
        Expr::Binary { lhs, rhs, .. } | Expr::Range { lo: lhs, hi: rhs, .. } => { record_expr_child(table, module, item, path, &mut next, lhs, origin); record_expr_child(table, module, item, path, &mut next, rhs, origin); }
        Expr::If { cond, then_block, else_block } => { record_expr_child(table, module, item, path, &mut next, cond, origin); let child = child_path(path, &mut next); record_block(table, module, item, &child, then_block, origin); if let Some(block) = else_block { let child = child_path(path, &mut next); record_block(table, module, item, &child, block, origin); } }
        Expr::Match { scrutinee, arms } => { record_expr_child(table, module, item, path, &mut next, scrutinee, origin); for arm in arms { let child = child_path(path, &mut next); record_arm(table, module, item, &child, arm, origin); } }
        Expr::Block(block) => { let child = child_path(path, &mut next); record_block(table, module, item, &child, block, origin); }
        Expr::While { cond, body } => { record_expr_child(table, module, item, path, &mut next, cond, origin); let child = child_path(path, &mut next); record_block(table, module, item, &child, body, origin); }
        Expr::For { iter, body, .. } => { record_expr_child(table, module, item, path, &mut next, iter, origin); let child = child_path(path, &mut next); record_block(table, module, item, &child, body, origin); }
        Expr::Index { base, index } => { record_expr_child(table, module, item, path, &mut next, base, origin); record_expr_child(table, module, item, path, &mut next, index, origin); }
        Expr::WhileLet { pattern, scrutinee, body } => { let child = child_path(path, &mut next); record_pattern(table, module, item, &child, pattern, origin); record_expr_child(table, module, item, path, &mut next, scrutinee, origin); let child = child_path(path, &mut next); record_block(table, module, item, &child, body, origin); }
        Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Var(_) | Expr::TaggedLit { .. } => {}
    }
}

fn record_expr_child(table: &mut OriginTable, module: &str, item: u32, parent: &[u32], next: &mut u32, expr: &Expr, origin: &ExpansionOrigin) {
    let child = child_path(parent, next);
    record_expr(table, module, item, &child, expr, origin);
}

fn record_arm(table: &mut OriginTable, module: &str, item: u32, path: &[u32], arm: &MatchArm, origin: &ExpansionOrigin) {
    let mut next = 0;
    let child = child_path(path, &mut next); record_pattern(table, module, item, &child, &arm.pattern, origin);
    if let Some(guard) = &arm.guard { let child = child_path(path, &mut next); record_expr(table, module, item, &child, guard, origin); }
    let child = child_path(path, &mut next); record_expr(table, module, item, &child, &arm.body, origin);
}

fn record_pattern(table: &mut OriginTable, module: &str, item: u32, path: &[u32], pattern: &Pattern, origin: &ExpansionOrigin) {
    record_node(table, module, item, path.to_vec(), SyntaxCategory::Pattern, origin);
    let mut next = 0;
    match pattern {
        Pattern::Ctor { args, .. } | Pattern::AnonCtor { args, .. } | Pattern::Tuple(args) | Pattern::Or(args) => for pattern in args { let child = child_path(path, &mut next); record_pattern(table, module, item, &child, pattern, origin); },
        Pattern::List { elems, .. } => for pattern in elems { let child = child_path(path, &mut next); record_pattern(table, module, item, &child, pattern, origin); },
        Pattern::Wildcard | Pattern::Var(_) | Pattern::Int(_) | Pattern::Str(_) | Pattern::Bool(_) | Pattern::Duration(_) | Pattern::IntRange { .. } => {}
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

    #[test]
    fn item_tree_records_nested_syntax_with_stable_structural_paths() {
        let module = crate::parser::parse_module(
            "fn calculate(xs: List(Int)) -> Int:\n    let first: Int = xs[0]\n    first\n",
        )
        .expect("parse generated item fixture");
        let mut table = OriginTable::default();
        table.record_item_tree("generated", 0, &module.items[0], trace("generated", 2, 7));

        assert!(table.origin_for_item(0).is_some());
        assert!(table.nodes.iter().any(|node| node.node.category == SyntaxCategory::Block));
        assert!(table.nodes.iter().any(|node| node.node.category == SyntaxCategory::Statement));
        assert!(table.nodes.iter().any(|node| node.node.category == SyntaxCategory::Expr));
        assert!(table.nodes.iter().any(|node| node.node.category == SyntaxCategory::Type));
        let mut next = 0;
        record_pattern(
            &mut table,
            "generated",
            0,
            &child_path(&[], &mut next),
            &Pattern::Ctor { name: "Pair".into(), args: vec![Pattern::Var("x".into())] },
            &trace("generated", 2, 7),
        );
        assert!(table.nodes.iter().any(|node| node.node.category == SyntaxCategory::Pattern));

        let nested = table
            .nodes
            .iter()
            .find(|node| node.node.category == SyntaxCategory::Expr && !node.node.path.is_empty())
            .expect("nested expression origin");
        assert_eq!(table.origin_for_node(&nested.node).expect("lookup by path").id, nested.id);
        assert_eq!(nested.origin.invocation.start.line, 7);
    }
}
