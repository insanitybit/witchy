//! Direct checked-AST to canonical SafeWIR lowering.
//!
//! This module never constructs legacy WIR. Its initial scalar subset is
//! deliberately fail-closed; every unsupported source operation is reported
//! before a `LoweredCandidate` can be returned.

use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::fmt;

use witchy_syntax::ast::{BinOp, Convention, Expr, Item, Stmt, Type};
use witchy_types::pipeline::CanonicalCheckedSource;
use witchy_types::typeck::{
    ProperTailCallableId, ProperTailDispatchKind, ProperTailExprKind, ProperTailPathStep,
    ProperTailSiteKey, ProperTailSiteManifest, TypedModule,
};
use witchy_wir::canonical_safewir::{
    ArithmeticOperator, BasicBlock, BlockId, BlockParameter, CallableContract, CanonicalModule,
    CheckedSourceIdentity, ComparisonOperator, Function, FunctionId, FunctionSignature,
    FunctionSignatureId, LoweredCandidate, LoweredModuleParts, Operation, OverflowBehavior,
    ParameterContract, ParameterOwnership, ResultContract, Revision, Terminator, ValueId,
    ValueType,
};
use witchy_wir::layout::LayoutBundle;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalLoweringError {
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
    IdOverflow,
    TailManifest(String),
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
    let tail_manifest = source.proper_tail_manifest();
    let mut signatures = Vec::new();
    let mut functions_by_name = HashMap::new();
    for (item_index, item) in typed.module().items.iter().enumerate() {
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
            u32::try_from(item_index).map_err(|_| CanonicalLoweringError::IdOverflow)?,
        );
        let signature_id = FunctionSignatureId::new(
            u32::try_from(signatures.len()).map_err(|_| CanonicalLoweringError::IdOverflow)?,
        );
        let signature = checked_signature(typed, function, signature_id)?;
        functions_by_name.insert(function.name.clone(), (function_id, signature_id));
        signatures.push(signature);
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
            &tail_manifest,
        )?);
    }

    Ok(CanonicalModule::from_lowering(LoweredModuleParts {
        revision: Revision::new(0),
        checked_source: CheckedSourceIdentity::from_digest(
            source.source_witness().source_bundle_digest(),
        ),
        layouts,
        signatures,
        imports: Vec::new(),
        functions,
        proper_tail_sites: canonical_tail_sites(tail_manifest, typed.module())?,
    }))
}

fn checked_signature(
    typed: &TypedModule,
    function: &witchy_syntax::ast::Function,
    id: FunctionSignatureId,
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
    for ((parameter, source), convention) in parameters.iter().zip(&function.params).zip(conventions) {
        if !matches!(convention, Convention::Let | Convention::Borrow) {
            return Err(CanonicalLoweringError::UnsupportedConvention {
                function: function.name.clone(),
                parameter: source.name.clone(),
                convention,
            });
        }
        let value_type = scalar_type(parameter)?;
        value_parameters.push(value_type);
        contract_parameters.push(ParameterContract {
            value_type,
            ownership: ParameterOwnership::Value,
            non_escaping: false,
        });
    }
    let result = scalar_type(&result)?;
    let results = (!is_unit(result)).then_some(result).into_iter().collect::<Vec<_>>();
    let result_contracts = results.iter().copied().map(ResultContract::Value).collect();

    Ok(FunctionSignature {
        id,
        parameters: value_parameters,
        results,
        contract: CallableContract {
            parameters: contract_parameters,
            results: result_contracts,
            parameter_effects: BTreeMap::new(),
        },
    })
}

