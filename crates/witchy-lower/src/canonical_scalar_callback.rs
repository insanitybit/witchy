use super::*;

#[derive(PartialEq, Eq)]
struct ScalarSignature {
    parameters: Vec<(Type, Convention)>,
    result: Type,
}

pub(super) struct Forwarders {
    helpers: HashMap<String, ScalarSignature>,
    targets: HashMap<String, ScalarSignature>,
}

fn scalar(ty: &Type) -> bool {
    matches!(scalar_type(ty), Ok(ValueType::Int | ValueType::Float | ValueType::Bool))
}

fn leaf_size(expression: &Expr) -> Option<usize> {
    match expression {
        Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Bool(_) | Expr::Var(_) => Some(1),
        Expr::Unary { op: UnOp::Neg | UnOp::Not | UnOp::BitNot, expr } =>
            leaf_size(expr)?.checked_add(1).filter(|size| *size <= 32),
        Expr::Binary { op, lhs, rhs } if !matches!(op, BinOp::Concat | BinOp::Coalesce) =>
            leaf_size(lhs)?.checked_add(leaf_size(rhs)?)?.checked_add(1).filter(|size| *size <= 32),
        _ => None,
    }
}

impl Forwarders {
    pub(super) fn new(module: &witchy_syntax::ast::Module) -> Self {
        let mut helpers = HashMap::new();
        let mut targets = HashMap::new();
        if !witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::ClosureElide) {
            return Self { helpers, targets };
        }
        for item in &module.items {
            let Item::Function(function) = item else { continue };
            if function.comptime_only || function.is_async || function.is_gen
                || function.attributes.iter().any(|attribute|
                    !attribute.starts_with(witchy_syntax::linker::LINKED_SOURCE_MODULE_ATTRIBUTE_PREFIX))
                || !function.type_params.is_empty()
                || !function.bounds.is_empty()
            {
                continue;
            }
            let Some(result) = function.ret.as_ref().filter(|ty| scalar(ty)) else { continue };
            if function.params.iter().any(|parameter| parameter.default.is_some()
                || !matches!(parameter.convention, Convention::Let | Convention::Borrow))
            {
                continue;
            }
            let [Stmt::Expr(body)] = function.body.stmts.as_slice() else { continue };
            if function.params.len() <= 4 && leaf_size(body).is_some()
                && function.params.iter().all(|parameter| parameter.ty.as_ref().is_some_and(scalar))
            {
                targets.insert(function.name.clone(), ScalarSignature {
                    parameters: function.params.iter().map(|parameter|
                        (parameter.ty.clone().expect("scalar parameter"), parameter.convention)).collect(),
                    result: result.clone(),
                });
            }
            let Some((callback, parameters)) = function.params.split_first() else { continue };
            let Some(Type::Fn(inputs, output, conventions, qualifiers)) = callback.ty.as_ref() else { continue };
            if qualifiers.once || inputs.len() > 4 || inputs.len() != parameters.len()
                || conventions.len() != inputs.len() || output.as_ref() != result
                || !inputs.iter().all(scalar)
                || !parameters.iter().zip(inputs).zip(conventions).all(|((parameter, input), convention)|
                    parameter.ty.as_ref() == Some(input) && parameter.convention == *convention)
            {
                continue;
            }
            let Expr::Call { name, args } = body else { continue };
            if name != &callback.name || args.len() != parameters.len()
                || !args.iter().zip(parameters).all(|(argument, parameter)|
                    matches!(argument, Expr::Var(name) if name == &parameter.name))
            {
                continue;
            }
            helpers.insert(function.name.clone(), ScalarSignature {
                parameters: inputs.iter().cloned().zip(conventions.iter().copied()).collect(),
                result: result.clone(),
            });
        }
        Self { helpers, targets }
    }

    fn target<'a>(&self, helper: &str, arguments: &'a [Expr]) -> Option<&'a str> {
        let signature = self.helpers.get(helper)?;
        let (Expr::Var(target), rest) = arguments.split_first()? else { return None };
        (rest.len() == signature.parameters.len() && self.targets.get(target) == Some(signature))
            .then_some(target.as_str())
    }
}

impl FunctionLowerer<'_> {
    pub(super) fn try_lower_scalar_callback(
        &mut self,
        expression: &Expr,
        helper: &str,
        arguments: &[Expr],
    ) -> Result<Option<ValueId>, CanonicalLoweringError> {
        if !witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::ClosureElide)
            || self.bindings.contains_key(helper) || !self.functions.contains_key(helper)
        {
            return Ok(None);
        }
        let Some(target) = self.forwarded_dict_intrinsics.scalar_callbacks.target(helper, arguments) else {
            return Ok(None);
        };
        if self.bindings.contains_key(target) { return Ok(None) }
        let Some((callee, signature_id)) = self.functions.get(target).copied() else { return Ok(None) };
        let signature = self.signatures[signature_id.get() as usize].clone();
        let [result] = signature.results.as_slice() else { return Ok(None) };
        if signature.parameters.len() != arguments.len() - 1
            || !signature.parameters.iter().copied().all(is_inline_scalar)
            || signature.contract.parameters.iter().any(|parameter| parameter.ownership != ParameterOwnership::Value)
            || scalar_type(&self.expression_type(expression)?)? != *result
        {
            return Ok(None);
        }
        let mut values = Vec::with_capacity(signature.parameters.len());
        for (argument, expected) in arguments[1..].iter().zip(&signature.parameters) {
            let value = self.lower_expr(argument)?;
            if self.values[&value] != *expected {
                return Err(CanonicalLoweringError::UnsupportedExpression("scalar callback argument type"));
            }
            values.push(CallArgument::Value(value));
        }
        let destination = self.fresh(*result)?;
        self.push_operation(Operation::Call { destinations: vec![destination], callee, arguments: values });
        Ok(Some(destination))
    }
}
