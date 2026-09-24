//! Authenticated, portable contracts for exported `mode opt` callables.
//!
//! The checker deliberately keeps opt callable provenance out of its runtime
//! function type. A separately compiled library therefore needs a signed
//! representation of the same recursive fact. Parsing these bytes is never an
//! admission receipt: a package loader must authenticate the sidecar digest and
//! bind its module identity to its independently authenticated load identity.

use std::collections::BTreeMap;
use sha2::{Digest, Sha256};

use crate::runtime_type::{ModuleLoadIdentity, PackageCoordinate, PackageSource, RuntimeTypeError};

/// Stable sidecar schema. Readers reject unknown schemas instead of applying a
/// best-effort interpretation to an older contract.
pub const OPT_CALLABLE_EXPORT_SCHEMA: u32 = 2;
const MAGIC: &[u8; 8] = b"WOPTEXP\0";
const MAX_SIDECAR_BYTES: usize = 1 << 20;
const MAX_EXPORTS: usize = 256;
const MAX_STRING_BYTES: usize = 1024;
const MAX_MODULE_PATH_SEGMENTS: usize = 64;
const MAX_CALLABLE_ARITY: usize = 64;
const MAX_CALLABLE_DEPTH: usize = 32;
const MAX_CALLABLE_NODES: usize = 4096;

/// The recursive provenance demanded at a callable input/result boundary.
///
/// This intentionally carries no ABI or source type. Those are checked against
/// the importing declaration. It records only the `mode opt` fact that an ABI
/// would otherwise erase.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum OptCallableSlot {
    Value,
    OptCallable(Box<OptCallableContract>),
}

/// Recursive input/result provenance for one exported opt callable.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct OptCallableContract {
    pub parameters: Vec<OptCallableSlot>,
    pub result: OptCallableSlot,
}

/// Digest of the separately authenticated, full checked callable signature.
///
/// The opt tree cannot stand alone: it says which nested callable positions
/// require provenance, but not the calling convention, ownership transfer,
/// qualifiers, loan/lifetime relations, or reference-bearing source type. A
/// precompiled import is admitted only when its independently authenticated
/// checked-signature envelope has this exact digest. The type checker owns the
/// envelope encoder; this transport schema deliberately cannot manufacture one
/// from an importing declaration.  It is SHA-256 because the signed package
/// manifest uses the same digest family for external sidecars.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CheckedCallableSignatureDigest([u8; 32]);

impl CheckedCallableSignatureDigest {
    /// Derive the domain-separated digest of a canonical checked-signature
    /// envelope. The envelope's format is checker-owned; this API exists so
    /// its authenticated loader can bind the same bytes to an opt sidecar.
    pub fn from_canonical_checked_signature(envelope: &[u8]) -> Result<Self, OptCallableExportError> {
        if envelope.is_empty() || envelope.len() > MAX_SIDECAR_BYTES {
            return Err(OptCallableExportError::LimitExceeded("checked signature envelope size"));
        }
        let mut hasher = Sha256::new();
        hasher.update(b"witchy.checked-callable-signature.v1\0");
        hasher.update(envelope);
        Ok(Self(hasher.finalize().into()))
    }

    pub fn as_bytes(self) -> [u8; 32] { self.0 }
}

