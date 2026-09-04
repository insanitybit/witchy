//! Direct checked-AST to canonical SafeWIR lowering.
//!
//! This module never constructs legacy WIR. Its initial scalar subset is
//! deliberately fail-closed; every unsupported source operation is reported
//! before a `LoweredCandidate` can be returned.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::error::Error;
use std::fmt;

use witchy_syntax::ast::{BinOp, Convention, Expr, Item, Stmt, Type};
use witchy_types::pipeline::CanonicalCheckedSource;
use witchy_types::typeck::{
    ProperTailCallableId, ProperTailDispatchKind, ProperTailExprKind, ProperTailPathStep,
    ProperTailSite, ProperTailSiteKey, ProperTailSiteManifest, TypeTable, TypedModule,
};
use witchy_wir::canonical_safewir::{
    AccessMode, ArithmeticOperator, BasicBlock, BlockId, BlockParameter, BorrowMode,
    CallArgument, CallBorrow, CallableContract, CanonicalAbiCatalog, CanonicalModule,
    CheckedSourceIdentity, ComparisonOperator, Function, FunctionId, FunctionSignature,
    FunctionSignatureId, HostBufferExtent, HostCallArgument, LoweredCandidate,
    LoweredModuleParts, MemoryEffect, Operation, OverflowBehavior, ParameterContract,
    ParameterOwnership, Projection, ResultContract, Revision,
    Terminator, ValueId, ValueType,
};
use witchy_wir::layout::{
    HeaderLayout, LayoutBundle, LayoutInterner, LayoutKind, LayoutSize, PointerSlot, ScalarKind,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalLoweringError {
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
    let mut signatures = Vec::new();
    let mut functions_by_name = HashMap::new();
    let mut function_types = HashMap::new();
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
        let function_id = FunctionId::new(
            u32::try_from(signatures.len()).map_err(|_| CanonicalLoweringError::IdOverflow)?,
        );
        let signature_id = FunctionSignatureId::new(
            u32::try_from(signatures.len()).map_err(|_| CanonicalLoweringError::IdOverflow)?,
        );
        functions_by_name.insert(function.name.clone(), (function_id, signature_id));
        let function_type = typed
            .table()
            .function_type(&function.name)
            .ok_or_else(|| CanonicalLoweringError::MissingCheckedFunctionType(function.name.clone()))?;
        function_types.insert(function_type, signature_id);
        signatures.push(FunctionSignature {
            id: signature_id,
            parameters: Vec::new(),
            results: Vec::new(),
            contract: CallableContract {
                parameters: Vec::new(),
                results: Vec::new(),
                parameter_effects: BTreeMap::new(),
            },
        });
    }
    for item in &typed.module().items {
        let Item::Function(function) = item else { continue };
        if !is_canonical_source_function(typed.module(), &function.name) {
            continue;
        }
        let (_, signature_id) = functions_by_name[&function.name];
        signatures[signature_id.get() as usize] = checked_signature(
            typed,
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
                    ownership: ParameterOwnership::CallBorrow { mode: BorrowMode::Read },
                    non_escaping: true,
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
        let (function_id, signature_id) = functions_by_name[&function.name];
        let signature = &signatures[signature_id.get() as usize];
        functions.push(lower_function(
            function,
            function_id,
            u32::try_from(item_index).map_err(|_| CanonicalLoweringError::IdOverflow)?,
            signature,
            &functions_by_name,
            &signatures,
            &tail_manifest,
            typed.table(),
            &layouts,
            &layout_catalog,
            &string_data_ids,
            print_import,
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
    }, static_data_segments);
    Ok(match abi_catalog {
        Some(catalog) => candidate.with_abi_catalog(catalog),
        None => candidate,
    })
}

fn checked_signature(
    typed: &TypedModule,
    function: &witchy_syntax::ast::Function,
    id: FunctionSignatureId,
    layout_bundle: &LayoutBundle,
    layouts: &LayoutInterner,
    function_types: &HashMap<Type, FunctionSignatureId>,
) -> Result<FunctionSignature, CanonicalLoweringError> {
    let checked = typed
        .table()
        .function_type(&function.name)
        .ok_or_else(|| CanonicalLoweringError::MissingCheckedFunctionType(function.name.clone()))?;
    let Type::Fn(parameters, result, conventions, _) = checked else {
        return Err(CanonicalLoweringError::InvalidCheckedFunctionType(function.name.clone()));
    };
    if parameters.len() != function.params.len() {
        return Err(CanonicalLoweringError::InvalidCheckedFunctionType(function.name.clone()));
    }
    let conventions = if conventions.is_empty() {
        vec![Convention::Let; parameters.len()]
    } else {
        conventions
    };
    if conventions.len() != parameters.len() {
        return Err(CanonicalLoweringError::InvalidCheckedFunctionType(function.name.clone()));
    }

    let mut value_parameters = Vec::with_capacity(parameters.len());
    let mut contract_parameters = Vec::with_capacity(parameters.len());
    let mut parameter_effects = BTreeMap::new();
    for (parameter_index, ((parameter, source), convention)) in
        parameters.iter().zip(&function.params).zip(conventions).enumerate()
    {
        if !matches!(convention, Convention::Let | Convention::Borrow) {
            return Err(CanonicalLoweringError::UnsupportedConvention {
                function: function.name.clone(),
                parameter: source.name.clone(),
                convention,
            });
        }
        let value_type = source_value_type(parameter, layout_bundle, layouts, function_types)?;
        value_parameters.push(value_type);
        let (ownership, non_escaping) = match (value_type, convention) {
            (ValueType::Ref(_), Convention::Let | Convention::Borrow) => {
                (ParameterOwnership::CallBorrow { mode: BorrowMode::Read }, true)
            }
            (_, Convention::Let | Convention::Borrow) => (ParameterOwnership::Value, false),
            _ => unreachable!("unsupported conventions are rejected above"),
        };
        if matches!(value_type, ValueType::Ref(_)) {
            parameter_effects.insert(
                u32::try_from(parameter_index).map_err(|_| CanonicalLoweringError::IdOverflow)?,
                vec![MemoryEffect::Read],
            );
        }
        contract_parameters.push(ParameterContract {
            value_type,
            ownership,
            non_escaping,
        });
    }
    let result = source_value_type(&result, layout_bundle, layouts, function_types)?;
    let results = (!is_unit(result)).then_some(result).into_iter().collect::<Vec<_>>();
    let result_contracts = results
        .iter()
        .copied()
        .map(|result| match result {
            ValueType::Ref(layout) => ResultContract::Fresh { layout },
            result => ResultContract::Value(result),
        })
        .collect();

    Ok(FunctionSignature {
        id,
        parameters: value_parameters,
        results,
        contract: CallableContract {
            parameters: contract_parameters,
            results: result_contracts,
            parameter_effects,
        },
    })
}

fn lower_function(
    function: &witchy_syntax::ast::Function,
    id: FunctionId,
    item: u32,
    signature: &FunctionSignature,
    functions: &HashMap<String, (FunctionId, FunctionSignatureId)>,
    signatures: &[FunctionSignature],
    tail_manifest: &ProperTailSiteManifest,
    type_table: &TypeTable,
    layout_bundle: &LayoutBundle,
    layouts: &LayoutInterner,
    string_data_ids: &BTreeMap<Vec<u8>, witchy_wir::canonical_safewir::StaticDataId>,
    print_import: Option<witchy_wir::canonical_safewir::ImportId>,
) -> Result<Function, CanonicalLoweringError> {
    let mut lowerer = FunctionLowerer {
        function: function.name.clone(),
        next_value: 0,
        values: BTreeMap::new(),
        bindings: HashMap::new(),
        string_lengths: HashMap::new(),
        list_counts: HashMap::new(),
        known_list_lengths: HashMap::new(),
        blocks: Vec::new(),
        current: BlockId::new(0),
        item,
        functions,
        signatures,
        tail_manifest,
        type_table,
        layout_bundle,
        layouts,
        string_data_ids,
        print_import,
    };
    let entry = lowerer.new_block(&signature.parameters)?;
    let parameters = lowerer.blocks[entry.get() as usize].parameters.clone();
    for (source, parameter) in function.params.iter().zip(&parameters) {
        if lowerer.bindings.insert(source.name.clone(), parameter.value).is_some() {
            return Err(CanonicalLoweringError::DuplicateBinding(source.name.clone()));
        }
    }

    lowerer.lower_body(&function.body.stmts, &signature.results)?;
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
    Ok(Function {
        id,
        signature: signature.id,
        entry,
        value_types: lowerer.values,
        blocks,
    })
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
    list_counts: HashMap<ValueId, ValueId>,
    known_list_lengths: HashMap<ValueId, u64>,
    blocks: Vec<BlockBuilder>,
    current: BlockId,
    item: u32,
    functions: &'a HashMap<String, (FunctionId, FunctionSignatureId)>,
    signatures: &'a [FunctionSignature],
    tail_manifest: &'a ProperTailSiteManifest,
    type_table: &'a TypeTable,
    layout_bundle: &'a LayoutBundle,
    layouts: &'a LayoutInterner,
    string_data_ids: &'a BTreeMap<Vec<u8>, witchy_wir::canonical_safewir::StaticDataId>,
    print_import: Option<witchy_wir::canonical_safewir::ImportId>,
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
                    self.finish_current(self.return_value(value, results)?)?;
                    return Ok(());
                }
                Stmt::Expr(value) if final_statement => {
                    if let Expr::Call { name, args } = value {
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
                    if results.is_empty() && matches!(value, Expr::While { .. }) {
                        self.lower_effect_expr(value)?;
                        self.finish_current(Terminator::Return { values: Vec::new() })?;
                        return Ok(());
                    }
                    let value = self.lower_expr(value)?;
                    self.finish_current(self.return_value(Some(value), results)?)?;
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
            Expr::If { .. } => Err(CanonicalLoweringError::UnsupportedExpression("if without else")),
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
        }
        let join_id = self.new_block(&parameter_types)?;
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
            join_parameters[parameter_index].value
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

    fn lower_while(
        &mut self,
        condition: &Expr,
        body: &witchy_syntax::ast::Block,
    ) -> Result<(), CanonicalLoweringError> {
        let outer = self.bindings.clone();
        let mut names = outer.keys().cloned().collect::<Vec<_>>();
        names.sort();
        let types = names
            .iter()
            .map(|name| self.values[&outer[name]])
            .collect::<Vec<_>>();
        let header = self.new_block(&types)?;
        let body_id = self.new_block(&[])?;
        let exit = self.new_block(&types)?;
        let header_parameters = self.blocks[header.get() as usize].parameters.clone();
        let header_values = header_parameters.iter().map(|parameter| parameter.value).collect::<Vec<_>>();
        self.finish_current(Terminator::Branch {
            target: header,
            arguments: names.iter().map(|name| outer[name]).collect(),
        })?;
        self.current = header;
        self.bindings = names
            .iter()
            .zip(&header_values)
            .map(|(name, value)| (name.clone(), *value))
            .collect();
        let condition = self.lower_expr(condition)?;
        if self.values[&condition] != ValueType::Bool {
            return Err(CanonicalLoweringError::UnsupportedExpression("non-boolean condition"));
        }
        let exit_parameters = self.blocks[exit.get() as usize].parameters.clone();
        self.finish_current(Terminator::BranchIf {
            condition,
            then_target: body_id,
            then_arguments: Vec::new(),
            else_target: exit,
            else_arguments: header_values.clone(),
        })?;
        self.current = body_id;
        self.bindings = names
            .iter()
            .zip(&header_values)
            .map(|(name, value)| (name.clone(), *value))
            .collect();
        self.lower_block_effect(body)?;
        let body_end = self.current;
        let carried = names.iter().map(|name| self.bindings[name]).collect();
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
        let block = &mut self.blocks[self.current.get() as usize];
        if block.terminator.replace(terminator).is_some() {
            return Err(CanonicalLoweringError::UnsupportedStatement("duplicate block terminator"));
        }
        Ok(())
    }

    fn push_operation(&mut self, operation: Operation) {
        self.blocks[self.current.get() as usize].operations.push(operation);
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
        let arguments = self.lower_call_arguments(name, arguments, &signature)?;
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
        let arguments = self.lower_call_arguments(name, arguments, &signature)?;
        let destinations = signature
            .results
            .iter()
            .copied()
            .map(|value_type| self.fresh(value_type))
            .collect::<Result<Vec<_>, _>>()?;
        self.push_operation(Operation::Call {
            destinations: destinations.clone(),
            callee,
            arguments,
        });
        Ok(destinations)
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
            .chain(&signature.results)
            .copied()
            .any(|value_type| !is_indirect_scalar(value_type))
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
        let arguments = self.lower_call_arguments("<indirect tail>", arguments, &signature)?;
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
            .any(|value_type| !is_indirect_scalar(value_type))
        {
            return Err(CanonicalLoweringError::UnsupportedExpression(
                "memory-bearing indirect call parameter",
            ));
        }
        if let Some(result) = signature
            .results
            .iter()
            .copied()
            .find(|value_type| !is_indirect_scalar(*value_type))
        {
            return Err(CanonicalLoweringError::UnsupportedIndirectResult(result));
        }
        let arguments = self.lower_call_arguments("<indirect>", arguments, &signature)?;
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
            arguments,
        });
        Ok(destinations)
    }

    fn lower_call_arguments(
        &mut self,
        callee: &str,
        arguments: &[Expr],
        signature: &FunctionSignature,
    ) -> Result<Vec<CallArgument>, CanonicalLoweringError> {
        let parameters = &signature.contract.parameters;
        if arguments.len() != parameters.len() {
            return Err(CanonicalLoweringError::CallArityMismatch {
                callee: callee.to_owned(),
                expected: parameters.len(),
                actual: arguments.len(),
            });
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
                match contract.ownership {
                    ParameterOwnership::Value => Ok(CallArgument::Value(value)),
                    ParameterOwnership::CallBorrow { mode: BorrowMode::Read } => {
                        Ok(CallArgument::Borrow(CallBorrow::new(value, BorrowMode::Read)))
                    }
                    ownership => Err(CanonicalLoweringError::UnsupportedCallOwnership {
                        callee: callee.to_owned(),
                        parameter,
                        ownership,
                    }),
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
                Ok(value)
            }
            Expr::Call { name, args } if is_console_print(name) => Err(
                CanonicalLoweringError::UnsupportedExpression("console.print used as a value"),
            ),
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
                        Ok(value)
                    }
                    [] => Err(CanonicalLoweringError::UnsupportedExpression("void call used as value")),
                    _ => Err(CanonicalLoweringError::UnsupportedExpression("multi-result call")),
                }
            }
            Expr::Str(value) => self.lower_string_literal(value),
            Expr::List(items) => self.lower_list_literal(expression, items),
            Expr::Tuple(_) => Err(CanonicalLoweringError::UnsupportedExpression("tuple")),
            Expr::Ctor { .. } | Expr::AnonCtor { .. } => {
                Err(CanonicalLoweringError::UnsupportedExpression("constructor"))
            }
            Expr::Unary { .. } => Err(CanonicalLoweringError::UnsupportedExpression("unary")),
            Expr::Binary { op, lhs, rhs } => self.lower_binary(*op, lhs, rhs),
            Expr::Field { .. } => Err(CanonicalLoweringError::UnsupportedExpression("field")),
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
            Expr::Match { .. } => Err(CanonicalLoweringError::UnsupportedExpression("match")),
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
        let object = self.fresh(ValueType::Ref(layout))?;
        self.push_operation(Operation::Alloc {
            destination: object,
            layout,
            extent: witchy_wir::canonical_safewir::Extent::Counted {
                header_bytes,
                count,
                capacity,
                stride_bytes,
            },
            class: witchy_wir::canonical_safewir::AllocationClass::ReferenceCounted,
        });
        self.list_counts.insert(object, count);
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
                value = self.clone_string_for_storage(value)?;
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
                right: count,
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
    /// The v1 template is deliberately limited to statically known lengths.
    /// A dynamic-length move needs a checked generic range operation; guessing
    /// from a helper name would violate the canonical SafeWIR boundary.
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
        let old_length = *self
            .list_counts
            .get(&old_object)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("list.push dynamic length"))?;
        let old_length_value = *self
            .known_list_lengths
            .get(&old_object)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("list.push dynamic length"))?;
        let mut value = self.lower_expr(value)?;
        if self.values[&value] != element_type {
            return Err(CanonicalLoweringError::UnsupportedListElement(list_type));
        }
        if matches!(element_type, ValueType::Ref(_)) {
            value = self.clone_string_for_storage(value)?;
        }

        let maximum = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::ConstSize {
            destination: maximum,
            value: u64::from(u32::MAX),
        });
        let increment_fits = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::Compare {
            destination: increment_fits,
            operator: ComparisonOperator::LessUnsigned,
            left: old_length,
            right: maximum,
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
        let new_length_value = old_length_value
            .checked_add(1)
            .ok_or(CanonicalLoweringError::IdOverflow)?;
        self.push_operation(Operation::ConstSize {
            destination: capacity,
            value: new_length_value,
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
        let new_object = self.fresh(ValueType::Ref(layout))?;
        self.push_operation(Operation::Alloc {
            destination: new_object,
            layout,
            extent: witchy_wir::canonical_safewir::Extent::Counted {
                header_bytes,
                count: old_length,
                capacity,
                stride_bytes,
            },
            class: witchy_wir::canonical_safewir::AllocationClass::ReferenceCounted,
        });

        for position in 0..old_length_value {
            let index = self.fresh(ValueType::Size)?;
            self.push_operation(Operation::ConstSize { destination: index, value: position });
            let in_bounds = self.fresh(ValueType::Bool)?;
            self.push_operation(Operation::Compare {
                destination: in_bounds,
                operator: ComparisonOperator::LessUnsigned,
                left: index,
                right: old_length,
            });
            self.push_operation(Operation::Guard {
                condition: in_bounds,
                kind: witchy_wir::canonical_safewir::GuardKind::Bounds,
            });
            let copied = self.fresh(element_type)?;
            self.push_operation(Operation::Load {
                destination: copied,
                place: witchy_wir::canonical_safewir::Place::projected(
                    old_object,
                    witchy_wir::canonical_safewir::Projection::Element { index },
                    element_type,
                    witchy_wir::canonical_safewir::AccessMode::Read,
                ),
                transfer: if matches!(element_type, ValueType::Ref(_)) {
                    witchy_wir::canonical_safewir::LoadTransfer::MoveUnique
                } else {
                    witchy_wir::canonical_safewir::LoadTransfer::CopyScalar
                },
            });
            self.push_operation(Operation::Store {
                place: witchy_wir::canonical_safewir::Place::projected(
                    new_object,
                    witchy_wir::canonical_safewir::Projection::Element { index },
                    element_type,
                    witchy_wir::canonical_safewir::AccessMode::Write,
                ),
                value: copied,
                transfer: if matches!(element_type, ValueType::Ref(_)) {
                    witchy_wir::canonical_safewir::StoreTransfer::MoveUnique
                } else {
                    witchy_wir::canonical_safewir::StoreTransfer::CopyScalar
                },
            });
        }

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
        let append_index = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::ConstSize {
            destination: append_index,
            value: old_length_value,
        });
        let append_in_bounds = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::Compare {
            destination: append_in_bounds,
            operator: ComparisonOperator::LessUnsigned,
            left: append_index,
            right: new_length,
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
            transfer: if matches!(element_type, ValueType::Ref(_)) {
                witchy_wir::canonical_safewir::StoreTransfer::MoveUnique
            } else {
                witchy_wir::canonical_safewir::StoreTransfer::CopyScalar
            },
        });
        self.push_operation(Operation::Reclaim { object: old_object });
        self.bindings.insert(name.clone(), new_object);
        self.list_counts.insert(new_object, new_length);
        self.known_list_lengths.insert(new_object, new_length_value);
        Ok(())
    }

    fn lower_index(&mut self, base: &Expr, index: &Expr) -> Result<ValueId, CanonicalLoweringError> {
        let logical_type = self.expression_type(base)?;
        let (layout, element_type, header_bytes, stride_bytes) =
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
        let logical_length = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::Load {
            destination: logical_length,
            place: witchy_wir::canonical_safewir::Place::projected(
                object,
                witchy_wir::canonical_safewir::Projection::Header(
                    witchy_wir::canonical_safewir::HeaderField::Length,
                ),
                ValueType::Size,
                witchy_wir::canonical_safewir::AccessMode::Read,
            ),
            transfer: witchy_wir::canonical_safewir::LoadTransfer::CopyScalar,
        });
        // A borrowed parameter starts with authority for its catalog-authenticated
        // header only. Authenticate both dynamic header values before allowing a
        // body projection: logical length is bounded by capacity, and capacity
        // cannot overflow the Wasm32 allocation size for this layout.
        if !self.list_counts.contains_key(&object) {
            let capacity = self.fresh(ValueType::Size)?;
            self.push_operation(Operation::Load {
                destination: capacity,
                place: witchy_wir::canonical_safewir::Place::projected(
                    object,
                    witchy_wir::canonical_safewir::Projection::Header(
                        witchy_wir::canonical_safewir::HeaderField::Capacity,
                    ),
                    ValueType::Size,
                    witchy_wir::canonical_safewir::AccessMode::Read,
                ),
                transfer: witchy_wir::canonical_safewir::LoadTransfer::CopyScalar,
            });
            let count_fits = self.fresh(ValueType::Bool)?;
            self.push_operation(Operation::Compare {
                destination: count_fits,
                operator: ComparisonOperator::LessEqualUnsigned,
                left: logical_length,
                right: capacity,
            });
            self.push_operation(Operation::Guard {
                condition: count_fits,
                kind: witchy_wir::canonical_safewir::GuardKind::Bounds,
            });
            let maximum = (u64::from(u32::MAX) - u64::from(header_bytes))
                / u64::from(stride_bytes);
            let maximum_value = self.fresh(ValueType::Size)?;
            self.push_operation(Operation::ConstSize {
                destination: maximum_value,
                value: maximum,
            });
            let allocation_fits = self.fresh(ValueType::Bool)?;
            self.push_operation(Operation::Compare {
                destination: allocation_fits,
                operator: ComparisonOperator::LessEqualUnsigned,
                left: capacity,
                right: maximum_value,
            });
            self.push_operation(Operation::Guard {
                condition: allocation_fits,
                kind: witchy_wir::canonical_safewir::GuardKind::Arithmetic,
            });
        }
        let logical_count = logical_length;
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
        Type::Named(name, arguments) if name == "String" && arguments.is_empty() => {
            string_layout(source, layout_bundle, layouts).map(ValueType::Ref)
        }
        Type::Named(name, arguments) if name == "Console" && arguments.is_empty() => {
            Ok(ValueType::Capability(witchy_wir::canonical_safewir::CapabilityKindId::new(0)))
        }
        _ => scalar_type(source),
    }
}

