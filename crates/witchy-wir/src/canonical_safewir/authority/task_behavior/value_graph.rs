//! Exact single-block value graph for constructor-only deferred storage.
//!
//! A callable and the environment stored next to it are separate fields of
//! one runtime object. Keeping their actual object identities here avoids
//! granting behavior from a matching function signature or a callback merely
//! present somewhere in the returned aggregate. Unsupported operations make
//! the graph unavailable; they never preserve an old authority fact.

use std::collections::BTreeMap;

use crate::canonical_safewir::{Function, FunctionId, LoadTransfer, Operation, Projection, ValueId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Node {
    Unknown,
    Input(usize),
    Object(ValueId),
    Function(FunctionId),
}

#[derive(Default)]
pub(super) struct ValueGraph {
    values: BTreeMap<ValueId, Node>,
    fields: BTreeMap<(ValueId, u32), Node>,
}

impl ValueGraph {
    pub(super) fn constructor(function: &Function) -> Option<Self> {
        let [block] = function.blocks.as_slice() else { return None };
        let mut graph = Self::default();
        for (index, parameter) in block.parameters.iter().enumerate() {
            graph.values.insert(parameter.value, Node::Input(index));
        }
        for operation in &block.operations {
            match operation {
                Operation::Alloc { destination, .. } => {
                    graph.values.insert(*destination, Node::Object(*destination));
                }
                Operation::FunctionValue { destination, function } => {
                    graph.values.insert(*destination, Node::Function(*function));
                }
                Operation::MoveValue { destination, source }
                | Operation::CloneRef { destination, source } => {
                    graph.values.insert(*destination, graph.node(*source));
                }
                Operation::Load { destination, place, transfer } => {
                    let Projection::Field { index } = place.projection() else { return None };
                    let Node::Object(root) = graph.node(place.object()) else { return None };
                    let child = graph.fields.get(&(root, *index)).copied().unwrap_or(Node::Unknown);
                    graph.values.insert(*destination, child);
                    if matches!(transfer, LoadTransfer::MoveUnique | LoadTransfer::TakeNullableUnique | LoadTransfer::TakeOwnedField | LoadTransfer::TakeLast) {
                        graph.fields.remove(&(root, *index));
                    }
                }
                Operation::Store { place, value, .. } => {
                    let Projection::Field { index } = place.projection() else { return None };
                    let Node::Object(root) = graph.node(place.object()) else { return None };
                    graph.fields.insert((root, *index), graph.node(*value));
                }
                Operation::InitNullableOwned { place } => {
                    let Projection::Field { index } = place.projection() else { return None };
                    let Node::Object(root) = graph.node(place.object()) else { return None };
                    graph.fields.remove(&(root, *index));
                }
                Operation::ConstUnit { destination }
                | Operation::ConstInt { destination, .. }
                | Operation::ConstFloat { destination, .. }
                | Operation::ConstBool { destination, .. }
                | Operation::ConstSize { destination, .. } => {
                    graph.values.insert(*destination, Node::Unknown);
                }
                Operation::DropRef { .. } => {}
                _ => return None,
            }
        }
        Some(graph)
    }

    pub(super) fn node(&self, value: ValueId) -> Node {
        self.values.get(&value).copied().unwrap_or(Node::Unknown)
    }

    pub(super) fn fields(&self, object: Node) -> impl Iterator<Item = (u32, Node)> + '_ {
        let Node::Object(root) = object else { return Vec::new().into_iter() };
        self.fields.iter().filter_map(move |((actual, index), value)|
            (*actual == root).then_some((*index, *value))).collect::<Vec<_>>().into_iter()
    }
}
