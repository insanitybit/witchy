//! Checked-source closure witnesses for canonical SafeWIR lowering.
//!
//! Closure identity and capture information are derived from the exact typed
//! AST allocation.  This module deliberately does not parse source or infer
//! authority from names: the shared lambda scanner supplies capture order,
//! the type table supplies checked expression types, and lexical declaration
//! paths supply stable binding identities.

use std::{collections::{BTreeMap, HashSet}, fmt};

use witchy_syntax::ast::{self, Block, Expr, Item, Pattern, Stmt, Type};

use crate::canonical_abi;
use crate::typeck::{self, Ty, TypedModule};
use witchy_canonical_source::closure::{
    ClosureBindingId, ClosureBindingPathStep, ClosureCaptureTransfer, ClosureCaptureWitness, ClosureWitness,
    ClosureWitnessManifest,
};
use witchy_canonical_source::proper_tail::{ProperTailCallableId, ProperTailExprKind, ProperTailPathStep};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClosureWitnessError {
    MissingCallableType { path: Vec<ProperTailPathStep> },
    CallableTypeNotFunction { path: Vec<ProperTailPathStep>, ty: Ty },
    BorrowedCapture { name: String },
    MissingCaptureType { name: String },
    UnresolvedCapture { name: String },
    UnsupportedType { context: String },
    PathOverflow,
    DuplicateLambda,
}

impl ClosureWitnessError {
    /// Whether this failure is a precision limit of the canonical v1 closure
    /// witness on a program the type checker accepted (an RFC-0165 obligation
    /// the pipeline records and canonical lowering refuses), as opposed to an
    /// internal inconsistency of the witness itself, which stays a hard error.
    pub fn is_canonical_obligation(&self) -> bool {
        match self {
            Self::MissingCallableType { .. }
            | Self::CallableTypeNotFunction { .. }
            | Self::BorrowedCapture { .. }
            | Self::MissingCaptureType { .. }
            | Self::UnresolvedCapture { .. }
            | Self::UnsupportedType { .. } => true,
            Self::PathOverflow | Self::DuplicateLambda => false,
        }
    }
}

impl fmt::Display for ClosureWitnessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingCallableType { .. } => f.write_str("canonical closure lacks a checked callable type"),
            Self::CallableTypeNotFunction { ty, .. } => write!(f, "canonical closure has non-callable checked type `{ty}`"),
            Self::BorrowedCapture { name } => write!(f, "canonical closure capture `{name}` carries an explicit reference and cannot be authenticated in v1"),
            Self::MissingCaptureType { name } => write!(f, "canonical closure capture `{name}` lacks an exact checked type"),
            Self::UnresolvedCapture { name } => write!(f, "canonical closure capture `{name}` is not a resolved lexical binding or authenticated module reference"),
            Self::UnsupportedType { context } => write!(f, "canonical closure contains an unsupported type: {context}"),
            Self::PathOverflow => f.write_str("canonical closure structural path exceeds u32 range"),
            Self::DuplicateLambda => f.write_str("canonical closure manifest contains a duplicate lambda identity"),
        }
    }
}

impl std::error::Error for ClosureWitnessError {}

#[derive(Clone)]
struct Binding {
    id: ClosureBindingId,
    ty: Option<Type>,
    borrowed: bool,
    mutable: bool,
    unproved_qualifier: bool,
}







fn expr_kind(expr: &Expr) -> ProperTailExprKind {
    match expr {
        Expr::Int(_) => ProperTailExprKind::Int, Expr::Float(_) => ProperTailExprKind::Float,
        Expr::Duration(_) => ProperTailExprKind::Duration, Expr::Str(_) => ProperTailExprKind::Str,
        Expr::Bool(_) => ProperTailExprKind::Bool, Expr::List(_) => ProperTailExprKind::List,
        Expr::Tuple(_) => ProperTailExprKind::Tuple, Expr::Var(_) => ProperTailExprKind::Var,
        Expr::Call { .. } => ProperTailExprKind::Call, Expr::LabeledCall { .. } => ProperTailExprKind::LabeledCall,
        Expr::LabeledMethodCall { .. } => ProperTailExprKind::LabeledMethodCall, Expr::MethodCall { .. } => ProperTailExprKind::MethodCall,
        Expr::Apply { .. } => ProperTailExprKind::Apply, Expr::Ctor { .. } => ProperTailExprKind::Ctor,
        Expr::AnonCtor { .. } => ProperTailExprKind::AnonCtor, Expr::Unary { .. } => ProperTailExprKind::Unary,
        Expr::Field { .. } => ProperTailExprKind::Field, Expr::Lambda { .. } => ProperTailExprKind::Lambda,
        Expr::RecordUpdate { .. } => ProperTailExprKind::RecordUpdate, Expr::Record { .. } => ProperTailExprKind::Record,
        Expr::Try(_) => ProperTailExprKind::Try, Expr::As { .. } => ProperTailExprKind::As,
        Expr::ExistentialPack { .. } => ProperTailExprKind::ExistentialPack, Expr::ExistentialUpcast { .. } => ProperTailExprKind::ExistentialUpcast,
        Expr::ExistentialCall { .. } => ProperTailExprKind::ExistentialCall, Expr::Binary { .. } => ProperTailExprKind::Binary,
        Expr::If { .. } => ProperTailExprKind::If, Expr::Match { .. } => ProperTailExprKind::Match,
        Expr::Block(_) => ProperTailExprKind::Block, Expr::While { .. } => ProperTailExprKind::While,
        Expr::For { .. } => ProperTailExprKind::For, Expr::Range { .. } => ProperTailExprKind::Range,
        Expr::Index { .. } => ProperTailExprKind::Index, Expr::WhileLet { .. } => ProperTailExprKind::WhileLet,
        Expr::TaggedLit { .. } => ProperTailExprKind::TaggedLit,
    }
}

