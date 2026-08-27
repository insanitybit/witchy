//! Final typed-frame input for RFC-0161 execution lowering.
//!
//! The legacy suspension carrier catalog remains useful historical layout
//! evidence, but it identifies states by generated function spelling and feeds
//! scalar transitions. This module deliberately does neither. It retains the
//! linked source callable identity, final typed callable table index, and exact
//! parameter types as the narrow input from which a canonical `AsyncProgram`
//! can assign frame lanes and backend bindings.

use witchy_syntax::ast::{Convention, Item, Type};
use witchy_syntax::async_surface::AsyncSurfaceId;
use witchy_syntax::suspension::{
    async_surface, async_surface_module, frame_state, source_suspension, FRAME_ENTRY_ATTRIBUTE,
    FRAME_FUNCTION_ATTRIBUTE,
};

use crate::async_control_flow::{
    AsyncCallableId, SourceAsyncControlFlow, SourceAsyncControlFlowInventory,
    SourceAsyncOperationKind,
};
use crate::pipeline::CheckedModule;
use crate::typeck::{self, TypedModule};

/// A final callable-table position. This is an internal backend binding input;
/// it is intentionally not a generated function name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FinalCallableIndex(pub u32);

/// The role of a compiler-formed async state before executable edges are built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TypedAsyncStateKind {
    Entry,
    Segment,
}

/// One typed value carried into a final state. Lane IDs and ownership actions
/// are assigned only when the complete closure layout is known.
#[derive(Debug, Clone, PartialEq)]
pub struct TypedAsyncFrameInput {
    pub parameter: u32,
    pub ty: Type,
    pub convention: Convention,
}

/// A source-addressable final typed async state.
#[derive(Debug, Clone, PartialEq)]
pub struct TypedAsyncStateInput {
    pub callable: AsyncCallableId,
    pub local_state: u32,
    pub kind: TypedAsyncStateKind,
    /// The lexical source suspension that resumes into this state. Entry states
    /// and compiler-formed joins have no source suspension identity.
    pub source_suspension: Option<u32>,
    pub final_callable: FinalCallableIndex,
    pub frame: Vec<TypedAsyncFrameInput>,
}

/// The complete bridge between source control flow and final typed state
/// layouts. It is not executable and has no legacy scalar transitions.
#[derive(Debug, Clone, PartialEq)]
pub struct TypedAsyncLoweringInput {
    pub source: Vec<SourceAsyncControlFlow>,
    pub states: Vec<TypedAsyncStateInput>,
}

impl TypedAsyncLoweringInput {
    /// Re-run the ordinary checked lowering projection, then bind each final
    /// state to the lossless source identity retained by [`CheckedModule`].
    pub fn from_checked(checked: &CheckedModule) -> Result<Self, String> {
        let typed = typeck::annotate_checked_source(checked.module().clone())
            .map_err(|error| error.message)?;
        Self::from_typed(&typed, checked.source_async_control_flow())
    }