/// Produce the canonical full-signature envelope for one exact checked
/// declaration. It joins the finalized callable type with the retained,
/// alpha-normalized checked access identity. The former carries source type
/// structure and conventions; the latter carries qualifier, ownership,
/// reference, and lifetime/loan relations. Both originate in the retained
/// checked module, so an importing declaration cannot forge this envelope.
pub fn canonical_checked_signature_bytes(
    typed: &crate::typeck::TypedModule,
    export: &str,
) -> Result<Vec<u8>, OptCallableExportError> {
    let declaration = crate::canonical_abi::checked_declaration_signature(typed, export)
        .map_err(|error| OptCallableExportError::InvalidSignature(error.to_string()))?;
    let source_type = crate::canonical_abi::canonical_ast_type_bytes(typed, &declaration)
        .map_err(|error| OptCallableExportError::InvalidSignature(error.to_string()))?;
    let access = typed.access_facts()
        .map_err(|error| OptCallableExportError::InvalidSignature(error.to_string()))?
        .declaration(export)
        .ok_or_else(|| OptCallableExportError::InvalidSignature(format!("missing checked declaration `{export}`")))?
        .identity_key();
    let mut bytes = b"witchy.checked-callable-signature-envelope.v1\0".to_vec();
    put_len(&mut bytes, source_type.len());
    bytes.extend_from_slice(&source_type);
    put_len(&mut bytes, access.as_bytes().len());
    bytes.extend_from_slice(access.as_bytes());
    if bytes.len() > MAX_SIDECAR_BYTES { return Err(OptCallableExportError::LimitExceeded("checked signature envelope size")); }
    Ok(bytes)
}

/// One opt export's recursively required provenance, bound to the exact full
/// checked signature of that export.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct OptCallableExport {
    declaration_source: String,
    canonical_source_signature_sha256: CheckedCallableSignatureDigest,
    contract: OptCallableContract,
}

impl OptCallableExport {
    pub fn new(
        declaration_source: String,
        canonical_source_signature_sha256: CheckedCallableSignatureDigest,
        contract: OptCallableContract,
    ) -> Result<Self, OptCallableExportError> {
        validate_declaration_source(&declaration_source)?;
        let mut nodes = MAX_CALLABLE_NODES;
        validate_contract(&contract, 0, &mut nodes)?;
        Ok(Self { declaration_source, canonical_source_signature_sha256, contract })
    }

    /// Required binding to the independently authenticated full declaration
    /// envelope. A precompiled importer must reject a missing or mismatched
    /// envelope before it can observe [`Self::contract`].
    pub fn canonical_source_signature_sha256(&self) -> [u8; 32] {
        self.canonical_source_signature_sha256.as_bytes()
    }
    /// Canonical one-line source declaration header. The authenticated loader
    /// reconstructs a checked declaration from this exact grammar, then checks
    /// its full checked-signature envelope against the signed digest above.
    pub fn declaration_source(&self) -> &str { &self.declaration_source }

    /// Reconstruct a sealed scalar-only declaration from the authenticated
    /// header. No imported body or import-site declaration participates. This
    /// deliberately refuses managed, reference, capability, nominal, generic,
    /// and callable ABI classes until their independently executable import
    /// adapters exist.
    pub fn checked_scalar_signature(&self, export_name: &str) -> Result<Vec<u8>, OptCallableExportError> {
        self.checked_scalar_declaration_and_signature(export_name).map(|(_, signature)| signature)
    }

    /// Recover the sealed declaration structurally from this authenticated
    /// export record.  The caller receives no body supplied by an importer:
    /// this is the scalar literal stub whose checked signature was compared
    /// with the signed envelope.
    pub fn checked_scalar_declaration(
        &self,
        export_name: &str,
    ) -> Result<witchy_syntax::ast::Function, OptCallableExportError> {
        self.checked_scalar_declaration_and_signature(export_name).map(|(declaration, _)| declaration)
    }

