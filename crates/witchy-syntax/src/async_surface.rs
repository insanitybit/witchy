//! Lossless source async surface retained across destructive CPS lowering.
//!
//! This is intentionally *not* RFC-0161's canonical `AsyncProgram`: it has no
//! typed frame layout, control-flow graph, backend binding, or fingerprint.
//! Its only contract is to retain the exact async-containing source item long
//! enough for a later typed control-flow pass to build those facts without
//! reverse-engineering generated `task.lazy` / `task.and_then` closures.

use crate::ast::{Item, Module};

/// Stable coordinate of an async source declaration after generator lowering.
/// It is diagnostic/source identity only; final execution and operation IDs
/// are assigned later from typed physical layout.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AsyncSurfaceId {
    pub item: u32,
    pub method: Option<u32>,
}

/// One lossless source item containing at least one `async fn`.
///
/// Inherent methods are retained as their complete enclosing `impl` item so a
/// future typed builder has the receiver and target-type context required to
/// form a callable identity without generated names.
#[derive(Debug, Clone, PartialEq)]
pub struct AsyncSurfaceItem {
    pub id: AsyncSurfaceId,
    pub line: u32,
    pub item: Item,
}

/// Noncanonical source preservation for every async item in a linked program.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AsyncSurfaceModule {
    pub module: String,
    pub items: Vec<AsyncSurfaceItem>,
}

impl AsyncSurfaceModule {
    /// Capture all async-containing items before destructive async lowering.
    pub fn from_module(module_name: &str, module: &Module) -> Self {
        let items = module
            .items
            .iter()
            .enumerate()
            .flat_map(|(source_item, source)| {
                let item = u32::try_from(source_item)
                    .expect("source item count exceeds async-surface identity range");
                let line = module.item_lines.get(source_item).copied().unwrap_or(0);
                let entry = |method| AsyncSurfaceItem {
                    id: AsyncSurfaceId { item, method },
                    line,
                    item: source.clone(),
                };
                match source {
                    Item::Function(function) if function.is_async => vec![entry(None)],
                    Item::Impl(definition) => definition
                        .methods
                        .iter()
                        .enumerate()
                        .filter(|(_, method)| method.is_async)
                        .map(|(method, _)| {
                            entry(Some(
                                u32::try_from(method).expect(
                                    "method count exceeds async-surface identity range",
                                ),
                            ))
                        })
                        .collect(),
                    Item::Function(_)
                    | Item::Type(_)
                    | Item::Trait(_)
                    | Item::Const { .. }
                    | Item::TypeAlias { .. }
                    | Item::Comptime(_) => Vec::new(),
                }
            })
            .collect();
        Self { module: module_name.to_string(), items }
    }
}

/// Final linked location of one lowered source async callable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkedAsyncCallableLocation {
    Function { item: u32 },
    InherentMethod { item: u32, method: u32 },
}

/// Binding from a pre-CPS async callable/state to its linked representation.
/// This is the bridge to later typed/layout binding; it deliberately has no
/// generated callable name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedAsyncCallableBinding {
    pub module: String,
    pub surface: AsyncSurfaceId,
    pub local_state: u32,
    pub location: LinkedAsyncCallableLocation,
}

/// Noncanonical source preservation plus post-link callable bindings.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LinkedAsyncSurface {
    pub modules: Vec<AsyncSurfaceModule>,
    pub bindings: Vec<LinkedAsyncCallableBinding>,
}

impl LinkedAsyncSurface {
    pub fn push_module(&mut self, module: AsyncSurfaceModule) {
        self.modules.push(module);
    }

    pub fn push_binding(&mut self, binding: LinkedAsyncCallableBinding) {
        self.bindings.push(binding);
    }
}