fn explicit_borrow(ty: &Type) -> bool {
    match ty {
        // Rooted views are ordinary copyable capture carriers, not lexical
        // references.  Only migration-only reference forms stay on the old
        // closure escape path.
        Type::Qualified(ast::TypeQual::View, _) => false,
        Type::Qualified(ast::TypeQual::Borrow(_) | ast::TypeQual::LegacyBorrow(_) | ast::TypeQual::BorrowMut(_), _) => true,
        Type::Qualified(_, inner) => explicit_borrow(inner),
        Type::Slice(inner) => explicit_borrow(inner),
        Type::Tuple(items) => items.iter().any(explicit_borrow),
        Type::Fn(inputs, output, _, _) => inputs.iter().any(explicit_borrow) || explicit_borrow(output),
        Type::Named(_, args) | Type::Dyn(_, args) => args.iter().any(explicit_borrow),
        Type::RecordCompose { base, fields } => explicit_borrow(base) || fields.iter().any(|(_, ty)| explicit_borrow(ty)),
    }
}

fn carries_view_syntax(ty: &Type) -> bool {
    fn walk(ty: &Type, depth: usize, remaining: &mut usize) -> bool {
        if depth >= 128 || *remaining == 0 { return true; }
        *remaining -= 1;
        let child = |ty: &Type, remaining: &mut usize| walk(ty, depth + 1, remaining);
        match ty {
            Type::Qualified(ast::TypeQual::View, _) => true,
            Type::Qualified(_, inner) | Type::Slice(inner) => child(inner, remaining),
            Type::Tuple(items) | Type::Named(_, items) | Type::Dyn(_, items) =>
                items.iter().any(|item| child(item, remaining)),
            Type::Fn(inputs, output, _, _) =>
                inputs.iter().any(|input| child(input, remaining)) || child(output, remaining),
            Type::RecordCompose { base, fields } =>
                child(base, remaining) || fields.iter().any(|(_, ty)| child(ty, remaining)),
        }
    }
    walk(ty, 0, &mut 4_096)
}

/// The runtime `Ty` authenticates inference, while the lambda annotation is
/// the source authority for qualifier-bearing callable lanes.  Fill only
/// omitted annotations from the checked runtime type; never manufacture a
/// qualifier that the checked AST did not declare.
fn lambda_declared_callable_type(expr: &Expr, checked: &Ty) -> Option<Type> {
    let Expr::Lambda {
        params,
        ret,
        qualifiers,
        ..
    } = expr else {
        return None;
    };
    let checked = typeck::ty_to_ast(checked)?;
    let Type::Fn(checked_parameters, checked_result, _, _) = checked.unqualified() else {
        return None;
    };
    if params.len() != checked_parameters.len() {
        return None;
    }
    let parameters = params
        .iter()
        .zip(checked_parameters)
        .map(|(parameter, checked)| parameter.ty.clone().unwrap_or_else(|| checked.clone()))
        .collect();
    let result = ret.clone().unwrap_or_else(|| (**checked_result).clone());
    Some(Type::Fn(
        parameters,
        Box::new(result),
        params.iter().map(|parameter| parameter.convention).collect(),
        *qualifiers,
    ))
}

fn pattern_names(pattern: &Pattern) -> Vec<String> {
    let mut names = Vec::new();
    ast::pattern_binds(pattern, &mut names);
    names
}

/// A lambda expression's structural identity (the callable id the closure
/// witness records) and the address of its `Expr` node in the checked
/// module, for a consumer that synthesizes a function per capture-free
/// lambda and meets the node again while walking the same module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LambdaSite {
    pub id: ProperTailCallableId,
    pub expr_address: usize,
}

struct Builder<'a> {
    typed: &'a TypedModule,
    module: &'a ast::Module,
    definitions: BTreeMap<String, ast::TypeDef>,
    entries: Vec<ClosureWitness>,
    scopes: Vec<Vec<Binding>>,
    sites: Vec<LambdaSite>,
}

impl<'a> Builder<'a> {
    fn plain_call_view_result(&self, name: &str, arguments: &[&Expr]) -> bool {
        if self.resolve(name).is_some_and(|binding| binding.mutable || binding.unproved_qualifier) {
            return false;
        }
        let declared_result = self.resolve(name).and_then(|binding| binding.ty.as_ref()).and_then(|ty|
            match ty.unqualified() { Type::Fn(_, result, _, _) => Some(result.as_ref()), _ => None })
            .or_else(|| self.module.items.iter().find_map(|item| match item {
                Item::Function(function) if function.name == name => function.ret.as_ref(),
                _ => None,
            }));
        if declared_result.is_none_or(canonical_abi::has_storage_qualifier) { return false; }
        arguments.iter().all(|argument| {
            let (source, unproved) = self.source_type_for_expr(argument);
            !unproved && source.as_ref().is_some_and(|ty| !canonical_abi::has_storage_qualifier(ty))
        })
    }

