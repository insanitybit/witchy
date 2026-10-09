//! The checker's access-flow verifier: propagates the ownership/access
//! contracts of `witchy_canonical_source::access` through a type-checked module
//! and records the finalized facts keyed to that exact AST (RFC-0110 stage 1).

use std::fmt;

// foldhash (not SipHash): all keys are compiler-internal names/ids, never
// attacker-controlled — see witchy-types/src/typeck.rs.
use foldhash::{HashMap, HashMapExt as _};

use witchy_canonical_source::access::*;
use witchy_syntax::ast::{
    BinOp, Block, CallableQualifiers, Convention, Expr, Function, Item, MatchArm, Module, Pattern,
    Stmt, Type, TypeDef, Variant, effective_type_def_params,
};

use crate::typeck::{TypeTable, ty_to_ast};

#[derive(Clone, Debug, PartialEq)]
enum AccessFlow {
    None,
    /// An access component which the active value does not instantiate (for
    /// example the element of `None` or an empty list). It refines to the other
    /// side at an ascription or control-flow join; it is never evidence that a
    /// present callable may erase its contract.
    Unknown,
    /// This control-flow path does not reach the value join. It is neutral
    /// there: it supplies no callable value and therefore cannot weaken the
    /// authenticated contract of a completing alternative.
    Diverges,
    Callable(AccessSignature),
    Product(Vec<AccessFlow>),
    Sequence(Box<AccessFlow>),
    Named { name: String, arguments: Vec<AccessFlow>, dynamic: bool },
}

impl AccessFlow {
    fn has_callable_contract(&self) -> bool {
        match self {
            Self::Callable(_) => true,
            Self::Product(children) => children.iter().any(Self::has_callable_contract),
            Self::Sequence(element) => element.has_callable_contract(),
            Self::Named { arguments, .. } => {
                arguments.iter().any(Self::has_callable_contract)
            }
            Self::None | Self::Unknown | Self::Diverges => false,
        }
    }

    fn product(children: Vec<Self>) -> Self {
        if children.iter().all(|child| matches!(child, Self::None)) {
            Self::None
        } else {
            Self::Product(children)
        }
    }

    fn from_type_with_catalog(
        ty: &Type,
        catalog: &BorrowRelationCatalog,
    ) -> Result<Self, AccessSignatureError> {
        if matches!(ty.unqualified(), Type::Fn(..)) {
            return AccessSignature::from_function_type_with_catalog(ty, catalog)
                .map(Self::Callable);
        }
        match ty.unqualified() {
            Type::Tuple(children) => {
                let children = children
                    .iter()
                    .map(|child| Self::from_type_with_catalog(child, catalog))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Self::product(children))
            }
            Type::Named(name, arguments) => {
                let arguments = arguments
                    .iter()
                    .map(|argument| Self::from_type_with_catalog(argument, catalog))
                    .collect::<Result<Vec<_>, _>>()?;
                if arguments.iter().all(|argument| matches!(argument, Self::None)) {
                    Ok(Self::None)
                } else {
                    Ok(Self::Named { name: name.clone(), arguments, dynamic: false })
                }
            }
            Type::Dyn(name, arguments) => {
                let arguments = arguments
                    .iter()
                    .map(|argument| Self::from_type_with_catalog(argument, catalog))
                    .collect::<Result<Vec<_>, _>>()?;
                if arguments.iter().all(|argument| matches!(argument, Self::None)) {
                    Ok(Self::None)
                } else {
                    Ok(Self::Named { name: name.clone(), arguments, dynamic: true })
                }
            }
            Type::Slice(elem) => {
                let argument = Self::from_type_with_catalog(elem, catalog)?;
                if matches!(argument, Self::None) {
                    Ok(Self::None)
                } else {
                    Ok(Self::Named { name: "Slice".into(), arguments: vec![argument], dynamic: false })
                }
            }
            Type::Qualified(_, _) => unreachable!("unqualified removes every qualifier"),
            Type::RecordCompose { .. } | Type::Fn(..) => Ok(Self::None),
        }
    }

    /// Reapply the access identity carried by this flow to a finalized type
    /// shape. Finalized checker types deliberately erase ownership qualifiers;
    /// inferred aggregate and closure results recover them from the checked
    /// value flow instead of silently publishing the erased shape.
    fn materialize_type(&self, ty: &Type) -> Type {
        match (self, ty.unqualified()) {
            (Self::Callable(signature), Type::Fn(..)) => signature.as_type(),
            (Self::Product(actual), Type::Tuple(expected)) if actual.len() == expected.len() => {
                Type::Tuple(
                    actual
                        .iter()
                        .zip(expected)
                        .map(|(actual, expected)| actual.materialize_type(expected))
                        .collect(),
                )
            }
            (Self::Sequence(actual), Type::Named(name, expected)) if expected.len() == 1 => {
                Type::Named(
                    name.clone(),
                    vec![actual.materialize_type(&expected[0])],
                )
            }
            (
                Self::Named { name: actual_name, arguments: actual, dynamic: false },
                Type::Named(expected_name, expected),
            ) if actual_name == expected_name && actual.len() == expected.len() => Type::Named(
                expected_name.clone(),
                actual
                    .iter()
                    .zip(expected)
                    .map(|(actual, expected)| actual.materialize_type(expected))
                    .collect(),
            ),
            (
                Self::Named { name: actual_name, arguments: actual, dynamic: true },
                Type::Dyn(expected_name, expected),
            ) if actual_name == expected_name && actual.len() == expected.len() => Type::Dyn(
                expected_name.clone(),
                actual
                    .iter()
                    .zip(expected)
                    .map(|(actual, expected)| actual.materialize_type(expected))
                    .collect(),
            ),
            _ => ty.clone(),
        }
    }

    fn verify_exact(&self, expected: &Self, context: &str) -> Result<(), AccessFlowError> {
        if !expected.has_callable_contract() {
            return Ok(());
        }
        match (self, expected) {
            (Self::Unknown, _) => Ok(()),
            (_, Self::None) => Ok(()),
            (Self::Callable(actual), Self::Callable(expected)) => actual
                .verify_exact(expected)
                .map_err(|mismatch| AccessFlowError::mismatch(context, mismatch)),
            (Self::Product(actual), Self::Product(expected))
                if actual.len() == expected.len() =>
            {
                for (actual, expected) in actual.iter().zip(expected) {
                    actual.verify_exact(expected, context)?;
                }
                Ok(())
            }
            (Self::Sequence(actual), Self::Sequence(expected)) => {
                actual.verify_exact(expected, context)
            }
            (
                Self::Sequence(actual),
                Self::Named { arguments, dynamic: false, .. },
            ) if arguments.len() == 1 => {
                actual.verify_exact(&arguments[0], context)
            }
            (
                Self::Named { arguments, dynamic: false, .. },
                Self::Sequence(expected),
            ) if arguments.len() == 1 => arguments[0].verify_exact(expected, context),
            (
                Self::Named { name: actual_name, arguments: actual, dynamic: actual_dynamic },
                Self::Named { name: expected_name, arguments: expected, dynamic: expected_dynamic },
            ) if actual_name == expected_name
                && actual_dynamic == expected_dynamic
                && actual.len() == expected.len() =>
            {
                for (actual, expected) in actual.iter().zip(expected) {
                    actual.verify_exact(expected, context)?;
                }
                Ok(())
            }
            _ => Err(AccessFlowError::missing(context)),
        }
    }

    fn verify_directed(&self, expected: &Self, context: &str) -> Result<(), AccessFlowError> {
        if !expected.has_callable_contract() {
            return Ok(());
        }
        match (self, expected) {
            (Self::Unknown, _) => Ok(()),
            (Self::Callable(actual), Self::Callable(expected)) => actual
                .verify_directed(expected)
                .map_err(|mismatch| AccessFlowError::mismatch(context, mismatch)),
            (
                Self::Named { name: actual_name, arguments: actual, dynamic: true },
                Self::Named { name: expected_name, arguments: expected, dynamic: true },
            ) if actual_name == expected_name && actual.len() == expected.len() => {
                for (actual, expected) in actual.iter().zip(expected) {
                    actual.verify_directed(expected, context)?;
                }
                Ok(())
            }
            // Concrete-to-existential conversion and existential upcasting change
            // nominal identity. Type checking has authenticated the witness or
            // upcast; the target publishes its own access identity.
            (_, Self::Named { dynamic: true, .. }) => Ok(()),
            (Self::Product(actual), Self::Product(expected))
                if actual.len() == expected.len() =>
            {
                for (actual, expected) in actual.iter().zip(expected) {
                    actual.verify_directed(expected, context)?;
                }
                Ok(())
            }
            (Self::Sequence(actual), Self::Sequence(expected)) => {
                actual.verify_directed(expected, context)
            }
            (
                Self::Sequence(actual),
                Self::Named { arguments, dynamic: false, .. },
            ) if arguments.len() == 1 => actual.verify_directed(&arguments[0], context),
            (
                Self::Named { arguments, dynamic: false, .. },
                Self::Sequence(expected),
            ) if arguments.len() == 1 => arguments[0].verify_directed(expected, context),
            (
                Self::Named { name: actual_name, arguments: actual, dynamic: actual_dynamic },
                Self::Named { name: expected_name, arguments: expected, dynamic: expected_dynamic },
            ) if actual_name == expected_name
                && actual_dynamic == expected_dynamic
                && actual.len() == expected.len() =>
            {
                for (actual, expected) in actual.iter().zip(expected) {
                    actual.verify_directed(expected, context)?;
                }
                Ok(())
            }
            _ => self.verify_exact(expected, context),
        }
    }

    fn join(&self, other: &Self, context: &str) -> Result<Self, AccessFlowError> {
        if matches!(self, Self::Diverges) {
            return Ok(other.clone());
        }
        if matches!(other, Self::Diverges) {
            return Ok(self.clone());
        }
        if matches!(self, Self::Unknown) {
            return Ok(other.clone());
        }
        if matches!(other, Self::Unknown) {
            return Ok(self.clone());
        }
        if let (Self::Callable(left), Self::Callable(right)) = (self, other) {
            return left
                .join(right)
                .map(Self::Callable)
                .map_err(|mismatch| AccessFlowError::mismatch(context, mismatch));
        }
        if let (
            Self::Named { name: left_name, arguments: left, dynamic: left_dynamic },
            Self::Named { name: right_name, arguments: right, dynamic: right_dynamic },
        ) = (self, other)
            && left_name == right_name
            && left_dynamic == right_dynamic
            && left.len() == right.len()
        {
            return Ok(Self::Named {
                name: left_name.clone(),
                arguments: left
                    .iter()
                    .zip(right)
                    .map(|(left, right)| left.join(right, context))
                    .collect::<Result<Vec<_>, _>>()?,
                dynamic: *left_dynamic,
            });
        }
        self.verify_exact(other, context)?;
        other.verify_exact(self, context)?;
        Ok(self.clone())
    }

}