fn is_indirect_scalar(value_type: ValueType) -> bool {
    matches!(
        value_type,
        ValueType::Int | ValueType::Float | ValueType::Bool | ValueType::Size | ValueType::V128
    )
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
    let mut payloads = BTreeSet::new();
    for item in &module.items {
        let Item::Function(function) = item else { continue };
        if !is_canonical_source_function(module, &function.name) { continue; }
        witchy_syntax::ast::visit::visit_block(&function.body, &mut |expression| {
            if let Expr::Str(value) = expression {
                payloads.insert(value.as_bytes().to_vec());
            }
            Ok::<(), ()>(())
        }).expect("string payload visitor cannot fail");
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
    let element_name = match arguments[0].unqualified() {
        Type::Named(element, args) if args.is_empty() => element.as_str(),
        _ => return Err(CanonicalLoweringError::UnsupportedListElement(source.clone())),
    };

    let mut candidates = Vec::new();
    for root in layout_bundle.roots() {
        let Some(descriptor) = layouts.get(*root) else { continue };
        let LayoutKind::PackedList { element, .. } = descriptor.kind() else { continue };
        let Some(element_descriptor) = layouts.get(*element) else { continue };
        let element_type = match (element_name, element_descriptor.kind()) {
            ("Int", LayoutKind::Scalar(ScalarKind::Int)) => ValueType::Int,
            (
                "String",
                LayoutKind::RuntimePointerValueSlot {
                    pointee,
                    semantics: PointerSlot::Owned,
                },
            ) if layouts.get(*pointee).is_some_and(|descriptor| {
                matches!(descriptor.kind(), LayoutKind::RuntimeBuffer { element }
                    if layouts.get(*element).is_some_and(|element| element.kind() == &LayoutKind::Scalar(ScalarKind::RawByte)))
            }) => ValueType::Ref(*pointee),
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
                            && !is_console_print_tail_site(site, function)
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

fn is_console_print_tail_site(
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
        Some(Stmt::Expr(Expr::Call { name, .. })) if is_console_print(name) => true,
        Some(Stmt::Return(Some(Expr::Call { name, .. }))) if is_console_print(name) => true,
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
    use witchy_syntax::ast::Item;
    use witchy_syntax::parser::parse_module;
    use witchy_wir::canonical_safewir::{
        check_initial, BorrowMode, CallArgument, Extent, GuardKind, LoadTransfer, Operation,
        ParameterOwnership, Projection, Terminator,
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

    #[test]
    fn checked_scalar_source_lowers_and_validates_without_wir() {
        let candidate = lower("fn answer() -> Int:\n    let x = 40\n    x + 2\n");
        assert!(check_initial(candidate).is_ok());
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
        assert!(check_initial(candidate).is_ok());
    }

    #[test]
    fn nested_scalar_block_preserves_the_join() {
        let candidate = lower(
            "fn nested(flag: Bool) -> Int:\n    if flag:\n        if flag:\n            1\n        else:\n            2\n    else:\n        3\n",
        );
        assert!(candidate.functions()[0].blocks.len() >= 4);
        assert!(check_initial(candidate).is_ok());
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
        assert!(check_initial(candidate).is_ok());
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
            Operation::Compare { operator: witchy_wir::canonical_safewir::ComparisonOperator::LessUnsigned, .. }
        )));
        assert!(operations.iter().any(|operation| matches!(
            operation,
            Operation::Guard { kind: GuardKind::Bounds, .. }
        )));
        assert!(operations.iter().any(|operation| matches!(
            operation,
            Operation::Load { place, transfer: LoadTransfer::CopyScalar, .. }
                if matches!(place.projection(), Projection::Header(witchy_wir::canonical_safewir::HeaderField::Length))
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
            Operation::Reclaim { .. }
        )));
        assert!(check_initial(candidate).is_ok());
    }

    #[test]
    fn borrowed_list_parameter_index_uses_loaded_length_for_bounds_guard() {
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
    fn borrowed_list_parameter_index_promotes_authenticated_header_extent() {
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
    fn reference_let_parameter_uses_a_non_escaping_read_call_borrow() {
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
            ParameterOwnership::CallBorrow { mode: BorrowMode::Read }
        ));
        assert!(callee.contract.parameters[0].non_escaping);

        let call = candidate.functions()[1].blocks[0]
            .operations
            .iter()
            .find_map(|operation| match operation {
                Operation::Call { arguments, .. } => Some(arguments),
                _ => None,
            })
            .expect("caller has a direct object call");
        assert!(matches!(
            call.as_slice(),
            [CallArgument::Borrow(borrow)]
                if borrow.mode() == BorrowMode::Read
        ));
    }

    #[test]
    fn reference_tail_call_uses_a_read_call_borrow() {
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
        assert!(matches!(
            tail.as_slice(),
            [CallArgument::Borrow(borrow)]
                if borrow.mode() == BorrowMode::Read
        ));
    }

    #[test]
    fn unsupported_own_parameter_still_fails_closed() {
        let checked = checked("fn consume(own value: Int) -> Int:\n    value\n");
        let error = lower_typed_module(
            checked.canonical_source().expect("canonical source"),
            empty_layouts(),
        )
        .expect_err("v1 canonical lowering does not guess move ownership");
        assert!(matches!(error, CanonicalLoweringError::UnsupportedConvention { .. }));
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
