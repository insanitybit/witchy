//! Final typed-frame input for RFC-0161 execution lowering.
//!
//! The legacy suspension carrier catalog remains useful historical layout
//! evidence, but it identifies states by generated function spelling and feeds
//! scalar transitions. This module deliberately does neither. It retains the
//! linked source callable identity, final typed callable table index, and exact
//! parameter types as the narrow input from which a canonical `AsyncProgram`
//! can assign frame lanes and backend bindings.

use witchy_syntax::ast::{Convention, Expr, Function, Item, Type};
use witchy_syntax::async_surface::AsyncSurfaceId;
use witchy_syntax::suspension::{
    async_surface, async_surface_module, frame_state, source_suspension, FRAME_ENTRY_ATTRIBUTE,
    FRAME_FUNCTION_ATTRIBUTE,
};

use crate::async_control_flow::{
    AsyncCallableId, ControlPathStep, SourceAsyncControlFlow, SourceAsyncControlFlowInventory,
    SourceAsyncOperationKind, SourceBindingId, SourceBindingKind, SourceExpressionId, source_expression,
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

/// Semantic class of a source async operation after ordinary linking and type
/// checking. This is deliberately a language-operation catalog, not a
/// benchmark recognizer: every call site of one of these public operations is
/// classified uniformly, while ordinary static and dynamic async calls retain
/// their own explicit classes for the shared runtime or a typed rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypedAsyncOperationKind {
    ChannelCreate,
    ChannelSend,
    ChannelReceive,
    Spawn,
    Join,
    Yield,
    CallOperand,
    DynamicCallOperand,
    StaticAsyncCall,
    DynamicAsyncCall,
    Return,
}

/// One typed value carried into a final state. Lane IDs and ownership actions
/// are assigned only when the complete closure layout is known.
#[derive(Debug, Clone, PartialEq)]
pub struct TypedAsyncFrameInput {
    pub parameter: u32,
    /// The final callable's declared parameter name. This is layout metadata
    /// from the compiler-produced state, retained so a later lane builder can
    /// map checked source places to actual capture/parameter fields without
    /// inspecting a generated callable name or reconstructing a CPS body.
    pub name: String,
    /// The source declaration carried by this final frame field when the
    /// lowering contract proves one. Entry parameters are structurally aligned
    /// with the source callable; continuation captures remain `None` until
    /// async lowering stamps their lexical binding IDs explicitly.
    pub source_binding: Option<SourceBindingId>,
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

/// A lossless link from one source `await` operation to the compiler-generated
/// continuation state entered after that operation completes. The source
/// operation ordinal is diagnostic/source identity; `final_state` is the typed
/// backend binding. Neither is a generated function name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedAsyncSuspensionBinding {
    pub callable: AsyncCallableId,
    pub source_operation: u32,
    pub final_state: FinalCallableIndex,
}

/// One source operation retained in the final typed-lowering input.  This is
/// deliberately complete, not merely a list of suspension sites: a producer
/// must account for calls and terminal source edges as well as awaits.  Only
/// an await has a continuation state at this stage; the later executable
/// lowering supplies its resolved operands, queue/host binding, and state
/// body without rediscovering source order from CPS expressions.
#[derive(Debug, Clone, PartialEq)]
pub struct TypedAsyncSourceOperationBinding {
    pub callable: AsyncCallableId,
    pub source_operation: u32,
    pub kind: SourceAsyncOperationKind,
    pub runtime_kind: TypedAsyncOperationKind,
    pub path: Vec<ControlPathStep>,
    /// A source-coordinate handle for the exact pre-CPS operation expression.
    /// It is intentionally noncanonical transport; final typed lowering uses
    /// it to obtain operands from the retained source item, never from a
    /// generated closure name or scalar transition recognizer.
    pub expression: Option<SourceExpressionId>,
    /// Ordered lexical bindings read by this operation expression. The IDs are
    /// source facts, not variable spellings, so shadowed locals remain distinct
    /// when final closure layout later assigns frame lanes.
    pub operand_bindings: Vec<SourceBindingId>,
    /// The finalized operand type captured from the linked pre-CPS typed
    /// projection. A bare `return` has no operand; every other operation must
    /// carry a concrete type before executable frame/lane formation begins.
    pub operand_type: Option<Type>,
    pub continuation: Option<FinalCallableIndex>,
    /// A tail await completes its execution directly and therefore has no
    /// generated continuation state. Keeping this explicit is required for
    /// canonical terminal edges; it must never be reconstructed from CPS.
    pub terminal: bool,
}

