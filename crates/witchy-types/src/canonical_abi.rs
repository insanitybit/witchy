//! Canonical source-level callable ABI identities.
//!
//! This module deliberately knows nothing about WIR.  It projects the final
//! checked type representation into a deterministic, tagged description that
//! can be bound to a later candidate by a consumer.  Function names and
//! backend-assigned IDs are never part of the projection.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use witchy_syntax::ast::{self, Convention, Item, Type};

use crate::typeck::{Ty, TypedModule};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalScalarKind {
    Int,
    Float,
    Duration,
    Bool,
    Unit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalCapabilityKind {
    Console,
    Clock,
    Rand,
    Env,
    Secret,
    Exec,
    Fetch,
    Dir,
    File,
    Net,
    Socket,
    Listener,
    BuildOut,
    BuildRead,
    BuildEnv,
    BuildNet,
    BuildExec,
}

/// A structural identity for a checked source type used by a reference or
/// aggregate ABI entry.  It is not a WIR layout identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalTypeKey([u8; 32]);

impl CanonicalTypeKey {
    pub fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalReferenceKind {
    String,
    Bytes,
    Aggregate(CanonicalTypeKey),
    Capability(CanonicalCapabilityKind),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalValueKind {
    Scalar(CanonicalScalarKind),
    Reference(CanonicalReferenceKind),
    Callable(CanonicalCallableAbiKey),
    Value(CanonicalTypeKey),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalParameterConvention {
    Let,
    Borrow,
    Var,
    Own,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalParameterAbi {
    pub convention: CanonicalParameterConvention,
    pub value: CanonicalValueKind,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalResultKind {
    Scalar(CanonicalScalarKind),
    Reference(CanonicalReferenceKind),
    Value(CanonicalValueKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalCallableQualifiers {
    pub pure: bool,
    pub once: bool,
}

/// A source-structural local target.  The ordinal is assigned only from the
/// final checked function sequence, never from a function spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalLocalTarget {
    pub function_ordinal: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalTargetPolicy {
    ClosedLocal(CanonicalLocalTarget),
    OpenAbi,
}

/// Hash identity of a complete source-level callable ABI shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalCallableAbiKey([u8; 32]);

impl CanonicalCallableAbiKey {
    pub fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CanonicalCallableAbi {
    pub key: CanonicalCallableAbiKey,
    pub parameters: Vec<CanonicalParameterAbi>,
    pub result: CanonicalResultKind,
    pub qualifiers: CanonicalCallableQualifiers,
    pub target_policy: CanonicalTargetPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalCallableAbiManifest {
    entries: Vec<CanonicalCallableAbi>,
    digest: [u8; 32],
}

impl CanonicalCallableAbiManifest {
    pub fn entries(&self) -> &[CanonicalCallableAbi] {
        &self.entries
    }

    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalAbiError {
    NonConcrete(String),
    Unsupported(String),
    InvalidCallable(String),
}

impl fmt::Display for CanonicalAbiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonConcrete(context) => write!(f, "canonical callable ABI is not concrete: {context}"),
            Self::Unsupported(context) => write!(f, "canonical callable ABI has unsupported type: {context}"),
            Self::InvalidCallable(context) => write!(f, "canonical callable ABI is invalid: {context}"),
        }
    }
}

impl std::error::Error for CanonicalAbiError {}

struct Context {
    nominal_ordinals: BTreeMap<String, u32>,
}

impl Context {
    fn from_module(module: &ast::Module) -> Self {
        let nominal_ordinals = module
            .items
            .iter()
            .enumerate()
            .filter_map(|(ordinal, item)| match item {
                Item::Type(definition) => Some((definition.name.clone(), ordinal as u32)),
                Item::TypeAlias { name, .. } => Some((name.clone(), ordinal as u32)),
                _ => None,
            })
            .collect();
        Self { nominal_ordinals }
    }

    fn nominal(&self, name: &str) -> Result<u32, CanonicalAbiError> {
        self.nominal_ordinals.get(name).copied().ok_or_else(|| {
            CanonicalAbiError::Unsupported(format!("unresolved nominal type `{name}`"))
        })
    }
}

/// Derive the complete callable ABI manifest from the final checked module.
/// Declarations contribute closed local targets. Concrete expression types
/// contribute open ABI entries, including nested function types.
pub fn derive_manifest(typed: &TypedModule) -> Result<CanonicalCallableAbiManifest, CanonicalAbiError> {
    let context = Context::from_module(typed.module());
    let mut entries = BTreeSet::new();
    let mut function_ordinal = 0u32;
    for item in &typed.module().items {
        let Item::Function(function) = item else { continue };
        let signature = typed.table().function_type(&function.name).ok_or_else(|| {
            CanonicalAbiError::InvalidCallable(format!("checked declaration at ordinal {function_ordinal} has no finalized signature"))
        })?;
        let entry = callable_from_ast(
            &signature,
            CanonicalTargetPolicy::ClosedLocal(CanonicalLocalTarget { function_ordinal }),
            &context,
        )?;
        entries.insert(entry);
        function_ordinal = function_ordinal.checked_add(1).ok_or_else(|| {
            CanonicalAbiError::InvalidCallable("function ordinal overflow".into())
        })?;
    }
    for ty in typed.table().concrete_types() {
        collect_ty_callables(ty, CanonicalTargetPolicy::OpenAbi, &context, &mut entries)?;
    }
    let entries = entries.into_iter().collect::<Vec<_>>();
    let digest = manifest_digest(&entries);
    Ok(CanonicalCallableAbiManifest { entries, digest })
}

fn collect_ty_callables(
    ty: &Ty,
    policy: CanonicalTargetPolicy,
    context: &Context,
    entries: &mut BTreeSet<CanonicalCallableAbi>,
) -> Result<(), CanonicalAbiError> {
    match ty {
        Ty::Fn(..) => {
            entries.insert(callable_from_ty(ty, policy, context)?);
        }
        Ty::List(element) => collect_ty_callables(element, CanonicalTargetPolicy::OpenAbi, context, entries)?,
        Ty::Tuple(items) | Ty::Named(_, items) | Ty::Dyn(_, items) => {
            for item in items.iter() {
                collect_ty_callables(item, CanonicalTargetPolicy::OpenAbi, context, entries)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn callable_from_ty(
    ty: &Ty,
    target_policy: CanonicalTargetPolicy,
    context: &Context,
) -> Result<CanonicalCallableAbi, CanonicalAbiError> {
    let Ty::Fn(params, result, conventions, _, qualifiers) = ty else {
        return Err(CanonicalAbiError::InvalidCallable("expected function type".into()));
    };
    if !conventions.is_empty() && conventions.len() != params.len() {
        return Err(CanonicalAbiError::InvalidCallable("parameter convention arity does not match parameter type arity".into()));
    }
    let parameters = params
        .iter()
        .enumerate()
        .map(|(index, ty)| {
            Ok(CanonicalParameterAbi {
                convention: convention(conventions.get(index).copied().unwrap_or_default()),
                value: value_from_ty(ty, context)?,
            })
        })
        .collect::<Result<Vec<_>, CanonicalAbiError>>()?;
    let result = result_from_value(value_from_ty(result, context)?)?;
    let qualifiers = CanonicalCallableQualifiers { pure: qualifiers.pure, once: qualifiers.once };
    let bytes = encode_callable_parts(&parameters, &result, qualifiers);
    Ok(CanonicalCallableAbi {
        key: CanonicalCallableAbiKey(*blake3::hash(&bytes).as_bytes()),
        parameters,
        result,
        qualifiers,
        target_policy,
    })
}

fn callable_from_ast(
    ty: &Type,
    target_policy: CanonicalTargetPolicy,
    context: &Context,
) -> Result<CanonicalCallableAbi, CanonicalAbiError> {
    let Type::Fn(params, result, conventions, qualifiers) = ty.unqualified() else {
        return Err(CanonicalAbiError::InvalidCallable("finalized declaration is not a function type".into()));
    };
    if !conventions.is_empty() && conventions.len() != params.len() {
        return Err(CanonicalAbiError::InvalidCallable("parameter convention arity does not match parameter type arity".into()));
    }
    let parameters = params
        .iter()
        .enumerate()
        .map(|(index, ty)| {
            Ok(CanonicalParameterAbi {
                convention: convention(conventions.get(index).copied().unwrap_or_default()),
                value: value_from_ast(ty, context)?,
            })
        })
        .collect::<Result<Vec<_>, CanonicalAbiError>>()?;
    let result = result_from_value(value_from_ast(result, context)?)?;
    let qualifiers = CanonicalCallableQualifiers { pure: qualifiers.pure, once: qualifiers.once };
    let bytes = encode_callable_parts(&parameters, &result, qualifiers);
    Ok(CanonicalCallableAbi {
        key: CanonicalCallableAbiKey(*blake3::hash(&bytes).as_bytes()),
        parameters,
        result,
        qualifiers,
        target_policy,
    })
}

fn result_from_value(value: CanonicalValueKind) -> Result<CanonicalResultKind, CanonicalAbiError> {
    match value {
        CanonicalValueKind::Scalar(kind) => Ok(CanonicalResultKind::Scalar(kind)),
        CanonicalValueKind::Reference(kind) => Ok(CanonicalResultKind::Reference(kind)),
        other @ (CanonicalValueKind::Callable(_) | CanonicalValueKind::Value(_)) => {
            Ok(CanonicalResultKind::Value(other))
        }
    }
}

fn convention(convention: Convention) -> CanonicalParameterConvention {
    match convention {
        Convention::Let => CanonicalParameterConvention::Let,
        Convention::Borrow => CanonicalParameterConvention::Borrow,
        Convention::Var => CanonicalParameterConvention::Var,
        Convention::Own => CanonicalParameterConvention::Own,
    }
}

fn value_from_ty(ty: &Ty, context: &Context) -> Result<CanonicalValueKind, CanonicalAbiError> {
    let mut bytes = Vec::new();
    encode_ty(ty, context, &mut bytes)?;
    let key = CanonicalTypeKey(*blake3::hash(&bytes).as_bytes());
    Ok(match ty {
        Ty::Int => CanonicalValueKind::Scalar(CanonicalScalarKind::Int),
        Ty::Float => CanonicalValueKind::Scalar(CanonicalScalarKind::Float),
        Ty::Duration => CanonicalValueKind::Scalar(CanonicalScalarKind::Duration),
        Ty::Bool => CanonicalValueKind::Scalar(CanonicalScalarKind::Bool),
        Ty::Unit => CanonicalValueKind::Scalar(CanonicalScalarKind::Unit),
        Ty::String => CanonicalValueKind::Reference(CanonicalReferenceKind::String),
        Ty::Bytes => CanonicalValueKind::Reference(CanonicalReferenceKind::Bytes),
        Ty::Fn(..) => CanonicalValueKind::Callable(CanonicalCallableAbiKey(*blake3::hash(&bytes).as_bytes())),
        Ty::Console(_) | Ty::Clock | Ty::Rand | Ty::Env | Ty::Secret(_) | Ty::Exec | Ty::Fetch
        | Ty::Dir(_) | Ty::File(_) | Ty::Net(_) | Ty::Socket | Ty::Listener | Ty::BuildOut
        | Ty::BuildRead | Ty::BuildEnv | Ty::BuildNet | Ty::BuildExec => {
            CanonicalValueKind::Reference(CanonicalReferenceKind::Capability(capability_kind(ty)))
        }
        Ty::List(_) | Ty::Named(_, _) | Ty::Dyn(_, _) => {
            CanonicalValueKind::Reference(CanonicalReferenceKind::Aggregate(key))
        }
        Ty::Tuple(_) => CanonicalValueKind::Value(key),
        Ty::Var(id) => return Err(CanonicalAbiError::NonConcrete(format!("type variable {id}"))),
        Ty::Msg => CanonicalValueKind::Value(key),
    })
}

fn value_from_ast(ty: &Type, context: &Context) -> Result<CanonicalValueKind, CanonicalAbiError> {
    let mut bytes = Vec::new();
    encode_ast(ty, context, &mut bytes)?;
    let key = CanonicalTypeKey(*blake3::hash(&bytes).as_bytes());
    let ty = ty.unqualified();
    Ok(match ty {
        Type::Named(name, args) if name == "Int" && args.is_empty() => CanonicalValueKind::Scalar(CanonicalScalarKind::Int),
        Type::Named(name, args) if name == "Float" && args.is_empty() => CanonicalValueKind::Scalar(CanonicalScalarKind::Float),
        Type::Named(name, args) if name == "Duration" && args.is_empty() => CanonicalValueKind::Scalar(CanonicalScalarKind::Duration),
        Type::Named(name, args) if name == "Bool" && args.is_empty() => CanonicalValueKind::Scalar(CanonicalScalarKind::Bool),
        Type::Named(name, args) if (name == "Nil" || name == "Unit") && args.is_empty() => CanonicalValueKind::Scalar(CanonicalScalarKind::Unit),
        Type::Named(name, args) if name == "String" && args.is_empty() => CanonicalValueKind::Reference(CanonicalReferenceKind::String),
        Type::Named(name, args) if name == "Bytes" && args.is_empty() => CanonicalValueKind::Reference(CanonicalReferenceKind::Bytes),
        Type::Fn(..) => CanonicalValueKind::Callable(CanonicalCallableAbiKey(*blake3::hash(&bytes).as_bytes())),
        Type::Named(name, _) if is_capability_name(name) => CanonicalValueKind::Reference(CanonicalReferenceKind::Capability(capability_name(name))),
        Type::Named(_, _) | Type::Tuple(_) | Type::Slice(_) | Type::Dyn(_, _) => CanonicalValueKind::Reference(CanonicalReferenceKind::Aggregate(key)),
        Type::RecordCompose { .. } => return Err(CanonicalAbiError::Unsupported("record composition survived type checking".into())),
        Type::Qualified(_, _) => unreachable!("Type::unqualified removes qualifiers"),
    })
}

fn encode_callable_parts(parameters: &[CanonicalParameterAbi], result: &CanonicalResultKind, qualifiers: CanonicalCallableQualifiers) -> Vec<u8> {
    let mut bytes = b"witchy.canonical-callable-abi.v1".to_vec();
    put_len(&mut bytes, parameters.len());
    for parameter in parameters {
        bytes.push(match parameter.convention { CanonicalParameterConvention::Let => 0, CanonicalParameterConvention::Borrow => 1, CanonicalParameterConvention::Var => 2, CanonicalParameterConvention::Own => 3 });
        encode_value(&mut bytes, &parameter.value);
    }
    encode_result(&mut bytes, result);
    bytes.push(u8::from(qualifiers.pure));
    bytes.push(u8::from(qualifiers.once));
    bytes
}

fn encode_value(bytes: &mut Vec<u8>, value: &CanonicalValueKind) {
    match value {
        CanonicalValueKind::Scalar(kind) => { bytes.push(0); bytes.push(*kind as u8); }
        CanonicalValueKind::Reference(kind) => { bytes.push(1); encode_reference(bytes, kind); }
        CanonicalValueKind::Callable(key) => { bytes.push(2); bytes.extend_from_slice(&key.0); }
        CanonicalValueKind::Value(key) => { bytes.push(3); bytes.extend_from_slice(&key.0); }
    }
}

fn encode_result(bytes: &mut Vec<u8>, result: &CanonicalResultKind) {
    match result {
        CanonicalResultKind::Scalar(kind) => { bytes.push(0); bytes.push(*kind as u8); }
        CanonicalResultKind::Reference(kind) => { bytes.push(1); encode_reference(bytes, kind); }
        CanonicalResultKind::Value(value) => { bytes.push(2); encode_value(bytes, value); }
    }
}

fn encode_reference(bytes: &mut Vec<u8>, kind: &CanonicalReferenceKind) {
    match kind {
        CanonicalReferenceKind::String => bytes.push(0),
        CanonicalReferenceKind::Bytes => bytes.push(1),
        CanonicalReferenceKind::Aggregate(key) => { bytes.push(2); bytes.extend_from_slice(&key.0); }
        CanonicalReferenceKind::Capability(kind) => { bytes.push(3); bytes.push(*kind as u8); }
    }
}

fn manifest_digest(entries: &[CanonicalCallableAbi]) -> [u8; 32] {
    let mut bytes = b"witchy.canonical-callable-abi-manifest.v1".to_vec();
    put_len(&mut bytes, entries.len());
    for entry in entries {
        bytes.extend_from_slice(&entry.key.0);
        let policy = match entry.target_policy { CanonicalTargetPolicy::ClosedLocal(target) => { bytes.push(0); target.function_ordinal } CanonicalTargetPolicy::OpenAbi => { bytes.push(1); 0 } };
        bytes.extend_from_slice(&policy.to_le_bytes());
    }
    *blake3::hash(&bytes).as_bytes()
}

fn put_len(bytes: &mut Vec<u8>, length: usize) {
    bytes.extend_from_slice(&(u32::try_from(length).expect("validated canonical ABI length")).to_le_bytes());
}

fn encode_ty(ty: &Ty, context: &Context, bytes: &mut Vec<u8>) -> Result<(), CanonicalAbiError> {
    match ty {
        Ty::Int => bytes.push(0), Ty::Float => bytes.push(1), Ty::Duration => bytes.push(2), Ty::String => bytes.push(3), Ty::Bytes => bytes.push(4), Ty::Msg => bytes.push(5), Ty::Bool => bytes.push(6), Ty::Unit => bytes.push(7),
        Ty::Console(rights) => { bytes.push(8); bytes.push(u8::from(rights.read)); bytes.push(u8::from(rights.write)); }
        Ty::Clock => bytes.push(9), Ty::Rand => bytes.push(10), Ty::Env => bytes.push(11),
        Ty::Secret(rights) => { bytes.push(12); bytes.push(u8::from(rights.reveal)); bytes.push(u8::from(rights.seal)); }
        Ty::Exec => bytes.push(13), Ty::Fetch => bytes.push(14),
        Ty::Dir(rights) => { bytes.push(15); bytes.push(u8::from(rights.read)); bytes.push(u8::from(rights.write)); }
        Ty::File(rights) => { bytes.push(16); bytes.push(u8::from(rights.read)); bytes.push(u8::from(rights.write)); }
        Ty::Net(rights) => { bytes.push(17); bytes.push(u8::from(rights.connect)); bytes.push(u8::from(rights.listen)); bytes.push(u8::from(rights.tcp)); bytes.push(u8::from(rights.udp)); bytes.push(u8::from(rights.uds)); }
        Ty::Socket => bytes.push(18), Ty::Listener => bytes.push(19), Ty::BuildOut => bytes.push(20), Ty::BuildRead => bytes.push(21), Ty::BuildEnv => bytes.push(22), Ty::BuildNet => bytes.push(23), Ty::BuildExec => bytes.push(24),
        Ty::List(element) => { bytes.push(25); encode_ty(element, context, bytes)?; }
        Ty::Tuple(items) => { bytes.push(26); put_len(bytes, items.len()); for item in items.iter() { encode_ty(item, context, bytes)?; } }
        Ty::Named(name, args) => { bytes.push(27); bytes.extend_from_slice(&context.nominal(name)?.to_le_bytes()); put_len(bytes, args.len()); for arg in args.iter() { encode_ty(arg, context, bytes)?; } }
        Ty::Dyn(name, args) => { bytes.push(28); bytes.extend_from_slice(&context.nominal(name)?.to_le_bytes()); put_len(bytes, args.len()); for arg in args.iter() { encode_ty(arg, context, bytes)?; } }
        Ty::Fn(params, result, conventions, mutable, qualifiers) => { bytes.push(29); put_len(bytes, params.len()); for (index, param) in params.iter().enumerate() { bytes.push(convention(conventions.get(index).copied().unwrap_or_default()) as u8); bytes.push(u8::from(mutable.get(index).copied().unwrap_or(false))); encode_ty(param, context, bytes)?; } encode_ty(result, context, bytes)?; bytes.push(u8::from(qualifiers.pure)); bytes.push(u8::from(qualifiers.once)); }
        Ty::Var(id) => return Err(CanonicalAbiError::NonConcrete(format!("type variable {id}"))),
    }
    Ok(())
}

fn encode_ast(ty: &Type, context: &Context, bytes: &mut Vec<u8>) -> Result<(), CanonicalAbiError> {
    match ty.unqualified() {
        Type::Named(name, args) => { bytes.push(0); let tag = match (name.as_str(), args.is_empty()) { ("Int", true) => 0, ("Float", true) => 1, ("Duration", true) => 2, ("String", true) => 3, ("Bytes", true) => 4, ("Bool", true) => 5, ("Nil", true) | ("Unit", true) => 6, _ => context.nominal(name)? + 32 }; bytes.extend_from_slice(&tag.to_le_bytes()); put_len(bytes, args.len()); for arg in args { encode_ast(arg, context, bytes)?; } }
        Type::Tuple(items) => { bytes.push(1); put_len(bytes, items.len()); for item in items { encode_ast(item, context, bytes)?; } }
        Type::Slice(item) => { bytes.push(2); encode_ast(item, context, bytes)?; }
        Type::RecordCompose { .. } => return Err(CanonicalAbiError::Unsupported("record composition survived type checking".into())),
        Type::Fn(params, result, conventions, qualifiers) => { bytes.push(3); put_len(bytes, params.len()); for (index, param) in params.iter().enumerate() { bytes.push(convention(conventions.get(index).copied().unwrap_or_default()) as u8); encode_ast(param, context, bytes)?; } encode_ast(result, context, bytes)?; bytes.push(u8::from(qualifiers.pure)); bytes.push(u8::from(qualifiers.once)); }
        Type::Dyn(name, args) => { bytes.push(4); bytes.extend_from_slice(&(context.nominal(name)?).to_le_bytes()); put_len(bytes, args.len()); for arg in args { encode_ast(arg, context, bytes)?; } }
        Type::Qualified(_, _) => unreachable!("Type::unqualified removes qualifiers"),
    }
    Ok(())
}

fn capability_kind(ty: &Ty) -> CanonicalCapabilityKind {
    match ty { Ty::Console(_) => CanonicalCapabilityKind::Console, Ty::Clock => CanonicalCapabilityKind::Clock, Ty::Rand => CanonicalCapabilityKind::Rand, Ty::Env => CanonicalCapabilityKind::Env, Ty::Secret(_) => CanonicalCapabilityKind::Secret, Ty::Exec => CanonicalCapabilityKind::Exec, Ty::Fetch => CanonicalCapabilityKind::Fetch, Ty::Dir(_) => CanonicalCapabilityKind::Dir, Ty::File(_) => CanonicalCapabilityKind::File, Ty::Net(_) => CanonicalCapabilityKind::Net, Ty::Socket => CanonicalCapabilityKind::Socket, Ty::Listener => CanonicalCapabilityKind::Listener, Ty::BuildOut => CanonicalCapabilityKind::BuildOut, Ty::BuildRead => CanonicalCapabilityKind::BuildRead, Ty::BuildEnv => CanonicalCapabilityKind::BuildEnv, Ty::BuildNet => CanonicalCapabilityKind::BuildNet, Ty::BuildExec => CanonicalCapabilityKind::BuildExec, _ => unreachable!("not a capability") }
}

fn is_capability_name(name: &str) -> bool { matches!(name, "Console" | "Clock" | "Rand" | "Env" | "Secret" | "Exec" | "Fetch" | "Dir" | "File" | "Net" | "Socket" | "Listener" | "BuildOut" | "BuildRead" | "BuildEnv" | "BuildNet" | "BuildExec") }
fn capability_name(name: &str) -> CanonicalCapabilityKind { match name { "Console" => CanonicalCapabilityKind::Console, "Clock" => CanonicalCapabilityKind::Clock, "Rand" => CanonicalCapabilityKind::Rand, "Env" => CanonicalCapabilityKind::Env, "Secret" => CanonicalCapabilityKind::Secret, "Exec" => CanonicalCapabilityKind::Exec, "Fetch" => CanonicalCapabilityKind::Fetch, "Dir" => CanonicalCapabilityKind::Dir, "File" => CanonicalCapabilityKind::File, "Net" => CanonicalCapabilityKind::Net, "Socket" => CanonicalCapabilityKind::Socket, "Listener" => CanonicalCapabilityKind::Listener, "BuildOut" => CanonicalCapabilityKind::BuildOut, "BuildRead" => CanonicalCapabilityKind::BuildRead, "BuildEnv" => CanonicalCapabilityKind::BuildEnv, "BuildNet" => CanonicalCapabilityKind::BuildNet, "BuildExec" => CanonicalCapabilityKind::BuildExec, _ => unreachable!("checked capability name") } }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typeck;

    fn typed(source: &str) -> TypedModule {
        let module = witchy_syntax::parser::parse_module(source).expect("parse");
        typeck::annotate_checked(module).expect("type check")
    }

    #[test]
    fn function_rename_does_not_change_manifest() {
        let a = derive_manifest(&typed("fn apply(value: Int) -> Int:\n    value\n\nfn main() -> Int:\n    apply(1)\n")).expect("manifest");
        let b = derive_manifest(&typed("fn renamed(value: Int) -> Int:\n    value\n\nfn main() -> Int:\n    renamed(1)\n")).expect("manifest");
        assert_eq!(a.digest(), b.digest());
    }

    #[test]
    fn convention_and_result_type_change_manifest() {
        let base = derive_manifest(&typed("fn apply(value: Int) -> Int:\n    value\n\nfn main() -> Int:\n    apply(1)\n")).expect("manifest");
        let convention = derive_manifest(&typed("fn apply(var value: Int) -> Int:\n    value\n\nfn main() -> Int:\n    var n = 1\n    apply(n)\n")).expect("manifest");
        let result = derive_manifest(&typed("fn apply(value: Int) -> String:\n    \"x\"\n\nfn main() -> Int:\n    apply(1).length()\n")).expect("manifest");
        assert_ne!(base.digest(), convention.digest());
        assert_ne!(base.digest(), result.digest());
    }
}
