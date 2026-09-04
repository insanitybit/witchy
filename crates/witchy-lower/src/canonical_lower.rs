//! Direct checked-AST to canonical SafeWIR lowering.
//!
//! This module never constructs legacy WIR. Its initial scalar subset is
//! deliberately fail-closed; every unsupported source operation is reported
//! before a `LoweredCandidate` can be returned.

use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::fmt;

use witchy_syntax::ast::{BinOp, Convention, Expr, Item, Stmt, Type};
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
/// The source identity and layout bundle are independent checked-pipeline
/// inputs. Supplying them does not grant validation or encoding authority.
pub fn lower_typed_module(
    typed: &TypedModule,
    checked_source: CheckedSourceIdentity,
    layouts: LayoutBundle,
) -> Result<CanonicalModule<LoweredCandidate>, CanonicalLoweringError> {
    let tail_manifest = typed
        .proper_tail_manifest()
        .map_err(|error| CanonicalLoweringError::TailManifest(error.to_string()))?;
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
        checked_source,
        layouts,
        signatures,
        imports: Vec::new(),
        functions,
        proper_tail_sites: canonical_tail_sites(&tail_manifest)?,
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
        return Err(CanonicalLoweringError::InvalidCheckedFunctionType(
            function.name.clone(),
        ));
    };
    if parameters.len() != function.params.len() {
        return Err(CanonicalLoweringError::InvalidCheckedFunctionType(
            function.name.clone(),
        ));
    }
    let conventions = if conventions.is_empty() {
        vec![Convention::Let; parameters.len()]
    } else {
        conventions
    };
    if conventions.len() != parameters.len() {
        return Err(CanonicalLoweringError::InvalidCheckedFunctionType(
            function.name.clone(),
        ));
    }

    let mut value_parameters = Vec::with_capacity(parameters.len());
    let mut contract_parameters = Vec::with_capacity(parameters.len());
    for ((parameter, source), convention) in parameters
        .iter()
        .zip(&function.params)
        .zip(conventions)
    {
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
    let result_contracts = results
        .iter()
        .copied()
        .map(ResultContract::Value)
        .collect();

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
    let entry = BlockId::new(0);
    let mut lowerer = FunctionLowerer {
        function: function.name.clone(),
        next_value: 0,
        values: BTreeMap::new(),
        bindings: HashMap::new(),
        operations: Vec::new(),
        item,
        functions,
        tail_manifest,
    };
    let mut parameters = Vec::new();
    for (source, value_type) in function.params.iter().zip(&signature.parameters) {
        let value = lowerer.fresh(*value_type)?;
        if lowerer.bindings.insert(source.name.clone(), value).is_some() {
            return Err(CanonicalLoweringError::DuplicateBinding(source.name.clone()));
        }
        parameters.push(BlockParameter {
            value,
            value_type: *value_type,
            ownership: None,
        });
    }

    let terminator = lowerer.lower_body(&function.body.stmts, &signature.results)?;
    Ok(Function {
        id,
        signature: signature.id,
        entry,
        value_types: lowerer.values,
        blocks: vec![BasicBlock {
            id: entry,
            parameters,
            operations: lowerer.operations,
            terminator,
        }],
    })
}

struct FunctionLowerer<'a> {
    function: String,
    next_value: u32,
    values: BTreeMap<ValueId, ValueType>,
    bindings: HashMap<String, ValueId>,
    operations: Vec<Operation>,
    item: u32,
    functions: &'a HashMap<String, (FunctionId, FunctionSignatureId)>,
    tail_manifest: &'a ProperTailSiteManifest,
}