/// One source control selector retained with its concrete checked type.  It is
/// intentionally separate from executable edges: a later CFG builder chooses
/// states and lane actions after final closure layout, but it need not inspect
/// generated CPS expressions to obtain the branch discriminator.
#[derive(Debug, Clone, PartialEq)]
pub struct TypedAsyncSourceControlBinding {
    pub callable: AsyncCallableId,
    pub control: u32,
    pub selector: SourceExpressionId,
    pub selector_type: Option<Type>,
}

/// The complete bridge between source control flow and final typed state
/// layouts. It is not executable and has no legacy scalar transitions.
#[derive(Debug, Clone, PartialEq)]
pub struct TypedAsyncLoweringInput {
    pub source: Vec<SourceAsyncControlFlow>,
    pub states: Vec<TypedAsyncStateInput>,
    pub suspensions: Vec<TypedAsyncSuspensionBinding>,
    pub controls: Vec<TypedAsyncSourceControlBinding>,
    pub operations: Vec<TypedAsyncSourceOperationBinding>,
}

impl TypedAsyncLoweringInput {
    /// Re-run the ordinary checked lowering projection, then bind each final
    /// state to the lossless source identity retained by [`CheckedModule`].
    pub fn from_checked(checked: &CheckedModule) -> Result<Self, String> {
        let typed = typeck::annotate_checked_source(checked.module().clone())
            .map_err(|error| error.message)?;
        let mut input = Self::from_typed(&typed, checked.source_async_control_flow())?;
        for operation in &mut input.operations {
            let Some(_) = operation.expression else { continue };
            let (expression, ty) = Self::typed_source_operation(checked, operation)?;
            operation.operand_type = Some(typeck::ty_to_ast(ty).ok_or_else(|| format!(
                "source async {:?} operation {} has a non-concrete operand type `{ty}`",
                operation.callable,
                operation.source_operation,
            ))?);
            operation.runtime_kind = classify_typed_operation(operation.kind, expression);
        }
        for control in &mut input.controls {
            let (_, ty) = Self::typed_source_expression(checked, &control.callable, control.selector)?;
            control.selector_type = Some(typeck::ty_to_ast(ty).ok_or_else(|| format!(
                "source async {:?} control {} has a non-concrete selector type `{ty}`",
                control.callable,
                control.control,
            ))?);
        }
        Ok(input)
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
                    name: parameter_info.name.clone(),
                    source_binding: None,
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
        for state in &mut states {
            if state.kind != TypedAsyncStateKind::Entry {
                continue;
            }
            let flow = source.iter().find(|flow| flow.callable == state.callable)
                .expect("source callable existence was checked while collecting states");
            let parameters = flow.bindings.iter().filter(|binding| {
                binding.kind == SourceBindingKind::Parameter
            }).collect::<Vec<_>>();
            if parameters.len() != state.frame.len() {
                return Err(format!(
                    "async entry {:?} has {} final parameters but {} source parameters",
                    state.callable,
                    state.frame.len(),
                    parameters.len(),
                ));
            }
            for (frame, source_parameter) in state.frame.iter_mut().zip(parameters) {
                frame.source_binding = Some(source_parameter.id);
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
        let mut suspensions = Vec::new();
        for flow in &source {
            let source_awaits = flow.operations.iter().filter(|operation| {
                operation.kind == SourceAsyncOperationKind::Await
            }).collect::<Vec<_>>();
            for state in states.iter().filter(|state| state.callable == flow.callable) {
                let Some(source_suspension) = state.source_suspension else { continue };
                let source_operation = source_awaits
                    .get(usize::try_from(source_suspension).map_err(|_| "source suspension exceeds usize")?)
                    .ok_or_else(|| format!("async state {:?} refers to missing source suspension {source_suspension}", state.final_callable))?;
                suspensions.push(TypedAsyncSuspensionBinding {
                    callable: flow.callable.clone(),
                    source_operation: source_operation.ordinal,
                    final_state: state.final_callable,
                });
            }
        }
        suspensions.sort_by(|left, right| {
            (&left.callable, left.source_operation, left.final_state)
                .cmp(&(&right.callable, right.source_operation, right.final_state))
        });
        for pair in suspensions.windows(2) {
            if pair[0].callable == pair[1].callable
                && pair[0].source_operation == pair[1].source_operation
            {
                return Err(format!(
                    "source async operation {} has multiple continuation-state bindings",
                    pair[0].source_operation,
                ));
            }
        }
        // Every await either enters its compiler-formed continuation state or
        // is a terminal edge. The latter must be represented explicitly below;
        // a later program producer must not rediscover it from CPS shape.
        let mut operations = Vec::new();
        for flow in &source {
            for operation in &flow.operations {
                let continuation = suspensions.iter().find(|binding| {
                    binding.callable == flow.callable
                        && binding.source_operation == operation.ordinal
                }).map(|binding| binding.final_state);
                if operation.kind != SourceAsyncOperationKind::Await && continuation.is_some() {
                    return Err(format!(
                        "source async non-await operation {} has a continuation-state binding",
                        operation.ordinal,
                    ));
                }
                let mut operand_bindings = Vec::new();
                if let Some(expression) = operation.expression
                    && let Some(span) = flow.expression_spans.iter().find(|span| span.root == expression)
                {
                    for use_site in flow.binding_uses.iter().filter(|use_site| {
                        use_site.expression.0 >= span.root.0 && use_site.expression.0 < span.end.0
                    }) {
                        if !operand_bindings.contains(&use_site.binding) {
                            operand_bindings.push(use_site.binding);
                        }
                    }
                }
                operations.push(TypedAsyncSourceOperationBinding {
                    callable: flow.callable.clone(),
                    source_operation: operation.ordinal,
                    kind: operation.kind,
                    runtime_kind: source_operation_kind(operation.kind),
                    path: operation.path.clone(),
                    expression: operation.expression,
                    operand_bindings,
                    operand_type: None,
                    continuation,
                    terminal: operation.kind == SourceAsyncOperationKind::Await && continuation.is_none()
                        || operation.kind == SourceAsyncOperationKind::Return,
                });
            }
        }
        operations.sort_by(|left, right| {
            (&left.callable, left.source_operation)
                .cmp(&(&right.callable, right.source_operation))
        });
        for pair in operations.windows(2) {
            if pair[0].callable == pair[1].callable
                && pair[0].source_operation == pair[1].source_operation
            {
                return Err(format!(
                    "source async operation {} has multiple typed operation bindings",
                    pair[0].source_operation,
                ));
            }
        }
        let mut controls = source.iter().flat_map(|flow| flow.controls.iter().map(|control| {
            TypedAsyncSourceControlBinding {
                callable: flow.callable.clone(),
                control: control.id.0,
                selector: control.selector,
                selector_type: None,
            }
        })).collect::<Vec<_>>();
        controls.sort_by(|left, right| {
            (&left.callable, left.control).cmp(&(&right.callable, right.control))
        });
        for pair in controls.windows(2) {
            if pair[0].callable == pair[1].callable && pair[0].control == pair[1].control {
                return Err(format!("source async control {} has multiple typed bindings", pair[0].control));
            }
        }
        Ok(Self { source, states, suspensions, controls, operations })
    }

    /// Resolve one typed source-operation binding back to the exact retained
    /// pre-CPS expression which supplies its operands. This is deliberately a
    /// source/typed-lowering bridge only; callers still need to form typed
    /// lanes, ownership actions, and executable state edges from checked data.
    pub fn source_operation_expression<'a>(
        checked: &'a CheckedModule,
        operation: &TypedAsyncSourceOperationBinding,
    ) -> Result<&'a Expr, String> {
        let expression = operation.expression.ok_or_else(|| format!(
            "source async {:?} operation {} has no expression operand",
            operation.callable,
            operation.source_operation,
        ))?;
        let function = checked
            .async_surface()
            .function(&operation.callable.module, &operation.callable.surface)
            .ok_or_else(|| format!(
                "source async operation {} has no retained callable {:?}",
                operation.source_operation,
                operation.callable,
            ))?;
        source_expression(function, expression).ok_or_else(|| format!(
            "source async {:?} operation {} refers to absent source expression {:?}",
            operation.callable,
            operation.source_operation,
            expression,
        ))
    }