    fn checked_scalar_declaration_and_signature(
        &self,
        export_name: &str,
    ) -> Result<(witchy_syntax::ast::Function, Vec<u8>), OptCallableExportError> {
        use witchy_syntax::ast::{Block, Expr, Item, Stmt, Type};
        if !is_closed_scalar_contract(&self.contract) {
            return Err(OptCallableExportError::UnsupportedAbi(
                "higher-order opt callback contracts need an executable adapter",
            ));
        }
        // The header is signed textual input, so parse it exactly once. The
        // placeholder exists only to complete that header's grammar; before it
        // reaches the checker we replace it with a sealed AST body. In
        // particular, do not render the parsed header back into a new source
        // string: that would create a second raw-generation path.
        let mut module = witchy_syntax::parser::parse_module(&format!("mode opt\n{}:\n    ()\n", self.declaration_source))
            .map_err(|error| OptCallableExportError::InvalidSignature(error.to_string()))?;
        let function = module.items.iter_mut().find_map(|item| match item { Item::Function(function) if function.name == export_name => Some(function), _ => None })
            .ok_or_else(|| OptCallableExportError::InvalidSignature("declaration header name does not match export".into()))?;
        let scalar = |ty: &Type| matches!(ty, Type::Named(name, args) if args.is_empty() && matches!(name.as_str(), "Int" | "Float" | "Bool" | "Unit" | "Nil"));
        if function.params.iter().any(|parameter| parameter.convention != witchy_syntax::ast::Convention::Let || parameter.ty.as_ref().is_none_or(|ty| !scalar(ty)))
            || function.ret.as_ref().is_none_or(|ty| !scalar(ty))
        {
            return Err(OptCallableExportError::UnsupportedAbi("only closed let scalar parameters/results are admitted"));
        }
        let body = match function.ret.as_ref().expect("checked above") {
            Type::Named(name, _) if name == "Int" => Expr::Int(0),
            Type::Named(name, _) if name == "Float" => Expr::Float(0.0),
            Type::Named(name, _) if name == "Bool" => Expr::Bool(false),
            Type::Named(_, _) => Expr::Tuple(vec![]),
            _ => unreachable!("scalar predicate"),
        };
        function.body = Block { stmts: vec![Stmt::Expr(body)], lines: vec![0], region: None };
        let declaration = function.clone();
        let typed = crate::typeck::annotate_checked(module)
            .map_err(|error| OptCallableExportError::InvalidSignature(error.to_string()))?;
        let signature = canonical_checked_signature_bytes(&typed, export_name)?;
        if CheckedCallableSignatureDigest::from_canonical_checked_signature(&signature)?
            != self.canonical_source_signature_sha256
        {
            return Err(OptCallableExportError::InvalidSignature(
                "declaration source does not match canonical_source_signature_sha256".into(),
            ));
        }
        Ok((declaration, signature))
    }
    pub fn contract(&self) -> &OptCallableContract { &self.contract }
}

/// The first precompiled ABI admits only direct scalar calls. In particular,
/// accepting a recursive provenance tree here would falsely imply that a
/// callback/function-value adapter crosses the separate-Wasm boundary.
fn is_closed_scalar_contract(contract: &OptCallableContract) -> bool {
    contract.parameters.iter().all(|slot| matches!(slot, OptCallableSlot::Value))
        && matches!(&contract.result, OptCallableSlot::Value)
}

/// Canonical export contracts keyed by a module's authenticated identity and
/// each exported callable name. The repeated identity prevents a sidecar from
/// one module being replayed for another.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptCallableExportSidecar {
    module: ModuleLoadIdentity,
    exports: BTreeMap<String, OptCallableExport>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OptCallableExportError {
    Malformed(&'static str),
    InvalidIdentity(String),
    InvalidSignature(String),
    UnsupportedAbi(&'static str),
    InvalidExportName,
    DuplicateExport(String),
    LimitExceeded(&'static str),
    TrailingBytes,
}

impl std::fmt::Display for OptCallableExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(reason) => write!(f, "malformed opt callable export sidecar: {reason}"),
            Self::InvalidIdentity(reason) => write!(f, "invalid opt callable export identity: {reason}"),
            Self::InvalidSignature(reason) => write!(f, "invalid checked callable signature: {reason}"),
            Self::UnsupportedAbi(reason) => write!(f, "unsupported precompiled callable ABI: {reason}"),
            Self::InvalidExportName => f.write_str("opt callable export name is empty"),
            Self::DuplicateExport(name) => write!(f, "duplicate opt callable export `{name}`"),
            Self::LimitExceeded(which) => write!(f, "opt callable export sidecar exceeds {which} limit"),
            Self::TrailingBytes => f.write_str("opt callable export sidecar has trailing bytes"),
        }
    }
}