fn lower_function(
    function: &witchy_syntax::ast::Function,
    id: FunctionId,
    item: u32,
    signature: &FunctionSignature,
    functions: &HashMap<String, (FunctionId, FunctionSignatureId)>,
    tail_manifest: &ProperTailSiteManifest,
) -> Result<Function, CanonicalLoweringError> {
    let mut lowerer = FunctionLowerer {
        function: function.name.clone(),
        next_value: 0,
        values: BTreeMap::new(),
        bindings: HashMap::new(),
        blocks: Vec::new(),
        current: BlockId::new(0),
        item,
        functions,
        tail_manifest,
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
    blocks: Vec<BlockBuilder>,
    current: BlockId,
    item: u32,
    functions: &'a HashMap<String, (FunctionId, FunctionSignatureId)>,
    tail_manifest: &'a ProperTailSiteManifest,
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
                    let value = self.lower_expr(value)?;
                    self.bindings.insert(name.clone(), value);
                }
                Stmt::Return(value) => {
                    if !final_statement {
                        return Err(CanonicalLoweringError::UnsupportedStatement("early return"));
                    }
                    if let Some(Expr::Call { name, args }) = value {
                        let terminator = self.lower_direct_tail_call(
                            name,
                            args,
                            index,
                            ProperTailPathStep::ReturnValue,
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
                        let terminator = self.lower_direct_tail_call(
                            name,
                            args,
                            index,
                            ProperTailPathStep::Value,
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
        let (callee, _) = self
            .functions
            .get(name)
            .copied()
            .ok_or_else(|| CanonicalLoweringError::MissingBinding(name.to_owned()))?;
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
        let arguments = arguments
            .iter()
            .map(|argument| {
                self.lower_expr(argument)
                    .map(witchy_wir::canonical_safewir::CallArgument::Value)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Terminator::TailCall {
            site: witchy_wir::canonical_safewir::TailSiteId::from_digest(site.id.as_bytes()),
            callee,
            arguments,
        })
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
            Expr::Var(name) => self
                .bindings
                .get(name)
                .copied()
                .ok_or_else(|| CanonicalLoweringError::MissingBinding(name.clone())),
            Expr::Call { .. }
            | Expr::LabeledCall { .. }
            | Expr::LabeledMethodCall { .. }
            | Expr::MethodCall { .. }
            | Expr::Apply { .. } => Err(CanonicalLoweringError::UnsupportedExpression("call")),
            Expr::Str(_) => Err(CanonicalLoweringError::UnsupportedExpression("string")),
            Expr::List(_) => Err(CanonicalLoweringError::UnsupportedExpression("list")),
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
            Expr::Index { .. } => Err(CanonicalLoweringError::UnsupportedExpression("index")),
            Expr::WhileLet { .. } => Err(CanonicalLoweringError::UnsupportedExpression("while let")),
            Expr::TaggedLit { .. } => {
                Err(CanonicalLoweringError::UnsupportedExpression("tagged literal"))
            }
        }
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
        if matches!(operator, BinOp::Concat | BinOp::And | BinOp::Or | BinOp::Coalesce) {
            return Err(CanonicalLoweringError::UnsupportedExpression(
                "short-circuit or concatenating binary operation",
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
                    }
                    _ => false,
                })
        })
        .map(|site| {
            if site.key.callable.method.is_some() || !site.key.callable.lambda_path.is_empty() {
                return Err(CanonicalLoweringError::UnsupportedExpression("method or closure tail site"));
            }
            Ok(witchy_wir::canonical_safewir::ProperTailSite {
                id: witchy_wir::canonical_safewir::TailSiteId::from_digest(site.id.as_bytes()),
                function: FunctionId::new(site.key.callable.item),
            })
        })
        .collect()
}

fn is_canonical_source_function(module: &witchy_syntax::ast::Module, name: &str) -> bool {
    let Some(entry) = module.linked_entry.as_deref() else {
        return true;
    };
    name == entry || name.starts_with(&format!("{entry}."))
}

#[cfg(test)]
mod tests {
    use super::lower_typed_module;
    use witchy_syntax::ast::Item;
    use witchy_syntax::parser::parse_module;
    use witchy_wir::canonical_safewir::{check_initial, Terminator};
    use witchy_wir::layout::{LayoutBundle, LayoutInterner};

    fn empty_layouts() -> LayoutBundle {
        LayoutBundle::from_interner(&LayoutInterner::new(), [])
            .expect("empty layouts are canonical")
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