    /// Recover only a qualifier that the checked source actually declared.
    /// `Ty::View` erases storage qualifiers, so an inferred local view needs
    /// the checked formation fact and its lexical root before its capture can
    /// receive an exact callable/owner identity. Mutable aliases may change
    /// that source type before capture and therefore stay unproved.
    fn source_type_for_expr(&self, expr: &Expr) -> (Option<Type>, bool) {
        let checked = self.typed.table().type_of(expr).and_then(typeck::ty_to_ast);
        let agrees = |source: &Type| {
            canonical_abi::erase_storage_qualifiers(source, 0, &mut 4_096)
                .as_ref() == checked.as_ref()
        };
        match expr {
            Expr::Var(name) => {
                let Some(binding) = self.resolve(name) else { return (checked, false) };
                if binding.unproved_qualifier { return (None, true) }
                let Some(source) = binding.ty.as_ref() else { return (checked, false) };
                if !canonical_abi::has_storage_qualifier(source) { return (checked, false) }
                if binding.mutable || !agrees(source) { return (None, true) }
                (Some(source.clone()), false)
            }
            Expr::Unary { op: ast::UnOp::Borrow, expr: place } => {
                let fact = self.typed.view_facts().ok().and_then(|facts| facts.view_for(expr));
                let root_name = fact.map(|fact| fact.place().root()).or_else(|| match place.as_ref() {
                    Expr::Var(name) => Some(name.as_str()),
                    _ => None,
                });
                let Some(root) = root_name.and_then(|name| self.resolve(name)) else {
                    return (checked, false);
                };
                if root.unproved_qualifier { return (None, true) }
                let Some(source_root) = root.ty.as_ref() else { return (checked, false) };
                if !canonical_abi::has_storage_qualifier(source_root) { return (checked, false) }
                let Some(fact) = fact else { return (None, true) };
                if root.mutable || !fact.place().steps().is_empty()
                    || !matches!(place.as_ref(), Expr::Var(name) if name == fact.place().root())
                {
                    return (None, true);
                }
                let source = Type::Qualified(ast::TypeQual::View, Box::new(source_root.clone()));
                if !agrees(&source)
                    || !matches!(
                        witchy_canonical_source::storage::StoragePlan::classify(
                            &source, &self.definitions, |_| Some(0)),
                        witchy_canonical_source::storage::StoragePlan::RootedView(_)
                    )
                {
                    return (None, true);
                }
                (Some(source), false)
            }
            // The checker does not yet publish a qualifier-preservation fact
            // for every `as` route (notably record width and dyn casts).
            // An erased-HM type match is not authority to mint that target.
            Expr::As { ty, .. } if canonical_abi::has_storage_qualifier(ty) => (None, true),
            Expr::Tuple(items) => {
                let parts: Vec<_> = items.iter().map(|item| self.source_type_for_expr(item)).collect();
                if parts.iter().any(|(_, unproved)| *unproved) { return (None, true) }
                if parts.iter().any(|(ty, _)| ty.as_ref().is_some_and(canonical_abi::has_storage_qualifier)) {
                    let Some(parts) = parts.into_iter().map(|(ty, _)| ty).collect::<Option<Vec<_>>>() else {
                        return (None, true);
                    };
                    let source = Type::Tuple(parts);
                    return if agrees(&source) { (Some(source), false) } else { (None, true) };
                }
                (checked, false)
            }
            Expr::Call { name, args } if checked.as_ref().is_some_and(carries_view_syntax) => {
                if self.plain_call_view_result(name, &args.iter().collect::<Vec<_>>()) {
                    (checked, false)
                } else { (None, true) }
            }
            Expr::LabeledCall { name, args } if checked.as_ref().is_some_and(carries_view_syntax) => {
                if self.plain_call_view_result(name, &args.iter().map(|(_, expr)| expr).collect::<Vec<_>>()) {
                    (checked, false)
                } else { (None, true) }
            }
            Expr::Apply { func, args } if checked.as_ref().is_some_and(carries_view_syntax) => {
                if let Expr::Var(name) = func.as_ref()
                    && self.plain_call_view_result(name, &args.iter().collect::<Vec<_>>())
                {
                    (checked, false)
                } else { (None, true) }
            }
            Expr::If { then_block, else_block: Some(else_block), .. } => {
                let tail = |block: &Block| match block.stmts.as_slice() {
                    [Stmt::Expr(value)] => Some(self.source_type_for_expr(value)),
                    _ => None,
                };
                if let (Some((Some(left), false)), Some((Some(right), false))) =
                    (tail(then_block), tail(else_block))
                    && left == right && agrees(&left)
                {
                    return (Some(left), false);
                }
                if checked.as_ref().is_some_and(carries_view_syntax) { (None, true) } else { (checked, false) }
            }
            // Other view-producing expressions need a source-authenticated
            // result contract before a closure may capture them. HM's view
            // payload type alone has erased qualifier identity.
            _ if checked.as_ref().is_some_and(carries_view_syntax) => (None, true),
            _ => (checked, false),
        }
    }

