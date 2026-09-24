//! Checked-source closure witnesses for canonical SafeWIR lowering.
//!
//! Closure identity and capture information are derived from the exact typed
//! AST allocation.  This module deliberately does not parse source or infer
//! authority from names: the shared lambda scanner supplies capture order,
//! the type table supplies checked expression types, and lexical declaration
//! paths supply stable binding identities.

use std::fmt;

use witchy_syntax::ast::{self, Block, Expr, Item, Pattern, Stmt, Type};

use crate::canonical_abi;
use crate::typeck::{self, Ty, TypedModule};
use witchy_canonical_source::closure::{ClosureBindingPathStep, ClosureCaptureTransfer};
use witchy_canonical_source::proper_tail::{ProperTailCallableId, ProperTailExprKind, ProperTailPathStep};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClosureBindingId {
    owner: ProperTailCallableId,
    path: Vec<ClosureBindingPathStep>,
    name: String,
}

impl ClosureBindingId {
    pub fn owner(&self) -> &ProperTailCallableId { &self.owner }
    pub fn path(&self) -> &[ClosureBindingPathStep] { &self.path }
    pub fn name(&self) -> &str { &self.name }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosureCaptureWitness {
    binding: ClosureBindingId,
    checked_type: Type,
    checked_type_encoding: Vec<u8>,
    transfer: ClosureCaptureTransfer,
}

impl ClosureCaptureWitness {
    pub fn binding(&self) -> &ClosureBindingId { &self.binding }
    pub fn checked_type(&self) -> &Type { &self.checked_type }
    pub fn checked_type_encoding(&self) -> &[u8] { &self.checked_type_encoding }
    pub fn transfer(&self) -> ClosureCaptureTransfer { self.transfer }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosureWitness {
    id: ProperTailCallableId,
    /// The runtime type-table view used for physical closure-family layout.
    signature: Ty,
    /// The checked source declaration used to authenticate the callable ABI.
    /// `Ty` intentionally omits qualifier-only state, so a compiler-generated
    /// eta lambda must retain its declared parameter contract separately.
    declared_signature: Type,
    signature_encoding: Vec<u8>,
    captures: Vec<ClosureCaptureWitness>,
}

impl ClosureWitness {
    pub fn id(&self) -> &ProperTailCallableId { &self.id }
    pub fn signature(&self) -> &Ty { &self.signature }
    pub fn declared_signature(&self) -> &Type { &self.declared_signature }
    pub fn captures(&self) -> &[ClosureCaptureWitness] { &self.captures }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClosureWitnessManifest {
    entries: Vec<ClosureWitness>,
    source_bundle_digest: [u8; 32],
}

impl ClosureWitnessManifest {
    pub fn entries(&self) -> &[ClosureWitness] { &self.entries }

    pub fn digest(&self) -> [u8; 32] {
        let mut bytes = b"witchy.canonical-closure-witness.v1".to_vec();
        bytes.extend_from_slice(&self.source_bundle_digest);
        put_u32(&mut bytes, self.entries.len());
        for entry in &self.entries {
            encode_callable(&entry.id, &mut bytes);
            put_bytes(&mut bytes, &entry.signature_encoding);
            put_u32(&mut bytes, entry.captures.len());
            for capture in &entry.captures {
                encode_binding(&capture.binding, &mut bytes);
                put_bytes(&mut bytes, &capture.checked_type_encoding);
                bytes.push(capture.transfer as u8);
            }
        }
        *blake3::hash(&bytes).as_bytes()
    }