/// Finalized access-signature facts keyed to the exact typed AST used to build
/// them. The owning [`crate::typeck::TypedModule`] must remain alive while these
/// address-keyed queries are used.
#[derive(Debug, PartialEq)]
pub struct CheckedAccessFacts {
    owner: *const Module,
    values: HashMap<usize, AccessFlow>,
    declarations: HashMap<String, AccessSignature>,
    calls: HashMap<usize, AccessSignature>,
    places: CheckedPlaceFacts,
}

impl witchy_canonical_source::source::CheckedAccessQuery for CheckedAccessFacts {
    fn declaration(&self, name: &str) -> Option<&AccessSignature> {
        CheckedAccessFacts::declaration(self, name)
    }

    fn call_at(&self, module: &Module, expression: &Expr) -> Option<&AccessSignature> {
        CheckedAccessFacts::call_at(self, module, expression)
    }
}

impl CheckedAccessFacts {
    fn owns(&self, module: &Module) -> bool {
        std::ptr::eq(self.owner, module)
    }

    pub fn callable_at(&self, module: &Module, expression: &Expr) -> Option<&AccessSignature> {
        if !self.owns(module) {
            return None;
        }
        match self.values.get(&(expression as *const Expr as usize)) {
            Some(AccessFlow::Callable(signature)) => Some(signature),
            _ => None,
        }
    }

    /// The finalized access identity of a checked direct declaration.
    pub fn declaration(&self, name: &str) -> Option<&AccessSignature> {
        self.declarations.get(name)
    }

    /// The finalized direct, indirect, or existential callable selected at one
    /// exact checked call expression.
    pub fn call_at(&self, module: &Module, expression: &Expr) -> Option<&AccessSignature> {
        if !self.owns(module) {
            return None;
        }
        self.calls.get(&(expression as *const Expr as usize))
    }

    /// Every direct, indirect, closure, or witness call contract recorded for
    /// this exact checked module.  Backend-owned adapter selection consumes
    /// this finalized table instead of rebuilding callable effects from the
    /// spelling of a call expression.
    pub fn call_contracts(&self) -> impl Iterator<Item = (usize, &AccessSignature)> {
        self.calls.iter().map(|(site, signature)| (*site, signature))
    }

    /// The assignable root and fixed/dynamic projection path authenticated for
    /// this exact checked expression.
    pub fn place_at(&self, module: &Module, expression: &Expr) -> Option<&CheckedPlace> {
        if !self.owns(module) {
            return None;
        }
        self.places.place_at(module, expression)
    }
}

#[derive(Clone, Debug)]
pub struct AccessFlowError {
    message: String,
}

impl AccessFlowError {
    fn mismatch(context: &str, mismatch: AccessMismatch) -> Self {
        Self {
            message: format!(
                "{context} erases or changes its ownership/access contract ({mismatch})"
            ),
        }
    }

    fn missing(context: &str) -> Self {
        Self {
            message: format!(
                "{context} loses a checked callable ownership/access contract"
            ),
        }
    }
}

impl fmt::Display for AccessFlowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AccessFlowError {}

type FlowEnvironment = HashMap<String, AccessFlow>;

#[derive(Clone, Copy)]
struct CallTypeContext<'a> {
    arguments: &'a [Option<Type>],
    result: Option<&'a Type>,
}

struct AccessVerifier<'a> {
    module: &'a Module,
    table: &'a TypeTable,
    // Borrowed from `module`, which outlives the verifier: the verifier runs
    // once per annotate pass, several times per compile, and copying every
    // declaration of the linked program (std included) each time was a
    // measurable share of every compile.
    functions: HashMap<&'a str, &'a Function>,
    types: HashMap<&'a str, &'a TypeDef>,
    variants: HashMap<&'a str, Vec<(&'a str, &'a Variant)>>,
    borrow_catalog: BorrowRelationCatalog,
    facts: CheckedAccessFacts,
    return_frames: Vec<Vec<AccessFlow>>,
    expression_type_hints: HashMap<usize, Type>,
}

