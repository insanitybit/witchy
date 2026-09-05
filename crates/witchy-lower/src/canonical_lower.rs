//! Direct checked-AST to canonical SafeWIR lowering.
//!
//! This module never constructs legacy WIR. Its initial scalar subset is
//! deliberately fail-closed; every unsupported source operation is reported
//! before a `LoweredCandidate` can be returned.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::error::Error;
use std::fmt;

#[path = "canonical_owned.rs"]
mod owned;

#[cfg(test)]
#[path = "canonical_owned_tests.rs"]
mod owned_tests;

use witchy_syntax::ast::{BinOp, Convention, Expr, Item, Pattern, Stmt, Type, TypeDef, TypeQual, UnOp};
use witchy_types::pipeline::CanonicalCheckedSource;
use witchy_types::typeck::{
    ProperTailCallableId, ProperTailDispatchKind, ProperTailExprKind, ProperTailPathStep,
    ProperTailSite, ProperTailSiteKey, ProperTailSiteManifest, TypeTable,
};
use witchy_wir::canonical_safewir::{
    AccessMode, ArithmeticOperator, BasicBlock, BlockId, BlockParameter,
    CallArgument, CallableContract, CanonicalAbiCatalog, CanonicalModule,
    CheckedSourceIdentity, ComparisonOperator, Function, FunctionId, FunctionSignature,
    FunctionSignatureId, HostBufferExtent, HostCallArgument, LoweredCandidate,
    LoweredModuleParts, CandidateAbiBinding, CandidateAbiBindings, MemoryEffect, Operation, OverflowBehavior, ParameterContract,
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
            .filter_map(|parameter| parameter.ty.as_ref())
            .chain(function.ret.iter())
            .find(|ty| contains_deferred_v1_qualifier(ty))
            .cloned()
            .or_else(|| deferred_v1_qualifier_in_block(&function.body))
        {
            return Err(CanonicalLoweringError::UnsupportedType(unsupported));
        }
    }
    let packed_records = typed.module().items.iter().filter_map(|item| match item {
        Item::Type(definition) if is_v1_packed_scalar_record(definition) => {
            Some((definition.name.clone(), definition))
        }
        _ => None,
    }).collect::<HashMap<_, _>>();
    let (_, layout_catalog) = LayoutBundle::decode_canonical(&layouts.canonical_bytes())
        .map_err(|error| CanonicalLoweringError::InvalidLayoutBundle(error.to_string()))?;
    let tail_manifest = source.proper_tail_manifest();
    let string_payloads = collect_string_payloads(typed.module());
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
            signature_from_type(id, ty, &layouts, &layout_catalog, &function_types)
        })
        .collect::<Result<Vec<_>, CanonicalLoweringError>>()?;
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
            &function_types,
        )?;
    }

    let mut imports = Vec::new();
    let mut abi_catalog = None;
    let mut print_import = None;
    if contains_console_print(typed.module()) {
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
        let catalog = CanonicalAbiCatalog::from_authenticated_prelude(signature_id, string_layout)
            .map_err(CanonicalLoweringError::InvalidAuthenticatedAbiCatalog)?;
        print_import = catalog.imports().first().map(|import| import.id);
        imports = catalog.imports();
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
            function_id,
            u32::try_from(item_index).map_err(|_| CanonicalLoweringError::IdOverflow)?,
            signature,
            &functions_by_name,
            &checked_parameter_conventions,
            &signatures,
            &tail_manifest,
            typed.table(),
            &layouts,
            &layout_catalog,
            &string_data_ids,
            print_import,
            &packed_records,
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

    let mut abi_bindings = functions_by_name
        .iter()
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
        proper_tail_sites: canonical_tail_sites(tail_manifest, typed.module())?,
    }, static_data_segments).with_source_abi_bindings(CandidateAbiBindings::new(abi_bindings));
    Ok(match abi_catalog {
        Some(catalog) => candidate.with_abi_catalog(catalog),
        None => candidate,
    })
}