    pub(crate) fn bind_source_bundle_digest(mut self, digest: [u8; 32]) -> Self {
        self.source_bundle_digest = digest;
        self
    }
}

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
}

fn put_u32(bytes: &mut Vec<u8>, value: usize) {
    bytes.extend_from_slice(&u32::try_from(value).expect("validated closure manifest size").to_le_bytes());
}

fn put_bytes(bytes: &mut Vec<u8>, value: &[u8]) {
    put_u32(bytes, value.len());
    bytes.extend_from_slice(value);
}

fn put_str(bytes: &mut Vec<u8>, value: &str) {
    put_bytes(bytes, value.as_bytes());
}

fn encode_callable(callable: &ProperTailCallableId, bytes: &mut Vec<u8>) {
    put_u32(bytes, callable.item as usize);
    match callable.method { Some(method) => { bytes.push(1); put_u32(bytes, method as usize); }, None => bytes.push(0) }
    put_path(&callable.lambda_path, bytes);
}

fn encode_binding(binding: &ClosureBindingId, bytes: &mut Vec<u8>) {
    encode_callable(&binding.owner, bytes);
    put_u32(bytes, binding.path.len());
    for step in &binding.path {
        match step {
            ClosureBindingPathStep::Parameter(index) => { bytes.push(0); put_u32(bytes, *index as usize); }
            ClosureBindingPathStep::Statement(index) => { bytes.push(1); put_u32(bytes, *index as usize); }
            ClosureBindingPathStep::Pattern(index) => { bytes.push(2); put_u32(bytes, *index as usize); }
            ClosureBindingPathStep::LoopVariable => bytes.push(3),
        }
    }
    put_str(bytes, &binding.name);
}

fn put_path(path: &[ProperTailPathStep], bytes: &mut Vec<u8>) {
    put_u32(bytes, path.len());
    for step in path {
        match step {
            ProperTailPathStep::Statement(v) => { bytes.push(0); put_u32(bytes, *v as usize); }
            ProperTailPathStep::Value => bytes.push(1),
            ProperTailPathStep::ReturnValue => bytes.push(2),
            ProperTailPathStep::YieldValue => bytes.push(3),
            ProperTailPathStep::Node(v) => { bytes.push(4); bytes.push(*v as u8); }
            ProperTailPathStep::Child(v) => { bytes.push(5); put_u32(bytes, *v as usize); }
            ProperTailPathStep::Arm(v) => { bytes.push(6); put_u32(bytes, *v as usize); }
            ProperTailPathStep::Guard => bytes.push(7),
        }
    }
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

fn is_capability(ty: &Type, module: &ast::Module) -> bool {
    let Type::Named(name, _) = ty.unqualified() else { return false; };
    module.items.iter().any(|item| {
        matches!(item, Item::Type(def) if def.name == *name && def.is_capability)
    }) || witchy_cap_model::CapabilityKind::from_name(name).is_some()
}

fn capture_transfer(ty: &Type, module: &ast::Module) -> ClosureCaptureTransfer {
    // Ordinary closures capture by value, including values received by own.
    // Parameter transfer conventions do not describe closure construction.
    // A rooted view is a retained root-plus-projection carrier.  Give it a
    // distinct manifest transfer before asking whether the payload happens to
    // have an ordinary managed representation: later lowering must carry this
    // exact authenticated carrier, not reconstruct its projection.
    if matches!(ty, Type::Qualified(ast::TypeQual::View, _)) {
        return ClosureCaptureTransfer::RootedViewByValue;
    }
    if is_capability(ty, module) { return ClosureCaptureTransfer::CopyCapability; }
    if witchy_canonical_source::aggregates::is_scalar_type(ty) {
        ClosureCaptureTransfer::CopyScalar
    } else {
        ClosureCaptureTransfer::ManagedReferenceByValue
    }
}

fn type_from_expr(typed: &TypedModule, expr: &Expr) -> Option<Type> {
    typed.table().type_of(expr).and_then(typeck::ty_to_ast)
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
    entries: Vec<ClosureWitness>,
    scopes: Vec<Vec<Binding>>,
    sites: Vec<LambdaSite>,
}

impl<'a> Builder<'a> {
    fn observe_binding_type(&mut self, name: &str, checked_type: Type) {
        if let Some(binding) = self.scopes.iter_mut().rev()
            .flat_map(|scope| scope.iter_mut().rev())
            .find(|binding| binding.id.name == name)
            && binding.ty.is_none()
        {
            binding.borrowed |= explicit_borrow(&checked_type);
            binding.ty = Some(checked_type);
        }
    }

    fn resolve(&self, name: &str) -> Option<&Binding> {
        self.scopes.iter().rev().flat_map(|scope| scope.iter().rev()).find(|binding| binding.id.name == name)
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
            scope.push(Binding { id: ClosureBindingId { owner: callable.clone(), path: vec![ClosureBindingPathStep::Parameter(index as u32)], name: param.name.clone() }, ty: ty.clone(), borrowed: ty.as_ref().is_some_and(explicit_borrow) });
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
                Stmt::Let { name, ty, value, .. } => {
                    self.child(value, callable.clone(), path, 0)?;
                    let checked = ty.clone().or_else(|| type_from_expr(self.typed, value));
                    let borrowed = ty.as_ref().is_some_and(explicit_borrow)
                        || matches!(value, Expr::Unary { op: ast::UnOp::BorrowMut, .. })
                        || matches!(value, Expr::Var(source) if self.resolve(source).is_some_and(|binding| binding.borrowed));
                    self.scopes.last_mut().unwrap().push(Binding { id: ClosureBindingId { owner: callable.clone(), path: vec![ClosureBindingPathStep::Statement(index)], name: name.clone() }, ty: checked, borrowed });
                }
                Stmt::Assign { value, .. } => self.child(value, callable.clone(), path, 0)?,
                Stmt::LetPattern { pattern, value } => {
                    self.child(value, callable.clone(), path, 0)?;
                    for (slot, name) in pattern_names(pattern).into_iter().enumerate() {
                        self.scopes.last_mut().unwrap().push(Binding { id: ClosureBindingId { owner: callable.clone(), path: vec![ClosureBindingPathStep::Statement(index), ClosureBindingPathStep::Pattern(slot as u32)], name }, ty: None, borrowed: false });
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
                put_bytes(&mut signature_encoding, &declared_signature_encoding);
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
                    self.scopes.last_mut().unwrap().push(Binding { id: ClosureBindingId { owner: id.clone(), path: vec![ClosureBindingPathStep::Parameter(index as u32)], name: param.name.clone() }, ty: ty.clone(), borrowed: ty.as_ref().is_some_and(explicit_borrow) });
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
                    let checked_type = binding.ty.clone().ok_or_else(|| ClosureWitnessError::MissingCaptureType { name: name.clone() })?;
                    if binding.borrowed || explicit_borrow(&checked_type) { return Err(ClosureWitnessError::BorrowedCapture { name }); }
                    let checked_type_encoding = canonical_abi::canonical_ast_type_bytes(self.typed, &checked_type)
                        .map_err(|error| ClosureWitnessError::UnsupportedType { context: error.to_string() })?;
                    capture_witnesses.push(ClosureCaptureWitness { binding: binding.id, transfer: capture_transfer(&checked_type, self.module), checked_type, checked_type_encoding });
                }
                self.entries.push(ClosureWitness {
                    id,
                    signature,
                    declared_signature,
                    signature_encoding,
                    captures: capture_witnesses,
                });
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
                for (index, arm) in arms.iter().enumerate() {
                    path.push(ProperTailPathStep::Arm(u32::try_from(index).map_err(|_| ClosureWitnessError::PathOverflow)?));
                    self.scopes.push(Vec::new());
                    for (slot, name) in pattern_names(&arm.pattern).into_iter().enumerate() {
                        self.scopes.last_mut().unwrap().push(Binding { id: ClosureBindingId { owner: callable.clone(), path: vec![ClosureBindingPathStep::Pattern(slot as u32)], name }, ty: None, borrowed: false });
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
                self.scopes.push(vec![Binding { id: ClosureBindingId { owner: callable.clone(), path: vec![ClosureBindingPathStep::LoopVariable], name: var.clone() }, ty: None, borrowed: false }]);
                self.walk_block(body, callable.clone(), path)?;
                self.scopes.pop();
                path.pop();
            },
            Expr::WhileLet { pattern, scrutinee, body } => {
                self.child(scrutinee, callable.clone(), path, 0)?;
                path.push(ProperTailPathStep::Child(1));
                let bindings = pattern_names(pattern).into_iter().enumerate().map(|(slot, name)| Binding { id: ClosureBindingId { owner: callable.clone(), path: vec![ClosureBindingPathStep::Pattern(slot as u32)], name }, ty: None, borrowed: false }).collect();
                self.scopes.push(bindings);
                self.walk_block(body, callable.clone(), path)?;
                self.scopes.pop();
                path.pop();
            },
            Expr::Range { lo, hi, .. } | Expr::Index { base: lo, index: hi } => { self.child(lo, callable.clone(), path, 0)?; self.child(hi, callable, path, 1)?; },
            Expr::Var(name) => {
                if let Some(checked_type) = type_from_expr(self.typed, expr) {
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
    entries.sort_by(|left, right| left.id.cmp(&right.id));
    for pair in entries.windows(2) { if pair[0].id == pair[1].id { return Err(ClosureWitnessError::DuplicateLambda); } }
    Ok(ClosureWitnessManifest { entries, source_bundle_digest: [0; 32] })
}

/// Every lambda of the checked module with its structural identity, in the
/// order the closure walker meets them.
pub fn lambda_sites(typed: &TypedModule) -> Result<Vec<LambdaSite>, ClosureWitnessError> {
    Ok(walk_module(typed)?.sites)
}

fn walk_module(typed: &TypedModule) -> Result<Builder<'_>, ClosureWitnessError> {
    let mut builder = Builder { typed, module: typed.module(), entries: Vec::new(), scopes: Vec::new(), sites: Vec::new() };
    let scope = crate::pipeline::canonical_scope(typed.module());
    for (item_index, item_def) in typed.module().items.iter().enumerate() {
        let item = u32::try_from(item_index).map_err(|_| ClosureWitnessError::PathOverflow)?;
        match item_def {
            Item::Function(function) => {
                // Use the same runtime inventory as layouts, callable/tail
                // witnesses, and lowering. Unused generic library templates
                // need not have concrete lambda types. Keep original item
                // indices so admitted callable identities do not shift.
                if !scope.contains(&function.name) { continue; }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closure_witness_retains_declared_unique_var_parameter_contract() {
        let module = witchy_syntax::parser::parse_module(
            "fn main() -> Int:\n    let append = fn(var values: unique List(Int), value: Int) -> ():\n        return\n    let frozen_append = fn(var values: frozen List(Int), value: Int) -> ():\n        return\n    0\n",
        )
        .expect("parse qualifier-bearing lambda");
        let typed = typeck::annotate_checked(module).expect("check qualifier-bearing lambda");
        let witness = derive_closure_witness(&typed).expect("derive closure witness");
        let Type::Fn(parameters, result, conventions, _) = witness.entries()[0].declared_signature() else {
            panic!("closure witness must retain callable declaration");
        };
        assert!(matches!(parameters.as_slice(), [
            Type::Qualified(ast::TypeQual::Unique, inner),
            Type::Named(name, arguments),
        ] if matches!(inner.as_ref(), Type::Named(name, arguments) if name == "List" && arguments.len() == 1)
            && name == "Int" && arguments.is_empty()));
        assert_eq!(conventions.as_slice(), &[ast::Convention::Var, ast::Convention::Let]);
        assert!(
            matches!(result.as_ref(), Type::Tuple(items) if items.is_empty())
                || matches!(result.as_ref(), Type::Named(name, arguments) if (name == "Unit" || name == "Nil") && arguments.is_empty())
        );
        let Type::Fn(parameters, _, _, _) = witness.entries()[1].declared_signature() else {
            panic!("closure witness must retain frozen callable declaration");
        };
        assert!(matches!(parameters.first(), Some(Type::Qualified(ast::TypeQual::Frozen, _))));
    }

    #[test]
    fn intrinsic_calls_do_not_become_environment_captures() {
        let module = witchy_syntax::parser::parse_module(
            "fn make(xs: List(Int)) -> fn() -> Int:\n    fn(): list.length(xs)\n",
        ).expect("parse closure calling an intrinsic");
        let typed = typeck::annotate_checked(module).expect("check intrinsic signature");
        let manifest = derive_closure_witness(&typed).expect("intrinsic is not a captured binding");
        let captures = manifest.entries()[0].captures();
        assert_eq!(captures.len(), 1);
        assert_eq!(captures[0].binding().name(), "xs");

        let mut builder = walk_module(&typed).expect("checked closure");
        assert!(builder.resolve_capture("list.length").expect("catalog intrinsic").is_none());
        assert!(matches!(builder.resolve_capture("list.not_an_intrinsic"),
            Err(ClosureWitnessError::UnresolvedCapture { .. })));
        // Lexical identity wins even for a name registered by the intrinsic
        // catalog. Catalog membership never grants a local value authority.
        let mut binding = builder.entries[0].captures[0].binding.clone();
        binding.name = "list.length".into();
        builder.scopes.push(vec![Binding { id: binding, ty: None, borrowed: false }]);
        assert!(builder.resolve_capture("list.length").expect("lexical shadow").is_some());
    }

    #[test]
    fn own_parameter_capture_uses_checked_value_semantics() {
        for (ty, transfer) in [
            ("Int", ClosureCaptureTransfer::CopyScalar),
            ("()", ClosureCaptureTransfer::CopyScalar),
            ("String", ClosureCaptureTransfer::ManagedReferenceByValue),
            ("List(String)", ClosureCaptureTransfer::ManagedReferenceByValue),
        ] {
            let source = format!("fn make(own value: {ty}) -> fn() -> {ty}:\n    let captured = fn(): value\n    let retained = value\n    captured\n");
            let module = witchy_syntax::parser::parse_module(&source).expect("parse ordinary capture");
            let typed = typeck::annotate_checked(module).expect("capture does not consume its source");
            let manifest = derive_closure_witness(&typed).expect("ordinary capture is witnessed");
            assert_eq!(manifest.entries().len(), 1);
            let captures = manifest.entries()[0].captures();
            assert_eq!(captures.len(), 1);
            assert_eq!(captures[0].transfer(), transfer);
            assert_eq!(captures[0].binding().path(), &[ClosureBindingPathStep::Parameter(0)]);
        }
    }

    #[test]
    fn rooted_view_capture_has_a_distinct_source_transfer_and_digest() {
        let source = "fn first[a](items: List(a)) -> &a:\n    &items[0]\n\nfn main() -> Int:\n    var values = [7, 8]\n    let selected: &Int = first(values)\n    let read = fn() -> Int:\n        *selected\n    values = [9, 10]\n    read() * 10 + values[0]\n";
        let module = witchy_syntax::parser::parse_module(source).expect("parse rooted-view closure");
        let typed = typeck::annotate_checked(module).expect("check rooted-view closure");
        let manifest = derive_closure_witness(&typed).expect("derive rooted-view witness");
        let capture = &manifest.entries()[0].captures()[0];
        assert_eq!(capture.transfer(), ClosureCaptureTransfer::RootedViewByValue);
        assert!(matches!(capture.checked_type(), Type::Qualified(ast::TypeQual::View, _)));
        assert!(!capture.checked_type_encoding().is_empty());
        assert!(matches!(capture.binding().path(), [ClosureBindingPathStep::Statement(_)]));
        let rooted_digest = manifest.digest();

        let ordinary = witchy_syntax::parser::parse_module(
            "fn main() -> Int:\n    let value = 7\n    let read = fn() -> Int:\n        value\n    read()\n",
        ).expect("parse scalar closure");
        let ordinary = typeck::annotate_checked(ordinary).expect("check scalar closure");
        let ordinary = derive_closure_witness(&ordinary).expect("derive scalar witness");
        assert_ne!(rooted_digest, ordinary.digest(), "capture transfer participates in the manifest digest");
    }

    #[test]
    fn own_parameter_capture_preserves_excluded_capture_boundaries() {
        for (source, diagnostic) in [
            ("fn make(own value: once fn() -> Int) -> fn() -> Int:\n    fn(): value()\n", "captures affine once-callable"),
            ("must type Ticket:\n    Ticket(Int)\n\nfn make() -> fn() -> Ticket:\n    let value = Ticket(1)\n    fn(): value\n", "closure environment carries must-consume"),
        ] {
            let module = witchy_syntax::parser::parse_module(source).expect("parse excluded capture");
            let error = typeck::annotate_checked(module).expect_err("excluded captures remain rejected");
            assert!(error.message.contains(diagnostic), "{error:?}");
        }
        let module = witchy_syntax::parser::parse_module(
            "fn make(own console: Console) -> fn() -> Console:\n    fn(): console\n",
        ).expect("parse capability capture");
        let typed = typeck::annotate_checked(module).expect("ordinary source capability capture checks");
        let manifest = derive_closure_witness(&typed).expect("capability stays explicitly classified");
        assert_eq!(manifest.entries()[0].captures()[0].transfer(), ClosureCaptureTransfer::CopyCapability);
        // Borrow rejection is independent of transfer classification.
        let module = witchy_syntax::parser::parse_module(
            "fn make(value: String) -> fn() -> String:\n    fn(): value\n",
        ).expect("parse lexical capture");
        let typed = typeck::annotate_checked(module).expect("check lexical capture");
        let mut builder = walk_module(&typed).expect("ordinary witness");
        let capture = builder.entries[0].captures[0].clone();
        let borrowed = Binding { id: capture.binding, ty: Some(capture.checked_type), borrowed: true };
        builder.entries.clear();
        builder.sites.clear();
        builder.scopes = vec![vec![borrowed]];
        let Item::Function(function) = &typed.module().items[0] else { panic!("function") };
        let Stmt::Expr(lambda) = &function.body.stmts[0] else { panic!("lambda") };
        let error = builder.walk_expr(lambda, ProperTailCallableId { item: 0, method: None, lambda_path: Vec::new() }, &mut Vec::new())
            .expect_err("borrowed lexical capture is still refused");
        assert!(matches!(error, ClosureWitnessError::BorrowedCapture { .. }));
    }

    #[test]
    fn pattern_capture_types_come_from_checked_lexical_occurrences() {
        let module = witchy_syntax::parser::parse_module(
            "fn make(pair: (Int, String)) -> fn(Int) -> String:\n    let (number, text) = pair\n    fn(number: Int): text\n",
        ).expect("parse pattern capture with a shadowed parameter");
        let typed = typeck::annotate_checked(module).expect("type check pattern capture");
        let witness = derive_closure_witness(&typed).expect("checked occurrence supplies capture type");
        assert_eq!(witness.entries().len(), 1);
        let captures = witness.entries()[0].captures();
        assert_eq!(captures.len(), 1);
        assert_eq!(captures[0].binding().name(), "text");
        assert_eq!(captures[0].checked_type(), &Type::Named("String".into(), Vec::new()));
        assert_eq!(captures[0].transfer(), ClosureCaptureTransfer::ManagedReferenceByValue);
    }

    #[test]
    fn pattern_callable_capture_uses_the_checked_local_call_not_a_global_name() {
        let module = witchy_syntax::parser::parse_module(
            "fn cont(value: String) -> String:\n    value\n\ntype Handler:\n    Handler(fn(Int) -> Int)\n\nfn make(handler: Handler) -> fn(Int) -> Int:\n    match handler:\n        Handler(cont) -> fn(value: Int): cont(value)\n",
        ).expect("parse callable pattern capture");
        let typed = typeck::annotate_checked(module).expect("checked local call");
        let witness = derive_closure_witness(&typed).expect("callee-only occurrence supplies the capture type");
        let captures = witness.entries()[0].captures();
        assert_eq!(captures.len(), 1);
        assert_eq!(captures[0].binding().name(), "cont");
        let Type::Fn(parameters, result, ..) = captures[0].checked_type() else { panic!("callable capture") };
        assert_eq!(parameters, &[Type::Named("Int".into(), Vec::new())]);
        assert_eq!(result.as_ref(), &Type::Named("Int".into(), Vec::new()));
        assert_eq!(captures[0].transfer(), ClosureCaptureTransfer::ManagedReferenceByValue);
        let unrelated = Expr::Call { name: "cont".into(), args: vec![Expr::Int(1)] };
        assert!(typed.table().local_callee_type(&unrelated).is_none(), "a matching spelling is not a checked node");
    }

    #[test]
    fn closure_witness_uses_the_canonical_runtime_scope() {
        let mut module = witchy_syntax::parser::parse_module(
            "fn helper(value: a) -> fn(a) -> a:\n    fn(inner: a): inner\n\nfn main() -> Int:\n    1\n",
        ).expect("parse unused generic library lambda");
        let Item::Function(helper) = &mut module.items[0] else { panic!("helper") };
        helper.name = "semver.helper".into();
        module.linked_entry = Some("app".into());
        let typed = typeck::annotate_checked(module).expect("type check generic template");
        assert!(derive_closure_witness(&typed).expect("unused template is outside runtime scope").entries().is_empty());

        let mut module = witchy_syntax::parser::parse_module(
            "fn helper(value: Int) -> fn(Int) -> Int:\n    fn(inner: Int): value + inner\n\nfn main() -> Int:\n    let f = helper(1)\n    f(2)\n",
        ).expect("parse reachable library lambda");
        let Item::Function(helper) = &mut module.items[0] else { panic!("helper") };
        helper.name = "semver.helper".into();
        let Item::Function(main) = &mut module.items[1] else { panic!("main") };
        let Stmt::Let { value: Expr::Call { name, .. }, .. } = &mut main.body.stmts[0] else { panic!("linked call") };
        *name = "semver.helper".into();
        module.linked_entry = Some("app".into());
        let typed = typeck::annotate_checked(module).expect("type check reachable lambda");
        let witness = derive_closure_witness(&typed).expect("reachable lambda remains witnessed");
        assert_eq!(witness.entries().len(), 1);
        assert_eq!(witness.entries()[0].id().item, 0);
        assert_eq!(witness.entries()[0].captures().len(), 1);
        assert_eq!(witness.entries()[0].captures()[0].binding().name(), "value");
    }

    #[test]
    fn unresolved_capture_is_rejected_but_checked_module_reference_is_not_a_capture() {
        let module = witchy_syntax::parser::parse_module(
            "fn apply(value: Int) -> Int:\n    value\n\nfn main():\n    apply(1)\n",
        )
        .expect("parse");
        let typed = typeck::annotate_checked(module).expect("type check");
        let builder = Builder {
            typed: &typed,
            module: typed.module(),
            entries: Vec::new(),
            scopes: Vec::new(), sites: Vec::new(),
        };

        assert!(builder
            .resolve_capture("apply")
            .expect("authenticated module reference")
            .is_none());
        // A capability operation is a frontend-owned intrinsic, not a capture:
        // `__capop.print` inside an async lambda references no environment.
        assert!(builder
            .resolve_capture("__capop.print")
            .expect("capability operations are authenticated module references")
            .is_none());
        assert!(matches!(
            builder.resolve_capture("__capop.not_an_operation"),
            Err(ClosureWitnessError::UnresolvedCapture { name })
                if name == "__capop.not_an_operation"
        ));
        assert!(matches!(
            builder.resolve_capture("missing_lexical_binding"),
            Err(ClosureWitnessError::UnresolvedCapture { name })
                if name == "missing_lexical_binding"
        ));
    }
}