impl std::error::Error for OptCallableExportError {}
impl From<RuntimeTypeError> for OptCallableExportError {
    fn from(value: RuntimeTypeError) -> Self { Self::InvalidIdentity(value.to_string()) }
}

impl OptCallableExportSidecar {
    pub fn new(
        module: ModuleLoadIdentity,
        exports: impl IntoIterator<Item = (String, OptCallableExport)>,
    ) -> Result<Self, OptCallableExportError> {
        validate_identity(&module)?;
        let mut canonical = BTreeMap::new();
        let mut nodes = MAX_CALLABLE_NODES;
        for (name, contract) in exports {
            if name.is_empty() { return Err(OptCallableExportError::InvalidExportName); }
            if name.len() > MAX_STRING_BYTES { return Err(OptCallableExportError::LimitExceeded("export name length")); }
            if canonical.len() == MAX_EXPORTS { return Err(OptCallableExportError::LimitExceeded("export count")); }
            validate_contract(contract.contract(), 0, &mut nodes)?;
            if canonical.insert(name.clone(), contract).is_some() {
                return Err(OptCallableExportError::DuplicateExport(name));
            }
        }
        Ok(Self { module, exports: canonical })
    }

    pub fn module(&self) -> &ModuleLoadIdentity { &self.module }
    pub fn export(&self, name: &str) -> Option<&OptCallableExport> { self.exports.get(name) }
    pub fn exports(&self) -> impl Iterator<Item = (&str, &OptCallableExport)> {
        self.exports.iter().map(|(name, contract)| (name.as_str(), contract))
    }

    /// Deterministic, length-delimited bytes to commit in a signed library
    /// manifest. This is a sidecar file, never a Wasm custom section.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, OptCallableExportError> {
        let mut out = MAGIC.to_vec();
        put_u32(&mut out, OPT_CALLABLE_EXPORT_SCHEMA);
        put_identity(&mut out, &self.module);
        put_len(&mut out, self.exports.len());
        for (name, contract) in &self.exports {
            put_string(&mut out, name);
            put_string(&mut out, &contract.declaration_source);
            out.extend_from_slice(&contract.canonical_source_signature_sha256.0);
            put_contract(&mut out, &contract.contract);
        }
        if out.len() > MAX_SIDECAR_BYTES { return Err(OptCallableExportError::LimitExceeded("total sidecar bytes")); }
        Ok(out)
    }

    /// Decode the exact canonical format. The caller must still check the
    /// signed digest and authenticated loader identity before accepting it.
    pub fn decode(bytes: &[u8]) -> Result<Self, OptCallableExportError> {
        if bytes.len() > MAX_SIDECAR_BYTES { return Err(OptCallableExportError::LimitExceeded("total sidecar bytes")); }
        let mut input = Input { bytes, cursor: 0 };
        if input.take(MAGIC.len())? != MAGIC {
            return Err(OptCallableExportError::Malformed("wrong magic"));
        }
        if input.u32()? != OPT_CALLABLE_EXPORT_SCHEMA {
            return Err(OptCallableExportError::Malformed("unsupported schema"));
        }
        let module = input.identity()?;
        let count = input.len()?;
        if count > MAX_EXPORTS { return Err(OptCallableExportError::LimitExceeded("export count")); }
        let mut exports = Vec::with_capacity(count);
        let mut nodes = MAX_CALLABLE_NODES;
        for _ in 0..count {
            let name = input.string()?;
            let declaration_source = input.string()?;
            let canonical_source_signature_sha256 = CheckedCallableSignatureDigest(input.take(32)?.try_into().expect("exact signature digest"));
            let contract = input.contract(0, &mut nodes)?;
            exports.push((name, OptCallableExport::new(declaration_source, canonical_source_signature_sha256, contract)?));
        }
        if input.cursor != input.bytes.len() { return Err(OptCallableExportError::TrailingBytes); }
        Self::new(module, exports)
    }
}