    /// Project the source-authenticated type through a binding pattern. The
    /// checked HM type alone has already erased storage qualifiers. Patterns
    /// without a proved projection keep their bindings unproved instead of
    /// minting an unqualified closure capture.
    fn source_may_contain_qualifier(&self, ty: &Type, depth: usize, remaining: &mut usize) -> bool {
        fn walk(
            ty: &Type,
            definitions: &BTreeMap<String, ast::TypeDef>,
            depth: usize,
            remaining: &mut usize,
            active: &mut HashSet<Type>,
        ) -> bool {
            if *remaining == 0 || depth >= 128 { return true; }
            *remaining -= 1;
            // A repeated exact instantiation contributes no new fields. The
            // first visit still inspects every sibling, including qualifiers;
            // expanding generic instantiations retain the resource limits.
            if !active.insert(ty.clone()) { return false; }
            let mut child = |ty: &Type| walk(ty, definitions, depth + 1, remaining, active);
            let contains = match ty {
                Type::Qualified(ast::TypeQual::Frozen | ast::TypeQual::Unique, _) => true,
                Type::Qualified(_, inner) | Type::Slice(inner) => child(inner),
                Type::Tuple(items) | Type::Dyn(_, items) => items.iter().any(&mut child),
                Type::Named(_, arguments) => {
                    if arguments.iter().any(&mut child) { true }
                    else {
                        match witchy_canonical_source::aggregates::instantiated_declared_fields(ty, definitions) {
                            Some(Ok(fields)) => fields.iter().any(child),
                            Some(Err(())) => true,
                            None => false,
                        }
                    }
                }
                Type::Fn(parameters, result, _, _) => parameters.iter().any(&mut child) || child(result),
                Type::RecordCompose { base, fields } => child(base) || fields.iter().any(|(_, field)| child(field)),
            };
            active.remove(ty);
            contains
        }
        walk(ty, &self.definitions, depth, remaining, &mut HashSet::new())
    }

    fn pattern_binding_types(&self, pattern: &Pattern, source: Option<&Type>, unproved: bool) -> Vec<(String, Option<Type>, bool)> {
        match pattern {
            Pattern::Var(name) if name != "_" => vec![(name.clone(), source.cloned(), unproved)],
            Pattern::Tuple(parts) => {
                if let Some(Type::Tuple(fields)) = source && parts.len() == fields.len() {
                    return parts.iter().zip(fields).flat_map(|(part, field)|
                        self.pattern_binding_types(part, Some(field), unproved)).collect();
                }
                let mut remaining = 4_096;
                let uncertain = unproved || source.is_some_and(|ty| self.source_may_contain_qualifier(ty, 0, &mut remaining));
                parts.iter().flat_map(|part| self.pattern_binding_types(part, None, uncertain)).collect()
            }
            Pattern::Ctor { name, args } | Pattern::AnonCtor { tag: name, args } => {
                if let Some(ty @ Type::Named(type_name, _)) = source
                    && let Some(definition) = self.definitions.get(type_name)
                    && let Some(Ok(fields)) = witchy_canonical_source::aggregates::instantiated_declared_fields(ty, &self.definitions)
                {
                    let matching = definition.variants.iter().enumerate().filter(|(_, variant)|
                        variant.name == *name || variant.name.rsplit('.').next() == name.rsplit('.').next()).collect::<Vec<_>>();
                    if let [(index, variant)] = matching.as_slice() && args.len() == variant.fields.len() {
                        let first = definition.variants.iter().take(*index).map(|variant| variant.fields.len()).sum::<usize>();
                        if let Some(fields) = fields.get(first..first + args.len()) {
                            return args.iter().zip(fields).flat_map(|(part, field)| {
                                let mut remaining = 4_096;
                                if self.source_may_contain_qualifier(field, 0, &mut remaining) {
                                    self.pattern_binding_types(part, None, true)
                                } else {
                                    self.pattern_binding_types(part, Some(field), unproved)
                                }
                            }).collect();
                        }
                    }
                    let mut remaining = 4_096;
                    let uncertain = unproved || fields.iter().any(|field| self.source_may_contain_qualifier(field, 0, &mut remaining));
                    return pattern_names(pattern).into_iter().map(|name| (name, None, uncertain)).collect();
                }
                pattern_names(pattern).into_iter().map(|name| (name, None, true)).collect()
            }
            _ => {
                let mut remaining = 4_096;
                let uncertain = unproved || source.is_some_and(|ty| self.source_may_contain_qualifier(ty, 0, &mut remaining));
                pattern_names(pattern).into_iter().map(|name| (name, None, uncertain)).collect()
            }
        }
    }