    /// Resolve an operation through the fully linked typed-source projection.
    /// The returned expression and inferred type are guaranteed to share the
    /// same allocation, unlike the lossless syntax sidecar used for diagnostics.
    pub fn typed_source_operation<'a>(
        checked: &'a CheckedModule,
        operation: &TypedAsyncSourceOperationBinding,
    ) -> Result<(&'a Expr, &'a typeck::Ty), String> {
        let expression = operation.expression.ok_or_else(|| format!(
            "source async {:?} operation {} has no expression operand",
            operation.callable,
            operation.source_operation,
        ))?;
        Self::typed_source_expression(checked, &operation.callable, expression)
    }

    fn typed_source_expression<'a>(
        checked: &'a CheckedModule,
        callable: &AsyncCallableId,
        expression: SourceExpressionId,
    ) -> Result<(&'a Expr, &'a typeck::Ty), String> {
        let typed = checked.typed_async_source().ok_or_else(|| format!(
            "source async {:?} has no typed pre-CPS projection", callable,
        ))?;
        let function = typed_source_function(typed, callable).ok_or_else(|| format!(
            "source async has no typed callable {:?}", callable,
        ))?;
        let expression = source_expression(function, expression).ok_or_else(|| format!(
            "source async {:?} refers to absent typed expression {:?}", callable, expression,
        ))?;
        let ty = typed.table().type_of(expression).ok_or_else(|| format!(
            "source async {:?} expression {:?} has no inferred type", callable, expression,
        ))?;
        Ok((expression, ty))
    }
}