fn put_len(out: &mut Vec<u8>, length: usize) {
    put_u32(out, u32::try_from(length).expect("canonical opt export length"));
}
fn put_u32(out: &mut Vec<u8>, value: u32) { out.extend_from_slice(&value.to_le_bytes()); }
fn put_string(out: &mut Vec<u8>, value: &str) { put_len(out, value.len()); out.extend_from_slice(value.as_bytes()); }

fn put_identity(out: &mut Vec<u8>, identity: &ModuleLoadIdentity) {
    match identity.package().source() {
        PackageSource::Toolchain => out.push(0),
        PackageSource::Workspace => out.push(1),
        PackageSource::Registry(registry) => { out.push(2); put_string(out, registry); }
    }
    put_string(out, identity.package().name());
    put_string(out, identity.package().version());
    put_len(out, identity.module_path().len());
    for segment in identity.module_path() { put_string(out, segment); }
}

fn put_contract(out: &mut Vec<u8>, contract: &OptCallableContract) {
    put_len(out, contract.parameters.len());
    for parameter in &contract.parameters { put_slot(out, parameter); }
    put_slot(out, &contract.result);
}
fn put_slot(out: &mut Vec<u8>, slot: &OptCallableSlot) {
    match slot {
        OptCallableSlot::Value => out.push(0),
        OptCallableSlot::OptCallable(contract) => { out.push(1); put_contract(out, contract); }
    }
}

fn validate_identity(identity: &ModuleLoadIdentity) -> Result<(), OptCallableExportError> {
    let package = identity.package();
    for text in [package.name(), package.version()] { validate_string(text, "package identity string")?; }
    if let PackageSource::Registry(registry) = package.source() { validate_string(registry, "registry identity string")?; }
    if identity.module_path().len() > MAX_MODULE_PATH_SEGMENTS { return Err(OptCallableExportError::LimitExceeded("module path segments")); }
    for segment in identity.module_path() { validate_string(segment, "module path segment")?; }
    Ok(())
}

fn validate_string(value: &str, limit: &'static str) -> Result<(), OptCallableExportError> {
    if value.len() > MAX_STRING_BYTES { Err(OptCallableExportError::LimitExceeded(limit)) } else { Ok(()) }
}

fn validate_declaration_source(source: &str) -> Result<(), OptCallableExportError> {
    validate_string(source, "declaration source length")?;
    if source.is_empty() || source.contains(['\n', '\r', '\0']) {
        return Err(OptCallableExportError::InvalidSignature("declaration source must be one line".into()));
    }
    Ok(())
}

fn validate_contract(
    contract: &OptCallableContract,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), OptCallableExportError> {
    if depth > MAX_CALLABLE_DEPTH { return Err(OptCallableExportError::LimitExceeded("callable recursion depth")); }
    if contract.parameters.len() > MAX_CALLABLE_ARITY { return Err(OptCallableExportError::LimitExceeded("callable arity")); }
    for parameter in &contract.parameters { validate_slot(parameter, depth, nodes)?; }
    validate_slot(&contract.result, depth, nodes)
}

fn validate_slot(slot: &OptCallableSlot, depth: usize, nodes: &mut usize) -> Result<(), OptCallableExportError> {
    *nodes = nodes.checked_sub(1).ok_or(OptCallableExportError::LimitExceeded("callable nodes"))?;
    if let OptCallableSlot::OptCallable(contract) = slot { validate_contract(contract, depth + 1, nodes)?; }
    Ok(())
}