impl FunctionLowerer<'_> {
    fn lower_body(
        &mut self,
        statements: &[Stmt],
        results: &[ValueType],
    ) -> Result<Terminator, CanonicalLoweringError> {
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
                        return self.lower_direct_tail_call(
                            name,
                            args,
                            index,
                            ProperTailPathStep::ReturnValue,
                        );
                    }
                    return self.return_value(value.as_ref(), results);
                }
                Stmt::Expr(value) if final_statement => {
                    if let Expr::Call { name, args } = value {
                        return self.lower_direct_tail_call(
                            name,
                            args,
                            index,
                            ProperTailPathStep::Value,
                        );
                    }
                    return self.return_value(Some(value), results);
                }
                Stmt::Expr(value) => {
                    self.lower_expr(value)?;
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
        if results.is_empty() {
            Ok(Terminator::Return { values: Vec::new() })
        } else {
            Err(CanonicalLoweringError::MissingReturn(self.function.clone()))
        }
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
        let statement =
            u32::try_from(statement).map_err(|_| CanonicalLoweringError::IdOverflow)?;
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
            .ok_or_else(|| {
                CanonicalLoweringError::MissingProperTailSite(self.function.clone())
            })?;
        let arguments = arguments
            .iter()
            .map(|argument| self.lower_expr(argument).map(witchy_wir::canonical_safewir::CallArgument::Value))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Terminator::TailCall {
            site: witchy_wir::canonical_safewir::TailSiteId::from_digest(site.id.as_bytes()),
            callee,
            arguments,
        })
    }

    fn return_value(
        &mut self,
        expression: Option<&Expr>,
        results: &[ValueType],
    ) -> Result<Terminator, CanonicalLoweringError> {
        match (expression, results) {
            (None, []) => Ok(Terminator::Return { values: Vec::new() }),
            (Some(expression), [expected]) => {
                let value = self.lower_expr(expression)?;
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
            _ => Err(CanonicalLoweringError::InvalidCheckedFunctionType(
                self.function.clone(),
            )),
        }
    }

    fn lower_expr(&mut self, expression: &Expr) -> Result<ValueId, CanonicalLoweringError> {
        match expression {
            Expr::Int(value) => {
                let destination = self.fresh(ValueType::Int)?;
                self.operations.push(Operation::ConstInt {
                    destination,
                    value: *value,
                });
                Ok(destination)
            }
            Expr::Duration(value) => {
                let destination = self.fresh(ValueType::Int)?;
                self.operations.push(Operation::ConstInt {
                    destination,
                    value: *value,
                });
                Ok(destination)
            }
            Expr::Float(value) => {
                let destination = self.fresh(ValueType::Float)?;
                self.operations.push(Operation::ConstFloat {
                    destination,
                    bits: value.to_bits(),
                });
                Ok(destination)
            }
            Expr::Bool(value) => {
                let destination = self.fresh(ValueType::Bool)?;
                self.operations.push(Operation::ConstBool {
                    destination,
                    value: *value,
                });
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
            Expr::If { .. } => Err(CanonicalLoweringError::UnsupportedExpression("if")),
            Expr::Match { .. } => Err(CanonicalLoweringError::UnsupportedExpression("match")),
            Expr::Block(_) => Err(CanonicalLoweringError::UnsupportedExpression("block")),
            Expr::While { .. } => Err(CanonicalLoweringError::UnsupportedExpression("while")),
            Expr::For { .. } => Err(CanonicalLoweringError::UnsupportedExpression("for")),
            Expr::Range { .. } => Err(CanonicalLoweringError::UnsupportedExpression("range")),
            Expr::Index { .. } => Err(CanonicalLoweringError::UnsupportedExpression("index")),
            Expr::WhileLet { .. } => {
                Err(CanonicalLoweringError::UnsupportedExpression("while let"))
            }
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
        let left = self.lower_expr(left)?;
        let right = self.lower_expr(right)?;
        let left_type = self.values[&left];
        let right_type = self.values[&right];
        if left_type != right_type {
            return Err(CanonicalLoweringError::UnsupportedExpression(
                "mixed-type binary operation",
            ));
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
                return Err(CanonicalLoweringError::UnsupportedExpression(
                    "comparison type",
                ));
            }
            if left_type == ValueType::Bool
                && !matches!(operator, ComparisonOperator::Equal | ComparisonOperator::NotEqual)
            {
                return Err(CanonicalLoweringError::UnsupportedExpression(
                    "ordered boolean comparison",
                ));
            }
            let destination = self.fresh(ValueType::Bool)?;
            self.operations.push(Operation::Compare {
                destination,
                operator,
                left,
                right,
            });
            return Ok(destination);
        }

        if left_type != ValueType::Int {
            return Err(CanonicalLoweringError::UnsupportedExpression(
                "non-integer arithmetic",
            ));
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
            BinOp::Concat | BinOp::And | BinOp::Or | BinOp::Coalesce => {
                return Err(CanonicalLoweringError::UnsupportedExpression(
                    "short-circuit or concatenating binary operation",
                ));
            }
            BinOp::Eq
            | BinOp::NotEq
            | BinOp::Lt
            | BinOp::LtEq
            | BinOp::Gt
            | BinOp::GtEq => unreachable!(),
        };
        let destination = self.fresh(ValueType::Int)?;
        self.operations.push(Operation::Arithmetic {
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
) -> Result<Vec<witchy_wir::canonical_safewir::ProperTailSite>, CanonicalLoweringError> {
    manifest
        .sites()
        .iter()
        .map(|site| {
            if site.key.callable.method.is_some() || !site.key.callable.lambda_path.is_empty() {
                return Err(CanonicalLoweringError::UnsupportedExpression(
                    "method or closure tail site",
                ));
            }
            Ok(witchy_wir::canonical_safewir::ProperTailSite {
                id: witchy_wir::canonical_safewir::TailSiteId::from_digest(site.id.as_bytes()),
                function: FunctionId::new(site.key.callable.item),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::lower_typed_module;
    use witchy_syntax::parser::parse_module;
    use witchy_wir::canonical_safewir::{CheckedSourceIdentity, check_initial};
    use witchy_wir::layout::{LayoutBundle, LayoutInterner};

    fn empty_layouts() -> LayoutBundle {
        LayoutBundle::from_interner(&LayoutInterner::new(), [])
            .expect("empty layouts are canonical")
    }

    #[test]
    fn checked_scalar_source_lowers_and_validates_without_wir() {
        let module = parse_module("fn answer() -> Int:\n    let x = 40\n    x + 2\n")
            .expect("parse scalar fixture");
        let typed = witchy_types::typeck::annotate_checked(module).expect("type check fixture");
        let candidate = lower_typed_module(
            &typed,
            CheckedSourceIdentity::from_digest([3; 32]),
            empty_layouts(),
        )
        .expect("lower canonical SafeWIR");

        assert!(check_initial(candidate).is_ok());
    }

    #[test]
    fn proper_tail_source_lowers_with_manifest_identity() {
        let module = parse_module(
            "fn recurse(n: Int) -> Int:\n    return recurse(n)\n",
        )
        .expect("parse tail fixture");
        let typed = witchy_types::typeck::annotate_checked(module).expect("type check fixture");

        let candidate = lower_typed_module(
            &typed,
            CheckedSourceIdentity::from_digest([3; 32]),
            empty_layouts(),
        )
        .expect("lower proper tail call");

        assert!(check_initial(candidate).is_ok());
    }
}
