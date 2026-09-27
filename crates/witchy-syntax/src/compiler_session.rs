//! Runtime-neutral storage for typed compile-time syntax and its provenance.
//!
//! This is compiler data, not an admission proof or guest authority. Callers
//! must authenticate compiler intrinsics and decode their guest values before
//! using it. Handles retain the existing spelling and are meaningful only in
//! the matching category of the current session. No source text is evaluated.
//! Fresh sessions can reuse the same ordinal strings. These are not globally
//! unique tokens; a future guest transport must bind them to its session and
//! cannot authenticate or replay syntax from a handle string alone.
//! The frozen interpreter remains the default execution adapter. An explicit
//! admitted compiler-session runner currently supports deterministic fresh
//! names and bounded expression quotation/emission; other quotation categories
//! and builders still require migration.

use std::collections::HashMap;

use crate::ast::{Block, Expr, Item, MatchArm, Module, Param, Pattern, Stmt, Type};
use crate::origin::SyntaxCategory;

pub mod seeds;
pub mod type_expr_wire;
pub mod expr_compose_wire;
pub mod block_wire;
pub mod function_wire;

/// Data carried by the checked source launch contract and admitted artifact.
/// Constructing this value alone grants no import or launch authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompilerSessionContract { source: [u8; 32], scope: String }

impl CompilerSessionContract {
    pub fn for_source(source: [u8; 32], scope: String) -> Result<Self, String> {
        if scope.is_empty() || scope.len() > 4096 {
            return Err("compiler invocation scope must contain 1..=4096 UTF-8 bytes".into());
        }
        Ok(Self { source, scope })
    }
    pub fn source_digest(&self) -> [u8; 32] { self.source }
    pub fn scope(&self) -> &str { &self.scope }
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = vec![3, 1]; // launch version, compiler syntax session
        bytes.extend_from_slice(&self.source);
        bytes.extend_from_slice(&(self.scope.len() as u32).to_le_bytes());
        bytes.extend_from_slice(self.scope.as_bytes());
        bytes
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 38 || bytes[..2] != [3, 1] { return None; }
        let length = u32::from_le_bytes(bytes[34..38].try_into().ok()?) as usize;
        if length == 0 || length > 4096 || length != bytes.len() - 38 { return None; }
        let scope = std::str::from_utf8(&bytes[38..]).ok()?;
        Self::for_source(bytes[2..34].try_into().ok()?, scope.into()).ok()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ComptimeItemEmission {
    ModuleSyntax {
        module: Box<Module>,
        definition_line: u32,
        hole_ancestry: Vec<ComptimeHoleOrigin>,
    },
    Syntax {
        item: Box<Item>,
        definition_line: u32,
        hole_ancestry: Vec<ComptimeHoleOrigin>,
    },
}

#[cfg_attr(not(target_arch = "wasm32"), derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, PartialEq)]
pub struct ComptimeHoleOrigin {
    pub category: SyntaxCategory,
    pub definition_line: u32,
    pub invocation_line: u32,
}

#[cfg_attr(not(target_arch = "wasm32"), derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, PartialEq)]
pub struct ComptimeSyntaxOrigin {
    pub definition_line: u32,
    pub hole_ancestry: Vec<ComptimeHoleOrigin>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ComptimeExprEmission {
    Syntax(Box<Expr>),
}

pub struct ComptimeOutputs {
    pub output: Vec<String>,
    pub items: Vec<PositionedComptimeItem>,
    pub exprs: Vec<ComptimeExprEmission>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PositionedComptimeItem {
    pub output_position: usize,
    pub emission: ComptimeItemEmission,
}

/// A host-owned invocation's syntax. No runtime `Value`, evaluator, grants,
/// filesystem access or inference table is retained here. Fresh sessions keep
/// batched entries isolated, including their deterministic name sequences.
#[derive(Default)]
pub struct CompilerSyntaxSession {
    fresh_ident_scope: Option<String>,
    fresh_ident_counter: u64,
    syntax_instance_counter: u64,
    item_syntax: HashMap<String, Item>,
    item_modules: HashMap<String, Module>,
    expr_syntax: HashMap<String, Expr>,
    type_syntax: HashMap<String, Type>,
    pattern_syntax: HashMap<String, Pattern>,
    match_arm_syntax: HashMap<String, MatchArm>,
    param_syntax: HashMap<String, Param>,
    stmt_syntax: HashMap<String, Stmt>,
    block_syntax: HashMap<String, Block>,
    expr_origins: HashMap<String, ComptimeSyntaxOrigin>,
    type_origins: HashMap<String, ComptimeSyntaxOrigin>,
    pattern_origins: HashMap<String, ComptimeSyntaxOrigin>,
    stmt_origins: HashMap<String, ComptimeSyntaxOrigin>,
    block_origins: HashMap<String, ComptimeSyntaxOrigin>,
    /// Module qualifiers made available by the tagged-literal invocation.
    expr_qualifiers: Option<Vec<String>>,
    /// Exact opaque sentinels issued for the active tagged literal, not an
    /// open namespace of user-supplied hole names.
    tag_hole_markers: Option<Vec<String>>,
    item_output: Vec<PositionedComptimeItem>,
    expr_output: Vec<ComptimeExprEmission>,
}

// Typed host insertion and immutable lookup only: adapters cannot borrow a
// mutable map or erase its category. Insertion does not confer guest authority.
macro_rules! syntax_arena {
    ($read:ident, $insert:ident, $field:ident, $value:ty) => {
        pub fn $read(&self) -> &HashMap<String, $value> { &self.$field }
        pub fn $insert(&mut self, handle: String, value: $value) {
            self.$field.insert(handle, value);
        }
    };
}

impl CompilerSyntaxSession {
    /// Move parser/compiler payloads, leaving ordinary source items untouched.
    /// As before, a duplicate payload handle keeps its final occurrence. This
    /// transfer itself authenticates neither the module nor its payloads.
    pub fn take_module_payloads(module: &mut Module) -> Self {
        Self {
            item_syntax: std::mem::take(&mut module.compiler_item_syntax).into_iter()
                .map(|syntax| (syntax.handle, syntax.item)).collect(),
            expr_syntax: std::mem::take(&mut module.compiler_expr_syntax).into_iter()
                .map(|syntax| (syntax.handle, syntax.expr)).collect(),
            type_syntax: std::mem::take(&mut module.compiler_type_syntax).into_iter()
                .map(|syntax| (syntax.handle, syntax.ty)).collect(),
            pattern_syntax: std::mem::take(&mut module.compiler_pattern_syntax).into_iter()
                .map(|syntax| (syntax.handle, syntax.pattern)).collect(),
            stmt_syntax: std::mem::take(&mut module.compiler_stmt_syntax).into_iter()
                .map(|syntax| (syntax.handle, syntax.stmt)).collect(),
            block_syntax: std::mem::take(&mut module.compiler_block_syntax).into_iter()
                .map(|syntax| (syntax.handle, syntax.block)).collect(),
            fresh_ident_scope: None,
            fresh_ident_counter: 0,
            syntax_instance_counter: 0,
            item_modules: HashMap::new(),
            match_arm_syntax: HashMap::new(),
            param_syntax: HashMap::new(),
            expr_origins: HashMap::new(),
            type_origins: HashMap::new(),
            pattern_origins: HashMap::new(),
            stmt_origins: HashMap::new(),
            block_origins: HashMap::new(),
            expr_qualifiers: None,
            tag_hole_markers: None,
            item_output: Vec::new(),
            expr_output: Vec::new(),
        }
    }