struct Input<'a> { bytes: &'a [u8], cursor: usize }
impl<'a> Input<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8], OptCallableExportError> {
        let end = self.cursor.checked_add(length).ok_or(OptCallableExportError::Malformed("length overflow"))?;
        let result = self.bytes.get(self.cursor..end).ok_or(OptCallableExportError::Malformed("truncated input"))?;
        self.cursor = end;
        Ok(result)
    }
    fn u32(&mut self) -> Result<u32, OptCallableExportError> {
        let bytes: [u8; 4] = self.take(4)?.try_into().expect("exact four bytes");
        Ok(u32::from_le_bytes(bytes))
    }
    fn len(&mut self) -> Result<usize, OptCallableExportError> {
        usize::try_from(self.u32()?).map_err(|_| OptCallableExportError::Malformed("length does not fit platform"))
    }
    fn string(&mut self) -> Result<String, OptCallableExportError> {
        let length = self.len()?;
        if length > MAX_STRING_BYTES { return Err(OptCallableExportError::LimitExceeded("string length")); }
        let bytes = self.take(length)?;
        let value = std::str::from_utf8(bytes).map_err(|_| OptCallableExportError::Malformed("non-UTF-8 string"))?;
        Ok(value.to_owned())
    }
    fn identity(&mut self) -> Result<ModuleLoadIdentity, OptCallableExportError> {
        let source = match self.take(1)? {
            [0] => PackageSource::Toolchain,
            [1] => PackageSource::Workspace,
            [2] => PackageSource::Registry(self.string()?),
            _ => return Err(OptCallableExportError::Malformed("unknown package source")),
        };
        let package = PackageCoordinate::new(source, self.string()?, self.string()?)?;
        let count = self.len()?;
        if count > MAX_MODULE_PATH_SEGMENTS { return Err(OptCallableExportError::LimitExceeded("module path segments")); }
        let mut path = Vec::with_capacity(count);
        for _ in 0..count { path.push(self.string()?); }
        ModuleLoadIdentity::new(package, path).map_err(Into::into)
    }
    fn contract(&mut self, depth: usize, nodes: &mut usize) -> Result<OptCallableContract, OptCallableExportError> {
        if depth > MAX_CALLABLE_DEPTH { return Err(OptCallableExportError::LimitExceeded("callable recursion depth")); }
        let count = self.len()?;
        if count > MAX_CALLABLE_ARITY { return Err(OptCallableExportError::LimitExceeded("callable arity")); }
        let mut parameters = Vec::with_capacity(count);
        for _ in 0..count { parameters.push(self.slot(depth, nodes)?); }
        Ok(OptCallableContract { parameters, result: self.slot(depth, nodes)? })
    }
    fn slot(&mut self, depth: usize, nodes: &mut usize) -> Result<OptCallableSlot, OptCallableExportError> {
        *nodes = nodes.checked_sub(1).ok_or(OptCallableExportError::LimitExceeded("callable nodes"))?;
        match self.take(1)? {
            [0] => Ok(OptCallableSlot::Value),
            [1] => Ok(OptCallableSlot::OptCallable(Box::new(self.contract(depth + 1, nodes)?))),
            _ => Err(OptCallableExportError::Malformed("unknown callable slot")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module() -> ModuleLoadIdentity {
        ModuleLoadIdentity::new(
            PackageCoordinate::new(PackageSource::Registry("https://registry.example".into()), "acme/math", "1.2.3").unwrap(),
            ["src", "math"],
        ).unwrap()
    }
    fn nested() -> OptCallableContract {
        OptCallableContract {
            parameters: vec![OptCallableSlot::OptCallable(Box::new(OptCallableContract {
                parameters: vec![OptCallableSlot::Value], result: OptCallableSlot::Value,
            }))],
            result: OptCallableSlot::OptCallable(Box::new(OptCallableContract {
                parameters: vec![], result: OptCallableSlot::Value,
            })),
        }
    }

    fn export(contract: OptCallableContract) -> OptCallableExport {
        let signature = CheckedCallableSignatureDigest::from_canonical_checked_signature(b"checked-signature").unwrap();
        OptCallableExport::new("fn map(value: Int) -> Int".into(), signature, contract).unwrap()
    }

    #[test]
    fn rejects_tampering_missing_bytes_and_duplicate_exports() {
        let sidecar = OptCallableExportSidecar::new(module(), [("map".into(), export(nested()))]).unwrap();
        let mut bytes = sidecar.canonical_bytes().unwrap();
        *bytes.last_mut().unwrap() = 7;
        assert!(matches!(OptCallableExportSidecar::decode(&bytes), Err(OptCallableExportError::Malformed(_))));
        let full = sidecar.canonical_bytes().unwrap();
        assert!(matches!(OptCallableExportSidecar::decode(&full[..full.len() - 1]), Err(OptCallableExportError::Malformed(_))));
        assert!(matches!(
            OptCallableExportSidecar::new(module(), [("map".into(), export(nested())), ("map".into(), export(nested()))]),
            Err(OptCallableExportError::DuplicateExport(name)) if name == "map"
        ));
    }

    #[test]
    fn rejects_unbounded_wire_shapes_before_allocation_or_recursion() {
        let sidecar = OptCallableExportSidecar::new(module(), [("map".into(), export(nested()))]).unwrap();
        let mut bytes = sidecar.canonical_bytes().unwrap();
        // Export count begins immediately after the fixed magic/schema/identity
        // for this fixture. Altering the count to a hostile value must fail
        // before `Vec::with_capacity` can use it.
        let owner = module();
        let source_bytes = match owner.package().source() {
            PackageSource::Registry(registry) => 1 + 4 + registry.len(),
            PackageSource::Toolchain | PackageSource::Workspace => 1,
        };
        let count_at = MAGIC.len() + 4 + source_bytes
            + 4 + owner.package().name().len()
            + 4 + owner.package().version().len()
            + 4 + owner.module_path().iter().map(|segment| 4 + segment.len()).sum::<usize>();
        bytes[count_at..count_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(OptCallableExportSidecar::decode(&bytes), Err(OptCallableExportError::LimitExceeded("export count"))));

        let mut bytes = sidecar.canonical_bytes().unwrap();
        let export_name_len_at = count_at + 4;
        bytes[export_name_len_at..export_name_len_at + 4]
            .copy_from_slice(&u32::try_from(MAX_STRING_BYTES + 1).unwrap().to_le_bytes());
        assert!(matches!(OptCallableExportSidecar::decode(&bytes), Err(OptCallableExportError::LimitExceeded("string length"))));

        assert!(matches!(
            OptCallableExportSidecar::decode(&vec![0; MAX_SIDECAR_BYTES + 1]),
            Err(OptCallableExportError::LimitExceeded("total sidecar bytes"))
        ));

        let too_many = OptCallableContract {
            parameters: vec![OptCallableSlot::Value; MAX_CALLABLE_ARITY + 1], result: OptCallableSlot::Value,
        };
        let signature = CheckedCallableSignatureDigest::from_canonical_checked_signature(b"checked-signature").unwrap();
        assert!(matches!(OptCallableExport::new("fn map(value: Int) -> Int".into(), signature, too_many), Err(OptCallableExportError::LimitExceeded("callable arity"))));

        let mut deep = OptCallableSlot::Value;
        for _ in 0..=MAX_CALLABLE_DEPTH {
            deep = OptCallableSlot::OptCallable(Box::new(OptCallableContract { parameters: vec![], result: deep }));
        }
        let too_deep = OptCallableContract { parameters: vec![], result: deep };
        assert!(matches!(OptCallableExport::new("fn map(value: Int) -> Int".into(), signature, too_deep), Err(OptCallableExportError::LimitExceeded("callable recursion depth"))));
    }
}