    fn observe_binding_type(&mut self, name: &str, checked_type: Type) {
        if let Some(binding) = self.scopes.iter_mut().rev()
            .flat_map(|scope| scope.iter_mut().rev())
            .find(|binding| binding.id.name() == name)
            && binding.ty.is_none() && !binding.unproved_qualifier
        {
            binding.borrowed |= explicit_borrow(&checked_type);
            binding.ty = Some(checked_type);
        }
    }

    fn resolve(&self, name: &str) -> Option<&Binding> {
        self.scopes.iter().rev().flat_map(|scope| scope.iter().rev()).find(|binding| binding.id.name() == name)
    }

    /// A name a closure body may mention without capturing anything: a checked
    /// module function or constant, or a compiler-owned intrinsic/capability operation
    /// (`__capop.print`, `__capop.listen_tls`, ...). Capability operations are
    /// intrinsics keyed by the frontend's fixed table, not values in scope; the
    /// authority they exercise arrives as the capability argument at the call,
    /// so referencing one captures no environment. The cap-op table is the
    /// frontend's, never a spelling the closure builder invents.
    fn is_authenticated_module_reference(&self, name: &str) -> bool {
        self.typed.table().function_type(name).is_some()
            || witchy_syntax::intrinsics::lookup(name).is_some()
            || witchy_syntax::cap_ops::is_op_name(name)
            || witchy_syntax::ast::is_render_intrinsic(name)
            || self.module.items.iter().any(|item| match item {
                Item::Function(function) => function.name == name,
                Item::Const { name: constant, .. } => constant == name,
                _ => false,
            })
    }

    fn resolve_capture(&self, name: &str) -> Result<Option<&Binding>, ClosureWitnessError> {
        if let Some(binding) = self.resolve(name) {
            return Ok(Some(binding));
        }
        if self.is_authenticated_module_reference(name) {
            return Ok(None);
        }
        Err(ClosureWitnessError::UnresolvedCapture { name: name.to_string() })
    }

    fn walk_callable(&mut self, body: &Block, callable: ProperTailCallableId, params: &[ast::Param], param_types: &[Type]) -> Result<(), ClosureWitnessError> {
        let mut scope = Vec::new();
        for (index, param) in params.iter().enumerate() {
            let ty = param.ty.clone().or_else(|| param_types.get(index).cloned());
            scope.push(Binding { id: ClosureBindingId::new(callable.clone(), vec![ClosureBindingPathStep::Parameter(index as u32)], param.name.clone()), ty: ty.clone(), borrowed: ty.as_ref().is_some_and(explicit_borrow), mutable: param.convention.binds_mutable(), unproved_qualifier: false });
        }
        self.scopes.push(scope);
        let mut initial_path = Vec::new();
        let result = self.walk_block(body, callable, &mut initial_path);
        self.scopes.pop();
        result
    }