    pub fn set_invocation(
        &mut self, scope: Option<String>, qualifiers: Option<Vec<String>>,
        hole_markers: Option<Vec<String>>,
    ) {
        self.fresh_ident_scope = scope;
        self.expr_qualifiers = qualifiers;
        self.tag_hole_markers = hole_markers;
    }

    pub fn fresh_ident_scope(&self) -> &Option<String> { &self.fresh_ident_scope }
    pub fn expr_qualifiers(&self) -> &Option<Vec<String>> { &self.expr_qualifiers }
    pub fn tag_hole_markers(&self) -> &Option<Vec<String>> { &self.tag_hole_markers }

    pub fn next_fresh_ident(&mut self, hint: &str) -> Result<String, String> {
        let name = self.preview_fresh_ident(hint)?;
        self.fresh_ident_counter += 1; // preview checked overflow
        Ok(name)
    }

    /// Nonmutating capacity probe for the bounded guest-buffer ABI. A probe
    /// cannot consume an ordinal; only the successful write commits it.
    pub fn preview_fresh_ident(&self, hint: &str) -> Result<String, String> {
        let Some(scope) = self.fresh_ident_scope.as_deref() else {
            return Err("meta.fresh is available only during compile-time expansion".into());
        };
        let ordinal = self.fresh_ident_counter;
        self.fresh_ident_counter.checked_add(1)
            .ok_or_else(|| "meta.fresh identifier counter overflowed".to_string())?;
        Ok(format!("__witchy_fresh_{}_{ordinal}_{hint}", encode_fresh_scope(scope)))
    }