impl<'a> AccessVerifier<'a> {
    fn new(module: &'a Module, table: &'a TypeTable) -> Self {
        let borrow_catalog = BorrowRelationCatalog::from_module(module);
        let functions = module
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Function(function) => Some((function.name.as_str(), function)),
                _ => None,
            })
            .collect();
        let types = module
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Type(definition) => Some((definition.name.as_str(), definition)),
                _ => None,
            })
            .collect::<HashMap<_, _>>();
        let mut variants = HashMap::<&'a str, Vec<(&'a str, &'a Variant)>>::new();
        for definition in types.values() {
            for variant in &definition.variants {
                variants
                    .entry(variant.name.as_str())
                    .or_default()
                    .push((definition.name.as_str(), variant));
            }
        }
        Self {
            module,
            table,
            functions,
            types,
            variants,
            borrow_catalog,
            facts: CheckedAccessFacts {
                owner: std::ptr::from_ref(module),
                values: HashMap::new(),
                declarations: HashMap::new(),
                calls: HashMap::new(),
                places: checked_place_facts(module),
            },
            return_frames: Vec::new(),
            expression_type_hints: HashMap::new(),
        }
    }

    fn verify(mut self) -> Result<CheckedAccessFacts, AccessFlowError> {
        for item in &self.module.items {
            let Item::Function(function) = item else { continue };
            self.verify_function(function)?;
        }
        Ok(self.facts)
    }

    fn flow_from_type(&self, ty: &Type) -> Result<AccessFlow, AccessSignatureError> {
        AccessFlow::from_type_with_catalog(ty, &self.borrow_catalog)
    }

    fn resolved_expression_type(&self, expression: &Expr) -> Option<Type> {
        self.table.type_of(expression).and_then(ty_to_ast)
    }

    fn contextual_expression_type(&self, expression: &Expr) -> Option<Type> {
        self.expression_type_hints
            .get(&(expression as *const Expr as usize))
            .cloned()
            .or_else(|| self.resolved_expression_type(expression))
    }

    fn declaration_signature(
        &self,
        function: &Function,
    ) -> Result<Option<AccessSignature>, AccessFlowError> {
        self.declaration_signature_at(function, None)
    }

    /// The declaration's contract, at `conventions` when an owning adapter
    /// makes it fill a slot with other conventions than its own.
    fn declaration_signature_at(
        &self,
        function: &Function,
        conventions: Option<&[Convention]>,
    ) -> Result<Option<AccessSignature>, AccessFlowError> {
        let resolved = self.table.function_type(&function.name);
        let signature = match (resolved, conventions) {
            (Some(resolved), None) => AccessSignature::from_resolved_function_with_catalog(
                function,
                &resolved,
                &self.borrow_catalog,
            ),
            (Some(resolved), Some(conventions)) => AccessSignature::from_resolved_parts_with_catalog(
                function.params.iter().map(|param| param.ty.as_ref()).collect(),
                function.ret.as_ref(),
                conventions.to_vec(),
                &resolved,
                &self.borrow_catalog,
            ),
            (None, None) => AccessSignature::from_function_with_catalog(
                function,
                &self.borrow_catalog,
            ),
            (None, Some(conventions)) => AccessSignature::from_function_at_conventions_with_catalog(
                function,
                conventions.to_vec(),
                &self.borrow_catalog,
            ),
        };
        match signature {
            Ok(signature) => Ok(Some(signature)),
            Err(
                AccessSignatureError::MissingParameterType { .. }
                | AccessSignatureError::MissingResultType,
            ) => Ok(None),
            Err(error) => Err(AccessFlowError {
                message: format!("access signature for `{}` is invalid: {error}", function.name),
            }),
        }
    }

    fn verify_function(&mut self, function: &Function) -> Result<(), AccessFlowError> {
        let Some(signature) = self.declaration_signature(function)? else {
            return Ok(());
        };
        self.facts
            .declarations
            .insert(function.name.clone(), signature.clone());
        let mut environment = HashMap::new();
        for (parameter, access) in function.params.iter().zip(signature.params()) {
            environment.insert(
                parameter.name.clone(),
                self.flow_from_type(access.ty()).map_err(Self::signature_error)?,
            );
        }
        let expected = self.flow_from_type(signature.result().ty())
            .map_err(Self::signature_error)?;
        // Generic entry matches retain the checked parameter declaration when the expression type is unresolved.
        let parameter_match = match function.body.stmts.as_slice() {
            [Stmt::Expr(Expr::Match { scrutinee, .. })] => match scrutinee.as_ref() {
                Expr::Var(name) => function.params.iter().find(|parameter| parameter.name == *name)
                    .and_then(|parameter| parameter.ty.as_ref())
                    .map(|ty| (scrutinee.as_ref() as *const Expr as usize, ty.clone())),
                _ => None,
            },
            _ => None,
        };
        if let Some((site, ty)) = &parameter_match {
            self.expression_type_hints.insert(*site, ty.clone());
        }
        let tail = self.eval_block(&function.body, &mut environment, &expected);
        if let Some((site, _)) = parameter_match { self.expression_type_hints.remove(&site); }
        let tail = tail?;
        if matches!(function.body.stmts.last(), Some(Stmt::Expr(_))) {
            let context = if matches!(&expected, AccessFlow::Callable(_)) {
                "returned function value".to_string()
            } else {
                format!("returned value from `{}`", function.name)
            };
            tail.verify_directed(
                &expected,
                &context,
            )?;
        }
        Ok(())
    }

    fn signature_error(error: AccessSignatureError) -> AccessFlowError {
        AccessFlowError { message: error.to_string() }
    }

    fn signature_for_named_call(
        &self,
        name: &str,
        expression: &Expr,
        args: &[&Expr],
    ) -> Result<Option<AccessSignature>, AccessFlowError> {
        let Some(function) = self.functions.get(name) else {
            // A callee std pruning removed would skip its conventions here
            // (a `unique` parameter is what rejects an aliased owner): fail,
            // so the route reruns against the full standard library.
            if crate::std_pruning::removed(name) {
                return Err(AccessFlowError {
                    message: format!("std pruning removed `{name}`, which this call needs"),
                });
            }
            return Ok(None);
        };
        if function.params.len() != args.len() {
            return Ok(None);
        }
        match AccessSignature::from_function_with_catalog(function, &self.borrow_catalog) {
            Ok(signature) => return Ok(Some(signature)),
            Err(
                AccessSignatureError::MissingParameterType { .. }
                | AccessSignatureError::MissingResultType,
            ) => {}
            Err(error) => return Err(Self::signature_error(error)),
        }
        let Some(result) = self.resolved_expression_type(expression) else {
            return self.declaration_signature(function);
        };
        let params = args
            .iter()
            .map(|argument| self.resolved_expression_type(argument))
            .collect::<Option<Vec<_>>>();
        let Some(params) = params else {
            return self.declaration_signature(function);
        };
        let conventions = function.params.iter().map(|parameter| parameter.convention).collect();
        let resolved = Type::Fn(
            params,
            Box::new(result),
            conventions,
            CallableQualifiers::new(function.pure, false),
        );
        AccessSignature::from_resolved_function_with_catalog(
            function,
            &resolved,
            &self.borrow_catalog,
        )
            .map(Some)
            .map_err(Self::signature_error)
    }

    fn signature_for_function_value(
        &self,
        name: &str,
        expression: &Expr,
    ) -> Result<Option<AccessSignature>, AccessFlowError> {
        let Some(function) = self.functions.get(name) else { return Ok(None) };
        let adapter = self.table.let_callable_adapter(expression);
        let Some(resolved) = self.resolved_expression_type(expression) else {
            // A generic body leaves the use unresolved; an adapted use still
            // carries the slot's conventions over the declaration's types.
            return self.declaration_signature_at(function, adapter);
        };
        let Type::Fn(params, _, _, _) = resolved.unqualified() else { return Ok(None) };
        if params.len() != function.params.len() {
            return Ok(None);
        }
        // A value the checker accepted through the owning adapter has the
        // adapter's contract: the slot's conventions, which the rewrite in
        // `crate::let_callable_adapter` gives the lambda that replaces it.
        if let Some(conventions) = adapter {
            return AccessSignature::from_resolved_parts_with_catalog(
                function.params.iter().map(|param| param.ty.as_ref()).collect(),
                function.ret.as_ref(),
                conventions.to_vec(),
                &resolved,
                &self.borrow_catalog,
            )
                .map(Some)
                .map_err(Self::signature_error);
        }
        AccessSignature::from_resolved_function_with_catalog(
            function,
            &resolved,
            &self.borrow_catalog,
        )
            .map(Some)
            .map_err(Self::signature_error)
    }

    fn nominal_arguments<'b>(flow: &'b AccessFlow, name: &str) -> Option<&'b [AccessFlow]> {
        match flow {
            AccessFlow::Named { name: found, arguments, dynamic: false } if found == name => {
                Some(arguments)
            }
            _ => None,
        }
    }

    fn substitute_type(ty: &Type, substitutions: &HashMap<String, Type>) -> Type {
        match ty {
            Type::Named(name, arguments) if arguments.is_empty() => substitutions
                .get(name)
                .cloned()
                .unwrap_or_else(|| ty.clone()),
            Type::Named(name, arguments) => Type::Named(
                name.clone(),
                arguments
                    .iter()
                    .map(|argument| Self::substitute_type(argument, substitutions))
                    .collect(),
            ),
            Type::Dyn(name, arguments) => Type::Dyn(
                name.clone(),
                arguments
                    .iter()
                    .map(|argument| Self::substitute_type(argument, substitutions))
                    .collect(),
            ),
            Type::Tuple(fields) => Type::Tuple(
                fields
                    .iter()
                    .map(|field| Self::substitute_type(field, substitutions))
                    .collect(),
            ),
            Type::Fn(params, result, conventions, qualifiers) => Type::Fn(
                params
                    .iter()
                    .map(|param| Self::substitute_type(param, substitutions))
                    .collect(),
                Box::new(Self::substitute_type(result, substitutions)),
                conventions.clone(),
                *qualifiers,
            ),
            Type::Qualified(qualifier, inner) => Type::Qualified(
                qualifier.clone(),
                Box::new(Self::substitute_type(inner, substitutions)),
            ),
            Type::Slice(elem) => Type::Slice(Box::new(Self::substitute_type(elem, substitutions))),
            Type::RecordCompose { base, fields } => Type::RecordCompose {
                base: Box::new(Self::substitute_type(base, substitutions)),
                fields: fields
                    .iter()
                    .map(|(name, field)| {
                        (name.clone(), Self::substitute_type(field, substitutions))
                    })
                    .collect(),
            },
        }
    }

    fn nominal_substitutions(&self, name: &str, ty: &Type) -> HashMap<String, Type> {
        let Some(definition) = self.types.get(name) else { return HashMap::new() };
        let arguments = match ty.unqualified() {
            Type::Named(found, arguments) if found == name => arguments,
            _ => return HashMap::new(),
        };
        effective_type_def_params(definition)
            .into_iter()
            .zip(arguments.iter().cloned())
            .collect()
    }

    fn flow_for_declared_type(
        &self,
        declared: &Type,
        substitutions: &HashMap<String, Type>,
        access_arguments: &HashMap<String, AccessFlow>,
    ) -> Result<AccessFlow, AccessFlowError> {
        if let Type::Named(name, arguments) = declared
            && arguments.is_empty()
            && let Some(flow) = access_arguments.get(name)
        {
            return Ok(flow.clone());
        }
        let resolved = Self::substitute_type(declared, substitutions);
        self.flow_from_type(&resolved).map_err(Self::signature_error)
    }

    fn nominal_access_arguments(
        &self,
        name: &str,
        flow: &AccessFlow,
    ) -> HashMap<String, AccessFlow> {
        let Some(definition) = self.types.get(name) else { return HashMap::new() };
        let Some(arguments) = Self::nominal_arguments(flow, name) else {
            return HashMap::new();
        };
        effective_type_def_params(definition)
            .into_iter()
            .zip(arguments.iter().cloned())
            .collect()
    }

    fn constrain_declared_flow(
        &self,
        declared: &Type,
        actual: &AccessFlow,
        substitutions: &HashMap<String, Type>,
        access_arguments: &mut HashMap<String, AccessFlow>,
        context: &str,
    ) -> Result<(), AccessFlowError> {
        if let Type::Named(name, arguments) = declared
            && arguments.is_empty()
            && substitutions.contains_key(name)
        {
            let entry = access_arguments.entry(name.clone()).or_insert(AccessFlow::Unknown);
            *entry = entry.join(actual, context)?;
            return Ok(());
        }
        if let Type::Named(name, declared_arguments) = declared {
            let actual_arguments = match actual {
                AccessFlow::Named {
                    name: actual_name,
                    arguments,
                    dynamic: false,
                } if actual_name == name => Some(arguments.as_slice()),
                AccessFlow::Sequence(element) if declared_arguments.len() == 1 => {
                    Some(std::slice::from_ref(element.as_ref()))
                }
                _ => None,
            };
            if let Some(actual_arguments) = actual_arguments
                && actual_arguments.len() == declared_arguments.len()
            {
                for (declared, actual) in declared_arguments.iter().zip(actual_arguments) {
                    self.constrain_declared_flow(
                        declared,
                        actual,
                        substitutions,
                        access_arguments,
                        context,
                    )?;
                }
                return Ok(());
            }
        }
        if let Type::Tuple(declared_fields) = declared
            && let AccessFlow::Product(actual_fields) = actual
            && declared_fields.len() == actual_fields.len()
        {
            for (declared, actual) in declared_fields.iter().zip(actual_fields) {
                self.constrain_declared_flow(
                    declared,
                    actual,
                    substitutions,
                    access_arguments,
                    context,
                )?;
            }
            return Ok(());
        }
        let expected = self.flow_for_declared_type(declared, substitutions, access_arguments)?;
        actual.verify_directed(&expected, context)
    }

    fn variant_for(&self, constructor: &str, value_type: &Type) -> Option<&(&'a str, &'a Variant)> {
        let candidates = self.variants.get(constructor)?;
        let owner = match value_type.unqualified() {
            Type::Named(name, _) => Some(name),
            _ => None,
        };
        owner
            .and_then(|owner| candidates.iter().find(|(name, _)| *name == owner.as_str()))
            .or_else(|| (candidates.len() == 1).then(|| &candidates[0]))
    }

    fn builtin_constructor_flow(
        constructor: &str,
        expression_type: &Type,
        arguments: &[AccessFlow],
    ) -> Option<AccessFlow> {
        let Type::Named(type_name, type_arguments) = expression_type.unqualified() else {
            return None;
        };
        let constructor = constructor.rsplit('.').next().unwrap_or(constructor);
        let type_leaf = type_name.rsplit('.').next().unwrap_or(type_name);
        let access_arguments = match (type_leaf, constructor, type_arguments.len(), arguments) {
            ("Option", "Some", 1, [value]) => vec![value.clone()],
            ("Option", "None", 1, []) => vec![AccessFlow::Unknown],
            ("Result", "Ok", 2, [value]) => vec![value.clone(), AccessFlow::Unknown],
            ("Result", "Err", 2, [error]) => vec![AccessFlow::Unknown, error.clone()],
            _ => return None,
        };
        Some(AccessFlow::Named {
            name: type_name.clone(),
            arguments: access_arguments,
            dynamic: false,
        })
    }

    fn builtin_pattern_fields(
        constructor: &str,
        value: &AccessFlow,
        value_type: Option<&Type>,
    ) -> Option<Vec<(AccessFlow, Option<Type>)>> {
        let AccessFlow::Named {
            name: type_name,
            arguments: access_arguments,
            dynamic: false,
        } = value
        else {
            return None;
        };
        let constructor = constructor.rsplit('.').next().unwrap_or(constructor);
        let type_leaf = type_name.rsplit('.').next().unwrap_or(type_name);
        let type_arguments = value_type.and_then(|value_type| match value_type.unqualified() {
            Type::Named(name, arguments) if name == type_name => Some(arguments.as_slice()),
            _ => None,
        });
        let field_indexes: &[usize] = match (type_leaf, constructor, access_arguments.len()) {
            ("Option", "Some", 1) => &[0],
            ("Option", "None", 1) => &[],
            ("Result", "Ok", 2) => &[0],
            ("Result", "Err", 2) => &[1],
            _ => return None,
        };
        Some(
            field_indexes
                .iter()
                .map(|&index| {
                    let ty = type_arguments
                        .and_then(|arguments| arguments.get(index))
                        .cloned();
                    let flow = access_arguments
                        .get(index)
                        .cloned()
                        .unwrap_or(AccessFlow::Unknown);
                    (flow, ty)
                })
                .collect(),
        )
    }

    fn constructor_flow(
        &self,
        constructor: &str,
        expression_type: &Type,
        arguments: &[AccessFlow],
    ) -> Result<AccessFlow, AccessFlowError> {
        let Some((type_name, variant)) = self.variant_for(constructor, expression_type) else {
            if let Some(flow) =
                Self::builtin_constructor_flow(constructor, expression_type, arguments)
            {
                return Ok(flow);
            }
            return self.flow_from_type(expression_type).map_err(Self::signature_error);
        };
        let substitutions = self.nominal_substitutions(type_name, expression_type);
        let mut access_arguments = effective_type_def_params(self.types[type_name])
            .into_iter()
            .map(|name| (name, AccessFlow::Unknown))
            .collect::<HashMap<_, _>>();
        for (index, (declared, actual)) in variant.fields.iter().zip(arguments).enumerate() {
            self.constrain_declared_flow(
                declared,
                actual,
                &substitutions,
                &mut access_arguments,
                &format!("field {} of constructor `{constructor}`", index + 1),
            )?;
        }
        let arguments = effective_type_def_params(self.types[type_name])
            .into_iter()
            .map(|name| access_arguments.remove(&name).unwrap_or(AccessFlow::Unknown))
            .collect();
        Ok(AccessFlow::Named {
            name: (*type_name).to_owned(),
            arguments,
            dynamic: false,
        })
    }

    fn field_flow(
        &self,
        base_type: &Type,
        base_flow: &AccessFlow,
        field: &str,
    ) -> Result<AccessFlow, AccessFlowError> {
        let Type::Named(name, _) = base_type.unqualified() else { return Ok(AccessFlow::None) };
        let Some(definition) = self.types.get(name.as_str()) else { return Ok(AccessFlow::None) };
        let Some((variant, index)) = definition
            .variants
            .iter()
            .find_map(|variant| {
                variant
                    .field_names
                    .iter()
                    .position(|found| found == field)
                    .map(|index| (variant, index))
            })
        else {
            return Ok(AccessFlow::None);
        };
        let substitutions = self.nominal_substitutions(name, base_type);
        let access_arguments = self.nominal_access_arguments(name, base_flow);
        self.flow_for_declared_type(&variant.fields[index], &substitutions, &access_arguments)
    }

    fn record_flow(
        &mut self,
        name: &str,
        fields: &[(String, Expr)],
        spread: Option<&Expr>,
        expression_type: &Type,
        environment: &mut FlowEnvironment,
        return_expected: &AccessFlow,
    ) -> Result<AccessFlow, AccessFlowError> {
        let spread_flow = spread
            .map(|spread| self.eval_expr(spread, environment, return_expected))
            .transpose()?;
        let Type::Named(type_name, _) = expression_type.unqualified() else {
            for (_, value) in fields {
                self.eval_expr(value, environment, return_expected)?;
            }
            return self.flow_from_type(expression_type).map_err(Self::signature_error);
        };
        let Some(definition) = self.types.get(type_name.as_str()).copied() else {
            for (_, value) in fields {
                self.eval_expr(value, environment, return_expected)?;
            }
            return self.flow_from_type(expression_type).map_err(Self::signature_error);
        };
        let substitutions = self.nominal_substitutions(type_name, expression_type);
        let mut access_arguments = spread_flow
            .as_ref()
            .map(|flow| self.nominal_access_arguments(type_name, flow))
            .unwrap_or_default();
        for parameter in effective_type_def_params(&definition) {
            access_arguments.entry(parameter).or_insert(AccessFlow::Unknown);
        }
        for (field, value) in fields {
            let actual = self.eval_expr(value, environment, return_expected)?;
            let Some(declared) = definition.variants.iter().find_map(|variant| {
                variant
                    .field_names
                    .iter()
                    .position(|found| found == field)
                    .and_then(|index| variant.fields.get(index))
            }) else {
                continue;
            };
            self.constrain_declared_flow(
                declared,
                &actual,
                &substitutions,
                &mut access_arguments,
                &format!("record field `{field}` of `{name}`"),
            )?;
        }
        let arguments = effective_type_def_params(&definition)
            .into_iter()
            .map(|parameter| {
                access_arguments.remove(&parameter).unwrap_or(AccessFlow::Unknown)
            })
            .collect();
        Ok(AccessFlow::Named {
            name: type_name.clone(),
            arguments,
            dynamic: false,
        })
    }

    fn existential_signature(
        &self,
        receiver: &Type,
        params: &[Type],
        checked_result: &Type,
        conventions: &[Convention],
    ) -> Result<AccessSignature, AccessFlowError> {
        let mut params = params.to_vec();
        params.insert(0, receiver.clone());
        AccessSignature::from_parts_with_catalog(
            params,
            checked_result.clone(),
            conventions.to_vec(),
            &self.borrow_catalog,
        )
        .map_err(Self::signature_error)
    }

    fn verify_arguments(
        &self,
        name: &str,
        signature: &AccessSignature,
        arguments: &[AccessFlow],
    ) -> Result<(), AccessFlowError> {
        for (index, (argument, parameter)) in
            arguments.iter().zip(signature.params()).enumerate()
        {
            let expected = self.flow_from_type(parameter.ty()).map_err(Self::signature_error)?;
            // This reserved service site invokes its checked declaration with
            // copied owned Bytes; it does not coerce a function value or erase
            // the declaration's conventions. All other boundaries stay exact.
            if name == "vm.serve" && arguments.len() == 3 && index == 2 {
                if let (AccessFlow::Callable(actual), AccessFlow::Callable(expected)) = (argument, &expected) {
                    if crate::loans::vm_service_copy_callback_matches(expected, actual) {
                        continue;
                    }
                }
            }
            argument.verify_directed(
                &expected,
                &format!("argument {} passed to `{name}`", index + 1),
            )?;
        }
        Ok(())
    }

    fn collect_type_substitutions(
        declared: &Type,
        actual: &Type,
        substitutions: &mut HashMap<String, Type>,
    ) {
        if let Type::Qualified(_, inner) = declared {
            Self::collect_type_substitutions(inner, actual.unqualified(), substitutions);
            return;
        }
        if let Type::Named(name, arguments) = declared
            && arguments.is_empty()
            && !name.contains('.')
            && name.chars().next().is_some_and(char::is_lowercase)
        {
            substitutions.entry(name.clone()).or_insert_with(|| actual.clone());
            return;
        }
        match (declared.unqualified(), actual.unqualified()) {
            (Type::Named(declared_name, declared), Type::Named(actual_name, actual))
            | (Type::Dyn(declared_name, declared), Type::Dyn(actual_name, actual))
                if declared_name == actual_name && declared.len() == actual.len() =>
            {
                for (declared, actual) in declared.iter().zip(actual) {
                    Self::collect_type_substitutions(declared, actual, substitutions);
                }
            }
            (Type::Tuple(declared), Type::Tuple(actual)) if declared.len() == actual.len() => {
                for (declared, actual) in declared.iter().zip(actual) {
                    Self::collect_type_substitutions(declared, actual, substitutions);
                }
            }
            (Type::Fn(declared_params, declared_result, _, _), Type::Fn(actual_params, actual_result, _, _))
                if declared_params.len() == actual_params.len() =>
            {
                for (declared, actual) in declared_params.iter().zip(actual_params) {
                    Self::collect_type_substitutions(declared, actual, substitutions);
                }
                Self::collect_type_substitutions(declared_result, actual_result, substitutions);
            }
            _ => {}
        }
    }

    fn collect_flow_substitutions(
        declared: &Type,
        actual: &AccessFlow,
        substitutions: &mut HashMap<String, Type>,
    ) {
        if let Type::Named(name, arguments) = declared
            && arguments.is_empty()
            && !name.contains('.')
            && name.chars().next().is_some_and(char::is_lowercase)
            && let AccessFlow::Callable(signature) = actual
        {
            substitutions.entry(name.clone()).or_insert_with(|| signature.as_type());
            return;
        }
        match (declared.unqualified(), actual) {
            (Type::Fn(..), AccessFlow::Callable(signature)) => {
                Self::collect_type_substitutions(declared, &signature.as_type(), substitutions);
            }
            (Type::Tuple(declared), AccessFlow::Product(actual))
                if declared.len() == actual.len() =>
            {
                for (declared, actual) in declared.iter().zip(actual) {
                    Self::collect_flow_substitutions(declared, actual, substitutions);
                }
            }
            (Type::Named(_, declared), AccessFlow::Sequence(actual)) if declared.len() == 1 => {
                Self::collect_flow_substitutions(&declared[0], actual, substitutions);
            }
            (
                Type::Named(declared_name, declared),
                AccessFlow::Named {
                    name: actual_name,
                    arguments: actual,
                    dynamic: false,
                },
            ) if declared_name == actual_name && declared.len() == actual.len() => {
                for (declared, actual) in declared.iter().zip(actual) {
                    Self::collect_flow_substitutions(declared, actual, substitutions);
                }
            }
            (
                Type::Dyn(declared_name, declared),
                AccessFlow::Named {
                    name: actual_name,
                    arguments: actual,
                    dynamic: true,
                },
            ) if declared_name == actual_name && declared.len() == actual.len() => {
                for (declared, actual) in declared.iter().zip(actual) {
                    Self::collect_flow_substitutions(declared, actual, substitutions);
                }
            }
            _ => {}
        }
    }

    fn specialize_signature_from_flows(
        &self,
        signature: AccessSignature,
        arguments: &[AccessFlow],
        argument_types: &[Option<Type>],
        result_type: Option<&Type>,
    ) -> Result<(AccessSignature, HashMap<String, Type>), AccessFlowError> {
        let mut substitutions = HashMap::new();
        for (index, parameter) in signature.params().iter().enumerate() {
            if let Some(argument) = arguments.get(index) {
                // A checked value flow retains callable qualifiers that the
                // finalized expression type intentionally erases. Let that
                // access identity bind the callee variable first, then use the
                // checked type only to fill variables the flow cannot represent.
                Self::collect_flow_substitutions(
                    parameter.ty(),
                    argument,
                    &mut substitutions,
                );
                if let Some(actual_type) = argument_types.get(index).and_then(Option::as_ref) {
                    let actual_type = argument.materialize_type(actual_type);
                    Self::collect_type_substitutions(
                        parameter.ty(),
                        &actual_type,
                        &mut substitutions,
                    );
                }
            } else if let Some(actual_type) = argument_types.get(index).and_then(Option::as_ref) {
                Self::collect_type_substitutions(
                    parameter.ty(),
                    actual_type,
                    &mut substitutions,
                );
            }
        }
        if let Some(result_type) = result_type {
            Self::collect_type_substitutions(
                signature.result().ty(),
                result_type,
                &mut substitutions,
            );
        }
        if substitutions.is_empty() {
            return Ok((signature, substitutions));
        }
        let specialized = Self::substitute_type(&signature.as_type(), &substitutions);
        let signature = AccessSignature::from_function_type_with_catalog(
            &specialized,
            &self.borrow_catalog,
        )
        .map_err(Self::signature_error)?;
        Ok((signature, substitutions))
    }

    fn eval_arguments_with_dependent_hints(
        &mut self,
        expressions: &[&Expr],
        signature: Option<&AccessSignature>,
        prefix: &[AccessFlow],
        types: CallTypeContext<'_>,
        environment: &mut FlowEnvironment,
        return_expected: &AccessFlow,
    ) -> Result<Vec<AccessFlow>, AccessFlowError> {
        let mut evaluated = prefix.to_vec();
        let mut arguments = Vec::with_capacity(expressions.len());
        for expression in expressions {
            let parameter_index = evaluated.len();
            let hint = signature
                .cloned()
                .map(|signature| {
                    self.specialize_signature_from_flows(
                        signature,
                        &evaluated,
                        types.arguments,
                        types.result,
                    )
                    .map(|(signature, _)| signature)
                })
                .transpose()?;
            let argument = self.eval_expr_with_hint(
                expression,
                hint.as_ref()
                    .and_then(|signature| signature.params().get(parameter_index))
                    .map(AccessParam::ty),
                environment,
                return_expected,
            )?;
            evaluated.push(argument.clone());
            arguments.push(argument);
        }
        Ok(arguments)
    }

    fn eval_block(
        &mut self,
        block: &Block,
        environment: &mut FlowEnvironment,
        return_expected: &AccessFlow,
    ) -> Result<AccessFlow, AccessFlowError> {
        let mut tail = AccessFlow::None;
        let return_hint = match return_expected {
            AccessFlow::Callable(signature) => Some(signature.as_type()),
            _ => None,
        };
        for (index, statement) in block.stmts.iter().enumerate() {
            tail = match statement {
                Stmt::Let { name, ty, value, .. } => {
                    let actual =
                        self.eval_expr_with_hint(value, ty.as_ref(), environment, return_expected)?;
                    let value = if let Some(ty) = ty {
                        let expected = self.flow_from_type(ty).map_err(Self::signature_error)?;
                        actual.verify_directed(&expected, &format!("function value `{name}`"))?;
                        expected
                    } else {
                        actual
                    };
                    environment.insert(name.clone(), value);
                    AccessFlow::None
                }
                Stmt::Assign { name, value, .. } => {
                    let hint = match environment.get(name) {
                        Some(AccessFlow::Callable(signature)) => Some(signature.as_type()),
                        _ => None,
                    };
                    let actual = self.eval_expr_with_hint(
                        value,
                        hint.as_ref(),
                        environment,
                        return_expected,
                    )?;
                    if let Some(expected) = environment.get(name) {
                        actual.verify_directed(expected, &format!("assignment to `{name}`"))?;
                    }
                    environment.insert(name.clone(), actual);
                    AccessFlow::None
                }
                Stmt::LetPattern { pattern, value } => {
                    let value_type = self.resolved_expression_type(value);
                    let value = self.eval_expr(value, environment, return_expected)?;
                    self.bind_pattern(pattern, &value, value_type.as_ref(), environment)?;
                    AccessFlow::None
                }
                Stmt::Return(value) => {
                    let actual = match value {
                        Some(value) => self.eval_expr_with_hint(
                            value,
                            return_hint.as_ref(),
                            environment,
                            return_expected,
                        )?,
                        None => AccessFlow::None,
                    };
                    if let Some(frame) = self.return_frames.last_mut() {
                        frame.push(actual.clone());
                    }
                    actual.verify_directed(return_expected, "returned function value")?;
                    return Ok(AccessFlow::Diverges);
                }
                Stmt::Yield(value) => self.eval_expr(value, environment, return_expected)?,
                Stmt::Expr(value) => self.eval_expr_with_hint(
                    value,
                    (index + 1 == block.stmts.len())
                        .then_some(return_hint.as_ref())
                        .flatten(),
                    environment,
                    return_expected,
                )?,
                Stmt::Break | Stmt::Continue => return Ok(AccessFlow::Diverges),
            };
            if index + 1 != block.stmts.len() {
                tail = AccessFlow::None;
            }
        }
        Ok(tail)
    }

    fn bind_pattern(
        &self,
        pattern: &Pattern,
        value: &AccessFlow,
        value_type: Option<&Type>,
        environment: &mut FlowEnvironment,
    ) -> Result<(), AccessFlowError> {
        match pattern {
            Pattern::Var(name) if name != "_" => {
                environment.insert(name.clone(), value.clone());
            }
            Pattern::Tuple(args) => {
                let field_types = value_type.and_then(|ty| match ty.unqualified() {
                    Type::Tuple(fields) => Some(fields.as_slice()),
                    _ => None,
                });
                for (index, pattern) in args.iter().enumerate() {
                    let flow = match value {
                        AccessFlow::Product(children) => {
                            children.get(index).cloned().unwrap_or(AccessFlow::None)
                        }
                        _ => field_types
                            .and_then(|fields| fields.get(index))
                            .map(|field| self.flow_from_type(field))
                            .transpose()
                            .map_err(Self::signature_error)?
                            .unwrap_or(AccessFlow::None),
                    };
                    self.bind_pattern(
                        pattern,
                        &flow,
                        field_types.and_then(|fields| fields.get(index)),
                        environment,
                    )?;
                }
            }
            Pattern::Ctor { name: constructor, args }
            | Pattern::AnonCtor { tag: constructor, args } => {
                if let Some(value_type) = value_type
                    && let Some((type_name, variant)) = self.variant_for(constructor, value_type)
                {
                    let substitutions = self.nominal_substitutions(type_name, value_type);
                    let access_arguments = self.nominal_access_arguments(type_name, value);
                    for (index, pattern) in args.iter().enumerate() {
                        let Some(declared) = variant.fields.get(index) else { continue };
                        let flow = self.flow_for_declared_type(
                            declared,
                            &substitutions,
                            &access_arguments,
                        )?;
                        let field_type = Self::substitute_type(declared, &substitutions);
                        self.bind_pattern(pattern, &flow, Some(&field_type), environment)?;
                    }
                } else if let Some(fields) =
                    Self::builtin_pattern_fields(constructor, value, value_type)
                {
                    for (pattern, (flow, field_type)) in args.iter().zip(fields) {
                        self.bind_pattern(pattern, &flow, field_type.as_ref(), environment)?;
                    }
                }
            }
            Pattern::List { elems, rest } => {
                let element = match value {
                    AccessFlow::Sequence(element) => element.as_ref().clone(),
                    AccessFlow::Named { arguments, dynamic: false, .. }
                        if arguments.len() == 1 => arguments[0].clone(),
                    _ => AccessFlow::Unknown,
                };
                let element_type = value_type.and_then(|ty| match ty.unqualified() {
                    Type::Named(_, arguments) if arguments.len() == 1 => arguments.first(),
                    Type::Slice(elem) => Some(elem.as_ref()),
                    _ => None,
                });
                for pattern in elems {
                    self.bind_pattern(pattern, &element, element_type, environment)?;
                }
                if let Some(Some(name)) = rest {
                    environment.insert(name.clone(), value.clone());
                }
            }
            Pattern::Or(alternatives) => {
                if let Some(first) = alternatives.first() {
                    self.bind_pattern(first, value, value_type, environment)?;
                }
            }
            Pattern::Wildcard
            | Pattern::Var(_)
            | Pattern::Int(_)
            | Pattern::Str(_)
            | Pattern::Bool(_)
            | Pattern::Duration(_)
            | Pattern::IntRange { .. } => {}
        }
        Ok(())
    }

    fn eval_arm(
        &mut self,
        arm: &MatchArm,
        scrutinee: &AccessFlow,
        scrutinee_type: Option<&Type>,
        environment: &FlowEnvironment,
        return_expected: &AccessFlow,
    ) -> Result<(AccessFlow, FlowEnvironment), AccessFlowError> {
        let mut environment = environment.clone();
        self.bind_pattern(&arm.pattern, scrutinee, scrutinee_type, &mut environment)?;
        if let Some(guard) = &arm.guard {
            self.eval_expr(guard, &mut environment, return_expected)?;
        }
        let flow = self.eval_expr(&arm.body, &mut environment, return_expected)?;
        Ok((flow, environment))
    }

    fn join_flows(
        flows: impl IntoIterator<Item = AccessFlow>,
        context: &str,
    ) -> Result<AccessFlow, AccessFlowError> {
        let mut flows = flows.into_iter();
        let Some(first) = flows.next() else { return Ok(AccessFlow::None) };
        flows.try_fold(first, |left, right| left.join(&right, context))
    }

    fn merge_environments(
        target: &mut FlowEnvironment,
        branches: &[FlowEnvironment],
        context: &str,
    ) -> Result<(), AccessFlowError> {
        let names = target.keys().cloned().collect::<Vec<_>>();
        for name in names {
            let mut values = branches
                .iter()
                .filter_map(|environment| environment.get(&name).cloned());
            let Some(first) = values.next() else { continue };
            let value = values.try_fold(first, |left, right| {
                left.join(&right, &format!("{context} binding `{name}`"))
            })?;
            target.insert(name, value);
        }
        Ok(())
    }

    fn record(&mut self, expression: &Expr, flow: AccessFlow) -> AccessFlow {
        let key = expression as *const Expr as usize;
        self.facts.values.insert(key, flow.clone());
        flow
    }

    fn record_call(&mut self, expression: &Expr, signature: &AccessSignature) {
        self.facts
            .calls
            .insert(expression as *const Expr as usize, signature.clone());
    }

    fn eval_expr_with_hint(
        &mut self,
        expression: &Expr,
        hint: Option<&Type>,
        environment: &mut FlowEnvironment,
        return_expected: &AccessFlow,
    ) -> Result<AccessFlow, AccessFlowError> {
        let key = expression as *const Expr as usize;
        let previous = hint.and_then(|hint| self.expression_type_hints.insert(key, hint.clone()));
        let result = self.eval_expr(expression, environment, return_expected);
        if let Some(previous) = previous {
            self.expression_type_hints.insert(key, previous);
        } else if hint.is_some() {
            self.expression_type_hints.remove(&key);
        }
        result
    }

    fn eval_expr(
        &mut self,
        expression: &Expr,
        environment: &mut FlowEnvironment,
        return_expected: &AccessFlow,
    ) -> Result<AccessFlow, AccessFlowError> {
        let flow = match expression {
            Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_)
            | Expr::Bool(_) | Expr::TaggedLit { .. } => AccessFlow::None,
            Expr::Var(name) => {
                if let Some(flow) = environment.get(name) {
                    flow.clone()
                } else {
                    self.signature_for_function_value(name, expression)?
                        .map(AccessFlow::Callable)
                        .unwrap_or(AccessFlow::None)
                }
            }
            Expr::List(values) => {
                let element_hint = self
                    .expression_type_hints
                    .get(&(expression as *const Expr as usize))
                    .and_then(|hint| match hint.unqualified() {
                        Type::Named(_, arguments) if arguments.len() == 1 => {
                            arguments.first().cloned()
                        }
                        _ => None,
                    });
                let elements = values
                    .iter()
                    .map(|value| {
                        self.eval_expr_with_hint(
                            value,
                            element_hint.as_ref(),
                            environment,
                            return_expected,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let element = elements.into_iter().try_fold(
                    AccessFlow::Unknown,
                    |left, right| left.join(&right, "list element access contracts"),
                )?;
                if matches!(element, AccessFlow::None) {
                    AccessFlow::None
                } else {
                    AccessFlow::Sequence(Box::new(element))
                }
            }
            Expr::Tuple(values) => {
                let field_hints = self
                    .expression_type_hints
                    .get(&(expression as *const Expr as usize))
                    .and_then(|hint| match hint.unqualified() {
                        Type::Tuple(fields) if fields.len() == values.len() => Some(fields.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                AccessFlow::product(
                    values
                        .iter()
                        .enumerate()
                        .map(|(index, value)| {
                            self.eval_expr_with_hint(
                                value,
                                field_hints.get(index),
                                environment,
                                return_expected,
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                )
            }
            Expr::Call { name, args } => {
                let positional = args.iter().collect::<Vec<_>>();
                let signature = match environment.get(name) {
                    Some(AccessFlow::Callable(signature)) => Some(signature.clone()),
                    _ => self.signature_for_named_call(name, expression, &positional)?,
                };
                let argument_types = args
                    .iter()
                    .map(|argument| self.resolved_expression_type(argument))
                    .collect::<Vec<_>>();
                let result_type = self.contextual_expression_type(expression);
                let arguments = self.eval_arguments_with_dependent_hints(
                    &positional,
                    signature.as_ref(),
                    &[],
                    CallTypeContext {
                        arguments: &argument_types,
                        result: result_type.as_ref(),
                    },
                    environment,
                    return_expected,
                )?;
                match signature {
                    Some(signature) => {
                        let (signature, _) = self.specialize_signature_from_flows(
                            signature,
                            &arguments,
                            &argument_types,
                            result_type.as_ref(),
                        )?;
                        self.verify_arguments(name, &signature, &arguments)?;
                        self.record_call(expression, &signature);
                        self.flow_from_type(signature.result().ty())
                            .map_err(Self::signature_error)?
                    }
                    None => AccessFlow::None,
                }
            }
            Expr::LabeledCall { name, args } => {
                let positional = args.iter().map(|(_, argument)| argument).collect::<Vec<_>>();
                let signature = self.signature_for_named_call(name, expression, &positional)?;
                let argument_types = positional
                    .iter()
                    .map(|argument| self.resolved_expression_type(argument))
                    .collect::<Vec<_>>();
                let result_type = self.contextual_expression_type(expression);
                let arguments = self.eval_arguments_with_dependent_hints(
                    &positional,
                    signature.as_ref(),
                    &[],
                    CallTypeContext {
                        arguments: &argument_types,
                        result: result_type.as_ref(),
                    },
                    environment,
                    return_expected,
                )?;
                match signature {
                    Some(signature) => {
                        let (signature, _) = self.specialize_signature_from_flows(
                            signature,
                            &arguments,
                            &argument_types,
                            result_type.as_ref(),
                        )?;
                        self.verify_arguments(name, &signature, &arguments)?;
                        self.record_call(expression, &signature);
                        self.flow_from_type(signature.result().ty())
                            .map_err(Self::signature_error)?
                    }
                    None => AccessFlow::None,
                }
            }
            Expr::LabeledMethodCall {
                receiver,
                args,
                ..
            } => {
                self.eval_expr(receiver, environment, return_expected)?;
                for (_, argument) in args {
                    self.eval_expr(argument, environment, return_expected)?;
                }
                AccessFlow::None
            }
            Expr::Apply { func, args } => {
                let function = self.eval_expr(func, environment, return_expected)?;
                let signature = match function {
                    AccessFlow::Callable(signature) => Some(signature),
                    _ => None,
                };
                let argument_types = args
                    .iter()
                    .map(|argument| self.resolved_expression_type(argument))
                    .collect::<Vec<_>>();
                let result_type = self.contextual_expression_type(expression);
                let positional = args.iter().collect::<Vec<_>>();
                let arguments = self.eval_arguments_with_dependent_hints(
                    &positional,
                    signature.as_ref(),
                    &[],
                    CallTypeContext {
                        arguments: &argument_types,
                        result: result_type.as_ref(),
                    },
                    environment,
                    return_expected,
                )?;
                let Some(signature) = signature else {
                    return Ok(self.record(expression, AccessFlow::None));
                };
                let (signature, _) = self.specialize_signature_from_flows(
                    signature,
                    &arguments,
                    &argument_types,
                    result_type.as_ref(),
                )?;
                self.verify_arguments("indirect function", &signature, &arguments)?;
                self.record_call(expression, &signature);
                self.flow_from_type(signature.result().ty()).map_err(Self::signature_error)?
            }
            Expr::Ctor { name, args } | Expr::AnonCtor { tag: name, args } => {
                let expression_type = self.resolved_expression_type(expression).unwrap_or_else(|| {
                    self.variants
                        .get(name.as_str())
                        .and_then(|candidates| candidates.first())
                        .map(|(type_name, _)| Type::Named((*type_name).to_owned(), Vec::new()))
                        .unwrap_or_else(|| Type::Named(name.clone(), Vec::new()))
                });
                let field_hints = self
                    .variant_for(name, &expression_type)
                    .map(|(type_name, variant)| {
                        let substitutions = self.nominal_substitutions(type_name, &expression_type);
                        variant
                            .fields
                            .iter()
                            .map(|field| Self::substitute_type(field, &substitutions))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let arguments = args
                    .iter()
                    .enumerate()
                    .map(|(index, argument)| {
                        self.eval_expr_with_hint(
                            argument,
                            field_hints.get(index),
                            environment,
                            return_expected,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                self.constructor_flow(name, &expression_type, &arguments)?
            }
            Expr::Unary { expr, .. } => {
                self.eval_expr(expr, environment, return_expected)?
            }
            Expr::Try(expr) => {
                let operand = self.eval_expr(expr, environment, return_expected)?;
                match operand {
                    AccessFlow::Named {
                        name,
                        arguments,
                        dynamic: false,
                    } if matches!(name.rsplit('.').next(), Some("Option" | "Result")) => {
                        arguments.into_iter().next().unwrap_or(AccessFlow::None)
                    }
                    other => other,
                }
            }
            Expr::ExistentialPack { expr, ty, .. }
            | Expr::ExistentialUpcast { expr, ty } => {
                self.eval_expr(expr, environment, return_expected)?;
                self.flow_from_type(ty).map_err(Self::signature_error)?
            }
            Expr::Field { base, field } => {
                let base_flow = self.eval_expr(base, environment, return_expected)?;
                if let Ok(index) = field.parse::<usize>() {
                    if let AccessFlow::Product(fields) = &base_flow {
                        fields.get(index).cloned().unwrap_or(AccessFlow::None)
                    } else if let Some(Type::Tuple(fields)) =
                        self.resolved_expression_type(base).map(|ty| ty.unqualified().clone())
                    {
                        fields
                            .get(index)
                            .map(|field| self.flow_from_type(field))
                            .transpose()
                            .map_err(Self::signature_error)?
                            .unwrap_or(AccessFlow::None)
                    } else {
                        AccessFlow::None
                    }
                } else {
                    match self.resolved_expression_type(base) {
                        Some(base_type) => self.field_flow(&base_type, &base_flow, field)?,
                        None => AccessFlow::None,
                    }
                }
            }
            Expr::Lambda { params, body, ret, .. } => {
                let resolved = self
                    .expression_type_hints
                    .get(&(expression as *const Expr as usize))
                    .cloned()
                    .or_else(|| self.resolved_expression_type(expression))
                    .ok_or_else(|| AccessFlowError {
                        message: "lambda has no finalized checked type or checked expression context"
                            .into(),
                    })?;
                // An eta lambda accepted through the owning adapter takes the
                // slot's conventions (see `signature_for_function_value`).
                let conventions: Vec<Convention> = match self.table.let_callable_adapter(expression) {
                    Some(conventions) => conventions.to_vec(),
                    None => params.iter().map(|parameter| parameter.convention).collect(),
                };
                let mut signature = AccessSignature::from_resolved_parts_with_catalog(
                    params.iter().map(|parameter| parameter.ty.as_ref()).collect(),
                    ret.as_ref(),
                    conventions.clone(),
                    &resolved,
                    &self.borrow_catalog,
                )
                .map_err(Self::signature_error)?;
                let mut lambda_environment = environment.clone();
                for (parameter, access) in params.iter().zip(signature.params()) {
                    lambda_environment.insert(
                        parameter.name.clone(),
                        self.flow_from_type(access.ty()).map_err(Self::signature_error)?,
                    );
                }
                let preliminary_expected = if ret.is_none() {
                    AccessFlow::Unknown
                } else {
                    self.flow_from_type(signature.result().ty())
                        .map_err(Self::signature_error)?
                };
                self.return_frames.push(Vec::new());
                let tail =
                    self.eval_block(body, &mut lambda_environment, &preliminary_expected)?;
                let mut returned = self.return_frames.pop().unwrap_or_default();
                if ret.is_none() {
                    if matches!(body.stmts.last(), Some(Stmt::Expr(_))) {
                        returned.push(tail.clone());
                    }
                    let inferred = Self::join_flows(
                        returned.iter().cloned(),
                        "inferred lambda result paths",
                    )?;
                    let inferred_result = inferred.materialize_type(signature.result().ty());
                    let rebuilt = AccessSignature::from_parts_with_catalog(
                        signature
                            .params()
                            .iter()
                            .map(|parameter| parameter.ty().clone())
                            .collect(),
                        inferred_result,
                        conventions,
                        &self.borrow_catalog,
                    )
                    .map_err(Self::signature_error)?;
                    signature = rebuilt.with_callable_contract_of(&signature);
                }
                let expected = self.flow_from_type(signature.result().ty())
                    .map_err(Self::signature_error)?;
                for returned in &returned {
                    returned.verify_directed(&expected, "lambda return")?;
                }
                if matches!(body.stmts.last(), Some(Stmt::Expr(_))) {
                    tail.verify_directed(&expected, "lambda result")?;
                }
                AccessFlow::Callable(signature)
            }
            Expr::As { expr, ty } => {
                let actual = self.eval_expr(expr, environment, return_expected)?;
                let expected = self.flow_from_type(ty).map_err(Self::signature_error)?;
                actual.verify_directed(&expected, "function cast")?;
                expected
            }
            Expr::RecordUpdate { name, base, fields } => {
                let base_type = self.resolved_expression_type(base);
                let Some(base_type) = base_type else {
                    self.eval_expr(base, environment, return_expected)?;
                    for (_, value) in fields {
                        self.eval_expr(value, environment, return_expected)?;
                    }
                    return Ok(self.record(expression, AccessFlow::None));
                };
                let record_name = name
                    .as_deref()
                    .or_else(|| match base_type.unqualified() {
                        Type::Named(name, _) => Some(name.as_str()),
                        _ => None,
                    })
                    .unwrap_or("record");
                self.record_flow(
                    record_name,
                    fields,
                    Some(base),
                    &base_type,
                    environment,
                    return_expected,
                )?
            }
            Expr::Record { name, fields, spread } => {
                let expression_type = self
                    .resolved_expression_type(expression)
                    .unwrap_or_else(|| Type::Named(name.clone(), Vec::new()));
                self.record_flow(
                    name,
                    fields,
                    spread.as_deref(),
                    &expression_type,
                    environment,
                    return_expected,
                )?
            }
            Expr::ExistentialCall {
                receiver,
                args,
                ty,
                method,
                params,
                result,
                conventions,
                ..
            } => {
                let receiver_flow = self.eval_expr(receiver, environment, return_expected)?;
                let signature = self.existential_signature(ty, params, result, conventions)?;
                let mut argument_types = Vec::with_capacity(args.len() + 1);
                argument_types.push(self.resolved_expression_type(receiver));
                argument_types.extend(
                    args.iter()
                        .map(|argument| self.resolved_expression_type(argument)),
                );
                let result_type = self.contextual_expression_type(expression);
                let positional = args.iter().collect::<Vec<_>>();
                let arguments = self.eval_arguments_with_dependent_hints(
                    &positional,
                    Some(&signature),
                    std::slice::from_ref(&receiver_flow),
                    CallTypeContext {
                        arguments: &argument_types,
                        result: result_type.as_ref(),
                    },
                    environment,
                    return_expected,
                )?;
                let mut all_arguments = Vec::with_capacity(arguments.len() + 1);
                all_arguments.push(receiver_flow.clone());
                all_arguments.extend(arguments.iter().cloned());
                let (signature, _) = self.specialize_signature_from_flows(
                    signature,
                    &all_arguments,
                    &argument_types,
                    self.contextual_expression_type(expression).as_ref(),
                )?;
                if let Some(receiver_param) = signature.params().first() {
                    let expected = self.flow_from_type(receiver_param.ty())
                        .map_err(Self::signature_error)?;
                    receiver_flow.verify_directed(
                        &expected,
                        &format!("receiver passed to dynamic method `{method}`"),
                    )?;
                }
                for (index, (actual, expected)) in arguments
                    .iter()
                    .zip(signature.params().iter().skip(1))
                    .enumerate()
                {
                    let expected = self.flow_from_type(expected.ty())
                        .map_err(Self::signature_error)?;
                    actual.verify_directed(
                        &expected,
                        &format!("argument {} passed to dynamic method `{method}`", index + 1),
                    )?;
                }
                self.record_call(expression, &signature);
                self.flow_from_type(signature.result().ty()).map_err(Self::signature_error)?
            }
            Expr::Binary { op: BinOp::Coalesce, lhs, rhs } => {
                let left = self.eval_expr(lhs, environment, return_expected)?;
                let right = self.eval_expr(rhs, environment, return_expected)?;
                let left = match left {
                    AccessFlow::Named { name, mut arguments, dynamic: false }
                        if matches!(name.rsplit('.').next(), Some("Option" | "Result"))
                            && !arguments.is_empty() =>
                    {
                        arguments.remove(0)
                    }
                    _ => self
                        .resolved_expression_type(lhs)
                        .and_then(|ty| match ty.unqualified() {
                            Type::Named(name, arguments)
                                if matches!(name.rsplit('.').next(), Some("Option" | "Result"))
                                    && !arguments.is_empty() =>
                            {
                                arguments.first().cloned()
                            }
                            _ => None,
                        })
                        .map(|ty| self.flow_from_type(&ty))
                        .transpose()
                        .map_err(Self::signature_error)?
                        .unwrap_or(AccessFlow::None),
                };
                left.join(&right, "coalescing callable alternatives")?
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.eval_expr(lhs, environment, return_expected)?;
                self.eval_expr(rhs, environment, return_expected)?;
                AccessFlow::None
            }
            Expr::If { cond, then_block, else_block } => {
                self.eval_expr(cond, environment, return_expected)?;
                let mut then_environment = environment.clone();
                let then_flow =
                    self.eval_block(then_block, &mut then_environment, return_expected)?;
                let (else_flow, else_environment) = if let Some(else_block) = else_block {
                    let mut else_environment = environment.clone();
                    let flow =
                        self.eval_block(else_block, &mut else_environment, return_expected)?;
                    (flow, else_environment)
                } else {
                    (AccessFlow::None, environment.clone())
                };
                let completing_environments = [(&then_flow, then_environment), (&else_flow, else_environment)]
                    .into_iter()
                    .filter_map(|(flow, environment)| {
                        (!matches!(flow, AccessFlow::Diverges)).then_some(environment)
                    })
                    .collect::<Vec<_>>();
                Self::merge_environments(environment, &completing_environments, "if branches")?;
                then_flow.join(&else_flow, "if-expression branches")?
            }
            Expr::Match { scrutinee, arms } => {
                let scrutinee_type = self.resolved_expression_type(scrutinee)
                    .or_else(|| self.contextual_expression_type(scrutinee));
                let scrutinee = self.eval_expr(scrutinee, environment, return_expected)?;
                let branches = arms
                    .iter()
                    .map(|arm| {
                        self.eval_arm(
                            arm,
                            &scrutinee,
                            scrutinee_type.as_ref(),
                            environment,
                            return_expected,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let flows = branches.iter().map(|(flow, _)| flow.clone());
                let branch_environments = branches
                    .iter()
                    .filter_map(|(flow, environment)| {
                        (!matches!(flow, AccessFlow::Diverges)).then_some(environment.clone())
                    })
                    .collect::<Vec<_>>();
                Self::merge_environments(environment, &branch_environments, "match arms")?;
                Self::join_flows(flows, "match-expression arms")?
            }
            Expr::Block(block) => self.eval_block(block, environment, return_expected)?,
            Expr::While { cond, body } => {
                self.eval_expr(cond, environment, return_expected)?;
                self.eval_block(body, &mut environment.clone(), return_expected)?;
                AccessFlow::None
            }
            Expr::For { var, iter, body } => {
                let iter_type = self.resolved_expression_type(iter);
                let iter_flow = self.eval_expr(iter, environment, return_expected)?;
                let checked_element = iter_type
                    .as_ref()
                    .and_then(|ty| match ty.unqualified() {
                        Type::Named(_, arguments) if arguments.len() == 1 => arguments.first(),
                        Type::Slice(elem) => Some(elem.as_ref()),
                        _ => None,
                    })
                    .map(|element| self.flow_from_type(element))
                    .transpose()
                    .map_err(Self::signature_error)?;
                let element = match &iter_flow {
                    AccessFlow::Sequence(element) if element.has_callable_contract() => {
                        element.as_ref().clone()
                    }
                    AccessFlow::Named { arguments, dynamic: false, .. }
                        if arguments.len() == 1
                            && arguments[0].has_callable_contract() =>
                    {
                        arguments[0].clone()
                    }
                    _ => checked_element.unwrap_or(AccessFlow::None),
                };
                let mut body_environment = environment.clone();
                body_environment.insert(var.clone(), element);
                self.eval_block(body, &mut body_environment, return_expected)?;
                AccessFlow::None
            }
            Expr::Range { lo, hi, .. } => {
                self.eval_expr(lo, environment, return_expected)?;
                self.eval_expr(hi, environment, return_expected)?;
                AccessFlow::None
            }
            Expr::Index { base, index } => {
                let base = self.eval_expr(base, environment, return_expected)?;
                self.eval_expr(index, environment, return_expected)?;
                match &base {
                    AccessFlow::Sequence(element) => element.as_ref().clone(),
                    AccessFlow::Named { arguments, dynamic: false, .. }
                        if arguments.len() == 1 => arguments[0].clone(),
                    _ => self
                        .resolved_expression_type(expression)
                        .map(|ty| self.flow_from_type(&ty))
                        .transpose()
                        .map_err(Self::signature_error)?
                        .unwrap_or(AccessFlow::None),
                }
            }
            Expr::WhileLet { pattern, scrutinee, body } => {
                let scrutinee_type = self.resolved_expression_type(scrutinee);
                let scrutinee = self.eval_expr(scrutinee, environment, return_expected)?;
                let mut body_environment = environment.clone();
                self.bind_pattern(
                    pattern,
                    &scrutinee,
                    scrutinee_type.as_ref(),
                    &mut body_environment,
                )?;
                self.eval_block(body, &mut body_environment, return_expected)?;
                AccessFlow::None
            }
            Expr::MethodCall { receiver, args, .. } => {
                self.eval_expr(receiver, environment, return_expected)?;
                for argument in args {
                    self.eval_expr(argument, environment, return_expected)?;
                }
                AccessFlow::None
            }
        };
        Ok(self.record(expression, flow))
    }
}

/// Build the checked access-signature query for an already type-checked module.
/// The supplied table is authoritative for every inferred and use-site
/// specialized type; this pass propagates only ownership/access contracts.
pub fn checked_facts(
    module: &Module,
    table: &TypeTable,
) -> Result<CheckedAccessFacts, AccessFlowError> {
    AccessVerifier::new(module, table).verify()
}

/// The access facts of `typed`: the ones its checked annotation derived when
/// they are current, otherwise derived into `derived`. Every checked
/// annotation derives exactly [`checked_facts`] of its module and table, so a
/// retained copy is that answer. `WITCHY_VERIFY_INCREMENTAL_ANNOTATE=1`
/// derives them again and panics if they differ.
pub(crate) fn retained_or_checked<'a>(
    typed: &'a crate::typeck::TypedModule,
    derived: &'a mut Option<CheckedAccessFacts>,
) -> Result<&'a CheckedAccessFacts, AccessFlowError> {
    static VERIFY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    match typed.access_facts() {
        Ok(retained) => {
            if *VERIFY.get_or_init(|| std::env::var_os("WITCHY_VERIFY_INCREMENTAL_ANNOTATE").is_some()) {
                let fresh = checked_facts(typed.module(), typed.table())?;
                assert!(*retained == fresh, "retained access facts differ from the module's own");
            }
            Ok(retained)
        }
        Err(_) => Ok(derived.insert(checked_facts(typed.module(), typed.table())?)),
    }
}

/// Verify one lowered module through the same checked query used by access-fact
/// consumers. This convenience wrapper owns the cloned AST together with its
/// address-keyed type table for the duration of the query.
pub fn verify_module(module: &Module) -> Result<(), AccessFlowError> {
    let typed = crate::typeck::annotate_checked(module.clone()).map_err(|error| {
        AccessFlowError { message: error.message }
    })?;
    typed.access_facts().map(|_| ()).map_err(|error| AccessFlowError {
        message: error.to_string(),
    })
}