    fn walk_block(&mut self, block: &Block, callable: ProperTailCallableId, path: &mut Vec<ProperTailPathStep>) -> Result<(), ClosureWitnessError> {
        self.scopes.push(Vec::new());
        for (index, statement) in block.stmts.iter().enumerate() {
            let index = u32::try_from(index).map_err(|_| ClosureWitnessError::PathOverflow)?;
            path.push(ProperTailPathStep::Statement(index));
            match statement {
                Stmt::Let { name, ty, value, mutable } => {
                    self.child(value, callable.clone(), path, 0)?;
                    let (inferred, unproved_qualifier) = self.source_type_for_expr(value);
                    // A local annotation can constrain the erased HM type,
                    // but it cannot authenticate Frozen/Unique storage. The
                    // initializer must independently prove the same source
                    // type before a closure may capture this binding.
                    let annotation_unproved = ty.as_ref().is_some_and(|declared|
                        carries_view_syntax(declared)
                            && (unproved_qualifier || inferred.as_ref() != Some(declared)));
                    let checked = ty.clone().or(inferred);
                    let borrowed = ty.as_ref().is_some_and(explicit_borrow)
                        || matches!(value, Expr::Unary { op: ast::UnOp::BorrowMut, .. })
                        || matches!(value, Expr::Var(source) if self.resolve(source).is_some_and(|binding| binding.borrowed));
                    self.scopes.last_mut().unwrap().push(Binding { id: ClosureBindingId::new(callable.clone(), vec![ClosureBindingPathStep::Statement(index)], name.clone()), ty: checked, borrowed, mutable: *mutable, unproved_qualifier: annotation_unproved || ty.is_none() && unproved_qualifier });
                }
                Stmt::Assign { value, .. } => self.child(value, callable.clone(), path, 0)?,
                Stmt::LetPattern { pattern, value } => {
                    self.child(value, callable.clone(), path, 0)?;
                    let (source, unproved) = self.source_type_for_expr(value);
                    for (slot, (name, ty, unproved_qualifier)) in self.pattern_binding_types(pattern, source.as_ref(), unproved).into_iter().enumerate() {
                        let borrowed = ty.as_ref().is_some_and(explicit_borrow);
                        self.scopes.last_mut().unwrap().push(Binding { id: ClosureBindingId::new(callable.clone(), vec![ClosureBindingPathStep::Statement(index), ClosureBindingPathStep::Pattern(slot as u32)], name), ty, borrowed, mutable: false, unproved_qualifier });
                    }
                }
                Stmt::Return(Some(value)) => {
                    path.push(ProperTailPathStep::ReturnValue);
                    self.walk_expr(value, callable.clone(), path)?;
                    path.pop();
                }
                Stmt::Expr(value) => {
                    path.push(ProperTailPathStep::Value);
                    self.walk_expr(value, callable.clone(), path)?;
                    path.pop();
                }
                Stmt::Yield(value) => {
                    path.push(ProperTailPathStep::YieldValue);
                    self.walk_expr(value, callable.clone(), path)?;
                    path.pop();
                }
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
            path.pop();
        }
        self.scopes.pop();
        Ok(())
    }

    fn walk_expr(&mut self, expr: &Expr, callable: ProperTailCallableId, path: &mut Vec<ProperTailPathStep>) -> Result<(), ClosureWitnessError> {
        path.push(ProperTailPathStep::Node(expr_kind(expr)));
        if let Expr::Call { name, .. } | Expr::LabeledCall { name, .. } = expr
            && let Some(checked_type) = self.typed.table().local_callee_type(expr).and_then(typeck::ty_to_ast)
        {
            self.observe_binding_type(name, checked_type);
        }
        match expr {
            Expr::Lambda { params, body, .. } => {
                let id = ProperTailCallableId { lambda_path: path.clone(), ..callable.clone() };
                self.sites.push(LambdaSite { id: id.clone(), expr_address: expr as *const Expr as usize });
                let signature = self.typed.table().type_of(expr).cloned().ok_or_else(|| ClosureWitnessError::MissingCallableType { path: path.clone() })?;
                if !matches!(signature, Ty::Fn(..)) { return Err(ClosureWitnessError::CallableTypeNotFunction { path: path.clone(), ty: signature }); }
                let declared_signature = lambda_declared_callable_type(expr, &signature)
                    .ok_or_else(|| ClosureWitnessError::CallableTypeNotFunction {
                        path: path.clone(),
                        ty: signature.clone(),
                    })?;
                let mut signature_encoding = canonical_abi::canonical_checked_ty_bytes(self.typed, &signature)
                    .map_err(|error| ClosureWitnessError::UnsupportedType { context: error.to_string() })?;
                let declared_signature_encoding = canonical_abi::canonical_ast_type_bytes(self.typed, &declared_signature)
                    .map_err(|error| ClosureWitnessError::UnsupportedType { context: error.to_string() })?;
                witchy_canonical_source::closure::put_bytes(&mut signature_encoding, &declared_signature_encoding);
                let captures = witchy_syntax::lambda_scan::scan_lambda(params, body).captures();
                let param_types = match self.typed.table().type_of(expr) {
                    Some(Ty::Fn(inputs, _, _, _, _)) => inputs.iter().filter_map(typeck::ty_to_ast).collect::<Vec<_>>(),
                    _ => Vec::new(),
                };
                let mut body_path = path.clone();
                body_path.push(ProperTailPathStep::Child(0));
                self.scopes.push(Vec::new());
                for (index, param) in params.iter().enumerate() {
                    let ty = param.ty.clone().or_else(|| param_types.get(index).cloned());
                    self.scopes.last_mut().unwrap().push(Binding { id: ClosureBindingId::new(id.clone(), vec![ClosureBindingPathStep::Parameter(index as u32)], param.name.clone()), ty: ty.clone(), borrowed: ty.as_ref().is_some_and(explicit_borrow), mutable: param.convention.binds_mutable(), unproved_qualifier: false });
                }
                self.walk_block(body, id.clone(), &mut body_path)?;
                self.scopes.pop();
                // Visit the checked body before materializing captures. A
                // destructuring binder has no separate type-table entry, but
                // its resolved variable occurrences carry checked types in
                // this exact AST. Parameter/local scopes prevent a shadowed
                // occurrence from supplying an outer binding's type.
                let mut capture_witnesses = Vec::with_capacity(captures.len());
                for name in captures {
                    let Some(binding) = self.resolve_capture(&name)?.cloned() else { continue; };
                    if binding.unproved_qualifier || binding.mutable && binding.ty.as_ref().is_some_and(canonical_abi::has_storage_qualifier) {
                        return Err(ClosureWitnessError::UnsupportedType {
                            context: format!("closure capture `{name}` has no stable checked storage qualifier"),
                        });
                    }
                    let checked_type = binding.ty.clone().ok_or_else(|| ClosureWitnessError::MissingCaptureType { name: name.clone() })?;
                    if matches!(checked_type.unqualified(), Type::Named(type_name, _) if self.definitions.contains_key(type_name)) {
                        let mut remaining = 4_096;
                        if self.source_may_contain_qualifier(&checked_type, 0, &mut remaining) {
                            return Err(ClosureWitnessError::UnsupportedType {
                                context: format!("closure capture `{name}` has an unproved qualified nominal field"),
                            });
                        }
                    }
                    if binding.borrowed || explicit_borrow(&checked_type) { return Err(ClosureWitnessError::BorrowedCapture { name }); }
                    let checked_type_encoding = canonical_abi::canonical_ast_type_bytes(self.typed, &checked_type)
                        .map_err(|error| ClosureWitnessError::UnsupportedType { context: error.to_string() })?;
                    let transfer = ClosureCaptureTransfer::classify(&checked_type, &self.definitions)
                        .ok_or_else(|| ClosureWitnessError::UnsupportedType {
                            context: format!("closure capture `{name}` of type {checked_type:?} has no checked copy and release plan"),
                        })?;
                    capture_witnesses.push(ClosureCaptureWitness::new(binding.id, checked_type, checked_type_encoding, transfer));
                }
                self.entries.push(ClosureWitness::new(
                    id,
                    typeck::ty_to_ast(&signature),
                    declared_signature,
                    signature_encoding,
                    capture_witnesses,
                ));
            }
            Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } | Expr::List(args) | Expr::Tuple(args) => for (index, arg) in args.iter().enumerate() { self.child(arg, callable.clone(), path, index)?; },
            Expr::LabeledCall { args, .. } => for (index, (_, arg)) in args.iter().enumerate() { self.child(arg, callable.clone(), path, index)?; },
            Expr::LabeledMethodCall { receiver, args, .. } => { self.child(receiver, callable.clone(), path, 0)?; for (index, (_, arg)) in args.iter().enumerate() { self.child(arg, callable.clone(), path, index + 1)?; } },
            Expr::MethodCall { receiver, args, .. } | Expr::ExistentialCall { receiver, args, .. } => { self.child(receiver, callable.clone(), path, 0)?; for (index, arg) in args.iter().enumerate() { self.child(arg, callable.clone(), path, index + 1)?; } },
            Expr::Apply { func, args } => { self.child(func, callable.clone(), path, 0)?; for (index, arg) in args.iter().enumerate() { self.child(arg, callable.clone(), path, index + 1)?; } },
            Expr::Unary { expr, .. } | Expr::Field { base: expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } | Expr::ExistentialPack { expr, .. } | Expr::ExistentialUpcast { expr, .. } => self.child(expr, callable.clone(), path, 0)?,
            Expr::RecordUpdate { base, fields, .. } => { self.child(base, callable.clone(), path, 0)?; for (index, (_, value)) in fields.iter().enumerate() { self.child(value, callable.clone(), path, index + 1)?; } },
            Expr::Record { fields, spread, .. } => { for (index, (_, value)) in fields.iter().enumerate() { self.child(value, callable.clone(), path, index)?; } if let Some(spread) = spread { self.child(spread, callable.clone(), path, fields.len())?; } },
            Expr::Binary { lhs, rhs, .. } => { self.child(lhs, callable.clone(), path, 0)?; self.child(rhs, callable.clone(), path, 1)?; },
            Expr::If { cond, then_block, else_block } => { self.child(cond, callable.clone(), path, 0)?; self.block_child(then_block, callable.clone(), path, 1)?; if let Some(block) = else_block { self.block_child(block, callable.clone(), path, 2)?; } },
            Expr::Match { scrutinee, arms } => {
                self.child(scrutinee, callable.clone(), path, 0)?;
                let (source, unproved) = self.source_type_for_expr(scrutinee);
                for (index, arm) in arms.iter().enumerate() {
                    path.push(ProperTailPathStep::Arm(u32::try_from(index).map_err(|_| ClosureWitnessError::PathOverflow)?));
                    self.scopes.push(Vec::new());
                    for (slot, (name, ty, unproved_qualifier)) in self.pattern_binding_types(&arm.pattern, source.as_ref(), unproved).into_iter().enumerate() {
                        let borrowed = ty.as_ref().is_some_and(explicit_borrow);
                        self.scopes.last_mut().unwrap().push(Binding { id: ClosureBindingId::new(callable.clone(), vec![ClosureBindingPathStep::Pattern(slot as u32)], name), ty, borrowed, mutable: false, unproved_qualifier });
                    }
                    if let Some(guard) = &arm.guard { path.push(ProperTailPathStep::Guard); self.walk_expr(guard, callable.clone(), path)?; path.pop(); }
                    path.push(ProperTailPathStep::Value); self.walk_expr(&arm.body, callable.clone(), path)?; path.pop();
                    self.scopes.pop();
                    path.pop();
                }
            },
            Expr::Block(block) => self.walk_block(block, callable.clone(), path)?,
            Expr::While { cond, body } => { self.child(cond, callable.clone(), path, 0)?; self.block_child(body, callable.clone(), path, 1)?; },
            Expr::For { var, iter, body } => {
                self.child(iter, callable.clone(), path, 0)?;
                path.push(ProperTailPathStep::Child(1));
                self.scopes.push(vec![Binding { id: ClosureBindingId::new(callable.clone(), vec![ClosureBindingPathStep::LoopVariable], var.clone()), ty: None, borrowed: false, mutable: false, unproved_qualifier: false }]);
                self.walk_block(body, callable.clone(), path)?;
                self.scopes.pop();
                path.pop();
            },
            Expr::WhileLet { pattern, scrutinee, body } => {
                self.child(scrutinee, callable.clone(), path, 0)?;
                let (source, unproved) = self.source_type_for_expr(scrutinee);
                path.push(ProperTailPathStep::Child(1));
                let bindings = self.pattern_binding_types(pattern, source.as_ref(), unproved).into_iter().enumerate().map(|(slot, (name, ty, unproved_qualifier))| {
                    let borrowed = ty.as_ref().is_some_and(explicit_borrow);
                    Binding { id: ClosureBindingId::new(callable.clone(), vec![ClosureBindingPathStep::Pattern(slot as u32)], name), ty, borrowed, mutable: false, unproved_qualifier }
                }).collect();
                self.scopes.push(bindings);
                self.walk_block(body, callable.clone(), path)?;
                self.scopes.pop();
                path.pop();
            },
            Expr::Range { lo, hi, .. } | Expr::Index { base: lo, index: hi } => { self.child(lo, callable.clone(), path, 0)?; self.child(hi, callable, path, 1)?; },
            Expr::Var(name) => {
                if let (Some(checked_type), false) = self.source_type_for_expr(expr) {
                    self.observe_binding_type(name, checked_type);
                }
            }
            Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_) | Expr::Bool(_) | Expr::TaggedLit { .. } => {}
        }
        path.pop();
        Ok(())
    }

    fn child(&mut self, expr: &Expr, callable: ProperTailCallableId, path: &mut Vec<ProperTailPathStep>, index: usize) -> Result<(), ClosureWitnessError> {
        path.push(ProperTailPathStep::Child(u32::try_from(index).map_err(|_| ClosureWitnessError::PathOverflow)?));
        self.walk_expr(expr, callable, path)?;
        path.pop();
        Ok(())
    }

    fn block_child(&mut self, block: &Block, callable: ProperTailCallableId, path: &mut Vec<ProperTailPathStep>, index: usize) -> Result<(), ClosureWitnessError> {
        path.push(ProperTailPathStep::Child(u32::try_from(index).map_err(|_| ClosureWitnessError::PathOverflow)?));
        self.walk_block(block, callable, path)?;
        path.pop();
        Ok(())
    }
}