fn typed_source_function<'a>(
    typed: &'a TypedModule,
    callable: &AsyncCallableId,
) -> Option<&'a Function> {
    let matches_callable = |function: &Function| {
        async_surface(function)
            .is_some_and(|(item, method)| {
                u32::try_from(item).ok() == Some(callable.surface.item)
                    && method.and_then(|method| u32::try_from(method).ok()) == callable.surface.method
            })
            && async_surface_module(function) == Some(callable.module.as_str())
    };
    typed.module().items.iter().find_map(|item| match item {
        Item::Function(function) if matches_callable(function) => Some(function),
        Item::Impl(definition) => definition.methods.iter().find(|method| matches_callable(method)),
        Item::Function(_) | Item::Type(_) | Item::Trait(_) | Item::Const { .. }
        | Item::TypeAlias { .. } | Item::Comptime(_) => None,
    })
}

fn source_operation_kind(kind: SourceAsyncOperationKind) -> TypedAsyncOperationKind {
    match kind {
        SourceAsyncOperationKind::Await => TypedAsyncOperationKind::StaticAsyncCall,
        SourceAsyncOperationKind::StaticCall => TypedAsyncOperationKind::CallOperand,
        SourceAsyncOperationKind::DynamicCall => TypedAsyncOperationKind::DynamicCallOperand,
        SourceAsyncOperationKind::Return => TypedAsyncOperationKind::Return,
    }
}