fn checked_signature(
    source: &CanonicalCheckedSource,
    function: &witchy_syntax::ast::Function,
    id: FunctionSignatureId,
    layout_bundle: &LayoutBundle,
    layouts: &LayoutInterner,
    function_types: &HashMap<Type, FunctionSignatureId>,
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

    let checked_signature = signature_from_type(id, &checked, layout_bundle, layouts, function_types)?;
    let value_parameters = checked_signature.parameters;
    let contract_parameters = checked_signature.contract.parameters;
    let parameter_effects = checked_signature.contract.parameter_effects;
    for (parameter_index, ((parameter, source), convention)) in
        parameters.iter().zip(&function.params).zip(conventions).enumerate()
    {
        if matches!(convention, Convention::Borrow)
            || matches!(convention, Convention::Own) && !ordinary_own_type_supported(parameter)
            || matches!(convention, Convention::Var)
                && !matches!(source_value_type(parameter, layout_bundle, layouts, function_types)?, ValueType::Ref(_))
        {
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
    function_types: &HashMap<Type, FunctionSignatureId>,
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
        .chain(std::iter::once(result.as_ref()))
        .find(|ty| contains_deferred_v1_qualifier(ty))
    {
        return Err(CanonicalLoweringError::UnsupportedType(unsupported.clone()));
    }
    let mut value_parameters = Vec::with_capacity(parameters.len());
    let mut contract_parameters = Vec::with_capacity(parameters.len());
    let mut parameter_effects = BTreeMap::new();
    for (index, (parameter, convention)) in parameters.iter().zip(conventions).enumerate() {
        if matches!(convention, Convention::Own) && !ordinary_own_type_supported(parameter)
            || matches!(convention, Convention::Var)
                && !matches!(source_value_type(parameter, layout_bundle, layouts, function_types)?, ValueType::Ref(_))
        {
            return Err(CanonicalLoweringError::UnsupportedConvention {
                function: format!("signature {:?}", id),
                parameter: index.to_string(),
                convention,
            });
        }
        let value_type = source_value_type(parameter, layout_bundle, layouts, function_types)?;
        let ownership = match (value_type, convention) {
            (ValueType::Ref(_), Convention::Let | Convention::Own) => ParameterOwnership::Shared,
            (ValueType::Ref(_), Convention::Var) => ParameterOwnership::WriteBack,
            (_, Convention::Let | Convention::Own) => ParameterOwnership::Value,
            _ => unreachable!(),
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
    let result = source_value_type(result, layout_bundle, layouts, function_types)?;
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

fn lower_function(
    function: &witchy_syntax::ast::Function,
    id: FunctionId,
    item: u32,
    signature: &FunctionSignature,
    functions: &HashMap<String, (FunctionId, FunctionSignatureId)>,
    checked_parameter_conventions: &HashMap<FunctionSignatureId, Vec<Convention>>,
    signatures: &[FunctionSignature],
    tail_manifest: &ProperTailSiteManifest,
    type_table: &TypeTable,
    layout_bundle: &LayoutBundle,
    layouts: &LayoutInterner,
    string_data_ids: &BTreeMap<Vec<u8>, witchy_wir::canonical_safewir::StaticDataId>,
    print_import: Option<witchy_wir::canonical_safewir::ImportId>,
    packed_records: &HashMap<String, &TypeDef>,
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
        write_back_parameters: Vec::new(),
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
        print_import,
        packed_records,
    };
    let entry = lowerer.new_block(&signature.parameters)?;
    for (parameter, contract) in lowerer.blocks[entry.get() as usize]
        .parameters.iter_mut().zip(&signature.contract.parameters)
    {
        parameter.ownership = match contract.ownership {
            ParameterOwnership::Unique => Some(Ownership::Unique),
            ParameterOwnership::Shared | ParameterOwnership::WriteBack => Some(Ownership::Shared),
            ParameterOwnership::Value => None,
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
    }
    lowerer.write_back_parameters = function.params.iter().zip(&signature.contract.parameters)
        .filter(|(_, contract)| contract.ownership == ParameterOwnership::WriteBack)
        .map(|(parameter, _)| parameter.name.clone())
        .collect();

    let ordinary_results = &signature.results[..signature.results.len() - write_back_count(signature)];
    lowerer.lower_body(&function.body.stmts, ordinary_results)?;
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
    owned::insert_releases(&mut lowered, signature, signatures, functions)
        .map_err(|error| CanonicalLoweringError::OwnedReleasePlacement(format!("{error:?}")))?;
    Ok(lowered)
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
    string_lengths: HashMap<ValueId, ValueId>,
    string_capacities: HashMap<ValueId, ValueId>,
    list_counts: HashMap<ValueId, ValueId>,
    list_capacities: HashMap<ValueId, ValueId>,
    known_list_lengths: HashMap<ValueId, u64>,
    owned_parameters: Vec<ValueId>,
    write_back_parameters: Vec<String>,
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
    print_import: Option<witchy_wir::canonical_safewir::ImportId>,
    packed_records: &'a HashMap<String, &'a TypeDef>,
}

impl FunctionLowerer<'_> {
    fn lower_body(
        &mut self,
        statements: &[Stmt],
        results: &[ValueType],
    ) -> Result<(), CanonicalLoweringError> {
        for (index, statement) in statements.iter().enumerate() {
            let final_statement = index + 1 == statements.len();
            match statement {
                Stmt::Let { name, value, .. } => {
                    let value = self.lower_expr(value)?;
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
                Stmt::Return(value) => {
                    if !final_statement {
                        return Err(CanonicalLoweringError::UnsupportedStatement("early return"));
                    }
                    if let Some(Expr::Call { name, args }) = value {
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
                        if !self.bindings.contains_key(name) {
                            let terminator = self.lower_direct_tail_call(
                                name,
                                args,
                                index,
                                ProperTailPathStep::ReturnValue,
                            )?;
                            self.finish_current(terminator)?;
                        } else {
                            let callee = self.bindings[name];
                            let ValueType::Callable(signature) = self.values[&callee] else {
                                return Err(CanonicalLoweringError::IndirectCalleeType(
                                    self.values[&callee],
                                ));
                            };
                            let terminator = self.lower_indirect_tail_call(
                                callee,
                                args,
                                signature,
                                index,
                                ProperTailPathStep::ReturnValue,
                                ProperTailExprKind::Call,
                                ProperTailDispatchKind::Direct,
                            )?;
                            self.finish_current(terminator)?;
                        }
                        return Ok(());
                    }
                    if let Some(Expr::Apply { func, args }) = value {
                        let callee = self.lower_expr(func)?;
                        let ValueType::Callable(signature) = self.values[&callee] else {
                            return Err(CanonicalLoweringError::IndirectCalleeType(
                                self.values[&callee],
                            ));
                        };
                        let terminator = self.lower_indirect_tail_call(
                            callee,
                            args,
                            signature,
                            index,
                            ProperTailPathStep::ReturnValue,
                            ProperTailExprKind::Apply,
                            ProperTailDispatchKind::Indirect,
                        )?;
                        self.finish_current(terminator)?;
                        return Ok(());
                    }
                    let value = value.as_ref().map(|value| self.lower_expr(value)).transpose()?;
                    let terminator = self.return_value(value, results)?;
                    self.finish_current(terminator)?;
                    return Ok(());
                }
                Stmt::Expr(value) if final_statement => {
                    if let Expr::Call { name, args } = value {
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
                        if !self.bindings.contains_key(name) {
                            let terminator = self.lower_direct_tail_call(
                                name,
                                args,
                                index,
                                ProperTailPathStep::Value,
                            )?;
                            self.finish_current(terminator)?;
                        } else {
                            let callee = self.bindings[name];
                            let ValueType::Callable(signature) = self.values[&callee] else {
                                return Err(CanonicalLoweringError::IndirectCalleeType(
                                    self.values[&callee],
                                ));
                            };
                            let terminator = self.lower_indirect_tail_call(
                                callee,
                                args,
                                signature,
                                index,
                                ProperTailPathStep::Value,
                                ProperTailExprKind::Call,
                                ProperTailDispatchKind::Direct,
                            )?;
                            self.finish_current(terminator)?;
                        }
                        return Ok(());
                    }
                    if let Expr::Apply { func, args } = value {
                        let callee = self.lower_expr(func)?;
                        let ValueType::Callable(signature) = self.values[&callee] else {
                            return Err(CanonicalLoweringError::IndirectCalleeType(
                                self.values[&callee],
                            ));
                        };
                        let terminator = self.lower_indirect_tail_call(
                            callee,
                            args,
                            signature,
                            index,
                            ProperTailPathStep::Value,
                            ProperTailExprKind::Apply,
                            ProperTailDispatchKind::Indirect,
                        )?;
                        self.finish_current(terminator)?;
                        return Ok(());
                    }
                    if results.is_empty() && matches!(value, Expr::While { .. } | Expr::For { .. }) {
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
                Stmt::LetPattern { .. } => {
                    return Err(CanonicalLoweringError::UnsupportedStatement("let pattern"));
                }
                Stmt::Break => return Err(CanonicalLoweringError::UnsupportedStatement("break")),
                Stmt::Continue => {
                    return Err(CanonicalLoweringError::UnsupportedStatement("continue"));
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
            Expr::For { var, iter, body } => self.lower_for(var, iter, body),
            Expr::Block(block) => self.lower_block_effect(block),
            Expr::Call { name, args } if is_console_print(name) => self.lower_console_print(args),
            Expr::Call { name, args } if matches!(name.as_str(), "list.push" | "list.__push") => {
                self.lower_list_push(args)
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
                self.lower_match(scrutinee, arms, false).map(|_| ())
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
            match statement {
                Stmt::Let { name, value, .. } => {
                    let value = self.lower_expr(value)?;
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
                Stmt::Return(_) => {
                    return Err(CanonicalLoweringError::UnsupportedStatement("return in block"));
                }
                Stmt::LetPattern { .. } => {
                    return Err(CanonicalLoweringError::UnsupportedStatement("let pattern"));
                }
                Stmt::Break => return Err(CanonicalLoweringError::UnsupportedStatement("break")),
                Stmt::Continue => {
                    return Err(CanonicalLoweringError::UnsupportedStatement("continue"));
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
            match statement {
                Stmt::Let { name, value, .. } => {
                    let value = self.lower_expr(value)?;
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
            Some(self.lower_block_value(then_block)?)
        } else {
            self.lower_block_effect(then_block)?;
            None
        };
        let then_bindings = self.bindings.clone();
        let then_end = self.current;

        self.current = else_id;
        self.bindings = outer.clone();
        let else_value = if with_value {
            Some(self.lower_block_value(else_block)?)
        } else {
            self.lower_block_effect(else_block)?;
            None
        };
        let else_bindings = self.bindings.clone();
        let else_end = self.current;

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
                && self.known_shared_value(then_value)
                && self.known_shared_value(else_value))
            .then(|| {
                Some((
                    self.dynamic_extent_lanes(then_value)?,
                    self.dynamic_extent_lanes(else_value)?,
                ))
            })
            .flatten()
        });
        let mut names = outer.keys().cloned().collect::<Vec<_>>();
        names.sort();
        let changed_names = names
            .iter()
            .filter(|name| then_bindings[*name] != else_bindings[*name])
            .cloned()
            .collect::<Vec<_>>();
        let mut parameter_types = changed_names
            .iter()
            .map(|name| self.values[&then_bindings[name]])
            .collect::<Vec<_>>();
        if let Some((_, _, result_type)) = result_type {
            parameter_types.push(result_type);
            if shared_result_extents.is_some() {
                parameter_types.extend([ValueType::Size, ValueType::Size]);
            }
        }
        let join_id = self.new_block(&parameter_types)?;
        if shared_result_extents.is_some() {
            self.blocks[join_id.get() as usize].parameters[changed_names.len()].ownership =
                Some(Ownership::Shared);
        }
        let join_parameters = self.blocks[join_id.get() as usize].parameters.clone();
        let mut then_arguments = Vec::with_capacity(parameter_types.len());
        let mut else_arguments = Vec::with_capacity(parameter_types.len());
        let mut parameter_index = 0;
        let mut merged = outer;
        for name in changed_names {
            then_arguments.push(then_bindings[&name]);
            else_arguments.push(else_bindings[&name]);
            merged.insert(name, join_parameters[parameter_index].value);
            parameter_index += 1;
        }
        let result = result_type.map(|(then_value, else_value, _)| {
            then_arguments.push(then_value);
            else_arguments.push(else_value);
            let result = join_parameters[parameter_index].value;
            if let Some(((then_count, then_capacity), (else_count, else_capacity))) =
                shared_result_extents
            {
                then_arguments.extend([then_count, then_capacity]);
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
            result
        });
        self.blocks[then_end.get() as usize].terminator = Some(Terminator::Branch {
            target: join_id,
            arguments: then_arguments,
        });
        self.blocks[else_end.get() as usize].terminator = Some(Terminator::Branch {
            target: join_id,
            arguments: else_arguments,
        });
        self.current = join_id;
        self.bindings = merged;
        Ok(result)
    }

    fn lower_match(
        &mut self,
        scrutinee: &Expr,
        arms: &[witchy_syntax::ast::MatchArm],
        with_value: bool,
    ) -> Result<Option<ValueId>, CanonicalLoweringError> {
        if arms.is_empty() {
            return Err(CanonicalLoweringError::UnsupportedExpression("empty match"));
        }
        let scrutinee = self.lower_expr(scrutinee)?;
        let outer = self.bindings.clone();
        let result_type = if with_value {
            let first = self.expression_type(&arms[0].body)?;
            let first = source_value_type(&first, self.layout_bundle, self.layouts, &HashMap::new())?;
            if is_unit(first) {
                return Err(CanonicalLoweringError::UnsupportedExpression("unit match value"));
            }
            Some(first)
        } else {
            None
        };
        let mut arm_results = Vec::with_capacity(arms.len());
        let mut next = self.current;

        for arm in arms {
            self.current = next;
            self.bindings = outer.clone();
            let arm_block = self.new_block(&[])?;
            let next_block = self.new_block(&[])?;
            let pattern_binding = self.lower_match_pattern(scrutinee, &arm.pattern)?;
            if let Some((name, value)) = pattern_binding {
                self.bindings.insert(name, value);
            }
            let arm_bindings = self.bindings.clone();
            let condition = self.match_pattern_condition(scrutinee, &arm.pattern)?;
            if let Some(condition) = condition {
                self.finish_current(Terminator::BranchIf {
                    condition,
                    then_target: arm_block,
                    then_arguments: Vec::new(),
                    else_target: next_block,
                    else_arguments: Vec::new(),
                })?;
            } else {
                self.finish_current(Terminator::Branch {
                    target: arm_block,
                    arguments: Vec::new(),
                })?;
            }

            self.current = arm_block;
            self.bindings = arm_bindings;
            if let Some(guard) = &arm.guard {
                let guard = self.lower_expr(guard)?;
                if self.values[&guard] != ValueType::Bool {
                    return Err(CanonicalLoweringError::UnsupportedExpression("non-boolean match guard"));
                }
                // Replace the true edge with a dedicated body block. Keeping
                // the guard and body separate is necessary: the false edge
                // must not carry pattern bindings into the next arm.
                let body_block = self.new_block(&[])?;
                self.blocks[arm_block.get() as usize].terminator = Some(Terminator::BranchIf {
                    condition: guard,
                    then_target: body_block,
                    then_arguments: Vec::new(),
                    else_target: next_block,
                    else_arguments: Vec::new(),
                });
                self.current = body_block;
            }
            let value = if with_value {
                let value = self.lower_expr(&arm.body)?;
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
            self.current = next_block;
            self.bindings = outer.clone();
            next = next_block;
        }

        self.current = next;
        self.bindings = outer.clone();
        self.finish_current(Terminator::Trap)?;

        let mut changed_names = outer
            .keys()
            .filter(|name| arm_results.iter().any(|(_, bindings, _)| bindings[*name] != outer[*name]))
            .cloned()
            .collect::<Vec<_>>();
        changed_names.sort();
        let mut parameter_types = changed_names
            .iter()
            .map(|name| self.values[&outer[name]])
            .collect::<Vec<_>>();
        if let Some(result_type) = result_type {
            parameter_types.push(result_type);
        }
        let result_values = arm_results
            .iter()
            .filter_map(|(_, _, value)| *value)
            .collect::<Vec<_>>();
        let shared_result_extents = (result_type.is_some()
            && !result_values.is_empty()
            && result_values.iter().all(|value| self.known_shared_value(*value)))
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
        let join = self.new_block(&parameter_types)?;
        let result_parameter_index = changed_names.len();
        if shared_result_extents.is_some() {
            self.blocks[join.get() as usize].parameters[result_parameter_index].ownership =
                Some(Ownership::Shared);
        }
        let join_parameters = self.blocks[join.get() as usize].parameters.clone();
        let mut merged = outer;
        let result = result_type.map(|_| join_parameters[result_parameter_index].value);
        for (arm_index, (body_end, bindings, value)) in arm_results.into_iter().enumerate() {
            let mut arguments = changed_names
                .iter()
                .map(|name| bindings[name])
                .collect::<Vec<_>>();
            for (name, parameter) in changed_names.iter().zip(&join_parameters) {
                merged.insert(name.clone(), parameter.value);
            }
            if let Some(value) = value {
                arguments.push(value);
                if let Some(extents) = &shared_result_extents {
                    arguments.extend([extents[arm_index].0, extents[arm_index].1]);
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
        self.current = join;
        self.bindings = merged;
        Ok(result)
    }

    fn lower_match_pattern(
        &self,
        scrutinee: ValueId,
        pattern: &Pattern,
    ) -> Result<Option<(String, ValueId)>, CanonicalLoweringError> {
        match pattern {
            Pattern::Var(name) if name != "_" => Ok(Some((name.clone(), scrutinee))),
            Pattern::Wildcard
            | Pattern::Var(_)
            | Pattern::Int(_)
            | Pattern::Duration(_)
            | Pattern::Bool(_) => Ok(None),
            _ => Err(CanonicalLoweringError::UnsupportedExpression("match pattern")),
        }
    }

    fn match_pattern_condition(
        &mut self,
        scrutinee: ValueId,
        pattern: &Pattern,
    ) -> Result<Option<ValueId>, CanonicalLoweringError> {
        let (value, expected_type) = match pattern {
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

    fn lower_while(
        &mut self,
        condition: &Expr,
        body: &witchy_syntax::ast::Block,
    ) -> Result<(), CanonicalLoweringError> {
        let outer = self.bindings.clone();
        let mut names = outer.keys().cloned().collect::<Vec<_>>();
        names.sort();
        let list_names = names
            .iter()
            .filter_map(|name| {
                let object = outer[name];
                self.list_counts.contains_key(&object).then(|| name.clone())
            })
            .collect::<Vec<_>>();
        if list_names.iter().any(|name| {
            let object = outer[name];
            !self.list_capacities.contains_key(&object)
        }) {
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
        let header_parameters = self.blocks[header.get() as usize].parameters.clone();
        let header_values = header_parameters.iter().map(|parameter| parameter.value).collect::<Vec<_>>();
        let mut entry_state = names.iter().map(|name| outer[name]).collect::<Vec<_>>();
        for name in &list_names {
            let object = outer[name];
            entry_state.push(self.list_counts[&object]);
            entry_state.push(self.list_capacities[&object]);
        }
        self.finish_current(Terminator::Branch {
            target: header,
            arguments: entry_state,
        })?;
        self.current = header;
        self.bindings = names
            .iter()
            .zip(&header_values)
            .map(|(name, value)| (name.clone(), *value))
            .collect();
        let mut metadata_index = names.len();
        for name in &list_names {
            let object = self.bindings[name];
            self.list_counts.insert(object, header_values[metadata_index]);
            self.list_capacities.insert(object, header_values[metadata_index + 1]);
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
        self.bindings = names
            .iter()
            .zip(&body_values)
            .map(|(name, value)| (name.clone(), *value))
            .collect();
        metadata_index = names.len();
        for name in &list_names {
            let object = self.bindings[name];
            self.list_counts.insert(object, body_values[metadata_index]);
            self.list_capacities.insert(object, body_values[metadata_index + 1]);
            metadata_index += 2;
        }
        self.lower_block_effect(body)?;
        let body_bindings = self.bindings.clone();
        let mut carried = names.iter().map(|name| body_bindings[name]).collect::<Vec<_>>();
        for name in &list_names {
            let object = body_bindings[name];
            carried.push(*self.list_counts.get(&object).ok_or(
                CanonicalLoweringError::UnsupportedExpression("while backedge list count state"),
            )?);
            carried.push(*self.list_capacities.get(&object).ok_or(
                CanonicalLoweringError::UnsupportedExpression("while backedge list capacity state"),
            )?);
        }
        let body_end = self.current;
        self.blocks[body_end.get() as usize].terminator = Some(Terminator::Branch {
            target: header,
            arguments: carried,
        });
        self.current = exit;
        self.bindings = names
            .iter()
            .zip(exit_parameters.iter())
            .map(|(name, parameter)| (name.clone(), parameter.value))
            .collect();
        metadata_index = names.len();
        for name in &list_names {
            let object = self.bindings[name];
            self.list_counts.insert(object, exit_parameters[metadata_index].value);
            self.list_capacities.insert(object, exit_parameters[metadata_index + 1].value);
            metadata_index += 2;
        }
        Ok(())
    }

    fn lower_for(
        &mut self,
        variable: &str,
        iterable: &Expr,
        body: &witchy_syntax::ast::Block,
    ) -> Result<(), CanonicalLoweringError> {
        let logical_type = self.expression_type(iterable)?;
        let (layout, element_type, header_bytes, stride_bytes) =
            list_layout(&logical_type, self.layout_bundle, self.layouts)?;
        let source = self.lower_expr(iterable)?;
        if self.values[&source] != ValueType::Ref(layout) {
            return Err(CanonicalLoweringError::UnsupportedExpression("for list object type"));
        }

        let outer = self.bindings.clone();
        let mut names = body
            .stmts
            .iter()
            .filter_map(|statement| match statement {
                Stmt::Assign { name, .. } if outer.contains_key(name) => Some(name.clone()),
                Stmt::Expr(Expr::Call { name, args })
                    if matches!(name.as_str(), "list.push" | "list.__push") =>
                {
                    match args.first() {
                        Some(Expr::Var(name)) if outer.contains_key(name) => Some(name.clone()),
                        _ => None,
                    }
                }
                _ => None,
            })
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
                self.list_counts.contains_key(&object).then(|| name.clone())
            })
            .collect::<Vec<_>>();
        if list_names.iter().any(|name| {
            let object = outer[name];
            !self.list_capacities.contains_key(&object)
        }) {
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
        for (name, parameter) in names
            .iter()
            .zip(&mut self.blocks[header.get() as usize].parameters)
        {
            if list_names.iter().any(|list_name| list_name == name)
                && matches!(parameter.value_type, ValueType::Ref(_))
            {
                parameter.ownership = Some(Ownership::Unique);
            }
        }
        let mut entry_state = names.iter().map(|name| outer[name]).collect::<Vec<_>>();
        for name in &list_names {
            let object = outer[name];
            entry_state.push(self.list_counts[&object]);
            entry_state.push(self.list_capacities[&object]);
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
            self.list_counts.insert(object, header_values[metadata_index]);
            self.list_capacities.insert(object, header_values[metadata_index + 1]);
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
            self.list_counts.insert(object, body_values[metadata_index]);
            self.list_capacities.insert(object, body_values[metadata_index + 1]);
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
        self.lower_block_effect(body)?;
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
            back_state.push(*self.list_counts.get(&object).ok_or(
                CanonicalLoweringError::UnsupportedExpression("for backedge list count state"),
            )?);
            back_state.push(*self.list_capacities.get(&object).ok_or(
                CanonicalLoweringError::UnsupportedExpression("for backedge list capacity state"),
            )?);
        }
        back_state.push(next_index);
        self.finish_current(Terminator::Branch { target: header, arguments: back_state })?;

        let exit_parameters = self.blocks[exit.get() as usize].parameters.clone();
        self.current = exit;
        self.bindings = outer;
        for (name, parameter) in names.iter().zip(&exit_parameters) {
            self.bindings.insert(name.clone(), parameter.value);
        }
        metadata_index = names.len();
        for name in &list_names {
            let object = self.bindings[name];
            self.list_counts.insert(object, exit_parameters[metadata_index].value);
            self.list_capacities.insert(object, exit_parameters[metadata_index + 1].value);
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
                    if values.contains(&value) {
                        let clone = self.fresh(self.values[&value])?;
                        self.push_operation(Operation::CloneRef { destination: clone, source: value });
                        for result in &mut values {
                            if *result == value { *result = clone; }
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
                .copied()
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

    fn known_shared_value(&self, value: ValueId) -> bool {
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
                _ => false,
            })
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
        statement: usize,
        position: ProperTailPathStep,
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
        let statement = u32::try_from(statement).map_err(|_| CanonicalLoweringError::IdOverflow)?;
        let key = ProperTailSiteKey {
            callable: ProperTailCallableId {
                item: self.item,
                method: None,
                lambda_path: Vec::new(),
            },
            path: vec![
                ProperTailPathStep::Statement(statement),
                position,
                ProperTailPathStep::Node(ProperTailExprKind::Call),
            ],
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

    fn lower_indirect_tail_call(
        &mut self,
        callee: ValueId,
        arguments: &[Expr],
        signature_id: FunctionSignatureId,
        statement: usize,
        position: ProperTailPathStep,
        expression: ProperTailExprKind,
        dispatch: ProperTailDispatchKind,
    ) -> Result<Terminator, CanonicalLoweringError> {
        let signature = self
            .signatures
            .get(signature_id.get() as usize)
            .cloned()
            .ok_or(CanonicalLoweringError::UnsupportedExpression(
                "indirect tail-call signature",
            ))?;
        if signature
            .parameters
            .iter()
            .copied()
            .any(|value_type| !is_indirect_result(value_type))
            || signature
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
                lambda_path: Vec::new(),
            },
            path: vec![
                ProperTailPathStep::Statement(
                    u32::try_from(statement).map_err(|_| CanonicalLoweringError::IdOverflow)?,
                ),
                position,
                ProperTailPathStep::Node(expression),
            ],
            dispatch,
        };
        let site = self
            .tail_manifest
            .sites()
            .iter()
            .find(|site| site.key == key)
            .ok_or_else(|| CanonicalLoweringError::MissingProperTailSite(self.function.clone()))?;
        let arguments = self.lower_call_arguments("<indirect tail>", arguments, signature_id, &signature, true)?;
        Ok(Terminator::VarTailCall {
            site: witchy_wir::canonical_safewir::TailSiteId::from_digest(site.id.as_bytes()),
            callee,
            signature: signature_id,
            arguments,
        })
    }

    fn lower_indirect_call(
        &mut self,
        callee: ValueId,
        arguments: &[Expr],
        signature_id: FunctionSignatureId,
    ) -> Result<Vec<ValueId>, CanonicalLoweringError> {
        let signature = self
            .signatures
            .get(signature_id.get() as usize)
            .cloned()
            .ok_or(CanonicalLoweringError::UnsupportedExpression(
                "indirect call signature",
            ))?;
        if signature
            .parameters
            .iter()
            .copied()
            .any(|value_type| !is_indirect_result(value_type))
        {
            return Err(CanonicalLoweringError::UnsupportedExpression(
                "memory-bearing indirect call parameter",
            ));
        }
        if let Some(result) = signature
            .results
            .iter()
            .copied()
            .find(|value_type| !is_indirect_result(*value_type))
        {
            return Err(CanonicalLoweringError::UnsupportedIndirectResult(result));
        }
        let call_arguments = self.lower_call_arguments("<indirect>", arguments, signature_id, &signature, false)?;
        let destinations = signature
            .results
            .iter()
            .copied()
            .map(|value_type| self.fresh(value_type))
            .collect::<Result<Vec<_>, _>>()?;
        self.push_operation(Operation::VarCall {
            destinations: destinations.clone(),
            callee,
            signature: signature_id,
            arguments: call_arguments,
        });
        self.rebind_write_backs(arguments, &signature, &destinations)?;
        Ok(destinations[..destinations.len() - write_back_count(&signature)].to_vec())
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
                    Convention::Let => match contract.ownership {
                        ParameterOwnership::Value => Ok(CallArgument::Value(value)),
                        ParameterOwnership::Shared if tail => Ok(CallArgument::Move(value)),
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
                        if contract.ownership != ParameterOwnership::WriteBack
                            || !matches!(actual, ValueType::Ref(_))
                        {
                            return Err(CanonicalLoweringError::UnsupportedCallOwnership {
                                callee: callee.to_owned(), parameter, ownership: contract.ownership,
                            });
                        }
                        let Expr::Var(name) = argument else {
                            return Err(CanonicalLoweringError::UnsupportedExpression(
                                "var write-back argument must be a bound variable",
                            ));
                        };
                        self.owned_parameters.retain(|owned| *owned != value);
                        if !self.bindings.contains_key(name) {
                            return Err(CanonicalLoweringError::MissingBinding(name.clone()));
                        }
                        Ok(CallArgument::Move(value))
                    }
                    convention @ Convention::Borrow => Err(
                        CanonicalLoweringError::UnsupportedConvention {
                            function: callee.to_owned(), parameter: parameter.to_string(), convention,
                        },
                    ),
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
                } else if let Some((function, signature)) = self.functions.get(name).copied() {
                    let destination = self.fresh(ValueType::Callable(signature))?;
                    self.push_operation(Operation::FunctionValue { destination, function });
                    destination
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
            Expr::Call { name, args } if matches!(name.as_str(), "list.length" | "list.__length") => {
                self.lower_list_length(args)
            }
            Expr::Call { name, args } if matches!(name.as_str(), "string.chars" | "string.__chars") => {
                self.lower_string_chars(args)
            }
            Expr::Call { name, args } => {
                let results = if let Some(callee) = self.bindings.get(name).copied() {
                    let ValueType::Callable(signature) = self.values[&callee] else {
                        return Err(CanonicalLoweringError::IndirectCalleeType(self.values[&callee]));
                    };
                    self.lower_indirect_call(callee, args, signature)?
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
                let ValueType::Callable(signature) = self.values[&callee] else {
                    return Err(CanonicalLoweringError::IndirectCalleeType(self.values[&callee]));
                };
                let results = self.lower_indirect_call(callee, args, signature)?;
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
            Expr::Tuple(_) => Err(CanonicalLoweringError::UnsupportedExpression("tuple")),
            Expr::Ctor { name, args } => self.lower_packed_record_constructor(expression, name, args),
            Expr::AnonCtor { .. } => {
                Err(CanonicalLoweringError::UnsupportedExpression("constructor"))
            }
            Expr::Unary { op, expr } => self.lower_unary(*op, expr),
            Expr::Binary { op, lhs, rhs } => self.lower_binary(*op, lhs, rhs),
            Expr::Field { base, field } => self.lower_inline_aggregate_field(base, field),
            Expr::Lambda { .. } => Err(CanonicalLoweringError::UnsupportedExpression("lambda")),
            Expr::RecordUpdate { .. } | Expr::Record { .. } => {
                Err(CanonicalLoweringError::UnsupportedExpression("record"))
            }
            Expr::Try(_) => Err(CanonicalLoweringError::UnsupportedExpression("try")),
            Expr::As { .. } => Err(CanonicalLoweringError::UnsupportedExpression("as")),
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
                .lower_match(scrutinee, arms, true)?
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
        if !matches!(source_type.unqualified(), Type::Named(checked_name, arguments) if checked_name == name && arguments.is_empty()) {
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
        let import = self.print_import.ok_or_else(|| {
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
            list_layout(&logical_type, self.layout_bundle, self.layouts)?;
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
                    return Err(CanonicalLoweringError::UnsupportedExpression(
                        "non-literal nested list needs explicit ownership",
                    ));
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
        let ValueType::Ref(layout) = self.values[&old_object] else {
            return Err(CanonicalLoweringError::UnsupportedExpression("list.push receiver"));
        };
        let list_type = self.expression_type(&arguments[0])?;
        let (expected_layout, element_type, header_bytes, stride_bytes) =
            list_layout(&list_type, self.layout_bundle, self.layouts)?;
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
        let value_expression = value;
        let mut value = self.lower_expr(value_expression)?;
        if self.values[&value] != element_type {
            return Err(CanonicalLoweringError::UnsupportedListElement(list_type));
        }
        if pointer_elements && self.string_lengths.contains_key(&value) {
            value = self.clone_string_for_storage(value)?;
        } else if pointer_elements && !matches!(value_expression, Expr::List(_)) {
            return Err(CanonicalLoweringError::UnsupportedExpression(
                "non-literal nested list push needs explicit ownership",
            ));
        }

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
            self.push_operation(Operation::MoveRange {
                destination: witchy_wir::canonical_safewir::Place::projected(
                    new_object,
                    Projection::Slice { start: zero, length: old_length },
                    element_type,
                    AccessMode::Write,
                ),
                source: witchy_wir::canonical_safewir::Place::projected(
                    old_object,
                    Projection::Slice { start: zero, length: old_length },
                    element_type,
                    AccessMode::Read,
                ),
                count: old_length,
            });
            self.push_operation(Operation::UpdateCount { object: old_object, count: zero });
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
        self.bindings.insert(name.clone(), new_object);
        self.list_counts.insert(new_object, new_length);
        self.list_capacities.insert(new_object, capacity);
        self.known_list_lengths.remove(&new_object);
        Ok(())
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
            return Err(CanonicalLoweringError::UnsupportedExpression(
                "dynamic string.chars requires a canonical checked UTF-8 decoder",
            ));
        };
        let list_type = Type::Named(
            "List".into(),
            vec![Type::Named("String".into(), Vec::new())],
        );
        let (layout, element_type, header_bytes, stride_bytes) =
            list_layout(&list_type, self.layout_bundle, self.layouts)?;
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
            list_layout(&logical_type, self.layout_bundle, self.layouts)?;
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
        if let ValueType::Ref(string_layout) = element_type {
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
            debug_assert_eq!(self.values[&destination], ValueType::Ref(string_layout));
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
        if matches!(operator, BinOp::And | BinOp::Or | BinOp::Coalesce) {
            return Err(CanonicalLoweringError::UnsupportedExpression(
                "short-circuit binary operation",
            ));
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
            if !matches!(left_type, ValueType::Int | ValueType::Bool) {
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
        let left_length = *self.string_lengths.get(&left).ok_or(
            CanonicalLoweringError::UnsupportedExpression("dynamic string concatenation operand"),
        )?;
        let right_length = *self.string_lengths.get(&right).ok_or(
            CanonicalLoweringError::UnsupportedExpression("dynamic string concatenation operand"),
        )?;

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
}

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
    function_types: &HashMap<Type, FunctionSignatureId>,
) -> Result<ValueType, CanonicalLoweringError> {
    match source.unqualified() {
        Type::Fn(..) => function_types
            .get(source)
            .or_else(|| function_types.get(source.unqualified()))
            .copied()
            .map(ValueType::Callable)
            .ok_or_else(|| CanonicalLoweringError::UnsupportedType(source.clone())),
        Type::Named(name, arguments) if name == "List" && arguments.len() == 1 => {
            list_layout(source, layout_bundle, layouts).map(|(layout, _, _, _)| ValueType::Ref(layout))
        }
        Type::Named(name, arguments) if name == "Dict" && arguments.len() == 2 => {
            dictionary_layout(source, layout_bundle, layouts)
                .map(|(layout, _, _)| ValueType::Ref(layout))
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

fn collect_string_payloads(module: &witchy_syntax::ast::Module) -> Vec<Vec<u8>> {
    let mut literal_uses = BTreeMap::<Vec<u8>, usize>::new();
    let mut chars_inputs = BTreeMap::<Vec<u8>, usize>::new();
    let mut chars_outputs = BTreeSet::new();
    for item in &module.items {
        let Item::Function(function) = item else { continue };
        if !is_canonical_source_function(module, &function.name) { continue; }
        witchy_syntax::ast::visit::visit_block(&function.body, &mut |expression| {
            if let Expr::Str(value) = expression {
                *literal_uses.entry(value.as_bytes().to_vec()).or_default() += 1;
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
    for (payload, uses) in literal_uses {
        if uses > chars_inputs.get(&payload).copied().unwrap_or_default() {
            payloads.insert(payload);
        }
    }
    payloads.into_iter().collect()
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
            ValueType::Ref(list_layout(&arguments[0], layout_bundle, layouts)?.0)
        }
        _ => return Err(CanonicalLoweringError::UnsupportedListElement(source.clone())),
    };

    let mut candidates = Vec::new();
    for root in layout_bundle.roots() {
        let Some(descriptor) = layouts.get(*root) else { continue };
        let LayoutKind::PackedList { element, .. } = descriptor.kind() else { continue };
        let Some(element_descriptor) = layouts.get(*element) else { continue };
        let element_type = match (expected_element, element_descriptor.kind()) {
            (ValueType::Int, LayoutKind::Scalar(ScalarKind::Int)) => ValueType::Int,
            (ValueType::Float, LayoutKind::Scalar(ScalarKind::Float)) => ValueType::Float,
            (ValueType::Bool, LayoutKind::Scalar(ScalarKind::BoolValueSlot)) => ValueType::Bool,
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
            if site.key.callable.method.is_some() || !site.key.callable.lambda_path.is_empty() {
                return Err(CanonicalLoweringError::UnsupportedExpression("method or closure tail site"));
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
                function: FunctionId::new(site.canonical_function),
            })
        })
        .collect()
}

fn is_intrinsic_tail_site(
    site: &ProperTailSite,
    function: &witchy_syntax::ast::Function,
) -> bool {
    let Some(statement) = site.key.path.iter().find_map(|step| match step {
        ProperTailPathStep::Statement(index) => Some(*index as usize),
        _ => None,
    }) else {
        return false;
    };
    match function.body.stmts.get(statement) {
        Some(Stmt::Expr(Expr::Call { name, .. }))
        | Some(Stmt::Return(Some(Expr::Call { name, .. })))
            if is_console_print(name)
                || matches!(name.as_str(), "list.length" | "list.__length") => true,
        _ => false,
    }
}

fn is_canonical_source_function(module: &witchy_syntax::ast::Module, name: &str) -> bool {
    let Some(entry) = module.linked_entry.as_deref() else {
        return true;
    };
    name == entry || name.starts_with(&format!("{entry}."))
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
        let candidate = lower(
            "fn positive(n: Int) -> Bool:\n    n > 0\n\nfn nonzero(n: Int) -> Bool:\n    n != 0\n\nfn apply(f: fn(Int) -> Bool, n: Int) -> Bool:\n    f(n)\n\nfn main() -> Int:\n    0\n",
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
    fn shared_parameter_if_does_not_forge_a_shared_phi() {
        let candidate = lower_with_derived_layouts(
            "fn choose(flag: Bool, left: String, right: String) -> String:\n    if flag:\n        left\n    else:\n        right\n",
        );
        let join = candidate.functions()[0].blocks.last().expect("if join");
        assert_eq!(join.parameters[0].ownership, None);
        assert!(matches!(
            check_initial(candidate),
            Err(witchy_wir::canonical_safewir::InitialCheckError::UnknownCallableContract {
                ..
            })
        ));
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
        assert_eq!(function.blocks[1].parameters.len(), 2);
        assert_eq!(function.blocks[3].parameters.len(), 2);
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
        .expect("ordinary checked own calls lower");
        let caller = candidate.functions().iter().find(|function| function.id.get() == 2).expect("caller");
        assert!(caller.blocks[0].operations.iter().any(|operation| matches!(operation,
            Operation::Call { arguments, .. } if matches!(arguments.as_slice(), [CallArgument::Move(_)])
        )) || matches!(&caller.blocks[0].terminator,
            Terminator::TailCall { arguments, .. } if matches!(arguments.as_slice(), [CallArgument::Move(_)])
        ));
        assert!(!caller.blocks[0].operations.iter().any(|operation| matches!(operation, Operation::CloneRef { .. })));
        assert!(check_initial_with_witness(candidate, source).is_ok(), "source-bound own candidate admits");
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
    fn var_indirect_call_stays_fail_closed() {
        let checked = checked(
            "fn apply(f: fn(var Int) -> Int, var value: Int) -> Int:\n    f(value)\n",
        );
        let source = checked.canonical_source().expect("canonical source");
        let error = lower_typed_module(source, empty_layouts())
            .expect_err("indirect var calls remain unmodeled");
        assert!(matches!(
            error,
            CanonicalLoweringError::UnsupportedConvention {
                convention: Convention::Var,
                ..
            }
        ));
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
    fn no_else_value_is_rejected_fail_closed() {
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), "fn bad(flag: Bool) -> Nil:\n    if flag:\n        return\n".as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked source");
        let canonical = checked.canonical_source().expect("canonical source");
        assert!(lower_typed_module(canonical, empty_layouts()).is_err());
    }
}