    pub fn from_typed(
        typed: &TypedModule,
        source_inventory: SourceAsyncControlFlowInventory,
    ) -> Result<Self, String> {
        let mut source = source_inventory.callables;
        source.sort_by(|left, right| left.callable.cmp(&right.callable));
        let source_ids = source.iter().map(|flow| flow.callable.clone()).collect::<std::collections::BTreeSet<_>>();
        let source_suspension_counts = source.iter().map(|flow| {
            let count = flow.operations.iter().filter(|operation| {
                operation.kind == SourceAsyncOperationKind::Await
            }).count();
            Ok((
                flow.callable.clone(),
                u32::try_from(count).map_err(|_| "source suspension count exceeds u32")?,
            ))
        }).collect::<Result<std::collections::BTreeMap<_, _>, String>>()?;
        let mut source_suspension_bindings = std::collections::BTreeSet::new();
        let mut states = Vec::new();

        for (function_index, item) in typed.module().items.iter().enumerate() {
            let Item::Function(function) = item else { continue };
            let Some(local_state) = frame_state(function) else { continue };
            let kind = if function.attributes.iter().any(|attribute| attribute == FRAME_ENTRY_ATTRIBUTE) {
                TypedAsyncStateKind::Entry
            } else if function.attributes.iter().any(|attribute| attribute == FRAME_FUNCTION_ATTRIBUTE) {
                TypedAsyncStateKind::Segment
            } else {
                return Err(format!("async state at final callable index {function_index} has no state-kind marker"));
            };
            let Some((item, method)) = async_surface(function) else {
                return Err(format!("async state at final callable index {function_index} has no source identity"));
            };
            let Some(module) = async_surface_module(function) else {
                return Err(format!("async state at final callable index {function_index} has no source module identity"));
            };
            let callable = AsyncCallableId {
                module: module.to_string(),
                surface: AsyncSurfaceId {
                    item: u32::try_from(item).map_err(|_| "async source item exceeds u32")?,
                    method: method.map(|method| u32::try_from(method).map_err(|_| "async source method exceeds u32")).transpose()?,
                },
            };
            if !source_ids.contains(&callable) {
                return Err(format!("final async state at callable index {function_index} has no retained source control-flow record"));
            }
            let source_suspension = source_suspension(function);
            if let Some(source_suspension) = source_suspension {
                if kind != TypedAsyncStateKind::Segment {
                    return Err(format!("entry async state at callable index {function_index} has a source suspension identity"));
                }
                let source_suspension_count = source_suspension_counts[&callable];
                if source_suspension >= source_suspension_count {
                    return Err(format!("async state at callable index {function_index} refers to source suspension {source_suspension}, but `{}` has only {source_suspension_count}", function.name));
                }
                if !source_suspension_bindings.insert((callable.clone(), source_suspension)) {
                    return Err(format!("duplicate final async state binding for source suspension {source_suspension}"));
                }
            }
            // Segment captures are synthesized after source annotations and may
            // intentionally omit an AST annotation. Recover their exact type
            // from the checked declaration signature; the callable *identity*
            // above remains the final table index, not this lookup spelling.
            let inferred = match typed.table().function_type(&function.name) {
                Some(Type::Fn(parameters, _, _, _)) => parameters,
                Some(_) => return Err(format!("async state at final callable index {function_index} has a non-callable checked signature")),
                None => return Err(format!("async state at final callable index {function_index} has no checked signature")),
            };
            let frame = function.params.iter().enumerate().map(|(parameter, parameter_info)| {
                Ok(TypedAsyncFrameInput {
                    parameter: u32::try_from(parameter).map_err(|_| "async parameter count exceeds u32")?,
                    ty: inferred.get(parameter).cloned().or_else(|| parameter_info.ty.clone()).ok_or_else(|| format!("async state at final callable index {function_index} has untyped parameter `{}`", parameter_info.name))?,
                    convention: parameter_info.convention,
                })
            }).collect::<Result<Vec<_>, String>>()?;
            states.push(TypedAsyncStateInput {
                callable,
                local_state: u32::try_from(local_state).map_err(|_| "async local state exceeds u32")?,
                kind,
                source_suspension,
                final_callable: FinalCallableIndex(u32::try_from(function_index).map_err(|_| "final callable count exceeds u32")?),
                frame,
            });
        }
        states.sort_by(|left, right| {
            (&left.callable, left.local_state, left.final_callable)
                .cmp(&(&right.callable, right.local_state, right.final_callable))
        });
        for pair in states.windows(2) {
            if pair[0].callable == pair[1].callable && pair[0].local_state == pair[1].local_state {
                return Err(format!("duplicate final async state binding for {:?} state {}", pair[0].callable, pair[0].local_state));
            }
        }
        // Every source async callable starts exactly one execution. Continuation
        // states are ordinary entries in its resume table; accepting none or
        // more than one entry would force a backend to guess a start state.
        for source_callable in &source_ids {
            let entries = states.iter().filter(|state| {
                &state.callable == source_callable && state.kind == TypedAsyncStateKind::Entry
            }).count();
            if entries != 1 {
                return Err(format!(
                    "async callable {:?} must have exactly one final entry state, found {entries}",
                    source_callable,
                ));
            }
        }
        Ok(Self { source, states })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::link_checked;

    #[test]
    fn binds_inherent_async_states_to_final_indices_without_generated_names() {
        let module = witchy_syntax::parser::parse_module(
            "type Counter:\n    value: Int\n\nimpl Counter:\n    async fn next(self) -> Int:\n        let value = task.done(self.value).await\n        value + 1\n\nfn main(console: Console):\n    console.print(\"ready\")\n",
        ).expect("parse source");
        let checked = link_checked(
            vec![("main".to_string(), module)],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).expect("checked source");
        let input = TypedAsyncLoweringInput::from_checked(&checked).expect("typed lowering input");
        assert_eq!(input.source.len(), 1);
        assert_eq!(input.states.len(), 2);
        assert!(input.states.iter().all(|state| state.callable.module == "main"));
        assert!(input.states.iter().all(|state| state.callable.surface.method == Some(0)));
        assert!(input.states.iter().all(|state| !state.frame.is_empty()));
        assert_eq!(
            input.states.iter().filter_map(|state| state.source_suspension).collect::<Vec<_>>(),
            vec![0],
        );
    }

    #[test]
    fn continuation_source_ids_stay_in_lexical_order_when_segments_emit_in_reverse() {
        let module = witchy_syntax::parser::parse_module(
            "async fn pipeline() -> Int:\n    let first = task.done(1).await\n    let second = task.done(first + 1).await\n    second + 1\n\nfn main(console: Console):\n    console.print(\"ready\")\n",
        ).expect("parse source");
        let checked = link_checked(
            vec![("main".to_string(), module)],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).expect("checked source");
        let input = TypedAsyncLoweringInput::from_checked(&checked).expect("typed lowering input");
        let source_suspensions = input.states.iter()
            .filter_map(|state| state.source_suspension)
            .collect::<Vec<_>>();
        // States are sorted by physical state ID; the CPS transform forms the
        // second continuation first. The retained IDs nevertheless identify
        // their lexical source order without consulting generated names.
        assert_eq!(source_suspensions, vec![1, 0]);
    }
}