fn classify_typed_operation(
    source_kind: SourceAsyncOperationKind,
    expression: &Expr,
) -> TypedAsyncOperationKind {
    if source_kind != SourceAsyncOperationKind::Await {
        return source_operation_kind(source_kind);
    }
    let Expr::Unary { op: witchy_syntax::ast::UnOp::Await, expr } = expression else {
        return TypedAsyncOperationKind::StaticAsyncCall;
    };
    match expr.as_ref() {
        Expr::Call { name, .. } | Expr::LabeledCall { name, .. } => match name.as_str() {
            "chan.channel" | "chan.unbounded" => TypedAsyncOperationKind::ChannelCreate,
            "chan.send" => TypedAsyncOperationKind::ChannelSend,
            "chan.recv" => TypedAsyncOperationKind::ChannelReceive,
            "chan.spawn" | "task.spawn" => TypedAsyncOperationKind::Spawn,
            "chan.join" | "task.join" => TypedAsyncOperationKind::Join,
            "task.yield_now" | "chan.yield_now" => TypedAsyncOperationKind::Yield,
            _ => TypedAsyncOperationKind::StaticAsyncCall,
        },
        Expr::Apply { .. } | Expr::ExistentialCall { .. } => TypedAsyncOperationKind::DynamicAsyncCall,
        _ => TypedAsyncOperationKind::StaticAsyncCall,
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
        assert!(input.states.iter().all(|state| state.frame.iter().all(|lane| !lane.name.is_empty())));
        assert!(input.states.iter().any(|state| {
            state.frame.iter().any(|lane| lane.name == "self")
        }));
        assert!(input.states.iter().filter(|state| state.kind == TypedAsyncStateKind::Entry).all(|state| {
            state.frame.iter().all(|lane| lane.source_binding.is_some())
        }));
        assert_eq!(input.suspensions.len(), 1);
        assert_eq!(input.operations.len(), input.source[0].operations.len());
        assert_eq!(
            input.operations.iter().filter(|operation| operation.kind == SourceAsyncOperationKind::Await).count(),
            1,
        );
        assert!(input.operations.iter().all(|operation| {
            operation.kind != SourceAsyncOperationKind::Await
                || operation.continuation.is_some() != operation.terminal
        }));
        let await_operation = input.operations.iter().find(|operation| {
            operation.kind == SourceAsyncOperationKind::Await
        }).expect("typed await operation");
        assert_eq!(
            await_operation.operand_type,
            Some(witchy_syntax::ast::Type::Named("Int".into(), Vec::new())),
        );
        assert!(matches!(
            TypedAsyncLoweringInput::source_operation_expression(&checked, await_operation),
            Ok(witchy_syntax::ast::Expr::Unary {
                op: witchy_syntax::ast::UnOp::Await,
                ..
            })
        ));
        assert!(matches!(
            TypedAsyncLoweringInput::typed_source_operation(&checked, await_operation),
            Ok((
                witchy_syntax::ast::Expr::Unary { op: witchy_syntax::ast::UnOp::Await, .. },
                crate::typeck::Ty::Int,
            ))
        ));
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
        assert_eq!(input.suspensions.len(), 2);
        assert!(input.suspensions[0].source_operation < input.suspensions[1].source_operation);
        assert!(input.suspensions[0].final_state > input.suspensions[1].final_state);
        let source_awaits = input.source[0].operations.iter()
            .filter(|operation| operation.kind == SourceAsyncOperationKind::Await)
            .map(|operation| operation.ordinal)
            .collect::<Vec<_>>();
        assert_eq!(
            input.suspensions.iter().map(|binding| binding.source_operation).collect::<Vec<_>>(),
            source_awaits,
        );
        let source_operations = input.source[0].operations.iter()
            .map(|operation| operation.ordinal)
            .collect::<Vec<_>>();
        assert_eq!(
            input.operations.iter().map(|operation| operation.source_operation).collect::<Vec<_>>(),
            source_operations,
        );
        assert_eq!(
            input.operations.iter().map(|operation| operation.expression).collect::<Vec<_>>(),
            input.source[0].operations.iter().map(|operation| operation.expression).collect::<Vec<_>>(),
        );
        assert!(input.operations.iter().all(|operation| {
            operation.kind != SourceAsyncOperationKind::Await
                || operation.continuation.is_some() != operation.terminal
        }));
    }

    #[test]
    fn classifies_every_channel_send_await_as_one_language_operation() {
        let module = witchy_syntax::parser::parse_module(
            "from chan import Sender\n\nasync fn send_one(tx: Sender(Int)):\n    chan.send(tx, 7).await\n\nfn main():\n    ()\n",
        ).expect("parse source");
        let checked = link_checked(
            vec![("main".to_string(), module)],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).expect("checked source");
        let input = TypedAsyncLoweringInput::from_checked(&checked).expect("typed lowering input");
        assert!(input.operations.iter().any(|operation| {
            operation.kind == SourceAsyncOperationKind::Await
                && operation.runtime_kind == TypedAsyncOperationKind::ChannelSend
                && operation.terminal
                && operation.operand_bindings == vec![SourceBindingId(0)]
        }));
        assert!(input.operations.iter().any(|operation| {
            operation.kind == SourceAsyncOperationKind::StaticCall
                && operation.runtime_kind == TypedAsyncOperationKind::CallOperand
        }));
    }

    #[test]
    fn retains_concrete_control_selector_types_before_cps_lowering() {
        let module = witchy_syntax::parser::parse_module(
            "async fn choose(flag: Bool) -> Int:\n    if flag:\n        task.done(1).await\n    else:\n        task.done(2).await\n\nfn main():\n    ()\n",
        ).expect("parse source");
        let checked = link_checked(
            vec![("main".to_string(), module)],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).expect("checked source");
        let input = TypedAsyncLoweringInput::from_checked(&checked).expect("typed lowering input");
        assert_eq!(input.controls.len(), 1);
        assert_eq!(
            input.controls[0].selector_type,
            Some(witchy_syntax::ast::Type::Named("Bool".into(), Vec::new())),
        );
    }

    #[test]
    fn operation_operands_keep_the_inner_shadowed_binding() {
        let module = witchy_syntax::parser::parse_module(
            "from chan import Sender\n\nasync fn send(tx: Sender(Int)):\n    let value = 1\n    if true:\n        let value = 2\n        chan.send(tx, value).await\n\nfn main():\n    ()\n",
        ).expect("parse source");
        let checked = link_checked(
            vec![("main".to_string(), module)],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).expect("checked source");
        let input = TypedAsyncLoweringInput::from_checked(&checked).expect("typed lowering input");
        let send = input.operations.iter().find(|operation| {
            operation.runtime_kind == TypedAsyncOperationKind::ChannelSend
        }).expect("channel send operation");
        assert_eq!(send.operand_bindings, vec![SourceBindingId(0), SourceBindingId(2)]);
    }
}