    /// Exact output length without inspecting/copying the guest hint or
    /// allocating formatted output. Hosts use this before fuel/allocation.
    pub fn fresh_ident_byte_len(&self, hint_bytes: usize) -> Result<usize, String> {
        let scope = self.fresh_ident_scope.as_ref().ok_or_else(||
            "meta.fresh is available only during compile-time expansion".to_string())?;
        self.fresh_ident_counter.checked_add(1)
            .ok_or_else(|| "meta.fresh identifier counter overflowed".to_string())?;
        let digits = self.fresh_ident_counter.checked_ilog10().unwrap_or(0) as usize + 1;
        scope.len().checked_mul(2)
            .and_then(|n| n.checked_add("__witchy_fresh_".len() + 2 + digits))
            .and_then(|n| n.checked_add(hint_bytes))
            .ok_or_else(|| "meta.fresh identifier length overflowed".to_string())
    }

    /// Reserve the next host handle. Its category string preserves diagnostic
    /// identity; only lookup in the typed arena transfers a syntax payload.
    pub fn next_syntax_handle(&mut self, category: &str) -> Result<String, String> {
        let ordinal = self.syntax_instance_counter;
        self.syntax_instance_counter = self.syntax_instance_counter.checked_add(1)
            .ok_or_else(|| "compiler syntax instance counter overflowed".to_string())?;
        Ok(format!("\0compiler-syntax-instance\0{category}\0{ordinal}"))
    }

    pub fn syntax_handle_byte_len(&self, category: &str) -> Result<usize, String> {
        self.syntax_instance_counter.checked_add(1)
            .ok_or_else(|| "compiler syntax instance counter overflowed".to_owned())?;
        let digits = self.syntax_instance_counter.checked_ilog10().unwrap_or(0) as usize + 1;
        Ok("\0compiler-syntax-instance\0".len() + category.len() + 1 + digits)
    }

    syntax_arena!(items, insert_item, item_syntax, Item);
    syntax_arena!(item_modules, insert_item_module, item_modules, Module);
    syntax_arena!(expressions, insert_expression, expr_syntax, Expr);
    syntax_arena!(types, insert_type, type_syntax, Type);
    syntax_arena!(patterns, insert_pattern, pattern_syntax, Pattern);
    syntax_arena!(match_arms, insert_match_arm, match_arm_syntax, MatchArm);
    syntax_arena!(params, insert_param, param_syntax, Param);
    syntax_arena!(statements, insert_statement, stmt_syntax, Stmt);
    syntax_arena!(blocks, insert_block, block_syntax, Block);
    syntax_arena!(expression_origins, insert_expression_origin, expr_origins, ComptimeSyntaxOrigin);
    syntax_arena!(type_origins, insert_type_origin, type_origins, ComptimeSyntaxOrigin);
    syntax_arena!(pattern_origins, insert_pattern_origin, pattern_origins, ComptimeSyntaxOrigin);
    syntax_arena!(statement_origins, insert_statement_origin, stmt_origins, ComptimeSyntaxOrigin);
    syntax_arena!(block_origins, insert_block_origin, block_origins, ComptimeSyntaxOrigin);

    pub fn emit_item(&mut self, item: PositionedComptimeItem) { self.item_output.push(item); }
    pub fn emit_expression(&mut self, expr: ComptimeExprEmission) { self.expr_output.push(expr); }
    pub fn into_emissions(self) -> (Vec<PositionedComptimeItem>, Vec<ComptimeExprEmission>) {
        (self.item_output, self.expr_output)
    }
}

fn encode_fresh_scope(scope: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(scope.len().saturating_mul(2));
    for byte in scope.bytes() {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

/// Deterministic accounting interface for compiler execution adapters.
/// Units must be defined by that adapter (execution, syntax nodes, output bytes,
/// etc.); they are not wall time. Separate resources need separate counters.
/// Merely constructing this counter enforces no bound on an evaluator. The
/// frozen interpreter retains its existing step accounting. The explicit
/// canonical fresh-name adapter charges names/output bytes through this type
/// and uses Wasmtime fuel. Its expression adapter additionally charges separate
/// structural arena and emitted-output node/byte counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeterministicBudget {
    remaining: u64,
}

impl DeterministicBudget {
    pub fn new(limit: u64) -> Self { Self { remaining: limit } }
    pub fn remaining(&self) -> u64 { self.remaining }
    /// Reject without changing state when the exact remaining budget would be
    /// exceeded. Checked subtraction also refuses arithmetic overflow cases.
    pub fn charge(&mut self, units: u64) -> Result<(), BudgetExceeded> {
        self.remaining = self.remaining.checked_sub(units).ok_or(BudgetExceeded)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetExceeded;
