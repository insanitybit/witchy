//! Direct checked-AST to canonical SafeWIR lowering.
//!
//! This module never constructs legacy WIR. Its initial scalar subset is
//! deliberately fail-closed; every unsupported source operation is reported
//! before a `LoweredCandidate` can be returned.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::error::Error;
use std::fmt;

#[path = "canonical_owned.rs"]
mod owned;

#[cfg(test)]
#[path = "canonical_owned_tests.rs"]
mod owned_tests;

use witchy_syntax::ast::{BinOp, Convention, Expr, Item, Pattern, Stmt, Type, TypeDef, TypeQual, UnOp};
use witchy_types::canonical_aggregates::{dictionary_as_tuple, scalar_aggregate_shape, ScalarAggregateShape, ScalarVariant};
use witchy_types::pipeline::CanonicalCheckedSource;
use witchy_types::canonical_closure::lambda_sites;
use witchy_types::typeck::{
    ProperTailCallableId, ProperTailDispatchKind, ProperTailExprKind, ProperTailPathStep,
    ProperTailSite, ProperTailSiteKey, ProperTailSiteManifest, TypeTable,
};
use witchy_wir::canonical_safewir::{
    AccessMode, ArithmeticOperator, BasicBlock, BlockId, BlockParameter,
    CallArgument, CallableContract, CanonicalAbiCatalog, CanonicalModule,
    CheckedSourceIdentity, ComparisonOperator, Function, FunctionId, FunctionSignature,
    FunctionSignatureId, HostBufferExtent, HostCallArgument, LoweredCandidate,
    LoweredModuleParts, CandidateAbiBinding, CandidateAbiBindings, CandidateAdapterBinding, CandidateAdapterBindings, CandidateClosureBinding, CandidateClosureBindings, MemoryEffect, Operation, OverflowBehavior, ParameterContract,
    ParameterOwnership, Projection, ResultContract, Revision,
    Terminator, Ownership, ValueId, ValueType,
};
use witchy_wir::layout::{
    HeaderLayout, LayoutBundle, LayoutInterner, LayoutKind, LayoutSize, PointerSlot, ScalarKind,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalLoweringError {
    OwnedReleasePlacement(String),
    InvalidLayoutBundle(String),
    MissingLayout(Type),
    AmbiguousLayout(Type),
    UnsupportedListElement(Type),
    MissingCheckedExpressionType(String),
    MissingCheckedFunctionType(String),
    InvalidCheckedFunctionType(String),
    UnsupportedType(Type),
    UnsupportedConvention { function: String, parameter: String, convention: Convention },
    UnsupportedItem(&'static str),
    UnsupportedStatement(&'static str),
    UnsupportedExpression(&'static str),
    /// Checked source recorded obligations canonical v1 cannot discharge
    /// (library specializations, closure-witness or callable-ABI limits); the
    /// payload lists them, semicolon separated.
    UnsupportedCanonicalObligation(String),
    MissingProperTailSite(String),
    MissingBinding(String),
    DuplicateBinding(String),
    MissingReturn(String),
    ReturnTypeMismatch { function: String, expected: ValueType, actual: ValueType },
    CallArityMismatch { callee: String, expected: usize, actual: usize },
    CallArgumentTypeMismatch {
        callee: String,
        parameter: usize,
        expected: ValueType,
        actual: ValueType,
    },
    IndirectCalleeType(ValueType),
    UnsupportedIndirectResult(ValueType),
    UnsupportedCallOwnership { callee: String, parameter: usize, ownership: ParameterOwnership },
    MissingCheckedCallConventions(FunctionSignatureId),
    IdOverflow,
    TailManifest(String),
    InvalidAuthenticatedAbiCatalog(String),
}

impl fmt::Display for CanonicalLoweringError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "canonical SafeWIR lowering failed: {self:?}")
    }
}

impl Error for CanonicalLoweringError {}

/// Lower the exact final typed AST into canonical SafeWIR.
///
/// The frontend product owns the exact typed module, source identity, and
/// proper-tail manifest used by this lowering. No caller can cross-pair them.
pub fn lower_typed_module(
    source: &CanonicalCheckedSource,
    layouts: LayoutBundle,
) -> Result<CanonicalModule<LoweredCandidate>, CanonicalLoweringError> {
    let typed = source.typed_module();
    // Checked source records what it could not bind as canonical authority
    // (RFC-0165): library specializations, closures the v1 witness cannot
    // authenticate, callable ABIs it cannot encode. Canonical v1 has no
    // lowering for any of them, so the program fails closed here, naming
    // them, rather than at checked-source construction or an unrelated call.
    let obligations = source.obligations();
    if !obligations.is_empty() {
        return Err(CanonicalLoweringError::UnsupportedCanonicalObligation(
            obligations.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "),
        ));
    }
    for item in &typed.module().items {
        let Item::Function(function) = item else { continue };
        if !is_canonical_source_function(typed.module(), &function.name) {
            continue;
        }
        if let Some(unsupported) = function
            .params
            .iter()
            .filter_map(|parameter| parameter.ty.as_ref().map(|ty| var_parameter_type(ty, parameter.convention)))
            .chain(function.ret.iter().cloned())
            .find(contains_deferred_v1_qualifier)
            .or_else(|| deferred_v1_qualifier_in_block(&function.body))
        {
            return Err(CanonicalLoweringError::UnsupportedType(unsupported));
        }
    }
    // The checker spells an entry-module type as `entry.Name`; the definition
    // keeps its bare source name. Key both spellings to the one definition.
    let entry_module = typed.module().linked_entry.as_deref();
    let packed_records = typed.module().items.iter().filter_map(|item| match item {
        Item::Type(definition) if is_v1_scalar_record(definition) => Some(definition),
        _ => None,
    }).flat_map(|definition| {
        let qualified = entry_module
            .filter(|_| !definition.name.contains('.'))
            .map(|entry| (format!("{entry}.{}", definition.name), definition));
        std::iter::once((definition.name.clone(), definition)).chain(qualified)
    }).collect::<HashMap<_, _>>();
    let definitions = witchy_types::canonical_aggregates::type_definitions(typed.module());
    let (_, layout_catalog) = LayoutBundle::decode_canonical(&layouts.canonical_bytes())
        .map_err(|error| CanonicalLoweringError::InvalidLayoutBundle(error.to_string()))?;
    let tail_manifest = source.proper_tail_manifest();
    let closure_layouts = crate::canonical_layouts::closure_layouts(source)
        .map_err(|error| CanonicalLoweringError::InvalidLayoutBundle(error.to_string()))?;
    let string_payloads = collect_string_payloads(typed.module(), typed.table(), &definitions);
    let string_data_ids = string_payloads
        .iter()
        .enumerate()
        .map(|(index, bytes)| {
            Ok((
                bytes.clone(),
                witchy_wir::canonical_safewir::StaticDataId::new(
                    u32::try_from(index).map_err(|_| CanonicalLoweringError::IdOverflow)?,
                ),
            ))
        })
        .collect::<Result<BTreeMap<_, _>, CanonicalLoweringError>>()?;
    // Callable identity is structural.  Intern every concrete checked `Fn`
    // type, including types which occur only in parameters or lambda
    // expressions, before assigning any declaration/function IDs.  A source
    // name is never part of a callable ABI identity.
    let mut callable_types = Vec::new();
    for item in &typed.module().items {
        let Item::Function(function) = item else { continue };
        if !is_canonical_source_function(typed.module(), &function.name) {
            continue;
        }
        if let Some(function_type) = source.callable_signature(&function.name) {
            collect_callable_types(function_type, &mut callable_types);
        }
    }
    for item in &typed.module().items {
        let Item::Function(function) = item else { continue };
        if !is_canonical_source_function(typed.module(), &function.name) {
            continue;
        }
        let _: Result<(), ()> = witchy_syntax::ast::visit::visit_block(
            &function.body,
            &mut |expression| {
                if let Some(checked_type) = typed.table().type_of(expression)
                    .and_then(witchy_types::typeck::ty_to_ast)
                {
                    collect_callable_types(&checked_type, &mut callable_types);
                }
                Ok(())
            },
        );
    }
    callable_types.sort_by_key(|ty| format!("{ty:?}"));
    callable_types.dedup();
    let function_types = callable_types
        .iter()
        .enumerate()
        .map(|(index, ty)| {
            Ok((
                ty.clone(),
                FunctionSignatureId::new(
                    u32::try_from(index).map_err(|_| CanonicalLoweringError::IdOverflow)?,
                ),
            ))
        })
        .collect::<Result<HashMap<_, _>, CanonicalLoweringError>>()?;
    let mut signatures = callable_types
        .iter()
        .map(|ty| {
            let id = function_types[ty];
            signature_from_type(id, ty, &layouts, &layout_catalog, &closure_layouts, &definitions).map_err(|error| {
                if std::env::var_os("WITCHY_CANONICAL_LOWER_DEBUG").is_some() {
                    eprintln!("canonical callable signature refused for `{ty:?}`: {error}");
                }
                error
            })
        })
        .collect::<Result<Vec<_>, CanonicalLoweringError>>()?;
    // Every `fn` type also has its closure signature: the declaration
    // signature with the closure record prepended as a shared parameter.
    // The code slot of a closure record holds a function of that shape.
    let mut closure_signatures: HashMap<Type, FunctionSignatureId> = HashMap::new();
    let mut closure_context = ClosureContext {
        layouts: closure_layouts.clone(),
        signature_by_layout: HashMap::new(),
        declaration_by_layout: HashMap::new(),
        lambda_captures: HashMap::new(),
        adapters: HashMap::new(),
    };
    for ty in &callable_types {
        let declaration_id = function_types[ty];
        let key = crate::canonical_layouts::closure_key(ty);
        // A `fn` type that is only ever a declaration's shape has no
        // closure layout and needs no closure signature.
        let Some(&layout) = closure_layouts.get(&key) else { continue };
        if let Some(&id) = closure_signatures.get(&key) {
            closure_signatures.insert(ty.clone(), id);
            continue;
        }
        let id = FunctionSignatureId::new(
            u32::try_from(signatures.len()).map_err(|_| CanonicalLoweringError::IdOverflow)?,
        );
        let declaration = signatures[declaration_id.get() as usize].clone();
        signatures.push(closure_signature(id, layout, &declaration));
        closure_signatures.insert(key, id);
        closure_signatures.insert(ty.clone(), id);
        closure_context.signature_by_layout.insert(layout, id);
        closure_context.declaration_by_layout.insert(layout, declaration_id);
    }
    let mut functions_by_name = HashMap::new();
    for item in &typed.module().items {
        let Item::Function(function) = item else {
            match item {
                Item::Type(_) => continue,
                Item::Trait(_) => return Err(CanonicalLoweringError::UnsupportedItem("trait")),
                Item::Impl(_) => return Err(CanonicalLoweringError::UnsupportedItem("impl")),
                Item::Const { .. } => return Err(CanonicalLoweringError::UnsupportedItem("const")),
                Item::TypeAlias { .. } => {
                    return Err(CanonicalLoweringError::UnsupportedItem("type alias"));
                }
                Item::Comptime(_) => {
                    return Err(CanonicalLoweringError::UnsupportedItem("comptime"));
                }
                Item::Function(_) => unreachable!(),
            }
        };
        if !is_canonical_source_function(typed.module(), &function.name) {
            continue;
        }
        let Some(function_type) = source.callable_signature(&function.name).cloned() else {
            // A generic source template has no concrete checked callable body.
            // The sealed frontend witness lists only generated instances.
            continue;
        };
        let function_id = FunctionId::new(
            u32::try_from(functions_by_name.len()).map_err(|_| CanonicalLoweringError::IdOverflow)?,
        );
        let signature_id = *function_types
            .get(function_type.unqualified())
            .ok_or_else(|| CanonicalLoweringError::UnsupportedType(function_type.clone()))?;
        functions_by_name.insert(function.name.clone(), (function_id, signature_id));
    }
    // Lambdas become functions of their own, bound to their closure
    // witness: the closure record is their leading parameter and every
    // `Int` capture is loaded from it at entry. Each is met again by node
    // address when its function body is lowered.
    let sites = lambda_sites(typed)
        .map_err(|error| CanonicalLoweringError::UnsupportedCanonicalObligation(format!("closure witness: {error}")))?;
    let witnesses = source.closure_witness().entries();
    let mut lambda_names: HashMap<usize, String> = HashMap::new();
    let mut lambda_functions: Vec<LambdaFunction<'_>> = Vec::new();
    let source_function_names = functions_by_name.keys().cloned().collect::<HashSet<String>>();
    for (item_index, item) in typed.module().items.iter().enumerate() {
        let Item::Function(function) = item else { continue };
        if !is_canonical_source_function(typed.module(), &function.name) {
            continue;
        }
        let item_index = u32::try_from(item_index).map_err(|_| CanonicalLoweringError::IdOverflow)?;
        let mut visit_error = None;
        let _: Result<(), ()> = witchy_syntax::ast::visit::visit_block(&function.body, &mut |expression| {
            let Expr::Lambda { params, body, qualifiers, .. } = expression else { return Ok(()) };
            let address = expression as *const Expr as usize;
            let Some(site) = sites.iter().find(|site| site.expr_address == address) else { return Ok(()) };
            let Some(witness_index) = witnesses.iter().position(|witness| witness.id() == &site.id) else { return Ok(()) };
            let captures = witnesses[witness_index].captures();
            if captures.iter().any(|capture| !is_int_type(capture.checked_type())) {
                return Ok(());
            }
            let capture_names = captures.iter().map(|capture| capture.binding().name().to_owned()).collect::<Vec<_>>();
            let Some(fn_type) = typed.table().type_of(expression).and_then(witchy_types::typeck::ty_to_ast) else { return Ok(()) };
            let Type::Fn(parameter_types, result_type, _, _) = fn_type.unqualified() else { return Ok(()) };
            let Some(&signature_id) = closure_signatures.get(&crate::canonical_layouts::closure_key(&fn_type)) else { return Ok(()) };
            let name = format!("{}#lambda{}", function.name, lambda_functions.len());
            let params = std::iter::once(closure_environment_param())
                .chain(params
                    .iter()
                    .zip(parameter_types)
                    .map(|(parameter, ty)| witchy_syntax::ast::Param { ty: Some(parameter.ty.clone().unwrap_or_else(|| ty.clone())), ..parameter.clone() }))
                .collect::<Vec<_>>();
            let synthesized = witchy_syntax::ast::Function {
                line: 0,
                public: false,
                pure: qualifiers.pure,
                comptime_only: false,
                attributes: Vec::new(),
                name: name.clone(),
                params,
                ret: Some((**result_type).clone()),
                body: body.clone(),
                bounds: Vec::new(),
                is_gen: false,
                is_async: false,
            };
            let function_id = match u32::try_from(functions_by_name.len()) {
                Ok(index) => FunctionId::new(index),
                Err(_) => {
                    visit_error = Some(CanonicalLoweringError::IdOverflow);
                    return Err(());
                }
            };
            functions_by_name.insert(name.clone(), (function_id, signature_id));
            lambda_names.insert(address, name);
            closure_context.lambda_captures.insert(address, capture_names.clone());
            let witness_index = match u32::try_from(witness_index) {
                Ok(index) => index,
                Err(_) => {
                    visit_error = Some(CanonicalLoweringError::IdOverflow);
                    return Err(());
                }
            };
            lambda_functions.push(LambdaFunction {
                synthesized,
                body,
                function_id,
                item_index,
                lambda_path: site.id.lambda_path.clone(),
                signature_id,
                witness_index,
                captures: capture_names,
            });
            Ok(())
        });
        if let Some(error) = visit_error {
            return Err(error);
        }
    }
    // A named function used as a value is a closure whose code is an
    // adapter: the function's own signature behind the closure record.
    let mut adapters: Vec<AdapterFunction> = Vec::new();
    let mut adapter_function_names = HashSet::new();
    let mut adapter_targets = BTreeSet::new();
    for item in &typed.module().items {
        let Item::Function(function) = item else { continue };
        if !is_canonical_source_function(typed.module(), &function.name) {
            continue;
        }
        let _: Result<(), ()> = witchy_syntax::ast::visit::visit_block(&function.body, &mut |expression| {
            if let Expr::Var(name) = expression && source_function_names.contains(name) {
                adapter_targets.insert(name.clone());
            }
            Ok(())
        });
    }
    for target in adapter_targets {
        let Some((target_id, target_signature)) = functions_by_name.get(&target).copied() else { continue };
        let Some(fn_type) = source.callable_signature(&target) else { continue };
        let fn_type = crate::canonical_layouts::closure_key(fn_type);
        let Some(&signature_id) = closure_signatures.get(&fn_type) else { continue };
        let adapter_id = FunctionId::new(
            u32::try_from(functions_by_name.len()).map_err(|_| CanonicalLoweringError::IdOverflow)?,
        );
        let adapter_name = format!("{target}#adapter");
        functions_by_name.insert(adapter_name.clone(), (adapter_id, signature_id));
        adapter_function_names.insert(adapter_name);
        closure_context.adapters.insert(target.clone(), (adapter_id, signature_id, fn_type));
        adapters.push(AdapterFunction { id: adapter_id, signature_id, target: target_id, target_signature });
    }
    let checked_parameter_conventions = callable_types
        .iter()
        .filter_map(|ty| {
            let Type::Fn(parameters, _, conventions, _) = ty.unqualified() else {
                return None;
            };
            let conventions = if conventions.is_empty() {
                vec![Convention::Let; parameters.len()]
            } else {
                conventions.clone()
            };
            Some((function_types[ty], conventions))
        })
        .collect::<HashMap<_, _>>();
    let mut checked_parameter_conventions = checked_parameter_conventions;
    for ty in &callable_types {
        let (Some(conventions), Some(&closure_signature)) = (checked_parameter_conventions.get(&function_types[ty]).cloned(), closure_signatures.get(ty)) else { continue };
        let mut with_environment = vec![Convention::Let];
        with_environment.extend(conventions);
        checked_parameter_conventions.insert(closure_signature, with_environment);
    }
    for item in &typed.module().items {
        let Item::Function(function) = item else { continue };
        if !is_canonical_source_function(typed.module(), &function.name) {
            continue;
        }
        let Some((_, signature_id)) = functions_by_name.get(&function.name).copied() else {
            continue;
        };
        signatures[signature_id.get() as usize] = checked_signature(
            source,
            function,
            signature_id,
            &layouts,
            &layout_catalog,
            &closure_layouts,
            &definitions,
        )?;
    }

    let mut imports = Vec::new();
    let mut abi_catalog = None;
    let mut host_imports = HostImports::default();
    let prints = contains_console_print(typed.module());
    let floats = renders_float(typed.module(), typed.table(), &definitions);
    if prints || floats {
        let string_layout = string_layout(
            &Type::Named("String".into(), Vec::new()),
            &layouts,
            &layout_catalog,
        )?;
        let signature_id = FunctionSignatureId::new(
            u32::try_from(signatures.len()).map_err(|_| CanonicalLoweringError::IdOverflow)?,
        );
        signatures.push(FunctionSignature {
            id: signature_id,
            parameters: vec![ValueType::Ref(string_layout)],
            results: Vec::new(),
            contract: CallableContract {
                parameters: vec![ParameterContract {
                    value_type: ValueType::Ref(string_layout),
                    // Host buffer roles, not callable ownership, authorize this
                    // import's read-only string access.
                    ownership: ParameterOwnership::Value,
                }],
                results: Vec::new(),
                parameter_effects: BTreeMap::from([(0, vec![MemoryEffect::Read])]),
            },
        });
        let float_signature_id = FunctionSignatureId::new(
            u32::try_from(signatures.len()).map_err(|_| CanonicalLoweringError::IdOverflow)?,
        );
        signatures.push(FunctionSignature {
            id: float_signature_id,
            parameters: vec![ValueType::Float, ValueType::Ref(string_layout)],
            results: vec![ValueType::Size],
            contract: CallableContract {
                parameters: vec![
                    ParameterContract { value_type: ValueType::Float, ownership: ParameterOwnership::Value },
                    ParameterContract {
                        value_type: ValueType::Ref(string_layout),
                        // The host buffer role, not callable ownership,
                        // authorizes this import's bounded write.
                        ownership: ParameterOwnership::Value,
                    },
                ],
                results: vec![ResultContract::Value(ValueType::Size)],
                parameter_effects: BTreeMap::from([(1, vec![MemoryEffect::Write])]),
            },
        });
        let catalog = CanonicalAbiCatalog::from_authenticated_prelude(signature_id, float_signature_id, string_layout)
            .map_err(CanonicalLoweringError::InvalidAuthenticatedAbiCatalog)?;
        // The module declares only the imports its functions reach; the
        // catalog still authenticates each one it declares.
        let projected = catalog
            .imports()
            .into_iter()
            .filter(|import| (import.name == "print" && prints) || (import.name == "float_to_str" && floats))
            .collect::<Vec<_>>();
        host_imports = HostImports {
            print: projected.iter().find(|import| import.name == "print").map(|import| import.id),
            float_to_str: projected.iter().find(|import| import.name == "float_to_str").map(|import| import.id),
        };
        imports = projected;
        abi_catalog = Some(catalog);
    }

    let mut functions = Vec::new();
    for (item_index, item) in typed.module().items.iter().enumerate() {
        let Item::Function(function) = item else { continue };
        if !is_canonical_source_function(typed.module(), &function.name) {
            continue;
        }
        let Some((function_id, signature_id)) = functions_by_name.get(&function.name).copied() else {
            continue;
        };
        let signature = &signatures[signature_id.get() as usize];
        functions.push(lower_function(
            function,
            &function.body,
            function_id,
            u32::try_from(item_index).map_err(|_| CanonicalLoweringError::IdOverflow)?,
            Vec::new(),
            &lambda_names,
            signature,
            &functions_by_name,
            &checked_parameter_conventions,
            &signatures,
            &tail_manifest,
            typed.table(),
            &layouts,
            &layout_catalog,
            &string_data_ids,
            host_imports,
            &packed_records,
            &definitions,
            &closure_context,
            &[],
        )?);
    }

    let static_data_segments = string_payloads
        .iter()
        .enumerate()
        .map(|(index, payload)| {
            let length = u32::try_from(payload.len()).map_err(|_| CanonicalLoweringError::IdOverflow)?;
            let mut bytes = Vec::with_capacity(4 + payload.len());
            bytes.extend_from_slice(&length.to_le_bytes());
            bytes.extend_from_slice(payload);
            Ok(witchy_wir::canonical_safewir::StaticDataSegment::new(
                witchy_wir::canonical_safewir::StaticDataId::new(
                    u32::try_from(index).map_err(|_| CanonicalLoweringError::IdOverflow)?,
                ),
                bytes,
            ))
        })
        .collect::<Result<Vec<_>, CanonicalLoweringError>>()?;

    let mut closure_bindings = Vec::new();
    for lambda in &lambda_functions {
        let signature = &signatures[lambda.signature_id.get() as usize];
        functions.push(lower_function(
            &lambda.synthesized,
            lambda.body,
            lambda.function_id,
            lambda.item_index,
            lambda.lambda_path.clone(),
            &lambda_names,
            signature,
            &functions_by_name,
            &checked_parameter_conventions,
            &signatures,
            &tail_manifest,
            typed.table(),
            &layouts,
            &layout_catalog,
            &string_data_ids,
            host_imports,
            &packed_records,
            &definitions,
            &closure_context,
            &lambda.captures,
        )?);
        closure_bindings.push(CandidateClosureBinding::new(lambda.function_id, lambda.witness_index));
    }
    let mut adapter_bindings = Vec::new();
    for adapter in &adapters {
        functions.push(synthesize_adapter(adapter, &signatures));
        adapter_bindings.push(CandidateAdapterBinding::new(adapter.id, adapter.target));
    }
    let lambda_function_names = lambda_names.values().cloned().collect::<HashSet<_>>();
    let lambda_callables = lambda_functions
        .iter()
        .map(|lambda| {
            (ProperTailCallableId { item: lambda.item_index, method: None, lambda_path: lambda.lambda_path.clone() }, lambda.function_id)
        })
        .collect::<HashMap<_, _>>();
    let mut abi_bindings = functions_by_name
        .iter()
        .filter(|(name, _)| !lambda_function_names.contains(*name) && !adapter_function_names.contains(*name))
        .map(|(name, (function, _signature))| {
            let source_abi = source.closed_callable_abi(name)
                .ok_or_else(|| CanonicalLoweringError::MissingCheckedFunctionType(name.clone()))?;
            let witchy_types::canonical_abi::CanonicalTargetPolicy::ClosedLocal(target) = source_abi.target_policy else {
                return Err(CanonicalLoweringError::InvalidCheckedFunctionType(name.clone()));
            };
            Ok(CandidateAbiBinding::new(
                *function,
                target.function_ordinal,
            ))
        })
        .collect::<Result<Vec<_>, CanonicalLoweringError>>()?;
    abi_bindings.sort_by_key(|binding| binding.source_declaration_ordinal());

    let candidate = CanonicalModule::from_lowering_with_static_data(LoweredModuleParts {
        revision: Revision::new(0),
        checked_source: CheckedSourceIdentity::from_digest(
            source.source_witness().source_bundle_digest(),
        ),
        layouts,
        signatures,
        imports,
        functions,
        proper_tail_sites: canonical_tail_sites(tail_manifest, typed.module(), &lambda_callables)?,
    }, static_data_segments)
        .with_source_abi_bindings(CandidateAbiBindings::new(abi_bindings))
        .with_closure_bindings(CandidateClosureBindings::new(closure_bindings))
        .with_adapter_bindings(CandidateAdapterBindings::new(adapter_bindings))
        .with_closure_layouts(closure_context.signature_by_layout.iter().map(|(layout, signature)| (*layout, *signature)).collect());
    Ok(match abi_catalog {
        Some(catalog) => candidate.with_abi_catalog(catalog),
        None => candidate,
    })
}

/// A lambda's synthesized function and the facts its lowering needs.
struct LambdaFunction<'a> {
    synthesized: witchy_syntax::ast::Function,
    body: &'a witchy_syntax::ast::Block,
    function_id: FunctionId,
    item_index: u32,
    lambda_path: Vec<ProperTailPathStep>,
    signature_id: FunctionSignatureId,
    witness_index: u32,
    captures: Vec<String>,
}

/// A named function used as a value: its adapter forwards the closure
/// signature to the function's own.
struct AdapterFunction {
    id: FunctionId,
    signature_id: FunctionSignatureId,
    target: FunctionId,
    target_signature: FunctionSignatureId,
}

/// The closure facts a function's lowering reads: the closure layout of
/// every `fn` type, the closure and declaration signatures behind each
/// layout, each lambda's capture names, and each named function's adapter.
struct ClosureContext {
    layouts: HashMap<Type, witchy_wir::layout::LayoutId>,
    signature_by_layout: HashMap<witchy_wir::layout::LayoutId, FunctionSignatureId>,
    declaration_by_layout: HashMap<witchy_wir::layout::LayoutId, FunctionSignatureId>,
    lambda_captures: HashMap<usize, Vec<String>>,
    adapters: HashMap<String, (FunctionId, FunctionSignatureId, Type)>,
}

/// The leading parameter of a lambda's synthesized function: its closure
/// record. The spelling is internal; the parameter's value type comes from
/// the closure signature.
fn closure_environment_param() -> witchy_syntax::ast::Param {
    witchy_syntax::ast::Param {
        name: "__closure".to_owned(),
        ty: Some(Type::Named("__closure".into(), Vec::new())),
        convention: Convention::Let,
        default: None,
    }
}

fn is_int_type(ty: &Type) -> bool {
    matches!(ty.unqualified(), Type::Named(name, arguments) if name == "Int" && arguments.is_empty())
}

/// The closure signature of a `fn` type: the declaration signature with
/// the closure record prepended as a shared parameter.
fn closure_signature(
    id: FunctionSignatureId,
    layout: witchy_wir::layout::LayoutId,
    declaration: &FunctionSignature,
) -> FunctionSignature {
    let environment = ValueType::Ref(layout);
    let mut parameters = vec![environment];
    parameters.extend(declaration.parameters.iter().copied());
    let mut contract_parameters = vec![ParameterContract { value_type: environment, ownership: ParameterOwnership::Shared }];
    contract_parameters.extend(declaration.contract.parameters.iter().cloned());
    let mut parameter_effects = BTreeMap::from([(0, vec![MemoryEffect::Read, MemoryEffect::Retain, MemoryEffect::Release])]);
    parameter_effects.extend(declaration.contract.parameter_effects.iter().map(|(index, effects)| (index + 1, effects.clone())));
    FunctionSignature {
        id,
        parameters,
        results: declaration.results.clone(),
        contract: CallableContract {
            parameters: contract_parameters,
            results: declaration.contract.results.clone(),
            parameter_effects,
        },
    }
}

/// An adapter: one block that forwards the closure signature's parameters
/// (past the closure record) to the target and returns its results, then
/// drops the record it was handed.
fn synthesize_adapter(adapter: &AdapterFunction, signatures: &[FunctionSignature]) -> Function {
    let signature = &signatures[adapter.signature_id.get() as usize];
    let mut value_types = BTreeMap::new();
    let mut next = 0u32;
    let mut fresh = |value_type: ValueType, value_types: &mut BTreeMap<ValueId, ValueType>| {
        let value = ValueId::new(next);
        next += 1;
        value_types.insert(value, value_type);
        value
    };
    let parameters = signature
        .parameters
        .iter()
        .zip(&signature.contract.parameters)
        .map(|(value_type, contract)| BlockParameter {
            value: fresh(*value_type, &mut value_types),
            value_type: *value_type,
            ownership: match (contract.ownership, value_type) {
                (ParameterOwnership::Unique, _) => Some(Ownership::Unique),
                (ParameterOwnership::Shared | ParameterOwnership::WriteBack, ValueType::Ref(_)) => Some(Ownership::Shared),
                _ => None,
            },
        })
        .collect::<Vec<_>>();
    let arguments = parameters[1..]
        .iter()
        .map(|parameter| if matches!(parameter.value_type, ValueType::Ref(_)) {
            CallArgument::Move(parameter.value)
        } else {
            CallArgument::Value(parameter.value)
        })
        .collect::<Vec<_>>();
    let destinations = signature
        .results
        .iter()
        .map(|value_type| fresh(*value_type, &mut value_types))
        .collect::<Vec<_>>();
    let environment = parameters[0].value;
    let _ = adapter.target_signature;
    Function {
        id: adapter.id,
        signature: adapter.signature_id,
        entry: BlockId::new(0),
        value_types,
        blocks: vec![BasicBlock {
            id: BlockId::new(0),
            parameters,
            operations: vec![
                Operation::Call { destinations: destinations.clone(), callee: adapter.target, arguments },
                Operation::DropRef { object: environment },
            ],
            terminator: Terminator::Return { values: destinations },
        }],
    }
}

fn checked_signature(
    source: &CanonicalCheckedSource,
    function: &witchy_syntax::ast::Function,
    id: FunctionSignatureId,
    layout_bundle: &LayoutBundle,
    layouts: &LayoutInterner,
    closure_layouts: &HashMap<Type, witchy_wir::layout::LayoutId>,
    definitions: &BTreeMap<String, TypeDef>,
) -> Result<FunctionSignature, CanonicalLoweringError> {
    let checked = source
        .callable_signature(&function.name)
        .cloned()
        .ok_or_else(|| CanonicalLoweringError::MissingCheckedFunctionType(function.name.clone()))?;
    let Type::Fn(parameters, _, conventions, _) = &checked else {
        return Err(CanonicalLoweringError::InvalidCheckedFunctionType(function.name.clone()));
    };
    if parameters.len() != function.params.len() {
        return Err(CanonicalLoweringError::InvalidCheckedFunctionType(function.name.clone()));
    }
    let conventions = if conventions.is_empty() {
        vec![Convention::Let; parameters.len()]
    } else {
        conventions.to_vec()
    };
    if conventions.len() != parameters.len() {
        return Err(CanonicalLoweringError::InvalidCheckedFunctionType(function.name.clone()));
    }

    let checked_signature = signature_from_type(id, &checked, layout_bundle, layouts, closure_layouts, definitions).map_err(|error| {
        if std::env::var_os("WITCHY_CANONICAL_LOWER_DEBUG").is_some() {
            eprintln!("canonical signature refused for `{}`: {error}", function.name);
        }
        error
    })?;
    let value_parameters = checked_signature.parameters;
    let contract_parameters = checked_signature.contract.parameters;
    let parameter_effects = checked_signature.contract.parameter_effects;
    for (parameter_index, ((parameter, source), convention)) in
        parameters.iter().zip(&function.params).zip(conventions).enumerate()
    {
        if matches!(convention, Convention::Own) && !ordinary_own_type_supported(parameter) {
            return Err(CanonicalLoweringError::UnsupportedConvention {
                function: function.name.clone(),
                parameter: source.name.clone(),
                convention,
            });
        }
        let _ = parameter;
        let _ = parameter_index;
    }
    Ok(FunctionSignature { id, parameters: value_parameters, results: checked_signature.results, contract: CallableContract {
        parameters: contract_parameters,
        results: checked_signature.contract.results,
        parameter_effects,
    }})
}

fn signature_from_type(
    id: FunctionSignatureId,
    checked: &Type,
    layout_bundle: &LayoutBundle,
    layouts: &LayoutInterner,
    closure_layouts: &HashMap<Type, witchy_wir::layout::LayoutId>,
    definitions: &BTreeMap<String, TypeDef>,
) -> Result<FunctionSignature, CanonicalLoweringError> {
    let Type::Fn(parameters, result, conventions, _) = checked.unqualified() else {
        return Err(CanonicalLoweringError::InvalidCheckedFunctionType(format!("{checked:?}")));
    };
    let conventions = if conventions.is_empty() {
        vec![Convention::Let; parameters.len()]
    } else {
        conventions.clone()
    };
    if conventions.len() != parameters.len() {
        return Err(CanonicalLoweringError::InvalidCheckedFunctionType(format!("{checked:?}")));
    }
    if let Some(unsupported) = parameters
        .iter()
        .zip(&conventions)
        .map(|(ty, convention)| var_parameter_type(ty, *convention))
        .chain(std::iter::once(result.as_ref().clone()))
        .find(contains_deferred_v1_qualifier)
    {
        return Err(CanonicalLoweringError::UnsupportedType(unsupported));
    }
    let mut value_parameters = Vec::with_capacity(parameters.len());
    let mut contract_parameters = Vec::with_capacity(parameters.len());
    let mut parameter_effects = BTreeMap::new();
    for (index, (parameter, convention)) in parameters.iter().zip(conventions).enumerate() {
        if matches!(convention, Convention::Own) && !ordinary_own_type_supported(parameter) {
            return Err(CanonicalLoweringError::UnsupportedConvention {
                function: format!("signature {:?}", id),
                parameter: index.to_string(),
                convention,
            });
        }
        let value_type = source_value_type(parameter, layout_bundle, layouts, closure_layouts, definitions)?;
        // An explicit `let` (`Convention::Borrow`) has the default convention's
        // observable semantics; its no-escape rule is the type checker's. Under
        // the v1 owned-reference ABI it is one owned token like `let`/`own`.
        let ownership = match (value_type, convention) {
            (ValueType::Ref(_), Convention::Let | Convention::Borrow | Convention::Own) => {
                ParameterOwnership::Shared
            }
            // A `var` parameter is written back as an extra result lane. For a
            // reference that lane is the replacement token; for a scalar it is
            // a plain value and the parameter carries no memory token at all.
            (_, Convention::Var) => ParameterOwnership::WriteBack,
            (_, Convention::Let | Convention::Borrow | Convention::Own) => ParameterOwnership::Value,
        };
        if matches!(value_type, ValueType::Ref(_)) {
            parameter_effects.insert(
                index as u32,
                vec![
                    MemoryEffect::Read,
                    MemoryEffect::Retain,
                    MemoryEffect::Release,
                ],
            );
        }
        value_parameters.push(value_type);
        contract_parameters.push(ParameterContract { value_type, ownership });
    }
    let result = source_value_type(result, layout_bundle, layouts, closure_layouts, definitions)?;
    let mut results = (!is_unit(result)).then_some(result).into_iter().collect::<Vec<_>>();
    let mut result_contracts = results.iter().copied().map(|result| match result {
        ValueType::Ref(layout) => ResultContract::Shared { layout },
        result => ResultContract::Value(result),
    }).collect::<Vec<_>>();
    for parameter in contract_parameters.iter().filter(|parameter| parameter.ownership == ParameterOwnership::WriteBack) {
        results.push(parameter.value_type);
        result_contracts.push(ResultContract::BodyDerived(parameter.value_type));
    }
    Ok(FunctionSignature {
        id,
        parameters: value_parameters,
        results,
        contract: CallableContract { parameters: contract_parameters, results: result_contracts, parameter_effects },
    })
}

/// `unique` on a `var` parameter names the write-back convention the
/// canonical ABI already carries (the callee holds the value's token and
/// hands the value back); it is not a deferred optimization qualifier, so
/// the parameter reads as its plain type.
fn var_parameter_type(ty: &Type, convention: Convention) -> Type {
    match ty {
        Type::Qualified(TypeQual::Unique, inner) if matches!(convention, Convention::Var) => (**inner).clone(),
        _ => ty.clone(),
    }
}

fn contains_deferred_v1_qualifier(ty: &Type) -> bool {
    match ty {
        Type::Qualified(TypeQual::Frozen, inner) => contains_deferred_v1_qualifier(inner),
        Type::Qualified(
            TypeQual::Unique
            | TypeQual::LocalUnique
            | TypeQual::Borrow(_)
            | TypeQual::LegacyBorrow(_)
            | TypeQual::BorrowMut(_),
            _,
        ) => true,
        Type::Named(_, arguments) | Type::Tuple(arguments) | Type::Dyn(_, arguments) => {
            arguments.iter().any(contains_deferred_v1_qualifier)
        }
        Type::Slice(inner) => contains_deferred_v1_qualifier(inner),
        Type::RecordCompose { base, fields } => {
            contains_deferred_v1_qualifier(base)
                || fields.iter().any(|(_, field)| contains_deferred_v1_qualifier(field))
        }
        Type::Fn(parameters, result, _, _) => {
            parameters.iter().any(contains_deferred_v1_qualifier)
                || contains_deferred_v1_qualifier(result)
        }
    }
}

fn deferred_v1_qualifier_in_block(block: &witchy_syntax::ast::Block) -> Option<Type> {
    if let Some(ty) = block.region.as_ref().and_then(|region| region.ty.as_ref())
        && contains_deferred_v1_qualifier(ty)
    {
        return Some(ty.clone());
    }
    for statement in &block.stmts {
        if let Stmt::Let { ty: Some(ty), .. } = statement
            && contains_deferred_v1_qualifier(ty)
        {
            return Some(ty.clone());
        }
        let expression = match statement {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Yield(value)
            | Stmt::Expr(value)
            | Stmt::Return(Some(value)) => Some(value),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => None,
        };
        let Some(expression) = expression else { continue };
        let mut unsupported = None;
        let result = witchy_syntax::ast::visit::visit_expr(expression, &mut |expression| {
            let types: Vec<&Type> = match expression {
                Expr::Lambda { params, ret, .. } => params
                    .iter()
                    .filter_map(|parameter| parameter.ty.as_ref())
                    .chain(ret.iter())
                    .collect(),
                Expr::As { ty, .. }
                | Expr::ExistentialPack { ty, .. }
                | Expr::ExistentialUpcast { ty, .. }
                | Expr::ExistentialCall { ty, .. } => vec![ty],
                _ => Vec::new(),
            };
            if let Some(ty) = types.into_iter().find(|ty| contains_deferred_v1_qualifier(ty)) {
                unsupported = Some(ty.clone());
                return Err(());
            }
            if let Expr::Lambda { body, .. } | Expr::Block(body) = expression
                && let Some(ty) = deferred_v1_qualifier_in_block(body)
            {
                unsupported = Some(ty);
                return Err(());
            }
            Ok(())
        });
        if result.is_err() {
            return unsupported;
        }
    }
    None
}

/// v1 represents scalar consumption and one existing owned token for ordinary
/// String and recursively supported List values. Explicit qualifier state,
/// capabilities, and other reference state remain fail-closed.
fn ordinary_own_type_supported(ty: &Type) -> bool {
    match ty {
        Type::Named(name, arguments) if arguments.is_empty() => {
            matches!(name.as_str(), "Int" | "Float" | "Duration" | "Bool" | "Nil" | "Unit" | "String")
        }
        Type::Named(name, arguments) if name == "List" && arguments.len() == 1 => {
            ordinary_own_type_supported(&arguments[0])
        }
        _ => false,
    }
}

fn write_back_count(signature: &FunctionSignature) -> usize {
    signature.contract.parameters.iter()
        .filter(|parameter| parameter.ownership == ParameterOwnership::WriteBack)
        .count()
}

fn collect_callable_types(ty: &Type, out: &mut Vec<Type>) {
    let ty = ty.unqualified();
    match ty {
        Type::Fn(parameters, result, _, _) => {
            out.push(ty.clone());
            for parameter in parameters { collect_callable_types(parameter, out); }
            collect_callable_types(result, out);
        }
        Type::Named(_, arguments) | Type::Tuple(arguments) => {
            for argument in arguments { collect_callable_types(argument, out); }
        }
        Type::Slice(inner) => collect_callable_types(inner, out),
        _ => {}
    }
}

/// `body` is the block to lower: the function's own for a declaration, the
/// lambda's original node for a synthesized lambda function (the type table
/// is keyed by node address, so a clone would carry no types).
#[allow(clippy::too_many_arguments)]
fn lower_function(
    function: &witchy_syntax::ast::Function,
    body: &witchy_syntax::ast::Block,
    id: FunctionId,
    item: u32,
    lambda_path: Vec<ProperTailPathStep>,
    lambda_names: &HashMap<usize, String>,
    signature: &FunctionSignature,
    functions: &HashMap<String, (FunctionId, FunctionSignatureId)>,
    checked_parameter_conventions: &HashMap<FunctionSignatureId, Vec<Convention>>,
    signatures: &[FunctionSignature],
    tail_manifest: &ProperTailSiteManifest,
    type_table: &TypeTable,
    layout_bundle: &LayoutBundle,
    layouts: &LayoutInterner,
    string_data_ids: &BTreeMap<Vec<u8>, witchy_wir::canonical_safewir::StaticDataId>,
    host_imports: HostImports,
    packed_records: &HashMap<String, &TypeDef>,
    definitions: &BTreeMap<String, TypeDef>,
    closures: &ClosureContext,
    captures: &[String],
) -> Result<Function, CanonicalLoweringError> {
    let mut lowerer = FunctionLowerer {
        function: function.name.clone(),
        next_value: 0,
        values: BTreeMap::new(),
        bindings: HashMap::new(),
        string_lengths: HashMap::new(),
        string_capacities: HashMap::new(),
        list_counts: HashMap::new(),
        list_capacities: HashMap::new(),
        known_list_lengths: HashMap::new(),
        owned_parameters: Vec::new(),
        token_join_parameters: BTreeSet::new(),
        mutable_bindings: HashSet::new(),
        loop_contexts: Vec::new(),
        lambda_names,
        tail_prefix: if lambda_path.is_empty() { Vec::new() } else { lambda_path.iter().cloned().chain([ProperTailPathStep::Child(0)]).collect() },
        lambda_path,
        parameter_names: BTreeMap::new(),
        results: Vec::new(),
        write_back_parameters: Vec::new(),
        write_back_parameter_types: HashMap::new(),
        blocks: Vec::new(),
        current: BlockId::new(0),
        item,
        functions,
        checked_parameter_conventions,
        signatures,
        tail_manifest,
        type_table,
        layout_bundle,
        layouts,
        string_data_ids,
        host_imports,
        packed_records,
        definitions,
        closures,
        result_type: function.ret.clone(),
    };
    let entry = lowerer.new_block(&signature.parameters)?;
    for (parameter, contract) in lowerer.blocks[entry.get() as usize]
        .parameters.iter_mut().zip(&signature.contract.parameters)
    {
        parameter.ownership = match (contract.ownership, parameter.value_type) {
            (ParameterOwnership::Unique, _) => Some(Ownership::Unique),
            (ParameterOwnership::Shared | ParameterOwnership::WriteBack, ValueType::Ref(_)) => Some(Ownership::Shared),
            (ParameterOwnership::Shared | ParameterOwnership::WriteBack | ParameterOwnership::Value, _) => None,
        };
    }
    let parameters = lowerer.blocks[entry.get() as usize].parameters.clone();
    lowerer.owned_parameters = parameters
        .iter()
        .zip(&signature.contract.parameters)
        .filter_map(|(parameter, contract)| {
            (matches!(parameter.value_type, ValueType::Ref(_))
                && matches!(contract.ownership, ParameterOwnership::Unique | ParameterOwnership::Shared | ParameterOwnership::WriteBack))
            .then_some(parameter.value)
        })
        .collect();
    for (source, parameter) in function.params.iter().zip(&parameters) {
        if lowerer.bindings.insert(source.name.clone(), parameter.value).is_some() {
            return Err(CanonicalLoweringError::DuplicateBinding(source.name.clone()));
        }
        lowerer.parameter_names.insert(parameter.value, source.name.clone());
    }
    lowerer.write_back_parameters = function.params.iter().zip(&signature.contract.parameters)
        .filter(|(_, contract)| contract.ownership == ParameterOwnership::WriteBack)
        .map(|(parameter, _)| parameter.name.clone())
        .collect();
    lowerer.write_back_parameter_types = function.params.iter().zip(&signature.contract.parameters)
        .filter(|(_, contract)| contract.ownership == ParameterOwnership::WriteBack)
        .filter_map(|(parameter, _)| parameter.ty.clone().map(|ty| (parameter.name.clone(), ty)))
        .collect();
    // A lambda's captures are the `Int` slots of its closure record, the
    // leading parameter, bound by their source names.
    if !captures.is_empty() {
        let environment = parameters.first().map(|parameter| parameter.value).ok_or_else(|| {
            CanonicalLoweringError::InvalidCheckedFunctionType(function.name.clone())
        })?;
        for (index, capture) in captures.iter().enumerate() {
            let slot = u32::try_from(index + 1).map_err(|_| CanonicalLoweringError::IdOverflow)?;
            let value = lowerer.load_record_field(environment, slot, ValueType::Int)?;
            if lowerer.bindings.insert(capture.clone(), value).is_some() {
                return Err(CanonicalLoweringError::DuplicateBinding(capture.clone()));
            }
        }
    }

    let ordinary_results = &signature.results[..signature.results.len() - write_back_count(signature)];
    lowerer.lower_body(&body.stmts, ordinary_results)?;
    let blocks = lowerer
        .blocks
        .into_iter()
        .map(|block| {
            Ok(BasicBlock {
                id: block.id,
                parameters: block.parameters,
                operations: block.operations,
                terminator: block.terminator.ok_or_else(|| {
                    CanonicalLoweringError::MissingReturn(function.name.clone())
                })?,
            })
        })
        .collect::<Result<Vec<_>, CanonicalLoweringError>>()?;
    let mut lowered = Function {
        id,
        signature: signature.id,
        entry,
        value_types: lowerer.values,
        blocks,
    };
    let debug = std::env::var_os("WITCHY_CANONICAL_LOWER_DEBUG").is_some();
    if debug {
        eprintln!("canonical lowered `{}` before release placement:\n{lowered:#?}", function.name);
    }
    owned::insert_releases(&mut lowered, signature, signatures, functions)
        .map_err(|error| CanonicalLoweringError::OwnedReleasePlacement(format!("{error:?}")))?;
    if debug {
        eprintln!("canonical lowered `{}`:\n{lowered:#?}", function.name);
    }
    Ok(lowered)
}

/// The names a loop body rebinds: assigned (`x = e`) or pushed in place.
#[derive(Default)]
struct LoopRebindings {
    assigned: BTreeSet<String>,
    pushed: BTreeSet<String>,
}

/// Cached extent facts (see `snapshot_facts`).
#[derive(Clone)]
struct FactSnapshot {
    string_lengths: HashMap<ValueId, ValueId>,
    string_capacities: HashMap<ValueId, ValueId>,
    list_counts: HashMap<ValueId, ValueId>,
    list_capacities: HashMap<ValueId, ValueId>,
    known_list_lengths: HashMap<ValueId, u64>,
}

/// The loop a `break`/`continue` leaves or re-enters: its header and exit
/// blocks, the state lanes both expect (every outer binding, then each
/// tracked list's count/capacity), and how the loop advances.
struct LoopContext {
    header: BlockId,
    exit: BlockId,
    names: Vec<String>,
    list_names: Vec<String>,
    advance: LoopAdvance,
}

enum LoopAdvance {
    /// A `while` loop: the header re-evaluates its condition.
    Condition,
    /// A `for` over a list: the body's cursor, incremented on `continue`;
    /// the exit carries no cursor.
    ListCursor(ValueId),
    /// A range `for`: the body's counter, incremented on `continue` and
    /// carried as is on `break` (the exit keeps the counter lane).
    IntCounter(ValueId),
}

/// Whether a loop body leaves the loop early (a `break` outside any nested
/// loop), so the exit block is a join of the header and the break edges.
fn block_breaks_loop(block: &witchy_syntax::ast::Block) -> bool {
    fn walk_block(block: &witchy_syntax::ast::Block) -> bool {
        block.stmts.iter().any(|statement| match statement {
            Stmt::Break => true,
            Stmt::Expr(expression) => walk_expr(expression),
            Stmt::Let { value, .. } | Stmt::LetPattern { value, .. } | Stmt::Assign { value, .. } => walk_expr(value),
            _ => false,
        })
    }
    fn walk_expr(expression: &Expr) -> bool {
        match expression {
            Expr::If { cond, then_block, else_block } => {
                walk_expr(cond) || walk_block(then_block) || else_block.as_ref().is_some_and(walk_block)
            }
            Expr::Match { arms, .. } => arms.iter().any(|arm| walk_expr(&arm.body)),
            Expr::Block(block) => walk_block(block),
            // A nested loop's `break` leaves that loop, not this one.
            Expr::While { .. } | Expr::For { .. } | Expr::WhileLet { .. } => false,
            _ => false,
        }
    }
    walk_block(block)
}

/// The outer names a loop threads as header lanes: those its body rebinds
/// (assigns, pushes, or passes to a write-back parameter), sorted.
fn loop_lane_names(outer: &HashMap<String, ValueId>, rebound: &LoopRebindings) -> Vec<String> {
    let mut names = rebound
        .assigned
        .iter()
        .chain(rebound.pushed.iter())
        .filter(|name| outer.contains_key(*name))
        .cloned()
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

/// The bindings inside or after a loop: every outer binding, with the
/// threaded names rebound to their lane values.
fn lane_bindings(
    outer: &HashMap<String, ValueId>,
    names: &[String],
    values: impl Iterator<Item = ValueId>,
) -> HashMap<String, ValueId> {
    let mut bindings = outer.clone();
    bindings.extend(names.iter().cloned().zip(values));
    bindings
}

fn loop_rebindings(block: &witchy_syntax::ast::Block) -> LoopRebindings {
    fn walk_block(block: &witchy_syntax::ast::Block, out: &mut LoopRebindings) {
        for statement in &block.stmts {
            match statement {
                Stmt::Assign { name, value } => {
                    if let Expr::Call { name: call, .. } = value
                        && matches!(call.as_str(), "list.push" | "list.__push")
                    {
                        out.pushed.insert(name.clone());
                    } else {
                        out.assigned.insert(name.clone());
                    }
                    walk_expr(value, out);
                }
                Stmt::Expr(value) => {
                    if let Expr::Call { name: call, args } = value
                        && matches!(call.as_str(), "list.push" | "list.__push")
                        && let Some(Expr::Var(receiver)) = args.first()
                    {
                        out.pushed.insert(receiver.clone());
                    }
                    walk_expr(value, out);
                }
                Stmt::Let { value, .. } | Stmt::LetPattern { value, .. } | Stmt::Return(Some(value)) | Stmt::Yield(value) => {
                    walk_expr(value, out);
                }
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
    }
    fn walk_expr(expression: &Expr, out: &mut LoopRebindings) {
        match expression {
            Expr::If { then_block, else_block, .. } => {
                walk_block(then_block, out);
                if let Some(else_block) = else_block {
                    walk_block(else_block, out);
                }
            }
            Expr::Match { arms, .. } => {
                for arm in arms {
                    walk_expr(&arm.body, out);
                }
            }
            Expr::While { body, .. } | Expr::For { body, .. } | Expr::WhileLet { body, .. } => walk_block(body, out),
            Expr::Block(block) => walk_block(block, out),
            _ => {}
        }
    }
    let mut out = LoopRebindings::default();
    walk_block(block, &mut out);
    out
}

/// See `plan_binding_lanes`.
impl BindingLanePlan {
    /// A phi over a collection carries count and capacity lanes; one over a
    /// fixed layout carries none.
    fn has_lanes(&self) -> bool {
        self.phi.is_some() && self.extents.as_ref().is_some_and(|extents| !extents.is_empty())
    }
}

struct BindingLanePlan {
    name: String,
    values: Vec<ValueId>,
    extents: Option<Vec<(ValueId, ValueId)>>,
    phi: Option<Ownership>,
    all_tokens: bool,
}

struct BlockBuilder {
    id: BlockId,
    parameters: Vec<BlockParameter>,
    operations: Vec<Operation>,
    terminator: Option<Terminator>,
}

struct FunctionLowerer<'a> {
    function: String,
    next_value: u32,
    values: BTreeMap<ValueId, ValueType>,
    bindings: HashMap<String, ValueId>,
    /// Names bound by `var`: a later in-place mutation needs their token
    /// unique, so a call receives a copy of such a value, never a clone.
    mutable_bindings: HashSet<String>,
    /// The enclosing loops, innermost last: where `break` and `continue`
    /// branch and which lanes they carry.
    loop_contexts: Vec<LoopContext>,
    /// The synthesized function of every capture-free scalar lambda, keyed
    /// by the lambda expression's node address in the checked module.
    lambda_names: &'a HashMap<usize, String>,
    /// This function's lambda path when it implements a lambda (its tail
    /// sites are keyed by the lambda's structural identity), and the path
    /// prefix its body statements are addressed under.
    lambda_path: Vec<ProperTailPathStep>,
    tail_prefix: Vec<ProperTailPathStep>,
    string_lengths: HashMap<ValueId, ValueId>,
    string_capacities: HashMap<ValueId, ValueId>,
    list_counts: HashMap<ValueId, ValueId>,
    list_capacities: HashMap<ValueId, ValueId>,
    known_list_lengths: HashMap<ValueId, u64>,
    owned_parameters: Vec<ValueId>,
    /// Join parameters whose every lane carried a reference-count token, so
    /// the parameter carries one too (see `carries_token`).
    token_join_parameters: BTreeSet<ValueId>,
    /// Source name of each entry parameter, so an exit drop targets the
    /// token under its current binding (a loop threads parameters through
    /// block parameters) rather than the entry value.
    parameter_names: BTreeMap<ValueId, String>,
    write_back_parameters: Vec<String>,
    /// The source type of each `var` parameter, for the owned copy a result
    /// lane takes when it names the same value as the write-back lane.
    write_back_parameter_types: HashMap<String, Type>,
    /// The function's ordinary result types, so a `return` nested in a block
    /// can build the same terminator the body tail does.
    results: Vec<ValueType>,
    blocks: Vec<BlockBuilder>,
    current: BlockId,
    item: u32,
    functions: &'a HashMap<String, (FunctionId, FunctionSignatureId)>,
    checked_parameter_conventions: &'a HashMap<FunctionSignatureId, Vec<Convention>>,
    signatures: &'a [FunctionSignature],
    tail_manifest: &'a ProperTailSiteManifest,
    type_table: &'a TypeTable,
    layout_bundle: &'a LayoutBundle,
    layouts: &'a LayoutInterner,
    string_data_ids: &'a BTreeMap<Vec<u8>, witchy_wir::canonical_safewir::StaticDataId>,
    host_imports: HostImports,
    closures: &'a ClosureContext,
    packed_records: &'a HashMap<String, &'a TypeDef>,
    /// Every type definition, keyed by bare and entry-qualified names; the
    /// scalar aggregate shapes (records, sums, scalar `Option`/`Result`)
    /// are derived from it on demand.
    definitions: &'a BTreeMap<String, TypeDef>,
    /// The function's declared result type, for `?` to build its failure.
    result_type: Option<Type>,
}

impl<'a> FunctionLowerer<'a> {
    /// The scalar-record definition a type or constructor name denotes; the
    /// checker's entry-qualified spelling and the bare source name both key it.
    fn record_definition(&self, name: &str) -> Option<&'a TypeDef> {
        self.definitions
            .get(name)
            .or_else(|| name.rsplit('.').next().and_then(|bare| self.definitions.get(bare)))
    }
}

impl FunctionLowerer<'_> {
    fn lower_body(
        &mut self,
        statements: &[Stmt],
        results: &[ValueType],
    ) -> Result<(), CanonicalLoweringError> {
        self.results = results.to_vec();
        for (index, statement) in statements.iter().enumerate() {
            self.require_open_block()?;
            let final_statement = index + 1 == statements.len();
            match statement {
                Stmt::Let { name, value, .. } if name == "_" => {
                    // `let _ = e` keeps `e`'s effects and discards its value.
                    self.lower_effect_expr(value)?;
                }
                Stmt::Let { name, mutable, value, .. } => {
                    let value = self.lower_let_value(*mutable, value)?;
                    if self.bindings.insert(name.clone(), value).is_some() {
                        return Err(CanonicalLoweringError::DuplicateBinding(name.clone()));
                    }
                    if *mutable {
                        self.mutable_bindings.insert(name.clone());
                    }
                }
                Stmt::Assign { name, value } => {
                    if !self.bindings.contains_key(name) {
                        return Err(CanonicalLoweringError::MissingBinding(name.clone()));
                    }
                    if let Expr::Call { name: call, args } = value
                        && matches!(call.as_str(), "list.push" | "list.__push")
                    {
                        self.lower_list_push(args)?;
                        if !matches!(args.first(), Some(Expr::Var(receiver)) if receiver == name) {
                            return Err(CanonicalLoweringError::UnsupportedExpression(
                                "list.push assignment target",
                            ));
                        }
                        continue;
                    }
                    let value = self.lower_expr(value)?;
                    self.bindings.insert(name.clone(), value);
                }
                Stmt::Return(value) => {
                    if !final_statement {
                        return Err(CanonicalLoweringError::UnsupportedStatement("early return"));
                    }
                    if let Some(call @ Expr::Call { name, args }) = value {
                        if matches!(name.as_str(), "list.length" | "list.__length") {
                            let value = self.lower_list_length(args)?;
                            let terminator = self.return_value(Some(value), results)?;
                            self.finish_current(terminator)?;
                            return Ok(());
                        }
                        if matches!(name.as_str(), "string.chars" | "string.__chars") {
                            let value = self.lower_string_chars(args)?;
                            let terminator = self.return_value(Some(value), results)?;
                            self.finish_current(terminator)?;
                            return Ok(());
                        }
                        if is_console_print(name) {
                            if !results.is_empty() {
                                return Err(CanonicalLoweringError::UnsupportedExpression(
                                    "console.print in value-returning function",
                                ));
                            }
                            self.lower_console_print(args)?;
                            self.finish_current(Terminator::Return { values: Vec::new() })?;
                            return Ok(());
                        }
                        // A call by a name that is neither a binding nor a
                        // canonical function is an intrinsic the expression
                        // lowering handles (`int_to_duration`, `math.sqrt`):
                        // its value is returned, not tail-called.
                        if !self.bindings.contains_key(name) && !self.functions.contains_key(name) {
                            let value = self.lower_expr(call)?;
                            let terminator = self.return_value(Some(value), results)?;
                            self.finish_current(terminator)?;
                            return Ok(());
                        }
                        if !self.bindings.contains_key(name) {
                            let terminator = self.lower_direct_tail_call(
                                name,
                                args,
                                                                self.tail_path(index, ProperTailPathStep::ReturnValue)?,
                            )?;
                            self.finish_current(terminator)?;
                        } else {
                            let callee = self.bindings[name];
                            let terminator = self.lower_indirect_tail_call(
                                callee,
                                args,
                                                                self.tail_path(index, ProperTailPathStep::ReturnValue)?,
                                ProperTailExprKind::Call,
                                ProperTailDispatchKind::Direct,
                            )?;
                            self.finish_current(terminator)?;
                        }
                        return Ok(());
                    }
                    if let Some(Expr::Apply { func, args }) = value {
                        let callee = self.lower_expr(func)?;
                        let terminator = self.lower_indirect_tail_call(
                            callee,
                            args,
                                                        self.tail_path(index, ProperTailPathStep::ReturnValue)?,
                            ProperTailExprKind::Apply,
                            ProperTailDispatchKind::Indirect,
                        )?;
                        self.finish_current(terminator)?;
                        return Ok(());
                    }
                    if let Some(value) = value
                        && matches!(value, Expr::If { .. } | Expr::Match { .. } | Expr::Block(_))
                        && self.has_nested_tail_sites(&self.tail_path(index, ProperTailPathStep::ReturnValue)?)
                    {
                        return self.lower_tail_expr(value, self.tail_path(index, ProperTailPathStep::ReturnValue)?);
                    }
                    let value = value.as_ref().map(|value| self.lower_expr(value)).transpose()?;
                    let terminator = self.return_value(value, results)?;
                    self.finish_current(terminator)?;
                    return Ok(());
                }
                Stmt::Expr(value) if final_statement => {
                    if let call @ Expr::Call { name, args } = value {
                        if matches!(name.as_str(), "list.length" | "list.__length") {
                            let value = self.lower_list_length(args)?;
                            let terminator = self.return_value(Some(value), results)?;
                            self.finish_current(terminator)?;
                            return Ok(());
                        }
                        if matches!(name.as_str(), "string.chars" | "string.__chars") {
                            let value = self.lower_string_chars(args)?;
                            let terminator = self.return_value(Some(value), results)?;
                            self.finish_current(terminator)?;
                            return Ok(());
                        }
                        if is_console_print(name) {
                            if !results.is_empty() {
                                return Err(CanonicalLoweringError::UnsupportedExpression(
                                    "console.print in value-returning function",
                                ));
                            }
                            self.lower_console_print(args)?;
                            self.finish_current(Terminator::Return { values: Vec::new() })?;
                            return Ok(());
                        }
                        // A call by a name that is neither a binding nor a
                        // canonical function is an intrinsic the expression
                        // lowering handles (`int_to_duration`, `math.sqrt`):
                        // its value is returned, not tail-called.
                        if !self.bindings.contains_key(name) && !self.functions.contains_key(name) {
                            let value = self.lower_expr(call)?;
                            let terminator = self.return_value(Some(value), results)?;
                            self.finish_current(terminator)?;
                            return Ok(());
                        }
                        if !self.bindings.contains_key(name) {
                            let terminator = self.lower_direct_tail_call(
                                name,
                                args,
                                                                self.tail_path(index, ProperTailPathStep::Value)?,
                            )?;
                            self.finish_current(terminator)?;
                        } else {
                            let callee = self.bindings[name];
                            let terminator = self.lower_indirect_tail_call(
                                callee,
                                args,
                                                                self.tail_path(index, ProperTailPathStep::Value)?,
                                ProperTailExprKind::Call,
                                ProperTailDispatchKind::Direct,
                            )?;
                            self.finish_current(terminator)?;
                        }
                        return Ok(());
                    }
                    if let Expr::Apply { func, args } = value {
                        let callee = self.lower_expr(func)?;
                        let terminator = self.lower_indirect_tail_call(
                            callee,
                            args,
                                                        self.tail_path(index, ProperTailPathStep::Value)?,
                            ProperTailExprKind::Apply,
                            ProperTailDispatchKind::Indirect,
                        )?;
                        self.finish_current(terminator)?;
                        return Ok(());
                    }
                    // A unit function's final expression is an effect, whatever
                    // its shape: an `if` without `else`, an `if`/`match` whose
                    // arms print, a loop, a block. Lowering it as a value would
                    // refuse `if` without `else` and `console.print` in arms
                    // (census B2/B3).
                    // A final `if`/`match`/block whose arms hold manifest
                    // tail sites is lowered in tail context: each arm exits
                    // the function, with proper tail calls at the sites.
                    if matches!(value, Expr::If { .. } | Expr::Match { .. } | Expr::Block(_))
                        && self.has_nested_tail_sites(&self.tail_path(index, ProperTailPathStep::Value)?)
                    {
                        return self.lower_tail_expr(value, self.tail_path(index, ProperTailPathStep::Value)?);
                    }
                    if results.is_empty() {
                        self.lower_effect_expr(value)?;
                        self.finish_current(Terminator::Return { values: Vec::new() })?;
                        return Ok(());
                    }
                    let value = self.lower_expr(value)?;
                    let terminator = self.return_value(Some(value), results)?;
                    self.finish_current(terminator)?;
                    return Ok(());
                }
                Stmt::Expr(value) => self.lower_effect_expr(value)?,
                Stmt::LetPattern { pattern: Pattern::Wildcard, value } => {
                    // `let _ = e` keeps `e`'s effects and discards its value.
                    self.lower_effect_expr(value)?;
                }
                Stmt::LetPattern { pattern, value } => {
                    self.lower_let_pattern(pattern, value)?;
                }
                Stmt::Break => {
                    self.lower_break()?;
                    return Ok(());
                }
                Stmt::Continue => {
                    self.lower_continue()?;
                    return Ok(());
                }
                Stmt::Yield(_) => return Err(CanonicalLoweringError::UnsupportedStatement("yield")),
            }
        }
        if results.is_empty() {
            self.finish_current(Terminator::Return { values: Vec::new() })
        } else {
            Err(CanonicalLoweringError::MissingReturn(self.function.clone()))
        }
    }

    fn lower_effect_expr(&mut self, expression: &Expr) -> Result<(), CanonicalLoweringError> {
        match expression {
            Expr::While { cond, body } => self.lower_while(cond, body),
            Expr::WhileLet { pattern, scrutinee, body } => self.lower_while_let(pattern, scrutinee, body),
            Expr::For { var, iter, body } => self.lower_for(var, iter, body),
            Expr::Block(block) => self.lower_block_effect(block),
            Expr::Call { name, args } if is_console_print(name) => self.lower_console_print(args),
            // `fail(msg)` aborts on every backend: the message is evaluated
            // and the block traps. It must be the block's last statement.
            Expr::Call { name, args } if name == "fail" => {
                for argument in args {
                    let _ = self.lower_expr(argument)?;
                }
                self.finish_current(Terminator::Trap)
            }
            Expr::Call { name, args } if matches!(name.as_str(), "list.push" | "list.__push") => {
                self.lower_list_push(args)
            }
            // `Nil` as a statement or a unit body's last expression does
            // nothing.
            Expr::Ctor { name, args } if name == "Nil" && args.is_empty() => Ok(()),
            Expr::Var(name) if name == "Nil" && !self.bindings.contains_key(name) => Ok(()),
            // A call by a bound name is a call through a closure record.
            Expr::Call { name, args } if self.bindings.contains_key(name) => {
                let callee = self.bindings[name];
                let ValueType::Ref(layout) = self.values[&callee] else {
                    return Err(CanonicalLoweringError::IndirectCalleeType(self.values[&callee]));
                };
                self.lower_closure_call(callee, layout, args)?;
                Ok(())
            }
            Expr::Call { name, args } => {
                self.lower_direct_call(name, args)?;
                Ok(())
            }
            Expr::If {
                cond,
                then_block,
                else_block: Some(else_block),
            } => self.lower_if(cond, then_block, else_block, false).map(|_| ()),
            Expr::If { cond, then_block, else_block: None } => {
                let empty = witchy_syntax::ast::Block {
                    stmts: Vec::new(),
                    lines: Vec::new(),
                    region: None,
                };
                self.lower_if(cond, then_block, &empty, false).map(|_| ())
            }
            Expr::Match { scrutinee, arms } => {
                self.lower_match(scrutinee, arms, false, None).map(|_| ())
            }
            _ => {
                self.lower_expr(expression)?;
                Ok(())
            }
        }
    }

    fn lower_block_effect(
        &mut self,
        block: &witchy_syntax::ast::Block,
    ) -> Result<(), CanonicalLoweringError> {
        let outer = self.bindings.keys().cloned().collect::<Vec<_>>();
        for statement in &block.stmts {
            self.require_open_block()?;
            match statement {
                Stmt::Let { name, value, .. } if name == "_" => {
                    // `let _ = e` keeps `e`'s effects and discards its value.
                    self.lower_effect_expr(value)?;
                }
                Stmt::Let { name, mutable, value, .. } => {
                    let value = self.lower_let_value(*mutable, value)?;
                    if self.bindings.insert(name.clone(), value).is_some() {
                        return Err(CanonicalLoweringError::DuplicateBinding(name.clone()));
                    }
                }
                Stmt::Assign { name, value } => {
                    if !self.bindings.contains_key(name) {
                        return Err(CanonicalLoweringError::MissingBinding(name.clone()));
                    }
                    if let Expr::Call { name: call, args } = value
                        && matches!(call.as_str(), "list.push" | "list.__push")
                    {
                        self.lower_list_push(args)?;
                        if !matches!(args.first(), Some(Expr::Var(receiver)) if receiver == name) {
                            return Err(CanonicalLoweringError::UnsupportedExpression(
                                "list.push assignment target",
                            ));
                        }
                        continue;
                    }
                    let value = self.lower_expr(value)?;
                    self.bindings.insert(name.clone(), value);
                }
                Stmt::Expr(value) => self.lower_effect_expr(value)?,
                Stmt::Return(value) => {
                    // An early `return` ends this block's control flow. It is
                    // admitted only as the block's last statement; anything
                    // after it would be unreachable.
                    if !std::ptr::eq(statement, block.stmts.last().expect("iterating a statement")) {
                        return Err(CanonicalLoweringError::UnsupportedStatement("statements after return"));
                    }
                    let value = value.as_ref().map(|value| self.lower_expr(value)).transpose()?;
                    let results = self.results.clone();
                    let terminator = self.return_value(value, &results)?;
                    self.finish_current(terminator)?;
                    self.drop_bindings_except(&outer);
                    return Ok(());
                }
                Stmt::LetPattern { pattern: Pattern::Wildcard, value } => {
                    // `let _ = e` keeps `e`'s effects and discards its value.
                    self.lower_effect_expr(value)?;
                }
                Stmt::LetPattern { pattern, value } => {
                    self.lower_let_pattern(pattern, value)?;
                }
                Stmt::Break => {
                    self.lower_break()?;
                    return Ok(());
                }
                Stmt::Continue => {
                    self.lower_continue()?;
                    return Ok(());
                }
                Stmt::Yield(_) => return Err(CanonicalLoweringError::UnsupportedStatement("yield")),
            }
        }
        self.drop_bindings_except(&outer);
        Ok(())
    }

    fn lower_block_value(
        &mut self,
        block: &witchy_syntax::ast::Block,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let outer = self.bindings.keys().cloned().collect::<Vec<_>>();
        let (tail, statements) = block
            .stmts
            .split_last()
            .ok_or(CanonicalLoweringError::UnsupportedExpression("empty block"))?;
        for statement in statements {
            self.require_open_block()?;
            match statement {
                Stmt::Let { name, value, .. } if name == "_" => {
                    // `let _ = e` keeps `e`'s effects and discards its value.
                    self.lower_effect_expr(value)?;
                }
                Stmt::Let { name, mutable, value, .. } => {
                    let value = self.lower_let_value(*mutable, value)?;
                    if self.bindings.insert(name.clone(), value).is_some() {
                        return Err(CanonicalLoweringError::DuplicateBinding(name.clone()));
                    }
                }
                Stmt::Assign { name, value } => {
                    if !self.bindings.contains_key(name) {
                        return Err(CanonicalLoweringError::MissingBinding(name.clone()));
                    }
                    if let Expr::Call { name: call, args } = value
                        && matches!(call.as_str(), "list.push" | "list.__push")
                    {
                        self.lower_list_push(args)?;
                        if !matches!(args.first(), Some(Expr::Var(receiver)) if receiver == name) {
                            return Err(CanonicalLoweringError::UnsupportedExpression(
                                "list.push assignment target",
                            ));
                        }
                        continue;
                    }
                    let value = self.lower_expr(value)?;
                    self.bindings.insert(name.clone(), value);
                }
                Stmt::Expr(value) => self.lower_effect_expr(value)?,
                _ => return Err(CanonicalLoweringError::UnsupportedStatement("block statement")),
            }
        }
        let value = match tail {
            Stmt::Expr(value) => self.lower_expr(value)?,
            _ => return Err(CanonicalLoweringError::UnsupportedStatement("block tail")),
        };
        self.drop_bindings_except(&outer);
        Ok(value)
    }

    /// The value a `let`/`var` binds. A `var` that aliases a reference another
    /// binding still holds (`var next = placed`) needs its own token, or the
    /// copy-on-write path would drop the other binding's. A static string
    /// materializes an owned copy instead of cloning static data.
    /// `let (a, b) = value`: an irrefutable pattern binds each named slot of
    /// the value (a shared clone for a reference slot); the value itself is
    /// a temporary released by placement.
    fn lower_let_pattern(&mut self, pattern: &Pattern, value: &Expr) -> Result<(), CanonicalLoweringError> {
        let value_type = self.expression_type(value)?;
        let lowered = self.lower_expr(value)?;
        for (name, bound) in self.lower_match_pattern(lowered, pattern, &value_type)? {
            if self.bindings.insert(name.clone(), bound).is_some() {
                return Err(CanonicalLoweringError::DuplicateBinding(name));
            }
        }
        Ok(())
    }

    fn lower_let_value(&mut self, mutable: bool, value: &Expr) -> Result<ValueId, CanonicalLoweringError> {
        let value = self.lower_expr(value)?;
        if !mutable || !matches!(self.values[&value], ValueType::Ref(_)) {
            return Ok(value);
        }
        // A `var` initialised from a static string (`var out = ""`) owns a
        // copy from the start: it will be rebound to owned values (a loop
        // accumulator), and every edge into a loop header must carry the
        // same token shape.
        if self.is_static_string(value) {
            return self.clone_string_for_storage(value);
        }
        if !self.bindings.values().any(|bound| *bound == value) {
            return Ok(value);
        }
        self.clone_reference_token(value)
    }

    /// A second token for a reference: a fresh copy of a static string, or a
    /// `CloneRef` whose header is authenticated anew (the clone is a new
    /// object to the checker; it does not inherit the source's count facts).
    fn clone_reference_token(&mut self, value: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        if self.is_static_string(value) {
            return self.clone_string_for_storage(value);
        }
        let clone = self.fresh(self.values[&value])?;
        self.push_operation(Operation::CloneRef { destination: clone, source: value });
        // The clone shares the source's authenticated header facts (the
        // checker copies them with the object state), so its extent lanes are
        // the source's; a source without lanes yet is authenticated on the
        // clone.
        if let Some((count, capacity)) = self.tracked_lanes(value) {
            self.set_tracked_lanes(clone, count, capacity);
        } else {
            self.ensure_list_extent(clone)?;
            self.ensure_string_extent(clone)?;
        }
        Ok(clone)
    }

    /// The value an `if`/`match` arm contributes to its join. A reference that
    /// is also a named binding's token (`if done: placed else: ...`) would
    /// reach the join twice when that name is rebound in another arm, once as
    /// the result lane and once as the binding lane; a token cannot feed two
    /// lanes, so the result lane gets its own.
    fn detach_bound_result(&mut self, value: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        if !matches!(self.values[&value], ValueType::Ref(_))
            || !self.bindings.values().any(|bound| *bound == value)
        {
            return Ok(value);
        }
        self.clone_reference_token(value)
    }

    /// Whether the manifest marks a proper-tail site strictly inside the
    /// expression at `prefix` (an arm of an `if`/`match`, a block's last
    /// statement), as opposed to a call that is the expression itself.
    fn has_nested_tail_sites(&self, prefix: &[ProperTailPathStep]) -> bool {
        self.tail_manifest.sites().iter().any(|site| {
            site.key.callable.item == self.item
                && site.key.callable.method.is_none()
                && site.key.callable.lambda_path == self.lambda_path
                && site.key.path.starts_with(prefix)
                && site.key.path.len() > prefix.len() + 1
        })
    }

    fn has_tail_site(&self, path: &[ProperTailPathStep], node: ProperTailExprKind) -> bool {
        self.tail_manifest.sites().iter().any(|site| {
            site.key.callable.item == self.item
                && site.key.callable.method.is_none()
                && site.key.callable.lambda_path == self.lambda_path
                && site.key.path.len() == path.len() + 1
                && site.key.path.starts_with(path)
                && site.key.path.last() == Some(&ProperTailPathStep::Node(node))
        })
    }

    /// Lower an expression in tail position so that every leaf terminates the
    /// function: a manifest-marked call becomes a proper tail call, an
    /// `if`/`match`/block recurses into its arms along the manifest path, and
    /// anything else returns its value (or, in a unit function, its effect).
    fn lower_tail_expr(
        &mut self,
        expression: &Expr,
        path: Vec<ProperTailPathStep>,
    ) -> Result<(), CanonicalLoweringError> {
        let results = self.results.clone();
        match expression {
            Expr::If { cond, then_block, else_block } => {
                let condition = self.lower_expr(cond)?;
                if self.values[&condition] != ValueType::Bool {
                    return Err(CanonicalLoweringError::UnsupportedExpression("non-boolean condition"));
                }
                let outer = self.bindings.clone();
                let then_id = self.new_block(&[])?;
                let else_id = self.new_block(&[])?;
                self.finish_current(Terminator::BranchIf {
                    condition,
                    then_target: then_id,
                    then_arguments: Vec::new(),
                    else_target: else_id,
                    else_arguments: Vec::new(),
                })?;
                // Each arm ends in its own terminator, so no join merges
                // their facts; a load cached inside the then arm does not
                // dominate the else arm.
                let facts = self.snapshot_facts();
                self.current = then_id;
                self.bindings = outer.clone();
                let mut then_path = path.clone();
                then_path.extend([ProperTailPathStep::Node(ProperTailExprKind::If), ProperTailPathStep::Child(1)]);
                self.lower_tail_block(then_block, then_path)?;
                self.restore_snapshot(facts);
                self.current = else_id;
                self.bindings = outer;
                match else_block {
                    Some(else_block) => {
                        let mut else_path = path;
                        else_path.extend([ProperTailPathStep::Node(ProperTailExprKind::If), ProperTailPathStep::Child(2)]);
                        self.lower_tail_block(else_block, else_path)
                    }
                    None if results.is_empty() => self.finish_current(Terminator::Return { values: Vec::new() }),
                    None => Err(CanonicalLoweringError::UnsupportedExpression("valued if without else")),
                }
            }
            Expr::Match { scrutinee, arms } => {
                let mut match_path = path;
                match_path.push(ProperTailPathStep::Node(ProperTailExprKind::Match));
                self.lower_match(scrutinee, arms, !results.is_empty(), Some(match_path)).map(|_| ())
            }
            Expr::Block(block) => {
                let mut block_path = path;
                block_path.push(ProperTailPathStep::Node(ProperTailExprKind::Block));
                self.lower_tail_block(block, block_path)
            }
            Expr::Call { name, args }
                if !self.bindings.contains_key(name)
                    && self.functions.contains_key(name)
                    && self.has_tail_site(&path, ProperTailExprKind::Call) =>
            {
                let terminator = self.lower_direct_tail_call(name, args, path)?;
                self.finish_current(terminator)
            }
            Expr::Call { name, args }
                if self.bindings.contains_key(name) && self.has_tail_site(&path, ProperTailExprKind::Call) =>
            {
                let callee = self.bindings[name];
                let terminator = self.lower_indirect_tail_call(
                    callee, args, path, ProperTailExprKind::Call, ProperTailDispatchKind::Direct,
                )?;
                self.finish_current(terminator)
            }
            Expr::Apply { func, args } if self.has_tail_site(&path, ProperTailExprKind::Apply) => {
                let callee = self.lower_expr(func)?;
                let terminator = self.lower_indirect_tail_call(
                    callee, args, path, ProperTailExprKind::Apply, ProperTailDispatchKind::Indirect,
                )?;
                self.finish_current(terminator)
            }
            _ if results.is_empty() => {
                self.lower_effect_expr(expression)?;
                self.finish_current(Terminator::Return { values: Vec::new() })
            }
            _ => {
                let value = self.lower_expr(expression)?;
                let terminator = self.return_value(Some(value), &results)?;
                self.finish_current(terminator)
            }
        }
    }

    /// A block in tail position: its leading statements are ordinary, its
    /// last statement is the tail expression.
    fn lower_tail_block(
        &mut self,
        block: &witchy_syntax::ast::Block,
        mut path: Vec<ProperTailPathStep>,
    ) -> Result<(), CanonicalLoweringError> {
        let Some((last, leading)) = block.stmts.split_last() else {
            if !self.results.is_empty() {
                return Err(CanonicalLoweringError::UnsupportedExpression("empty valued block"));
            }
            return self.finish_current(Terminator::Return { values: Vec::new() });
        };
        for statement in leading {
            self.require_open_block()?;
            match statement {
                Stmt::Let { name, value, .. } if name == "_" => {
                    // `let _ = e` keeps `e`'s effects and discards its value.
                    self.lower_effect_expr(value)?;
                }
                Stmt::Let { name, mutable, value, .. } => {
                    let value = self.lower_let_value(*mutable, value)?;
                    if self.bindings.insert(name.clone(), value).is_some() {
                        return Err(CanonicalLoweringError::DuplicateBinding(name.clone()));
                    }
                }
                Stmt::Assign { name, value } => {
                    if !self.bindings.contains_key(name) {
                        return Err(CanonicalLoweringError::MissingBinding(name.clone()));
                    }
                    if let Expr::Call { name: call, args } = value
                        && matches!(call.as_str(), "list.push" | "list.__push")
                    {
                        self.lower_list_push(args)?;
                        if !matches!(args.first(), Some(Expr::Var(receiver)) if receiver == name) {
                            return Err(CanonicalLoweringError::UnsupportedExpression(
                                "list.push assignment target",
                            ));
                        }
                        continue;
                    }
                    let value = self.lower_expr(value)?;
                    self.bindings.insert(name.clone(), value);
                }
                Stmt::Expr(value) => self.lower_effect_expr(value)?,
                Stmt::LetPattern { pattern: Pattern::Wildcard, value } => self.lower_effect_expr(value)?,
                Stmt::LetPattern { pattern, value } => self.lower_let_pattern(pattern, value)?,
                _ => return Err(CanonicalLoweringError::UnsupportedStatement("tail block statement")),
            }
        }
        let index = u32::try_from(leading.len()).map_err(|_| CanonicalLoweringError::IdOverflow)?;
        match last {
            Stmt::Expr(value) => {
                path.extend([ProperTailPathStep::Statement(index), ProperTailPathStep::Value]);
                self.lower_tail_expr(value, path)
            }
            Stmt::Return(Some(value)) => {
                path.extend([ProperTailPathStep::Statement(index), ProperTailPathStep::ReturnValue]);
                self.lower_tail_expr(value, path)
            }
            Stmt::Return(None) if self.results.is_empty() => {
                self.finish_current(Terminator::Return { values: Vec::new() })
            }
            _ => Err(CanonicalLoweringError::UnsupportedStatement("tail block end")),
        }
    }

    /// A block that already terminated (a `fail`) accepts no further
    /// statement; the lowering refuses rather than emitting dead code.
    fn require_open_block(&self) -> Result<(), CanonicalLoweringError> {
        if self.blocks[self.current.get() as usize].terminator.is_some() {
            return Err(CanonicalLoweringError::UnsupportedStatement("statement after a terminating call"));
        }
        Ok(())
    }

    /// The join plan for one rebound reference binding: the value each arm
    /// contributes (an alias of another binding is cloned; a static lane is
    /// materialized when another lane carries a token), the extent lanes
    /// (distinct ids), and the phi the join parameter may take: unique when
    /// every arm holds its own token, shared when every arm is shared-phi
    /// eligible, none otherwise (the checker then demands one identity).
    fn plan_binding_lanes(
        &mut self,
        arms: &[(BlockId, &HashMap<String, ValueId>)],
        changed_names: &[String],
    ) -> Result<Vec<BindingLanePlan>, CanonicalLoweringError> {
        let mut plans = Vec::with_capacity(changed_names.len());
        for name in changed_names {
            let mut values = arms.iter().map(|(_, bindings)| bindings[name]).collect::<Vec<_>>();
            if !matches!(self.values[&values[0]], ValueType::Ref(_)) {
                plans.push(BindingLanePlan { name: name.clone(), values, extents: None, phi: None, all_tokens: false });
                continue;
            }
            for (index, (end, bindings)) in arms.iter().enumerate() {
                let value = values[index];
                if bindings.iter().any(|(other, bound)| other != name && *bound == value) {
                    self.current = *end;
                    values[index] = self.clone_reference_token(value)?;
                }
            }
            let mut lanes = arms.iter().map(|(end, _)| *end).zip(values.iter().copied()).collect::<Vec<_>>();
            let all_tokens = self.materialize_mixed_lanes(&mut lanes)?;
            values = lanes.iter().map(|(_, value)| *value).collect();
            let eligible = values.iter().all(|value| self.shared_phi_eligible(*value));
            // A fixed layout (a heap record) has no count or capacity: its phi
            // carries no lanes. A collection's phi carries both lanes from
            // every arm or none at all.
            let fixed_layout = values.iter().all(|value| {
                matches!(self.values[value], ValueType::Ref(layout)
                    if matches!(self.layouts.get(layout).map(|descriptor| descriptor.size()), Some(LayoutSize::Fixed(_))))
            });
            let mut extents = Vec::with_capacity(values.len());
            for ((end, _), value) in arms.iter().zip(&values) {
                if fixed_layout {
                    break;
                }
                // An arm that never touched the value has not authenticated
                // its extent; load it at the arm's end so the join carries
                // both lanes. That load is the arm's own fact: it must not
                // stay cached for code the arm does not dominate.
                self.current = *end;
                let authenticated = self.dynamic_extent_lanes(*value).is_some();
                if !authenticated {
                    self.ensure_list_extent(*value)?;
                    self.ensure_string_extent(*value)?;
                }
                let lanes = self.dynamic_extent_lanes(*value);
                if !authenticated {
                    self.string_lengths.remove(value);
                    self.string_capacities.remove(value);
                    self.list_counts.remove(value);
                    self.list_capacities.remove(value);
                }
                let Some((count, capacity)) = lanes else {
                    extents.clear();
                    break;
                };
                extents.push(self.split_lanes(count, capacity)?);
            }
            let phi = if eligible && (fixed_layout || extents.len() == values.len()) {
                if all_tokens && values.iter().all(|value| !self.known_shared_value(*value)) {
                    Some(Ownership::Unique)
                } else {
                    Some(Ownership::Shared)
                }
            } else {
                None
            };
            let extents = phi.is_some().then_some(extents);
            plans.push(BindingLanePlan { name: name.clone(), values, extents, phi, all_tokens });
        }
        Ok(plans)
    }

    fn drop_bindings_except(&mut self, names: &[String]) {
        self.bindings.retain(|name, _| names.iter().any(|outer| outer == name));
    }

    fn lower_if(
        &mut self,
        condition: &Expr,
        then_block: &witchy_syntax::ast::Block,
        else_block: &witchy_syntax::ast::Block,
        with_value: bool,
    ) -> Result<Option<ValueId>, CanonicalLoweringError> {
        let condition = self.lower_expr(condition)?;
        if self.values[&condition] != ValueType::Bool {
            return Err(CanonicalLoweringError::UnsupportedExpression("non-boolean condition"));
        }
        let facts = self.snapshot_facts();
        let outer = self.bindings.clone();
        let then_id = self.new_block(&[])?;
        let else_id = self.new_block(&[])?;
        self.finish_current(Terminator::BranchIf {
            condition,
            then_target: then_id,
            then_arguments: Vec::new(),
            else_target: else_id,
            else_arguments: Vec::new(),
        })?;

        self.current = then_id;
        self.bindings = outer.clone();
        let then_value = if with_value {
            let value = self.lower_block_value(then_block)?;
            Some(self.detach_bound_result(value)?)
        } else {
            self.lower_block_effect(then_block)?;
            None
        };
        let then_bindings = self.bindings.clone();
        let then_end = self.current;
        let then_terminated = self.blocks[then_end.get() as usize].terminator.is_some();
        // The else arm starts from the facts before the `if`; the then arm's
        // are merged back for the join's lane planning.
        let then_facts = self.snapshot_facts();
        self.restore_snapshot(facts.clone());

        self.current = else_id;
        self.bindings = outer.clone();
        let else_value = if with_value {
            let value = self.lower_block_value(else_block)?;
            Some(self.detach_bound_result(value)?)
        } else {
            self.lower_block_effect(else_block)?;
            None
        };
        let else_bindings = self.bindings.clone();
        let else_end = self.current;
        let else_terminated = self.blocks[else_end.get() as usize].terminator.is_some();
        let else_facts = self.snapshot_facts();
        self.absorb_facts(then_facts.clone());

        // An arm that returned early has no edge into the join. With one arm
        // gone the other simply continues as the fall-through; the `if` is a
        // statement here, so it carries no value.
        if then_terminated || else_terminated {
            if with_value {
                return Err(CanonicalLoweringError::UnsupportedExpression("return inside a valued if"));
            }
            if then_terminated && else_terminated {
                return Err(CanonicalLoweringError::UnsupportedStatement("return in every if arm"));
            }
            // Only the continuing arm's facts remain valid: a terminated
            // arm's cached loads never dominate what follows.
            let (end, bindings, arm_facts) = if then_terminated {
                (else_end, else_bindings, else_facts)
            } else {
                (then_end, then_bindings, then_facts)
            };
            self.restore_snapshot(facts);
            self.absorb_facts(arm_facts);
            self.current = end;
            self.bindings = bindings;
            return Ok(None);
        }

        // A static lane joined with a token lane: materialize the static one
        // so both edges carry a token (see `materialize_mixed_lanes`).
        let mut all_tokens = false;
        let (then_value, else_value) = match (then_value, else_value) {
            (Some(then), Some(other)) => {
                let mut lanes = [(then_end, then), (else_end, other)];
                all_tokens = self.materialize_mixed_lanes(&mut lanes)?;
                (Some(lanes[0].1), Some(lanes[1].1))
            }
            values => values,
        };
        let result_type = match (then_value, else_value) {
            (Some(then_value), Some(else_value)) => {
                let then_type = self.values[&then_value];
                let else_type = self.values[&else_value];
                if then_type != else_type {
                    return Err(CanonicalLoweringError::UnsupportedExpression("if branch types"));
                }
                Some((then_value, else_value, then_type))
            }
            (None, None) => None,
            _ => unreachable!(),
        };
        let shared_result_extents = result_type.and_then(|(then_value, else_value, result_type)| {
            (matches!(result_type, ValueType::Ref(_))
                && self.shared_phi_eligible(then_value)
                && self.shared_phi_eligible(else_value))
            .then(|| {
                Some((
                    self.dynamic_extent_lanes(then_value)?,
                    self.dynamic_extent_lanes(else_value)?,
                ))
            })
            .flatten()
        });
        // A fixed-layout result (a heap record or sum) joins through a phi
        // without count/capacity lanes: unique when both arms hold their own
        // fresh token, shared otherwise.
        let fixed_result_phi = result_type.and_then(|(then_value, else_value, result_type)| {
            let ValueType::Ref(layout) = result_type else { return None };
            let fixed = matches!(self.layouts.get(layout).map(|descriptor| descriptor.size()), Some(LayoutSize::Fixed(_)));
            (fixed && self.shared_phi_eligible(then_value) && self.shared_phi_eligible(else_value)).then(|| {
                if all_tokens && !self.known_shared_value(then_value) && !self.known_shared_value(else_value) {
                    Ownership::Unique
                } else {
                    Ownership::Shared
                }
            })
        });
        let mut names = outer.keys().cloned().collect::<Vec<_>>();
        names.sort();
        let changed_names = names
            .iter()
            .filter(|name| then_bindings[*name] != else_bindings[*name])
            .cloned()
            .collect::<Vec<_>>();
        let plans = self.plan_binding_lanes(&[(then_end, &then_bindings), (else_end, &else_bindings)], &changed_names)?;
        let mut parameter_types = Vec::new();
        for plan in &plans {
            parameter_types.push(self.values[&plan.values[0]]);
            if plan.has_lanes() {
                parameter_types.extend([ValueType::Size, ValueType::Size]);
            }
        }
        let binding_parameters = parameter_types.len();
        if let Some((_, _, result_type)) = result_type {
            parameter_types.push(result_type);
            if shared_result_extents.is_some() {
                parameter_types.extend([ValueType::Size, ValueType::Size]);
            }
        }
        let join_id = self.new_block(&parameter_types)?;
        if shared_result_extents.is_some() {
            self.blocks[join_id.get() as usize].parameters[binding_parameters].ownership =
                Some(Ownership::Shared);
        } else if let Some(phi) = fixed_result_phi {
            self.blocks[join_id.get() as usize].parameters[binding_parameters].ownership = Some(phi);
        }
        let join_parameters = self.blocks[join_id.get() as usize].parameters.clone();
        let mut then_arguments = Vec::with_capacity(parameter_types.len());
        let mut else_arguments = Vec::with_capacity(parameter_types.len());
        let mut parameter_index = 0;
        let mut merged = outer;
        let mut rebound_references = Vec::new();
        for plan in &plans {
            then_arguments.push(plan.values[0]);
            else_arguments.push(plan.values[1]);
            let parameter = join_parameters[parameter_index].value;
            if let (Some(phi), Some(extents)) = (plan.phi, &plan.extents) {
                self.blocks[join_id.get() as usize].parameters[parameter_index].ownership = Some(phi);
                if plan.has_lanes() {
                    then_arguments.extend([extents[0].0, extents[0].1]);
                    else_arguments.extend([extents[1].0, extents[1].1]);
                    let count = join_parameters[parameter_index + 1].value;
                    let capacity = join_parameters[parameter_index + 2].value;
                    self.record_joined_extent(parameter, count, capacity, &plan.values);
                    parameter_index += 2;
                }
                if plan.all_tokens {
                    self.token_join_parameters.insert(parameter);
                }
            } else if matches!(self.values[&parameter], ValueType::Ref(_)) {
                rebound_references.push(parameter);
            }
            merged.insert(plan.name.clone(), parameter);
            parameter_index += 1;
        }
        let result = match result_type {
            Some((then_value, else_value, _)) => {
                then_arguments.push(then_value);
                else_arguments.push(else_value);
                let result = join_parameters[parameter_index].value;
                if all_tokens {
                    self.token_join_parameters.insert(result);
                }
                if let Some(((then_count, then_capacity), (else_count, else_capacity))) =
                    shared_result_extents
                {
                    self.current = then_end;
                    let (then_count, then_capacity) = self.split_lanes(then_count, then_capacity)?;
                    then_arguments.extend([then_count, then_capacity]);
                    self.current = else_end;
                    let (else_count, else_capacity) = self.split_lanes(else_count, else_capacity)?;
                    else_arguments.extend([else_count, else_capacity]);
                    let count = join_parameters[parameter_index + 1].value;
                    let capacity = join_parameters[parameter_index + 2].value;
                    self.record_joined_extent(
                        result,
                        count,
                        capacity,
                        &[then_value, else_value],
                    );
                }
                Some(result)
            }
            None => None,
        };
        self.blocks[then_end.get() as usize].terminator = Some(Terminator::Branch {
            target: join_id,
            arguments: then_arguments,
        });
        self.blocks[else_end.get() as usize].terminator = Some(Terminator::Branch {
            target: join_id,
            arguments: else_arguments,
        });
        self.restore_facts(facts, join_id);
        self.current = join_id;
        // A rebound list or string arrives without extent facts; a loop
        // back edge or a later index needs them, so load the header once here.
        for parameter in rebound_references {
            self.ensure_list_extent(parameter)?;
            self.ensure_string_extent(parameter)?;
        }
        self.bindings = merged;
        Ok(result)
    }

    fn lower_match(
        &mut self,
        scrutinee: &Expr,
        arms: &[witchy_syntax::ast::MatchArm],
        with_value: bool,
        tail: Option<Vec<ProperTailPathStep>>,
    ) -> Result<Option<ValueId>, CanonicalLoweringError> {
        if arms.is_empty() {
            return Err(CanonicalLoweringError::UnsupportedExpression("empty match"));
        }
        let scrutinee_type = self.expression_type(scrutinee)?;
        let scrutinee = self.lower_expr(scrutinee)?;
        let facts = self.snapshot_facts();
        let outer = self.bindings.clone();
        let result_type = if with_value {
            let first = self.expression_type(&arms[0].body)?;
            let first = source_value_type(&first, self.layout_bundle, self.layouts, &HashMap::new(), self.definitions)?;
            if is_unit(first) {
                return Err(CanonicalLoweringError::UnsupportedExpression("unit match value"));
            }
            Some(first)
        } else {
            None
        };
        let mut arm_results = Vec::with_capacity(arms.len());
        // The dispatch block the next arm tests in. It is `None` once an
        // irrefutable arm has ended dispatch: allocating a fallthrough block
        // for an arm that cannot fail would leave a block no edge reaches, and
        // the structured backend refuses an unreachable block rather than
        // guessing it is dead (BUG-canonical-match-cfg-unstructured-block).
        let mut next = Some(self.current);

        let mut arm_facts = Vec::new();
        for (arm_index, arm) in arms.iter().enumerate() {
            let Some(current) = next else { break };
            // Each arm starts from the facts before the `match`; every arm's
            // facts are merged back for the join's lane planning.
            if arm_index > 0 {
                arm_facts.push(self.snapshot_facts());
                self.restore_snapshot(facts.clone());
            }
            self.current = current;
            self.bindings = outer.clone();
            let arm_block = self.new_block(&[])?;
            let condition = self.match_pattern_condition(scrutinee, &arm.pattern, &scrutinee_type)?;
            // Only an arm that can fail (a refutable pattern or a guard) has a
            // fallthrough successor.
            let next_block = if condition.is_some() || arm.guard.is_some() {
                Some(self.new_block(&[])?)
            } else {
                None
            };
            if let Some(condition) = condition {
                self.finish_current(Terminator::BranchIf {
                    condition,
                    then_target: arm_block,
                    then_arguments: Vec::new(),
                    else_target: next_block.expect("a refutable arm has a fallthrough"),
                    else_arguments: Vec::new(),
                })?;
            } else {
                self.finish_current(Terminator::Branch {
                    target: arm_block,
                    arguments: Vec::new(),
                })?;
            }

            self.current = arm_block;
            self.bindings = outer.clone();
            // Pattern bindings are emitted in the arm block: a list pattern's
            // element loads are guarded by the length test that reached it.
            for (name, value) in self.lower_match_pattern(scrutinee, &arm.pattern, &scrutinee_type)? {
                self.bindings.insert(name, value);
            }
            if let Some(guard) = &arm.guard {
                let guard = self.lower_expr(guard)?;
                if self.values[&guard] != ValueType::Bool {
                    return Err(CanonicalLoweringError::UnsupportedExpression("non-boolean match guard"));
                }
                // Replace the true edge with a dedicated body block. Keeping
                // the guard and body separate is necessary: the false edge
                // must not carry pattern bindings into the next arm.
                // The guard expression may itself have split blocks (a
                // short-circuit `&&`), so terminate the block it ended in,
                // not the arm block it started in.
                let body_block = self.new_block(&[])?;
                self.finish_current(Terminator::BranchIf {
                    condition: guard,
                    then_target: body_block,
                    then_arguments: Vec::new(),
                    else_target: next_block.expect("a guarded arm has a fallthrough"),
                    else_arguments: Vec::new(),
                })?;
                self.current = body_block;
            }
            if let Some(tail) = &tail {
                // Tail mode: every arm exits the function itself, so the
                // match has no join.
                let mut arm_path = tail.clone();
                arm_path.extend([
                    ProperTailPathStep::Arm(u32::try_from(arm_index).map_err(|_| CanonicalLoweringError::IdOverflow)?),
                    ProperTailPathStep::Value,
                ]);
                self.lower_tail_expr(&arm.body, arm_path)?;
                self.bindings = outer.clone();
                next = next_block;
                continue;
            }
            let value = if with_value {
                let value = self.lower_expr(&arm.body)?;
                let value = self.detach_bound_result(value)?;
                if Some(self.values[&value]) != result_type {
                    return Err(CanonicalLoweringError::UnsupportedExpression("match arm types"));
                }
                Some(value)
            } else {
                self.lower_effect_expr(&arm.body)?;
                None
            };
            let body_end = self.current;
            let bindings = self.bindings.clone();
            arm_results.push((body_end, bindings, value));
            self.bindings = outer.clone();
            next = next_block;
        }
        for previous in arm_facts {
            self.absorb_facts(previous);
        }

        // Exhaustiveness is the type checker's judgment; a dispatch that falls
        // off the last refutable arm is unreachable by that judgment and traps.
        self.bindings = outer.clone();
        if let Some(fallthrough) = next {
            self.current = fallthrough;
            self.finish_current(Terminator::Trap)?;
        }
        if tail.is_some() {
            self.bindings = outer;
            return Ok(None);
        }

        let mut changed_names = outer
            .keys()
            .filter(|name| arm_results.iter().any(|(_, bindings, _)| bindings[*name] != outer[*name]))
            .cloned()
            .collect::<Vec<_>>();
        changed_names.sort();
        let arms = arm_results.iter().map(|(body_end, bindings, _)| (*body_end, bindings)).collect::<Vec<_>>();
        let plans = self.plan_binding_lanes(&arms, &changed_names)?;
        let mut parameter_types = Vec::new();
        for plan in &plans {
            parameter_types.push(self.values[&plan.values[0]]);
            if plan.has_lanes() {
                parameter_types.extend([ValueType::Size, ValueType::Size]);
            }
        }
        let binding_parameters = parameter_types.len();
        if let Some(result_type) = result_type {
            parameter_types.push(result_type);
        }
        // A static lane joined with a token lane would hand the release
        // placer a token on one edge only; materialize the static lanes.
        let mut lanes = arm_results
            .iter()
            .filter_map(|(body_end, _, value)| value.map(|value| (*body_end, value)))
            .collect::<Vec<_>>();
        let all_tokens = self.materialize_mixed_lanes(&mut lanes)?;
        for ((_, _, value), (_, lane)) in arm_results.iter_mut().filter(|(_, _, value)| value.is_some()).zip(&lanes) {
            *value = Some(*lane);
        }
        let result_values = arm_results
            .iter()
            .filter_map(|(_, _, value)| *value)
            .collect::<Vec<_>>();
        let shared_result_extents = (result_type.is_some()
            && !result_values.is_empty()
            && result_values.iter().all(|value| self.shared_phi_eligible(*value)))
        .then(|| {
            result_values
                .iter()
                .map(|value| self.dynamic_extent_lanes(*value))
                .collect::<Option<Vec<_>>>()
        })
        .flatten();
        if shared_result_extents.is_some() {
            parameter_types.extend([ValueType::Size, ValueType::Size]);
        }
        let fixed_result_phi = result_type.and_then(|result_type| {
            let ValueType::Ref(layout) = result_type else { return None };
            let fixed = matches!(self.layouts.get(layout).map(|descriptor| descriptor.size()), Some(LayoutSize::Fixed(_)));
            (fixed && !result_values.is_empty() && result_values.iter().all(|value| self.shared_phi_eligible(*value))).then(|| {
                if all_tokens && result_values.iter().all(|value| !self.known_shared_value(*value)) {
                    Ownership::Unique
                } else {
                    Ownership::Shared
                }
            })
        });
        let join = self.new_block(&parameter_types)?;
        let result_parameter_index = binding_parameters;
        if shared_result_extents.is_some() {
            self.blocks[join.get() as usize].parameters[result_parameter_index].ownership =
                Some(Ownership::Shared);
        } else if let Some(phi) = fixed_result_phi {
            self.blocks[join.get() as usize].parameters[result_parameter_index].ownership = Some(phi);
            if phi == Ownership::Unique {
                let result = self.blocks[join.get() as usize].parameters[result_parameter_index].value;
                self.token_join_parameters.insert(result);
            }
        }
        let join_parameters = self.blocks[join.get() as usize].parameters.clone();
        let mut merged = outer;
        let result = result_type.map(|_| join_parameters[result_parameter_index].value);
        if all_tokens && let Some(result) = result {
            self.token_join_parameters.insert(result);
        }
        let mut parameter_index = 0;
        for plan in &plans {
            let parameter = join_parameters[parameter_index].value;
            if let (Some(phi), Some(_)) = (plan.phi, &plan.extents) {
                self.blocks[join.get() as usize].parameters[parameter_index].ownership = Some(phi);
                if plan.has_lanes() {
                    let count = join_parameters[parameter_index + 1].value;
                    let capacity = join_parameters[parameter_index + 2].value;
                    self.record_joined_extent(parameter, count, capacity, &plan.values);
                    parameter_index += 2;
                }
                if plan.all_tokens {
                    self.token_join_parameters.insert(parameter);
                }
            }
            merged.insert(plan.name.clone(), parameter);
            parameter_index += 1;
        }
        for (arm_index, (body_end, _bindings, value)) in arm_results.into_iter().enumerate() {
            let mut arguments = Vec::new();
            for plan in &plans {
                arguments.push(plan.values[arm_index]);
                if let (true, Some(extents)) = (plan.has_lanes(), &plan.extents) {
                    arguments.extend([extents[arm_index].0, extents[arm_index].1]);
                }
            }
            if let Some(value) = value {
                arguments.push(value);
                if let Some(extents) = &shared_result_extents {
                    self.current = body_end;
                    let (count, capacity) = self.split_lanes(extents[arm_index].0, extents[arm_index].1)?;
                    arguments.extend([count, capacity]);
                }
            }
            self.blocks[body_end.get() as usize].terminator = Some(Terminator::Branch {
                target: join,
                arguments,
            });
        }
        if shared_result_extents.is_some() {
            let result = result.expect("a shared match join has a result parameter");
            self.record_joined_extent(
                result,
                join_parameters[result_parameter_index + 1].value,
                join_parameters[result_parameter_index + 2].value,
                &result_values,
            );
        }
        self.restore_facts(facts, join);
        self.current = join;
        self.bindings = merged;
        Ok(result)
    }

    fn lower_match_pattern(
        &mut self,
        scrutinee: ValueId,
        pattern: &Pattern,
        scrutinee_type: &Type,
    ) -> Result<Vec<(String, ValueId)>, CanonicalLoweringError> {
        match pattern {
            Pattern::Var(name) if name != "_" => Ok(vec![(name.clone(), scrutinee)]),
            Pattern::Tuple(args) => {
                let shape = scalar_aggregate_shape(scrutinee_type, self.definitions).ok_or(
                    CanonicalLoweringError::UnsupportedExpression("tuple pattern scrutinee"),
                )?;
                if args.len() != shape.fields.len() {
                    return Err(CanonicalLoweringError::UnsupportedExpression("tuple pattern arity"));
                }
                let mut bindings = Vec::new();
                for (slot, argument) in args.iter().enumerate() {
                    match argument {
                        Pattern::Var(bound) if bound != "_" => {
                            let value_type = self.field_value_type(&shape.fields[slot])?;
                            let slot = u32::try_from(slot).map_err(|_| CanonicalLoweringError::IdOverflow)?;
                            bindings.push((bound.clone(), self.load_record_field(scrutinee, slot, value_type)?));
                        }
                        Pattern::Wildcard | Pattern::Var(_) => {}
                        _ => return Err(CanonicalLoweringError::UnsupportedExpression("nested tuple pattern")),
                    }
                }
                Ok(bindings)
            }
            Pattern::Ctor { name, args } => {
                // Each named payload is a slot load in the arm the tag test
                // reached; a nested pattern beyond a name or wildcard is not
                // in the subset.
                let shape = scalar_aggregate_shape(scrutinee_type, self.definitions).ok_or(
                    CanonicalLoweringError::UnsupportedExpression("constructor pattern scrutinee"),
                )?;
                let (_, variant) = shape.variant(name).ok_or_else(|| CanonicalLoweringError::MissingBinding(name.clone()))?;
                if args.len() != variant.arity {
                    return Err(CanonicalLoweringError::UnsupportedExpression("constructor pattern arity"));
                }
                let variant = variant.clone();
                let mut bindings = Vec::new();
                for (offset, argument) in args.iter().enumerate() {
                    match argument {
                        Pattern::Var(bound) if bound != "_" => {
                            let slot = variant.first_field + offset;
                            let value_type = self.field_value_type(&shape.fields[slot])?;
                            let slot = u32::try_from(slot).map_err(|_| CanonicalLoweringError::IdOverflow)?;
                            bindings.push((bound.clone(), self.load_record_field(scrutinee, slot, value_type)?));
                        }
                        Pattern::Wildcard | Pattern::Var(_) => {}
                        // A tuple payload destructures through its own slot
                        // load; only names and wildcards inside it.
                        Pattern::Tuple(_) => {
                            let slot = variant.first_field + offset;
                            let field_type = shape.fields[slot].clone();
                            let value_type = self.field_value_type(&field_type)?;
                            let slot = u32::try_from(slot).map_err(|_| CanonicalLoweringError::IdOverflow)?;
                            let payload = self.load_record_field(scrutinee, slot, value_type)?;
                            bindings.extend(self.lower_match_pattern(payload, argument, &field_type)?);
                        }
                        // A nested constructor payload binds through its own
                        // slot load; the tag test that reached this arm has
                        // already selected its variant.
                        Pattern::Ctor { .. } => {
                            let slot = variant.first_field + offset;
                            let field_type = shape.fields[slot].clone();
                            let value_type = self.field_value_type(&field_type)?;
                            let slot = u32::try_from(slot).map_err(|_| CanonicalLoweringError::IdOverflow)?;
                            let payload = self.load_record_field(scrutinee, slot, value_type)?;
                            bindings.extend(self.lower_match_pattern(payload, argument, &field_type)?);
                        }
                        // A literal payload binds nothing; its test ran in the
                        // dispatch block.
                        Pattern::Int(_) | Pattern::Bool(_) | Pattern::Str(_) | Pattern::Duration(_) => {}
                        _ => return Err(CanonicalLoweringError::UnsupportedExpression("nested constructor pattern")),
                    }
                }
                Ok(bindings)
            }
            Pattern::Wildcard
            | Pattern::Var(_)
            | Pattern::Int(_)
            | Pattern::Duration(_)
            | Pattern::Bool(_)
            | Pattern::Str(_) => Ok(Vec::new()),
            Pattern::List { elems, rest } => self.lower_list_pattern_bindings(scrutinee, elems, rest.as_ref()),
            _ => Err(CanonicalLoweringError::UnsupportedExpression("match pattern")),
        }
    }

    /// Bindings of a list pattern, emitted in the arm the length test
    /// reached: each named element is a guarded scalar load, and `..rest`
    /// is a fresh list holding a copy of the remaining elements (value
    /// semantics; the source is not consumed).
    fn lower_list_pattern_bindings(
        &mut self,
        scrutinee: ValueId,
        elems: &[Pattern],
        rest: Option<&Option<String>>,
    ) -> Result<Vec<(String, ValueId)>, CanonicalLoweringError> {
        let ValueType::Ref(layout) = self.values[&scrutinee] else {
            return Err(CanonicalLoweringError::UnsupportedExpression("list pattern scrutinee"));
        };
        let descriptor = self.layouts.get(layout).ok_or(CanonicalLoweringError::UnsupportedExpression("list pattern layout"))?;
        let LayoutKind::PackedList { element, .. } = descriptor.kind() else {
            return Err(CanonicalLoweringError::UnsupportedExpression("list pattern scrutinee"));
        };
        let element = *element;
        let element_type = match self.layouts.get(element).map(|descriptor| descriptor.kind()) {
            Some(LayoutKind::Scalar(ScalarKind::Int)) => ValueType::Int,
            Some(LayoutKind::Scalar(ScalarKind::Float)) => ValueType::Float,
            Some(LayoutKind::Scalar(ScalarKind::Bool)) => ValueType::Bool,
            _ => return Err(CanonicalLoweringError::UnsupportedExpression("list pattern over reference elements")),
        };
        let (header_bytes, stride_bytes) = match (descriptor.header(), descriptor.size()) {
            (HeaderLayout::PackedList { data_offset, .. }, LayoutSize::Dynamic { stride, .. }) if stride != 0 => (data_offset, stride),
            _ => return Err(CanonicalLoweringError::UnsupportedExpression("list pattern layout")),
        };
        self.ensure_list_extent(scrutinee)?;
        let count = self.list_counts[&scrutinee];
        let mut bindings = Vec::new();
        for (position, pattern) in elems.iter().enumerate() {
            let name = match pattern {
                Pattern::Var(name) if name != "_" => name.clone(),
                Pattern::Wildcard | Pattern::Var(_) => continue,
                _ => return Err(CanonicalLoweringError::UnsupportedExpression("nested list pattern element")),
            };
            let index = self.push_const_size(u64::try_from(position).map_err(|_| CanonicalLoweringError::IdOverflow)?)?;
            let in_bounds = self.push_compare(ComparisonOperator::LessUnsigned, index, count)?;
            self.push_guard(in_bounds, witchy_wir::canonical_safewir::GuardKind::Bounds);
            let value = self.fresh(element_type)?;
            self.push_operation(Operation::Load {
                destination: value,
                place: witchy_wir::canonical_safewir::Place::projected(
                    scrutinee,
                    Projection::Element { index },
                    element_type,
                    AccessMode::Read,
                ),
                transfer: witchy_wir::canonical_safewir::LoadTransfer::CopyScalar,
            });
            bindings.push((name, value));
        }
        let Some(Some(rest_name)) = rest else { return Ok(bindings) };
        // `..rest`: the elements after the named prefix, copied into a fresh
        // list. `count - prefix` cannot wrap: the arm's length test proved
        // `prefix <= count`, and the trapping add of the prefix back onto the
        // remainder is the exact end the checker's range rule consumes.
        let prefix = self.push_const_size(u64::try_from(elems.len()).map_err(|_| CanonicalLoweringError::IdOverflow)?)?;
        let prefix_fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, prefix, count)?;
        self.push_guard(prefix_fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let rest_count = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Subtract, count, prefix, OverflowBehavior::Wrap)?;
        let maximum = self.push_const_size((u64::from(u32::MAX) - u64::from(header_bytes)) / u64::from(stride_bytes))?;
        let rest_fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, rest_count, maximum)?;
        self.push_guard(rest_fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let source_end = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, prefix, rest_count, OverflowBehavior::Trap)?;
        let source_in_bounds = self.push_compare(ComparisonOperator::LessEqualUnsigned, source_end, count)?;
        self.push_guard(source_in_bounds, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let tail = self.fresh(ValueType::Ref(layout))?;
        self.push_operation(Operation::Alloc {
            destination: tail,
            layout,
            extent: witchy_wir::canonical_safewir::Extent::Counted {
                header_bytes,
                count: rest_count,
                capacity: rest_count,
                stride_bytes,
            },
            class: witchy_wir::canonical_safewir::AllocationClass::ReferenceCounted,
        });
        let zero = self.push_const_size(0)?;
        self.push_operation(Operation::CloneRange {
            destination: witchy_wir::canonical_safewir::Place::projected(
                tail,
                Projection::Slice { start: zero, length: rest_count },
                ValueType::Ref(layout),
                AccessMode::Write,
            ),
            source: witchy_wir::canonical_safewir::Place::projected(
                scrutinee,
                Projection::Slice { start: prefix, length: rest_count },
                ValueType::Ref(layout),
                AccessMode::Read,
            ),
            count: rest_count,
        });
        self.push_operation(Operation::UpdateCount { object: tail, count: rest_count });
        self.list_counts.insert(tail, rest_count);
        self.list_capacities.insert(tail, rest_count);
        bindings.push((rest_name.clone(), tail));
        Ok(bindings)
    }

    fn match_pattern_condition(
        &mut self,
        scrutinee: ValueId,
        pattern: &Pattern,
        scrutinee_type: &Type,
    ) -> Result<Option<ValueId>, CanonicalLoweringError> {
        let (value, expected_type) = match pattern {
            Pattern::Ctor { name, args } => {
                // A sum type's tag selects the arm; a one-variant record's
                // constructor pattern always matches.
                let shape = scalar_aggregate_shape(scrutinee_type, self.definitions).ok_or(
                    CanonicalLoweringError::UnsupportedExpression("constructor pattern scrutinee"),
                )?;
                if !matches!(self.values[&scrutinee], ValueType::Ref(_)) {
                    return Err(CanonicalLoweringError::UnsupportedExpression("constructor pattern scrutinee"));
                }
                // Names, wildcards, and irrefutable tuple payloads leave the
                // tag test as the whole condition.
                let irrefutable = |argument: &Pattern| match argument {
                    Pattern::Var(_) | Pattern::Wildcard => true,
                    Pattern::Tuple(items) => items.iter().all(|item| matches!(item, Pattern::Var(_) | Pattern::Wildcard)),
                    _ => false,
                };
                let (index, variant) = shape.variant(name).ok_or_else(|| CanonicalLoweringError::MissingBinding(name.clone()))?;
                if args.len() != variant.arity {
                    return Err(CanonicalLoweringError::UnsupportedExpression("constructor pattern arity"));
                }
                let first_field = variant.first_field;
                let fields = shape.fields.clone();
                let refutable: Vec<(usize, &Pattern)> = args
                    .iter()
                    .enumerate()
                    .filter(|(_, argument)| !irrefutable(argument))
                    .collect();
                if !shape.tagged {
                    return self.payload_pattern_conditions(scrutinee, first_field, &fields, &refutable);
                }
                let tag = self.load_record_field(scrutinee, 0, ValueType::Int)?;
                let expected = self.push_const_int(i64::try_from(index).map_err(|_| CanonicalLoweringError::IdOverflow)?)?;
                let tag_test = self.push_compare(ComparisonOperator::Equal, tag, expected)?;
                if refutable.is_empty() {
                    return Ok(Some(tag_test));
                }
                // A refutable payload pattern tests its slot only once the tag
                // test has selected the variant: another variant's slot is
                // uninitialized, so the payload test sits behind the tag test
                // as a short-circuit conjunction.
                return Ok(Some(self.short_circuit_and(tag_test, |this| {
                    this.payload_pattern_conditions(scrutinee, first_field, &fields, &refutable)
                })?));
            }
            Pattern::Int(value) | Pattern::Duration(value) => {
                let destination = self.fresh(ValueType::Int)?;
                self.push_operation(Operation::ConstInt { destination, value: *value });
                (destination, ValueType::Int)
            }
            Pattern::Bool(value) => {
                let destination = self.fresh(ValueType::Bool)?;
                self.push_operation(Operation::ConstBool { destination, value: *value });
                (destination, ValueType::Bool)
            }
            Pattern::Wildcard | Pattern::Var(_) => return Ok(None),
            // A tuple pattern of names and wildcards always matches.
            Pattern::Tuple(args) => {
                if args.iter().any(|argument| !matches!(argument, Pattern::Var(_) | Pattern::Wildcard)) {
                    return Err(CanonicalLoweringError::UnsupportedExpression("nested tuple pattern"));
                }
                return Ok(None);
            }
            // A string literal pattern is an equality test against the
            // static literal.
            Pattern::Str(text) => {
                if !matches!(self.values[&scrutinee], ValueType::Ref(_)) {
                    return Err(CanonicalLoweringError::UnsupportedExpression("string pattern scrutinee"));
                }
                let literal = self.lower_string_literal(text)?;
                return Ok(Some(self.lower_string_equality(scrutinee, literal, false)?));
            }
            Pattern::List { elems, rest } => {
                // The length test: exact for `[a, b]`, at least the prefix
                // for `[a, ..]` / `[a, ..rest]`. Element bindings follow in
                // the arm block.
                if !matches!(self.values[&scrutinee], ValueType::Ref(_)) {
                    return Err(CanonicalLoweringError::UnsupportedExpression("list pattern scrutinee"));
                }
                self.ensure_list_extent(scrutinee)?;
                let count = self.list_counts[&scrutinee];
                let prefix = self.push_const_size(u64::try_from(elems.len()).map_err(|_| CanonicalLoweringError::IdOverflow)?)?;
                let operator = if rest.is_some() { ComparisonOperator::GreaterEqualUnsigned } else { ComparisonOperator::Equal };
                return Ok(Some(self.push_compare(operator, count, prefix)?));
            }
            _ => return Err(CanonicalLoweringError::UnsupportedExpression("match pattern")),
        };
        if self.values[&scrutinee] != expected_type {
            return Err(CanonicalLoweringError::UnsupportedExpression("match pattern type"));
        }
        let condition = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::Compare {
            destination: condition,
            operator: ComparisonOperator::Equal,
            left: scrutinee,
            right: value,
        });
        Ok(Some(condition))
    }

    /// The conjunction of the refutable payload patterns of one constructor
    /// arm, each tested on its own slot load. Later payload tests evaluate
    /// only when the earlier ones held, so a nested test's own guarded loads
    /// stay behind every test that dominates them. `None` when no payload
    /// test remains.
    fn payload_pattern_conditions(
        &mut self,
        scrutinee: ValueId,
        first_field: usize,
        fields: &[Type],
        refutable: &[(usize, &Pattern)],
    ) -> Result<Option<ValueId>, CanonicalLoweringError> {
        let Some(((offset, argument), rest)) = refutable.split_first() else {
            return Ok(None);
        };
        let slot = first_field + offset;
        let field_type = fields[slot].clone();
        let value_type = self.field_value_type(&field_type)?;
        let slot = u32::try_from(slot).map_err(|_| CanonicalLoweringError::IdOverflow)?;
        let payload = self.load_record_field(scrutinee, slot, value_type)?;
        let Some(test) = self.match_pattern_condition(payload, argument, &field_type)? else {
            return self.payload_pattern_conditions(scrutinee, first_field, fields, rest);
        };
        if rest.is_empty() {
            return Ok(Some(test));
        }
        Ok(Some(self.short_circuit_and(test, |this| {
            this.payload_pattern_conditions(scrutinee, first_field, fields, rest)
        })?))
    }

    /// `left && right` where `right` is lowered in its own block only when
    /// `left` held; a `None` right operand holds trivially. The lowered right
    /// operand may not change the binding environment.
    fn short_circuit_and(
        &mut self,
        left: ValueId,
        right: impl FnOnce(&mut Self) -> Result<Option<ValueId>, CanonicalLoweringError>,
    ) -> Result<ValueId, CanonicalLoweringError> {
        if self.values[&left] != ValueType::Bool {
            return Err(CanonicalLoweringError::UnsupportedExpression("non-boolean short-circuit operand"));
        }
        let facts = self.snapshot_facts();
        let bindings = self.bindings.clone();
        let evaluate_right = self.new_block(&[])?;
        let decided = self.new_block(&[])?;
        let join = self.new_block(&[ValueType::Bool])?;
        let result = self.blocks[join.get() as usize].parameters[0].value;
        self.finish_current(Terminator::BranchIf {
            condition: left,
            then_target: evaluate_right,
            then_arguments: Vec::new(),
            else_target: decided,
            else_arguments: Vec::new(),
        })?;

        self.current = decided;
        let decided_value = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::ConstBool { destination: decided_value, value: false });
        self.finish_current(Terminator::Branch { target: join, arguments: vec![decided_value] })?;

        self.current = evaluate_right;
        self.restore_snapshot(facts.clone());
        self.bindings = bindings.clone();
        let right = match right(self)? {
            Some(right) => right,
            None => {
                let held = self.fresh(ValueType::Bool)?;
                self.push_operation(Operation::ConstBool { destination: held, value: true });
                held
            }
        };
        if self.values[&right] != ValueType::Bool {
            return Err(CanonicalLoweringError::UnsupportedExpression("non-boolean short-circuit operand"));
        }
        if self.bindings != bindings {
            return Err(CanonicalLoweringError::UnsupportedExpression("binding change inside short-circuit operand"));
        }
        self.finish_current(Terminator::Branch { target: join, arguments: vec![right] })?;

        self.restore_facts(facts, join);
        self.current = join;
        self.bindings = bindings;
        Ok(result)
    }

    /// Whether `value` is also bound under a name other than `name`: a live
    /// `let` alias that must keep observing the pre-call value.
    fn aliased_binding(&self, name: &str, value: ValueId) -> bool {
        self.bindings.iter().any(|(other, bound)| other != name && *bound == value)
    }

    /// A tail-site path for the statement at `statement` of this function's
    /// body. A lambda's body is addressed under the lambda's own path plus
    /// `Child(0)`, as the checked manifest keys it.
    fn tail_path(&self, statement: usize, position: ProperTailPathStep) -> Result<Vec<ProperTailPathStep>, CanonicalLoweringError> {
        let statement = u32::try_from(statement).map_err(|_| CanonicalLoweringError::IdOverflow)?;
        let mut path = self.tail_prefix.clone();
        path.extend([ProperTailPathStep::Statement(statement), position]);
        Ok(path)
    }

    /// A loop header lane that started unique but is carried a known-shared
    /// value on a back edge (a call result, a clone) can only be a shared
    /// phi; the exit takes the same demotion when it mirrors the header.
    fn demote_unique_lanes_carrying_shared(&mut self, header: BlockId, exit: BlockId, carried: &[ValueId]) {
        let demoted = carried
            .iter()
            .enumerate()
            .filter(|(index, value)| {
                matches!(self.values.get(value), Some(ValueType::Ref(_)))
                    && self.known_shared_value(**value)
                    && self.blocks[header.get() as usize]
                        .parameters
                        .get(*index)
                        .is_some_and(|parameter| parameter.ownership == Some(Ownership::Unique))
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        for index in demoted {
            self.blocks[header.get() as usize].parameters[index].ownership = Some(Ownership::Shared);
            if let Some(parameter) = self.blocks[exit.get() as usize].parameters.get_mut(index)
                && parameter.ownership == Some(Ownership::Unique)
            {
                parameter.ownership = Some(Ownership::Shared);
            }
        }
    }

    /// The state lanes a `break`/`continue` carries out of the current
    /// block: every loop-state binding's current value, then each tracked
    /// list's split count/capacity lanes.
    fn loop_edge_arguments(&mut self, names: &[String], list_names: &[String]) -> Result<Vec<ValueId>, CanonicalLoweringError> {
        let mut arguments = names
            .iter()
            .map(|name| self.bindings.get(name).copied().ok_or_else(|| CanonicalLoweringError::MissingBinding(name.clone())))
            .collect::<Result<Vec<_>, _>>()?;
        for name in list_names {
            let object = self.bindings[name];
            let (count, capacity) = self.tracked_lanes(object).ok_or(
                CanonicalLoweringError::UnsupportedExpression("loop edge list count state"),
            )?;
            let (count, capacity) = self.split_lanes(count, capacity)?;
            arguments.push(count);
            arguments.push(capacity);
        }
        Ok(arguments)
    }

    /// `break`: leave the innermost loop through its exit block with the
    /// current state lanes.
    fn lower_break(&mut self) -> Result<(), CanonicalLoweringError> {
        let Some(context) = self.loop_contexts.last() else {
            return Err(CanonicalLoweringError::UnsupportedStatement("break outside a loop"));
        };
        let (exit, names, list_names) = (context.exit, context.names.clone(), context.list_names.clone());
        let counter = match context.advance {
            LoopAdvance::IntCounter(counter) => Some(counter),
            _ => None,
        };
        let mut arguments = self.loop_edge_arguments(&names, &list_names)?;
        arguments.extend(counter);
        self.finish_current(Terminator::Branch { target: exit, arguments })
    }

    /// `continue`: re-enter the innermost loop's header with the current
    /// state lanes and the advanced cursor.
    fn lower_continue(&mut self) -> Result<(), CanonicalLoweringError> {
        let Some(context) = self.loop_contexts.last() else {
            return Err(CanonicalLoweringError::UnsupportedStatement("continue outside a loop"));
        };
        let (header, names, list_names) = (context.header, context.names.clone(), context.list_names.clone());
        let advance = match context.advance {
            LoopAdvance::Condition => None,
            LoopAdvance::ListCursor(cursor) => {
                let one = self.push_const_size(1)?;
                Some(self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, cursor, one, OverflowBehavior::Wrap)?)
            }
            LoopAdvance::IntCounter(counter) => {
                let one = self.push_const_int(1)?;
                Some(self.push_arithmetic(ValueType::Int, ArithmeticOperator::Add, counter, one, OverflowBehavior::Wrap)?)
            }
        };
        let mut arguments = self.loop_edge_arguments(&names, &list_names)?;
        arguments.extend(advance);
        self.finish_current(Terminator::Branch { target: header, arguments })
    }

    /// A loop whose body breaks joins its exit from the header and from the
    /// break edges: the exit lanes take the header lanes' phi authority so
    /// a rebound list rebases there as it does at the header.
    fn give_exit_header_ownership(&mut self, header: BlockId, exit: BlockId) {
        let ownerships = self.blocks[header.get() as usize]
            .parameters
            .iter()
            .map(|parameter| parameter.ownership)
            .collect::<Vec<_>>();
        for (parameter, ownership) in self.blocks[exit.get() as usize].parameters.iter_mut().zip(ownerships) {
            parameter.ownership = ownership;
        }
    }

    /// `while let PATTERN = SCRUTINEE: BODY` as a loop whose header binds
    /// the scrutinee and tests the pattern; the body sees the pattern's
    /// bindings. The state tuple is `lower_while`'s.
    fn lower_while_let(
        &mut self,
        pattern: &Pattern,
        scrutinee: &Expr,
        body: &witchy_syntax::ast::Block,
    ) -> Result<(), CanonicalLoweringError> {
        let facts = self.snapshot_facts();
        let outer = self.bindings.clone();
        // Only a name the body rebinds is a header lane; every other outer
        // binding keeps its value (and its token) across the loop, as in
        // `lower_for`, so an inner loop never renames what an outer loop
        // does not carry.
        let rebound = self.loop_rebindings_with_write_backs(body);
        let mut names = loop_lane_names(&outer, &rebound);
        let list_names = names
            .iter()
            .filter_map(|name| {
                let object = outer[name];
                self.tracked_lanes(object).is_some().then(|| name.clone())
            })
            .collect::<Vec<_>>();
        let mut state_types = names
            .iter()
            .map(|name| self.values[&outer[name]])
            .collect::<Vec<_>>();
        state_types.extend(list_names.iter().flat_map(|_| [ValueType::Size, ValueType::Size]));
        let header = self.new_block(&state_types)?;
        let exit = self.new_block(&state_types)?;
        self.mark_tracked_phi_parameters(header, &names, &list_names, &outer, false, &rebound);
        if block_breaks_loop(body) {
            self.give_exit_header_ownership(header, exit);
        }
        let header_values = self.block_parameter_values(header);
        let mut entry_state = names.iter().map(|name| outer[name]).collect::<Vec<_>>();
        for name in &list_names {
            let (count, capacity) = self.tracked_lanes(outer[name]).expect("tracked names have lanes");
            let (count, capacity) = self.split_lanes(count, capacity)?;
            entry_state.push(count);
            entry_state.push(capacity);
        }
        self.finish_current(Terminator::Branch { target: header, arguments: entry_state })?;
        self.current = header;
        self.bindings = lane_bindings(&outer, &names, header_values.iter().copied());
        let mut metadata_index = names.len();
        for name in &list_names {
            let object = self.bindings[name];
            self.set_tracked_lanes(object, header_values[metadata_index], header_values[metadata_index + 1]);
            metadata_index += 2;
        }
        let scrutinee_type = self.expression_type(scrutinee)?;
        let value = self.lower_expr(scrutinee)?;
        let condition = self.match_pattern_condition(value, pattern, &scrutinee_type)?;
        // The body sees the scrutinee through its own lane: the edge renames
        // a lane value to the body's parameter, and a fresh value (a call
        // result) travels as one more lane rather than crossing the edge
        // unrenamed.
        let lane_position = header_values.iter().position(|lane| *lane == value);
        let mut body_types = state_types.clone();
        if lane_position.is_none() {
            body_types.push(self.values[&value]);
        }
        let body_id = self.new_block(&body_types)?;
        let mut body_arguments = header_values.clone();
        if lane_position.is_none() {
            body_arguments.push(value);
        }
        match condition {
            Some(condition) => self.finish_current(Terminator::BranchIf {
                condition,
                then_target: body_id,
                then_arguments: body_arguments,
                else_target: exit,
                else_arguments: header_values.clone(),
            })?,
            None => self.finish_current(Terminator::Branch { target: body_id, arguments: body_arguments })?,
        }
        let body_values = self.block_parameter_values(body_id);
        self.current = body_id;
        self.bindings = lane_bindings(&outer, &names, body_values.iter().copied());
        metadata_index = names.len();
        for name in &list_names {
            let object = self.bindings[name];
            self.set_tracked_lanes(object, body_values[metadata_index], body_values[metadata_index + 1]);
            metadata_index += 2;
        }
        let body_value = match lane_position {
            Some(position) => body_values[position],
            None => *body_values.last().expect("the scrutinee lane"),
        };
        if let Some(position) = lane_position
            && let Some((count, capacity)) = self.tracked_lanes(header_values[position])
        {
            let _ = (count, capacity);
        }
        for (name, bound) in self.lower_match_pattern(body_value, pattern, &scrutinee_type)? {
            self.bindings.insert(name, bound);
        }
        self.loop_contexts.push(LoopContext {
            header,
            exit,
            names: names.clone(),
            list_names: list_names.clone(),
            advance: LoopAdvance::Condition,
        });
        let lowered = self.lower_block_effect(body);
        self.loop_contexts.pop();
        lowered?;
        if self.blocks[self.current.get() as usize].terminator.is_none() {
            let carried = self.loop_edge_arguments(&names, &list_names)?;
            self.demote_unique_lanes_carrying_shared(header, exit, &carried);
            self.finish_current(Terminator::Branch { target: header, arguments: carried })?;
        }
        self.restore_facts(facts, exit);
        self.current = exit;
        let exit_parameters = self.blocks[exit.get() as usize].parameters.clone();
        self.bindings = lane_bindings(&outer, &names, exit_parameters.iter().map(|parameter| parameter.value));
        metadata_index = names.len();
        for name in &list_names {
            let object = self.bindings[name];
            self.set_tracked_lanes(object, exit_parameters[metadata_index].value, exit_parameters[metadata_index + 1].value);
            metadata_index += 2;
        }
        Ok(())
    }

    fn lower_while(
        &mut self,
        condition: &Expr,
        body: &witchy_syntax::ast::Block,
    ) -> Result<(), CanonicalLoweringError> {
        let facts = self.snapshot_facts();
        let outer = self.bindings.clone();
        // Only a name the body rebinds is a header lane; every other outer
        // binding keeps its value (and its token) across the loop, as in
        // `lower_for`, so an inner loop never renames what an outer loop
        // does not carry.
        let rebound = self.loop_rebindings_with_write_backs(body);
        let mut names = loop_lane_names(&outer, &rebound);
        let list_names = names
            .iter()
            .filter_map(|name| {
                let object = outer[name];
                self.tracked_lanes(object).is_some().then(|| name.clone())
            })
            .collect::<Vec<_>>();
        if list_names.iter().any(|name| self.tracked_lanes(outer[name]).is_none()) {
            return Err(CanonicalLoweringError::UnsupportedExpression("while list capacity state"));
        }
        let mut state_types = names
            .iter()
            .map(|name| self.values[&outer[name]])
            .collect::<Vec<_>>();
        state_types.extend(list_names.iter().flat_map(|_| [ValueType::Size, ValueType::Size]));
        let header = self.new_block(&state_types)?;
        let body_id = self.new_block(&state_types)?;
        let exit = self.new_block(&state_types)?;
        self.mark_tracked_phi_parameters(header, &names, &list_names, &outer, false, &rebound);
        if block_breaks_loop(body) {
            self.give_exit_header_ownership(header, exit);
        }
        let header_parameters = self.blocks[header.get() as usize].parameters.clone();
        let header_values = header_parameters.iter().map(|parameter| parameter.value).collect::<Vec<_>>();
        let mut entry_state = names.iter().map(|name| outer[name]).collect::<Vec<_>>();
        for name in &list_names {
            let object = outer[name];
            let (count, capacity) = self.tracked_lanes(object).expect("tracked names have lanes");
            let (count, capacity) = self.split_lanes(count, capacity)?;
            entry_state.push(count);
            entry_state.push(capacity);
        }
        self.finish_current(Terminator::Branch {
            target: header,
            arguments: entry_state,
        })?;
        self.current = header;
        self.bindings = lane_bindings(&outer, &names, header_values.iter().copied());
        let mut metadata_index = names.len();
        for name in &list_names {
            let object = self.bindings[name];
            self.set_tracked_lanes(object, header_values[metadata_index], header_values[metadata_index + 1]);
            metadata_index += 2;
        }
        let condition = self.lower_expr(condition)?;
        if self.values[&condition] != ValueType::Bool {
            return Err(CanonicalLoweringError::UnsupportedExpression("non-boolean condition"));
        }
        let exit_parameters = self.blocks[exit.get() as usize].parameters.clone();
        self.finish_current(Terminator::BranchIf {
            condition,
            then_target: body_id,
            then_arguments: header_values.clone(),
            else_target: exit,
            else_arguments: header_values.clone(),
        })?;
        let body_parameters = self.blocks[body_id.get() as usize].parameters.clone();
        let body_values = body_parameters.iter().map(|parameter| parameter.value).collect::<Vec<_>>();
        self.current = body_id;
        self.bindings = lane_bindings(&outer, &names, body_values.iter().copied());
        metadata_index = names.len();
        for name in &list_names {
            let object = self.bindings[name];
            self.set_tracked_lanes(object, body_values[metadata_index], body_values[metadata_index + 1]);
            metadata_index += 2;
        }
        self.loop_contexts.push(LoopContext {
            header,
            exit,
            names: names.clone(),
            list_names: list_names.clone(),
            advance: LoopAdvance::Condition,
        });
        let lowered = self.lower_block_effect(body);
        self.loop_contexts.pop();
        lowered?;
        let body_bindings = self.bindings.clone();
        let mut carried = names.iter().map(|name| body_bindings[name]).collect::<Vec<_>>();
        for name in &list_names {
            let object = body_bindings[name];
            let (count, capacity) = self.tracked_lanes(object).ok_or(
                CanonicalLoweringError::UnsupportedExpression("while backedge list count state"),
            )?;
            let (count, capacity) = self.split_lanes(count, capacity)?;
            carried.push(count);
            carried.push(capacity);
        }
        let body_end = self.current;
        self.demote_unique_lanes_carrying_shared(header, exit, &carried);
        self.blocks[body_end.get() as usize].terminator = Some(Terminator::Branch {
            target: header,
            arguments: carried,
        });
        self.restore_facts(facts, exit);
        self.current = exit;
        self.bindings = lane_bindings(&outer, &names, exit_parameters.iter().map(|parameter| parameter.value));
        metadata_index = names.len();
        for name in &list_names {
            let object = self.bindings[name];
            // A string lane's facts live in the string tables.
            self.set_tracked_lanes(object, exit_parameters[metadata_index].value, exit_parameters[metadata_index + 1].value);
            metadata_index += 2;
        }
        Ok(())
    }

    /// `for i in lo..hi` (or `lo..=hi`) over integers as an explicit counted
    /// loop: the same state tuple `lower_while` threads (every outer binding,
    /// each tracked list's count/capacity lanes) plus the counter. The bound
    /// is evaluated once before the loop; the counter is a fresh header lane
    /// and the loop variable binds to it inside the body only.
    fn lower_range_for(
        &mut self,
        variable: &str,
        lo: &Expr,
        hi: &Expr,
        inclusive: bool,
        body: &witchy_syntax::ast::Block,
    ) -> Result<(), CanonicalLoweringError> {
        let facts = self.snapshot_facts();
        let start = self.lower_expr(lo)?;
        let bound = self.lower_expr(hi)?;
        if self.values[&start] != ValueType::Int || self.values[&bound] != ValueType::Int {
            return Err(CanonicalLoweringError::UnsupportedExpression("non-integer range bounds"));
        }
        let outer = self.bindings.clone();
        if outer.contains_key(variable) {
            return Err(CanonicalLoweringError::DuplicateBinding(variable.to_owned()));
        }
        // Only a name the body rebinds is a header lane; every other outer
        // binding keeps its value (and its token) across the loop, as in
        // `lower_for`, so an inner loop never renames what an outer loop
        // does not carry.
        let rebound = self.loop_rebindings_with_write_backs(body);
        let mut names = loop_lane_names(&outer, &rebound);
        let list_names = names
            .iter()
            .filter_map(|name| {
                let object = outer[name];
                self.tracked_lanes(object).is_some().then(|| name.clone())
            })
            .collect::<Vec<_>>();
        if list_names.iter().any(|name| self.tracked_lanes(outer[name]).is_none()) {
            return Err(CanonicalLoweringError::UnsupportedExpression("for list capacity state"));
        }
        let mut state_types = names.iter().map(|name| self.values[&outer[name]]).collect::<Vec<_>>();
        state_types.extend(list_names.iter().flat_map(|_| [ValueType::Size, ValueType::Size]));
        state_types.push(ValueType::Int);
        let header = self.new_block(&state_types)?;
        let body_id = self.new_block(&state_types)?;
        let exit = self.new_block(&state_types)?;
        self.mark_tracked_phi_parameters(header, &names, &list_names, &outer, false, &rebound);
        if block_breaks_loop(body) {
            self.give_exit_header_ownership(header, exit);
        }
        let header_values = self.blocks[header.get() as usize].parameters.iter().map(|parameter| parameter.value).collect::<Vec<_>>();
        let mut entry_state = names.iter().map(|name| outer[name]).collect::<Vec<_>>();
        for name in &list_names {
            let object = outer[name];
            let (count, capacity) = self.tracked_lanes(object).expect("tracked names have lanes");
            let (count, capacity) = self.split_lanes(count, capacity)?;
            entry_state.push(count);
            entry_state.push(capacity);
        }
        entry_state.push(start);
        self.finish_current(Terminator::Branch { target: header, arguments: entry_state })?;

        let bind_state = |lowerer: &mut Self, values: &[ValueId]| {
            lowerer.bindings = lane_bindings(&outer, &names, values.iter().copied());
            let mut metadata_index = names.len();
            for name in &list_names {
                let object = lowerer.bindings[name];
                lowerer.set_tracked_lanes(object, values[metadata_index], values[metadata_index + 1]);
                metadata_index += 2;
            }
        };

        self.current = header;
        bind_state(self, &header_values);
        let counter = *header_values.last().ok_or(CanonicalLoweringError::IdOverflow)?;
        let condition = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::Compare {
            destination: condition,
            operator: if inclusive { ComparisonOperator::LessEqualSigned } else { ComparisonOperator::LessSigned },
            left: counter,
            right: bound,
        });
        self.finish_current(Terminator::BranchIf {
            condition,
            then_target: body_id,
            then_arguments: header_values.clone(),
            else_target: exit,
            else_arguments: header_values.clone(),
        })?;

        let body_values = self.blocks[body_id.get() as usize].parameters.iter().map(|parameter| parameter.value).collect::<Vec<_>>();
        self.current = body_id;
        bind_state(self, &body_values);
        let body_counter = *body_values.last().ok_or(CanonicalLoweringError::IdOverflow)?;
        self.bindings.insert(variable.to_owned(), body_counter);
        self.loop_contexts.push(LoopContext {
            header,
            exit,
            names: names.clone(),
            list_names: list_names.clone(),
            advance: LoopAdvance::IntCounter(body_counter),
        });
        let lowered = self.lower_block_effect(body);
        self.loop_contexts.pop();
        lowered?;
        self.bindings.remove(variable);
        let body_bindings = self.bindings.clone();
        let mut carried = names.iter().map(|name| body_bindings[name]).collect::<Vec<_>>();
        for name in &list_names {
            let object = body_bindings[name];
            let (count, capacity) = self.tracked_lanes(object).ok_or(
                CanonicalLoweringError::UnsupportedExpression("for backedge list count state"),
            )?;
            let (count, capacity) = self.split_lanes(count, capacity)?;
            carried.push(count);
            carried.push(capacity);
        }
        let one = self.fresh(ValueType::Int)?;
        self.push_operation(Operation::ConstInt { destination: one, value: 1 });
        let next = self.fresh(ValueType::Int)?;
        self.push_operation(Operation::Arithmetic {
            destination: next,
            operator: ArithmeticOperator::Add,
            left: body_counter,
            right: one,
            overflow: OverflowBehavior::Wrap,
        });
        carried.push(next);
        let body_end = self.current;
        self.demote_unique_lanes_carrying_shared(header, exit, &carried);
        self.blocks[body_end.get() as usize].terminator = Some(Terminator::Branch { target: header, arguments: carried });

        let exit_values = self.blocks[exit.get() as usize].parameters.iter().map(|parameter| parameter.value).collect::<Vec<_>>();
        self.restore_facts(facts, exit);
        self.current = exit;
        bind_state(self, &exit_values);
        Ok(())
    }

    fn lower_for(
        &mut self,
        variable: &str,
        iterable: &Expr,
        body: &witchy_syntax::ast::Block,
    ) -> Result<(), CanonicalLoweringError> {
        let facts = self.snapshot_facts();
        if let Expr::Range { lo, hi, inclusive } = iterable {
            return self.lower_range_for(variable, lo, hi, *inclusive, body);
        }
        let logical_type = self.expression_type(iterable)?;
        let (layout, element_type, header_bytes, stride_bytes) =
            list_layout(&logical_type, self.layout_bundle, self.layouts, self.definitions)?;
        let source = self.lower_expr(iterable)?;
        if self.values[&source] != ValueType::Ref(layout) {
            return Err(CanonicalLoweringError::UnsupportedExpression("for list object type"));
        }
        // A source without count facts (an element loaded out of a list of
        // lists, a call result) authenticates its header here.
        self.ensure_list_extent(source)?;

        let outer = self.bindings.clone();
        // Every outer binding the body rebinds anywhere (an assignment nested
        // in an `if` arm counts) is loop state; a top-level-only scan once
        // dropped a nested `n = n + 1` and the loop silently kept the entry
        // value.
        let rebound = self.loop_rebindings_with_write_backs(body);
        let mut names = rebound
            .assigned
            .iter()
            .chain(rebound.pushed.iter())
            .filter(|name| outer.contains_key(*name))
            .cloned()
            .collect::<Vec<_>>();
        names.sort();
        names.dedup();
        if matches!(iterable, Expr::Var(name) if names.contains(name)) {
            return Err(CanonicalLoweringError::UnsupportedExpression("for iterator mutation"));
        }
        let list_names = names
            .iter()
            .filter_map(|name| {
                let object = outer[name];
                self.tracked_lanes(object).is_some().then(|| name.clone())
            })
            .collect::<Vec<_>>();
        if list_names.iter().any(|name| self.tracked_lanes(outer[name]).is_none()) {
            return Err(CanonicalLoweringError::UnsupportedExpression("for list capacity state"));
        }

        // The loop carries one canonical state tuple through every CFG edge:
        // mutable bindings, each mutable list's logical count/capacity, then
        // the iterator index.  Do not reconstruct metadata lanes from the
        // binding map after a body has rebound an object: that can associate a
        // reference parameter with a Size parameter at the backedge.
        let mut state_types = names
            .iter()
            .map(|name| self.values[&outer[name]])
            .collect::<Vec<_>>();
        state_types.extend(list_names.iter().flat_map(|_| [ValueType::Size, ValueType::Size]));
        state_types.push(ValueType::Size);
        let exit_types = state_types[..state_types.len() - 1].to_vec();
        let zero = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::ConstSize { destination: zero, value: 0 });
        let header = self.new_block(&state_types)?;
        let body_id = self.new_block(&state_types)?;
        let exit = self.new_block(&exit_types)?;
        // A tracked mutable list is produced by canonical allocation/push
        // templates and remains the sole local token until it is explicitly
        // shared or moved.  Carry that fact only on the loop header's Ref
        // lane, which permits a checked Unique phi to rebase the fresh list
        // allocated by a prior iteration.  Untracked/Shared references stay
        // unspecified and therefore cannot acquire phi authority.
        self.mark_tracked_phi_parameters(header, &names, &list_names, &outer, true, &rebound);
        if block_breaks_loop(body) {
            self.give_exit_header_ownership(header, exit);
        }
        let mut entry_state = names.iter().map(|name| outer[name]).collect::<Vec<_>>();
        for name in &list_names {
            let object = outer[name];
            let (count, capacity) = self.tracked_lanes(object).expect("tracked names have lanes");
            let (count, capacity) = self.split_lanes(count, capacity)?;
            entry_state.push(count);
            entry_state.push(capacity);
        }
        entry_state.push(zero);
        self.finish_current(Terminator::Branch { target: header, arguments: entry_state })?;

        let header_parameters = self.blocks[header.get() as usize].parameters.clone();
        let header_values = header_parameters.iter().map(|parameter| parameter.value).collect::<Vec<_>>();
        self.current = header;
        self.bindings = outer.clone();
        for (name, value) in names.iter().zip(&header_values) {
            self.bindings.insert(name.clone(), *value);
        }
        let mut metadata_index = names.len();
        for name in &list_names {
            let object = self.bindings[name];
            self.set_tracked_lanes(object, header_values[metadata_index], header_values[metadata_index + 1]);
            metadata_index += 2;
        }
        let index = *header_values.last().ok_or(CanonicalLoweringError::IdOverflow)?;
        let logical_length = *self
            .list_counts
            .get(&source)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("for list count state"))?;
        let _capacity = *self
            .list_capacities
            .get(&source)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("for list capacity state"))?;
        let maximum_count = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::ConstSize {
            destination: maximum_count,
            value: (u64::from(u32::MAX) - u64::from(header_bytes)) / u64::from(stride_bytes),
        });
        let condition = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::Compare {
            destination: condition,
            operator: ComparisonOperator::LessUnsigned,
            left: index,
            right: logical_length,
        });
        self.finish_current(Terminator::BranchIf {
            condition,
            then_target: body_id,
            then_arguments: header_values.clone(),
            else_target: exit,
            else_arguments: header_values[..metadata_index].to_vec(),
        })?;

        let body_parameters = self.blocks[body_id.get() as usize].parameters.clone();
        let body_values = body_parameters.iter().map(|parameter| parameter.value).collect::<Vec<_>>();
        self.current = body_id;
        self.bindings = outer.clone();
        for (name, value) in names.iter().zip(&body_values) {
            self.bindings.insert(name.clone(), *value);
        }
        metadata_index = names.len();
        for name in &list_names {
            let object = self.bindings[name];
            self.set_tracked_lanes(object, body_values[metadata_index], body_values[metadata_index + 1]);
            metadata_index += 2;
        }
        let body_index = *body_values.last().ok_or(CanonicalLoweringError::IdOverflow)?;
        let body_logical_length = *self
            .list_counts
            .get(&source)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("for list count state"))?;
        let body_condition = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::Compare {
            destination: body_condition,
            operator: ComparisonOperator::LessUnsigned,
            left: body_index,
            right: body_logical_length,
        });
        self.push_operation(Operation::Guard {
            condition: body_condition,
            kind: witchy_wir::canonical_safewir::GuardKind::Bounds,
        });
        let element = self.fresh(element_type)?;
        self.push_operation(Operation::Load {
            destination: element,
            place: witchy_wir::canonical_safewir::Place::projected(
                source,
                Projection::Element { index: body_index },
                element_type,
                AccessMode::Read,
            ),
            transfer: if matches!(element_type, ValueType::Ref(_)) {
                witchy_wir::canonical_safewir::LoadTransfer::CloneShared
            } else {
                witchy_wir::canonical_safewir::LoadTransfer::CopyScalar
            },
        });
        self.ensure_string_extent(element)?;
        self.bindings.insert(variable.to_owned(), element);
        self.loop_contexts.push(LoopContext {
            header,
            exit,
            names: names.clone(),
            list_names: list_names.clone(),
            advance: LoopAdvance::ListCursor(body_index),
        });
        let lowered = self.lower_block_effect(body);
        self.loop_contexts.pop();
        lowered?;
        let body_bindings = self.bindings.clone();
        let increment_fits = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::Compare {
            destination: increment_fits,
            operator: ComparisonOperator::LessUnsigned,
            left: body_index,
            right: maximum_count,
        });
        self.push_operation(Operation::Guard {
            condition: increment_fits,
            kind: witchy_wir::canonical_safewir::GuardKind::Arithmetic,
        });
        let one = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::ConstSize { destination: one, value: 1 });
        let next_index = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::Arithmetic {
            destination: next_index,
            operator: ArithmeticOperator::Add,
            left: body_index,
            right: one,
            overflow: OverflowBehavior::Wrap,
        });
        let mut back_state = names
            .iter()
            .map(|name| body_bindings[name])
            .collect::<Vec<_>>();
        for name in &list_names {
            let object = body_bindings[name];
            let (count, capacity) = self.tracked_lanes(object).ok_or(
                CanonicalLoweringError::UnsupportedExpression("for backedge list count state"),
            )?;
            let (count, capacity) = self.split_lanes(count, capacity)?;
            back_state.push(count);
            back_state.push(capacity);
        }
        back_state.push(next_index);
        self.demote_unique_lanes_carrying_shared(header, exit, &back_state);
        self.finish_current(Terminator::Branch { target: header, arguments: back_state })?;

        let exit_parameters = self.blocks[exit.get() as usize].parameters.clone();
        self.restore_facts(facts, exit);
        self.current = exit;
        self.bindings = outer;
        for (name, parameter) in names.iter().zip(&exit_parameters) {
            self.bindings.insert(name.clone(), parameter.value);
        }
        metadata_index = names.len();
        for name in &list_names {
            let object = self.bindings[name];
            // A string lane's facts live in the string tables.
            self.set_tracked_lanes(object, exit_parameters[metadata_index].value, exit_parameters[metadata_index + 1].value);
            metadata_index += 2;
        }
        Ok(())
    }

    fn new_block(&mut self, parameter_types: &[ValueType]) -> Result<BlockId, CanonicalLoweringError> {
        let id = BlockId::new(
            u32::try_from(self.blocks.len()).map_err(|_| CanonicalLoweringError::IdOverflow)?,
        );
        let parameters = parameter_types
            .iter()
            .map(|value_type| {
                Ok(BlockParameter {
                    value: self.fresh(*value_type)?,
                    value_type: *value_type,
                    ownership: None,
                })
            })
            .collect::<Result<Vec<_>, CanonicalLoweringError>>()?;
        self.blocks.push(BlockBuilder {
            id,
            parameters,
            operations: Vec::new(),
            terminator: None,
        });
        Ok(id)
    }

    fn finish_current(&mut self, terminator: Terminator) -> Result<(), CanonicalLoweringError> {
        let terminator = match terminator {
            Terminator::Return { mut values } => {
                // Source `var` parameters are returned as affine replacement
                // tokens after the source-level result lanes.  A source-level
                // result that aliases the same token gets its own RC token.
                for name in self.write_back_parameters.clone() {
                    let value = *self.bindings.get(&name)
                        .ok_or_else(|| CanonicalLoweringError::MissingBinding(name.clone()))?;
                    if values.contains(&value) && matches!(self.values[&value], ValueType::Ref(_)) {
                        // The write-back lane keeps the unique token; the
                        // result lane gets its own owned copy (a shared
                        // clone would weaken the token the lane must return).
                        let ty = self.write_back_parameter_types.get(&name).cloned()
                            .ok_or_else(|| CanonicalLoweringError::MissingBinding(name.clone()))?;
                        let copy = self.owned_for_storage(value, &ty)?;
                        for result in &mut values {
                            if *result == value { *result = copy; }
                        }
                    }
                    values.push(value);
                }
                Terminator::Return { values }
            }
            terminator => terminator,
        };
        let transferred = match &terminator {
            Terminator::Return { values } => values.clone(),
            Terminator::TailCall { arguments, .. }
            | Terminator::VarTailCall { arguments, .. } => arguments
                .iter()
                .filter_map(|argument| match argument {
                    CallArgument::Move(value) => Some(*value),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        if matches!(terminator, Terminator::Return { .. } | Terminator::TailCall { .. } | Terminator::VarTailCall { .. }) {
            let drops = self
                .owned_parameters
                .iter()
                .map(|parameter| {
                    self.parameter_names
                        .get(parameter)
                        .and_then(|name| self.bindings.get(name))
                        .copied()
                        .filter(|current| matches!(self.values.get(current), Some(ValueType::Ref(_))))
                        .unwrap_or(*parameter)
                })
                .filter(|parameter| !transferred.contains(parameter))
                .collect::<Vec<_>>();
            for object in drops {
                self.push_operation(Operation::DropRef { object });
            }
        }
        let block = &mut self.blocks[self.current.get() as usize];
        if block.terminator.replace(terminator).is_some() {
            return Err(CanonicalLoweringError::UnsupportedStatement("duplicate block terminator"));
        }
        Ok(())
    }

    fn push_operation(&mut self, operation: Operation) {
        self.blocks[self.current.get() as usize].operations.push(operation);
    }

    /// Whether `value` is a direct static-data object (a string literal). A
    /// static carries no reference-count token: it cannot be cloned, moved into
    /// a callee, or returned as an owned result. Where the ABI needs a token the
    /// lowering materializes a fresh reference-counted copy, which is
    /// source-unobservable under value semantics (RFC-0165 static class).
    fn is_static_string(&self, value: ValueId) -> bool {
        self.blocks.iter().any(|block| {
            block.operations.iter().any(|operation| {
                matches!(operation, Operation::StaticData { destination, .. } if *destination == value)
            })
        })
    }

    /// Whether `value` holds a reference-count token the release placer
    /// tracks: a fresh allocation, a clone, a call result, a loaded shared
    /// element, or a join parameter all of whose lanes carried one. Static
    /// data and join parameters over static data carry none.
    fn carries_token(&self, value: ValueId) -> bool {
        self.carries_token_seen(value, &mut BTreeSet::new())
    }

    /// Token-ness follows the explicit CFG: an owned entry parameter, an
    /// operation that mints or transfers a token, or a block parameter some
    /// incoming edge feeds from a token-carrying argument (a loop threads a
    /// list binding through its header, body, and exit parameters). A cycle
    /// through a loop back edge contributes nothing.
    fn carries_token_seen(&self, value: ValueId, seen: &mut BTreeSet<ValueId>) -> bool {
        if self.token_join_parameters.contains(&value) {
            return true;
        }
        if !seen.insert(value) {
            return false;
        }
        if self.blocks.first().is_some_and(|entry| {
            entry.parameters.iter().any(|parameter| parameter.value == value && parameter.ownership.is_some())
        }) {
            return true;
        }
        let minted = self.blocks.iter().any(|block| {
            block.operations.iter().any(|operation| match operation {
                Operation::Alloc { destination, class: witchy_wir::canonical_safewir::AllocationClass::ReferenceCounted, .. }
                | Operation::CloneRef { destination, .. } => *destination == value,
                Operation::Load { destination, transfer: witchy_wir::canonical_safewir::LoadTransfer::CloneShared, .. } => {
                    *destination == value
                }
                Operation::Call { destinations, .. } | Operation::VarCall { destinations, .. } => {
                    destinations.contains(&value)
                }
                _ => false,
            })
        });
        if minted {
            return true;
        }
        let Some((target, index)) = self.blocks.iter().find_map(|block| {
            block.parameters.iter().position(|parameter| parameter.value == value).map(|index| (block.id, index))
        }) else {
            return false;
        };
        let mut arguments = Vec::new();
        for block in &self.blocks {
            match &block.terminator {
                Some(Terminator::Branch { target: edge, arguments: edge_arguments }) if *edge == target => {
                    arguments.extend(edge_arguments.get(index).copied());
                }
                Some(Terminator::BranchIf { then_target, then_arguments, else_target, else_arguments, .. }) => {
                    if *then_target == target {
                        arguments.extend(then_arguments.get(index).copied());
                    }
                    if *else_target == target {
                        arguments.extend(else_arguments.get(index).copied());
                    }
                }
                _ => {}
            }
        }
        arguments.into_iter().any(|argument| self.carries_token_seen(argument, seen))
    }

    /// Give every result lane of a join a token when any lane carries one:
    /// a static string (or a join parameter over static strings) is copied
    /// into owned storage in its own arm, so the release placer sees the
    /// same token shape on every edge. Returns whether the lanes now all
    /// carry tokens.
    fn materialize_mixed_lanes(
        &mut self,
        lanes: &mut [(BlockId, ValueId)],
    ) -> Result<bool, CanonicalLoweringError> {
        if lanes.is_empty() || !matches!(self.values[&lanes[0].1], ValueType::Ref(_)) {
            return Ok(false);
        }
        if !lanes.iter().any(|(_, value)| self.carries_token(*value)) {
            return Ok(false);
        }
        for (block, value) in lanes.iter_mut() {
            if self.carries_token(*value) {
                continue;
            }
            if !self.string_lengths.contains_key(value) {
                return Err(CanonicalLoweringError::UnsupportedExpression("mixed token and static join lanes"));
            }
            self.current = *block;
            *value = self.clone_string_for_storage(*value)?;
        }
        Ok(true)
    }

    /// The dynamic extent lanes a loop threads for a tracked reference: a
    /// list's count and capacity, or a string's length and capacity.
    fn tracked_lanes(&self, object: ValueId) -> Option<(ValueId, ValueId)> {
        if let (Some(count), Some(capacity)) = (self.list_counts.get(&object), self.list_capacities.get(&object)) {
            return Some((*count, *capacity));
        }
        if let (Some(length), Some(capacity)) = (self.string_lengths.get(&object), self.string_capacities.get(&object)) {
            return Some((*length, *capacity));
        }
        None
    }

    /// Extent lanes passed across a CFG edge must be distinct value ids: the
    /// checker renames a branch argument to its parameter in every fact table,
    /// and a value passed twice lands on the last parameter, which would put
    /// the object's count on the capacity lane. A string's capacity is its
    /// length, so its capacity lane is a fresh copy.
    fn split_lanes(&mut self, count: ValueId, capacity: ValueId) -> Result<(ValueId, ValueId), CanonicalLoweringError> {
        if count != capacity {
            return Ok((count, capacity));
        }
        let copy = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::MoveValue { destination: copy, source: capacity });
        Ok((count, copy))
    }

    fn set_tracked_lanes(&mut self, object: ValueId, count: ValueId, capacity: ValueId) {
        let is_string = string_layout(&Type::Named("String".into(), Vec::new()), self.layout_bundle, self.layouts)
            .is_ok_and(|layout| self.values.get(&object) == Some(&ValueType::Ref(layout)));
        if is_string {
            self.string_lengths.insert(object, count);
            self.string_capacities.insert(object, capacity);
        } else {
            self.list_counts.insert(object, count);
            self.list_capacities.insert(object, capacity);
        }
    }

    /// Give a loop header's tracked reference lanes phi authority. A tracked
    /// mutable list is the sole local token of canonical allocation/push
    /// templates (a unique phi rebases the list a prior iteration built). A
    /// string is a unique phi when it owns its token and a shared phi when it
    /// came from a shared source (the checker weakens a fresh concat result to
    /// shared at that phi). Untracked references stay unspecified.
    /// The loop body's rebindings plus every `var` name the body passes to
    /// a write-back parameter: the call mutates that value in place and
    /// hands it back, as a push does, so the lane is planned like a pushed
    /// one (a unique phi when the entry value is a token); a shared value
    /// arriving on the back edge still demotes it to a shared phi.
    fn loop_rebindings_with_write_backs(&self, body: &witchy_syntax::ast::Block) -> LoopRebindings {
        let mut rebound = loop_rebindings(body);
        let _ = witchy_syntax::ast::visit::visit_block::<()>(body, &mut |expression| {
            if let Expr::Call { name, args } = expression
                && let Some((_, signature_id)) = self.functions.get(name.as_str())
                && let Some(signature) = self.signatures.get(signature_id.get() as usize)
            {
                for (argument, contract) in args.iter().zip(&signature.contract.parameters) {
                    if contract.ownership == ParameterOwnership::WriteBack
                        && let Expr::Var(name) = argument
                    {
                        rebound.pushed.insert(name.clone());
                    }
                }
            }
            Ok(())
        });
        rebound
    }

    fn mark_tracked_phi_parameters(
        &mut self,
        header: BlockId,
        names: &[String],
        tracked: &[String],
        outer: &HashMap<String, ValueId>,
        mark_lists: bool,
        rebound: &LoopRebindings,
    ) {
        let string = string_layout(&Type::Named("String".into(), Vec::new()), self.layout_bundle, self.layouts).ok();
        let ownerships = names
            .iter()
            .map(|name| {
                let object = outer[name];
                // A fixed-layout aggregate lane (a record, tuple, Option)
                // carries no extent lanes and is never pushed in place; it
                // follows the general rule below.
                let fixed_layout = matches!(self.values[&object], ValueType::Ref(layout)
                    if self.layouts.get(layout).is_some_and(|descriptor| matches!(descriptor.size(), witchy_wir::layout::LayoutSize::Fixed(_))));
                if !matches!(self.values[&object], ValueType::Ref(_))
                    || (!fixed_layout && !tracked.iter().any(|tracked| tracked == name))
                {
                    return None;
                }
                let is_string = Some(self.values[&object]) == string.map(ValueType::Ref);
                if !is_string && !fixed_layout && mark_lists && !self.known_shared_value(object) {
                    // The `for` loop's rule: a unique phi over the canonical
                    // push templates. A lane holding a shared value (a call
                    // result, a clone) cannot be unique and takes the
                    // general rule below.
                    return Some(Ownership::Unique);
                }
                // Only a name the body rebinds needs a phi. A lane from a
                // shared source is a shared phi. A lane the body mutates in
                // place (a push) must stay unique; one the body merely
                // reassigns is a shared phi, which also accepts the shared
                // join a body-local `if` produces for it. Anything else stays
                // unspecified, so one identity is demanded.
                let pushed = rebound.pushed.contains(name);
                let assigned = rebound.assigned.contains(name);
                if !pushed && !assigned {
                    return None;
                }
                if self.known_shared_value(object) {
                    Some(Ownership::Shared)
                } else if !self.carries_token(object) {
                    None
                } else if pushed {
                    Some(Ownership::Unique)
                } else {
                    Some(Ownership::Shared)
                }
            })
            .collect::<Vec<_>>();
        for (parameter, ownership) in self.blocks[header.get() as usize].parameters.iter_mut().zip(ownerships) {
            if ownership.is_some() {
                parameter.ownership = ownership;
            }
        }
    }

    /// A value may feed a shared phi when it is static data, a shared token,
    /// or a token the checker may weaken to shared: a fresh allocation, a
    /// call result, an owned parameter, or a block parameter every incoming
    /// edge feeds from such a value (a loop exit carrying a rebound list).
    fn shared_phi_eligible(&self, value: ValueId) -> bool {
        self.known_shared_value(value) || self.carries_token(value)
    }

    fn known_shared_value(&self, value: ValueId) -> bool {
        self.known_shared_value_via(value, &mut BTreeSet::new())
    }

    /// A value is known shared when it is a shared phi, a static datum, a
    /// clone, a shared load, or an unspecified block parameter every one of
    /// whose incoming arguments is itself known shared (a body block renames
    /// a shared header lane without restating its ownership).
    fn known_shared_value_via(&self, value: ValueId, visited: &mut BTreeSet<ValueId>) -> bool {
        // A value reached again is on a cycle of block parameters (a loop
        // lane); it is shared exactly when the cycle's entries are, which
        // the enclosing `all` still demands.
        if !visited.insert(value) {
            return true;
        }
        self.blocks.iter().any(|block| {
            block.parameters.iter().any(|parameter| {
                parameter.value == value && parameter.ownership == Some(Ownership::Shared)
            }) || block.operations.iter().any(|operation| match operation {
                Operation::StaticData { destination, .. }
                | Operation::CloneRef { destination, .. } => *destination == value,
                Operation::Load {
                    destination,
                    transfer: witchy_wir::canonical_safewir::LoadTransfer::CloneShared,
                    ..
                } => *destination == value,
                // A call's reference result is a shared token (its result
                // contract), never a unique one.
                Operation::Call { destinations, .. }
                | Operation::VarCall { destinations, .. }
                | Operation::HostCall { destinations, .. } => destinations.contains(&value),
                _ => false,
            })
        }) || self.blocks.iter().any(|block| {
            let Some(index) = block.parameters.iter().position(|parameter| {
                parameter.value == value && parameter.ownership.is_none()
            }) else {
                return false;
            };
            let mut incoming = Vec::new();
            for predecessor in &self.blocks {
                match &predecessor.terminator {
                    Some(Terminator::Branch { target, arguments }) if *target == block.id => {
                        incoming.extend(arguments.get(index).copied());
                    }
                    Some(Terminator::BranchIf { then_target, then_arguments, else_target, else_arguments, .. }) => {
                        if *then_target == block.id {
                            incoming.extend(then_arguments.get(index).copied());
                        }
                        if *else_target == block.id {
                            incoming.extend(else_arguments.get(index).copied());
                        }
                    }
                    _ => {}
                }
            }
            !incoming.is_empty() && incoming.iter().all(|argument| self.known_shared_value_via(*argument, visited))
        })
    }

    fn dynamic_extent_lanes(&self, value: ValueId) -> Option<(ValueId, ValueId)> {
        if let (Some(length), Some(capacity)) = (
            self.string_lengths.get(&value),
            self.string_capacities.get(&value),
        ) {
            return Some((*length, *capacity));
        }
        Some((*self.list_counts.get(&value)?, *self.list_capacities.get(&value)?))
    }

    fn record_joined_extent(
        &mut self,
        result: ValueId,
        count: ValueId,
        capacity: ValueId,
        sources: &[ValueId],
    ) {
        if sources.iter().all(|source| self.string_lengths.contains_key(source)) {
            self.string_lengths.insert(result, count);
            self.string_capacities.insert(result, capacity);
        } else if sources.iter().all(|source| {
            self.list_counts.contains_key(source) && self.list_capacities.contains_key(source)
        }) {
            self.list_counts.insert(result, count);
            self.list_capacities.insert(result, capacity);
        }
    }

    fn lower_direct_tail_call(
        &mut self,
        name: &str,
        arguments: &[Expr],
        path: Vec<ProperTailPathStep>,
    ) -> Result<Terminator, CanonicalLoweringError> {
        let (callee, signature_id) = self
            .functions
            .get(name)
            .copied()
            .ok_or_else(|| CanonicalLoweringError::MissingBinding(name.to_owned()))?;
        let signature = self
            .signatures
            .get(signature_id.get() as usize)
            .cloned()
            .ok_or_else(|| CanonicalLoweringError::MissingCheckedFunctionType(name.to_owned()))?;
        let mut path = path;
        path.push(ProperTailPathStep::Node(ProperTailExprKind::Call));
        let key = ProperTailSiteKey {
            callable: ProperTailCallableId {
                item: self.item,
                method: None,
                lambda_path: self.lambda_path.clone(),
            },
            path,
            dispatch: ProperTailDispatchKind::Direct,
        };
        let site = self
            .tail_manifest
            .sites()
            .iter()
            .find(|site| site.key == key)
            .ok_or_else(|| CanonicalLoweringError::MissingProperTailSite(self.function.clone()))?;
        let arguments = self.lower_call_arguments(name, arguments, signature_id, &signature, true)?;
        Ok(Terminator::TailCall {
            site: witchy_wir::canonical_safewir::TailSiteId::from_digest(site.id.as_bytes()),
            callee,
            arguments,
        })
    }

    fn lower_direct_call(
        &mut self,
        name: &str,
        arguments: &[Expr],
    ) -> Result<Vec<ValueId>, CanonicalLoweringError> {
        let (callee, signature_id) = self
            .functions
            .get(name)
            .copied()
            .ok_or_else(|| CanonicalLoweringError::MissingBinding(name.to_owned()))?;
        let signature = self
            .signatures
            .get(signature_id.get() as usize)
            .cloned()
            .ok_or_else(|| CanonicalLoweringError::MissingCheckedFunctionType(name.to_owned()))?;
        let call_arguments = self.lower_call_arguments(name, arguments, signature_id, &signature, false)?;
        let destinations = signature
            .results
            .iter()
            .copied()
            .map(|value_type| self.fresh(value_type))
            .collect::<Result<Vec<_>, _>>()?;
        self.push_operation(Operation::Call {
            destinations: destinations.clone(),
            callee,
            arguments: call_arguments,
        });
        self.rebind_write_backs(arguments, &signature, &destinations)?;
        Ok(destinations[..destinations.len() - write_back_count(&signature)].to_vec())
    }

    /// A tail call through a closure record: the record's code slot is the
    /// callee, a shared clone of the record travels as the leading
    /// argument, and the source arguments follow under the declaration's
    /// conventions.
    fn lower_indirect_tail_call(
        &mut self,
        callee: ValueId,
        arguments: &[Expr],
        path: Vec<ProperTailPathStep>,
        expression: ProperTailExprKind,
        dispatch: ProperTailDispatchKind,
    ) -> Result<Terminator, CanonicalLoweringError> {
        let ValueType::Ref(layout) = self.values[&callee] else {
            return Err(CanonicalLoweringError::IndirectCalleeType(self.values[&callee]));
        };
        let (Some(&signature_id), Some(&declaration_id)) = (
            self.closures.signature_by_layout.get(&layout),
            self.closures.declaration_by_layout.get(&layout),
        ) else {
            return Err(CanonicalLoweringError::IndirectCalleeType(self.values[&callee]));
        };
        let declaration = self
            .signatures
            .get(declaration_id.get() as usize)
            .cloned()
            .ok_or(CanonicalLoweringError::UnsupportedExpression(
                "indirect tail-call signature",
            ))?;
        if declaration
            .parameters
            .iter()
            .copied()
            .any(|value_type| !is_indirect_result(value_type))
            || declaration
                .results
                .iter()
                .copied()
                .any(|value_type| !is_indirect_result(value_type))
        {
            return Err(CanonicalLoweringError::UnsupportedExpression(
                "memory-bearing indirect tail call",
            ));
        }
        let key = ProperTailSiteKey {
            callable: ProperTailCallableId {
                item: self.item,
                method: None,
                lambda_path: self.lambda_path.clone(),
            },
            path: {
                let mut path = path;
                path.push(ProperTailPathStep::Node(expression));
                path
            },
            dispatch,
        };
        let site = self
            .tail_manifest
            .sites()
            .iter()
            .find(|site| site.key == key)
            .ok_or_else(|| CanonicalLoweringError::MissingProperTailSite(self.function.clone()))?;
        let mut arguments = self.lower_call_arguments("<indirect tail>", arguments, declaration_id, &declaration, true)?;
        let code = self.load_record_field(callee, 0, ValueType::Callable(signature_id))?;
        let environment = self.fresh(ValueType::Ref(layout))?;
        self.push_operation(Operation::CloneRef { destination: environment, source: callee });
        arguments.insert(0, CallArgument::Move(environment));
        Ok(Terminator::VarTailCall {
            site: witchy_wir::canonical_safewir::TailSiteId::from_digest(site.id.as_bytes()),
            callee: code,
            signature: signature_id,
            arguments,
        })
    }

    /// A closure record for `function` with the given `Int` captures in its
    /// slots (the remaining slots zero).
    fn alloc_closure(
        &mut self,
        layout: witchy_wir::layout::LayoutId,
        signature: FunctionSignatureId,
        function: FunctionId,
        captures: Vec<ValueId>,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let descriptor = self
            .layouts
            .get(layout)
            .ok_or(CanonicalLoweringError::MissingLayout(Type::Named("__closure".into(), Vec::new())))?;
        let LayoutSize::Fixed(bytes) = descriptor.size() else {
            return Err(CanonicalLoweringError::UnsupportedExpression("closure layout size"));
        };
        let LayoutKind::PackedRecord { fields } = descriptor.kind() else {
            return Err(CanonicalLoweringError::UnsupportedExpression("closure layout"));
        };
        let slots = fields.len().saturating_sub(1);
        if captures.len() > slots {
            return Err(CanonicalLoweringError::UnsupportedExpression("closure capture count"));
        }
        let object = self.fresh(ValueType::Ref(layout))?;
        self.push_operation(Operation::Alloc {
            destination: object,
            layout,
            extent: witchy_wir::canonical_safewir::Extent::FixedBytes(u64::from(bytes)),
            class: witchy_wir::canonical_safewir::AllocationClass::ReferenceCounted,
        });
        let code = self.fresh(ValueType::Callable(signature))?;
        self.push_operation(Operation::FunctionValue { destination: code, function });
        self.push_operation(Operation::Store {
            place: witchy_wir::canonical_safewir::Place::projected(
                object,
                Projection::Field { index: 0 },
                ValueType::Callable(signature),
                AccessMode::Write,
            ),
            value: code,
            transfer: witchy_wir::canonical_safewir::StoreTransfer::CopyScalar,
        });
        for slot in 0..slots {
            let value = match captures.get(slot) {
                Some(value) => *value,
                None => self.push_const_int(0)?,
            };
            self.push_operation(Operation::Store {
                place: witchy_wir::canonical_safewir::Place::projected(
                    object,
                    Projection::Field { index: u32::try_from(slot + 1).map_err(|_| CanonicalLoweringError::IdOverflow)? },
                    ValueType::Int,
                    AccessMode::Write,
                ),
                value,
                transfer: witchy_wir::canonical_safewir::StoreTransfer::CopyScalar,
            });
        }
        Ok(object)
    }

    /// Call through a closure record: its code slot is the callee, a shared
    /// clone of the record its leading argument, then the source arguments
    /// under the declaration's conventions.
    fn lower_closure_call(
        &mut self,
        closure: ValueId,
        layout: witchy_wir::layout::LayoutId,
        arguments: &[Expr],
    ) -> Result<Vec<ValueId>, CanonicalLoweringError> {
        let signature_id = *self.closures.signature_by_layout.get(&layout).ok_or(
            CanonicalLoweringError::UnsupportedExpression("closure signature"),
        )?;
        let declaration_id = *self.closures.declaration_by_layout.get(&layout).ok_or(
            CanonicalLoweringError::UnsupportedExpression("closure signature"),
        )?;
        let signature = self
            .signatures
            .get(signature_id.get() as usize)
            .cloned()
            .ok_or(CanonicalLoweringError::UnsupportedExpression("closure signature"))?;
        let declaration = self
            .signatures
            .get(declaration_id.get() as usize)
            .cloned()
            .ok_or(CanonicalLoweringError::UnsupportedExpression("closure signature"))?;
        if declaration.parameters.iter().copied().any(|value_type| !is_indirect_result(value_type)) {
            return Err(CanonicalLoweringError::UnsupportedExpression("indirect call parameter outside the v1 value set"));
        }
        if let Some(result) = declaration.results.iter().copied().find(|value_type| !is_indirect_result(*value_type)) {
            return Err(CanonicalLoweringError::UnsupportedIndirectResult(result));
        }
        let mut call_arguments = self.lower_call_arguments("<closure>", arguments, declaration_id, &declaration, false)?;
        let code = self.load_record_field(closure, 0, ValueType::Callable(signature_id))?;
        let environment = self.fresh(ValueType::Ref(layout))?;
        self.push_operation(Operation::CloneRef { destination: environment, source: closure });
        call_arguments.insert(0, CallArgument::Move(environment));
        let destinations = signature
            .results
            .iter()
            .copied()
            .map(|value_type| self.fresh(value_type))
            .collect::<Result<Vec<_>, _>>()?;
        self.push_operation(Operation::VarCall {
            destinations: destinations.clone(),
            callee: code,
            signature: signature_id,
            arguments: call_arguments,
        });
        self.rebind_write_backs(arguments, &declaration, &destinations)?;
        Ok(destinations[..destinations.len() - write_back_count(&declaration)].to_vec())
    }

    fn rebind_write_backs(
        &mut self,
        source_arguments: &[Expr],
        signature: &FunctionSignature,
        destinations: &[ValueId],
    ) -> Result<(), CanonicalLoweringError> {
        let ordinary_results = destinations.len() - write_back_count(signature);
        let mut destination = ordinary_results;
        for (parameter, contract) in signature.contract.parameters.iter().enumerate() {
            if contract.ownership != ParameterOwnership::WriteBack {
                continue;
            }
            let Expr::Var(name) = source_arguments.get(parameter).ok_or_else(|| {
                CanonicalLoweringError::CallArityMismatch {
                    callee: "write-back".to_owned(),
                    expected: signature.contract.parameters.len(),
                    actual: source_arguments.len(),
                }
            })? else {
                return Err(CanonicalLoweringError::UnsupportedExpression(
                    "var write-back argument must be a bound variable",
                ));
            };
            let replacement = *destinations.get(destination).ok_or(
                CanonicalLoweringError::UnsupportedExpression("missing var write-back destination"),
            )?;
            // The replacement is a fresh value: authenticate its extent so a
            // loop header can carry it as a tracked lane.
            self.ensure_string_extent(replacement)?;
            self.ensure_list_extent(replacement)?;
            self.bindings.insert(name.clone(), replacement);
            destination += 1;
        }
        Ok(())
    }

    fn lower_call_arguments(
        &mut self,
        callee: &str,
        arguments: &[Expr],
        signature_id: FunctionSignatureId,
        signature: &FunctionSignature,
        tail: bool,
    ) -> Result<Vec<CallArgument>, CanonicalLoweringError> {
        let parameters = &signature.contract.parameters;
        let conventions = self.checked_parameter_conventions.get(&signature_id)
            .ok_or(CanonicalLoweringError::MissingCheckedCallConventions(signature_id))?;
        if arguments.len() != parameters.len() {
            return Err(CanonicalLoweringError::CallArityMismatch {
                callee: callee.to_owned(),
                expected: parameters.len(),
                actual: arguments.len(),
            });
        }
        if conventions.len() != parameters.len() {
            return Err(CanonicalLoweringError::MissingCheckedCallConventions(signature_id));
        }
        arguments
            .iter()
            .zip(parameters)
            .enumerate()
            .map(|(parameter, (argument, contract))| {
                let value = self.lower_expr(argument)?;
                let actual = self.values[&value];
                if actual != contract.value_type {
                    return Err(CanonicalLoweringError::CallArgumentTypeMismatch {
                        callee: callee.to_owned(),
                        parameter,
                        expected: contract.value_type,
                        actual,
                    });
                }
                match conventions[parameter] {
                    Convention::Own => match contract.ownership {
                        ParameterOwnership::Value => Ok(CallArgument::Value(value)),
                        ParameterOwnership::Shared => {
                        // A string literal has no token to transfer; give the
                        // callee a fresh reference-counted copy instead.
                        if self.is_static_string(value) {
                            let owned = self.clone_string_for_storage(value)?;
                            return Ok(CallArgument::Move(owned));
                        }
                        // The authenticated source `own` convention transfers
                        // this entry token to the callee.  Do not also append a
                        // local DropRef at this function's exit; the central
                        // release placer owns cleanup after the transfer.
                        self.owned_parameters.retain(|owned| *owned != value);
                        Ok(CallArgument::Move(value))
                        }
                        ownership => Err(CanonicalLoweringError::UnsupportedCallOwnership {
                            callee: callee.to_owned(), parameter, ownership,
                        }),
                    },
                    // An explicit `let` (`Borrow`) is the default convention
                    // spelled out: one owned token, cloned before a non-tail
                    // call and moved at a tail edge.
                    Convention::Let | Convention::Borrow => match contract.ownership {
                        ParameterOwnership::Value => Ok(CallArgument::Value(value)),
                        // A string literal cannot be cloned or moved as a token;
                        // the callee receives a fresh reference-counted copy.
                        ParameterOwnership::Shared if self.is_static_string(value) => {
                            let owned = self.clone_string_for_storage(value)?;
                            Ok(CallArgument::Move(owned))
                        }
                        ParameterOwnership::Shared if tail => Ok(CallArgument::Move(value)),
                        // A `var` binding may be mutated in place after the
                        // call; a clone would leave its token shared, so the
                        // callee receives an owned copy (value semantics).
                        ParameterOwnership::Shared
                            if matches!(argument, Expr::Var(name) if self.mutable_bindings.contains(name)) =>
                        {
                            let source_type = self.expression_type(argument)?;
                            let owned = self.owned_for_storage(value, &source_type)?;
                            Ok(CallArgument::Move(owned))
                        }
                        ParameterOwnership::Shared => {
                            let destination = self.fresh(actual)?;
                            self.push_operation(Operation::CloneRef { destination, source: value });
                            Ok(CallArgument::Move(destination))
                        }
                        ownership => Err(CanonicalLoweringError::UnsupportedCallOwnership {
                            callee: callee.to_owned(), parameter, ownership,
                        }),
                    },
                    Convention::Var => {
                        if contract.ownership != ParameterOwnership::WriteBack {
                            return Err(CanonicalLoweringError::UnsupportedCallOwnership {
                                callee: callee.to_owned(), parameter, ownership: contract.ownership,
                            });
                        }
                        let Expr::Var(name) = argument else {
                            return Err(CanonicalLoweringError::UnsupportedExpression(
                                "var write-back argument must be a bound variable",
                            ));
                        };
                        if !self.bindings.contains_key(name) {
                            return Err(CanonicalLoweringError::MissingBinding(name.clone()));
                        }
                        // A scalar `var` argument is a plain value; only a
                        // reference transfers its token to the callee.
                        if !matches!(actual, ValueType::Ref(_)) {
                            return Ok(CallArgument::Value(value));
                        }
                        // The callee receives its own copy (value semantics:
                        // a `let` alias taken before the call is unchanged
                        // after it); the caller's binding is rebound to the
                        // write-back result.
                        let ty = self.expression_type(argument)?;
                        let value = if self.known_shared_value(value) || self.aliased_binding(name, value) {
                            self.owned_for_storage(value, &ty)?
                        } else {
                            self.owned_parameters.retain(|owned| *owned != value);
                            value
                        };
                        Ok(CallArgument::Move(value))
                    }
                }
            })
            .collect()
    }

    fn return_value(
        &self,
        expression: Option<ValueId>,
        results: &[ValueType],
    ) -> Result<Terminator, CanonicalLoweringError> {
        match (expression, results) {
            (None, []) => Ok(Terminator::Return { values: Vec::new() }),
            (Some(value), [expected]) => {
                let actual = self.values[&value];
                if actual != *expected {
                    return Err(CanonicalLoweringError::ReturnTypeMismatch {
                        function: self.function.clone(),
                        expected: *expected,
                        actual,
                    });
                }
                Ok(Terminator::Return { values: vec![value] })
            }
            _ => Err(CanonicalLoweringError::InvalidCheckedFunctionType(self.function.clone())),
        }
    }

    fn lower_expr(&mut self, expression: &Expr) -> Result<ValueId, CanonicalLoweringError> {
        match expression {
            Expr::Int(value) => {
                let destination = self.fresh(ValueType::Int)?;
                self.push_operation(Operation::ConstInt { destination, value: *value });
                Ok(destination)
            }
            Expr::Duration(value) => {
                let destination = self.fresh(ValueType::Int)?;
                self.push_operation(Operation::ConstInt { destination, value: *value });
                Ok(destination)
            }
            Expr::Float(value) => {
                let destination = self.fresh(ValueType::Float)?;
                self.push_operation(Operation::ConstFloat { destination, bits: value.to_bits() });
                Ok(destination)
            }
            Expr::Bool(value) => {
                let destination = self.fresh(ValueType::Bool)?;
                self.push_operation(Operation::ConstBool { destination, value: *value });
                Ok(destination)
            }
            Expr::Var(name) => {
                let value = if let Some(value) = self.bindings.get(name).copied() {
                    value
                } else if let Some((adapter, signature, fn_type)) = self.closures.adapters.get(name).cloned() {
                    // A named function as a value: a closure whose code is
                    // the function's adapter and whose environment is empty.
                    let layout = *self.closures.layouts.get(&fn_type).ok_or_else(|| CanonicalLoweringError::MissingLayout(fn_type.clone()))?;
                    self.alloc_closure(layout, signature, adapter, Vec::new())?
                } else if self.functions.contains_key(name) {
                    return Err(CanonicalLoweringError::UnsupportedExpression("function value without adapter"));
                } else {
                    return Err(CanonicalLoweringError::MissingBinding(name.clone()));
                };
                self.ensure_string_extent(value)?;
                self.ensure_list_extent(value)?;
                Ok(value)
            }
            Expr::Call { name, args } if is_console_print(name) => Err(
                CanonicalLoweringError::UnsupportedExpression("console.print used as a value"),
            ),
            // A `dict.` name that is not a linked function is a dictionary
            // intrinsic; the std wrappers themselves are ordinary calls.
            Expr::Call { name, args } if name.starts_with("dict.") && !self.functions.contains_key(name.as_str()) => {
                self.lower_dict_call(expression, name, args)
            }
            Expr::Call { name, args } if matches!(name.as_str(), "list.length" | "list.__length") => {
                self.lower_list_length(args)
            }
            Expr::Call { name, args } if matches!(name.as_str(), "string.chars" | "string.__chars") => {
                self.lower_string_chars(args)
            }
            // `xs.at(i)` reaches the lowering as the `list.at` intrinsic, the
            // same bounds-checked element read as `xs[i]`.
            Expr::Call { name, args } if matches!(name.as_str(), "string.length" | "string.__length") => {
                let [receiver] = args.as_slice() else {
                    return Err(CanonicalLoweringError::UnsupportedExpression("string.length arity"));
                };
                let object = self.lower_expr(receiver)?;
                self.ensure_string_extent(object)?;
                let length = *self.string_lengths.get(&object).ok_or(
                    CanonicalLoweringError::UnsupportedExpression("string.length receiver"),
                )?;
                let result = self.fresh(ValueType::Int)?;
                self.push_operation(Operation::WidenSize { destination: result, value: length });
                Ok(result)
            }
            Expr::Call { name, args } if matches!(name.as_str(), "string.char_count" | "string.__char_count") => {
                let [receiver] = args.as_slice() else {
                    return Err(CanonicalLoweringError::UnsupportedExpression("string.char_count arity"));
                };
                let object = self.lower_expr(receiver)?;
                self.lower_char_count(object)
            }
            Expr::Call { name, args } if matches!(name.as_str(), "string.substring" | "string.__substring") => {
                let [receiver, start, end] = args.as_slice() else {
                    return Err(CanonicalLoweringError::UnsupportedExpression("string.substring arity"));
                };
                let object = self.lower_expr(receiver)?;
                let start = self.lower_expr(start)?;
                let end = self.lower_expr(end)?;
                if self.values[&start] != ValueType::Int || self.values[&end] != ValueType::Int {
                    return Err(CanonicalLoweringError::UnsupportedExpression("string.substring index type"));
                }
                self.lower_substring(object, start, end)
            }
            Expr::Call { name, args } if matches!(name.as_str(), "math.sqrt" | "math.__sqrt") => {
                let [argument] = args.as_slice() else {
                    return Err(CanonicalLoweringError::UnsupportedExpression("math.sqrt arity"));
                };
                let value = self.lower_expr(argument)?;
                if self.values[&value] != ValueType::Float {
                    return Err(CanonicalLoweringError::UnsupportedExpression("math.sqrt operand type"));
                }
                let destination = self.fresh(ValueType::Float)?;
                self.push_operation(Operation::FloatUnary {
                    destination,
                    operator: witchy_wir::canonical_safewir::FloatUnaryOperator::Sqrt,
                    value,
                });
                Ok(destination)
            }
            Expr::Call { name, args } if matches!(name.as_str(), "math.to_int" | "math.__to_int") => {
                let [argument] = args.as_slice() else {
                    return Err(CanonicalLoweringError::UnsupportedExpression("math.to_int arity"));
                };
                let value = self.lower_expr(argument)?;
                if self.values[&value] != ValueType::Float {
                    return Err(CanonicalLoweringError::UnsupportedExpression("math.to_int operand type"));
                }
                // NaN is a runtime error on every backend: a NaN is the one
                // float unequal to itself, so the conversion traps on it before
                // the saturating truncation would map it to zero.
                let is_number = self.push_compare(ComparisonOperator::Equal, value, value)?;
                self.push_trap_unless(is_number)?;
                let destination = self.fresh(ValueType::Int)?;
                self.push_operation(Operation::FloatUnary {
                    destination,
                    operator: witchy_wir::canonical_safewir::FloatUnaryOperator::TruncateToInt,
                    value,
                });
                Ok(destination)
            }
            Expr::Call { name, args } if matches!(name.as_str(), "math.to_float" | "math.__to_float") => {
                let [argument] = args.as_slice() else {
                    return Err(CanonicalLoweringError::UnsupportedExpression("math.to_float arity"));
                };
                let value = self.lower_expr(argument)?;
                if self.values[&value] != ValueType::Int {
                    return Err(CanonicalLoweringError::UnsupportedExpression("math.to_float operand type"));
                }
                let destination = self.fresh(ValueType::Float)?;
                self.push_operation(Operation::FloatUnary {
                    destination,
                    operator: witchy_wir::canonical_safewir::FloatUnaryOperator::FromInt,
                    value,
                });
                Ok(destination)
            }
            Expr::Call { name, args } if matches!(name.as_str(), "int_to_duration" | "duration_to_int") => {
                // A `Duration` is an `Int` of milliseconds at runtime.
                let [argument] = args.as_slice() else {
                    return Err(CanonicalLoweringError::UnsupportedExpression("duration conversion arity"));
                };
                let value = self.lower_expr(argument)?;
                if self.values[&value] != ValueType::Int {
                    return Err(CanonicalLoweringError::UnsupportedExpression("duration conversion operand type"));
                }
                Ok(value)
            }
            Expr::Call { name, args }
                if matches!(
                    name.as_str(),
                    "string.starts_with" | "string.__starts_with" | "string.ends_with" | "string.__ends_with"
                        | "string.contains" | "string.__contains"
                ) =>
            {
                let [receiver, needle] = args.as_slice() else {
                    return Err(CanonicalLoweringError::UnsupportedExpression("string search arity"));
                };
                let object = self.lower_expr(receiver)?;
                let needle = self.lower_expr(needle)?;
                match name.as_str() {
                    "string.starts_with" | "string.__starts_with" => self.lower_string_affix(object, needle, true),
                    "string.ends_with" | "string.__ends_with" => self.lower_string_affix(object, needle, false),
                    _ => self.lower_string_contains(object, needle),
                }
            }
            Expr::Call { name, args } if matches!(name.as_str(), "string.to_upper" | "string.__to_upper" | "string.to_lower" | "string.__to_lower") => {
                let [receiver] = args.as_slice() else {
                    return Err(CanonicalLoweringError::UnsupportedExpression("string case arity"));
                };
                let object = self.lower_expr(receiver)?;
                self.lower_string_case(object, name.ends_with("to_upper"))
            }
            Expr::Call { name, args } if matches!(name.as_str(), "string.split" | "string.__split") => {
                let [receiver, separator] = args.as_slice() else {
                    return Err(CanonicalLoweringError::UnsupportedExpression("string.split arity"));
                };
                let object = self.lower_expr(receiver)?;
                let separator = self.lower_expr(separator)?;
                self.lower_string_split(object, separator)
            }
            Expr::Call { name, args } if matches!(name.as_str(), "string.replace" | "string.__replace") => {
                let [receiver, from, to] = args.as_slice() else {
                    return Err(CanonicalLoweringError::UnsupportedExpression("string.replace arity"));
                };
                let object = self.lower_expr(receiver)?;
                let from = self.lower_expr(from)?;
                let to = self.lower_expr(to)?;
                self.lower_string_replace(object, from, to)
            }
            Expr::Call { name, args } if matches!(name.as_str(), "string.find" | "string.__find") => {
                let [receiver, needle] = args.as_slice() else {
                    return Err(CanonicalLoweringError::UnsupportedExpression("string.find arity"));
                };
                let object = self.lower_expr(receiver)?;
                let needle = self.lower_expr(needle)?;
                self.lower_string_find(object, needle)
            }
            Expr::Call { name, args } if matches!(name.as_str(), "string.to_int" | "string.__to_int") => {
                let [receiver] = args.as_slice() else {
                    return Err(CanonicalLoweringError::UnsupportedExpression("string.to_int arity"));
                };
                let object = self.lower_expr(receiver)?;
                self.lower_string_to_int(object)
            }
            Expr::Call { name, args } if matches!(name.as_str(), "string.trim" | "string.__trim") => {
                let [receiver] = args.as_slice() else {
                    return Err(CanonicalLoweringError::UnsupportedExpression("string.trim arity"));
                };
                let object = self.lower_expr(receiver)?;
                self.lower_trim(object)
            }
            Expr::Call { name, args } if matches!(name.as_str(), "list.at" | "list.__at") => {
                let [base, index] = args.as_slice() else {
                    return Err(CanonicalLoweringError::UnsupportedExpression("list.at arity"));
                };
                self.lower_index(base, index)
            }
            Expr::Call { name, args } if witchy_syntax::ast::is_render_intrinsic(name) => {
                let [argument] = args.as_slice() else {
                    return Err(CanonicalLoweringError::UnsupportedExpression("render arity"));
                };
                self.lower_render(argument)
            }
            Expr::Call { name, args } => {
                let results = if let Some(callee) = self.bindings.get(name).copied() {
                    {
                        let ValueType::Ref(layout) = self.values[&callee] else {
                            return Err(CanonicalLoweringError::IndirectCalleeType(self.values[&callee]));
                        };
                        self.lower_closure_call(callee, layout, args)?
                    }
                } else {
                    self.lower_direct_call(name, args)?
                };
                match results.as_slice() {
                    [value] => {
                        let value = *value;
                        self.ensure_string_extent(value)?;
                        self.ensure_list_extent(value)?;
                        Ok(value)
                    }
                    [] => Err(CanonicalLoweringError::UnsupportedExpression("void call used as value")),
                    _ => Err(CanonicalLoweringError::UnsupportedExpression("multi-result call")),
                }
            }
            Expr::LabeledCall { .. }
            | Expr::LabeledMethodCall { .. }
            | Expr::MethodCall { .. } => Err(CanonicalLoweringError::UnsupportedExpression("call")),
            Expr::Apply { func, args } => {
                let callee = self.lower_expr(func)?;
                let ValueType::Ref(layout) = self.values[&callee] else {
                    return Err(CanonicalLoweringError::IndirectCalleeType(self.values[&callee]));
                };
                if !self.closures.signature_by_layout.contains_key(&layout) {
                    if std::env::var_os("WITCHY_CANONICAL_LOWER_DEBUG").is_some() {
                        eprintln!("canonical closure call: callee layout {layout:?} is not a closure layout; closure layouts: {:?}", self.closures.layouts);
                    }
                    return Err(CanonicalLoweringError::IndirectCalleeType(self.values[&callee]));
                }
                let results = self.lower_closure_call(callee, layout, args)?;
                match results.as_slice() {
                    [value] => {
                        let value = *value;
                        self.ensure_string_extent(value)?;
                        self.ensure_list_extent(value)?;
                        Ok(value)
                    }
                    [] => Err(CanonicalLoweringError::UnsupportedExpression("void call used as value")),
                    _ => Err(CanonicalLoweringError::UnsupportedExpression("multi-result call")),
                }
            }
            Expr::Str(value) => self.lower_string_literal(value),
            Expr::List(items) => self.lower_list_literal(expression, items),
            Expr::Tuple(items) => {
                let source_type = self.expression_type(expression)?;
                let shape = scalar_aggregate_shape(&source_type, self.definitions)
                    .ok_or(CanonicalLoweringError::UnsupportedExpression("tuple type"))?;
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(self.lower_expr(item)?);
                }
                self.lower_aggregate(&shape, 0, values)
            }
            Expr::Ctor { name, args } => self.lower_record_constructor(expression, name, args),
            Expr::AnonCtor { .. } => {
                Err(CanonicalLoweringError::UnsupportedExpression("constructor"))
            }
            Expr::Unary { op, expr } => self.lower_unary(*op, expr),
            Expr::Binary { op, lhs, rhs } => self.lower_binary(*op, lhs, rhs),
            Expr::Field { base, field } => self.lower_record_field(base, field),
            Expr::Lambda { .. } => {
                // A lambda is a closure record: the code slot holds its
                // synthesized function (bound to its closure witness), the
                // capture slots the current values of its `Int` captures.
                let address = expression as *const Expr as usize;
                let Some(name) = self.lambda_names.get(&address) else {
                    return Err(CanonicalLoweringError::UnsupportedExpression("lambda with non-integer captures"));
                };
                let (function, signature) = *self.functions.get(name).ok_or_else(|| CanonicalLoweringError::MissingBinding(name.clone()))?;
                let fn_type = self.expression_type(expression)?;
                let layout = *self.closures.layouts.get(&crate::canonical_layouts::closure_key(&fn_type)).ok_or_else(|| CanonicalLoweringError::MissingLayout(fn_type.clone()))?;
                let captures = self.closures.lambda_captures.get(&address).cloned().unwrap_or_default();
                let mut values = Vec::with_capacity(captures.len());
                for capture in &captures {
                    let value = *self.bindings.get(capture).ok_or_else(|| CanonicalLoweringError::MissingBinding(capture.clone()))?;
                    if self.values[&value] != ValueType::Int {
                        return Err(CanonicalLoweringError::UnsupportedExpression("closure capture type"));
                    }
                    values.push(value);
                }
                self.alloc_closure(layout, signature, function, values)
            }
            Expr::Record { name, fields, spread } => {
                self.lower_record_literal(expression, name, fields, spread.as_deref())
            }
            Expr::RecordUpdate { base, fields, .. } => {
                let Type::Named(name, _) = self.expression_type(expression)?.unqualified().clone() else {
                    return Err(CanonicalLoweringError::UnsupportedExpression("record update type"));
                };
                self.lower_record_literal(expression, &name, fields, Some(base))
            }
            Expr::Try(inner) => self.lower_try(inner),
            // `as` narrows a capability or ascribes an alias; at the value
            // level both sides share one representation, so the cast is the
            // value itself. A representation change is outside the subset.
            Expr::As { expr, ty } => {
                let value = self.lower_expr(expr)?;
                let target = source_value_type(ty, self.layout_bundle, self.layouts, &HashMap::new(), self.definitions)?;
                if self.values[&value] != target {
                    return Err(CanonicalLoweringError::UnsupportedExpression("as between different representations"));
                }
                Ok(value)
            }
            Expr::ExistentialPack { .. }
            | Expr::ExistentialUpcast { .. }
            | Expr::ExistentialCall { .. } => {
                Err(CanonicalLoweringError::UnsupportedExpression("existential"))
            }
            Expr::If {
                cond,
                then_block,
                else_block: Some(else_block),
            } => self
                .lower_if(cond, then_block, else_block, true)?
                .ok_or(CanonicalLoweringError::UnsupportedExpression("if value")),
            Expr::If { .. } => Err(CanonicalLoweringError::UnsupportedExpression("if without else")),
            Expr::Match { scrutinee, arms } => self
                .lower_match(scrutinee, arms, true, None)?
                .ok_or(CanonicalLoweringError::UnsupportedExpression("match value")),
            Expr::Block(block) => self.lower_block_value(block),
            Expr::While { .. } => Err(CanonicalLoweringError::UnsupportedExpression("while value")),
            Expr::For { .. } => Err(CanonicalLoweringError::UnsupportedExpression("for")),
            Expr::Range { .. } => Err(CanonicalLoweringError::UnsupportedExpression("range")),
            Expr::Index { base, index } => self.lower_index(base, index),
            Expr::WhileLet { .. } => Err(CanonicalLoweringError::UnsupportedExpression("while let")),
            Expr::TaggedLit { .. } => {
                Err(CanonicalLoweringError::UnsupportedExpression("tagged literal"))
            }
        }
    }

    fn expression_type(&self, expression: &Expr) -> Result<Type, CanonicalLoweringError> {
        self.type_table
            .type_of(expression)
            .and_then(witchy_types::typeck::ty_to_ast)
            .ok_or_else(|| CanonicalLoweringError::MissingCheckedExpressionType(format!("{expression:?}")))
    }

    fn lower_record_constructor(
        &mut self,
        expression: &Expr,
        name: &str,
        arguments: &[Expr],
    ) -> Result<ValueId, CanonicalLoweringError> {
        let source_type = self.expression_type(expression)?;
        let shape = scalar_aggregate_shape(&source_type, self.definitions).ok_or(
            CanonicalLoweringError::UnsupportedExpression("non-record constructor"),
        )?;
        if shape.packed {
            return self.lower_packed_record_constructor(expression, name, arguments);
        }
        // A record's one constructor is named after the type; a sum type's
        // constructors are its variants.
        let (index, variant) = shape
            .variant(name)
            .or_else(|| (!shape.tagged).then(|| (0, &shape.variants[0])))
            .ok_or_else(|| CanonicalLoweringError::MissingBinding(name.to_owned()))?;
        if arguments.len() != variant.arity {
            return Err(CanonicalLoweringError::CallArityMismatch {
                callee: name.to_owned(), expected: variant.arity, actual: arguments.len(),
            });
        }
        let mut values = Vec::with_capacity(arguments.len());
        for argument in arguments {
            values.push(self.lower_expr(argument)?);
        }
        self.lower_aggregate(&shape, index, values)
    }

    /// Allocate an immutable reference-counted scalar aggregate holding
    /// variant `index` with `payload`: the tag (for a sum type), the
    /// variant's slots, and a zero in every other slot so the object is
    /// fully initialized.
    fn lower_aggregate(
        &mut self,
        shape: &ScalarAggregateShape,
        index: usize,
        payload: Vec<ValueId>,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let variant = &shape.variants[index];
        let mut values = Vec::with_capacity(shape.fields.len());
        for (slot, field) in shape.fields.iter().enumerate() {
            let value = if shape.tagged && slot == 0 {
                self.push_const_int(i64::try_from(index).map_err(|_| CanonicalLoweringError::IdOverflow)?)?
            } else if slot >= variant.first_field && slot < variant.first_field + variant.arity {
                payload[slot - variant.first_field]
            } else {
                self.push_empty_value(field)?
            };
            values.push(value);
        }
        self.lower_heap_record(&shape.name, &shape.fields, values)
    }

    /// `lower_aggregate` whose reference payloads are fresh owned tokens the
    /// kernel built for this aggregate: they move into their slots.
    fn lower_aggregate_moving(
        &mut self,
        shape: &ScalarAggregateShape,
        index: usize,
        payload: Vec<ValueId>,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let variant = &shape.variants[index];
        let mut values = Vec::with_capacity(shape.fields.len());
        for (slot, field) in shape.fields.iter().enumerate() {
            let value = if shape.tagged && slot == 0 {
                self.push_const_int(i64::try_from(index).map_err(|_| CanonicalLoweringError::IdOverflow)?)?
            } else if slot >= variant.first_field && slot < variant.first_field + variant.arity {
                payload[slot - variant.first_field]
            } else {
                self.push_empty_value(field)?
            };
            values.push(value);
        }
        self.lower_heap_record_with(&shape.name, &shape.fields, values, true)
    }

    fn push_zero(&mut self, value_type: ValueType) -> Result<ValueId, CanonicalLoweringError> {
        let destination = self.fresh(value_type)?;
        match value_type {
            ValueType::Int => self.push_operation(Operation::ConstInt { destination, value: 0 }),
            ValueType::Float => self.push_operation(Operation::ConstFloat { destination, bits: 0 }),
            ValueType::Bool => self.push_operation(Operation::ConstBool { destination, value: false }),
            _ => return Err(CanonicalLoweringError::UnsupportedExpression("aggregate slot type")),
        }
        Ok(destination)
    }

    /// `option ?? default`: the payload when the tag is `Some`, otherwise
    /// the default, evaluated only then.
    fn lower_coalesce(&mut self, left: &Expr, right: &Expr) -> Result<ValueId, CanonicalLoweringError> {
        let left_type = self.expression_type(left)?;
        let shape = scalar_aggregate_shape(&left_type, self.definitions)
            .filter(|shape| shape.name == "Option")
            .ok_or(CanonicalLoweringError::UnsupportedExpression("coalesce operand"))?;
        let object = self.lower_expr(left)?;
        let payload_type = self.field_value_type(&shape.fields[1])?;
        let tag = self.load_record_field(object, 0, ValueType::Int)?;
        let some_tag = self.push_const_int(0)?;
        let is_some = self.push_compare(ComparisonOperator::Equal, tag, some_tag)?;
        let facts = self.snapshot_facts();
        let bindings = self.bindings.clone();
        let when_some = self.new_block(&[])?;
        let when_none = self.new_block(&[])?;
        // A reference payload joins through a shared phi (the payload is a
        // shared clone, the default its own token, which weakens); a string
        // or list phi also carries count and capacity lanes.
        let dynamic_payload = matches!(payload_type, ValueType::Ref(layout)
            if matches!(self.layouts.get(layout).map(|descriptor| descriptor.size()), Some(LayoutSize::Dynamic { .. })));
        let join = if dynamic_payload {
            self.new_block(&[payload_type, ValueType::Size, ValueType::Size])?
        } else {
            self.new_block(&[payload_type])?
        };
        if matches!(payload_type, ValueType::Ref(_)) {
            self.blocks[join.get() as usize].parameters[0].ownership = Some(Ownership::Shared);
        }
        self.finish_current(Terminator::BranchIf {
            condition: is_some,
            then_target: when_some,
            then_arguments: Vec::new(),
            else_target: when_none,
            else_arguments: Vec::new(),
        })?;
        self.current = when_some;
        let payload = self.load_record_field(object, 1, payload_type)?;
        let some_end = self.current;
        self.current = when_none;
        self.bindings = bindings.clone();
        let default = self.lower_expr(right)?;
        if self.values[&default] != payload_type {
            return Err(CanonicalLoweringError::UnsupportedExpression("coalesce default type"));
        }
        if self.bindings != bindings {
            return Err(CanonicalLoweringError::UnsupportedExpression("binding change inside coalesce default"));
        }
        let none_end = self.current;
        // A static default (`?? ""`) joined with the payload's token is
        // materialized so both edges carry a token.
        let mut lanes = [(some_end, payload), (none_end, default)];
        if matches!(payload_type, ValueType::Ref(_)) {
            self.materialize_mixed_lanes(&mut lanes)?;
        }
        let [(_, payload), (_, default)] = lanes;
        let mut some_arguments = vec![payload];
        let mut none_arguments = vec![default];
        if dynamic_payload {
            self.current = some_end;
            self.ensure_string_extent(payload)?;
            self.ensure_list_extent(payload)?;
            let (count, capacity) = self.dynamic_extent_lanes(payload).ok_or(
                CanonicalLoweringError::UnsupportedExpression("coalesce payload extent"),
            )?;
            let (count, capacity) = self.split_lanes(count, capacity)?;
            some_arguments.extend([count, capacity]);
            self.current = none_end;
            self.ensure_string_extent(default)?;
            self.ensure_list_extent(default)?;
            let (count, capacity) = self.dynamic_extent_lanes(default).ok_or(
                CanonicalLoweringError::UnsupportedExpression("coalesce default extent"),
            )?;
            let (count, capacity) = self.split_lanes(count, capacity)?;
            none_arguments.extend([count, capacity]);
        }
        self.current = some_end;
        self.finish_current(Terminator::Branch { target: join, arguments: some_arguments })?;
        self.current = none_end;
        self.finish_current(Terminator::Branch { target: join, arguments: none_arguments })?;
        self.restore_facts(facts, join);
        self.current = join;
        self.bindings = bindings;
        let join_values = self.block_parameter_values(join);
        if dynamic_payload {
            self.record_joined_extent(join_values[0], join_values[1], join_values[2], &[payload, default]);
        }
        Ok(join_values[0])
    }

    /// `value?` on a scalar `Option`/`Result`: continue with the payload, or
    /// return the function's own failure (`None`, or `Err` carrying the
    /// same error) from a terminal block.
    fn lower_try(&mut self, inner: &Expr) -> Result<ValueId, CanonicalLoweringError> {
        let inner_type = self.expression_type(inner)?;
        let shape = scalar_aggregate_shape(&inner_type, self.definitions)
            .filter(|shape| matches!(shape.name.as_str(), "Option" | "Result"))
            .ok_or(CanonicalLoweringError::UnsupportedExpression("try operand"))?;
        let result_type = self.result_type.clone().ok_or(
            CanonicalLoweringError::UnsupportedExpression("try outside a result-returning function"),
        )?;
        let failure_shape = scalar_aggregate_shape(&result_type, self.definitions)
            .filter(|failure| failure.name == shape.name)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("try in a function without a matching result"))?;
        let object = self.lower_expr(inner)?;
        let tag = self.load_record_field(object, 0, ValueType::Int)?;
        let success_tag = self.push_const_int(0)?;
        let is_success = self.push_compare(ComparisonOperator::Equal, tag, success_tag)?;
        let proceed = self.new_block(&[])?;
        let fail = self.new_block(&[])?;
        self.finish_current(Terminator::BranchIf {
            condition: is_success,
            then_target: proceed,
            then_arguments: Vec::new(),
            else_target: fail,
            else_arguments: Vec::new(),
        })?;
        self.current = fail;
        let failure_payload = if shape.name == "Result" {
            let error_type = self.field_value_type(&shape.fields[2])?;
            if error_type != self.field_value_type(&failure_shape.fields[2])? {
                return Err(CanonicalLoweringError::UnsupportedExpression("try error type"));
            }
            vec![self.load_record_field(object, 2, error_type)?]
        } else {
            Vec::new()
        };
        let failure = self.lower_aggregate(&failure_shape, 1, failure_payload)?;
        let results = self.results.clone();
        let terminator = self.return_value(Some(failure), &results)?;
        self.finish_current(terminator)?;
        self.current = proceed;
        let payload_type = self.field_value_type(&shape.fields[1])?;
        self.load_record_field(object, 1, payload_type)
    }

    /// A named-field record literal, with the fields not named taken from the
    /// spread base (`Account(balance: n, ..acc)`).
    fn lower_record_literal(
        &mut self,
        expression: &Expr,
        name: &str,
        fields: &[(String, Expr)],
        spread: Option<&Expr>,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let source_type = self.expression_type(expression)?;
        let shape = scalar_aggregate_shape(&source_type, self.definitions)
            .filter(|shape| !shape.tagged)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("non-record literal"))?;
        if shape.packed {
            return Err(CanonicalLoweringError::UnsupportedExpression("packed record literal"));
        }
        let definition = self.record_definition(name).ok_or(
            CanonicalLoweringError::UnsupportedExpression("non-record literal"),
        )?;
        let field_names = definition.variants[0].field_names.clone();
        let field_types = shape.fields.clone();
        for (given, _) in fields {
            if !field_names.iter().any(|candidate| candidate == given) {
                return Err(CanonicalLoweringError::MissingBinding(given.clone()));
            }
        }
        let base = match spread {
            Some(base) => Some(self.lower_expr(base)?),
            None => None,
        };
        let mut values = Vec::with_capacity(field_names.len());
        for (index, field_name) in field_names.iter().enumerate() {
            if let Some((_, value)) = fields.iter().find(|(given, _)| given == field_name) {
                values.push(self.lower_expr(value)?);
            } else if let Some(base) = base {
                let index = u32::try_from(index).map_err(|_| CanonicalLoweringError::IdOverflow)?;
                let value_type = self.field_value_type(&field_types[index as usize])?;
                values.push(self.load_record_field(base, index, value_type)?);
            } else {
                return Err(CanonicalLoweringError::MissingBinding(field_name.clone()));
            }
        }
        self.lower_heap_record(name, &field_types, values)
    }

    /// Allocate an immutable reference-counted record and store its fields.
    /// The SafeWIR value type of an aggregate field or payload.
    fn field_value_type(&self, ty: &Type) -> Result<ValueType, CanonicalLoweringError> {
        source_value_type(ty, self.layout_bundle, self.layouts, &HashMap::new(), self.definitions)
    }

    /// A reference value as a fresh owned token for an aggregate slot: a
    /// string or scalar list is copied (value semantics; the source keeps
    /// its own token).
    fn owned_for_storage(&mut self, value: ValueId, ty: &Type) -> Result<ValueId, CanonicalLoweringError> {
        match ty.unqualified() {
            Type::Named(name, arguments) if name == "String" && arguments.is_empty() => {
                self.ensure_string_extent(value)?;
                self.clone_string_for_storage(value)
            }
            Type::Named(name, arguments) if name == "List" && arguments.len() == 1 => {
                self.clone_list_value_for_storage(value, ty)
            }
            _ if scalar_aggregate_shape(ty, self.definitions).is_some() => self.clone_aggregate_for_storage(value, ty),
            _ => Err(CanonicalLoweringError::UnsupportedType(ty.clone())),
        }
    }

    /// A fresh copy of an aggregate value: every slot is loaded and stored
    /// again, reference slots through their own copies.
    fn clone_aggregate_for_storage(&mut self, value: ValueId, ty: &Type) -> Result<ValueId, CanonicalLoweringError> {
        let shape = scalar_aggregate_shape(ty, self.definitions)
            .ok_or_else(|| CanonicalLoweringError::UnsupportedType(ty.clone()))?;
        let mut slots = Vec::with_capacity(shape.fields.len());
        for (index, field) in shape.fields.iter().enumerate() {
            let value_type = self.field_value_type(field)?;
            let index = u32::try_from(index).map_err(|_| CanonicalLoweringError::IdOverflow)?;
            slots.push(self.load_record_field(value, index, value_type)?);
        }
        self.lower_heap_record(&shape.name, &shape.fields, slots)
    }

    /// A fresh empty value for an aggregate slot no variant payload fills:
    /// every owned pointer slot must hold an object for the destructor.
    fn push_empty_value(&mut self, ty: &Type) -> Result<ValueId, CanonicalLoweringError> {
        match ty.unqualified() {
            Type::Named(name, arguments) if name == "String" && arguments.is_empty() => {
                let layout = self.string_layout_id()?;
                let zero = self.push_const_size(0)?;
                let object = self.fresh(ValueType::Ref(layout))?;
                self.push_operation(Operation::Alloc {
                    destination: object,
                    layout,
                    extent: witchy_wir::canonical_safewir::Extent::Counted {
                        header_bytes: 4,
                        count: zero,
                        capacity: zero,
                        stride_bytes: 1,
                    },
                    class: witchy_wir::canonical_safewir::AllocationClass::ReferenceCounted,
                });
                self.push_operation(Operation::UpdateCount { object, count: zero });
                self.string_lengths.insert(object, zero);
                self.string_capacities.insert(object, zero);
                Ok(object)
            }
            Type::Named(name, arguments) if name == "List" && arguments.len() == 1 => {
                let (layout, element_type, header_bytes, stride_bytes) =
                    list_layout(ty, self.layout_bundle, self.layouts, self.definitions)?;
                let _ = element_type;
                // Distinct count and capacity ids, as the list literal
                // allocates: a loop lane renames each to its own parameter.
                let zero = self.push_const_size(0)?;
                let capacity = self.push_const_size(0)?;
                let object = self.fresh(ValueType::Ref(layout))?;
                self.push_operation(Operation::Alloc {
                    destination: object,
                    layout,
                    extent: witchy_wir::canonical_safewir::Extent::Counted {
                        header_bytes,
                        count: zero,
                        capacity,
                        stride_bytes,
                    },
                    class: witchy_wir::canonical_safewir::AllocationClass::ReferenceCounted,
                });
                self.list_counts.insert(object, zero);
                self.list_capacities.insert(object, capacity);
                Ok(object)
            }
            _ if scalar_aggregate_shape(ty, self.definitions).is_some() => {
                let shape = scalar_aggregate_shape(ty, self.definitions).expect("checked above");
                let mut slots = Vec::with_capacity(shape.fields.len());
                for field in &shape.fields {
                    slots.push(self.push_empty_value(field)?);
                }
                self.lower_aggregate(&shape, 0, slots)
            }
            _ => self.push_zero(scalar_type(ty)?),
        }
    }

    fn lower_heap_record(
        &mut self,
        name: &str,
        field_types: &[Type],
        values: Vec<ValueId>,
    ) -> Result<ValueId, CanonicalLoweringError> {
        self.lower_heap_record_with(name, field_types, values, false)
    }

    /// `lower_heap_record` for the kernels: `move_references` stores a
    /// reference value's own token into its slot (the value is a fresh
    /// owned object the kernel built for exactly this slot) instead of an
    /// owned copy of it.
    fn lower_heap_record_with(
        &mut self,
        name: &str,
        field_types: &[Type],
        values: Vec<ValueId>,
        move_references: bool,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let layout = aggregate_layout(field_types, self.layout_bundle, self.layouts, self.definitions)?;
        let LayoutSize::Fixed(bytes) = self
            .layouts
            .get(layout)
            .ok_or(CanonicalLoweringError::MissingLayout(Type::Named(name.to_owned(), Vec::new())))?
            .size()
        else {
            return Err(CanonicalLoweringError::UnsupportedExpression("record layout size"));
        };
        let object = self.fresh(ValueType::Ref(layout))?;
        self.push_operation(Operation::Alloc {
            destination: object,
            layout,
            extent: witchy_wir::canonical_safewir::Extent::FixedBytes(u64::from(bytes)),
            class: witchy_wir::canonical_safewir::AllocationClass::ReferenceCounted,
        });
        for (index, (value, field_type)) in values.into_iter().zip(field_types).enumerate() {
            let expected = self.field_value_type(field_type)?;
            let actual = self.values[&value];
            if actual != expected {
                return Err(CanonicalLoweringError::CallArgumentTypeMismatch {
                    callee: name.to_owned(), parameter: index, expected, actual,
                });
            }
            let reference = matches!(expected, ValueType::Ref(_));
            let value = if reference && !move_references { self.owned_for_storage(value, field_type)? } else { value };
            self.push_operation(Operation::Store {
                place: witchy_wir::canonical_safewir::Place::projected(
                    object,
                    Projection::Field { index: u32::try_from(index).map_err(|_| CanonicalLoweringError::IdOverflow)? },
                    expected,
                    AccessMode::Write,
                ),
                value,
                transfer: if reference {
                    witchy_wir::canonical_safewir::StoreTransfer::MoveUnique
                } else {
                    witchy_wir::canonical_safewir::StoreTransfer::CopyScalar
                },
            });
        }
        Ok(object)
    }

    fn load_record_field(
        &mut self,
        object: ValueId,
        index: u32,
        value_type: ValueType,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let destination = self.fresh(value_type)?;
        self.push_operation(Operation::Load {
            destination,
            place: witchy_wir::canonical_safewir::Place::projected(
                object,
                Projection::Field { index },
                value_type,
                AccessMode::Read,
            ),
            // A reference field yields a shared clone; the object keeps its
            // own token.
            transfer: if matches!(value_type, ValueType::Ref(_)) {
                witchy_wir::canonical_safewir::LoadTransfer::CloneShared
            } else {
                witchy_wir::canonical_safewir::LoadTransfer::CopyScalar
            },
        });
        Ok(destination)
    }

    fn lower_record_field(&mut self, base: &Expr, field: &str) -> Result<ValueId, CanonicalLoweringError> {
        let base_type = self.expression_type(base)?;
        if let Type::Tuple(items) = base_type.unqualified() {
            // `pair.1`: the item's slot.
            let index = field.parse::<usize>().ok().filter(|index| *index < items.len())
                .ok_or_else(|| CanonicalLoweringError::MissingBinding(field.to_owned()))?;
            let value_type = self.field_value_type(&items[index])?;
            let object = self.lower_expr(base)?;
            return self.load_record_field(object, u32::try_from(index).map_err(|_| CanonicalLoweringError::IdOverflow)?, value_type);
        }
        let Type::Named(name, _) = base_type.unqualified() else {
            return Err(CanonicalLoweringError::UnsupportedExpression("field base"));
        };
        let definition = self.record_definition(name).ok_or(
            CanonicalLoweringError::UnsupportedExpression("non-record field"),
        )?;
        if definition.packed && definition.variants.len() == 1 {
            return self.lower_inline_aggregate_field(base, field);
        }
        let index = definition.variants[0].field_names.iter().position(|candidate| candidate == field)
            .ok_or_else(|| CanonicalLoweringError::MissingBinding(field.to_owned()))?;
        let value_type = self.field_value_type(&definition.variants[0].fields[index])?;
        let object = self.lower_expr(base)?;
        if !matches!(self.values[&object], ValueType::Ref(_)) {
            return Err(CanonicalLoweringError::UnsupportedExpression("heap record field base"));
        }
        self.load_record_field(object, u32::try_from(index).map_err(|_| CanonicalLoweringError::IdOverflow)?, value_type)
    }

    fn lower_packed_record_constructor(
        &mut self,
        expression: &Expr,
        name: &str,
        arguments: &[Expr],
    ) -> Result<ValueId, CanonicalLoweringError> {
        let definition = self.packed_records.get(name).copied().ok_or(
            CanonicalLoweringError::UnsupportedExpression("non-packed record constructor"),
        )?;
        let fields = &definition.variants[0].fields;
        if arguments.len() != fields.len() {
            return Err(CanonicalLoweringError::CallArityMismatch {
                callee: name.to_owned(), expected: fields.len(), actual: arguments.len(),
            });
        }
        let source_type = self.expression_type(expression)?;
        if !matches!(source_type.unqualified(), Type::Named(checked_name, arguments) if same_record_name(checked_name, name) && arguments.is_empty()) {
            return Err(CanonicalLoweringError::UnsupportedExpression("checked packed record constructor type"));
        }
        let layout = packed_scalar_layout(fields, self.layout_bundle, self.layouts)?;
        let mut values = Vec::with_capacity(arguments.len());
        for (argument, field_type) in arguments.iter().zip(fields) {
            let value = self.lower_expr(argument)?;
            let expected = scalar_type(field_type)?;
            let actual = self.values[&value];
            if actual != expected {
                return Err(CanonicalLoweringError::CallArgumentTypeMismatch {
                    callee: name.to_owned(), parameter: values.len(), expected, actual,
                });
            }
            values.push(value);
        }
        let destination = self.fresh(ValueType::InlineAggregate(layout))?;
        self.push_operation(Operation::MakeInlineAggregate { destination, layout, fields: values });
        Ok(destination)
    }

    fn lower_inline_aggregate_field(
        &mut self,
        base: &Expr,
        field: &str,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let base_type = self.expression_type(base)?;
        let Type::Named(name, _) = base_type.unqualified() else {
            return Err(CanonicalLoweringError::UnsupportedExpression("field base"));
        };
        let definition = self.packed_records.get(name).copied().ok_or(
            CanonicalLoweringError::UnsupportedExpression("non-packed record field"),
        )?;
        let index = definition.variants[0].field_names.iter().position(|candidate| candidate == field)
            .ok_or_else(|| CanonicalLoweringError::MissingBinding(field.to_owned()))?;
        let aggregate = self.lower_expr(base)?;
        let ValueType::InlineAggregate(layout) = self.values[&aggregate] else {
            return Err(CanonicalLoweringError::UnsupportedExpression("inline aggregate field base"));
        };
        let result = self.fresh(scalar_type(&definition.variants[0].fields[index])?)?;
        self.push_operation(Operation::ExtractInlineField {
            destination: result,
            aggregate,
            index: u32::try_from(index).map_err(|_| CanonicalLoweringError::IdOverflow)?,
        });
        Ok(result)
    }

    fn ensure_string_extent(&mut self, value: ValueId) -> Result<(), CanonicalLoweringError> {
        let ValueType::Ref(layout) = self.values[&value] else {
            return Ok(());
        };
        let Some(string_layout) = string_layout(
            &Type::Named("String".into(), Vec::new()),
            self.layout_bundle,
            self.layouts,
        )
        .ok()
        else {
            return Ok(());
        };
        if layout != string_layout || self.string_lengths.contains_key(&value) {
            return Ok(());
        }
        let length = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::Load {
            destination: length,
            place: witchy_wir::canonical_safewir::Place::projected(
                value,
                Projection::Header(witchy_wir::canonical_safewir::HeaderField::Length),
                ValueType::Size,
                AccessMode::Read,
            ),
            transfer: witchy_wir::canonical_safewir::LoadTransfer::CopyScalar,
        });
        let maximum_length = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::ConstSize {
            destination: maximum_length,
            value: u64::from(u32::MAX - 4),
        });
        let length_fits = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::Compare {
            destination: length_fits,
            operator: ComparisonOperator::LessEqualUnsigned,
            left: length,
            right: maximum_length,
        });
        self.push_operation(Operation::Guard {
            condition: length_fits,
            kind: witchy_wir::canonical_safewir::GuardKind::Bounds,
        });
        self.string_lengths.insert(value, length);
        // A byte buffer's authenticated capacity is its count; record it so a
        // join over two such strings has both extent lanes.
        self.string_capacities.insert(value, length);
        Ok(())
    }

    fn ensure_list_extent(&mut self, value: ValueId) -> Result<(), CanonicalLoweringError> {
        let ValueType::Ref(layout) = self.values[&value] else {
            return Ok(());
        };
        if self.list_counts.contains_key(&value) && self.list_capacities.contains_key(&value) {
            return Ok(());
        }
        let descriptor = self
            .layouts
            .get(layout)
            .ok_or(CanonicalLoweringError::MissingLayout(Type::Named("List".into(), Vec::new())))?;
        if !matches!(descriptor.kind(), LayoutKind::PackedList { .. }) {
            return Ok(());
        }
        let (header_bytes, stride_bytes) = match (descriptor.header(), descriptor.size()) {
            (
                HeaderLayout::PackedList { data_offset, .. },
                LayoutSize::Dynamic { stride, .. },
            ) if stride != 0 => (data_offset, stride),
            _ => return Err(CanonicalLoweringError::UnsupportedExpression("list extent layout")),
        };
        let count = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::Load {
            destination: count,
            place: witchy_wir::canonical_safewir::Place::projected(
                value,
                Projection::Header(witchy_wir::canonical_safewir::HeaderField::Length),
                ValueType::Size,
                AccessMode::Read,
            ),
            transfer: witchy_wir::canonical_safewir::LoadTransfer::CopyScalar,
        });
        let capacity = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::Load {
            destination: capacity,
            place: witchy_wir::canonical_safewir::Place::projected(
                value,
                Projection::Header(witchy_wir::canonical_safewir::HeaderField::Capacity),
                ValueType::Size,
                AccessMode::Read,
            ),
            transfer: witchy_wir::canonical_safewir::LoadTransfer::CopyScalar,
        });
        let count_fits = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::Compare {
            destination: count_fits,
            operator: ComparisonOperator::LessEqualUnsigned,
            left: count,
            right: capacity,
        });
        self.push_operation(Operation::Guard {
            condition: count_fits,
            kind: witchy_wir::canonical_safewir::GuardKind::Bounds,
        });
        let maximum = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::ConstSize {
            destination: maximum,
            value: (u64::from(u32::MAX) - u64::from(header_bytes)) / u64::from(stride_bytes),
        });
        let capacity_fits = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::Compare {
            destination: capacity_fits,
            operator: ComparisonOperator::LessEqualUnsigned,
            left: capacity,
            right: maximum,
        });
        self.push_operation(Operation::Guard {
            condition: capacity_fits,
            kind: witchy_wir::canonical_safewir::GuardKind::Arithmetic,
        });
        self.list_counts.insert(value, count);
        self.list_capacities.insert(value, capacity);
        Ok(())
    }

    fn lower_console_print(&mut self, arguments: &[Expr]) -> Result<(), CanonicalLoweringError> {
        let [receiver, message] = arguments else {
            return Err(CanonicalLoweringError::UnsupportedExpression("console.print arguments"));
        };
        if !matches!(self.expression_type(receiver)?.unqualified(), Type::Named(name, args) if name == "Console" && args.is_empty()) {
            return Err(CanonicalLoweringError::UnsupportedExpression("console.print receiver"));
        }
        let object = self.lower_expr(message)?;
        let layout = string_layout(
            &Type::Named("String".into(), Vec::new()),
            self.layout_bundle,
            self.layouts,
        )?;
        if self.values.get(&object).copied() != Some(ValueType::Ref(layout)) {
            return Err(CanonicalLoweringError::UnsupportedExpression("console.print string"));
        }
        let import = self.host_imports.print.ok_or_else(|| {
            CanonicalLoweringError::InvalidAuthenticatedAbiCatalog(
                "console.print has no projected ABI import".into(),
            )
        })?;
        let start = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::ConstSize { destination: start, value: 0 });
        let length = self.string_lengths.get(&object).copied().ok_or(
            CanonicalLoweringError::UnsupportedExpression("console.print dynamic string length"),
        )?;
        self.push_operation(Operation::HostCall {
            destinations: Vec::new(),
            import,
            arguments: vec![HostCallArgument::Buffer {
                place: witchy_wir::canonical_safewir::Place::projected(
                    object,
                    Projection::Slice { start, length },
                    ValueType::Ref(layout),
                    AccessMode::Read,
                ),
                extent: HostBufferExtent::Slice,
            }],
        });
        Ok(())
    }

    fn lower_list_literal(
        &mut self,
        expression: &Expr,
        items: &[Expr],
    ) -> Result<ValueId, CanonicalLoweringError> {
        let logical_type = self.expression_type(expression)?;
        let (layout, element_type, header_bytes, stride_bytes) =
            list_layout(&logical_type, self.layout_bundle, self.layouts, self.definitions)?;
        let count = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::ConstSize {
            destination: count,
            value: u64::try_from(items.len()).map_err(|_| CanonicalLoweringError::IdOverflow)?,
        });
        let capacity = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::ConstSize {
            destination: capacity,
            value: u64::try_from(items.len()).map_err(|_| CanonicalLoweringError::IdOverflow)?,
        });
        let allocation_count = if matches!(element_type, ValueType::Ref(_)) {
            let zero = self.fresh(ValueType::Size)?;
            self.push_operation(Operation::ConstSize { destination: zero, value: 0 });
            zero
        } else {
            count
        };
        let object = self.fresh(ValueType::Ref(layout))?;
        self.push_operation(Operation::Alloc {
            destination: object,
            layout,
            extent: witchy_wir::canonical_safewir::Extent::Counted {
                header_bytes,
                count: allocation_count,
                capacity,
                stride_bytes,
            },
            class: witchy_wir::canonical_safewir::AllocationClass::ReferenceCounted,
        });
        self.list_counts.insert(object, count);
        self.list_capacities.insert(object, capacity);
        self.known_list_lengths.insert(
            object,
            u64::try_from(items.len()).map_err(|_| CanonicalLoweringError::IdOverflow)?,
        );

        for (position, item) in items.iter().enumerate() {
            let mut value = self.lower_expr(item)?;
            if self.values[&value] != element_type {
                return Err(CanonicalLoweringError::UnsupportedListElement(logical_type.clone()));
            }
            if matches!(element_type, ValueType::Ref(_)) {
                if self.string_lengths.contains_key(&value) {
                    value = self.clone_string_for_storage(value)?;
                } else if !matches!(item, Expr::List(_)) {
                    let Type::Named(_, arguments) = logical_type.unqualified() else {
                        return Err(CanonicalLoweringError::UnsupportedListElement(logical_type.clone()));
                    };
                    value = self.owned_for_storage(value, &arguments[0])?;
                }
            }
            let index = self.fresh(ValueType::Size)?;
            self.push_operation(Operation::ConstSize {
                destination: index,
                value: u64::try_from(position).map_err(|_| CanonicalLoweringError::IdOverflow)?,
            });
            let condition = self.fresh(ValueType::Bool)?;
            self.push_operation(Operation::Compare {
                destination: condition,
                operator: ComparisonOperator::LessUnsigned,
                left: index,
                right: capacity,
            });
            self.push_operation(Operation::Guard {
                condition,
                kind: witchy_wir::canonical_safewir::GuardKind::Bounds,
            });
            self.push_operation(Operation::Store {
                place: witchy_wir::canonical_safewir::Place::projected(
                    object,
                    witchy_wir::canonical_safewir::Projection::Element { index },
                    element_type,
                    witchy_wir::canonical_safewir::AccessMode::Write,
                ),
                value,
                transfer: if matches!(element_type, ValueType::Ref(_)) {
                    witchy_wir::canonical_safewir::StoreTransfer::MoveUnique
                } else {
                    witchy_wir::canonical_safewir::StoreTransfer::CopyScalar
                },
            });
        }
        if matches!(element_type, ValueType::Ref(_)) {
            self.push_operation(Operation::UpdateCount { object, count });
        }
        Ok(object)
    }

    fn lower_string_literal(&mut self, value: &str) -> Result<ValueId, CanonicalLoweringError> {
        let source_type = Type::Named("String".into(), Vec::new());
        let layout = string_layout(&source_type, self.layout_bundle, self.layouts)?;
        let payload = value.as_bytes();
        let data = self
            .string_data_ids
            .get(payload)
            .copied()
            .ok_or(CanonicalLoweringError::UnsupportedExpression("unregistered string payload"))?;
        let count = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::ConstSize {
            destination: count,
            value: u64::try_from(payload.len()).map_err(|_| CanonicalLoweringError::IdOverflow)?,
        });
        let capacity = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::ConstSize {
            destination: capacity,
            value: u64::try_from(payload.len()).map_err(|_| CanonicalLoweringError::IdOverflow)?,
        });
        let object = self.fresh(ValueType::Ref(layout))?;
        self.push_operation(Operation::StaticData {
            destination: object,
            layout,
            extent: witchy_wir::canonical_safewir::Extent::Counted {
                header_bytes: 4,
                count,
                capacity,
                stride_bytes: 1,
            },
            data,
        });
        self.string_lengths.insert(object, count);
        self.string_capacities.insert(object, capacity);
        Ok(object)
    }

    /// Materialize a string value into independently owned storage before it
    /// enters an owned pointer slot. Static data and borrowed parameters may be
    /// read, but neither is silently promoted to unique ownership.
    fn clone_string_for_storage(
        &mut self,
        source: ValueId,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let ValueType::Ref(layout) = self.values[&source] else {
            return Err(CanonicalLoweringError::UnsupportedExpression(
                "non-string pointer-list element",
            ));
        };
        let length = *self.string_lengths.get(&source).ok_or(
            CanonicalLoweringError::UnsupportedExpression("string element without byte extent"),
        )?;
        let maximum = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::ConstSize {
            destination: maximum,
            value: u64::from(u32::MAX - 4),
        });
        let fits = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::Compare {
            destination: fits,
            operator: ComparisonOperator::LessEqualUnsigned,
            left: length,
            right: maximum,
        });
        self.push_operation(Operation::Guard {
            condition: fits,
            kind: witchy_wir::canonical_safewir::GuardKind::Arithmetic,
        });
        let object = self.fresh(ValueType::Ref(layout))?;
        self.push_operation(Operation::Alloc {
            destination: object,
            layout,
            extent: witchy_wir::canonical_safewir::Extent::Counted {
                header_bytes: 4,
                count: length,
                capacity: length,
                stride_bytes: 1,
            },
            class: witchy_wir::canonical_safewir::AllocationClass::ReferenceCounted,
        });
        self.push_operation(Operation::UpdateCount { object, count: length });
        let zero = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::ConstSize {
            destination: zero,
            value: 0,
        });
        self.push_string_copy(object, source, zero, length, length, layout)?;
        self.string_lengths.insert(object, length);
        self.string_capacities.insert(object, length);
        Ok(object)
    }

    /// A list stored into another list is copied (value semantics): a fresh
    /// exact-count allocation receives the source's elements through
    /// `CloneRange`. Only scalar-element lists copy this way; a list of
    /// references would need per-element retains the v1 range operation does
    /// not express, and a literal is fresh already and moves.
    fn clone_list_for_storage(
        &mut self,
        source: ValueId,
        source_expression: &Expr,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let source_type = self.expression_type(source_expression)?;
        self.clone_list_value_for_storage(source, &source_type)
    }

    fn clone_list_value_for_storage(
        &mut self,
        source: ValueId,
        source_type: &Type,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let (layout, element_type, header_bytes, stride_bytes) =
            list_layout(source_type, self.layout_bundle, self.layouts, self.definitions)?;
        if self.values.get(&source).copied() != Some(ValueType::Ref(layout)) {
            return Err(CanonicalLoweringError::UnsupportedExpression("nested list element type"));
        }
        self.ensure_list_extent(source)?;
        let (length, _) = self.tracked_lanes(source).ok_or(
            CanonicalLoweringError::UnsupportedExpression("nested list element without count state"),
        )?;
        let maximum = self.push_const_size(
            (u64::from(u32::MAX) - u64::from(header_bytes)) / u64::from(stride_bytes),
        )?;
        let fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, length, maximum)?;
        self.push_guard(fits, witchy_wir::canonical_safewir::GuardKind::Arithmetic);
        // A pointer-element list is allocated empty and grows to the cloned
        // prefix once every element has been retained; a scalar list is
        // allocated at its final count.
        let pointer_elements = matches!(element_type, ValueType::Ref(_));
        let zero = self.push_const_size(0)?;
        let allocation_count = if pointer_elements { zero } else { length };
        let object = self.fresh(ValueType::Ref(layout))?;
        self.push_operation(Operation::Alloc {
            destination: object,
            layout,
            extent: witchy_wir::canonical_safewir::Extent::Counted {
                header_bytes,
                count: allocation_count,
                capacity: length,
                stride_bytes,
            },
            class: witchy_wir::canonical_safewir::AllocationClass::ReferenceCounted,
        });
        let in_bounds = self.push_compare(ComparisonOperator::LessEqualUnsigned, length, length)?;
        self.push_guard(in_bounds, witchy_wir::canonical_safewir::GuardKind::Bounds);
        self.push_operation(Operation::CloneRange {
            destination: witchy_wir::canonical_safewir::Place::projected(
                object,
                Projection::Slice { start: zero, length },
                ValueType::Ref(layout),
                AccessMode::Write,
            ),
            source: witchy_wir::canonical_safewir::Place::projected(
                source,
                Projection::Slice { start: zero, length },
                ValueType::Ref(layout),
                AccessMode::Read,
            ),
            count: length,
        });
        if pointer_elements {
            self.push_operation(Operation::UpdateCount { object, count: length });
        }
        self.list_counts.insert(object, length);
        self.list_capacities.insert(object, length);
        Ok(object)
    }

    /// Lower resolved `list.push(xs, value)` in normal mode. The operation
    /// retains source-level mutation syntax while using copy-correct value
    /// semantics: grow into a fresh allocation, initialize it, then reclaim
    /// the old unique allocation and rebind the source name.
    ///
    fn lower_list_push(&mut self, arguments: &[Expr]) -> Result<(), CanonicalLoweringError> {
        let [Expr::Var(name), value] = arguments else {
            return Err(CanonicalLoweringError::UnsupportedExpression("list.push arguments"));
        };
        let old_object = *self
            .bindings
            .get(name)
            .ok_or_else(|| CanonicalLoweringError::MissingBinding(name.clone()))?;
        let list_type = self.expression_type(&arguments[0])?;
        let (_, element_type, _, _) =
            list_layout(&list_type, self.layout_bundle, self.layouts, self.definitions)?;
        let pointer_elements = matches!(element_type, ValueType::Ref(_));
        let value_expression = value;
        let mut value = self.lower_expr(value_expression)?;
        if self.values[&value] != element_type {
            return Err(CanonicalLoweringError::UnsupportedListElement(list_type.clone()));
        }
        if pointer_elements && self.string_lengths.contains_key(&value) {
            value = self.clone_string_for_storage(value)?;
        } else if pointer_elements && !matches!(value_expression, Expr::List(_)) {
            let Type::Named(_, arguments) = list_type.unqualified() else {
                return Err(CanonicalLoweringError::UnsupportedListElement(list_type.clone()));
            };
            value = self.owned_for_storage(value, &arguments[0])?;
        }

        let new_object = self.push_owned_element(old_object, value, &list_type)?;
        self.bindings.insert(name.clone(), new_object);
        Ok(())
    }

    /// Append an owned element to a list value: a fresh list one longer
    /// than `old_object` holding a clone of its prefix and `value`, with the
    /// old list dropped. The canonical push template; `list.push` and the
    /// list-building kernels (`string.chars`, `string.split`) share it.
    fn push_owned_element(
        &mut self,
        old_object: ValueId,
        value: ValueId,
        list_type: &Type,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let ValueType::Ref(layout) = self.values[&old_object] else {
            return Err(CanonicalLoweringError::UnsupportedExpression("list.push receiver"));
        };
        let (expected_layout, element_type, header_bytes, stride_bytes) =
            list_layout(list_type, self.layout_bundle, self.layouts, self.definitions)?;
        if layout != expected_layout {
            return Err(CanonicalLoweringError::UnsupportedExpression("list.push receiver layout"));
        }
        let pointer_elements = matches!(element_type, ValueType::Ref(_));
        self.ensure_list_extent(old_object)?;
        let old_length = *self
            .list_counts
            .get(&old_object)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("list.push dynamic length"))?;
        let old_capacity = *self
            .list_capacities
            .get(&old_object)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("list.push dynamic capacity"))?;
        let maximum_count = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::ConstSize {
            destination: maximum_count,
            value: (u64::from(u32::MAX) - u64::from(header_bytes))
                / u64::from(stride_bytes),
        });
        let old_count_fits = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::Compare {
            destination: old_count_fits,
            operator: ComparisonOperator::LessEqualUnsigned,
            left: old_length,
            right: old_capacity,
        });
        self.push_operation(Operation::Guard {
            condition: old_count_fits,
            kind: witchy_wir::canonical_safewir::GuardKind::Bounds,
        });
        let increment_fits = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::Compare {
            destination: increment_fits,
            operator: ComparisonOperator::LessUnsigned,
            left: old_length,
            right: maximum_count,
        });
        self.push_operation(Operation::Guard {
            condition: increment_fits,
            kind: witchy_wir::canonical_safewir::GuardKind::Arithmetic,
        });
        let one = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::ConstSize { destination: one, value: 1 });
        let new_length = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::Arithmetic {
            destination: new_length,
            operator: ArithmeticOperator::Add,
            left: old_length,
            right: one,
            overflow: OverflowBehavior::Wrap,
        });
        let capacity = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::Arithmetic {
            destination: capacity,
            operator: ArithmeticOperator::Add,
            left: old_length,
            right: one,
            overflow: OverflowBehavior::Wrap,
        });
        let capacity_fits = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::Compare {
            destination: capacity_fits,
            operator: ComparisonOperator::LessEqualUnsigned,
            left: capacity,
            right: maximum_count,
        });
        self.push_operation(Operation::Guard {
            condition: capacity_fits,
            kind: witchy_wir::canonical_safewir::GuardKind::Arithmetic,
        });
        let initial_count_fits = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::Compare {
            destination: initial_count_fits,
            operator: ComparisonOperator::LessEqualUnsigned,
            left: old_length,
            right: capacity,
        });
        self.push_operation(Operation::Guard {
            condition: initial_count_fits,
            kind: witchy_wir::canonical_safewir::GuardKind::Bounds,
        });
        let allocation_count = if pointer_elements {
            let zero = self.fresh(ValueType::Size)?;
            self.push_operation(Operation::ConstSize { destination: zero, value: 0 });
            zero
        } else {
            old_length
        };
        if pointer_elements {
            let allocation_count_fits = self.fresh(ValueType::Bool)?;
            self.push_operation(Operation::Compare {
                destination: allocation_count_fits,
                operator: ComparisonOperator::LessEqualUnsigned,
                left: allocation_count,
                right: capacity,
            });
            self.push_operation(Operation::Guard {
                condition: allocation_count_fits,
                kind: witchy_wir::canonical_safewir::GuardKind::Bounds,
            });
        }
        let new_object = self.fresh(ValueType::Ref(layout))?;
        self.push_operation(Operation::Alloc {
            destination: new_object,
            layout,
            extent: witchy_wir::canonical_safewir::Extent::Counted {
                header_bytes,
                count: allocation_count,
                capacity,
                stride_bytes,
            },
            class: witchy_wir::canonical_safewir::AllocationClass::ReferenceCounted,
        });

        if pointer_elements {
            let zero = self.fresh(ValueType::Size)?;
            self.push_operation(Operation::ConstSize { destination: zero, value: 0 });
            let move_fits = self.fresh(ValueType::Bool)?;
            self.push_operation(Operation::Compare {
                destination: move_fits,
                operator: ComparisonOperator::LessEqualUnsigned,
                left: old_length,
                right: capacity,
            });
            self.push_operation(Operation::Guard {
                condition: move_fits,
                kind: witchy_wir::canonical_safewir::GuardKind::Bounds,
            });
            // The grown list clones the old prefix (retaining every element)
            // rather than moving it out: the old list may be shared with a
            // callee or a stored copy, and its own drop releases the elements.
            let _ = element_type;
            self.push_operation(Operation::CloneRange {
                destination: witchy_wir::canonical_safewir::Place::projected(
                    new_object,
                    Projection::Slice { start: zero, length: old_length },
                    ValueType::Ref(layout),
                    AccessMode::Write,
                ),
                source: witchy_wir::canonical_safewir::Place::projected(
                    old_object,
                    Projection::Slice { start: zero, length: old_length },
                    ValueType::Ref(layout),
                    AccessMode::Read,
                ),
                count: old_length,
            });
        } else {
            let zero = self.fresh(ValueType::Size)?;
            self.push_operation(Operation::ConstSize { destination: zero, value: 0 });
            self.push_operation(Operation::CloneRange {
                destination: witchy_wir::canonical_safewir::Place::projected(
                    new_object,
                    Projection::Slice { start: zero, length: old_length },
                    ValueType::Ref(layout),
                    AccessMode::Write,
                ),
                source: witchy_wir::canonical_safewir::Place::projected(
                    old_object,
                    Projection::Slice { start: zero, length: old_length },
                    ValueType::Ref(layout),
                    AccessMode::Read,
                ),
                count: old_length,
            });
        }

        let append_index = old_length;
        let append_in_bounds = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::Compare {
            destination: append_in_bounds,
            operator: ComparisonOperator::LessUnsigned,
            left: append_index,
            right: capacity,
        });
        self.push_operation(Operation::Guard {
            condition: append_in_bounds,
            kind: witchy_wir::canonical_safewir::GuardKind::Bounds,
        });
        self.push_operation(Operation::Store {
            place: witchy_wir::canonical_safewir::Place::projected(
                new_object,
                witchy_wir::canonical_safewir::Projection::Element { index: append_index },
                element_type,
                witchy_wir::canonical_safewir::AccessMode::Write,
            ),
            value,
            transfer: if pointer_elements {
                witchy_wir::canonical_safewir::StoreTransfer::MoveUnique
            } else {
                witchy_wir::canonical_safewir::StoreTransfer::CopyScalar
            },
        });
        let new_count_fits = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::Compare {
            destination: new_count_fits,
            operator: ComparisonOperator::LessEqualUnsigned,
            left: new_length,
            right: capacity,
        });
        self.push_operation(Operation::Guard {
            condition: new_count_fits,
            kind: witchy_wir::canonical_safewir::GuardKind::Bounds,
        });
        self.push_operation(Operation::UpdateCount { object: new_object, count: new_length });
        self.push_operation(Operation::DropRef { object: old_object });
        self.list_counts.insert(new_object, new_length);
        self.list_capacities.insert(new_object, capacity);
        self.known_list_lengths.remove(&new_object);
        Ok(new_object)
    }

    fn lower_list_length(&mut self, arguments: &[Expr]) -> Result<ValueId, CanonicalLoweringError> {
        let [receiver] = arguments else {
            return Err(CanonicalLoweringError::UnsupportedExpression("list.length arguments"));
        };
        let object = self.lower_expr(receiver)?;
        self.ensure_list_extent(object)?;
        let length = *self
            .list_counts
            .get(&object)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("list.length receiver"))?;
        let result = self.fresh(ValueType::Int)?;
        self.push_operation(Operation::WidenSize { destination: result, value: length });
        Ok(result)
    }

    /// Decode a compile-time UTF-8 string into its Unicode scalar strings.
    ///
    /// This is intentionally the first, narrow `string.chars` slice: the
    /// source literal is already a valid Rust `str`, so `chars()` performs the
    /// language-mandated scalar decoding before any bytes enter SafeWIR. Each
    /// result is then materialized through the ordinary canonical owned-string
    /// and pointer-list templates. Dynamic strings remain rejected until the
    /// canonical kernel has an explicit checked UTF-8 decoder operation.
    fn lower_string_chars(&mut self, arguments: &[Expr]) -> Result<ValueId, CanonicalLoweringError> {
        let [Expr::Str(source)] = arguments else {
            // A runtime string decodes in a canonical loop over its bytes.
            let [receiver] = arguments else {
                return Err(CanonicalLoweringError::UnsupportedExpression("string.chars arity"));
            };
            let object = self.lower_expr(receiver)?;
            return self.lower_string_chars_dynamic(object);
        };
        let list_type = Type::Named(
            "List".into(),
            vec![Type::Named("String".into(), Vec::new())],
        );
        let (layout, element_type, header_bytes, stride_bytes) =
            list_layout(&list_type, self.layout_bundle, self.layouts, self.definitions)?;
        let characters = source.chars().collect::<Vec<_>>();
        let count = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::ConstSize {
            destination: count,
            value: u64::try_from(characters.len()).map_err(|_| CanonicalLoweringError::IdOverflow)?,
        });
        let capacity = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::ConstSize {
            destination: capacity,
            value: u64::try_from(characters.len()).map_err(|_| CanonicalLoweringError::IdOverflow)?,
        });
        let zero = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::ConstSize { destination: zero, value: 0 });
        let object = self.fresh(ValueType::Ref(layout))?;
        self.push_operation(Operation::Alloc {
            destination: object,
            layout,
            extent: witchy_wir::canonical_safewir::Extent::Counted {
                header_bytes,
                count: zero,
                capacity,
                stride_bytes,
            },
            class: witchy_wir::canonical_safewir::AllocationClass::ReferenceCounted,
        });
        self.list_counts.insert(object, count);
        self.list_capacities.insert(object, capacity);
        self.known_list_lengths.insert(
            object,
            u64::try_from(characters.len()).map_err(|_| CanonicalLoweringError::IdOverflow)?,
        );

        for (position, character) in characters.iter().enumerate() {
            let source = self.lower_string_literal(&character.to_string())?;
            let value = self.clone_string_for_storage(source)?;
            if self.values[&value] != element_type {
                return Err(CanonicalLoweringError::UnsupportedListElement(list_type.clone()));
            }
            let index = self.fresh(ValueType::Size)?;
            self.push_operation(Operation::ConstSize {
                destination: index,
                value: u64::try_from(position).map_err(|_| CanonicalLoweringError::IdOverflow)?,
            });
            let in_capacity = self.fresh(ValueType::Bool)?;
            self.push_operation(Operation::Compare {
                destination: in_capacity,
                operator: ComparisonOperator::LessUnsigned,
                left: index,
                right: capacity,
            });
            self.push_operation(Operation::Guard {
                condition: in_capacity,
                kind: witchy_wir::canonical_safewir::GuardKind::Bounds,
            });
            self.push_operation(Operation::Store {
                place: witchy_wir::canonical_safewir::Place::projected(
                    object,
                    witchy_wir::canonical_safewir::Projection::Element { index },
                    element_type,
                    witchy_wir::canonical_safewir::AccessMode::Write,
                ),
                value,
                transfer: witchy_wir::canonical_safewir::StoreTransfer::MoveUnique,
            });
        }
        self.push_operation(Operation::UpdateCount { object, count });
        Ok(object)
    }

    fn lower_index(&mut self, base: &Expr, index: &Expr) -> Result<ValueId, CanonicalLoweringError> {
        let logical_type = self.expression_type(base)?;
        let (layout, element_type, _header_bytes, _stride_bytes) =
            list_layout(&logical_type, self.layout_bundle, self.layouts, self.definitions)?;
        let object = self.lower_expr(base)?;
        if self.values[&object] != ValueType::Ref(layout) {
            return Err(CanonicalLoweringError::UnsupportedExpression("list object type"));
        }
        let index_value = self.lower_expr(index)?;
        if self.values[&index_value] != ValueType::Int {
            return Err(CanonicalLoweringError::UnsupportedExpression("list index type"));
        }
        let index_size = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::NarrowSize { destination: index_size, value: index_value });
        // `lower_expr(base)` has already authenticated the list header and
        // recorded the exact count definition related to its capacity. Do not
        // reload the header here: a new definition would discard that relation.
        // An element of a nested list has no recorded extent yet; load it once.
        self.ensure_list_extent(object)?;
        self.load_list_element(object, index_size, element_type)
    }

    /// Load the element at `index_size` of `object` behind a bounds guard
    /// against the recorded count; a reference element is a shared clone.
    fn load_list_element(
        &mut self,
        object: ValueId,
        index_size: ValueId,
        element_type: ValueType,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let logical_count = *self.list_counts.get(&object).ok_or(
            CanonicalLoweringError::UnsupportedExpression("list index extent"),
        )?;
        let condition = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::Compare {
            destination: condition,
            operator: ComparisonOperator::LessUnsigned,
            left: index_size,
            right: logical_count,
        });
        self.push_operation(Operation::Guard {
            condition,
            kind: witchy_wir::canonical_safewir::GuardKind::Bounds,
        });
        let destination = self.fresh(element_type)?;
        self.push_operation(Operation::Load {
            destination,
            place: witchy_wir::canonical_safewir::Place::projected(
                object,
                witchy_wir::canonical_safewir::Projection::Element { index: index_size },
                element_type,
                witchy_wir::canonical_safewir::AccessMode::Read,
            ),
            transfer: if matches!(element_type, ValueType::Ref(_)) {
                witchy_wir::canonical_safewir::LoadTransfer::CloneShared
            } else {
                witchy_wir::canonical_safewir::LoadTransfer::CopyScalar
            },
        });
        // Only a string element carries a byte extent to authenticate here;
        // an aggregate or nested-list element records nothing, its extent
        // (if any) is loaded lazily at the first access that needs it.
        let string = string_layout(&Type::Named("String".into(), Vec::new()), self.layout_bundle, self.layouts).ok();
        if let ValueType::Ref(element_layout) = element_type && Some(element_layout) == string {
            let length = self.fresh(ValueType::Size)?;
            self.push_operation(Operation::Load {
                destination: length,
                place: witchy_wir::canonical_safewir::Place::projected(
                    destination,
                    Projection::Header(witchy_wir::canonical_safewir::HeaderField::Length),
                    ValueType::Size,
                    AccessMode::Read,
                ),
                transfer: witchy_wir::canonical_safewir::LoadTransfer::CopyScalar,
            });
            let maximum_length = self.fresh(ValueType::Size)?;
            self.push_operation(Operation::ConstSize {
                destination: maximum_length,
                value: u64::from(u32::MAX - 4),
            });
            let length_fits = self.fresh(ValueType::Bool)?;
            self.push_operation(Operation::Compare {
                destination: length_fits,
                operator: ComparisonOperator::LessEqualUnsigned,
                left: length,
                right: maximum_length,
            });
            self.push_operation(Operation::Guard {
                condition: length_fits,
                kind: witchy_wir::canonical_safewir::GuardKind::Bounds,
            });
            debug_assert_eq!(self.values[&destination], ValueType::Ref(element_layout));
            self.string_lengths.insert(destination, length);
        }
        Ok(destination)
    }

    fn fresh(&mut self, value_type: ValueType) -> Result<ValueId, CanonicalLoweringError> {
        let value = ValueId::new(self.next_value);
        self.next_value = self
            .next_value
            .checked_add(1)
            .ok_or(CanonicalLoweringError::IdOverflow)?;
        self.values.insert(value, value_type);
        Ok(value)
    }

    fn lower_unary(
        &mut self,
        operator: UnOp,
        operand: &Expr,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let operand = self.lower_expr(operand)?;
        let value_type = self.values[&operand];
        match (operator, value_type) {
            (UnOp::Neg, ValueType::Int) => {
                let zero = self.fresh(ValueType::Int)?;
                self.push_operation(Operation::ConstInt { destination: zero, value: 0 });
                let destination = self.fresh(ValueType::Int)?;
                self.push_operation(Operation::Arithmetic {
                    destination,
                    operator: ArithmeticOperator::Subtract,
                    left: zero,
                    right: operand,
                    overflow: OverflowBehavior::Wrap,
                });
                Ok(destination)
            }
            (UnOp::Neg, ValueType::Float) => {
                let zero = self.fresh(ValueType::Float)?;
                self.push_operation(Operation::ConstFloat {
                    destination: zero,
                    bits: 0.0f64.to_bits(),
                });
                let destination = self.fresh(ValueType::Float)?;
                self.push_operation(Operation::Arithmetic {
                    destination,
                    operator: ArithmeticOperator::Subtract,
                    left: zero,
                    right: operand,
                    overflow: OverflowBehavior::Wrap,
                });
                Ok(destination)
            }
            (UnOp::Not, ValueType::Bool) => {
                let false_value = self.fresh(ValueType::Bool)?;
                self.push_operation(Operation::ConstBool {
                    destination: false_value,
                    value: false,
                });
                let destination = self.fresh(ValueType::Bool)?;
                self.push_operation(Operation::Compare {
                    destination,
                    operator: ComparisonOperator::Equal,
                    left: operand,
                    right: false_value,
                });
                Ok(destination)
            }
            (UnOp::BitNot, ValueType::Int) => {
                let mask = self.fresh(ValueType::Int)?;
                self.push_operation(Operation::ConstInt {
                    destination: mask,
                    value: -1,
                });
                let destination = self.fresh(ValueType::Int)?;
                self.push_operation(Operation::Arithmetic {
                    destination,
                    operator: ArithmeticOperator::BitXor,
                    left: operand,
                    right: mask,
                    overflow: OverflowBehavior::Wrap,
                });
                Ok(destination)
            }
            (UnOp::Neg | UnOp::Not | UnOp::BitNot, _) => {
                Err(CanonicalLoweringError::UnsupportedExpression("unary operand type"))
            }
            (UnOp::Borrow, _) => {
                Err(CanonicalLoweringError::UnsupportedExpression("shared borrow"))
            }
            (UnOp::BorrowMut, _) => {
                Err(CanonicalLoweringError::UnsupportedExpression("mutable borrow"))
            }
            (UnOp::Deref, _) => Err(CanonicalLoweringError::UnsupportedExpression("dereference")),
            (UnOp::Move, _) => {
                Err(CanonicalLoweringError::UnsupportedExpression("explicit move"))
            }
            (UnOp::Await, _) => Err(CanonicalLoweringError::UnsupportedExpression("await")),
        }
    }

    /// `a && b` / `a || b` as explicit control flow: the right operand is
    /// evaluated only when the left does not decide the result, and the two
    /// arms meet in a join block whose single `Bool` parameter is the value.
    /// No binding can change inside an operand, so the join carries nothing
    /// else.
    fn lower_short_circuit(
        &mut self,
        operator: BinOp,
        left: &Expr,
        right: &Expr,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let left = self.lower_expr(left)?;
        let facts = self.snapshot_facts();
        if self.values[&left] != ValueType::Bool {
            return Err(CanonicalLoweringError::UnsupportedExpression("non-boolean short-circuit operand"));
        }
        let bindings = self.bindings.clone();
        let evaluate_right = self.new_block(&[])?;
        let decided = self.new_block(&[])?;
        let join = self.new_block(&[ValueType::Bool])?;
        let result = self.blocks[join.get() as usize].parameters[0].value;
        // `&&`: a false left decides false, a true left evaluates the right.
        // `||`: a true left decides true, a false left evaluates the right.
        let (then_target, else_target) = match operator {
            BinOp::And => (evaluate_right, decided),
            BinOp::Or => (decided, evaluate_right),
            _ => unreachable!("only `&&` and `||` short-circuit"),
        };
        self.finish_current(Terminator::BranchIf {
            condition: left,
            then_target,
            then_arguments: Vec::new(),
            else_target,
            else_arguments: Vec::new(),
        })?;

        self.current = decided;
        let decided_value = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::ConstBool {
            destination: decided_value,
            value: operator == BinOp::Or,
        });
        self.finish_current(Terminator::Branch { target: join, arguments: vec![decided_value] })?;

        self.current = evaluate_right;
        self.bindings = bindings.clone();
        let right = self.lower_expr(right)?;
        if self.values[&right] != ValueType::Bool {
            return Err(CanonicalLoweringError::UnsupportedExpression("non-boolean short-circuit operand"));
        }
        if self.bindings != bindings {
            return Err(CanonicalLoweringError::UnsupportedExpression("binding change inside short-circuit operand"));
        }
        self.finish_current(Terminator::Branch { target: join, arguments: vec![right] })?;

        self.restore_facts(facts, join);
        self.current = join;
        self.bindings = bindings;
        Ok(result)
    }

    fn lower_binary(
        &mut self,
        operator: BinOp,
        left: &Expr,
        right: &Expr,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let string_operands = matches!(
            self.expression_type(left)?.unqualified(),
            Type::Named(name, arguments) if name == "String" && arguments.is_empty()
        ) && matches!(
            self.expression_type(right)?.unqualified(),
            Type::Named(name, arguments) if name == "String" && arguments.is_empty()
        );
        if operator == BinOp::Concat || (operator == BinOp::Add && string_operands) {
            return self.lower_string_concat(left, right);
        }
        if matches!(operator, BinOp::And | BinOp::Or) {
            return self.lower_short_circuit(operator, left, right);
        }
        if operator == BinOp::Coalesce {
            return self.lower_coalesce(left, right);
        }
        let left = self.lower_expr(left)?;
        let right = self.lower_expr(right)?;
        let left_type = self.values[&left];
        let right_type = self.values[&right];
        if left_type != right_type {
            return Err(CanonicalLoweringError::UnsupportedExpression("mixed-type binary operation"));
        }
        let comparison = match operator {
            BinOp::Eq => Some(ComparisonOperator::Equal),
            BinOp::NotEq => Some(ComparisonOperator::NotEqual),
            BinOp::Lt => Some(ComparisonOperator::LessSigned),
            BinOp::LtEq => Some(ComparisonOperator::LessEqualSigned),
            BinOp::Gt => Some(ComparisonOperator::GreaterSigned),
            BinOp::GtEq => Some(ComparisonOperator::GreaterEqualSigned),
            _ => None,
        };
        if let Some(operator) = comparison {
            if string_operands && matches!(left_type, ValueType::Ref(_)) {
                return match operator {
                    ComparisonOperator::Equal => self.lower_string_equality(left, right, false),
                    ComparisonOperator::NotEqual => self.lower_string_equality(left, right, true),
                    operator => {
                        // Lexicographic byte order is code-point order for
                        // UTF-8: the sign of the first differing byte, else
                        // of the length difference.
                        let order = self.lower_string_order(left, right)?;
                        let zero = self.push_const_int(0)?;
                        self.push_compare(operator, order, zero)
                    }
                };
            }
            if !matches!(left_type, ValueType::Int | ValueType::Bool | ValueType::Float) {
                return Err(CanonicalLoweringError::UnsupportedExpression("comparison type"));
            }
            if left_type == ValueType::Bool
                && !matches!(operator, ComparisonOperator::Equal | ComparisonOperator::NotEqual)
            {
                return Err(CanonicalLoweringError::UnsupportedExpression("ordered boolean comparison"));
            }
            let destination = self.fresh(ValueType::Bool)?;
            self.push_operation(Operation::Compare { destination, operator, left, right });
            return Ok(destination);
        }

        if left_type == ValueType::Float {
            let operator = match operator {
                BinOp::Add => ArithmeticOperator::Add,
                BinOp::Sub => ArithmeticOperator::Subtract,
                BinOp::Mul => ArithmeticOperator::Multiply,
                BinOp::Div => ArithmeticOperator::DivideSigned,
                _ => return Err(CanonicalLoweringError::UnsupportedExpression("float operator")),
            };
            let destination = self.fresh(ValueType::Float)?;
            self.push_operation(Operation::Arithmetic {
                destination,
                operator,
                left,
                right,
                overflow: OverflowBehavior::Wrap,
            });
            return Ok(destination);
        }
        if left_type != ValueType::Int {
            return Err(CanonicalLoweringError::UnsupportedExpression("non-integer arithmetic"));
        }
        let (operator, overflow) = match operator {
            BinOp::Add => (ArithmeticOperator::Add, OverflowBehavior::Wrap),
            BinOp::Sub => (ArithmeticOperator::Subtract, OverflowBehavior::Wrap),
            BinOp::Mul => (ArithmeticOperator::Multiply, OverflowBehavior::Wrap),
            BinOp::Div => (ArithmeticOperator::DivideSigned, OverflowBehavior::Trap),
            BinOp::Mod => (ArithmeticOperator::RemainderSigned, OverflowBehavior::Trap),
            BinOp::BitAnd => (ArithmeticOperator::BitAnd, OverflowBehavior::Wrap),
            BinOp::BitOr => (ArithmeticOperator::BitOr, OverflowBehavior::Wrap),
            BinOp::BitXor => (ArithmeticOperator::BitXor, OverflowBehavior::Wrap),
            BinOp::Shl => (ArithmeticOperator::ShiftLeft, OverflowBehavior::Wrap),
            BinOp::Shr => (ArithmeticOperator::ShiftRightSigned, OverflowBehavior::Wrap),
            BinOp::Concat | BinOp::And | BinOp::Or | BinOp::Coalesce => unreachable!(),
            BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                unreachable!()
            }
        };
        let destination = self.fresh(ValueType::Int)?;
        self.push_operation(Operation::Arithmetic {
            destination,
            operator,
            left,
            right,
            overflow,
        });
        Ok(destination)
    }

    /// Concatenate UTF-8 byte strings into a fresh, uniquely-owned buffer.
    /// Dynamic operands remain fail-closed until symbolic slice ranges are
    /// available to the canonical checker.
    fn lower_string_concat(
        &mut self,
        left: &Expr,
        right: &Expr,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let left = self.lower_expr(left)?;
        let right = self.lower_expr(right)?;
        self.concat_string_values(left, right)
    }

    /// A fresh string holding `left` followed by `right`; neither operand
    /// is consumed.
    fn concat_string_values(
        &mut self,
        left: ValueId,
        right: ValueId,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let layout = string_layout(
            &Type::Named("String".into(), Vec::new()),
            self.layout_bundle,
            self.layouts,
        )?;
        if self.values.get(&left).copied() != Some(ValueType::Ref(layout))
            || self.values.get(&right).copied() != Some(ValueType::Ref(layout))
        {
            return Err(CanonicalLoweringError::UnsupportedExpression(
                "string concatenation operands",
            ));
        }
        let left_length = self.string_length_of(left)?;
        let right_length = self.string_length_of(right)?;

        let maximum = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::ConstSize {
            destination: maximum,
            value: u64::from(u32::MAX - 4),
        });
        let right_fits = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::Compare {
            destination: right_fits,
            operator: ComparisonOperator::LessEqualUnsigned,
            left: right_length,
            right: maximum,
        });
        self.push_operation(Operation::Guard {
            condition: right_fits,
            kind: witchy_wir::canonical_safewir::GuardKind::Arithmetic,
        });
        let remaining = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::Arithmetic {
            destination: remaining,
            operator: ArithmeticOperator::Subtract,
            left: maximum,
            right: right_length,
            overflow: OverflowBehavior::Wrap,
        });
        let left_fits = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::Compare {
            destination: left_fits,
            operator: ComparisonOperator::LessEqualUnsigned,
            left: left_length,
            right: remaining,
        });
        self.push_operation(Operation::Guard {
            condition: left_fits,
            kind: witchy_wir::canonical_safewir::GuardKind::Arithmetic,
        });
        let total = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::Arithmetic {
            destination: total,
            operator: ArithmeticOperator::Add,
            left: left_length,
            right: right_length,
            overflow: OverflowBehavior::Trap,
        });
        let total_fits = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::Compare {
            destination: total_fits,
            operator: ComparisonOperator::LessEqualUnsigned,
            left: total,
            right: maximum,
        });
        self.push_operation(Operation::Guard {
            condition: total_fits,
            kind: witchy_wir::canonical_safewir::GuardKind::Bounds,
        });

        let object = self.fresh(ValueType::Ref(layout))?;
        self.push_operation(Operation::Alloc {
            destination: object,
            layout,
            extent: witchy_wir::canonical_safewir::Extent::Counted {
                header_bytes: 4,
                count: total,
                capacity: total,
                stride_bytes: 1,
            },
            class: witchy_wir::canonical_safewir::AllocationClass::ReferenceCounted,
        });
        self.push_operation(Operation::UpdateCount { object, count: total });
        let zero = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::ConstSize { destination: zero, value: 0 });
        self.push_string_copy(object, left, zero, left_length, total, layout)?;
        self.push_string_copy(object, right, left_length, right_length, total, layout)?;
        self.string_lengths.insert(object, total);
        self.string_capacities.insert(object, total);
        Ok(object)
    }

    fn push_string_copy(
        &mut self,
        destination: ValueId,
        source: ValueId,
        start: ValueId,
        length: ValueId,
        total: ValueId,
        layout: witchy_wir::layout::LayoutId,
    ) -> Result<(), CanonicalLoweringError> {
        let in_bounds = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::Compare {
            destination: in_bounds,
            operator: ComparisonOperator::LessEqualUnsigned,
            left: length,
            right: total,
        });
        self.push_operation(Operation::Guard {
            condition: in_bounds,
            kind: witchy_wir::canonical_safewir::GuardKind::Bounds,
        });
        let destination_place = witchy_wir::canonical_safewir::Place::projected(
            destination,
            Projection::Slice { start, length },
            ValueType::Ref(layout),
            AccessMode::Write,
        );
        let source_start = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::ConstSize { destination: source_start, value: 0 });
        let source_place = witchy_wir::canonical_safewir::Place::projected(
            source,
            Projection::Slice { start: source_start, length },
            ValueType::Ref(layout),
            AccessMode::Read,
        );
        self.push_operation(Operation::CopyBytes {
            destination: destination_place,
            source: source_place,
            length,
        });
        Ok(())
    }

    /// `"${e}"` desugars to the render intrinsic. The compact subset renders a
    /// `String` as itself and an `Int` as decimal digits; every other operand
    /// type is refused by name so the census attributes the gap to rendering
    /// rather than to the call path.
    fn lower_render(&mut self, argument: &Expr) -> Result<ValueId, CanonicalLoweringError> {
        let source_type = self.expression_type(argument)?;
        let value = self.lower_expr(argument)?;
        self.lower_render_typed(value, &source_type)
    }

    /// Render `value` of checked type `ty`: a `String` is itself, an `Int`
    /// its decimal digits, a `Bool` its word; an aggregate renders
    /// structurally as the interpreter does (`Some(1)`, `Circle(2)`,
    /// `Point(1, 2)`, `(1, 2)`, `[1, 2]`, a payload-free variant by name).
    fn lower_render_typed(&mut self, value: ValueId, ty: &Type) -> Result<ValueId, CanonicalLoweringError> {
        let ty = ty.unqualified();
        if let Type::Named(name, arguments) = ty {
            match (name.as_str(), arguments.as_slice(), self.values[&value]) {
                ("String", [], ValueType::Ref(_)) => {
                    self.ensure_string_extent(value)?;
                    return Ok(value);
                }
                ("Int", [], ValueType::Int) => return self.lower_render_int(value),
                ("Bool", [], ValueType::Bool) => return self.lower_render_bool(value),
                ("Float", [], ValueType::Float) => return self.lower_render_float(value),
                ("Duration", _, _) => return Err(CanonicalLoweringError::UnsupportedExpression("render Duration")),
                ("List", [element], ValueType::Ref(_)) => return self.lower_render_list(value, ty, element),
                ("Dict", _, _) => return Err(CanonicalLoweringError::UnsupportedExpression("render Dict")),
                _ => {}
            }
        }
        if !matches!(self.values[&value], ValueType::Ref(_)) {
            return Err(CanonicalLoweringError::UnsupportedExpression("render type"));
        }
        let Some(shape) = scalar_aggregate_shape(ty, self.definitions) else {
            return Err(CanonicalLoweringError::UnsupportedExpression("render type"));
        };
        let tuple = matches!(ty, Type::Tuple(_));
        if !shape.tagged {
            let variant = shape.variants.first().cloned().ok_or(CanonicalLoweringError::UnsupportedExpression("render type"))?;
            return self.lower_render_variant(value, &shape, &variant, tuple);
        }
        // A sum dispatches on its tag: each variant's arm renders its own
        // payload into a fresh string, and the arms join on string lanes.
        let layout = self.string_layout_id()?;
        let tag = self.load_record_field(value, 0, ValueType::Int)?;
        let facts = self.snapshot_facts();
        let bindings = self.bindings.clone();
        // Dispatch and arm blocks first, the join last: every edge runs
        // forward.
        let variants = shape.variants.len();
        let mut arms = Vec::with_capacity(variants);
        let mut nexts = Vec::with_capacity(variants);
        for index in 0..variants {
            arms.push(self.new_block(&[])?);
            if index + 1 < variants {
                nexts.push(self.new_block(&[])?);
            }
        }
        let join = self.new_block_owned(&[(ValueType::Ref(layout), Some(Ownership::Unique)), (ValueType::Size, None), (ValueType::Size, None)])?;
        for (index, variant) in shape.variants.iter().enumerate() {
            let arm = arms[index];
            let next = nexts.get(index).copied();
            match next {
                None => self.finish_current(Terminator::Branch { target: arm, arguments: Vec::new() })?,
                Some(next) => {
                    let expected = self.push_const_int(i64::try_from(index).map_err(|_| CanonicalLoweringError::IdOverflow)?)?;
                    let matched = self.push_compare(ComparisonOperator::Equal, tag, expected)?;
                    self.finish_current(Terminator::BranchIf {
                        condition: matched,
                        then_target: arm,
                        then_arguments: Vec::new(),
                        else_target: next,
                        else_arguments: Vec::new(),
                    })?;
                }
            }
            self.current = arm;
            self.restore_snapshot(facts.clone());
            let rendered = self.lower_render_variant(value, &shape, variant, false)?;
            let (lanes, _) = self.accumulator_lanes(rendered)?;
            self.finish_current(Terminator::Branch { target: join, arguments: lanes })?;
            if let Some(next) = next {
                self.current = next;
                self.restore_snapshot(facts.clone());
            }
        }
        self.restore_facts(facts, join);
        self.current = join;
        self.bindings = bindings;
        let join_values = self.block_parameter_values(join);
        self.track_accumulator_lanes(&join_values);
        Ok(join_values[0])
    }

    /// A dictionary operand: its value, its canonical shape (the tuple of
    /// the key list and the value list), and its key and value types.
    fn lower_dict_operand(&mut self, operand: &Expr) -> Result<(ValueId, ScalarAggregateShape, Type, Type), CanonicalLoweringError> {
        let ty = self.expression_type(operand)?;
        let (shape, key, value) = self.dictionary_parts(&ty)?;
        let dict = self.lower_expr(operand)?;
        if !matches!(self.values[&dict], ValueType::Ref(_)) {
            return Err(CanonicalLoweringError::UnsupportedExpression("dict operand"));
        }
        Ok((dict, shape, key, value))
    }

    fn dictionary_parts(&self, ty: &Type) -> Result<(ScalarAggregateShape, Type, Type), CanonicalLoweringError> {
        let Type::Named(name, arguments) = ty.unqualified() else {
            return Err(CanonicalLoweringError::UnsupportedExpression("dict type"));
        };
        if name != "Dict" || arguments.len() != 2 {
            return Err(CanonicalLoweringError::UnsupportedExpression("dict type"));
        }
        let shape = scalar_aggregate_shape(ty, self.definitions)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("dict type"))?;
        Ok((shape, arguments[0].clone(), arguments[1].clone()))
    }

    /// One of the dictionary's two lists (0: keys, 1: values), a shared clone.
    fn dict_list(&mut self, dict: ValueId, shape: &ScalarAggregateShape, slot: u32) -> Result<ValueId, CanonicalLoweringError> {
        let value_type = self.field_value_type(&shape.fields[slot as usize])?;
        let list = self.load_record_field(dict, slot, value_type)?;
        self.ensure_list_extent(list)?;
        Ok(list)
    }

    /// Whether `ty` is a key the dictionary kernels compare: an `Int` or a
    /// `String`.
    fn dict_key_supported(ty: &Type) -> bool {
        matches!(ty.unqualified(), Type::Named(name, arguments) if arguments.is_empty() && matches!(name.as_str(), "Int" | "String"))
    }

    /// Whether `ty` is a value the dictionary kernels store in place: a
    /// scalar in its slot.
    fn dict_scalar_value(ty: &Type) -> bool {
        scalar_type(ty).is_ok()
    }

    fn is_string_type(ty: &Type) -> bool {
        matches!(ty.unqualified(), Type::Named(name, arguments) if name == "String" && arguments.is_empty())
    }

    /// Search `keys` for `key` in insertion order: `(found, index)`.
    fn lower_dict_find(&mut self, keys: ValueId, key: ValueId, key_type: &Type) -> Result<(ValueId, ValueId), CanonicalLoweringError> {
        if !Self::dict_key_supported(key_type) {
            return Err(CanonicalLoweringError::UnsupportedExpression("dict key type"));
        }
        let list_type = Type::Named("List".into(), vec![key_type.clone()]);
        let (_, element_type, _, _) = list_layout(&list_type, self.layout_bundle, self.layouts, self.definitions)?;
        self.ensure_list_extent(keys)?;
        let count = *self.list_counts.get(&keys).ok_or(CanonicalLoweringError::UnsupportedExpression("dict keys extent"))?;
        let zero = self.push_const_size(0)?;
        let one = self.push_const_size(1)?;
        let missing = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::ConstBool { destination: missing, value: false });
        let present = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::ConstBool { destination: present, value: true });
        let header = self.new_block(&[ValueType::Size])?;
        let body = self.new_block(&[ValueType::Size])?;
        let exit = self.new_block(&[ValueType::Bool, ValueType::Size])?;
        self.finish_current(Terminator::Branch { target: header, arguments: vec![zero] })?;
        self.current = header;
        let cursor = self.block_parameter_values(header)[0];
        let more = self.push_compare(ComparisonOperator::LessUnsigned, cursor, count)?;
        self.finish_current(Terminator::BranchIf {
            condition: more,
            then_target: body,
            then_arguments: vec![cursor],
            else_target: exit,
            else_arguments: vec![missing, cursor],
        })?;
        self.current = body;
        let cursor = self.block_parameter_values(body)[0];
        let element = self.load_list_element(keys, cursor, element_type)?;
        let matched = if Self::is_string_type(key_type) {
            self.lower_string_equality(element, key, false)?
        } else {
            self.push_compare(ComparisonOperator::Equal, element, key)?
        };
        let advance = self.new_block(&[])?;
        self.finish_current(Terminator::BranchIf {
            condition: matched,
            then_target: exit,
            then_arguments: vec![present, cursor],
            else_target: advance,
            else_arguments: Vec::new(),
        })?;
        self.current = advance;
        let next = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, cursor, one, OverflowBehavior::Wrap)?;
        self.finish_current(Terminator::Branch { target: header, arguments: vec![next] })?;
        self.current = exit;
        let exit_values = self.block_parameter_values(exit);
        Ok((exit_values[0], exit_values[1]))
    }

    /// The value at `index` of the dictionary's value list as an owned value
    /// (a scalar as itself, a string as a fresh copy).
    fn lower_dict_value_at(&mut self, values: ValueId, index: ValueId, value_type: &Type) -> Result<ValueId, CanonicalLoweringError> {
        let list_type = Type::Named("List".into(), vec![value_type.clone()]);
        let (_, element_type, _, _) = list_layout(&list_type, self.layout_bundle, self.layouts, self.definitions)?;
        self.ensure_list_extent(values)?;
        let count = *self.list_counts.get(&values).ok_or(CanonicalLoweringError::UnsupportedExpression("dict values extent"))?;
        let in_bounds = self.push_compare(ComparisonOperator::LessUnsigned, index, count)?;
        self.push_guard(in_bounds, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let element = self.load_list_element(values, index, element_type)?;
        self.owned_dict_value(element, value_type)
    }

    fn owned_dict_value(&mut self, value: ValueId, value_type: &Type) -> Result<ValueId, CanonicalLoweringError> {
        if Self::is_string_type(value_type) {
            self.ensure_string_extent(value)?;
            self.owned_string_copy(value)
        } else if Self::dict_scalar_value(value_type) {
            Ok(value)
        } else {
            Err(CanonicalLoweringError::UnsupportedExpression("dict value type"))
        }
    }

    /// Join two arms that each produce an owned dictionary value: string
    /// lanes for a string, one parameter for a scalar. Returns the join
    /// block and a closure-free protocol: callers branch to it with
    /// `dict_value_lanes`.
    fn dict_value_join(&mut self, value_type: &Type) -> Result<BlockId, CanonicalLoweringError> {
        if Self::is_string_type(value_type) {
            let layout = self.string_layout_id()?;
            self.new_block_owned(&[(ValueType::Ref(layout), Some(Ownership::Unique)), (ValueType::Size, None), (ValueType::Size, None)])
        } else {
            let value_type = scalar_type(value_type)?;
            self.new_block(&[value_type])
        }
    }

    fn dict_value_lanes(&mut self, value: ValueId, value_type: &Type) -> Result<Vec<ValueId>, CanonicalLoweringError> {
        if Self::is_string_type(value_type) {
            Ok(self.accumulator_lanes(value)?.0)
        } else {
            Ok(vec![value])
        }
    }

    /// The dictionary intrinsics over the canonical representation. Keys
    /// are `Int` or `String`; values a scalar or a `String` for reads and a
    /// scalar for in-place writes; everything else is refused by name.
    fn lower_dict_call(&mut self, expression: &Expr, name: &str, args: &[Expr]) -> Result<ValueId, CanonicalLoweringError> {
        match name {
            "dict.new" => {
                let ty = self.expression_type(expression)?;
                let (shape, key, value) = self.dictionary_parts(&ty)?;
                let keys = self.push_empty_value(&Type::Named("List".into(), vec![key]))?;
                let values = self.push_empty_value(&Type::Named("List".into(), vec![value]))?;
                self.lower_aggregate_moving(&shape, 0, vec![keys, values])
            }
            "dict.length" => {
                let [operand] = args else {
                    return Err(CanonicalLoweringError::UnsupportedExpression("dict.length arguments"));
                };
                let (dict, shape, _, _) = self.lower_dict_operand(operand)?;
                let keys = self.dict_list(dict, &shape, 0)?;
                let count = *self.list_counts.get(&keys).ok_or(CanonicalLoweringError::UnsupportedExpression("dict keys extent"))?;
                let length = self.fresh(ValueType::Int)?;
                self.push_operation(Operation::WidenSize { destination: length, value: count });
                Ok(length)
            }
            "dict.contains_key" => {
                let [operand, key] = args else {
                    return Err(CanonicalLoweringError::UnsupportedExpression("dict.contains_key arguments"));
                };
                let (dict, shape, key_type, _) = self.lower_dict_operand(operand)?;
                let key = self.lower_expr(key)?;
                let keys = self.dict_list(dict, &shape, 0)?;
                let (found, _) = self.lower_dict_find(keys, key, &key_type)?;
                Ok(found)
            }
            "dict.at" => {
                let [operand, key] = args else {
                    return Err(CanonicalLoweringError::UnsupportedExpression("dict.at arguments"));
                };
                let (dict, shape, key_type, value_type) = self.lower_dict_operand(operand)?;
                let key = self.lower_expr(key)?;
                let keys = self.dict_list(dict, &shape, 0)?;
                let (found, index) = self.lower_dict_find(keys, key, &key_type)?;
                self.push_trap_unless(found)?;
                let values = self.dict_list(dict, &shape, 1)?;
                self.lower_dict_value_at(values, index, &value_type)
            }
            "dict.get_or" => {
                let [operand, key, default] = args else {
                    return Err(CanonicalLoweringError::UnsupportedExpression("dict.get_or arguments"));
                };
                let (dict, shape, key_type, value_type) = self.lower_dict_operand(operand)?;
                let key = self.lower_expr(key)?;
                let default = self.lower_expr(default)?;
                let keys = self.dict_list(dict, &shape, 0)?;
                let values = self.dict_list(dict, &shape, 1)?;
                let (found, index) = self.lower_dict_find(keys, key, &key_type)?;
                let facts = self.snapshot_facts();
                let bindings = self.bindings.clone();
                let hit = self.new_block(&[])?;
                let miss = self.new_block(&[])?;
                let join = self.dict_value_join(&value_type)?;
                self.finish_current(Terminator::BranchIf {
                    condition: found,
                    then_target: hit,
                    then_arguments: Vec::new(),
                    else_target: miss,
                    else_arguments: Vec::new(),
                })?;
                self.current = hit;
                let present = self.lower_dict_value_at(values, index, &value_type)?;
                let lanes = self.dict_value_lanes(present, &value_type)?;
                self.finish_current(Terminator::Branch { target: join, arguments: lanes })?;
                self.current = miss;
                self.restore_snapshot(facts.clone());
                let absent = self.owned_dict_value(default, &value_type)?;
                let lanes = self.dict_value_lanes(absent, &value_type)?;
                self.finish_current(Terminator::Branch { target: join, arguments: lanes })?;
                self.restore_facts(facts, join);
                self.current = join;
                self.bindings = bindings;
                let join_values = self.block_parameter_values(join);
                self.track_accumulator_lanes(&join_values);
                Ok(join_values[0])
            }
            "dict.keys" | "dict.values" => {
                let [operand] = args else {
                    return Err(CanonicalLoweringError::UnsupportedExpression("dict list arguments"));
                };
                let (dict, shape, key_type, value_type) = self.lower_dict_operand(operand)?;
                let (slot, element) = if name == "dict.keys" { (0, key_type) } else { (1, value_type) };
                let list = self.dict_list(dict, &shape, slot)?;
                self.clone_list_value_for_storage(list, &Type::Named("List".into(), vec![element]))
            }
            "dict.pairs" => {
                let [operand] = args else {
                    return Err(CanonicalLoweringError::UnsupportedExpression("dict.pairs arguments"));
                };
                let (dict, shape, key_type, value_type) = self.lower_dict_operand(operand)?;
                let keys = self.dict_list(dict, &shape, 0)?;
                let values = self.dict_list(dict, &shape, 1)?;
                self.lower_dict_pairs(keys, values, &key_type, &value_type)
            }
            "dict.__insert_extract" => {
                let [Expr::Var(target), key, value] = args else {
                    return Err(CanonicalLoweringError::UnsupportedExpression("dict.insert arguments"));
                };
                let (dict, shape, key_type, value_type) = self.lower_dict_operand(&args[0])?;
                let key = self.lower_expr(key)?;
                let value = self.lower_expr(value)?;
                let (rebuilt, extracted) = self.lower_dict_insert(dict, &shape, &key_type, &value_type, key, value, true)?;
                self.bindings.insert(target.clone(), rebuilt);
                extracted.ok_or(CanonicalLoweringError::UnsupportedExpression("dict insert result"))
            }
            // The pure form: the dictionary with the entry, as a value.
            "dict.__insert" => {
                let [operand, key, value] = args else {
                    return Err(CanonicalLoweringError::UnsupportedExpression("dict.insert arguments"));
                };
                let (dict, shape, key_type, value_type) = self.lower_dict_operand(operand)?;
                let key = self.lower_expr(key)?;
                let value = self.lower_expr(value)?;
                let (rebuilt, _) = self.lower_dict_insert(dict, &shape, &key_type, &value_type, key, value, false)?;
                Ok(rebuilt)
            }
            _ => Err(CanonicalLoweringError::MissingBinding(name.to_owned())),
        }
    }

    /// Insert `key -> value`: fresh copies of both lists carry the write
    /// (the old dictionary is untouched, value semantics); a present key
    /// has its value slot overwritten and the old scalar comes back as
    /// `Some(old)`, an absent key grows both lists and yields `None`.
    /// Returns the rebuilt dictionary and, when `extract` asks for it, the
    /// extracted option (its `Option(v)` layout exists only in a program
    /// that names it, which the pure insert form does not).
    fn lower_dict_insert(
        &mut self,
        dict: ValueId,
        shape: &ScalarAggregateShape,
        key_type: &Type,
        value_type: &Type,
        key: ValueId,
        value: ValueId,
        extract: bool,
    ) -> Result<(ValueId, Option<ValueId>), CanonicalLoweringError> {
        if !Self::dict_scalar_value(value_type) {
            return Err(CanonicalLoweringError::UnsupportedExpression("dict value type"));
        }
        let key_type = key_type.clone();
        let value_type = value_type.clone();
        let key_list = Type::Named("List".into(), vec![key_type.clone()]);
        let value_list = Type::Named("List".into(), vec![value_type.clone()]);
        let keys = self.dict_list(dict, shape, 0)?;
        let values = self.dict_list(dict, shape, 1)?;
        let keys = self.clone_list_value_for_storage(keys, &key_list)?;
        let values = self.clone_list_value_for_storage(values, &value_list)?;
                let (found, index) = self.lower_dict_find(keys, key, &key_type)?;
        let option_type = Type::Named("Option".into(), vec![value_type.clone()]);
        let option_shape = extract
            .then(|| scalar_aggregate_shape(&option_type, self.definitions))
            .flatten();
        let option_layout = if extract { Some(self.field_value_type(&option_type)?) } else { None };
        let (_, element_type, _, _) = list_layout(&value_list, self.layout_bundle, self.layouts, self.definitions)?;
        let dict_layout = self.values[&dict];
        let facts = self.snapshot_facts();
        let bindings = self.bindings.clone();
        // Each arm finishes its own dictionary; the join carries the
        // finished tuple (and the extracted option), never the two lists,
        // whose prefix facts do not survive a join.
        let hit = self.new_block(&[])?;
        let miss = self.new_block(&[])?;
        let mut join_shapes = vec![(dict_layout, Some(Ownership::Unique))];
        if let Some(option_layout) = option_layout {
            join_shapes.push((option_layout, Some(Ownership::Unique)));
        }
        let join = self.new_block_owned(&join_shapes)?;
        self.finish_current(Terminator::BranchIf {
            condition: found,
            then_target: hit,
            then_arguments: Vec::new(),
            else_target: miss,
            else_arguments: Vec::new(),
        })?;
        // Hit: the value slot is overwritten in the fresh copy and the old
        // scalar comes back as `Some(old)`.
        self.current = hit;
        let count = *self.list_counts.get(&values).ok_or(CanonicalLoweringError::UnsupportedExpression("dict values extent"))?;
        let in_bounds = self.push_compare(ComparisonOperator::LessUnsigned, index, count)?;
        self.push_guard(in_bounds, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let old = self.load_list_element(values, index, element_type)?;
        self.push_operation(Operation::Store {
            place: witchy_wir::canonical_safewir::Place::projected(
                values,
                Projection::Element { index },
                element_type,
                AccessMode::Write,
            ),
            value,
            transfer: witchy_wir::canonical_safewir::StoreTransfer::CopyScalar,
        });
        let rebuilt = self.lower_aggregate_moving(shape, 0, vec![keys, values])?;
        let mut lanes = vec![rebuilt];
        if let Some(option_shape) = &option_shape {
            lanes.push(self.lower_aggregate(option_shape, 0, vec![old])?);
        }
        self.finish_current(Terminator::Branch { target: join, arguments: lanes })?;
        // Miss: both lists grow by the new entry; `None` comes back.
        self.current = miss;
        self.restore_snapshot(facts.clone());
        let key = if Self::is_string_type(&key_type) { self.owned_dict_value(key, &key_type)? } else { key };
        let grown_keys = self.push_owned_element(keys, key, &key_list)?;
        let grown_values = self.push_owned_element(values, value, &value_list)?;
        let rebuilt = self.lower_aggregate_moving(shape, 0, vec![grown_keys, grown_values])?;
        let mut lanes = vec![rebuilt];
        if let Some(option_shape) = &option_shape {
            lanes.push(self.lower_aggregate(option_shape, 1, Vec::new())?);
        }
        self.finish_current(Terminator::Branch { target: join, arguments: lanes })?;
        self.restore_facts(facts, join);
        self.current = join;
        self.bindings = bindings;
        let join_values = self.block_parameter_values(join);
        Ok((join_values[0], join_values.get(1).copied()))
    }

    /// `dict.pairs`: a fresh list of `(key, value)` tuples in insertion
    /// order, built by a counted loop over both lists.
    fn lower_dict_pairs(&mut self, keys: ValueId, values: ValueId, key_type: &Type, value_type: &Type) -> Result<ValueId, CanonicalLoweringError> {
        let pair_type = Type::Tuple(vec![key_type.clone(), value_type.clone()]);
        let pair_shape = scalar_aggregate_shape(&pair_type, self.definitions)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("dict pair type"))?;
        let list_type = Type::Named("List".into(), vec![pair_type.clone()]);
        let key_list = Type::Named("List".into(), vec![key_type.clone()]);
        let value_list = Type::Named("List".into(), vec![value_type.clone()]);
        let (_, key_element, _, _) = list_layout(&key_list, self.layout_bundle, self.layouts, self.definitions)?;
        let (_, value_element, _, _) = list_layout(&value_list, self.layout_bundle, self.layouts, self.definitions)?;
        self.ensure_list_extent(keys)?;
        self.ensure_list_extent(values)?;
        let count = *self.list_counts.get(&keys).ok_or(CanonicalLoweringError::UnsupportedExpression("dict keys extent"))?;
        let value_count = *self.list_counts.get(&values).ok_or(CanonicalLoweringError::UnsupportedExpression("dict values extent"))?;
        let out = self.push_empty_value(&list_type)?;
        let zero = self.push_const_size(0)?;
        let one = self.push_const_size(1)?;
        let (lanes, mut shapes) = self.accumulator_lanes(out)?;
        let exit = self.new_block_owned(&shapes)?;
        shapes.push((ValueType::Size, None));
        let header = self.new_block_owned(&shapes)?;
        let body = self.new_block_owned(&shapes)?;
        let mut entry = lanes.clone();
        entry.push(zero);
        self.finish_current(Terminator::Branch { target: header, arguments: entry })?;
        let header_values = self.block_parameter_values(header);
        self.track_accumulator_lanes(&header_values);
        self.current = header;
        let cursor = header_values[3];
        let more = self.push_compare(ComparisonOperator::LessUnsigned, cursor, count)?;
        self.finish_current(Terminator::BranchIf {
            condition: more,
            then_target: body,
            then_arguments: header_values.clone(),
            else_target: exit,
            else_arguments: header_values[..3].to_vec(),
        })?;
        let body_values = self.block_parameter_values(body);
        self.track_accumulator_lanes(&body_values);
        self.current = body;
        let cursor = body_values[3];
        let key_in_bounds = self.push_compare(ComparisonOperator::LessUnsigned, cursor, count)?;
        self.push_guard(key_in_bounds, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let key = self.load_list_element(keys, cursor, key_element)?;
        let key = self.owned_dict_value(key, key_type)?;
        let value_in_bounds = self.push_compare(ComparisonOperator::LessUnsigned, cursor, value_count)?;
        self.push_guard(value_in_bounds, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let value = self.load_list_element(values, cursor, value_element)?;
        let value = self.owned_dict_value(value, value_type)?;
        let pair = self.lower_aggregate_moving(&pair_shape, 0, vec![key, value])?;
        let grown = self.push_owned_element(body_values[0], pair, &list_type)?;
        let (folded, _) = self.accumulator_lanes(grown)?;
        let mut folded = self.split_folded_lanes(folded)?;
        let next = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, cursor, one, OverflowBehavior::Wrap)?;
        folded.push(next);
        self.finish_current(Terminator::Branch { target: header, arguments: folded })?;
        self.current = exit;
        let exit_values = self.block_parameter_values(exit);
        self.track_accumulator_lanes(&exit_values);
        Ok(exit_values[0])
    }

    /// Render a `Float` through the authenticated `float_to_str` import: the
    /// host writes the shortest round-trip decimal (the shared `render_float`
    /// rule) into a fresh buffer of `FLOAT_RENDER_BUFFER_BYTES` and returns
    /// the byte length, which becomes the string's logical length behind a
    /// capacity guard.
    fn lower_render_float(&mut self, value: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        let layout = self.string_layout_id()?;
        let import = self.host_imports.float_to_str.ok_or_else(|| {
            CanonicalLoweringError::InvalidAuthenticatedAbiCatalog("Float render has no projected ABI import".into())
        })?;
        let capacity = self.push_const_size(witchy_wir::canonical_safewir::FLOAT_RENDER_BUFFER_BYTES)?;
        let object = self.fresh(ValueType::Ref(layout))?;
        self.push_operation(Operation::Alloc {
            destination: object,
            layout,
            extent: witchy_wir::canonical_safewir::Extent::Counted {
                header_bytes: 4,
                count: capacity,
                capacity,
                stride_bytes: 1,
            },
            class: witchy_wir::canonical_safewir::AllocationClass::ReferenceCounted,
        });
        self.push_operation(Operation::UpdateCount { object, count: capacity });
        let zero = self.push_const_size(0)?;
        let length = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::HostCall {
            destinations: vec![length],
            import,
            arguments: vec![
                HostCallArgument::Value(value),
                HostCallArgument::Buffer {
                    place: witchy_wir::canonical_safewir::Place::projected(
                        object,
                        Projection::Slice { start: zero, length: capacity },
                        ValueType::Ref(layout),
                        AccessMode::Write,
                    ),
                    extent: HostBufferExtent::AtLeast(witchy_wir::canonical_safewir::FLOAT_RENDER_BUFFER_BYTES),
                },
            ],
        });
        let fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, length, capacity)?;
        self.push_guard(fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
        self.push_operation(Operation::UpdateCount { object, count: length });
        self.string_lengths.insert(object, length);
        self.string_capacities.insert(object, capacity);
        Ok(object)
    }

    /// One variant's render: its short constructor name (the type name for
    /// a record), then the rendered payloads between parentheses when it
    /// has any; a tuple is its payloads between parentheses. Always a fresh
    /// owned string.
    fn lower_render_variant(
        &mut self,
        value: ValueId,
        shape: &ScalarAggregateShape,
        variant: &ScalarVariant,
        tuple: bool,
    ) -> Result<ValueId, CanonicalLoweringError> {
        // An anonymous record's constructor is a generated name the
        // interpreter renders by field names; the lowering has no field
        // names, so it refuses rather than print the generated name.
        if witchy_syntax::linker::is_generated_anon_name(&variant.name) {
            return Err(CanonicalLoweringError::UnsupportedExpression("render anonymous record"));
        }
        let shown = render_variant_name(variant);
        let mut out = if tuple {
            self.lower_string_literal("(")?
        } else if variant.arity == 0 {
            let literal = self.lower_string_literal(&shown)?;
            return self.owned_string_copy(literal);
        } else {
            self.lower_string_literal(&format!("{shown}("))?
        };
        for offset in 0..variant.arity {
            if offset > 0 {
                let separator = self.lower_string_literal(", ")?;
                out = self.concat_string_values(out, separator)?;
            }
            let slot = variant.first_field + offset;
            let field_type = shape.fields[slot].clone();
            let value_type = self.field_value_type(&field_type)?;
            let slot = u32::try_from(slot).map_err(|_| CanonicalLoweringError::IdOverflow)?;
            let field = self.load_record_field(value, slot, value_type)?;
            let rendered = self.lower_render_typed(field, &field_type)?;
            out = self.concat_string_values(out, rendered)?;
        }
        let close = self.lower_string_literal(")")?;
        self.concat_string_values(out, close)
    }

    /// `[a, b, c]`: the elements rendered in order between brackets, folded
    /// into an owned accumulator by a counted loop.
    fn lower_render_list(&mut self, object: ValueId, list_type: &Type, element_type: &Type) -> Result<ValueId, CanonicalLoweringError> {
        let (_, element_value_type, _, _) = list_layout(list_type, self.layout_bundle, self.layouts, self.definitions)?;
        self.ensure_list_extent(object)?;
        let count = *self.list_counts.get(&object).ok_or(CanonicalLoweringError::UnsupportedExpression("render list extent"))?;
        let open = self.lower_string_literal("[")?;
        let accumulator = self.owned_string_copy(open)?;
        let zero = self.push_const_size(0)?;
        let one = self.push_const_size(1)?;
        let (lanes, mut shapes) = self.accumulator_lanes(accumulator)?;
        let exit = self.new_block_owned(&shapes)?;
        shapes.push((ValueType::Size, None));
        let header = self.new_block_owned(&shapes)?;
        let body = self.new_block_owned(&shapes)?;
        let mut entry = lanes.clone();
        entry.push(zero);
        self.finish_current(Terminator::Branch { target: header, arguments: entry })?;
        let header_values = self.block_parameter_values(header);
        self.track_accumulator_lanes(&header_values);
        self.current = header;
        let cursor = header_values[3];
        let more = self.push_compare(ComparisonOperator::LessUnsigned, cursor, count)?;
        self.finish_current(Terminator::BranchIf {
            condition: more,
            then_target: body,
            then_arguments: header_values.clone(),
            else_target: exit,
            else_arguments: header_values[..3].to_vec(),
        })?;
        let body_values = self.block_parameter_values(body);
        self.track_accumulator_lanes(&body_values);
        self.current = body;
        let cursor = body_values[3];
        // The separator before every element but the first: an owned copy
        // of `, ` or an empty string, joined on string lanes.
        let layout = self.string_layout_id()?;
        let separator = self.lower_string_literal(", ")?;
        let first = self.push_compare(ComparisonOperator::Equal, cursor, zero)?;
        let facts = self.snapshot_facts();
        let without = self.new_block(&[])?;
        let with = self.new_block(&[])?;
        let selected = self.new_block_owned(&[(ValueType::Ref(layout), Some(Ownership::Unique)), (ValueType::Size, None), (ValueType::Size, None)])?;
        self.finish_current(Terminator::BranchIf {
            condition: first,
            then_target: without,
            then_arguments: Vec::new(),
            else_target: with,
            else_arguments: Vec::new(),
        })?;
        self.current = without;
        let empty = self.push_empty_value(&Type::Named("String".into(), Vec::new()))?;
        let (lanes, _) = self.accumulator_lanes(empty)?;
        self.finish_current(Terminator::Branch { target: selected, arguments: lanes })?;
        self.current = with;
        let piece = self.owned_string_copy(separator)?;
        let (lanes, _) = self.accumulator_lanes(piece)?;
        self.finish_current(Terminator::Branch { target: selected, arguments: lanes })?;
        self.restore_facts(facts, selected);
        self.current = selected;
        let selected_values = self.block_parameter_values(selected);
        self.track_accumulator_lanes(&selected_values);
        let out = self.concat_string_values(body_values[0], selected_values[0])?;
        let element = self.load_list_element(object, cursor, element_value_type)?;
        let rendered = self.lower_render_typed(element, element_type)?;
        let out = self.concat_string_values(out, rendered)?;
        let (folded, _) = self.accumulator_lanes(out)?;
        let mut folded = self.split_folded_lanes(folded)?;
        let next = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, cursor, one, OverflowBehavior::Wrap)?;
        folded.push(next);
        self.finish_current(Terminator::Branch { target: header, arguments: folded })?;
        self.current = exit;
        let exit_values = self.block_parameter_values(exit);
        self.track_accumulator_lanes(&exit_values);
        let close = self.lower_string_literal("]")?;
        self.concat_string_values(exit_values[0], close)
    }

    /// A fresh owned copy of a string (a literal, a shared field) so it can
    /// travel an owned accumulator lane.
    fn owned_string_copy(&mut self, value: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        let layout = self.string_layout_id()?;
        let length = self.string_length_of(value)?;
        let zero = self.push_const_size(0)?;
        self.copy_checked_slice(value, zero, length, length, layout)
    }

    /// Render a `Bool` as `true` or `false`: a branch selects the word's
    /// offset and length in the static table, and the bytes are copied into a
    /// fresh string.
    fn lower_render_bool(&mut self, value: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        let layout = string_layout(
            &Type::Named("String".into(), Vec::new()),
            self.layout_bundle,
            self.layouts,
        )?;
        let table = self.lower_string_literal(RENDER_BOOL_TABLE)?;
        let table_length = self.string_lengths[&table];
        let when_true = self.new_block(&[])?;
        let when_false = self.new_block(&[])?;
        let join = self.new_block(&[ValueType::Size, ValueType::Size])?;
        self.finish_current(Terminator::BranchIf {
            condition: value,
            then_target: when_true,
            then_arguments: Vec::new(),
            else_target: when_false,
            else_arguments: Vec::new(),
        })?;
        self.current = when_true;
        let true_start = self.push_const_size(5)?;
        let true_length = self.push_const_size(4)?;
        self.finish_current(Terminator::Branch { target: join, arguments: vec![true_start, true_length] })?;
        self.current = when_false;
        let false_start = self.push_const_size(0)?;
        let false_length = self.push_const_size(5)?;
        self.finish_current(Terminator::Branch { target: join, arguments: vec![false_start, false_length] })?;
        self.current = join;
        let join_values = self.block_parameter_values(join);
        let (start, length) = (join_values[0], join_values[1]);
        let end = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, start, length, OverflowBehavior::Trap)?;
        let fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, end, table_length)?;
        self.push_guard(fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
        self.copy_string_slice(table, start, length, layout)
    }

    /// Render an `Int` as a fresh reference-counted decimal string.
    ///
    /// The string layout's element is a raw byte with no SafeWIR value type, so
    /// digits land through one-byte `CopyBytes` slices from the static
    /// `0123456789` table. Each slice end is a trapping `Size` add followed by
    /// an explicit `<=` guard against the object's count, which is exactly the
    /// fact the checker's range rule consumes. The sign is an arithmetic mask
    /// (`value >> 63`) turned into a zero- or one-byte copy from the static `-`
    /// table, so no branch is needed and `Int::MIN` is never negated: digits
    /// are taken from the still-negative remainder and multiplied by the sign.
    /// Two loops: count the digits, then write them from the end.
    fn lower_render_int(&mut self, value: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        let layout = string_layout(
            &Type::Named("String".into(), Vec::new()),
            self.layout_bundle,
            self.layouts,
        )?;
        let digits = self.lower_string_literal(RENDER_DIGIT_TABLE)?;
        let minus = self.lower_string_literal(RENDER_MINUS_SIGN)?;
        let digit_table_count = self.string_lengths[&digits];
        let minus_count = self.string_lengths[&minus];

        let zero_int = self.push_const_int(0)?;
        let one_int = self.push_const_int(1)?;
        let ten_int = self.push_const_int(10)?;
        let sign_bit = self.push_const_int(63)?;
        let zero = self.push_const_size(0)?;
        let one = self.push_const_size(1)?;
        // mask = -1 for a negative value, 0 otherwise.
        let mask = self.push_arithmetic(ValueType::Int, ArithmeticOperator::ShiftRightSigned, value, sign_bit, OverflowBehavior::Wrap)?;
        let twice_mask = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Add, mask, mask, OverflowBehavior::Wrap)?;
        // sign = 1 + 2 * mask: -1 for a negative value, 1 otherwise.
        let sign = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Add, one_int, twice_mask, OverflowBehavior::Wrap)?;
        // sign_len = -mask: one byte for the `-`, or none.
        let sign_len_int = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Subtract, zero_int, mask, OverflowBehavior::Wrap)?;
        let sign_len = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::NarrowSize { destination: sign_len, value: sign_len_int });

        // The checker renames a branch argument to its block parameter in
        // every fact table, so a value whose facts are needed after the loop
        // (`one`, `total`) must never be passed as a loop argument; each loop
        // starts from a dedicated copy instead.
        // Loop A: count the digits of |value| beyond the first.
        let first_rest = self.push_arithmetic(ValueType::Int, ArithmeticOperator::DivideSigned, value, ten_int, OverflowBehavior::Trap)?;
        let initial_count = self.push_const_size(1)?;
        let count_state = [ValueType::Int, ValueType::Size];
        let count_header = self.new_block(&count_state)?;
        let count_body = self.new_block(&count_state)?;
        let count_exit = self.new_block(&count_state)?;
        self.finish_current(Terminator::Branch { target: count_header, arguments: vec![first_rest, initial_count] })?;
        let header_values = self.block_parameter_values(count_header);
        self.current = count_header;
        let more_digits = self.push_compare(ComparisonOperator::NotEqual, header_values[0], zero_int)?;
        self.finish_current(Terminator::BranchIf {
            condition: more_digits,
            then_target: count_body,
            then_arguments: header_values.clone(),
            else_target: count_exit,
            else_arguments: header_values.clone(),
        })?;
        let body_values = self.block_parameter_values(count_body);
        self.current = count_body;
        let next_count = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, body_values[1], one, OverflowBehavior::Wrap)?;
        let next_rest = self.push_arithmetic(ValueType::Int, ArithmeticOperator::DivideSigned, body_values[0], ten_int, OverflowBehavior::Trap)?;
        self.finish_current(Terminator::Branch { target: count_header, arguments: vec![next_rest, next_count] })?;
        let exit_values = self.block_parameter_values(count_exit);
        self.current = count_exit;
        let digit_count = exit_values[1];

        // Allocate `digits + sign` bytes; the trapping add is the checker's
        // exact-length fact and the guard is the allocation bound.
        let total = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, digit_count, sign_len, OverflowBehavior::Trap)?;
        let maximum = self.push_const_size(u64::from(u32::MAX - 4))?;
        let total_fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, total, maximum)?;
        self.push_guard(total_fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let object = self.fresh(ValueType::Ref(layout))?;
        self.push_operation(Operation::Alloc {
            destination: object,
            layout,
            extent: witchy_wir::canonical_safewir::Extent::Counted {
                header_bytes: 4,
                count: total,
                capacity: total,
                stride_bytes: 1,
            },
            class: witchy_wir::canonical_safewir::AllocationClass::ReferenceCounted,
        });
        self.push_operation(Operation::UpdateCount { object, count: total });

        // Sign: copy `sign_len` bytes (zero or one) of `-` to the front.
        let sign_fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, sign_len, total)?;
        self.push_guard(sign_fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let sign_in_table = self.push_compare(ComparisonOperator::LessEqualUnsigned, sign_len, minus_count)?;
        self.push_guard(sign_in_table, witchy_wir::canonical_safewir::GuardKind::Bounds);
        self.push_byte_copy(object, zero, minus, zero, sign_len, layout);

        // Loop B: write digits from the end down to the sign.
        let write_state = [ValueType::Int, ValueType::Size];
        let write_header = self.new_block(&write_state)?;
        let write_body = self.new_block(&write_state)?;
        let write_exit = self.new_block(&[])?;
        let start_position = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, total, zero, OverflowBehavior::Wrap)?;
        self.finish_current(Terminator::Branch { target: write_header, arguments: vec![value, start_position] })?;
        let header_values = self.block_parameter_values(write_header);
        self.current = write_header;
        let more_positions = self.push_compare(ComparisonOperator::LessUnsigned, sign_len, header_values[1])?;
        self.finish_current(Terminator::BranchIf {
            condition: more_positions,
            then_target: write_body,
            then_arguments: header_values.clone(),
            else_target: write_exit,
            else_arguments: Vec::new(),
        })?;
        let body_values = self.block_parameter_values(write_body);
        self.current = write_body;
        let rest = body_values[0];
        let position = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Subtract, body_values[1], one, OverflowBehavior::Wrap)?;
        let remainder = self.push_arithmetic(ValueType::Int, ArithmeticOperator::RemainderSigned, rest, ten_int, OverflowBehavior::Trap)?;
        let digit_int = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Multiply, remainder, sign, OverflowBehavior::Wrap)?;
        let digit = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::NarrowSize { destination: digit, value: digit_int });
        let digit_end = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, digit, one, OverflowBehavior::Trap)?;
        let digit_in_table = self.push_compare(ComparisonOperator::LessEqualUnsigned, digit_end, digit_table_count)?;
        self.push_guard(digit_in_table, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let position_end = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, position, one, OverflowBehavior::Trap)?;
        let position_fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, position_end, total)?;
        self.push_guard(position_fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
        self.push_byte_copy(object, position, digits, digit, one, layout);
        let next_rest = self.push_arithmetic(ValueType::Int, ArithmeticOperator::DivideSigned, rest, ten_int, OverflowBehavior::Trap)?;
        self.finish_current(Terminator::Branch { target: write_header, arguments: vec![next_rest, position] })?;
        self.current = write_exit;

        self.string_lengths.insert(object, total);
        self.string_capacities.insert(object, total);
        Ok(object)
    }

    /// Read byte `index` of a string as a zero-extended `Size`. The caller has
    /// pushed the `index < count` guard the checker's element rule consumes.
    fn push_string_byte_load(
        &mut self,
        object: ValueId,
        index: ValueId,
        layout: witchy_wir::layout::LayoutId,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let destination = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::Load {
            destination,
            place: witchy_wir::canonical_safewir::Place::projected(
                object,
                Projection::Element { index },
                ValueType::Size,
                AccessMode::Read,
            ),
            transfer: witchy_wir::canonical_safewir::LoadTransfer::CopyScalar,
        });
        Ok(destination)
    }

    /// `a == b` over strings: equal byte lengths and equal bytes. The scan is
    /// a loop whose header tests the position and whose body leaves early on
    /// the first differing byte; the result is the exit block's parameter.
    fn lower_string_equality(
        &mut self,
        left: ValueId,
        right: ValueId,
        negate: bool,
    ) -> Result<ValueId, CanonicalLoweringError> {
        self.ensure_string_extent(left)?;
        self.ensure_string_extent(right)?;
        let ValueType::Ref(layout) = self.values[&left] else {
            return Err(CanonicalLoweringError::UnsupportedExpression("string equality operand"));
        };
        let left_length = *self.string_lengths.get(&left).ok_or(
            CanonicalLoweringError::UnsupportedExpression("string equality operand extent"),
        )?;
        let right_length = *self.string_lengths.get(&right).ok_or(
            CanonicalLoweringError::UnsupportedExpression("string equality operand extent"),
        )?;
        let equal = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::ConstBool { destination: equal, value: true });
        let differ = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::ConstBool { destination: differ, value: false });
        let zero = self.push_const_size(0)?;
        let one = self.push_const_size(1)?;
        let same_length = self.push_compare(ComparisonOperator::Equal, left_length, right_length)?;
        let scan = self.new_block(&[])?;
        let header = self.new_block(&[ValueType::Size])?;
        let body = self.new_block(&[ValueType::Size])?;
        let exit = self.new_block(&[ValueType::Bool])?;
        self.finish_current(Terminator::BranchIf {
            condition: same_length,
            then_target: scan,
            then_arguments: Vec::new(),
            else_target: exit,
            else_arguments: vec![differ],
        })?;
        self.current = scan;
        self.finish_current(Terminator::Branch { target: header, arguments: vec![zero] })?;
        let header_values = self.block_parameter_values(header);
        self.current = header;
        let more = self.push_compare(ComparisonOperator::LessUnsigned, header_values[0], left_length)?;
        self.finish_current(Terminator::BranchIf {
            condition: more,
            then_target: body,
            then_arguments: header_values.clone(),
            else_target: exit,
            else_arguments: vec![equal],
        })?;
        let body_values = self.block_parameter_values(body);
        self.current = body;
        let index = body_values[0];
        let left_in_bounds = self.push_compare(ComparisonOperator::LessUnsigned, index, left_length)?;
        self.push_guard(left_in_bounds, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let right_in_bounds = self.push_compare(ComparisonOperator::LessUnsigned, index, right_length)?;
        self.push_guard(right_in_bounds, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let left_byte = self.push_string_byte_load(left, index, layout)?;
        let right_byte = self.push_string_byte_load(right, index, layout)?;
        let same_byte = self.push_compare(ComparisonOperator::Equal, left_byte, right_byte)?;
        let next = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, index, one, OverflowBehavior::Wrap)?;
        self.finish_current(Terminator::BranchIf {
            condition: same_byte,
            then_target: header,
            then_arguments: vec![next],
            else_target: exit,
            else_arguments: vec![differ],
        })?;
        self.current = exit;
        let result = self.block_parameter_values(exit)[0];
        if negate {
            return self.push_compare(ComparisonOperator::Equal, result, differ);
        }
        Ok(result)
    }

    /// The string layout every string value carries.
    fn string_layout_id(&self) -> Result<witchy_wir::layout::LayoutId, CanonicalLoweringError> {
        string_layout(&Type::Named("String".into(), Vec::new()), self.layout_bundle, self.layouts)
    }

    /// The byte length fact of a string value, loading the header when it is
    /// not cached.
    fn string_length_of(&mut self, object: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        self.ensure_string_extent(object)?;
        self.string_lengths.get(&object).copied().ok_or(
            CanonicalLoweringError::UnsupportedExpression("string operand extent"),
        )
    }

    /// `sign(value)` for an `Int`, branch-free: `-1`, `0`, or `1`.
    fn push_sign(&mut self, value: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        let zero = self.push_const_int(0)?;
        let sixty_three = self.push_const_int(63)?;
        let negative = self.push_arithmetic(ValueType::Int, ArithmeticOperator::ShiftRightSigned, value, sixty_three, OverflowBehavior::Wrap)?;
        let negated = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Subtract, zero, value, OverflowBehavior::Wrap)?;
        let positive = self.push_arithmetic(ValueType::Int, ArithmeticOperator::ShiftRightUnsigned, negated, sixty_three, OverflowBehavior::Wrap)?;
        self.push_arithmetic(ValueType::Int, ArithmeticOperator::BitOr, negative, positive, OverflowBehavior::Wrap)
    }

    /// Whether `left[left_start ..+ length)` and `right[right_start ..+ length)`
    /// hold the same bytes. Every byte read is guarded against its string's
    /// length, so a caller need not prove the ranges fit.
    fn lower_string_range_equal(
        &mut self,
        left: ValueId,
        left_start: ValueId,
        right: ValueId,
        right_start: ValueId,
        length: ValueId,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let layout = self.string_layout_id()?;
        let left_length = self.string_length_of(left)?;
        let right_length = self.string_length_of(right)?;
        let equal = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::ConstBool { destination: equal, value: true });
        let differ = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::ConstBool { destination: differ, value: false });
        let zero = self.push_const_size(0)?;
        let one = self.push_const_size(1)?;
        let header = self.new_block(&[ValueType::Size])?;
        let body = self.new_block(&[ValueType::Size])?;
        let exit = self.new_block(&[ValueType::Bool])?;
        self.finish_current(Terminator::Branch { target: header, arguments: vec![zero] })?;
        let cursor = self.block_parameter_values(header)[0];
        self.current = header;
        let more = self.push_compare(ComparisonOperator::LessUnsigned, cursor, length)?;
        self.finish_current(Terminator::BranchIf {
            condition: more,
            then_target: body,
            then_arguments: vec![cursor],
            else_target: exit,
            else_arguments: vec![equal],
        })?;
        let cursor = self.block_parameter_values(body)[0];
        self.current = body;
        let left_index = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, left_start, cursor, OverflowBehavior::Wrap)?;
        let right_index = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, right_start, cursor, OverflowBehavior::Wrap)?;
        let left_in_bounds = self.push_compare(ComparisonOperator::LessUnsigned, left_index, left_length)?;
        self.push_guard(left_in_bounds, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let right_in_bounds = self.push_compare(ComparisonOperator::LessUnsigned, right_index, right_length)?;
        self.push_guard(right_in_bounds, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let left_byte = self.push_string_byte_load(left, left_index, layout)?;
        let right_byte = self.push_string_byte_load(right, right_index, layout)?;
        let same = self.push_compare(ComparisonOperator::Equal, left_byte, right_byte)?;
        let next = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, cursor, one, OverflowBehavior::Wrap)?;
        self.finish_current(Terminator::BranchIf {
            condition: same,
            then_target: header,
            then_arguments: vec![next],
            else_target: exit,
            else_arguments: vec![differ],
        })?;
        self.current = exit;
        Ok(self.block_parameter_values(exit)[0])
    }

    /// The lane shape of a list or string accumulator threaded through a
    /// kernel loop: the object owns its token; its count and capacity ride
    /// along as scalars.
    fn accumulator_lanes(&mut self, object: ValueId) -> Result<(Vec<ValueId>, Vec<(ValueType, Option<Ownership>)>), CanonicalLoweringError> {
        let ValueType::Ref(_) = self.values[&object] else {
            return Err(CanonicalLoweringError::UnsupportedExpression("kernel accumulator"));
        };
        self.ensure_list_extent(object)?;
        self.ensure_string_extent(object)?;
        let (count, capacity) = self.tracked_lanes(object).ok_or(
            CanonicalLoweringError::UnsupportedExpression("kernel accumulator without extent lanes"),
        )?;
        // A fresh value's count and capacity are one id; the lanes must be
        // distinct so the header renames each to its own parameter.
        let (count, capacity) = self.split_lanes(count, capacity)?;
        Ok((
            vec![object, count, capacity],
            vec![(self.values[&object], Some(Ownership::Unique)), (ValueType::Size, None), (ValueType::Size, None)],
        ))
    }

    /// Split a folded accumulator's count and capacity lanes (a fresh
    /// value aliases them) so a back edge renames each to its own parameter.
    fn split_folded_lanes(&mut self, mut lanes: Vec<ValueId>) -> Result<Vec<ValueId>, CanonicalLoweringError> {
        if lanes.len() < 3 {
            return Err(CanonicalLoweringError::UnsupportedExpression("kernel accumulator lanes"));
        }
        let (count, capacity) = self.split_lanes(lanes[1], lanes[2])?;
        lanes[1] = count;
        lanes[2] = capacity;
        Ok(lanes)
    }

    /// The byte length of the UTF-8 sequence whose lead byte is `byte`:
    /// with `h = b >> 7`, `f = (b >> 5) & 1`, `e = (b >> 4) & 1`, it is
    /// `1 + h + (h & f) + (h & f & e)`: 1 below 0x80, 2 for 0xC0..0xDF,
    /// 3 for 0xE0..0xEF, and 4 above.
    fn push_utf8_sequence_length(&mut self, byte: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        let value = self.fresh(ValueType::Int)?;
        self.push_operation(Operation::WidenSize { destination: value, value: byte });
        let one = self.push_const_int(1)?;
        let four = self.push_const_int(4)?;
        let five = self.push_const_int(5)?;
        let seven = self.push_const_int(7)?;
        let high = self.push_arithmetic(ValueType::Int, ArithmeticOperator::ShiftRightUnsigned, value, seven, OverflowBehavior::Wrap)?;
        let shifted5 = self.push_arithmetic(ValueType::Int, ArithmeticOperator::ShiftRightUnsigned, value, five, OverflowBehavior::Wrap)?;
        let shifted4 = self.push_arithmetic(ValueType::Int, ArithmeticOperator::ShiftRightUnsigned, value, four, OverflowBehavior::Wrap)?;
        let bit5 = self.push_arithmetic(ValueType::Int, ArithmeticOperator::BitAnd, shifted5, one, OverflowBehavior::Wrap)?;
        let bit4 = self.push_arithmetic(ValueType::Int, ArithmeticOperator::BitAnd, shifted4, one, OverflowBehavior::Wrap)?;
        let third = self.push_arithmetic(ValueType::Int, ArithmeticOperator::BitAnd, high, bit5, OverflowBehavior::Wrap)?;
        let fourth = self.push_arithmetic(ValueType::Int, ArithmeticOperator::BitAnd, third, bit4, OverflowBehavior::Wrap)?;
        let sum = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Add, one, high, OverflowBehavior::Wrap)?;
        let sum = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Add, sum, third, OverflowBehavior::Wrap)?;
        let sum = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Add, sum, fourth, OverflowBehavior::Wrap)?;
        let length = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::NarrowSize { destination: length, value: sum });
        Ok(length)
    }

    /// Establish `start + length <= total` (a trapping add and a bounds
    /// guard, the shape `copy_string_slice` relies on) and copy that slice.
    fn copy_checked_slice(
        &mut self,
        object: ValueId,
        start: ValueId,
        length: ValueId,
        total: ValueId,
        layout: witchy_wir::layout::LayoutId,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let end = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, start, length, OverflowBehavior::Trap)?;
        let fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, end, total)?;
        self.push_guard(fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
        self.copy_string_slice(object, start, length, layout)
    }

    /// Loop over the UTF-8 sequences of `object`, folding an owned copy of
    /// each into the accumulator lanes (`step` returns the new lanes). The
    /// sequence length comes from the lead byte; a sequence past the end
    /// traps (every string is valid UTF-8, so it never does).
    fn lower_utf8_sequence_loop(
        &mut self,
        object: ValueId,
        accumulator: ValueId,
        mut step: impl FnMut(&mut Self, &[ValueId], ValueId) -> Result<Vec<ValueId>, CanonicalLoweringError>,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let layout = self.string_layout_id()?;
        let total = self.string_length_of(object)?;
        let zero = self.push_const_size(0)?;
        let (lanes, mut shapes) = self.accumulator_lanes(accumulator)?;
        let exit = self.new_block_owned(&shapes)?;
        shapes.push((ValueType::Size, None));
        let header = self.new_block_owned(&shapes)?;
        let body = self.new_block_owned(&shapes)?;
        let mut entry = lanes.clone();
        entry.push(zero);
        self.finish_current(Terminator::Branch { target: header, arguments: entry })?;
        let header_values = self.block_parameter_values(header);
        self.track_accumulator_lanes(&header_values);
        self.current = header;
        let cursor = header_values[3];
        let more = self.push_compare(ComparisonOperator::LessUnsigned, cursor, total)?;
        self.finish_current(Terminator::BranchIf {
            condition: more,
            then_target: body,
            then_arguments: header_values.clone(),
            else_target: exit,
            else_arguments: header_values[..3].to_vec(),
        })?;
        let body_values = self.block_parameter_values(body);
        self.track_accumulator_lanes(&body_values);
        self.current = body;
        let cursor = body_values[3];
        let in_bounds = self.push_compare(ComparisonOperator::LessUnsigned, cursor, total)?;
        self.push_guard(in_bounds, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let byte = self.push_string_byte_load(object, cursor, layout)?;
        let length = self.push_utf8_sequence_length(byte)?;
        let piece = self.copy_checked_slice(object, cursor, length, total, layout)?;
        let next = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, cursor, length, OverflowBehavior::Wrap)?;
        let folded = step(self, &body_values[..3], piece)?;
        let mut folded = self.split_folded_lanes(folded)?;
        folded.push(next);
        self.finish_current(Terminator::Branch { target: header, arguments: folded })?;
        self.current = exit;
        let exit_values = self.block_parameter_values(exit);
        self.track_accumulator_lanes(&exit_values);
        Ok(exit_values[0])
    }

    /// Loop over the non-overlapping matches of a non-empty `needle` in
    /// `object`, folding an owned copy of each piece between matches (and
    /// finally the tail) into the accumulator lanes; `step` sees whether
    /// another match follows the piece.
    fn lower_match_scan_loop(
        &mut self,
        object: ValueId,
        needle: ValueId,
        accumulator: ValueId,
        mut step: impl FnMut(&mut Self, &[ValueId], ValueId, bool) -> Result<Vec<ValueId>, CanonicalLoweringError>,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let layout = self.string_layout_id()?;
        let total = self.string_length_of(object)?;
        let needle_length = self.string_length_of(needle)?;
        let zero = self.push_const_size(0)?;
        let one = self.push_const_size(1)?;
        let (lanes, mut shapes) = self.accumulator_lanes(accumulator)?;
        // The tail block receives the lanes and the start of the tail piece.
        shapes.push((ValueType::Size, None));
        let tail = self.new_block_owned(&shapes)?;
        // The header and body add the scan cursor.
        shapes.push((ValueType::Size, None));
        let header = self.new_block_owned(&shapes)?;
        let body = self.new_block_owned(&shapes)?;
        let fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, needle_length, total)?;
        let scan = self.new_block(&[])?;
        let mut no_match = lanes.clone();
        no_match.push(zero);
        self.finish_current(Terminator::BranchIf {
            condition: fits,
            then_target: scan,
            then_arguments: Vec::new(),
            else_target: tail,
            else_arguments: no_match,
        })?;
        self.current = scan;
        let last_start = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Subtract, total, needle_length, OverflowBehavior::Wrap)?;
        let mut entry = lanes.clone();
        entry.extend([zero, zero]);
        self.finish_current(Terminator::Branch { target: header, arguments: entry })?;
        let header_values = self.block_parameter_values(header);
        self.track_accumulator_lanes(&header_values);
        self.current = header;
        let cursor = header_values[4];
        let more = self.push_compare(ComparisonOperator::LessEqualUnsigned, cursor, last_start)?;
        self.finish_current(Terminator::BranchIf {
            condition: more,
            then_target: body,
            then_arguments: header_values.clone(),
            else_target: tail,
            else_arguments: header_values[..4].to_vec(),
        })?;
        let body_values = self.block_parameter_values(body);
        self.track_accumulator_lanes(&body_values);
        self.current = body;
        let start = body_values[3];
        let cursor = body_values[4];
        let matched = self.lower_string_range_equal(object, cursor, needle, zero, needle_length)?;
        let found = self.new_block(&[])?;
        let advance = self.new_block(&[])?;
        self.finish_current(Terminator::BranchIf {
            condition: matched,
            then_target: found,
            then_arguments: Vec::new(),
            else_target: advance,
            else_arguments: Vec::new(),
        })?;
        self.current = found;
        let ordered = self.push_compare(ComparisonOperator::LessEqualUnsigned, start, cursor)?;
        self.push_guard(ordered, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let piece_length = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Subtract, cursor, start, OverflowBehavior::Wrap)?;
        let piece = self.copy_checked_slice(object, start, piece_length, total, layout)?;
        let after = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, cursor, needle_length, OverflowBehavior::Trap)?;
        let folded = step(self, &body_values[..3], piece, true)?;
        let mut folded = self.split_folded_lanes(folded)?;
        folded.extend([after, after]);
        self.finish_current(Terminator::Branch { target: header, arguments: folded })?;
        self.current = advance;
        let next = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, cursor, one, OverflowBehavior::Wrap)?;
        let mut unchanged = body_values[..4].to_vec();
        unchanged.push(next);
        self.finish_current(Terminator::Branch { target: header, arguments: unchanged })?;
        self.current = tail;
        let tail_values = self.block_parameter_values(tail);
        self.track_accumulator_lanes(&tail_values);
        let start = tail_values[3];
        let ordered = self.push_compare(ComparisonOperator::LessEqualUnsigned, start, total)?;
        self.push_guard(ordered, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let rest = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Subtract, total, start, OverflowBehavior::Wrap)?;
        let piece = self.copy_checked_slice(object, start, rest, total, layout)?;
        let folded = step(self, &tail_values[..3], piece, false)?;
        Ok(folded[0])
    }

    /// `string.to_upper` / `string.to_lower`: ASCII case mapping (the
    /// interpreter's rule) over a fresh copy, one byte at a time and without
    /// branches: a byte in the source range moves by 32, every other byte,
    /// including every byte of a multi-byte sequence, is left as it is.
    fn lower_string_case(&mut self, object: ValueId, upper: bool) -> Result<ValueId, CanonicalLoweringError> {
        let layout = self.string_layout_id()?;
        let total = self.string_length_of(object)?;
        let zero = self.push_const_size(0)?;
        let copy = self.copy_checked_slice(object, zero, total, total, layout)?;
        let length = self.string_length_of(copy)?;
        let one = self.push_const_size(1)?;
        let (low, high) = if upper { (97, 122) } else { (65, 90) };
        let low = self.push_const_int(low)?;
        let high = self.push_const_int(high)?;
        let sixty_three = self.push_const_int(63)?;
        let one_int = self.push_const_int(1)?;
        let thirty_two = self.push_const_int(32)?;
        let header = self.new_block(&[ValueType::Size])?;
        let body = self.new_block(&[ValueType::Size])?;
        let exit = self.new_block(&[])?;
        self.finish_current(Terminator::Branch { target: header, arguments: vec![zero] })?;
        let cursor = self.block_parameter_values(header)[0];
        self.current = header;
        let more = self.push_compare(ComparisonOperator::LessUnsigned, cursor, length)?;
        self.finish_current(Terminator::BranchIf {
            condition: more,
            then_target: body,
            then_arguments: vec![cursor],
            else_target: exit,
            else_arguments: Vec::new(),
        })?;
        let cursor = self.block_parameter_values(body)[0];
        self.current = body;
        let in_bounds = self.push_compare(ComparisonOperator::LessUnsigned, cursor, length)?;
        self.push_guard(in_bounds, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let byte = self.push_string_byte_load(copy, cursor, layout)?;
        let value = self.fresh(ValueType::Int)?;
        self.push_operation(Operation::WidenSize { destination: value, value: byte });
        // `below = value - low` and `above = high - value` are both non-negative
        // exactly when the byte is in range; the sign bit of their union is
        // the out-of-range flag, so `1 + (flag ? -1 : 0)` selects the shift.
        let below = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Subtract, value, low, OverflowBehavior::Wrap)?;
        let above = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Subtract, high, value, OverflowBehavior::Wrap)?;
        let either = self.push_arithmetic(ValueType::Int, ArithmeticOperator::BitOr, below, above, OverflowBehavior::Wrap)?;
        let flag = self.push_arithmetic(ValueType::Int, ArithmeticOperator::ShiftRightSigned, either, sixty_three, OverflowBehavior::Wrap)?;
        let select = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Add, one_int, flag, OverflowBehavior::Wrap)?;
        let delta = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Multiply, thirty_two, select, OverflowBehavior::Wrap)?;
        let operator = if upper { ArithmeticOperator::Subtract } else { ArithmeticOperator::Add };
        let mapped = self.push_arithmetic(ValueType::Int, operator, value, delta, OverflowBehavior::Wrap)?;
        let narrowed = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::NarrowSize { destination: narrowed, value: mapped });
        let byte_limit = self.push_const_size(255)?;
        let fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, narrowed, byte_limit)?;
        self.push_guard(fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
        self.push_operation(Operation::Store {
            place: witchy_wir::canonical_safewir::Place::projected(
                copy,
                Projection::Element { index: cursor },
                ValueType::Size,
                AccessMode::Write,
            ),
            value: narrowed,
            transfer: witchy_wir::canonical_safewir::StoreTransfer::CopyScalar,
        });
        let next = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, cursor, one, OverflowBehavior::Wrap)?;
        self.finish_current(Terminator::Branch { target: header, arguments: vec![next] })?;
        self.current = exit;
        Ok(copy)
    }

    /// `string.chars` over a runtime string: one owned string per UTF-8
    /// sequence.
    fn lower_string_chars_dynamic(&mut self, object: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        let list_type = Type::Named("List".into(), vec![Type::Named("String".into(), Vec::new())]);
        let list = self.push_empty_value(&list_type)?;
        self.lower_utf8_sequence_loop(object, list, |this, lanes, piece| {
            let grown = this.push_owned_element(lanes[0], piece, &list_type)?;
            let (count, capacity) = this.tracked_lanes(grown).expect("a pushed list has lanes");
            Ok(vec![grown, count, capacity])
        })
    }

    /// `string.split`: the pieces between non-overlapping matches of the
    /// separator, the tail included. An empty separator yields the whole
    /// string as its one piece (the interpreter's rule).
    fn lower_string_split(&mut self, object: ValueId, separator: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        let list_type = Type::Named("List".into(), vec![Type::Named("String".into(), Vec::new())]);
        let (list_layout_id, _, _, _) = list_layout(&list_type, self.layout_bundle, self.layouts, self.definitions)?;
        let separator_length = self.string_length_of(separator)?;
        let zero = self.push_const_size(0)?;
        let empty_separator = self.push_compare(ComparisonOperator::Equal, separator_length, zero)?;
        let boundaries = self.new_block(&[])?;
        let pieces = self.new_block(&[])?;
        let join = self.new_block_owned(&[(ValueType::Ref(list_layout_id), Some(Ownership::Unique)), (ValueType::Size, None), (ValueType::Size, None)])?;
        self.finish_current(Terminator::BranchIf {
            condition: empty_separator,
            then_target: boundaries,
            then_arguments: Vec::new(),
            else_target: pieces,
            else_arguments: Vec::new(),
        })?;
        let push = |this: &mut Self, lanes: &[ValueId], piece: ValueId| -> Result<Vec<ValueId>, CanonicalLoweringError> {
            let grown = this.push_owned_element(lanes[0], piece, &list_type)?;
            let (count, capacity) = this.tracked_lanes(grown).expect("a pushed list has lanes");
            Ok(vec![grown, count, capacity])
        };
        self.current = boundaries;
        let layout = self.string_layout_id()?;
        let total = self.string_length_of(object)?;
        let whole = self.copy_checked_slice(object, zero, total, total, layout)?;
        let list = self.push_empty_value(&list_type)?;
        let list = self.push_owned_element(list, whole, &list_type)?;
        let (lanes, _) = self.accumulator_lanes(list)?;
        self.finish_current(Terminator::Branch { target: join, arguments: lanes })?;
        self.current = pieces;
        let list = self.push_empty_value(&list_type)?;
        let list = self.lower_match_scan_loop(object, separator, list, |this, lanes, piece, _| push(this, lanes, piece))?;
        let (lanes, _) = self.accumulator_lanes(list)?;
        self.finish_current(Terminator::Branch { target: join, arguments: lanes })?;
        self.current = join;
        let join_values = self.block_parameter_values(join);
        self.track_accumulator_lanes(&join_values);
        Ok(join_values[0])
    }

    /// `string.replace`: every non-overlapping match of `from` replaced by
    /// `to`. An empty `from` matches at every character boundary, so `to`
    /// surrounds every character.
    fn lower_string_replace(&mut self, object: ValueId, from: ValueId, to: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        let layout = self.string_layout_id()?;
        let from_length = self.string_length_of(from)?;
        let to_length = self.string_length_of(to)?;
        let zero = self.push_const_size(0)?;
        let empty_from = self.push_compare(ComparisonOperator::Equal, from_length, zero)?;
        let boundaries = self.new_block(&[])?;
        let pieces = self.new_block(&[])?;
        let join = self.new_block_owned(&[(ValueType::Ref(layout), Some(Ownership::Unique)), (ValueType::Size, None), (ValueType::Size, None)])?;
        self.finish_current(Terminator::BranchIf {
            condition: empty_from,
            then_target: boundaries,
            then_arguments: Vec::new(),
            else_target: pieces,
            else_arguments: Vec::new(),
        })?;
        let append = |this: &mut Self, lanes: &[ValueId], piece: ValueId, with_to: bool| -> Result<Vec<ValueId>, CanonicalLoweringError> {
            let mut out = this.concat_string_values(lanes[0], piece)?;
            if with_to {
                out = this.concat_string_values(out, to)?;
            }
            let (count, capacity) = this.tracked_lanes(out).expect("a concatenation has lanes");
            Ok(vec![out, count, capacity])
        };
        self.current = boundaries;
        let out = self.copy_checked_slice(to, zero, to_length, to_length, layout)?;
        let out = self.lower_utf8_sequence_loop(object, out, |this, lanes, piece| append(this, lanes, piece, true))?;
        let (lanes, _) = self.accumulator_lanes(out)?;
        self.finish_current(Terminator::Branch { target: join, arguments: lanes })?;
        self.current = pieces;
        let out = self.push_empty_value(&Type::Named("String".into(), Vec::new()))?;
        let out = self.lower_match_scan_loop(object, from, out, append)?;
        let (lanes, _) = self.accumulator_lanes(out)?;
        self.finish_current(Terminator::Branch { target: join, arguments: lanes })?;
        self.current = join;
        let join_values = self.block_parameter_values(join);
        self.track_accumulator_lanes(&join_values);
        Ok(join_values[0])
    }

    fn lower_string_find(&mut self, object: ValueId, needle: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        let length = self.string_length_of(object)?;
        let needle_length = self.string_length_of(needle)?;
        let missing = self.push_const_int(-1)?;
        let zero = self.push_const_size(0)?;
        let one = self.push_const_size(1)?;
        let fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, needle_length, length)?;
        let scan = self.new_block(&[])?;
        let exit = self.new_block(&[ValueType::Int])?;
        self.finish_current(Terminator::BranchIf {
            condition: fits,
            then_target: scan,
            then_arguments: Vec::new(),
            else_target: exit,
            else_arguments: vec![missing],
        })?;
        self.current = scan;
        let last_start = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Subtract, length, needle_length, OverflowBehavior::Wrap)?;
        let header = self.new_block(&[ValueType::Size])?;
        let body = self.new_block(&[ValueType::Size])?;
        self.finish_current(Terminator::Branch { target: header, arguments: vec![zero] })?;
        let cursor = self.block_parameter_values(header)[0];
        self.current = header;
        let more = self.push_compare(ComparisonOperator::LessEqualUnsigned, cursor, last_start)?;
        self.finish_current(Terminator::BranchIf {
            condition: more,
            then_target: body,
            then_arguments: vec![cursor],
            else_target: exit,
            else_arguments: vec![missing],
        })?;
        let cursor = self.block_parameter_values(body)[0];
        self.current = body;
        let matched = self.lower_string_range_equal(object, cursor, needle, zero, needle_length)?;
        let found = self.new_block(&[])?;
        let advance = self.new_block(&[])?;
        self.finish_current(Terminator::BranchIf {
            condition: matched,
            then_target: found,
            then_arguments: Vec::new(),
            else_target: advance,
            else_arguments: Vec::new(),
        })?;
        self.current = found;
        // The match's character index counts the sequence starts before it.
        let characters = self.lower_char_count_prefix(object, cursor)?;
        self.finish_current(Terminator::Branch { target: exit, arguments: vec![characters] })?;
        self.current = advance;
        let next = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, cursor, one, OverflowBehavior::Wrap)?;
        self.finish_current(Terminator::Branch { target: header, arguments: vec![next] })?;
        self.current = exit;
        Ok(self.block_parameter_values(exit)[0])
    }

    /// `string.to_int`: the decimal integer between the surrounding ASCII
    /// whitespace, with an optional sign; anything else aborts, as does an
    /// overflow. Digits accumulate negatively so the most negative value
    /// parses, and the positive result negates at the end.
    fn lower_string_to_int(&mut self, object: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        let layout = self.string_layout_id()?;
        let length = self.string_length_of(object)?;
        let zero = self.push_const_size(0)?;
        let one = self.push_const_size(1)?;
        let start = self.lower_whitespace_scan(object, layout, length, zero, length, true)?;
        let end = self.lower_whitespace_scan(object, layout, length, start, length, false)?;
        let non_empty = self.push_compare(ComparisonOperator::LessUnsigned, start, end)?;
        self.push_trap_unless(non_empty)?;
        let in_bounds = self.push_compare(ComparisonOperator::LessUnsigned, start, length)?;
        self.push_guard(in_bounds, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let first = self.push_string_byte_load(object, start, layout)?;
        let minus = self.push_const_size(45)?;
        let plus = self.push_const_size(43)?;
        let is_minus = self.push_compare(ComparisonOperator::Equal, first, minus)?;
        let is_plus = self.push_compare(ComparisonOperator::Equal, first, plus)?;
        let after_sign = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, start, one, OverflowBehavior::Wrap)?;
        let negative_flag = self.push_const_int(1)?;
        let positive_flag = self.push_const_int(0)?;
        // (digit start, sign flag) after the optional sign.
        let digits = self.new_block(&[ValueType::Size, ValueType::Int])?;
        let when_minus = self.new_block(&[])?;
        let check_plus = self.new_block(&[])?;
        let when_plus = self.new_block(&[])?;
        let no_sign = self.new_block(&[])?;
        self.finish_current(Terminator::BranchIf {
            condition: is_minus,
            then_target: when_minus,
            then_arguments: Vec::new(),
            else_target: check_plus,
            else_arguments: Vec::new(),
        })?;
        self.current = when_minus;
        self.finish_current(Terminator::Branch { target: digits, arguments: vec![after_sign, negative_flag] })?;
        self.current = check_plus;
        self.finish_current(Terminator::BranchIf {
            condition: is_plus,
            then_target: when_plus,
            then_arguments: Vec::new(),
            else_target: no_sign,
            else_arguments: Vec::new(),
        })?;
        self.current = when_plus;
        self.finish_current(Terminator::Branch { target: digits, arguments: vec![after_sign, positive_flag] })?;
        self.current = no_sign;
        self.finish_current(Terminator::Branch { target: digits, arguments: vec![start, positive_flag] })?;
        let digit_values = self.block_parameter_values(digits);
        self.current = digits;
        let (digit_start, sign) = (digit_values[0], digit_values[1]);
        let has_digits = self.push_compare(ComparisonOperator::LessUnsigned, digit_start, end)?;
        self.push_trap_unless(has_digits)?;
        let zero_int = self.push_const_int(0)?;
        let ten = self.push_const_int(10)?;
        let ascii_zero = self.push_const_size(48)?;
        let nine = self.push_const_size(9)?;
        let cursor_start = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, digit_start, zero, OverflowBehavior::Wrap)?;
        let header = self.new_block(&[ValueType::Size, ValueType::Int])?;
        let body = self.new_block(&[ValueType::Size, ValueType::Int])?;
        let exit = self.new_block(&[ValueType::Int])?;
        self.finish_current(Terminator::Branch { target: header, arguments: vec![cursor_start, zero_int] })?;
        let header_values = self.block_parameter_values(header);
        self.current = header;
        let more = self.push_compare(ComparisonOperator::LessUnsigned, header_values[0], end)?;
        self.finish_current(Terminator::BranchIf {
            condition: more,
            then_target: body,
            then_arguments: header_values.clone(),
            else_target: exit,
            else_arguments: vec![header_values[1]],
        })?;
        let body_values = self.block_parameter_values(body);
        self.current = body;
        let (cursor, accumulator) = (body_values[0], body_values[1]);
        let cursor_in_bounds = self.push_compare(ComparisonOperator::LessUnsigned, cursor, length)?;
        self.push_guard(cursor_in_bounds, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let byte = self.push_string_byte_load(object, cursor, layout)?;
        let digit = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Subtract, byte, ascii_zero, OverflowBehavior::Wrap)?;
        let is_digit = self.push_compare(ComparisonOperator::LessEqualUnsigned, digit, nine)?;
        self.push_trap_unless(is_digit)?;
        let digit_int = self.fresh(ValueType::Int)?;
        self.push_operation(Operation::WidenSize { destination: digit_int, value: digit });
        // `acc * 10 - digit` stays in range iff `acc >= (MIN + digit) / 10`
        // (truncating division rounds the negative bound up), checked
        // before the wrapping arithmetic.
        let minimum = self.push_const_int(i64::MIN)?;
        let bound_numerator = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Add, minimum, digit_int, OverflowBehavior::Wrap)?;
        let bound = self.push_arithmetic(ValueType::Int, ArithmeticOperator::DivideSigned, bound_numerator, ten, OverflowBehavior::Wrap)?;
        let in_range = self.push_compare(ComparisonOperator::GreaterEqualSigned, accumulator, bound)?;
        self.push_trap_unless(in_range)?;
        let scaled = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Multiply, accumulator, ten, OverflowBehavior::Wrap)?;
        let next_accumulator = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Subtract, scaled, digit_int, OverflowBehavior::Wrap)?;
        let next_cursor = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, cursor, one, OverflowBehavior::Wrap)?;
        self.finish_current(Terminator::Branch { target: header, arguments: vec![next_cursor, next_accumulator] })?;
        self.current = exit;
        let negative_magnitude = self.block_parameter_values(exit)[0];
        // sign 1: keep the negative accumulator; sign 0: negate it, which
        // traps only for the minimum (one past the positive bound): the
        // accumulator differs from the minimum or the sign is negative.
        let minimum = self.push_const_int(i64::MIN)?;
        let sixty_three = self.push_const_int(63)?;
        let from_minimum = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Subtract, negative_magnitude, minimum, OverflowBehavior::Wrap)?;
        let zero_int = self.push_const_int(0)?;
        let negated_distance = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Subtract, zero_int, from_minimum, OverflowBehavior::Wrap)?;
        let either = self.push_arithmetic(ValueType::Int, ArithmeticOperator::BitOr, from_minimum, negated_distance, OverflowBehavior::Wrap)?;
        let not_minimum = self.push_arithmetic(ValueType::Int, ArithmeticOperator::ShiftRightUnsigned, either, sixty_three, OverflowBehavior::Wrap)?;
        let representable = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Add, not_minimum, sign, OverflowBehavior::Wrap)?;
        let one_int = self.push_const_int(1)?;
        let fits = self.push_compare(ComparisonOperator::GreaterEqualSigned, representable, one_int)?;
        self.push_trap_unless(fits)?;
        let two = self.push_const_int(2)?;
        let doubled = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Multiply, sign, two, OverflowBehavior::Wrap)?;
        let factor = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Subtract, doubled, one_int, OverflowBehavior::Wrap)?;
        self.push_arithmetic(ValueType::Int, ArithmeticOperator::Multiply, negative_magnitude, factor, OverflowBehavior::Wrap)
    }

    /// `string.starts_with` / `string.ends_with`: the affix fits and its
    /// bytes match at the start or at the end.
    fn lower_string_affix(
        &mut self,
        object: ValueId,
        affix: ValueId,
        at_start: bool,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let length = self.string_length_of(object)?;
        let affix_length = self.string_length_of(affix)?;
        let differ = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::ConstBool { destination: differ, value: false });
        let zero = self.push_const_size(0)?;
        let fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, affix_length, length)?;
        let compare = self.new_block(&[])?;
        let exit = self.new_block(&[ValueType::Bool])?;
        self.finish_current(Terminator::BranchIf {
            condition: fits,
            then_target: compare,
            then_arguments: Vec::new(),
            else_target: exit,
            else_arguments: vec![differ],
        })?;
        self.current = compare;
        let start = if at_start {
            zero
        } else {
            self.push_arithmetic(ValueType::Size, ArithmeticOperator::Subtract, length, affix_length, OverflowBehavior::Wrap)?
        };
        let matched = self.lower_string_range_equal(object, start, affix, zero, affix_length)?;
        self.finish_current(Terminator::Branch { target: exit, arguments: vec![matched] })?;
        self.current = exit;
        Ok(self.block_parameter_values(exit)[0])
    }

    /// `string.contains`: some window of the needle's length matches.
    fn lower_string_contains(&mut self, object: ValueId, needle: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        let length = self.string_length_of(object)?;
        let needle_length = self.string_length_of(needle)?;
        let found = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::ConstBool { destination: found, value: true });
        let missing = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::ConstBool { destination: missing, value: false });
        let zero = self.push_const_size(0)?;
        let one = self.push_const_size(1)?;
        let fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, needle_length, length)?;
        let scan = self.new_block(&[])?;
        let exit = self.new_block(&[ValueType::Bool])?;
        self.finish_current(Terminator::BranchIf {
            condition: fits,
            then_target: scan,
            then_arguments: Vec::new(),
            else_target: exit,
            else_arguments: vec![missing],
        })?;
        self.current = scan;
        // Windows start at 0 ..= length - needle_length.
        let last_start = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Subtract, length, needle_length, OverflowBehavior::Wrap)?;
        let header = self.new_block(&[ValueType::Size])?;
        let body = self.new_block(&[ValueType::Size])?;
        self.finish_current(Terminator::Branch { target: header, arguments: vec![zero] })?;
        let cursor = self.block_parameter_values(header)[0];
        self.current = header;
        let more = self.push_compare(ComparisonOperator::LessEqualUnsigned, cursor, last_start)?;
        self.finish_current(Terminator::BranchIf {
            condition: more,
            then_target: body,
            then_arguments: vec![cursor],
            else_target: exit,
            else_arguments: vec![missing],
        })?;
        let cursor = self.block_parameter_values(body)[0];
        self.current = body;
        let matched = self.lower_string_range_equal(object, cursor, needle, zero, needle_length)?;
        let next = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, cursor, one, OverflowBehavior::Wrap)?;
        let advance = self.new_block(&[])?;
        self.finish_current(Terminator::BranchIf {
            condition: matched,
            then_target: exit,
            then_arguments: vec![found],
            else_target: advance,
            else_arguments: Vec::new(),
        })?;
        self.current = advance;
        self.finish_current(Terminator::Branch { target: header, arguments: vec![next] })?;
        self.current = exit;
        Ok(self.block_parameter_values(exit)[0])
    }

    /// The lexicographic order of two strings as `-1`, `0`, or `1`: the
    /// sign of the first differing byte, else of the length difference.
    fn lower_string_order(&mut self, left: ValueId, right: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        let layout = self.string_layout_id()?;
        let left_length = self.string_length_of(left)?;
        let right_length = self.string_length_of(right)?;
        let zero = self.push_const_size(0)?;
        let one = self.push_const_size(1)?;
        let left_int = self.fresh(ValueType::Int)?;
        self.push_operation(Operation::WidenSize { destination: left_int, value: left_length });
        let right_int = self.fresh(ValueType::Int)?;
        self.push_operation(Operation::WidenSize { destination: right_int, value: right_length });
        let length_difference = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Subtract, left_int, right_int, OverflowBehavior::Wrap)?;
        let by_length = self.push_sign(length_difference)?;
        // The shared prefix length is the shorter length: min(a, b) = b + ((a - b) & ((a - b) >> 63)).
        let sixty_three = self.push_const_int(63)?;
        let mask = self.push_arithmetic(ValueType::Int, ArithmeticOperator::ShiftRightSigned, length_difference, sixty_three, OverflowBehavior::Wrap)?;
        let masked = self.push_arithmetic(ValueType::Int, ArithmeticOperator::BitAnd, length_difference, mask, OverflowBehavior::Wrap)?;
        let shared_int = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Add, right_int, masked, OverflowBehavior::Wrap)?;
        let shared = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::NarrowSize { destination: shared, value: shared_int });
        let header = self.new_block(&[ValueType::Size])?;
        let body = self.new_block(&[ValueType::Size])?;
        let exit = self.new_block(&[ValueType::Int])?;
        self.finish_current(Terminator::Branch { target: header, arguments: vec![zero] })?;
        let cursor = self.block_parameter_values(header)[0];
        self.current = header;
        let more = self.push_compare(ComparisonOperator::LessUnsigned, cursor, shared)?;
        self.finish_current(Terminator::BranchIf {
            condition: more,
            then_target: body,
            then_arguments: vec![cursor],
            else_target: exit,
            else_arguments: vec![by_length],
        })?;
        let cursor = self.block_parameter_values(body)[0];
        self.current = body;
        let left_in_bounds = self.push_compare(ComparisonOperator::LessUnsigned, cursor, left_length)?;
        self.push_guard(left_in_bounds, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let right_in_bounds = self.push_compare(ComparisonOperator::LessUnsigned, cursor, right_length)?;
        self.push_guard(right_in_bounds, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let left_byte = self.push_string_byte_load(left, cursor, layout)?;
        let right_byte = self.push_string_byte_load(right, cursor, layout)?;
        let left_byte_int = self.fresh(ValueType::Int)?;
        self.push_operation(Operation::WidenSize { destination: left_byte_int, value: left_byte });
        let right_byte_int = self.fresh(ValueType::Int)?;
        self.push_operation(Operation::WidenSize { destination: right_byte_int, value: right_byte });
        let byte_difference = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Subtract, left_byte_int, right_byte_int, OverflowBehavior::Wrap)?;
        let by_byte = self.push_sign(byte_difference)?;
        let same = self.push_compare(ComparisonOperator::Equal, left_byte, right_byte)?;
        let next = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, cursor, one, OverflowBehavior::Wrap)?;
        self.finish_current(Terminator::BranchIf {
            condition: same,
            then_target: header,
            then_arguments: vec![next],
            else_target: exit,
            else_arguments: vec![by_byte],
        })?;
        self.current = exit;
        Ok(self.block_parameter_values(exit)[0])
    }

    /// `string.char_count`: the number of UTF-8 sequence starts, counted
    /// without a branch as `(((byte >> 6) ^ 2) + 3) >> 2`, which is zero for
    /// a continuation byte (`10xxxxxx`) and one otherwise.
    fn lower_char_count(&mut self, object: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        let length = self.string_length_of(object)?;
        self.lower_char_count_prefix(object, length)
    }

    /// The number of UTF-8 sequence starts in the first `length` bytes.
    fn lower_char_count_prefix(&mut self, object: ValueId, length: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        let ValueType::Ref(layout) = self.values[&object] else {
            return Err(CanonicalLoweringError::UnsupportedExpression("string.char_count receiver"));
        };
        let total = self.string_length_of(object)?;
        let zero = self.push_const_size(0)?;
        let one = self.push_const_size(1)?;
        let zero_int = self.push_const_int(0)?;
        let two_int = self.push_const_int(2)?;
        let three_int = self.push_const_int(3)?;
        let six_int = self.push_const_int(6)?;
        let state = [ValueType::Size, ValueType::Int];
        let header = self.new_block(&state)?;
        let body = self.new_block(&state)?;
        let exit = self.new_block(&[ValueType::Int])?;
        self.finish_current(Terminator::Branch { target: header, arguments: vec![zero, zero_int] })?;
        let header_values = self.block_parameter_values(header);
        self.current = header;
        let more = self.push_compare(ComparisonOperator::LessUnsigned, header_values[0], length)?;
        self.finish_current(Terminator::BranchIf {
            condition: more,
            then_target: body,
            then_arguments: header_values.clone(),
            else_target: exit,
            else_arguments: vec![header_values[1]],
        })?;
        let body_values = self.block_parameter_values(body);
        self.current = body;
        let index = body_values[0];
        let in_bounds = self.push_compare(ComparisonOperator::LessUnsigned, index, total)?;
        self.push_guard(in_bounds, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let byte = self.push_string_byte_load(object, index, layout)?;
        let byte_int = self.fresh(ValueType::Int)?;
        self.push_operation(Operation::WidenSize { destination: byte_int, value: byte });
        let high_bits = self.push_arithmetic(ValueType::Int, ArithmeticOperator::ShiftRightUnsigned, byte_int, six_int, OverflowBehavior::Wrap)?;
        let flipped = self.push_arithmetic(ValueType::Int, ArithmeticOperator::BitXor, high_bits, two_int, OverflowBehavior::Wrap)?;
        let biased = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Add, flipped, three_int, OverflowBehavior::Wrap)?;
        let increment = self.push_arithmetic(ValueType::Int, ArithmeticOperator::ShiftRightUnsigned, biased, two_int, OverflowBehavior::Wrap)?;
        let count = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Add, body_values[1], increment, OverflowBehavior::Wrap)?;
        let next = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, index, one, OverflowBehavior::Wrap)?;
        self.finish_current(Terminator::Branch { target: header, arguments: vec![next, count] })?;
        self.current = exit;
        Ok(self.block_parameter_values(exit)[0])
    }

    /// The extent facts cached before a region whose blocks do not dominate
    /// the code after it (the arms of an `if`/`match`, a short-circuit right
    /// operand, a loop body). A header length loaded inside one arm is not
    /// available in the other arm or after the join; keeping it cached once
    /// produced a use the value did not dominate.
    fn snapshot_facts(&self) -> FactSnapshot {
        FactSnapshot {
            string_lengths: self.string_lengths.clone(),
            string_capacities: self.string_capacities.clone(),
            list_counts: self.list_counts.clone(),
            list_capacities: self.list_capacities.clone(),
            known_list_lengths: self.known_list_lengths.clone(),
        }
    }

    fn restore_snapshot(&mut self, snapshot: FactSnapshot) {
        self.string_lengths = snapshot.string_lengths;
        self.string_capacities = snapshot.string_capacities;
        self.list_counts = snapshot.list_counts;
        self.list_capacities = snapshot.list_capacities;
        self.known_list_lengths = snapshot.known_list_lengths;
    }

    /// Add another arm's facts for values the current facts do not cover.
    fn absorb_facts(&mut self, other: FactSnapshot) {
        fn absorb<V>(current: &mut HashMap<ValueId, V>, other: HashMap<ValueId, V>) {
            for (key, value) in other {
                current.entry(key).or_insert(value);
            }
        }
        absorb(&mut self.string_lengths, other.string_lengths);
        absorb(&mut self.string_capacities, other.string_capacities);
        absorb(&mut self.list_counts, other.list_counts);
        absorb(&mut self.list_capacities, other.list_capacities);
        absorb(&mut self.known_list_lengths, other.known_list_lengths);
    }

    /// Restore the facts from before a region, keeping only the entries the
    /// region's join block itself defines (its parameters' lanes).
    fn restore_facts(&mut self, snapshot: FactSnapshot, join: BlockId) {
        let defined = self.blocks[join.get() as usize]
            .parameters
            .iter()
            .map(|parameter| parameter.value)
            .collect::<BTreeSet<_>>();
        fn merge<V: Clone>(current: &mut HashMap<ValueId, V>, before: HashMap<ValueId, V>, defined: &BTreeSet<ValueId>) {
            let kept = current
                .iter()
                .filter(|(key, _)| defined.contains(*key))
                .map(|(key, value)| (*key, value.clone()))
                .collect::<Vec<_>>();
            *current = before;
            current.extend(kept);
        }
        merge(&mut self.string_lengths, snapshot.string_lengths, &defined);
        merge(&mut self.string_capacities, snapshot.string_capacities, &defined);
        merge(&mut self.list_counts, snapshot.list_counts, &defined);
        merge(&mut self.list_capacities, snapshot.list_capacities, &defined);
        merge(&mut self.known_list_lengths, snapshot.known_list_lengths, &defined);
    }

    /// A fresh reference-counted string holding bytes `[start, start + length)`
    /// of `source`. The caller has established `start + length <= count` as a
    /// trapping add followed by a guard.
    fn copy_string_slice(
        &mut self,
        source: ValueId,
        start: ValueId,
        length: ValueId,
        layout: witchy_wir::layout::LayoutId,
    ) -> Result<ValueId, CanonicalLoweringError> {
        // The copy's extent is its own value: sharing the caller's length
        // id with the source's extent would rename both together at an
        // edge that carries the copy's lanes.
        let extent = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::MoveValue { destination: extent, source: length });
        let maximum = self.push_const_size(u64::from(u32::MAX - 4))?;
        let fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, extent, maximum)?;
        self.push_guard(fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let object = self.fresh(ValueType::Ref(layout))?;
        self.push_operation(Operation::Alloc {
            destination: object,
            layout,
            extent: witchy_wir::canonical_safewir::Extent::Counted {
                header_bytes: 4,
                count: extent,
                capacity: extent,
                stride_bytes: 1,
            },
            class: witchy_wir::canonical_safewir::AllocationClass::ReferenceCounted,
        });
        self.push_operation(Operation::UpdateCount { object, count: extent });
        let zero = self.push_const_size(0)?;
        let destination_fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, length, extent)?;
        self.push_guard(destination_fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
        self.push_byte_copy(object, zero, source, start, length, layout);
        self.string_lengths.insert(object, extent);
        self.string_capacities.insert(object, extent);
        Ok(object)
    }

    /// Continue only when `condition` holds; otherwise trap. The trap is a
    /// separate terminal block, so no guard-kind rule constrains the
    /// condition's shape.
    fn push_trap_unless(&mut self, condition: ValueId) -> Result<(), CanonicalLoweringError> {
        let ok = self.new_block(&[])?;
        let trap = self.new_block(&[])?;
        self.finish_current(Terminator::BranchIf {
            condition,
            then_target: ok,
            then_arguments: Vec::new(),
            else_target: trap,
            else_arguments: Vec::new(),
        })?;
        self.current = trap;
        self.finish_current(Terminator::Trap)?;
        self.current = ok;
        Ok(())
    }

    /// `max(value, 0)` for an `Int`, branch-free: subtract the value masked by
    /// its own sign.
    fn push_clamp_non_negative(&mut self, value: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        let sign_bit = self.push_const_int(63)?;
        let mask = self.push_arithmetic(ValueType::Int, ArithmeticOperator::ShiftRightSigned, value, sign_bit, OverflowBehavior::Wrap)?;
        let negative_part = self.push_arithmetic(ValueType::Int, ArithmeticOperator::BitAnd, value, mask, OverflowBehavior::Wrap)?;
        self.push_arithmetic(ValueType::Int, ArithmeticOperator::Subtract, value, negative_part, OverflowBehavior::Wrap)
    }

    /// The byte offset where character `target` (a non-negative `Int` count
    /// of Unicode scalars) starts, or the byte length when the string has
    /// fewer characters. A loop over bytes counts sequence starts and leaves
    /// at the start of character `target`.
    fn lower_char_offset(
        &mut self,
        object: ValueId,
        layout: witchy_wir::layout::LayoutId,
        length: ValueId,
        target: ValueId,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let zero = self.push_const_size(0)?;
        let one = self.push_const_size(1)?;
        let zero_int = self.push_const_int(0)?;
        let one_int = self.push_const_int(1)?;
        let two_int = self.push_const_int(2)?;
        let three_int = self.push_const_int(3)?;
        let six_int = self.push_const_int(6)?;
        let sixty_three = self.push_const_int(63)?;
        let state = [ValueType::Size, ValueType::Int];
        let header = self.new_block(&state)?;
        let check = self.new_block(&state)?;
        let exit = self.new_block(&[ValueType::Size])?;
        self.finish_current(Terminator::Branch { target: header, arguments: vec![zero, zero_int] })?;
        let header_values = self.block_parameter_values(header);
        self.current = header;
        let more = self.push_compare(ComparisonOperator::LessUnsigned, header_values[0], length)?;
        self.finish_current(Terminator::BranchIf {
            condition: more,
            then_target: check,
            then_arguments: header_values.clone(),
            else_target: exit,
            else_arguments: vec![header_values[0]],
        })?;
        let check_values = self.block_parameter_values(check);
        self.current = check;
        let index = check_values[0];
        let seen = check_values[1];
        let in_bounds = self.push_compare(ComparisonOperator::LessUnsigned, index, length)?;
        self.push_guard(in_bounds, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let byte = self.push_string_byte_load(object, index, layout)?;
        let byte_int = self.fresh(ValueType::Int)?;
        self.push_operation(Operation::WidenSize { destination: byte_int, value: byte });
        // start = 1 for a sequence start, 0 for a continuation byte.
        let high_bits = self.push_arithmetic(ValueType::Int, ArithmeticOperator::ShiftRightUnsigned, byte_int, six_int, OverflowBehavior::Wrap)?;
        let flipped = self.push_arithmetic(ValueType::Int, ArithmeticOperator::BitXor, high_bits, two_int, OverflowBehavior::Wrap)?;
        let biased = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Add, flipped, three_int, OverflowBehavior::Wrap)?;
        let start = self.push_arithmetic(ValueType::Int, ArithmeticOperator::ShiftRightUnsigned, biased, two_int, OverflowBehavior::Wrap)?;
        // at_target = 1 when seen == target: neither difference is negative.
        let forward = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Subtract, seen, target, OverflowBehavior::Wrap)?;
        let backward = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Subtract, target, seen, OverflowBehavior::Wrap)?;
        let either = self.push_arithmetic(ValueType::Int, ArithmeticOperator::BitOr, forward, backward, OverflowBehavior::Wrap)?;
        let differs = self.push_arithmetic(ValueType::Int, ArithmeticOperator::ShiftRightUnsigned, either, sixty_three, OverflowBehavior::Wrap)?;
        let at_target = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Subtract, one_int, differs, OverflowBehavior::Wrap)?;
        let found_int = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Multiply, start, at_target, OverflowBehavior::Wrap)?;
        let found = self.push_compare(ComparisonOperator::Equal, found_int, one_int)?;
        let next_seen = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Add, seen, start, OverflowBehavior::Wrap)?;
        let next_index = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, index, one, OverflowBehavior::Wrap)?;
        self.finish_current(Terminator::BranchIf {
            condition: found,
            then_target: exit,
            then_arguments: vec![index],
            else_target: header,
            else_arguments: vec![next_index, next_seen],
        })?;
        self.current = exit;
        Ok(self.block_parameter_values(exit)[0])
    }

    /// `string.substring(s, start, end)`: the characters in `[start, end)`,
    /// counted by Unicode scalar and clamped to the string, empty when the
    /// range is inverted. Two scans find the byte offsets; the bytes between
    /// them are copied into a fresh string.
    fn lower_substring(
        &mut self,
        object: ValueId,
        start: ValueId,
        end: ValueId,
    ) -> Result<ValueId, CanonicalLoweringError> {
        self.ensure_string_extent(object)?;
        let ValueType::Ref(layout) = self.values[&object] else {
            return Err(CanonicalLoweringError::UnsupportedExpression("string.substring receiver"));
        };
        let length = *self.string_lengths.get(&object).ok_or(
            CanonicalLoweringError::UnsupportedExpression("string.substring receiver extent"),
        )?;
        let start = self.push_clamp_non_negative(start)?;
        let end = self.push_clamp_non_negative(end)?;
        let start_offset = self.lower_char_offset(object, layout, length, start)?;
        let end_offset = self.lower_char_offset(object, layout, length, end)?;
        let start_int = self.fresh(ValueType::Int)?;
        self.push_operation(Operation::WidenSize { destination: start_int, value: start_offset });
        let end_int = self.fresh(ValueType::Int)?;
        self.push_operation(Operation::WidenSize { destination: end_int, value: end_offset });
        let span = self.push_arithmetic(ValueType::Int, ArithmeticOperator::Subtract, end_int, start_int, OverflowBehavior::Wrap)?;
        let span = self.push_clamp_non_negative(span)?;
        let byte_count = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::NarrowSize { destination: byte_count, value: span });
        let slice_end = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, start_offset, byte_count, OverflowBehavior::Trap)?;
        let slice_fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, slice_end, length)?;
        self.push_guard(slice_fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
        self.copy_string_slice(object, start_offset, byte_count, layout)
    }

    /// Scan over ASCII whitespace (space, tab, LF, VT, FF, CR: the set both
    /// backends trim). Forward from `from` while the byte at the cursor is
    /// whitespace and the cursor is below `to`; backward from `to` while the
    /// byte before the cursor is whitespace and the cursor is above `from`.
    /// Yields the cursor where the scan stopped.
    fn lower_whitespace_scan(
        &mut self,
        object: ValueId,
        layout: witchy_wir::layout::LayoutId,
        length: ValueId,
        from: ValueId,
        to: ValueId,
        forward: bool,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let one = self.push_const_size(1)?;
        let space = self.push_const_size(32)?;
        let tab = self.push_const_size(9)?;
        let control_span = self.push_const_size(4)?;
        let header = self.new_block(&[ValueType::Size])?;
        let check = self.new_block(&[ValueType::Size])?;
        let advance = self.new_block(&[ValueType::Size])?;
        let exit = self.new_block(&[ValueType::Size])?;
        // The checker renames a loop argument to the header parameter in
        // every fact table, so the cursor starts from a dedicated copy and the
        // bounds (`from`, `to`, `length`) keep their facts.
        let zero = self.push_const_size(0)?;
        let initial = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, if forward { from } else { to }, zero, OverflowBehavior::Wrap)?;
        self.finish_current(Terminator::Branch { target: header, arguments: vec![initial] })?;
        let cursor = self.block_parameter_values(header)[0];
        self.current = header;
        let more = if forward {
            self.push_compare(ComparisonOperator::LessUnsigned, cursor, to)?
        } else {
            self.push_compare(ComparisonOperator::GreaterUnsigned, cursor, from)?
        };
        self.finish_current(Terminator::BranchIf {
            condition: more,
            then_target: check,
            then_arguments: vec![cursor],
            else_target: exit,
            else_arguments: vec![cursor],
        })?;
        let cursor = self.block_parameter_values(check)[0];
        self.current = check;
        let index = if forward {
            cursor
        } else {
            self.push_arithmetic(ValueType::Size, ArithmeticOperator::Subtract, cursor, one, OverflowBehavior::Wrap)?
        };
        let in_bounds = self.push_compare(ComparisonOperator::LessUnsigned, index, length)?;
        self.push_guard(in_bounds, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let byte = self.push_string_byte_load(object, index, layout)?;
        let is_space = self.push_compare(ComparisonOperator::Equal, byte, space)?;
        let from_tab = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Subtract, byte, tab, OverflowBehavior::Wrap)?;
        let is_control = self.push_compare(ComparisonOperator::LessEqualUnsigned, from_tab, control_span)?;
        let control_check = self.new_block(&[])?;
        self.finish_current(Terminator::BranchIf {
            condition: is_space,
            then_target: advance,
            then_arguments: vec![cursor],
            else_target: control_check,
            else_arguments: Vec::new(),
        })?;
        self.current = control_check;
        self.finish_current(Terminator::BranchIf {
            condition: is_control,
            then_target: advance,
            then_arguments: vec![cursor],
            else_target: exit,
            else_arguments: vec![cursor],
        })?;
        let cursor = self.block_parameter_values(advance)[0];
        self.current = advance;
        let next = if forward {
            self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, cursor, one, OverflowBehavior::Wrap)?
        } else {
            self.push_arithmetic(ValueType::Size, ArithmeticOperator::Subtract, cursor, one, OverflowBehavior::Wrap)?
        };
        self.finish_current(Terminator::Branch { target: header, arguments: vec![next] })?;
        self.current = exit;
        Ok(self.block_parameter_values(exit)[0])
    }

    /// `string.trim`: the bytes between the leading and trailing ASCII
    /// whitespace, copied into a fresh string.
    fn lower_trim(&mut self, object: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        self.ensure_string_extent(object)?;
        let ValueType::Ref(layout) = self.values[&object] else {
            return Err(CanonicalLoweringError::UnsupportedExpression("string.trim receiver"));
        };
        let length = *self.string_lengths.get(&object).ok_or(
            CanonicalLoweringError::UnsupportedExpression("string.trim receiver extent"),
        )?;
        let zero = self.push_const_size(0)?;
        let start = self.lower_whitespace_scan(object, layout, length, zero, length, true)?;
        let end = self.lower_whitespace_scan(object, layout, length, start, length, false)?;
        let byte_count = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Subtract, end, start, OverflowBehavior::Wrap)?;
        let slice_end = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, start, byte_count, OverflowBehavior::Trap)?;
        let slice_fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, slice_end, length)?;
        self.push_guard(slice_fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
        self.copy_string_slice(object, start, byte_count, layout)
    }

    /// A block whose parameters carry the given ownership (a loop header
    /// lane that owns a reference-count token is `Some(Unique)`).
    fn new_block_owned(
        &mut self,
        parameters: &[(ValueType, Option<Ownership>)],
    ) -> Result<BlockId, CanonicalLoweringError> {
        let id = self.new_block(&parameters.iter().map(|(value_type, _)| *value_type).collect::<Vec<_>>())?;
        for (parameter, (_, ownership)) in self.blocks[id.get() as usize].parameters.iter_mut().zip(parameters) {
            parameter.ownership = *ownership;
        }
        Ok(id)
    }

    /// Record the extent lanes of an accumulator's block parameters: a list
    /// or string lane is `[object, count, capacity]`.
    fn track_accumulator_lanes(&mut self, values: &[ValueId]) {
        if let [object, count, capacity, ..] = values
            && matches!(self.values.get(object), Some(ValueType::Ref(_)))
        {
            self.set_tracked_lanes(*object, *count, *capacity);
        }
    }

    fn block_parameter_values(&self, block: BlockId) -> Vec<ValueId> {
        self.blocks[block.get() as usize].parameters.iter().map(|parameter| parameter.value).collect()
    }

    fn push_const_int(&mut self, value: i64) -> Result<ValueId, CanonicalLoweringError> {
        let destination = self.fresh(ValueType::Int)?;
        self.push_operation(Operation::ConstInt { destination, value });
        Ok(destination)
    }

    fn push_const_size(&mut self, value: u64) -> Result<ValueId, CanonicalLoweringError> {
        let destination = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::ConstSize { destination, value });
        Ok(destination)
    }

    fn push_arithmetic(
        &mut self,
        value_type: ValueType,
        operator: ArithmeticOperator,
        left: ValueId,
        right: ValueId,
        overflow: OverflowBehavior,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let destination = self.fresh(value_type)?;
        self.push_operation(Operation::Arithmetic { destination, operator, left, right, overflow });
        Ok(destination)
    }

    fn push_compare(
        &mut self,
        operator: ComparisonOperator,
        left: ValueId,
        right: ValueId,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let destination = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::Compare { destination, operator, left, right });
        Ok(destination)
    }

    fn push_guard(&mut self, condition: ValueId, kind: witchy_wir::canonical_safewir::GuardKind) {
        self.push_operation(Operation::Guard { condition, kind });
    }

    /// `destination[start..start+length] = source[source_start..+length]` over
    /// raw bytes. The caller has already pushed the guards the checker's range
    /// rule needs for both slices.
    fn push_byte_copy(
        &mut self,
        destination: ValueId,
        start: ValueId,
        source: ValueId,
        source_start: ValueId,
        length: ValueId,
        layout: witchy_wir::layout::LayoutId,
    ) {
        self.push_operation(Operation::CopyBytes {
            destination: witchy_wir::canonical_safewir::Place::projected(
                destination,
                Projection::Slice { start, length },
                ValueType::Ref(layout),
                AccessMode::Write,
            ),
            source: witchy_wir::canonical_safewir::Place::projected(
                source,
                Projection::Slice { start: source_start, length },
                ValueType::Ref(layout),
                AccessMode::Read,
            ),
            length,
        });
    }
}

/// A checked type name may carry the entry-module qualifier the constructor
/// spelling lacks (`entry.Point` versus `Point`).
fn same_record_name(checked: &str, constructor: &str) -> bool {
    checked == constructor
        || checked.strip_suffix(constructor).is_some_and(|prefix| prefix.ends_with('.'))
        || constructor.strip_suffix(checked).is_some_and(|prefix| prefix.ends_with('.'))
}

/// Static payloads the `Int` renderer copies digits and the sign from.
const RENDER_DIGIT_TABLE: &str = "0123456789";
const RENDER_MINUS_SIGN: &str = "-";
/// The two `Bool` renderings, back to back: `false` at 0, `true` at 5.
const RENDER_BOOL_TABLE: &str = "falsetrue";

fn scalar_type(source: &Type) -> Result<ValueType, CanonicalLoweringError> {
    match source.unqualified() {
        Type::Named(name, arguments) if arguments.is_empty() => match name.as_str() {
            "Int" | "Duration" => Ok(ValueType::Int),
            "Float" => Ok(ValueType::Float),
            "Bool" => Ok(ValueType::Bool),
            "Nil" | "Unit" => Ok(ValueType::Unit),
            _ => Err(CanonicalLoweringError::UnsupportedType(source.clone())),
        },
        _ => Err(CanonicalLoweringError::UnsupportedType(source.clone())),
    }
}

fn source_value_type(
    source: &Type,
    layout_bundle: &LayoutBundle,
    layouts: &LayoutInterner,
    closure_layouts: &HashMap<Type, witchy_wir::layout::LayoutId>,
    definitions: &BTreeMap<String, TypeDef>,
) -> Result<ValueType, CanonicalLoweringError> {
    if let Some(shape) = scalar_aggregate_shape(source, definitions)
        && !shape.packed
    {
        // A scalar aggregate (record, sum, scalar `Option`/`Result`) is one
        // owned reference; a packed record stays an inline aggregate, which
        // the ABI does not carry (`scalar_type` refuses it below).
        return aggregate_layout(&shape.fields, layout_bundle, layouts, definitions).map(ValueType::Ref);
    }
    match source.unqualified() {
        // A `fn` value is a closure record.
        Type::Fn(..) => closure_layouts
            .get(&crate::canonical_layouts::closure_key(source))
            .copied()
            .map(ValueType::Ref)
            .ok_or_else(|| CanonicalLoweringError::UnsupportedType(source.clone())),
        Type::Named(name, arguments) if name == "List" && arguments.len() == 1 => {
            list_layout(source, layout_bundle, layouts, definitions).map(|(layout, _, _, _)| ValueType::Ref(layout))
        }
        Type::Named(name, arguments) if name == "Dict" && arguments.len() == 2 => {
            let tuple = dictionary_as_tuple(source).expect("a two-argument Dict has a tuple form");
            source_value_type(&tuple, layout_bundle, layouts, closure_layouts, definitions)
        }
        Type::Named(name, arguments) if name == "String" && arguments.is_empty() => {
            string_layout(source, layout_bundle, layouts).map(ValueType::Ref)
        }
        Type::Named(name, arguments) if name == "Console" && arguments.is_empty() => {
            Ok(ValueType::Capability(witchy_wir::canonical_safewir::CapabilityKindId::new(0)))
        }
        _ => scalar_type(source),
    }
}

/// The interned record layout whose slots are exactly `expected_fields`: a
/// scalar in its slot, a string or list behind an owned pointer slot.
fn aggregate_layout(
    expected_fields: &[Type],
    layout_bundle: &LayoutBundle,
    layouts: &LayoutInterner,
    definitions: &BTreeMap<String, TypeDef>,
) -> Result<witchy_wir::layout::LayoutId, CanonicalLoweringError> {
    let mut expected = Vec::with_capacity(expected_fields.len());
    for field in expected_fields {
        let slot = if let Ok(scalar) = scalar_type(field) {
            (Some(scalar), None)
        } else {
            let pointee = match field.unqualified() {
                Type::Named(name, arguments) if name == "String" && arguments.is_empty() => {
                    string_layout(field, layout_bundle, layouts)?
                }
                Type::Named(name, arguments) if name == "List" && arguments.len() == 1 => {
                    list_layout(field, layout_bundle, layouts, definitions)?.0
                }
                _ => match scalar_aggregate_shape(field, definitions) {
                    Some(shape) if !shape.packed => aggregate_layout(&shape.fields, layout_bundle, layouts, definitions)?,
                    _ => return Err(CanonicalLoweringError::UnsupportedType(field.clone())),
                },
            };
            (None, Some(pointee))
        };
        expected.push(slot);
    }
    let mut candidates = Vec::new();
    for root in layout_bundle.roots() {
        let Some(descriptor) = layouts.get(*root) else { continue };
        let LayoutKind::PackedRecord { fields } = descriptor.kind() else { continue };
        if fields.len() != expected.len() {
            continue;
        }
        let matches = fields.iter().zip(&expected).all(|(field, (scalar, pointee))| {
            match (layouts.get(*field).map(|field| field.kind()), scalar, pointee) {
                (Some(LayoutKind::Scalar(ScalarKind::Int | ScalarKind::Duration)), Some(ValueType::Int), None) => true,
                (Some(LayoutKind::Scalar(ScalarKind::Float)), Some(ValueType::Float), None) => true,
                (Some(LayoutKind::Scalar(ScalarKind::Bool)), Some(ValueType::Bool), None) => true,
                (
                    Some(LayoutKind::RuntimePointerValueSlot { pointee: actual, semantics: PointerSlot::Owned }),
                    None,
                    Some(pointee),
                ) => actual == pointee,
                _ => false,
            }
        });
        if matches {
            candidates.push(*root);
        }
    }
    match candidates.as_slice() {
        [layout] => Ok(*layout),
        [] => Err(CanonicalLoweringError::MissingLayout(Type::Tuple(expected_fields.to_vec()))),
        _ => Err(CanonicalLoweringError::AmbiguousLayout(Type::Tuple(expected_fields.to_vec()))),
    }
}

fn packed_scalar_layout(
    expected_fields: &[Type],
    layout_bundle: &LayoutBundle,
    layouts: &LayoutInterner,
) -> Result<witchy_wir::layout::LayoutId, CanonicalLoweringError> {
    let mut candidates = Vec::new();
    for root in layout_bundle.roots() {
        let Some(descriptor) = layouts.get(*root) else { continue };
        let fields = match descriptor.kind() {
            LayoutKind::PackedRecord { fields } | LayoutKind::Tuple { fields } => fields,
            _ => continue,
        };
        if fields.len() == expected_fields.len() && fields.iter().zip(expected_fields).all(|(field, expected)| {
            let actual = match layouts.get(*field).map(|field| field.kind()) {
                Some(LayoutKind::Scalar(ScalarKind::Int | ScalarKind::Duration)) => ValueType::Int,
                Some(LayoutKind::Scalar(ScalarKind::Float)) => ValueType::Float,
                Some(LayoutKind::Scalar(ScalarKind::Bool)) => ValueType::Bool,
                _ => return false,
            };
            scalar_type(expected).is_ok_and(|expected| expected == actual)
        }) {
            candidates.push(*root);
        }
    }
    match candidates.as_slice() {
        [layout] => Ok(*layout),
        [] => Err(CanonicalLoweringError::UnsupportedExpression("packed scalar record layout")),
        _ => Err(CanonicalLoweringError::UnsupportedExpression("ambiguous packed scalar record layout")),
    }
}

/// A one-variant record with scalar fields: inline when `packed`, otherwise
/// an immutable reference-counted heap aggregate.
fn is_v1_scalar_record(definition: &TypeDef) -> bool {
    definition.params.is_empty()
        && definition.variants.len() == 1
        && definition.variants[0].fields.iter().all(|field| matches!(field.unqualified(),
            Type::Named(name, arguments) if arguments.is_empty() && matches!(name.as_str(), "Int" | "Float" | "Duration" | "Bool")
        ))
}

#[allow(dead_code)]
fn is_v1_packed_scalar_record(definition: &TypeDef) -> bool {
    definition.packed
        && definition.params.is_empty()
        && definition.variants.len() == 1
        && definition.variants[0].fields.iter().all(|field| matches!(field.unqualified(),
            Type::Named(name, arguments) if arguments.is_empty() && matches!(name.as_str(), "Int" | "Float" | "Duration" | "Bool")
        ))
}

fn is_indirect_scalar(value_type: ValueType) -> bool {
    matches!(
        value_type,
        ValueType::Int | ValueType::Float | ValueType::Bool | ValueType::Size | ValueType::V128
    )
}

fn is_indirect_result(value_type: ValueType) -> bool {
    is_indirect_scalar(value_type) || matches!(value_type, ValueType::Ref(_))
}

fn string_layout(
    source: &Type,
    layout_bundle: &LayoutBundle,
    layouts: &LayoutInterner,
) -> Result<witchy_wir::layout::LayoutId, CanonicalLoweringError> {
    let mut candidates = Vec::new();
    for root in layout_bundle.roots() {
        let Some(descriptor) = layouts.get(*root) else { continue };
        let LayoutKind::RuntimeBuffer { element } = descriptor.kind() else { continue };
        let Some(element_descriptor) = layouts.get(*element) else { continue };
        if !matches!(element_descriptor.kind(), LayoutKind::Scalar(ScalarKind::RawByte)) {
            continue;
        }
        if descriptor.size() != (LayoutSize::Dynamic { base: 4, stride: 1 }) {
            continue;
        }
        candidates.push(*root);
    }
    match candidates.as_slice() {
        [] => Err(CanonicalLoweringError::MissingLayout(source.clone())),
        [candidate] => Ok(*candidate),
        _ => Err(CanonicalLoweringError::AmbiguousLayout(source.clone())),
    }
}

/// The prelude imports the canonical ABI catalog projected for this module,
/// by the role the lowering reaches them through.
#[derive(Clone, Copy, Default)]
struct HostImports {
    print: Option<witchy_wir::canonical_safewir::ImportId>,
    float_to_str: Option<witchy_wir::canonical_safewir::ImportId>,
}

/// Whether any canonical source function renders a `Float`, which reaches
/// the `float_to_str` import.
fn renders_float(module: &witchy_syntax::ast::Module, table: &TypeTable, definitions: &BTreeMap<String, TypeDef>) -> bool {
    module.items.iter().any(|item| {
        let Item::Function(function) = item else { return false };
        if !is_canonical_source_function(module, &function.name) {
            return false;
        }
        let mut found = false;
        witchy_syntax::ast::visit::visit_block::<()>(&function.body, &mut |expression| {
            if let Expr::Call { name, args } = expression
                && witchy_syntax::ast::is_render_intrinsic(name)
                && let [argument] = args.as_slice()
                && let Some(ty) = table.type_of(argument).and_then(witchy_types::typeck::ty_to_ast)
            {
                found |= type_renders_float(&ty, definitions);
            }
            Ok(())
        }).expect("render visitor cannot fail");
        found
    })
}

/// Whether the structural render of `ty` reaches a `Float` anywhere inside:
/// the same walk as `collect_render_payloads`, so the import is projected
/// exactly when a render will call it.
fn type_renders_float(ty: &Type, definitions: &BTreeMap<String, TypeDef>) -> bool {
    let ty = ty.unqualified();
    if let Type::Named(name, arguments) = ty {
        match (name.as_str(), arguments.as_slice()) {
            ("Float", []) => return true,
            ("String", []) | ("Int", []) | ("Bool", []) | ("Duration", _) | ("Dict", _) => return false,
            ("List", [element]) => return type_renders_float(element, definitions),
            _ => {}
        }
    }
    scalar_aggregate_shape(ty, definitions).is_some_and(|shape| {
        shape.variants.iter().any(|variant| {
            (0..variant.arity).any(|offset| type_renders_float(&shape.fields[variant.first_field + offset], definitions))
        })
    })
}

/// The static payloads the structural render of a value of type `ty` reads
/// (`lower_render_typed`): list and tuple punctuation, a variant's name or
/// `Name(` opener, the `, ` between payloads, and the scalar tables of the
/// nested `Int` and `Bool` fields. Mirrors the lowering exactly: a segment
/// no function references is refused by the checker.
fn collect_render_payloads(
    ty: &Type,
    definitions: &BTreeMap<String, TypeDef>,
    payloads: &mut BTreeSet<Vec<u8>>,
    renders_int: &mut bool,
    renders_bool: &mut bool,
) {
    let ty = ty.unqualified();
    if let Type::Named(name, arguments) = ty {
        match (name.as_str(), arguments.as_slice()) {
            ("String", []) | ("Float", _) | ("Duration", _) => return,
            ("Int", []) => {
                *renders_int = true;
                return;
            }
            ("Bool", []) => {
                *renders_bool = true;
                return;
            }
            ("List", [element]) => {
                payloads.insert(b"[".to_vec());
                payloads.insert(b", ".to_vec());
                payloads.insert(b"]".to_vec());
                collect_render_payloads(element, definitions, payloads, renders_int, renders_bool);
                return;
            }
            _ => {}
        }
    }
    let Some(shape) = scalar_aggregate_shape(ty, definitions) else { return };
    let tuple = matches!(ty, Type::Tuple(_));
    for variant in &shape.variants {
        if tuple {
            payloads.insert(b"(".to_vec());
        } else if variant.arity == 0 {
            payloads.insert(render_variant_name(variant).into_bytes());
            continue;
        } else {
            payloads.insert(format!("{}(", render_variant_name(variant)).into_bytes());
        }
        if variant.arity > 1 {
            payloads.insert(b", ".to_vec());
        }
        payloads.insert(b")".to_vec());
        for offset in 0..variant.arity {
            collect_render_payloads(&shape.fields[variant.first_field + offset], definitions, payloads, renders_int, renders_bool);
        }
    }
}

/// The constructor name a render shows: the source spelling after any
/// module qualifier, as the interpreter prints it.
fn render_variant_name(variant: &ScalarVariant) -> String {
    if variant.name.starts_with('.') {
        return variant.name.clone();
    }
    variant.name.rsplit('.').next().unwrap_or(&variant.name).to_owned()
}

fn collect_string_payloads(module: &witchy_syntax::ast::Module, table: &TypeTable, definitions: &BTreeMap<String, TypeDef>) -> Vec<Vec<u8>> {
    let mut literal_uses = BTreeMap::<Vec<u8>, usize>::new();
    let mut chars_inputs = BTreeMap::<Vec<u8>, usize>::new();
    let mut chars_outputs = BTreeSet::new();
    let mut renders_int = false;
    let mut renders_bool = false;
    let mut render_payloads = BTreeSet::new();
    for item in &module.items {
        let Item::Function(function) = item else { continue };
        if !is_canonical_source_function(module, &function.name) { continue; }
        witchy_syntax::ast::visit::visit_block(&function.body, &mut |expression| {
            if let Expr::Str(value) = expression {
                *literal_uses.entry(value.as_bytes().to_vec()).or_default() += 1;
            }
            // String literal patterns compare against static literals too.
            if let Expr::Match { arms, .. } = expression {
                for arm in arms {
                    collect_pattern_literals(&arm.pattern, &mut literal_uses);
                }
            }
            // A render reaches exactly the tables and punctuation its type's
            // structural render reads; the checker refuses a registered
            // segment no function references.
            if let Expr::Call { name, args } = expression
                && witchy_syntax::ast::is_render_intrinsic(name)
                && let [argument] = args.as_slice()
                && let Some(ty) = table.type_of(argument).and_then(witchy_types::typeck::ty_to_ast)
            {
                collect_render_payloads(&ty, definitions, &mut render_payloads, &mut renders_int, &mut renders_bool);
            }
            if let Expr::Call { name, args } = expression
                && matches!(name.as_str(), "string.chars" | "string.__chars")
            {
                if let [Expr::Str(value)] = args.as_slice() {
                    *chars_inputs.entry(value.as_bytes().to_vec()).or_default() += 1;
                    for character in value.chars() {
                        chars_outputs.insert(character.to_string().into_bytes());
                    }
                }
            }
            Ok::<(), ()>(())
        }).expect("string payload visitor cannot fail");
    }
    let mut payloads = chars_outputs;
    payloads.extend(render_payloads);
    if renders_int {
        payloads.insert(RENDER_DIGIT_TABLE.as_bytes().to_vec());
        payloads.insert(RENDER_MINUS_SIGN.as_bytes().to_vec());
    }
    if renders_bool {
        payloads.insert(RENDER_BOOL_TABLE.as_bytes().to_vec());
    }
    for (payload, uses) in literal_uses {
        if uses > chars_inputs.get(&payload).copied().unwrap_or_default() {
            payloads.insert(payload);
        }
    }
    payloads.into_iter().collect()
}

fn collect_pattern_literals(pattern: &Pattern, literal_uses: &mut BTreeMap<Vec<u8>, usize>) {
    match pattern {
        Pattern::Str(text) => *literal_uses.entry(text.as_bytes().to_vec()).or_default() += 1,
        Pattern::Ctor { args, .. } | Pattern::AnonCtor { args, .. } | Pattern::Tuple(args) => {
            for argument in args {
                collect_pattern_literals(argument, literal_uses);
            }
        }
        Pattern::List { elems, .. } => {
            for element in elems {
                collect_pattern_literals(element, literal_uses);
            }
        }
        _ => {}
    }
}

fn contains_console_print(module: &witchy_syntax::ast::Module) -> bool {
    module.items.iter().filter_map(|item| match item {
        Item::Function(function) if is_canonical_source_function(module, &function.name) => {
            Some(&function.body)
        }
        _ => None,
    }).any(|body| {
        let mut found = false;
        witchy_syntax::ast::visit::visit_block(body, &mut |expression| {
            if matches!(expression, Expr::Call { name, .. } if is_console_print(name)) {
                found = true;
            }
            Ok::<(), ()>(())
        }).expect("canonical expression visitor cannot fail");
        found
    })
}

fn is_console_print(name: &str) -> bool {
    name == "__capop.print"
}

fn list_layout(
    source: &Type,
    layout_bundle: &LayoutBundle,
    layouts: &LayoutInterner,
    definitions: &BTreeMap<String, TypeDef>,
) -> Result<(witchy_wir::layout::LayoutId, ValueType, u32, u32), CanonicalLoweringError> {
    let Type::Named(name, arguments) = source.unqualified() else {
        return Err(CanonicalLoweringError::UnsupportedType(source.clone()));
    };
    if name != "List" || arguments.len() != 1 {
        return Err(CanonicalLoweringError::UnsupportedType(source.clone()));
    }
    let expected_element = match arguments[0].unqualified() {
        Type::Named(element, args) if element == "Int" && args.is_empty() => ValueType::Int,
        Type::Named(element, args) if element == "Float" && args.is_empty() => ValueType::Float,
        Type::Named(element, args) if element == "Bool" && args.is_empty() => ValueType::Bool,
        Type::Named(element, args) if element == "String" && args.is_empty() => {
            ValueType::Ref(string_layout(&arguments[0], layout_bundle, layouts)?)
        }
        Type::Named(element, _) if element == "List" => {
            ValueType::Ref(list_layout(&arguments[0], layout_bundle, layouts, definitions)?.0)
        }
        // An aggregate element (a record, sum, tuple, scalar Option/Result)
        // lives behind an owned pointer slot to its record layout.
        _ => match scalar_aggregate_shape(&arguments[0], definitions) {
            Some(shape) if !shape.packed => {
                ValueType::Ref(aggregate_layout(&shape.fields, layout_bundle, layouts, definitions)?)
            }
            _ => return Err(CanonicalLoweringError::UnsupportedListElement(source.clone())),
        },
    };

    let mut candidates = Vec::new();
    for root in layout_bundle.roots() {
        let Some(descriptor) = layouts.get(*root) else { continue };
        let LayoutKind::PackedList { element, .. } = descriptor.kind() else { continue };
        let Some(element_descriptor) = layouts.get(*element) else { continue };
        let element_type = match (expected_element, element_descriptor.kind()) {
            (ValueType::Int, LayoutKind::Scalar(ScalarKind::Int)) => ValueType::Int,
            (ValueType::Float, LayoutKind::Scalar(ScalarKind::Float)) => ValueType::Float,
            (ValueType::Bool, LayoutKind::Scalar(ScalarKind::Bool)) => ValueType::Bool,
            (
                ValueType::Ref(expected),
                LayoutKind::RuntimePointerValueSlot {
                    pointee,
                    semantics: PointerSlot::Owned,
                },
            ) if *pointee == expected => ValueType::Ref(expected),
            _ => continue,
        };
        let LayoutSize::Dynamic { base, stride } = descriptor.size() else { continue };
        if !matches!(descriptor.header(), HeaderLayout::PackedList { data_offset, .. } if data_offset == base) {
            continue;
        }
        candidates.push((*root, element_type, base, stride));
    }
    match candidates.as_slice() {
        [] => Err(CanonicalLoweringError::MissingLayout(source.clone())),
        [candidate] => Ok(*candidate),
        _ => Err(CanonicalLoweringError::AmbiguousLayout(source.clone())),
    }
}

fn dictionary_layout(
    source: &Type,
    layout_bundle: &LayoutBundle,
    layouts: &LayoutInterner,
) -> Result<(witchy_wir::layout::LayoutId, ValueType, ValueType), CanonicalLoweringError> {
    let Type::Named(name, arguments) = source.unqualified() else {
        return Err(CanonicalLoweringError::UnsupportedType(source.clone()));
    };
    if name != "Dict" || arguments.len() != 2 {
        return Err(CanonicalLoweringError::UnsupportedType(source.clone()));
    }
    let key_type = dictionary_slot_type(&arguments[0], layout_bundle, layouts)?;
    let value_type = dictionary_slot_type(&arguments[1], layout_bundle, layouts)?;
    let mut candidates = Vec::new();
    for root in layout_bundle.roots() {
        let Some(descriptor) = layouts.get(*root) else { continue };
        let LayoutKind::RuntimeDictionaryBody { key, value, .. } = descriptor.kind() else {
            continue;
        };
        if dictionary_slot_matches(*key, key_type, layouts)
            && dictionary_slot_matches(*value, value_type, layouts)
        {
            candidates.push(*root);
        }
    }
    match candidates.as_slice() {
        [] => Err(CanonicalLoweringError::MissingLayout(source.clone())),
        [candidate] => Ok((*candidate, key_type, value_type)),
        _ => Err(CanonicalLoweringError::AmbiguousLayout(source.clone())),
    }
}

fn dictionary_slot_type(
    source: &Type,
    layout_bundle: &LayoutBundle,
    layouts: &LayoutInterner,
) -> Result<ValueType, CanonicalLoweringError> {
    match source.unqualified() {
        Type::Named(name, arguments) if name == "String" && arguments.is_empty() => {
            string_layout(source, layout_bundle, layouts).map(ValueType::Ref)
        }
        _ => scalar_type(source),
    }
}

fn dictionary_slot_matches(
    layout: witchy_wir::layout::LayoutId,
    value_type: ValueType,
    layouts: &LayoutInterner,
) -> bool {
    let Some(descriptor) = layouts.get(layout) else { return false };
    match (descriptor.kind(), value_type) {
        (LayoutKind::Scalar(ScalarKind::Int), ValueType::Int)
        | (LayoutKind::Scalar(ScalarKind::Float), ValueType::Float)
        | (LayoutKind::Scalar(ScalarKind::Duration), ValueType::Int)
        | (LayoutKind::Scalar(ScalarKind::BoolValueSlot), ValueType::Bool) => true,
        (
            LayoutKind::RuntimePointerValueSlot {
                pointee,
                semantics: PointerSlot::Owned,
            },
            ValueType::Ref(value_layout),
        ) => *pointee == value_layout,
        _ => false,
    }
}

const fn is_unit(value_type: ValueType) -> bool {
    matches!(value_type, ValueType::Unit)
}

fn canonical_tail_sites(
    manifest: &ProperTailSiteManifest,
    module: &witchy_syntax::ast::Module,
    lambda_functions: &HashMap<ProperTailCallableId, FunctionId>,
) -> Result<Vec<witchy_wir::canonical_safewir::ProperTailSite>, CanonicalLoweringError> {
    manifest
        .sites()
        .iter()
        .filter(|site| {
            module
                .items
                .get(site.key.callable.item as usize)
                .is_some_and(|item| match item {
                    Item::Function(function) => {
                        is_canonical_source_function(module, &function.name)
                            && !is_intrinsic_tail_site(site, function)
                    }
                    _ => false,
                })
        })
        .map(|site| {
            if site.key.callable.method.is_some() {
                return Err(CanonicalLoweringError::UnsupportedExpression("method tail site"));
            }
            // A site inside a lambda belongs to the function synthesized for
            // that lambda; a lambda without one is outside the subset.
            if !site.key.callable.lambda_path.is_empty() {
                let function = lambda_functions.get(&site.key.callable).copied().ok_or(
                    CanonicalLoweringError::UnsupportedExpression("tail site inside a lambda outside the v1 subset"),
                )?;
                return Ok(witchy_wir::canonical_safewir::ProperTailSite {
                    id: witchy_wir::canonical_safewir::TailSiteId::from_digest(site.id.as_bytes()),
                    // The checked manifest attributes the site to its owner;
                    // the synthesized lambda function discharges it.
                    function: FunctionId::new(site.canonical_function),
                    discharged_by: Some(function),
                });
            }
            let expected_function = module
                .items
                .iter()
                .enumerate()
                .filter_map(|(item_index, item)| {
                    let Item::Function(function) = item else { return None };
                    if is_canonical_source_function(module, &function.name) {
                        Some((item_index, function))
                    } else {
                        None
                    }
                })
                .position(|(item_index, _)| {
                    u32::try_from(item_index).ok() == Some(site.key.callable.item)
                })
                .ok_or(CanonicalLoweringError::MissingProperTailSite(
                    "proper-tail callable is not a canonical function".to_string(),
                ))?;
            if u32::try_from(expected_function).map_err(|_| CanonicalLoweringError::IdOverflow)?
                != site.canonical_function
            {
                return Err(CanonicalLoweringError::TailManifest(
                    "proper-tail callable identity does not match canonical function mapping".to_string(),
                ));
            }
            Ok(witchy_wir::canonical_safewir::ProperTailSite {
                id: witchy_wir::canonical_safewir::TailSiteId::from_digest(site.id.as_bytes()),
                function: FunctionId::new(site.canonical_function), discharged_by: None,
            })
        })
        .collect()
}

/// A manifest site whose call is an intrinsic the lowering emits inline
/// (a host print, a header read) is not a proper-tail obligation, wherever
/// in the tail structure it sits.
fn is_intrinsic_tail_site(
    site: &ProperTailSite,
    function: &witchy_syntax::ast::Function,
) -> bool {
    matches!(
        expr_at_tail_path(&function.body, &site.key.path),
        Some(Expr::Call { name, .. })
            if is_console_print(name)
                || matches!(name.as_str(), "list.length" | "list.__length" | "string.chars" | "string.__chars")
                || witchy_syntax::ast::is_render_intrinsic(name)
    )
}

/// Follow a proper-tail manifest path (the grammar of the checker's
/// `collect_block`) from a function body to the expression it names.
fn expr_at_tail_path<'a>(
    block: &'a witchy_syntax::ast::Block,
    path: &[ProperTailPathStep],
) -> Option<&'a Expr> {
    let [ProperTailPathStep::Statement(index), rest @ ..] = path else { return None };
    match (block.stmts.get(*index as usize)?, rest) {
        (Stmt::Return(Some(value)), [ProperTailPathStep::ReturnValue, rest @ ..])
        | (Stmt::Expr(value), [ProperTailPathStep::Value, rest @ ..]) => expr_in_tail_path(value, rest),
        _ => None,
    }
}

fn expr_in_tail_path<'a>(expression: &'a Expr, path: &[ProperTailPathStep]) -> Option<&'a Expr> {
    let [ProperTailPathStep::Node(_), rest @ ..] = path else { return None };
    if rest.is_empty() {
        return Some(expression);
    }
    match (expression, rest) {
        (Expr::If { then_block, .. }, [ProperTailPathStep::Child(1), rest @ ..]) => expr_at_tail_path(then_block, rest),
        (Expr::If { else_block: Some(else_block), .. }, [ProperTailPathStep::Child(2), rest @ ..]) => {
            expr_at_tail_path(else_block, rest)
        }
        (Expr::Match { arms, .. }, [ProperTailPathStep::Arm(arm), ProperTailPathStep::Value, rest @ ..]) => {
            expr_in_tail_path(&arms.get(*arm as usize)?.body, rest)
        }
        (Expr::Block(block), rest) => expr_at_tail_path(block, rest),
        (Expr::Binary { rhs, .. }, [ProperTailPathStep::Child(1), rest @ ..]) => expr_in_tail_path(rhs, rest),
        _ => None,
    }
}


fn is_canonical_source_function(module: &witchy_syntax::ast::Module, name: &str) -> bool {
    witchy_types::pipeline::in_canonical_scope(module, name)
}

#[cfg(test)]
mod tests {
    use super::{lower_typed_module, CanonicalLoweringError};
    use witchy_syntax::ast::{Convention, Item};
    use witchy_syntax::parser::parse_module;
    use witchy_wir::canonical_safewir::{
        check_initial, check_initial_with_witness, CallArgument, Extent, GuardKind, LoadTransfer, Operation,
        Ownership, ParameterOwnership, Projection, Terminator, ValueType,
    };
    use witchy_wir::layout::{ClosedTypeResolver, LayoutBundle, LayoutInterner, RcHeader, ResolvedNamed, ScalarKind};

    struct ListResolver;

    impl ClosedTypeResolver for ListResolver {
        fn resolve_named<'a>(
            &'a self,
            name: &str,
            _arguments: &[witchy_syntax::ast::Type],
        ) -> Option<ResolvedNamed<'a>> {
            match name {
                "Int" => Some(ResolvedNamed::Scalar(ScalarKind::Int)),
                "List" => Some(ResolvedNamed::PackedList { rc: RcHeader::Required }),
                _ => None,
            }
        }
    }

    fn empty_layouts() -> LayoutBundle {
        LayoutBundle::from_interner(&LayoutInterner::new(), [])
            .expect("empty layouts are canonical")
    }

    fn list_layouts() -> LayoutBundle {
        let mut interner = LayoutInterner::new();
        let list = interner
            .intern_type(
                &witchy_syntax::ast::Type::Named(
                    "List".into(),
                    vec![witchy_syntax::ast::Type::Named("Int".into(), Vec::new())],
                ),
                &ListResolver,
            )
            .expect("canonical List(Int) layout");
        LayoutBundle::from_interner(&interner, [list]).expect("list layout bundle is canonical")
    }

    fn checked(source: &str) -> witchy_types::pipeline::CheckedModule {
        witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked source")
    }

    fn lower(source: &str) -> witchy_wir::canonical_safewir::CanonicalModule<
        witchy_wir::canonical_safewir::LoweredCandidate,
    > {
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked source");
        let canonical = checked.canonical_source().expect("canonical source");
        lower_typed_module(canonical, empty_layouts())
            .expect("lower canonical SafeWIR")
    }

    fn lower_with_derived_layouts(
        source: &str,
    ) -> witchy_wir::canonical_safewir::CanonicalModule<
        witchy_wir::canonical_safewir::LoweredCandidate,
    > {
        let checked = checked(source);
        let canonical = checked.canonical_source().expect("canonical source");
        let layouts = crate::canonical_layouts::derive_layout_bundle(canonical)
            .expect("derive canonical layouts");
        lower_typed_module(canonical, layouts).expect("lower canonical SafeWIR")
    }

    #[test]
    fn checked_scalar_source_lowers_and_validates_without_wir() {
        let candidate = lower("fn answer() -> Int:\n    let x = 40\n    x + 2\n");
        let result = check_initial(candidate);
        assert!(result.is_ok(), "DropRef candidate rejected: {result:?}");
    }

    #[test]
    fn canonical_lowering_uses_expanded_final_typed_source() {
        fn append_expanded_function(
            name: &str,
            module: &mut witchy_syntax::ast::Module,
            _siblings: &[(String, witchy_syntax::ast::Module)],
        ) -> Result<witchy_syntax::origin::OriginTable, String> {
            if name == "main" {
                module.items.extend(
                    parse_module("fn expanded(n: Int) -> Int:\n    return expanded(n)\n")
                        .expect("expanded fixture parses")
                        .items,
                );
            }
            Ok(witchy_syntax::origin::OriginTable::default())
        }

        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), b"fn main() -> Int:\n    1\n".to_vec())],
            "main",
            append_expanded_function,
        )
        .expect("expanded source checks");
        let canonical = checked.canonical_source().expect("canonical source");
        let expanded_item = canonical
            .typed_module()
            .module()
            .items
            .iter()
            .enumerate()
            .find_map(|(item, item_def)| {
                matches!(item_def, Item::Function(function) if function.name.ends_with(".expanded"))
                    .then_some(item as u32)
            })
            .expect("expanded function is in final typed source");
        assert!(canonical
            .proper_tail_manifest()
            .sites()
            .iter()
            .any(|site| site.key.callable.item == expanded_item));

        let candidate = lower_typed_module(canonical, empty_layouts())
            .expect("canonical lowering includes expanded function");
        assert_eq!(candidate.functions().len(), 2);
    }

    #[test]
    fn canonical_lowering_uses_checked_generic_callable_witness() {
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![(
                "main".to_string(),
                b"fn identity(value: a) -> a:\n    value\n\nfn main() -> Int:\n    identity(1)\n".to_vec(),
            )],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("generic source checks");
        let candidate = lower_typed_module(
            checked.canonical_source().expect("canonical source"),
            empty_layouts(),
        )
        .expect("generic direct call lowers through its checked instance");
        assert_eq!(candidate.functions().len(), 2, "main plus concrete identity");
    }

    #[test]
    fn canonical_lowering_interns_structural_callable_parameter() {
        // A `fn` value is a closure record, so the candidate needs the
        // derived layouts.
        let candidate = lower_with_derived_layouts(
            "fn positive(n: Int) -> Bool:\n    n > 0\n\nfn nonzero(n: Int) -> Bool:\n    n != 0\n\nfn apply(f: fn(Int) -> Bool, n: Int) -> Bool:\n    f(n)\n\nfn main() -> Int:\n    if apply(positive, 3):\n        1\n    else:\n        0\n",
        );
        assert_eq!(candidate.functions()[0].signature, candidate.functions()[1].signature);
        let apply = &candidate.functions()[2];
        assert_eq!(candidate.signatures()[apply.signature.get() as usize].parameters.len(), 2);
        assert!(candidate
            .functions()
            .iter()
            .flat_map(|function| function.blocks.iter())
            .any(|block| matches!(block.terminator, Terminator::VarTailCall { .. })));
        let result = check_initial(candidate);
        assert!(result.is_ok(), "callable candidate rejected: {result:?}");
    }

    #[test]
    fn indirect_string_result_uses_body_derived_contract() {
        let candidate = lower_with_derived_layouts(
            "fn stringify(n: Int) -> String:\n    \"value\"\n\nfn apply(f: fn(Int) -> String, n: Int) -> String:\n    let value = f(n)\n    value\n\nfn main() -> Int:\n    let value = apply(stringify, 1)\n    0\n",
        );
        assert!(candidate
            .functions()
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| &block.operations)
            .any(|operation| matches!(operation, Operation::VarCall { .. })));
        let result = check_initial(candidate);
        assert!(result.is_ok(), "memory-result callable candidate rejected: {result:?}");
    }

    #[test]
    fn proper_tail_source_lowers_with_manifest_identity() {
        let candidate = lower("fn recurse(n: Int) -> Int:\n    return recurse(n)\n");
        let result = check_initial(candidate);
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn scalar_if_uses_typed_join_parameter() {
        let candidate = lower("fn choose(flag: Bool) -> Int:\n    if flag:\n        1\n    else:\n        2\n");
        let function = &candidate.functions()[0];
        assert_eq!(function.blocks.len(), 4);
        assert_eq!(function.blocks[3].parameters.len(), 1);
        assert!(matches!(function.blocks[0].terminator, Terminator::BranchIf { .. }));
        let result = check_initial(candidate);
        assert!(result.is_ok(), "owned-call candidate rejected: {result:?}");
    }

    #[test]
    fn statement_if_without_else_uses_an_explicit_empty_cfg_arm() {
        let candidate = lower(
            "fn choose(flag: Bool) -> Int:\n    var value = 1\n    if flag:\n        value = 2\n    value\n",
        );
        assert!(candidate.functions()[0].blocks.iter().any(|block| matches!(
            block.terminator,
            Terminator::BranchIf { .. }
        )));
        let result = check_initial(candidate);
        assert!(result.is_ok(), "statement if candidate rejected: {result:?}");
    }

    #[test]
    fn shared_static_if_carries_object_and_dynamic_extent_through_phi() {
        let candidate = lower_with_derived_layouts(
            "fn choose(flag: Bool) -> String:\n    if flag:\n        \"left\"\n    else:\n        \"right\"\n",
        );
        let join = candidate.functions()[0].blocks.last().expect("if join");
        assert_eq!(join.parameters.len(), 3);
        assert_eq!(join.parameters[0].ownership, Some(Ownership::Shared));
        assert_eq!(join.parameters[1].value_type, ValueType::Size);
        assert_eq!(join.parameters[2].value_type, ValueType::Size);
        let result = check_initial(candidate);
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn shared_parameter_if_gives_the_result_lane_its_own_token() {
        // The join lane over two shared parameters is no forged token: each
        // arm clones the parameter it returns, so the result owns a token of
        // its own and both parameter tokens are released at the return.
        let checked = checked(
            "fn choose(flag: Bool, left: String, right: String) -> String:\n    if flag:\n        left\n    else:\n        right\n",
        );
        let source = checked.canonical_source().expect("canonical source");
        let layouts = crate::canonical_layouts::derive_layout_bundle(source).expect("derive layouts");
        let candidate = lower_typed_module(source, layouts).expect("shared parameter join lowers");
        let clones = candidate.functions()[0]
            .blocks
            .iter()
            .flat_map(|block| &block.operations)
            .filter(|operation| matches!(operation, Operation::CloneRef { .. }))
            .count();
        assert_eq!(clones, 2, "each arm clones the parameter it returns");
    }

    #[test]
    fn scalar_match_uses_typed_cfg_join() {
        let candidate = lower(
            "fn choose(flag: Bool) -> Int:\n    match flag:\n        true -> 1\n        false -> 2\n",
        );
        let function = &candidate.functions()[0];
        assert!(function.blocks.iter().any(|block| matches!(
            block.terminator,
            Terminator::BranchIf { .. }
        )));
        assert_eq!(function.blocks.last().unwrap().parameters.len(), 1);
        let result = check_initial(candidate);
        assert!(result.is_ok(), "owned tail-call candidate rejected: {result:?}");
    }

    #[test]
    fn shared_static_match_carries_object_and_dynamic_extent_through_phi() {
        let candidate = lower_with_derived_layouts(
            "fn choose(flag: Bool) -> String:\n    match flag:\n        true -> \"left\"\n        false -> \"right\"\n",
        );
        let join = candidate.functions()[0].blocks.last().expect("match join");
        assert_eq!(join.parameters.len(), 3);
        assert_eq!(join.parameters[0].ownership, Some(Ownership::Shared));
        let result = check_initial(candidate);
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn nested_scalar_block_preserves_the_join() {
        let candidate = lower(
            "fn nested(flag: Bool) -> Int:\n    if flag:\n        if flag:\n            1\n        else:\n            2\n    else:\n        3\n",
        );
        assert!(candidate.functions()[0].blocks.len() >= 4);
        let result = check_initial(candidate);
        assert!(result.is_ok(), "owned-call candidate rejected: {result:?}");
    }

    #[test]
    fn scalar_while_carries_bindings_through_header_and_exit() {
        let candidate = lower(
            "fn count(limit: Int) -> Int:\n    var i = 0\n    while i < limit:\n        i = i + 1\n    i\n",
        );
        let function = &candidate.functions()[0];
        assert_eq!(function.blocks.len(), 4);
        // Only the rebound `i` is a lane; `limit` keeps its entry value.
        assert_eq!(function.blocks[1].parameters.len(), 1);
        assert_eq!(function.blocks[3].parameters.len(), 1);
        let result = check_initial(candidate);
        assert!(result.is_ok(), "owned tail-call candidate rejected: {result:?}");
    }

    #[test]
    fn list_literal_and_index_lower_to_validated_length_guarded_safewir() {
        let checked = checked("fn read(i: Int) -> Int:\n    let xs = [10, 20]\n    xs[i]\n");
        let candidate = lower_typed_module(
            checked.canonical_source().expect("canonical source"),
            list_layouts(),
        )
        .expect("lower List(Int) source");
        let operations = &candidate.functions()[0].blocks[0].operations;

        let (count, capacity) = operations
            .iter()
            .find_map(|operation| match operation {
                Operation::Alloc { extent: Extent::Counted { count, capacity, .. }, .. } => {
                    Some((*count, *capacity))
                }
                _ => None,
            })
            .expect("list allocation has counted extent");
        assert_ne!(count, capacity, "logical count and capacity use distinct SSA values");
        assert!(operations.iter().any(|operation| matches!(
            operation,
            Operation::Store { place, .. }
                if place.value_type() == witchy_wir::canonical_safewir::ValueType::Int
                    && matches!(place.projection(), Projection::Element { .. })
        )));
        assert!(operations.iter().any(|operation| matches!(
            operation,
            Operation::Compare {
                operator: witchy_wir::canonical_safewir::ComparisonOperator::LessUnsigned,
                right,
                ..
            } if *right == count
        )), "the index guard uses the allocation's intrinsic logical-length identity");
        assert!(operations.iter().any(|operation| matches!(
            operation,
            Operation::Guard { kind: GuardKind::Bounds, .. }
        )));
        assert!(operations.iter().any(|operation| matches!(
            operation,
            Operation::Load { place, .. }
                if matches!(place.projection(), Projection::Element { .. })
        )));
        let validation = check_initial(candidate);
        assert!(validation.is_ok(), "{validation:?}");
    }

    #[test]
    fn dynamic_list_push_carries_length_and_capacity_through_loop() {
        let checked = checked(
            "fn pushed() -> Int:\n    var xs = [1]\n    for n in [2, 3]:\n        xs.push(n)\n    xs[2]\n",
        );
        let candidate = lower_typed_module(
            checked.canonical_source().expect("canonical source"),
            list_layouts(),
        )
        .expect("dynamic list push lowers");
        assert!(candidate.functions().iter().flat_map(|function| &function.blocks).any(|block| {
            block.operations.iter().any(|operation| matches!(operation, Operation::CloneRange { .. }))
        }));
        let result = check_initial(candidate);
        assert!(result.is_ok(), "dynamic list push rejected: {result:?}");
    }

    #[test]
    fn list_length_loads_authenticated_header_and_widens_to_int() {
        let checked = checked("fn count(xs: List(Int)) -> Int:\n    xs.length()\n");
        let candidate = lower_typed_module(
            checked.canonical_source().expect("canonical source"),
            list_layouts(),
        )
        .expect("lower List.length source");
        let operations = &candidate.functions()[0].blocks[0].operations;
        assert!(operations.iter().any(|operation| matches!(
            operation,
            Operation::Load { place, transfer: LoadTransfer::CopyScalar, .. }
                if matches!(place.projection(), Projection::Header(witchy_wir::canonical_safewir::HeaderField::Length))
        )));
        assert!(operations.iter().any(|operation| matches!(
            operation,
            Operation::WidenSize { .. }
        )));
        let result = check_initial(candidate);
        assert!(result.is_ok(), "list length candidate rejected: {result:?}");
    }

    #[test]
    fn nested_list_literals_move_fresh_inner_aggregates() {
        let candidate = lower_with_derived_layouts(
            "fn main() -> Int:\n    let nested = [[\"a\"], [\"b\"]]\n    var more = []\n    more.push([\"c\"])\n    0\n",
        );
        assert!(candidate
            .functions()
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.operations)
            .any(|operation| matches!(
                operation,
                Operation::Store {
                    transfer: witchy_wir::canonical_safewir::StoreTransfer::MoveUnique,
                    ..
                }
            )));
        let result = check_initial(candidate);
        assert!(result.is_ok(), "nested list literal rejected: {result:?}");
    }

    #[test]
    fn string_chars_literal_decodes_utf8_scalars_into_owned_strings() {
        let candidate = lower_with_derived_layouts(
            "fn chars() -> List(String):\n    \"h\u{00e9}\".chars()\n\nfn main() -> Int:\n    0\n",
        );
        let operations = candidate.functions()
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.operations)
            .collect::<Vec<_>>();
        assert_eq!(
            operations
                .iter()
                .filter(|operation| matches!(operation, Operation::Store { .. }))
                .count(),
            2,
            "one owned string is stored for each Unicode scalar",
        );
        assert_eq!(
            operations
                .iter()
                .filter(|operation| matches!(operation, Operation::CopyBytes { .. }))
                .count(),
            2,
        );
        let result = check_initial(candidate);
        assert!(result.is_ok(), "UTF-8 chars candidate rejected: {result:?}");
    }

    #[test]
    fn list_push_lowering_has_checked_increment_and_rebinds_unique_storage() {
        let checked = checked("fn main() -> Int:\n    var xs = [1]\n    xs.push(2)\n    xs[1]\n");
        let candidate = lower_typed_module(
            checked.canonical_source().expect("canonical source"),
            list_layouts(),
        )
        .expect("lower List(Int) push source");
        let operations = &candidate.functions()[0].blocks[0].operations;
        assert!(operations.iter().any(|operation| matches!(
            operation,
            Operation::Guard { kind: GuardKind::Arithmetic, .. }
        )));
        assert!(operations.iter().any(|operation| matches!(
            operation,
            Operation::Arithmetic {
                operator: witchy_wir::canonical_safewir::ArithmeticOperator::Add,
                overflow: witchy_wir::canonical_safewir::OverflowBehavior::Wrap,
                ..
            }
        )));
        assert!(operations.iter().any(|operation| matches!(
            operation,
            Operation::UpdateCount { .. }
        )));
        assert!(operations.iter().any(|operation| matches!(
            operation,
            Operation::DropRef { .. }
        )));
        let result = check_initial(candidate);
        assert!(result.is_ok(), "DropRef candidate rejected: {result:?}");
    }

    #[test]
    fn shared_list_parameter_index_uses_loaded_length_for_bounds_guard() {
        let checked = checked("fn first(xs: List(Int)) -> Int:\n    xs[0]\n");
        let candidate = lower_typed_module(
            checked.canonical_source().expect("canonical source"),
            list_layouts(),
        )
        .expect("lower borrowed List(Int) parameter index");
        let operations = &candidate.functions()[0].blocks[0].operations;

        let (header_position, logical_length) = operations
            .iter()
            .enumerate()
            .find_map(|(position, operation)| match operation {
                Operation::Load {
                    destination,
                    place,
                    transfer: LoadTransfer::CopyScalar,
                } if matches!(
                    place.projection(),
                    Projection::Header(witchy_wir::canonical_safewir::HeaderField::Length)
                ) => Some((position, *destination)),
                _ => None,
            })
            .expect("borrowed parameter length is loaded from its semantic header");
        let (compare_position, condition, index) = operations
            .iter()
            .enumerate()
            .find_map(|(position, operation)| match operation {
                Operation::Compare {
                    destination,
                    operator: witchy_wir::canonical_safewir::ComparisonOperator::LessUnsigned,
                    left,
                    right,
                } if *right == logical_length => Some((position, *destination, *left)),
                _ => None,
            })
            .expect("index is compared against the loaded logical length");
        let guard_position = operations
            .iter()
            .enumerate()
            .find_map(|(position, operation)| match operation {
                Operation::Guard { condition: actual, kind: GuardKind::Bounds }
                    if *actual == condition => Some(position),
                _ => None,
            })
            .expect("the length comparison is guarded");
        let element_position = operations
            .iter()
            .enumerate()
            .find_map(|(position, operation)| match operation {
                Operation::Load {
                    place,
                    transfer: LoadTransfer::CopyScalar,
                    ..
                } if matches!(place.projection(), Projection::Element { index: actual } if *actual == index) => {
                    Some(position)
                }
                _ => None,
            })
            .expect("the guarded element is loaded");

        assert!(header_position < compare_position);
        assert!(compare_position < guard_position);
        assert!(guard_position < element_position);
    }

    #[test]
    fn shared_list_parameter_index_promotes_authenticated_header_extent() {
        let checked = checked("fn first(xs: List(Int)) -> Int:\n    xs[0]\n");
        let candidate = lower_typed_module(
            checked.canonical_source().expect("canonical source"),
            list_layouts(),
        )
        .expect("lower borrowed List(Int) parameter index");
        assert!(check_initial(candidate).is_ok());
    }

    #[test]
    fn empty_list_literal_is_a_fresh_validated_safewir_object() {
        let checked = checked("fn empty() -> List(Int):\n    []\n");
        let candidate = lower_typed_module(
            checked.canonical_source().expect("canonical source"),
            list_layouts(),
        )
        .expect("lower empty List(Int) source");
        assert!(candidate.functions()[0].blocks[0]
            .operations
            .iter()
            .any(|operation| matches!(operation, Operation::Alloc { extent: Extent::Counted { .. }, .. })));
        assert!(check_initial(candidate).is_ok());
    }

    #[test]
    fn scalar_direct_call_remains_a_value_argument() {
        let candidate = lower(
            "fn increment(n: Int) -> Int:\n    n + 1\nfn caller() -> Int:\n    let result = increment(41)\n    result\n",
        );
        let caller = &candidate.functions()[1];
        let call = caller.blocks[0]
            .operations
            .iter()
            .find_map(|operation| match operation {
                Operation::Call { arguments, .. } => Some(arguments),
                _ => None,
            })
            .expect("caller has a direct call");
        assert!(matches!(call.as_slice(), [CallArgument::Value(_)]));
    }

    #[test]
    fn reference_let_parameter_uses_an_owned_clone() {
        let checked = checked(
            "fn inspect(xs: List(Int)) -> Int:\n    7\nfn caller() -> Int:\n    let xs = [1]\n    let result = inspect(xs)\n    result\n",
        );
        let candidate = lower_typed_module(
            checked.canonical_source().expect("canonical source"),
            list_layouts(),
        )
        .expect("lower direct object call");
        let callee = candidate
            .signatures()
            .iter()
            .find(|signature| signature.contract.parameters.len() == 1)
            .expect("object callee signature");
        assert!(matches!(
            callee.contract.parameters[0].ownership,
            ParameterOwnership::Shared
        ));

        let call = candidate.functions()[1].blocks[0]
            .operations
            .iter()
            .find_map(|operation| match operation {
                Operation::Call { arguments, .. } => Some(arguments),
                _ => None,
            })
            .expect("caller has a direct object call");
        assert!(matches!(call.as_slice(), [CallArgument::Move(_)]));
        assert!(candidate.functions()[1].blocks[0].operations.iter().any(|operation| {
            matches!(operation, Operation::CloneRef { .. })
        }));
        let result = check_initial(candidate);
        assert!(result.is_ok(), "owned-call candidate rejected: {result:?}");
    }

    #[test]
    fn reference_tail_call_moves_the_owned_token() {
        let checked = checked("fn recurse(xs: List(Int)) -> Int:\n    return recurse(xs)\n");
        let candidate = lower_typed_module(
            checked.canonical_source().expect("canonical source"),
            list_layouts(),
        )
        .expect("lower object tail call");
        let tail = match &candidate.functions()[0].blocks[0].terminator {
            Terminator::TailCall { arguments, .. } => arguments,
            other => panic!("expected tail call, got {other:?}"),
        };
        assert!(matches!(tail.as_slice(), [CallArgument::Move(_)]));
        assert!(!candidate.functions()[0].blocks[0].operations.iter().any(|operation| {
            matches!(operation, Operation::CloneRef { .. } | Operation::DropRef { .. })
        }));
        let result = check_initial(candidate);
        assert!(result.is_ok(), "owned tail-call candidate rejected: {result:?}");
    }

    #[test]
    fn ordinary_own_identity_transfers_reference_without_clone_and_admits() {
        let checked = checked(
            "fn scalar(own value: Int) -> Int:\n    value\nfn identity(own value: String) -> String:\n    value\nfn caller(number: Int, value: String) -> String:\n    let consumed = scalar(number)\n    identity(value)\n",
        );
        let source = checked.canonical_source().expect("canonical source");
        let candidate = lower_typed_module(
            source,
            crate::canonical_layouts::derive_layout_bundle(source).expect("derive layouts"),
        )
        .expect("ordinary checked own calls lower")
        .with_launch_contract_payload(source.launch_contract_payload().to_vec());
        let caller = candidate.functions().iter().find(|function| function.id.get() == 2).expect("caller");
        assert!(caller.blocks[0].operations.iter().any(|operation| matches!(operation,
            Operation::Call { arguments, .. } if matches!(arguments.as_slice(), [CallArgument::Move(_)])
        )) || matches!(&caller.blocks[0].terminator,
            Terminator::TailCall { arguments, .. } if matches!(arguments.as_slice(), [CallArgument::Move(_)])
        ));
        assert!(!caller.blocks[0].operations.iter().any(|operation| matches!(operation, Operation::CloneRef { .. })));
        let admitted = check_initial_with_witness(candidate, source);
        assert!(admitted.is_ok(), "source-bound own candidate admits: {:?}", admitted.err());
    }

    #[test]
    fn unique_contracts_stay_fail_closed() {
        let checked = checked(
            "fn consume(own value: unique List(Int)) -> Int:\n    value.length()\n",
        );
        let source = checked.canonical_source().expect("canonical source");
        let error = lower_typed_module(
            source,
            crate::canonical_layouts::derive_layout_bundle(source).expect("derive layouts"),
        )
        .expect_err("v1 must not silently erase a unique contract");
        assert!(matches!(error, CanonicalLoweringError::UnsupportedType(_)));
    }

    #[test]
    fn local_unique_ascriptions_stay_fail_closed() {
        let checked = checked(
            "fn main() -> Int:\n    let values: unique List(Int) = [1]\n    values.length()\n",
        );
        let source = checked.canonical_source().expect("canonical source");
        let error = lower_typed_module(
            source,
            crate::canonical_layouts::derive_layout_bundle(source).expect("derive layouts"),
        )
        .expect_err("v1 must reject a local qualifier before type normalization erases it");
        assert!(matches!(error, CanonicalLoweringError::UnsupportedType(_)));
    }

    #[test]
    fn scalar_var_indirect_call_lowers_as_a_write_back_lane() {
        // A scalar `var` parameter is a value in and a result lane out, for
        // direct and indirect calls alike; the call site rebinds the variable.
        let checked = checked(
            "fn apply(f: fn(var Int) -> Int, var value: Int) -> Int:\n    f(value)\n",
        );
        let source = checked.canonical_source().expect("canonical source");
        lower_typed_module(source, empty_layouts()).expect("scalar var indirect calls lower");
    }

    #[test]
    fn list_lowering_refuses_a_missing_layout() {
        let checked = checked("fn read() -> Int:\n    let xs = [1]\n    xs[0]\n");
        let error = lower_typed_module(
            checked.canonical_source().expect("canonical source"),
            empty_layouts(),
        )
        .expect_err("a List(Int) root must be supplied by the layout bundle");
        assert!(matches!(error, CanonicalLoweringError::MissingLayout(_)));
    }

    #[test]
    fn if_without_else_with_an_early_return_lowers_as_an_effect() {
        // Once a v1 refusal; a unit function's final `if` is an effect, and an
        // arm that returns early simply has no join edge.
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), "fn bad(flag: Bool) -> Nil:\n    if flag:\n        return\n".as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked source");
        let canonical = checked.canonical_source().expect("canonical source");
        let candidate = lower_typed_module(canonical, empty_layouts()).expect("lowers as an effect");
        assert!(check_initial(candidate).is_ok());
    }
}