pub fn derive_closure_witness(typed: &TypedModule) -> Result<ClosureWitnessManifest, ClosureWitnessError> {
    let builder = walk_module(typed)?;
    let mut entries = builder.entries;
    entries.sort_by(|left, right| left.id().cmp(right.id()));
    for pair in entries.windows(2) { if pair[0].id() == pair[1].id() { return Err(ClosureWitnessError::DuplicateLambda); } }
    Ok(ClosureWitnessManifest::new(entries))
}

/// Every lambda of the checked module with its structural identity, in the
/// order the closure walker meets them.
pub fn lambda_sites(typed: &TypedModule) -> Result<Vec<LambdaSite>, ClosureWitnessError> {
    Ok(walk_module(typed)?.sites)
}

fn walk_module(typed: &TypedModule) -> Result<Builder<'_>, ClosureWitnessError> {
    let mut builder = Builder {
        typed, module: typed.module(),
        definitions: witchy_canonical_source::aggregates::type_definitions(typed.module()),
        entries: Vec::new(), scopes: Vec::new(), sites: Vec::new(),
    };
    let scope = witchy_canonical_source::scope::canonical_scope(typed.module());
    for (item_index, item_def) in typed.module().items.iter().enumerate() {
        let item = u32::try_from(item_index).map_err(|_| ClosureWitnessError::PathOverflow)?;
        match item_def {
            Item::Function(function) => {
                // Entry-module generic templates remain in canonical scope to
                // preserve item ordinals, but have no executable lambda type.
                // Witness only concrete runtime callables, as layouts and
                // lowering do. Keep original item indices for identity.
                if !scope.contains(&function.name)
                    || witchy_canonical_source::scope::function_has_free_signature_variables(function)
                { continue; }
                let callable = ProperTailCallableId { item, method: None, lambda_path: Vec::new() };
                let param_types = typed.table().function_type(&function.name).and_then(|ty| match ty { Type::Fn(inputs, _, _, _) => Some(inputs), _ => None }).unwrap_or_default();
                builder.walk_callable(&function.body, callable, &function.params, &param_types)?;
            }
            Item::Impl(definition) => {
            for (method_index, function) in definition.methods.iter().enumerate() {
                let callable = ProperTailCallableId { item, method: Some(u32::try_from(method_index).map_err(|_| ClosureWitnessError::PathOverflow)?), lambda_path: Vec::new() };
                builder.walk_callable(&function.body, callable, &function.params, &[])?;
            }
            }
            Item::Trait(definition) => {
                for (method_index, method) in definition.methods.iter().enumerate() {
                    let Some(body) = &method.default else { continue };
                    let callable = ProperTailCallableId { item, method: Some(u32::try_from(method_index).map_err(|_| ClosureWitnessError::PathOverflow)?), lambda_path: Vec::new() };
                    builder.walk_callable(body, callable, &[], &[])?;
                }
            }
            Item::Type(_) | Item::TypeAlias { .. } | Item::Const { .. } | Item::Comptime(_) => {}
        }
    }
    Ok(builder)
}

impl TypedModule {
    pub fn canonical_closure_witness(&self) -> Result<ClosureWitnessManifest, ClosureWitnessError> {
        derive_closure_witness(self)
    }
}
