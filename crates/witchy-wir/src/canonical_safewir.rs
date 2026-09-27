//! Canonical, semantic SafeWIR for RFC-0165.
//!
//! This is the optimizing representation. Unlike the RFC-0162 retrofit
//! surface, it does not annotate raw WIR expressions or infer memory meaning
//! from pointer arithmetic. Memory is addressed through layout projections,
//! ownership changes are explicit, and proper tail calls are terminators.
//!
//! Candidate modules are intentionally constructible by lowering and
//! optimization code. Validated states are not: only the canonical checker may
//! mint those states, and no backend accepts this module until that checker and
//! the one-way lowering are implemented.

use std::collections::BTreeMap;
use std::marker::PhantomData;

use crate::layout::{LayoutBundle, LayoutId};

pub(crate) mod check;
pub mod analysis;
pub mod optimize;
pub mod manifest;
pub use check::{InitialCheckError, check_initial};
pub(crate) use check::{operation_uses, terminator_uses};
pub(crate) mod authority;
pub use authority::{
    CheckedInitialSafeWir, CheckedLoweringWitness, CheckedOptimizedSafeWir,
    CheckedSafeWirRevision,
    FinalValidationError, InitialWitnessError, OptimizationAuthorityError, ValidatedSafeWir,
    check_initial_with_witness, validate_final, validate_optimized_final,
};
#[cfg(feature = "raw-encoder-test-api")]
pub(crate) use authority::{
    check_initial_with_test_witness, validate_final_for_test,
};
#[path = "canonical_safewir/history.rs"]
mod identity;
pub(crate) use identity::ModuleDigest;

macro_rules! id_type {
    ($name:ident, $inner:ty) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name($inner);

        impl $name {
            pub const fn new(value: $inner) -> Self {
                Self(value)
            }

            pub const fn get(self) -> $inner {
                self.0
            }
        }
    };
}

id_type!(Revision, u64);
id_type!(FunctionId, u32);
id_type!(BlockId, u32);
id_type!(ValueId, u32);
id_type!(ImportId, u32);
id_type!(StaticDataId, u32);
id_type!(GcTypeId, u32);
id_type!(CapabilityKindId, u32);

impl Revision {
    /// Return the identity for a transformed module, or refuse rather than
    /// wrapping into an earlier analysis epoch.
    pub fn checked_next(self) -> Option<Self> {
        self.get().checked_add(1).map(Self::new)
    }
}

/// Staged opaque-owner ABI contract. Not yet admitted as executable operations.
pub mod closure_owner_contract;
mod owner_authority;
// Dev-only: SafeWIR cases driven by a real checked source (RFC-0170 P4).
#[cfg(feature = "checked-source-test-api")]
#[doc(hidden)]
pub mod checked_source_test_api;
mod exec;
pub use exec::{EXEC_CAPABILITY_KIND, EXEC_RESPONSE_KIND, EXEC_IMPORT_NAMES};
mod fetch;
pub use fetch::{FETCH_CAPABILITY_KIND, FETCH_RESPONSE_KIND, FETCH_IMPORT_NAMES};
mod registry_hosts;
pub use registry_hosts::{CLOCK_CAPABILITY_KIND, SECRET_STORE_CAPABILITY_KIND, SECRET_CAPABILITY_KIND, REGISTRY_HOST_IMPORT_NAMES};
mod dir_storage;
pub use dir_storage::{DIR_BYTES_RESPONSE_KIND, DIR_STORAGE_IMPORT_NAMES};
pub(crate) mod build_read;
pub use build_read::{BUILD_READ_CAPABILITY_KIND, BUILD_READ_RESPONSE_KIND, BUILD_READ_IMPORT_NAMES};
pub use build_read::{BUILD_ENV_CAPABILITY_KIND, BUILD_ENV_IMPORT_NAMES};
pub use build_read::{BUILD_EXEC_CAPABILITY_KIND, BUILD_EXEC_IMPORT_NAMES};
pub use build_read::{BUILD_NET_CAPABILITY_KIND, BUILD_NET_IMPORT_NAMES};
mod crypto;
mod dir_metadata;
pub use crypto::{CRYPTO_ED25519_VERIFY_IMPORT, CRYPTO_RSA_PKCS1_SHA256_VERIFY_IMPORT, CRYPTO_SHA256_IMPORT, CRYPTO_RUNE_HASH_IMPORT};
pub use crypto::{CRYPTO_SHA512_IMPORT, CRYPTO_SHA3_256_IMPORT, CRYPTO_HMAC_SHA256_IMPORT,
    CRYPTO_SHAKE128_IMPORT, CRYPTO_SHAKE256_IMPORT, CRYPTO_ECDSA_P256_VERIFY_IMPORT,
    CRYPTO_ECDSA_P256_VERIFY_HEX_IMPORT, CRYPTO_EXTENSION_IMPORT_NAMES};
mod vm_service;
pub use vm_service::{CandidateVmService, VM_SERVICE_IMPORT_NAMES, VM_SERVICE_KIND, VM_SERVICE_RESPONSE_KIND};
mod compiler_session;
pub use compiler_session::{COMPILER_FRESH_IMPORT, COMPILER_QUOTE_EXPR_IMPORT, COMPILER_EMIT_EXPR_IMPORT};

/// Stable identity supplied by canonical type checking. It identifies the
/// checked source but grants no validation or encoding authority by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CheckedSourceIdentity([u8; 32]);

impl CheckedSourceIdentity {
    pub const fn from_digest(digest: [u8; 32]) -> Self {
        Self(digest)
    }

    pub const fn digest(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ValueType {
    Int,
    Float,
    Bool,
    Unit,
    Size,
    V128,
    /// A fixed-layout scalar aggregate. This value has no address, ownership,
    /// or ABI representation in v1; it exists only within one SSA block.
    InlineAggregate(LayoutId),
    Ref(LayoutId),
    Gc(GcTypeId),
    Capability(CapabilityKindId),
    /// An optional authenticated capability. It is never a linear-memory value
    /// or entry argument; a host may return it only through its dedicated role.
    NullableCapability(CapabilityKindId),
    Callable(FunctionSignatureId),
}

id_type!(FunctionSignatureId, u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TailSiteId([u8; 32]);

impl TailSiteId {
    pub const fn from_digest(digest: [u8; 32]) -> Self {
        Self(digest)
    }

    pub const fn digest(self) -> [u8; 32] {
        self.0
    }
}

/// Overflow is part of program meaning, never an optimizer annotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OverflowBehavior {
    Wrap,
    Trap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ArithmeticOperator {
    Add,
    Subtract,
    Multiply,
    DivideSigned,
    DivideUnsigned,
    RemainderSigned,
    RemainderUnsigned,
    BitAnd,
    BitOr,
    BitXor,
    ShiftLeft,
    ShiftRightSigned,
    ShiftRightUnsigned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FloatUnaryOperator {
    Sqrt,
    TruncateToInt,
    /// An `Int` converted to the nearest `Float`.
    FromInt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ComparisonOperator {
    Equal,
    NotEqual,
    LessSigned,
    LessUnsigned,
    LessEqualSigned,
    LessEqualUnsigned,
    GreaterSigned,
    GreaterUnsigned,
    GreaterEqualSigned,
    GreaterEqualUnsigned,
}

/// The object's live logical extent and its allocated capacity are distinct.
/// Bounds checks use `count`; allocation and growth use `capacity`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Extent {
    FixedBytes(u64),
    /// A catalog-authenticated prefix that may be read before a dynamic count
    /// has been loaded. It grants no access beyond `minimum_bytes`.
    HeaderPrefix { minimum_bytes: u32 },
    Counted {
        header_bytes: u32,
        count: ValueId,
        capacity: ValueId,
        stride_bytes: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Ownership {
    Unique,
    Shared,
    Consumed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AllocationClass {
    Bump,
    ReferenceCounted,
    Scratch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AccessMode {
    Read,
    Write,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Projection {
    Header(HeaderField),
    Field { index: u32 },
    Element { index: ValueId },
    Slice { start: ValueId, length: ValueId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HeaderField {
    Length,
    Capacity,
}

/// The only v1 representation of an addressable memory location.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Place {
    object: ValueId,
    projection: Projection,
    value_type: ValueType,
    access: AccessMode,
}

impl Place {
    pub fn projected(
        object: ValueId,
        projection: Projection,
        value_type: ValueType,
        access: AccessMode,
    ) -> Self {
        Self {
            object,
            projection,
            value_type,
            access,
        }
    }

    pub const fn object(&self) -> ValueId {
        self.object
    }

    pub const fn projection(&self) -> &Projection {
        &self.projection
    }

    pub const fn value_type(&self) -> ValueType {
        self.value_type
    }

    pub const fn access(&self) -> AccessMode {
        self.access
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GuardKind {
    Bounds,
    Arithmetic,
    NonNull,
    DynamicType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LoadTransfer {
    CopyScalar,
    CloneShared,
    ClonePresent,
    /// Detach the final element of a unique packed list and publish its shorter
    /// count atomically. The element's allocation need not be unique.
    TakeLast,
    /// Take one present reference from a nullable field of a uniquely owned
    /// aggregate, then clear that field before its shell can be released.
    TakeNullableUnique,
    MoveUnique,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StoreTransfer {
    CopyScalar,
    CloneShared,
    MoveUnique,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallArgument {
    Value(ValueId),
    Move(ValueId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ParameterOwnership {
    /// With `ValueType::Ref`, an explicit source `&'a T`: one
    /// owner-retaining, read-only handle. The checker accepts the call only
    /// when another live token for the same storage remains with the caller.
    Value,
    Unique,
    Shared,
    /// An affine managed-reference token passed by source `var`. The ABI
    /// returns one trailing replacement token per such parameter.
    WriteBack,
    /// Checked source `var unique`: exclusive RC input and unique write-back.
    UniqueWriteBack,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterContract {
    pub value_type: ValueType,
    pub ownership: ParameterOwnership,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReturnedExtent {
    FixedBytes(u64),
    Counted { header_bytes: u32, stride_bytes: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultContract {
    /// Non-authoritative lowering placeholder. Initial checking replaces this
    /// with the exact contract derived from the corresponding function body;
    /// it is never valid for an import or a validated module.
    BodyDerived(ValueType),
    Value(ValueType),
    /// A reference-valued result guaranteed by the checked callable ABI.
    /// It grants liveness and read access only; allocation class, mutation,
    /// release, and reclaim authority remain unknown.
    CheckedValue { layout: LayoutId },
    Fresh {
        layout: LayoutId,
        class: AllocationClass,
        extent: ReturnedExtent,
    },
    Shared { layout: LayoutId },
    /// A complete exclusive RC token, not a claim of fresh allocation.
    Unique { layout: LayoutId },
    /// One source-authenticated opaque GC owner token. The GC ownership
    /// dataflow must prove an owned origin; no linear-memory layout is implied.
    OwnedGc { family: GcTypeId },
    AliasesParameter { parameter: u32, layout: LayoutId },
    /// A checked exclusive result carrier. Unlike `AliasesParameter`, this
    /// requires a declaration-derived `WriteBack` input and retains its
    /// independent replacement lane after the ordinary alias result. A
    /// projected carrier records its sole fixed record-field index here; it
    /// is never widened to a root alias.
    AliasesWriteBackParameter {
        parameter: u32,
        layout: LayoutId,
        projection: Option<u32>,
    },
    /// A checked direct-exclusive result selected from exactly one of several
    /// direct write-back inputs.  The ordered candidate list is source/access
    /// authority, not an ABI-shape heuristic; each candidate retains its own
    /// hidden replacement lane.
    AliasesOneOfWriteBackParameters {
        parameters: Vec<u32>,
        layout: LayoutId,
    },
    /// The authenticated Bool discriminator paired with the leading
    /// `AliasesOneOfWriteBackParameters` result.  It is an ABI-only carrier:
    /// `carrier_result` and `parameters` must exactly match that leading
    /// result, while `selector_parameter` is the checked source Bool input
    /// whose true arm selects `parameters[0]` and false arm `parameters[1]`.
    /// No caller may manufacture a selector from a same-shaped Bool value.
    DisjunctiveWriteBackSelector {
        carrier_result: u32,
        parameters: Vec<u32>,
        selector_parameter: u32,
    },
    ReallocatesParameter { parameter: u32, layout: LayoutId },
    StaticData { layout: LayoutId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MemoryEffect {
    Read,
    Write,
    Retain,
    Release,
    Reclaim,
}

/// A declaration constrains callers. The checker derives the implementation
/// summary from the body and verifies conformance; a name never supplies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallableContract {
    pub parameters: Vec<ParameterContract>,
    pub results: Vec<ResultContract>,
    pub parameter_effects: BTreeMap<u32, Vec<MemoryEffect>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionSignature {
    pub id: FunctionSignatureId,
    pub parameters: Vec<ValueType>,
    pub results: Vec<ValueType>,
    pub contract: CallableContract,
}

/// Package-authenticated identity of a separately compiled scalar Wasm
/// library.  These are commitments from the signed library manifest and its
/// bounded export sidecar, never custom-section metadata or an import-site
/// declaration.  The runtime compares this exact triple before linking an
/// instance into an admitted caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExternalWasmIdentity {
    pub library_binding_digest: [u8; 32],
    pub wasm_sha256: [u8; 32],
    pub sidecar_sha256: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportContractIdentity {
    /// The canonical catalog authenticates the ABI contract only. Host
    /// implementation authentication remains a launch-context dependency.
    AuthenticatedAbi([u8; 32]),
    /// A separately compiled scalar-library function. This is intentionally
    /// disjoint from the host ABI catalog: it carries no host authority,
    /// buffer access, reentrancy, or capability role.
    AuthenticatedExternal(ExternalWasmIdentity),
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HostBufferExtent {
    /// The host may access exactly the checked runtime slice passed at this
    /// call site. The slice bounds remain part of SafeWIR and are independently
    /// re-established by the memory checker.
    Slice,
    Exact(u64),
    AtLeast(u64),
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostParameterRole {
    Value(ValueType),
    Capability(CapabilityKindId),
    Buffer {
        value_type: ValueType,
        access: AccessMode,
        extent: HostBufferExtent,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostResultRole {
    Value(ValueType),
    Capability(CapabilityKindId),
    NullableCapability(CapabilityKindId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HostMutation {
    ReadOnly,
    WriteBuffers,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HostReentrancy {
    Forbidden,
    Allowed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostCallArgument {
    Value(ValueId),
    Capability(ValueId),
    Buffer {
        place: Place,
        extent: HostBufferExtent,
    },
}

/// Imported contracts are keyed by authenticated ABI identity. The concrete
/// host implementation identity belongs to launch context, not artifact ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportContract {
    pub id: ImportId,
    pub signature: FunctionSignatureId,
    pub abi_identity: ImportContractIdentity,
    /// Physical linkage is authenticated as part of `abi_identity` and is
    /// never consulted to derive memory or capability authority.
    pub module: String,
    pub name: String,
    pub parameter_roles: Vec<HostParameterRole>,
    pub result_roles: Vec<HostResultRole>,
    pub mutation: HostMutation,
    pub reentrancy: HostReentrancy,
}

/// Trusted ABI facts are supplied by the canonical generated host catalog,
/// never by a candidate module or a helper-name registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalAbiCatalog {
    imports: BTreeMap<ImportId, ImportContract>,
}

/// The buffer a `Float` render hands the `float_to_str` import: an f64's
/// shortest round-trip decimal never exceeds 24 bytes (`-1.2345678901234568e-308`),
/// so 32 leaves the host no way to overrun it.
pub const FLOAT_RENDER_BUFFER_BYTES: u64 = 32;

/// The fixed ABI kind of a `Dir` token. Rights (`Dir[Read]`, `Dir[Write]`)
/// and subtree scope live in the host's authority object behind the
/// externref, never in the kind, so every `Dir` spelling is this one kind.
pub const DIR_CAPABILITY_KIND: CapabilityKindId = CapabilityKindId::new(1);

mod net;
pub use net::{NET_CAPABILITY_KIND, LISTENER_CAPABILITY_KIND, SOCKET_CAPABILITY_KIND, NET_IMPORT_NAMES};

/// The fixed ABI kind of the `Env` token.
pub const ENV_CAPABILITY_KIND: CapabilityKindId = CapabilityKindId::new(2);

pub const FILE_CAPABILITY_KIND: CapabilityKindId = CapabilityKindId::new(3);

/// BuildOut is a write-only host capability, never a linear-memory value.
pub const BUILD_OUT_CAPABILITY_KIND: CapabilityKindId = CapabilityKindId::new(9);

/// The canonical signatures the module declares for the six `Dir`/`Env`
/// operations. Lowering owns the signature ids; the catalog owns everything
/// authenticated about the imports themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityImportSignatures {
    pub dir_read_into: FunctionSignatureId,
    pub dir_list_into: FunctionSignatureId,
    pub dir_exists_at: FunctionSignatureId,
    pub dir_write_at: FunctionSignatureId,
    pub dir_subtree_at: FunctionSignatureId,
    pub dir_read_file: FunctionSignatureId,
    pub file_read_into: FunctionSignatureId,
    pub env_has_var: FunctionSignatureId,
    pub env_get_into: FunctionSignatureId,
}

/// Semantic signatures for stateless compiler introspection imports. The
/// authenticated prelude, not lowering, assigns their stable import IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompilerImportSignatures {
    pub footprint_into: FunctionSignatureId,
    pub diff_into: FunctionSignatureId,
    pub doc_into: FunctionSignatureId,
    pub doc_result_json_into: FunctionSignatureId,
}

/// One prelude import as an authenticated contract: the generated ABI must
/// carry it with exactly the expected Wasm shape and as authority-free pure
/// infrastructure; its identity digest binds the ABI version, linkage, and
/// the authenticated metadata, never the candidate's own claims.
fn authenticated_prelude_import(
    name: &str,
    params: &[crate::wir_prelude::WasmTy],
    results: &[crate::wir_prelude::WasmTy],
    signature: FunctionSignatureId,
    parameter_roles: Vec<HostParameterRole>,
    result_roles: Vec<HostResultRole>,
    mutation: HostMutation,
) -> Result<ImportContract, String> {
    authenticated_import(name, params, results, signature, parameter_roles, result_roles, mutation, ImportAuthentication::PureInfrastructure)
}

/// An authenticated toolchain service has no capability authority but is not
/// generic infrastructure: it is explicitly absent from hosts that do not
/// implement this compiler-service ABI.
fn authenticated_internal_service_import(
    name: &str,
    params: &[crate::wir_prelude::WasmTy],
    results: &[crate::wir_prelude::WasmTy],
    signature: FunctionSignatureId,
    parameter_roles: Vec<HostParameterRole>,
    result_roles: Vec<HostResultRole>,
    mutation: HostMutation,
) -> Result<ImportContract, String> {
    authenticated_import(name, params, results, signature, parameter_roles, result_roles, mutation, ImportAuthentication::InternalService)
}

/// One capability import as an authenticated contract: the mirror image of
/// the pure-infrastructure rule. The generated ABI must carry it as a
/// capability-authority import whose authorities are exactly the ones this
/// operation exercises, so an authority-free (or differently authorized)
/// import can never be projected as a `Dir`/`Env` operation.
fn authenticated_capability_import(
    name: &str,
    params: &[crate::wir_prelude::WasmTy],
    results: &[crate::wir_prelude::WasmTy],
    signature: FunctionSignatureId,
    parameter_roles: Vec<HostParameterRole>,
    result_roles: Vec<HostResultRole>,
    mutation: HostMutation,
    authorities: &[crate::wir_prelude::AbiImportAuthority],
) -> Result<ImportContract, String> {
    authenticated_import(name, params, results, signature, parameter_roles, result_roles, mutation, ImportAuthentication::Capability(authorities))
}

enum ImportAuthentication<'a> {
    PureInfrastructure,
    InternalService,
    CompilerSession,
    RuntimeDiagnostic,
    Capability(&'a [crate::wir_prelude::AbiImportAuthority]),
}

/// The shared projection binds the generated classification as well as the
/// ABI shape.  An internal service remains authority-free, but cannot be
/// projected through the ordinary pure-infrastructure path.
#[allow(clippy::too_many_arguments)]
fn authenticated_import(
    name: &str,
    params: &[crate::wir_prelude::WasmTy],
    results: &[crate::wir_prelude::WasmTy],
    signature: FunctionSignatureId,
    parameter_roles: Vec<HostParameterRole>,
    result_roles: Vec<HostResultRole>,
    mutation: HostMutation,
    authentication: ImportAuthentication<'_>,
) -> Result<ImportContract, String> {
    use crate::wir_prelude::{abi_import_info, prelude, AbiImportClass, WasmTy, WITCHY_ABI_VERSION};

    let (index, import) = prelude()
        .imports
        .iter()
        .enumerate()
        .find(|(_, import)| import.module == "witchy" && import.name == name)
        .ok_or_else(|| format!("generated ABI has no witchy.{name} import"))?;
    if import.params != params || import.results != results {
        return Err(format!("generated witchy.{name} ABI has an unexpected shape"));
    }
    let info = abi_import_info(&import.name)
        .ok_or_else(|| format!("generated ABI witchy.{name} import has no authenticated metadata"))?;
    match authentication {
        ImportAuthentication::PureInfrastructure => {
            if info.class != AbiImportClass::PureInfrastructure || !info.authorities.is_empty() {
                return Err(format!("generated witchy.{name} ABI is authority-bearing"));
            }
        }
        ImportAuthentication::InternalService => {
            if info.class != AbiImportClass::InternalService || !info.authorities.is_empty() {
                return Err(format!("generated witchy.{name} ABI is not an authority-free internal service"));
            }
        }
        ImportAuthentication::CompilerSession => {
            if info.class != AbiImportClass::CompilerSession || !info.authorities.is_empty() {
                return Err(format!("generated witchy.{name} ABI is not a compiler-session operation"));
            }
        }
        ImportAuthentication::RuntimeDiagnostic => {
            if info.class != AbiImportClass::RuntimeDiagnostic || !info.authorities.is_empty() {
                return Err(format!("generated witchy.{name} ABI is not an authority-free runtime diagnostic"));
            }
        }
        ImportAuthentication::Capability(expected) => {
            if info.class != AbiImportClass::CapabilityAuthority {
                return Err(format!("generated witchy.{name} ABI is not a capability operation"));
            }
            if info.authorities.is_empty() {
                return Err(format!("generated witchy.{name} ABI names no authority"));
            }
            if info.authorities != expected {
                return Err(format!("generated witchy.{name} ABI names unexpected authorities"));
            }
        }
    }

    let mut digest = blake3::Hasher::new();
    digest.update(b"witchy.canonical-abi-import.v1");
    digest.update(&WITCHY_ABI_VERSION.to_le_bytes());
    digest.update(import.module.as_bytes());
    digest.update(&[0]);
    digest.update(import.name.as_bytes());
    digest.update(&[0, import.params.len() as u8, import.results.len() as u8]);
    for ty in import.params.iter().chain(&import.results) {
        digest.update(&[match ty {
            WasmTy::I32 => 0,
            WasmTy::I64 => 1,
            WasmTy::F32 => 2,
            WasmTy::F64 => 3,
            WasmTy::ExternRef => 4,
        }]);
    }
    digest.update(info.class.label().as_bytes());
    for authority in info.authorities {
        digest.update(authority.label().as_bytes());
        digest.update(&[0]);
    }

    let import_id = ImportId::new(u32::try_from(index).map_err(|_| "ABI import index overflow")?);
    Ok(ImportContract {
        id: import_id,
        signature,
        abi_identity: ImportContractIdentity::AuthenticatedAbi(*digest.finalize().as_bytes()),
        module: import.module.clone(),
        name: import.name.clone(),
        parameter_roles,
        result_roles,
        mutation,
        reentrancy: HostReentrancy::Forbidden,
    })
}

impl CanonicalAbiCatalog {
    pub fn with_build_out_import(mut self, signature: FunctionSignatureId, string_layout: LayoutId) -> Result<Self, String> {
        use crate::wir_prelude::{WasmTy, AbiImportAuthority};
        let read = || HostParameterRole::Buffer {
            value_type: ValueType::Ref(string_layout), access: AccessMode::Read,
            extent: HostBufferExtent::Slice,
        };
        let import = authenticated_capability_import(
            "build_out_write_at", &[WasmTy::ExternRef, WasmTy::I32, WasmTy::I32, WasmTy::I32, WasmTy::I32],
            &[], signature, vec![HostParameterRole::Capability(BUILD_OUT_CAPABILITY_KIND), read(), read()],
            Vec::new(), HostMutation::ReadOnly, &[AbiImportAuthority::BuildOut],
        )?;
        self.imports.insert(import.id, import);
        Ok(self)
    }
    /// Source-bearing Console output borrows two bounded slices and carries
    /// the actual checked receiver. This adds no ambient grant or write range.
    pub fn with_console_print_at(mut self, signature: FunctionSignatureId, string_layout: LayoutId) -> Result<Self, String> {
        use crate::wir_prelude::WasmTy;
        let read = || HostParameterRole::Buffer {
            value_type: ValueType::Ref(string_layout), access: AccessMode::Read,
            extent: HostBufferExtent::Slice,
        };
        let import = authenticated_prelude_import(
            "console_print_at", &[WasmTy::ExternRef, WasmTy::I32, WasmTy::I32, WasmTy::I32, WasmTy::I32, WasmTy::I32],
            &[], signature, vec![HostParameterRole::Capability(CapabilityKindId::new(0)),
                read(), read(), HostParameterRole::Value(ValueType::Size)],
            Vec::new(), HostMutation::ReadOnly,
        )?;
        self.imports.insert(import.id, import);
        Ok(self)
    }
    /// Terminal list diagnostics carry values and a bounded read-only label,
    /// not pointer authority. They do not replace an access's Bounds guard.
    pub fn with_list_bounds_import(mut self, signature: FunctionSignatureId, string_layout: LayoutId) -> Result<Self, String> {
        use crate::wir_prelude::WasmTy;
        let import = authenticated_import(
            "__witchy_list_bounds_at", &[WasmTy::I64, WasmTy::I32, WasmTy::I32, WasmTy::I32, WasmTy::I32],
            &[], signature, vec![
                HostParameterRole::Value(ValueType::Int), HostParameterRole::Value(ValueType::Size),
                HostParameterRole::Buffer { value_type: ValueType::Ref(string_layout),
                    access: AccessMode::Read, extent: HostBufferExtent::Slice },
                HostParameterRole::Value(ValueType::Size),
            ], Vec::new(), HostMutation::ReadOnly, ImportAuthentication::RuntimeDiagnostic,
        )?;
        self.imports.insert(import.id, import);
        Ok(self)
    }

    /// Terminal source diagnostics borrow two bounded UTF-8 slices. They carry
    /// no capability, no write authority, no result, and cannot reenter Wasm.
    pub fn with_abort_import(mut self, signature: FunctionSignatureId, string_layout: LayoutId) -> Result<Self, String> {
        use crate::wir_prelude::WasmTy;
        let read = || HostParameterRole::Buffer {
            value_type: ValueType::Ref(string_layout), access: AccessMode::Read,
            extent: HostBufferExtent::Slice,
        };
        let import = authenticated_import(
            "__witchy_abort_at", &[WasmTy::I32, WasmTy::I32, WasmTy::I32, WasmTy::I32, WasmTy::I32],
            &[], signature, vec![read(), read(), HostParameterRole::Value(ValueType::Size)],
            Vec::new(), HostMutation::ReadOnly, ImportAuthentication::RuntimeDiagnostic,
        )?;
        self.imports.insert(import.id, import);
        Ok(self)
    }

    /// Canonical output carries the actual Console value. Legacy buffer-only
    /// print is deliberately absent from this catalog; its existing factory
    /// remains available for frozen ABI fixtures. Output is non-privileged
    /// infrastructure, but its receiver still has a checked capability type.
    pub fn from_authenticated_console_prelude(
        print_signature: FunctionSignatureId,
        float_to_str_signature: FunctionSignatureId,
        string_layout: LayoutId,
    ) -> Result<Self, String> {
        use crate::wir_prelude::WasmTy;
        let mut imports = Self::from_authenticated_prelude(
            print_signature, float_to_str_signature, string_layout,
        )?.imports();
        imports.retain(|import| import.name != "print");
        imports.push(authenticated_prelude_import(
            "console_print",
            &[WasmTy::ExternRef, WasmTy::I32, WasmTy::I32],
            &[],
            print_signature,
            vec![
                HostParameterRole::Capability(CapabilityKindId::new(0)),
                HostParameterRole::Buffer {
                    value_type: ValueType::Ref(string_layout),
                    access: AccessMode::Read,
                    extent: HostBufferExtent::Slice,
                },
            ],
            Vec::new(),
            HostMutation::ReadOnly,
        )?);
        Ok(Self::from_imports(imports))
    }

    /// Project the canonical host imports from the generated prelude and its
    /// authenticated ABI metadata: `print`, a read-only byte sink, and
    /// `float_to_str`, which renders a `Float` into a guest buffer of at
    /// least [`FLOAT_RENDER_BUFFER_BYTES`] bytes and returns the byte length.
    /// The canonical signatures are semantic: the backend representation
    /// remains an implementation detail.
    pub fn from_authenticated_prelude(
        print_signature: FunctionSignatureId,
        float_to_str_signature: FunctionSignatureId,
        string_layout: LayoutId,
    ) -> Result<Self, String> {
        use crate::wir_prelude::WasmTy;
        let print = authenticated_prelude_import(
            "print",
            &[WasmTy::I32, WasmTy::I32],
            &[],
            print_signature,
            vec![HostParameterRole::Buffer {
                value_type: ValueType::Ref(string_layout),
                access: AccessMode::Read,
                extent: HostBufferExtent::Slice,
            }],
            Vec::new(),
            HostMutation::ReadOnly,
        )?;
        let float_to_str = authenticated_prelude_import(
            "float_to_str",
            &[WasmTy::F64, WasmTy::I32],
            &[WasmTy::I32],
            float_to_str_signature,
            vec![
                HostParameterRole::Value(ValueType::Float),
                HostParameterRole::Buffer {
                    value_type: ValueType::Ref(string_layout),
                    access: AccessMode::Write,
                    extent: HostBufferExtent::AtLeast(FLOAT_RENDER_BUFFER_BYTES),
                },
            ],
            vec![HostResultRole::Value(ValueType::Size)],
            HostMutation::WriteBuffers,
        )?;
        Ok(Self::from_imports(vec![print, float_to_str]))
    }

    /// Project the canonical `Dir`/`File`/`Env` operations onto an existing
    /// catalog. Each one is authenticated as a capability-authority import
    /// carrying exactly the authority it exercises: a path or name arrives as
    /// a checked read slice, a filled buffer as a checked write slice bounded
    /// by its capacity, and a narrowed `Dir` only ever as a host-minted token
    /// of [`DIR_CAPABILITY_KIND`].
    pub fn with_capability_imports(
        mut self,
        signatures: CapabilityImportSignatures,
        string_layout: LayoutId,
    ) -> Result<Self, String> {
        use crate::wir_prelude::{AbiImportAuthority, WasmTy};

        let read_slice = || HostParameterRole::Buffer {
            value_type: ValueType::Ref(string_layout),
            access: AccessMode::Read,
            extent: HostBufferExtent::Slice,
        };
        let write_slice = || HostParameterRole::Buffer {
            value_type: ValueType::Ref(string_layout),
            access: AccessMode::Write,
            extent: HostBufferExtent::Slice,
        };
        let dir = || HostParameterRole::Capability(DIR_CAPABILITY_KIND);
        let file = || HostParameterRole::Capability(FILE_CAPABILITY_KIND);
        let env = || HostParameterRole::Capability(ENV_CAPABILITY_KIND);
        let size = || vec![HostResultRole::Value(ValueType::Size)];

        let imports = vec![
            authenticated_capability_import(
                "dir_read_into",
                &[WasmTy::ExternRef, WasmTy::I32, WasmTy::I32, WasmTy::I32, WasmTy::I32],
                &[WasmTy::I32],
                signatures.dir_read_into,
                vec![dir(), read_slice(), write_slice()],
                size(),
                HostMutation::WriteBuffers,
                &[AbiImportAuthority::DirRead],
            )?,
            // The directory's admitted entry names, NUL-separated, into a
            // guest buffer (the guest splits them).
            authenticated_capability_import(
                "dir_list_into",
                &[WasmTy::ExternRef, WasmTy::I32, WasmTy::I32],
                &[WasmTy::I32],
                signatures.dir_list_into,
                vec![dir(), write_slice()],
                size(),
                HostMutation::WriteBuffers,
                &[AbiImportAuthority::DirRead],
            )?,
            authenticated_capability_import(
                "dir_exists_at",
                &[WasmTy::ExternRef, WasmTy::I32, WasmTy::I32],
                &[WasmTy::I32],
                signatures.dir_exists_at,
                vec![dir(), read_slice()],
                size(),
                HostMutation::ReadOnly,
                &[AbiImportAuthority::DirRead],
            )?,
            authenticated_capability_import(
                "dir_write_at",
                &[WasmTy::ExternRef, WasmTy::I32, WasmTy::I32, WasmTy::I32, WasmTy::I32],
                &[],
                signatures.dir_write_at,
                vec![dir(), read_slice(), read_slice()],
                Vec::new(),
                HostMutation::ReadOnly,
                &[AbiImportAuthority::DirWrite],
            )?,
            authenticated_capability_import(
                "dir_subtree_at",
                &[WasmTy::ExternRef, WasmTy::I32, WasmTy::I32],
                &[WasmTy::ExternRef],
                signatures.dir_subtree_at,
                vec![dir(), read_slice()],
                vec![HostResultRole::Capability(DIR_CAPABILITY_KIND)],
                HostMutation::ReadOnly,
                &[AbiImportAuthority::DirGrant],
            )?,
            authenticated_capability_import(
                "dir_read_file",
                &[WasmTy::ExternRef, WasmTy::I32, WasmTy::I32],
                &[WasmTy::ExternRef],
                signatures.dir_read_file,
                vec![dir(), read_slice()],
                vec![HostResultRole::Capability(FILE_CAPABILITY_KIND)],
                HostMutation::ReadOnly,
                &[AbiImportAuthority::DirRead],
            )?,
            authenticated_capability_import(
                "file_read_into",
                &[WasmTy::ExternRef, WasmTy::I32, WasmTy::I32],
                &[WasmTy::I32],
                signatures.file_read_into,
                vec![file(), write_slice()],
                size(),
                HostMutation::WriteBuffers,
                &[AbiImportAuthority::FileAuthority],
            )?,
            authenticated_capability_import(
                "env_has_var",
                &[WasmTy::ExternRef, WasmTy::I32, WasmTy::I32],
                &[WasmTy::I32],
                signatures.env_has_var,
                vec![env(), read_slice()],
                size(),
                HostMutation::ReadOnly,
                &[AbiImportAuthority::Env],
            )?,
            authenticated_capability_import(
                "env_get_into",
                &[WasmTy::ExternRef, WasmTy::I32, WasmTy::I32, WasmTy::I32, WasmTy::I32],
                &[WasmTy::I32],
                signatures.env_get_into,
                vec![env(), read_slice(), write_slice()],
                size(),
                HostMutation::WriteBuffers,
                &[AbiImportAuthority::Env],
            )?,
        ];
        for import in imports {
            self.imports.insert(import.id, import);
        }
        Ok(self)
    }

    /// Add pure, stateless compiler services. Inputs and output are checked
    /// UTF-8 byte slices; no capability token or staged host state is present.
    pub fn with_compiler_imports(
        mut self,
        signatures: CompilerImportSignatures,
        string_layout: LayoutId,
    ) -> Result<Self, String> {
        use crate::wir_prelude::WasmTy;
        let read = || HostParameterRole::Buffer {
            value_type: ValueType::Ref(string_layout), access: AccessMode::Read, extent: HostBufferExtent::Slice,
        };
        let write = || HostParameterRole::Buffer {
            value_type: ValueType::Ref(string_layout), access: AccessMode::Write, extent: HostBufferExtent::Slice,
        };
        let result = || vec![HostResultRole::Value(ValueType::Size)];
        let imports = vec![
            authenticated_internal_service_import(
                "compiler_footprint_into", &[WasmTy::I32, WasmTy::I32, WasmTy::I32, WasmTy::I32],
                &[WasmTy::I32], signatures.footprint_into, vec![read(), write()], result(), HostMutation::WriteBuffers,
            )?,
            authenticated_internal_service_import(
                "compiler_diff_into", &[WasmTy::I32, WasmTy::I32, WasmTy::I32, WasmTy::I32, WasmTy::I32, WasmTy::I32],
                &[WasmTy::I32], signatures.diff_into, vec![read(), read(), write()], result(), HostMutation::WriteBuffers,
            )?,
            authenticated_internal_service_import(
                "compiler_doc_into", &[WasmTy::I32; 6], &[WasmTy::I32],
                signatures.doc_into, vec![read(), read(), write()], result(), HostMutation::WriteBuffers,
            )?,
            authenticated_internal_service_import(
                "compiler_doc_result_json_into", &[WasmTy::I32; 6], &[WasmTy::I32],
                signatures.doc_result_json_into, vec![read(), read(), write()], result(), HostMutation::WriteBuffers,
            )?,
        ];
        for import in imports { self.imports.insert(import.id, import); }
        Ok(self)
    }

    pub fn imports(&self) -> Vec<ImportContract> {
        self.imports.values().cloned().collect()
    }

    /// Pure regex computation over two UTF-8 read slices and one bounded
    /// output slice. The result is a byte count, never allocation authority.
    /// Insufficient capacity leaves the entire output unchanged.
    pub fn with_regex_spans_import(
        mut self,
        signature: FunctionSignatureId,
        string_layout: LayoutId,
    ) -> Result<Self, String> {
        use crate::wir_prelude::WasmTy;
        let read = || HostParameterRole::Buffer {
            value_type: ValueType::Ref(string_layout), access: AccessMode::Read,
            extent: HostBufferExtent::Slice,
        };
        let import = authenticated_prelude_import(
            "regex_match_spans_into",
            &[WasmTy::I32, WasmTy::I32, WasmTy::I32, WasmTy::I32, WasmTy::I32, WasmTy::I32],
            &[WasmTy::I32], signature,
            vec![read(), read(), HostParameterRole::Buffer {
                value_type: ValueType::Ref(string_layout), access: AccessMode::Write,
                extent: HostBufferExtent::Slice,
            }],
            vec![HostResultRole::Value(ValueType::Size)], HostMutation::WriteBuffers,
        )?;
        self.imports.insert(import.id, import);
        Ok(self)
    }

    fn from_imports(imports: Vec<ImportContract>) -> Self {
        Self {
            imports: imports.into_iter().map(|import| (import.id, import)).collect(),
        }
    }
}

/// Stable identity for every judgment in the canonical SafeWIR kernel.
///
/// The checker and its evidence ledger key tests by this identity rather than
/// by diagnostic strings or backend instruction shapes. The exhaustive
/// `rule_id` matches below make adding an operation or terminator a compile
/// error until it is assigned a checker judgment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalRuleId {
    ConstUnit,
    ConstInt,
    ConstFloat,
    ConstBool,
    ConstSize,
    CapabilityNone,
    CapabilitySome,
    CapabilityIsPresent,
    CapabilityUnwrap,
    Arithmetic,
    Compare,
    NarrowSize,
    WidenSize,
    FloatUnary,
    Guard,
    ReauthenticateRawHeaderExtent,
    Alloc,
    StaticData,
    Load,
    InitNullableOwned,
    Store,
    /// Not an operation of its own: the extra judgment a `Load` or `Store` of
    /// a closure object's callable lane must pass. It has a rule row because
    /// the lane's value type comes from the module's closure-layout registry
    /// rather than from the lane itself.
    ClosureSlot,
    CopyBytes,
    MoveRange,
    CloneRange,
    UpdateCount,
    MoveValue,
    MakeInlineAggregate,
    ExtractInlineField,
    CloneRef,
    Retain,
    Release,
    DropRef,
    Reclaim,
    FunctionValue,
    Call,
    VarCall,
    ExternalCall,
    HostCall,
    Branch,
    BranchIf,
    Return,
    TailCall,
    VarTailCall,
    Trap,
    Unreachable,
    ClosureOwner,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    ClosureOwner(closure_owner_contract::OwnerOperation),
    /// The sole Unit value, with no integer or Bool authority.
    ConstUnit { destination: ValueId },
    ConstInt { destination: ValueId, value: i64 },
    ConstFloat { destination: ValueId, bits: u64 },
    ConstBool { destination: ValueId, value: bool },
    ConstSize { destination: ValueId, value: u64 },
    CapabilityNone { destination: ValueId, kind: CapabilityKindId },
    CapabilitySome { destination: ValueId, source: ValueId },
    CapabilityIsPresent { destination: ValueId, source: ValueId },
    CapabilityUnwrap { destination: ValueId, source: ValueId },
    Arithmetic {
        destination: ValueId,
        operator: ArithmeticOperator,
        left: ValueId,
        right: ValueId,
        overflow: OverflowBehavior,
    },
    Compare {
        destination: ValueId,
        operator: ComparisonOperator,
        left: ValueId,
        right: ValueId,
    },
    NarrowSize { destination: ValueId, value: ValueId },
    WidenSize { destination: ValueId, value: ValueId },
    /// A unary float operation: `Sqrt` maps a `Float` to a `Float`;
    /// `TruncateToInt` maps a `Float` to an `Int` toward zero, saturating at
    /// the `Int` bounds and mapping NaN to zero (the lowering traps on NaN
    /// before it when the language semantics abort); `FromInt` maps an `Int`
    /// to a `Float`.
    FloatUnary { destination: ValueId, operator: FloatUnaryOperator, value: ValueId },
    Guard { condition: ValueId, kind: GuardKind },
    /// Proof-only: establish a current counted extent for a raw byte buffer
    /// after the checker verifies the explicit fresh-header proof sequence.
    ReauthenticateRawHeaderExtent { object: ValueId, header: ValueId },
    Alloc {
        destination: ValueId,
        layout: LayoutId,
        extent: Extent,
        class: AllocationClass,
    },
    StaticData {
        destination: ValueId,
        layout: LayoutId,
        extent: Extent,
        data: StaticDataId,
    },
    Load {
        destination: ValueId,
        place: Place,
        transfer: LoadTransfer,
    },
    InitNullableOwned { place: Place },
    Store {
        place: Place,
        value: ValueId,
        transfer: StoreTransfer,
    },
    /// Raw bytes only. The checker rejects layouts containing references.
    CopyBytes { destination: Place, source: Place, length: ValueId },
    /// Transfers ownership of each typed element and consumes the source range.
    MoveRange { destination: Place, source: Place, count: ValueId },
    /// Clones each typed element, including required retains for shared refs.
    CloneRange { destination: Place, source: Place, count: ValueId },
    UpdateCount { object: ValueId, count: ValueId },
    MoveValue { destination: ValueId, source: ValueId },
    /// Construct an authenticated fixed-layout scalar aggregate. It is an SSA
    /// value, never a linear-memory object.
    MakeInlineAggregate {
        destination: ValueId,
        layout: LayoutId,
        fields: Vec<ValueId>,
    },
    /// Mechanically select one scalar lane from an inline aggregate.
    ExtractInlineField {
        destination: ValueId,
        aggregate: ValueId,
        index: u32,
    },
    /// Create a second affine token for reference-counted storage. Both the
    /// source and destination become Shared and must be consumed separately.
    CloneRef { destination: ValueId, source: ValueId },
    Retain { object: ValueId },
    Release { object: ValueId },
    /// Consume one affine reference-counted token. The runtime reclaims only
    /// when this decrement reaches zero.
    DropRef { object: ValueId },
    Reclaim { object: ValueId },
    /// Construct a callable value from an authenticated function table entry.
    /// The checker derives the exact signature from the referenced function;
    /// this is never represented by a scalar carrier.
    FunctionValue { destination: ValueId, function: FunctionId },
    Call {
        destinations: Vec<ValueId>,
        callee: FunctionId,
        arguments: Vec<CallArgument>,
    },
    VarCall {
        destinations: Vec<ValueId>,
        callee: ValueId,
        signature: FunctionSignatureId,
        arguments: Vec<CallArgument>,
    },
    /// Direct invocation of an authenticated separately compiled scalar Wasm
    /// export. This is not `HostCall`: no host capability or caller-memory
    /// buffer can cross this boundary.
    ExternalCall {
        destinations: Vec<ValueId>,
        import: ImportId,
        arguments: Vec<CallArgument>,
    },
    HostCall {
        destinations: Vec<ValueId>,
        import: ImportId,
        arguments: Vec<HostCallArgument>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Terminator {
    Branch { target: BlockId, arguments: Vec<ValueId> },
    BranchIf {
        condition: ValueId,
        then_target: BlockId,
        then_arguments: Vec<ValueId>,
        else_target: BlockId,
        else_arguments: Vec<ValueId>,
    },
    Return { values: Vec<ValueId> },
    /// Proper tail position is semantic, not an optional optimization.
    TailCall {
        site: TailSiteId,
        callee: FunctionId,
        arguments: Vec<CallArgument>,
    },
    VarTailCall {
        site: TailSiteId,
        callee: ValueId,
        signature: FunctionSignatureId,
        arguments: Vec<CallArgument>,
    },
    Trap,
    Unreachable,
}

impl Operation {
    pub const fn rule_id(&self) -> CanonicalRuleId {
        match self {
            Self::ClosureOwner(_) => CanonicalRuleId::ClosureOwner,
            Self::ConstUnit { .. } => CanonicalRuleId::ConstUnit,
            Self::ConstInt { .. } => CanonicalRuleId::ConstInt,
            Self::ConstFloat { .. } => CanonicalRuleId::ConstFloat,
            Self::ConstBool { .. } => CanonicalRuleId::ConstBool,
            Self::ConstSize { .. } => CanonicalRuleId::ConstSize,
            Self::CapabilityNone { .. } => CanonicalRuleId::CapabilityNone,
            Self::CapabilitySome { .. } => CanonicalRuleId::CapabilitySome,
            Self::CapabilityIsPresent { .. } => CanonicalRuleId::CapabilityIsPresent,
            Self::CapabilityUnwrap { .. } => CanonicalRuleId::CapabilityUnwrap,
            Self::Arithmetic { .. } => CanonicalRuleId::Arithmetic,
            Self::Compare { .. } => CanonicalRuleId::Compare,
            Self::NarrowSize { .. } => CanonicalRuleId::NarrowSize,
            Self::WidenSize { .. } => CanonicalRuleId::WidenSize,
            Self::FloatUnary { .. } => CanonicalRuleId::FloatUnary,
            Self::Guard { .. } => CanonicalRuleId::Guard,
            Self::ReauthenticateRawHeaderExtent { .. } => CanonicalRuleId::ReauthenticateRawHeaderExtent,
            Self::Alloc { .. } => CanonicalRuleId::Alloc,
            Self::StaticData { .. } => CanonicalRuleId::StaticData,
            Self::Load { .. } => CanonicalRuleId::Load,
            Self::InitNullableOwned { .. } => CanonicalRuleId::InitNullableOwned,
            Self::Store { .. } => CanonicalRuleId::Store,
            Self::CopyBytes { .. } => CanonicalRuleId::CopyBytes,
            Self::MoveRange { .. } => CanonicalRuleId::MoveRange,
            Self::CloneRange { .. } => CanonicalRuleId::CloneRange,
            Self::UpdateCount { .. } => CanonicalRuleId::UpdateCount,
            Self::MoveValue { .. } => CanonicalRuleId::MoveValue,
            Self::MakeInlineAggregate { .. } => CanonicalRuleId::MakeInlineAggregate,
            Self::ExtractInlineField { .. } => CanonicalRuleId::ExtractInlineField,
            Self::CloneRef { .. } => CanonicalRuleId::CloneRef,
            Self::Retain { .. } => CanonicalRuleId::Retain,
            Self::Release { .. } => CanonicalRuleId::Release,
            Self::DropRef { .. } => CanonicalRuleId::DropRef,
            Self::Reclaim { .. } => CanonicalRuleId::Reclaim,
            Self::FunctionValue { .. } => CanonicalRuleId::FunctionValue,
            Self::Call { .. } => CanonicalRuleId::Call,
            Self::VarCall { .. } => CanonicalRuleId::VarCall,
            Self::ExternalCall { .. } => CanonicalRuleId::ExternalCall,
            Self::HostCall { .. } => CanonicalRuleId::HostCall,
        }
    }
}

impl Terminator {
    pub const fn rule_id(&self) -> CanonicalRuleId {
        match self {
            Self::Branch { .. } => CanonicalRuleId::Branch,
            Self::BranchIf { .. } => CanonicalRuleId::BranchIf,
            Self::Return { .. } => CanonicalRuleId::Return,
            Self::TailCall { .. } => CanonicalRuleId::TailCall,
            Self::VarTailCall { .. } => CanonicalRuleId::VarTailCall,
            Self::Trap => CanonicalRuleId::Trap,
            Self::Unreachable => CanonicalRuleId::Unreachable,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockParameter {
    pub value: ValueId,
    pub value_type: ValueType,
    pub ownership: Option<Ownership>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BasicBlock {
    pub id: BlockId,
    pub parameters: Vec<BlockParameter>,
    pub operations: Vec<Operation>,
    pub terminator: Terminator,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    pub id: FunctionId,
    pub signature: FunctionSignatureId,
    pub entry: BlockId,
    /// The canonical type of every SSA definition in this function. Operations
    /// do not infer a type from a Wasm carrier or instruction spelling.
    pub value_types: BTreeMap<ValueId, ValueType>,
    pub blocks: Vec<BasicBlock>,
}

/// Independently derived from typed source. Initial validation requires a
/// bijection between these obligations and SafeWIR tail-call terminators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProperTailSite {
    pub id: TailSiteId,
    /// The checked callable the site belongs to (the identity the checked
    /// manifest digest is computed over).
    pub function: FunctionId,
    /// The lowered function whose terminator discharges the site when it is
    /// not `function` itself: the function synthesized for a lambda whose
    /// body holds the site. `None` for an ordinary site.
    pub discharged_by: Option<FunctionId>,
}

/// Exact canonical control location to which one checked source proper-tail
/// obligation lowered.
///
/// This is a lowering-authored binding, not independent evidence that lowering
/// selected the correct source branch. It prevents a later transformation from
/// substituting another reachable tail terminator for the one lowering bound.
/// Independent source-to-CFG correctness remains an obligation of canonical
/// lowering (or a future translation validator).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProperTailLocation {
    site: TailSiteId,
    function: FunctionId,
    block: BlockId,
}

impl ProperTailLocation {
    pub const fn new(site: TailSiteId, function: FunctionId, block: BlockId) -> Self {
        Self { site, function, block }
    }

    pub const fn site(self) -> TailSiteId { self.site }
    pub const fn function(self) -> FunctionId { self.function }
    pub const fn block(self) -> BlockId { self.block }
}

/// Immutable bytes installed by the canonical backend before heap allocation.
/// Segment identity is part of the authenticated module digest; callers can
/// only borrow the bytes after construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticDataSegment {
    id: StaticDataId,
    bytes: Vec<u8>,
}

impl StaticDataSegment {
    pub fn new(id: StaticDataId, bytes: Vec<u8>) -> Self {
        Self { id, bytes }
    }

    pub const fn id(&self) -> StaticDataId {
        self.id
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweredModuleParts {
    pub revision: Revision,
    pub checked_source: CheckedSourceIdentity,
    pub layouts: LayoutBundle,
    pub signatures: Vec<FunctionSignature>,
    pub imports: Vec<ImportContract>,
    pub functions: Vec<Function>,
    pub proper_tail_sites: Vec<ProperTailSite>,
}

/// A source declaration selected explicitly by lowering, never inferred from a
/// backend function ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateAbiBinding {
    function: FunctionId,
    source_declaration_ordinal: u32,
}

impl CandidateAbiBinding {
    pub fn new(
        function: FunctionId,
        source_declaration_ordinal: u32,
    ) -> Self {
        Self { function, source_declaration_ordinal }
    }

    pub const fn function(&self) -> FunctionId { self.function }
    pub const fn source_declaration_ordinal(&self) -> u32 { self.source_declaration_ordinal }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CandidateAbiBindings {
    entries: Vec<CandidateAbiBinding>,
}

impl CandidateAbiBindings {
    pub fn new(entries: Vec<CandidateAbiBinding>) -> Self { Self { entries } }
    pub fn entries(&self) -> &[CandidateAbiBinding] { &self.entries }
}

/// A lowered function that implements one capture-free lambda of the checked
/// source, selected by the lambda's index in the closure witness manifest.
/// The authority admits it only when that witness has no captures and its
/// signature matches the function's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateClosureBinding {
    function: FunctionId,
    closure: u32,
}

impl CandidateClosureBinding {
    pub fn new(function: FunctionId, closure: u32) -> Self {
        Self { function, closure }
    }

    pub const fn function(&self) -> FunctionId { self.function }
    pub const fn closure(&self) -> u32 { self.closure }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CandidateClosureBindings {
    entries: Vec<CandidateClosureBinding>,
}

impl CandidateClosureBindings {
    pub fn new(entries: Vec<CandidateClosureBinding>) -> Self { Self { entries } }
    pub fn entries(&self) -> &[CandidateClosureBinding] { &self.entries }
}

/// A lowered function that adapts one named source function to the
/// environment-taking closure ABI: it takes the closure object as its leading
/// parameter, ignores it, and forwards the rest to the target. The authority
/// admits it only when the body is exactly that forwarding call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateAdapterBinding {
    adapter: FunctionId,
    target: FunctionId,
}

impl CandidateAdapterBinding {
    pub fn new(adapter: FunctionId, target: FunctionId) -> Self {
        Self { adapter, target }
    }

    pub const fn adapter(&self) -> FunctionId { self.adapter }
    pub const fn target(&self) -> FunctionId { self.target }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CandidateAdapterBindings {
    entries: Vec<CandidateAdapterBinding>,
}

impl CandidateAdapterBindings {
    pub fn new(entries: Vec<CandidateAdapterBinding>) -> Self { Self { entries } }
    pub fn entries(&self) -> &[CandidateAdapterBinding] { &self.entries }
}

mod stage {
    pub trait Sealed {}
}

pub trait ModuleStage: stage::Sealed {}

#[derive(Debug, Clone, Copy)]
pub enum LoweredCandidate {}
impl stage::Sealed for LoweredCandidate {}
impl ModuleStage for LoweredCandidate {}

#[derive(Debug, Clone, Copy)]
pub enum InitialValid {}
impl stage::Sealed for InitialValid {}
impl ModuleStage for InitialValid {}

/// A transformed graph that carries no validation or encoder authority.
/// Only the canonical checker can promote it back to `InitialValid`.
#[derive(Debug, Clone, Copy)]
pub(crate) enum OptimizationCandidate {}
impl stage::Sealed for OptimizationCandidate {}
impl ModuleStage for OptimizationCandidate {}

/// One representation with typestate authority. Its fields are private so a
/// validated module cannot be assembled or retagged outside this module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalModule<S: ModuleStage> {
    revision: Revision,
    checked_source: CheckedSourceIdentity,
    launch_contract_payload: Vec<u8>,
    layouts: LayoutBundle,
    signatures: Vec<FunctionSignature>,
    imports: Vec<ImportContract>,
    functions: Vec<Function>,
    proper_tail_sites: Vec<ProperTailSite>,
    proper_tail_locations: Vec<ProperTailLocation>,
    static_data_segments: Vec<StaticDataSegment>,
    abi_catalog: Option<CanonicalAbiCatalog>,
    abi_bindings: CandidateAbiBindings,
    closure_bindings: CandidateClosureBindings,
    adapter_bindings: CandidateAdapterBindings,
    /// The signature every closure object of one layout carries in slot 0.
    /// A `Load` or `Store` of that lane is typed from this registry alone; a
    /// layout absent from it has no readable callable slot.
    closure_layouts: BTreeMap<LayoutId, FunctionSignatureId>,
    owner_families: Vec<closure_owner_contract::OwnerFamily>,
    vm_services: Vec<CandidateVmService>,
    stage: PhantomData<fn() -> S>,
}

/// Lowered candidates can only acquire validation authority through the
/// canonical checker; clients cannot invoke the typestate transition.
///
/// ```compile_fail
/// use witchy_wir::canonical_safewir::{CanonicalModule, InitialValid, LoweredCandidate};
///
/// fn forge(
///     candidate: CanonicalModule<LoweredCandidate>,
/// ) -> CanonicalModule<InitialValid> {
///     candidate.into_initial_valid()
/// }
/// ```
impl CanonicalModule<LoweredCandidate> {
    pub fn from_lowering(parts: LoweredModuleParts) -> Self {
        Self::from_lowering_with_static_data(parts, Vec::new())
    }

    /// Construct a candidate with module-level immutable static data. This is
    /// intentionally separate from source lowering: only canonical producers
    /// that already own the exact bytes may bind them here.
    pub fn from_lowering_with_static_data(
        parts: LoweredModuleParts,
        static_data_segments: Vec<StaticDataSegment>,
    ) -> Self {
        Self {
            revision: parts.revision,
            checked_source: parts.checked_source,
            launch_contract_payload: Vec::new(),
            layouts: parts.layouts,
            signatures: parts.signatures,
            imports: parts.imports,
            functions: parts.functions,
            proper_tail_sites: parts.proper_tail_sites,
            proper_tail_locations: Vec::new(),
            static_data_segments,
            abi_catalog: None,
            abi_bindings: CandidateAbiBindings::default(),
            closure_bindings: CandidateClosureBindings::default(),
            adapter_bindings: CandidateAdapterBindings::default(),
            closure_layouts: BTreeMap::new(),
            owner_families: Vec::new(),
            vm_services: Vec::new(),
            stage: PhantomData,
        }
    }

    pub fn with_vm_services(mut self, services: Vec<CandidateVmService>) -> Self {
        self.vm_services = services;
        self
    }

    /// Apply a construction-time transformation to an authority-free lowered
    /// candidate. The initial checker still re-establishes every invariant.
    pub fn transform_functions(mut self, transform: impl FnOnce(&mut [Function])) -> Self {
        transform(&mut self.functions);
        self
    }

    pub fn with_abi_catalog(mut self, catalog: CanonicalAbiCatalog) -> Self {
        self.abi_catalog = Some(catalog);
        self
    }

    pub fn with_owner_families(mut self, families: Vec<closure_owner_contract::OwnerFamily>) -> Self {
        self.owner_families = families;
        self
    }

    /// Bind exact source-derived launch metadata before initial validation. It
    /// becomes part of SafeWIR identity and cannot be appended after admission.
    pub fn with_launch_contract_payload(mut self, payload: Vec<u8>) -> Self {
        self.launch_contract_payload = payload;
        self
    }

    /// Attach the source-derived local ABI projection emitted by canonical
    /// lowering. Production source admission rejects a missing or incomplete
    /// projection; native hand-built test candidates retain the empty default.
    pub fn with_source_abi_bindings(mut self, bindings: CandidateAbiBindings) -> Self {
        self.abi_bindings = bindings;
        self
    }

    /// Attach the closure-witness bindings of the functions canonical
    /// lowering synthesized for capture-free lambdas.
    pub fn with_closure_bindings(mut self, bindings: CandidateClosureBindings) -> Self {
        self.closure_bindings = bindings;
        self
    }

    /// Attach the adapter bindings of the functions canonical lowering
    /// synthesized for named functions used as values.
    pub fn with_adapter_bindings(mut self, bindings: CandidateAdapterBindings) -> Self {
        self.adapter_bindings = bindings;
        self
    }

    /// Attach lowering-derived source-site bindings before initial validation.
    ///
    /// This operation exists only for [`CanonicalModule<LoweredCandidate>`].
    /// Once the initial checker has minted validated authority, neither an
    /// optimizer nor another later stage can rewrite the bindings together with
    /// a terminator:
    ///
    /// ```compile_fail
    /// use witchy_wir::canonical_safewir::{
    ///     CanonicalModule, InitialValid, ProperTailLocation,
    /// };
    ///
    /// fn rewrite_validated_binding(
    ///     module: CanonicalModule<InitialValid>,
    ///     locations: Vec<ProperTailLocation>,
    /// ) {
    ///     let _ = module.with_proper_tail_locations(locations);
    /// }
    /// ```
    ///
    /// The binding establishes substitution resistance after lowering. It does
    /// not independently establish that lowering mapped the source site to the
    /// correct CFG location.
    pub fn with_proper_tail_locations(mut self, locations: Vec<ProperTailLocation>) -> Self {
        self.proper_tail_locations = locations;
        self
    }

    /// Declare the closure-object layouts: each `PackedRecord` whose slot 0 is
    /// the callable lane, mapped to the environment-taking signature that lane
    /// holds. The checker types every callable slot from this map.
    pub fn with_closure_layouts(
        mut self,
        closure_layouts: BTreeMap<LayoutId, FunctionSignatureId>,
    ) -> Self {
        self.closure_layouts = closure_layouts;
        self
    }

    fn into_initial_valid(self) -> CanonicalModule<InitialValid> {
        self.into_stage()
    }
}

impl<S: ModuleStage> CanonicalModule<S> {
    fn into_stage<T: ModuleStage>(self) -> CanonicalModule<T> {
        CanonicalModule {
            revision: self.revision,
            checked_source: self.checked_source,
            launch_contract_payload: self.launch_contract_payload,
            layouts: self.layouts,
            signatures: self.signatures,
            imports: self.imports,
            functions: self.functions,
            proper_tail_sites: self.proper_tail_sites,
            proper_tail_locations: self.proper_tail_locations,
            static_data_segments: self.static_data_segments,
            abi_catalog: self.abi_catalog,
            abi_bindings: self.abi_bindings,
            closure_bindings: self.closure_bindings,
            adapter_bindings: self.adapter_bindings,
            closure_layouts: self.closure_layouts,
            owner_families: self.owner_families,
            vm_services: self.vm_services,
            stage: PhantomData,
        }
    }

    pub const fn revision(&self) -> Revision {
        self.revision
    }

    pub const fn checked_source(&self) -> CheckedSourceIdentity {
        self.checked_source
    }

    pub fn launch_contract_payload(&self) -> &[u8] {
        &self.launch_contract_payload
    }

    pub const fn layouts(&self) -> &LayoutBundle {
        &self.layouts
    }

    pub fn signatures(&self) -> &[FunctionSignature] {
        &self.signatures
    }

    pub fn imports(&self) -> &[ImportContract] {
        &self.imports
    }

    pub fn functions(&self) -> &[Function] {
        &self.functions
    }

    pub fn proper_tail_sites(&self) -> &[ProperTailSite] {
        &self.proper_tail_sites
    }

    pub fn proper_tail_locations(&self) -> &[ProperTailLocation] {
        &self.proper_tail_locations
    }

    pub fn static_data_segments(&self) -> &[StaticDataSegment] {
        &self.static_data_segments
    }

    pub(crate) fn abi_catalog(&self) -> Option<&CanonicalAbiCatalog> {
        self.abi_catalog.as_ref()
    }

    pub fn source_abi_bindings(&self) -> &CandidateAbiBindings {
        &self.abi_bindings
    }

    pub fn closure_bindings(&self) -> &CandidateClosureBindings {
        &self.closure_bindings
    }

    pub fn adapter_bindings(&self) -> &CandidateAdapterBindings {
        &self.adapter_bindings
    }

    pub fn closure_layouts(&self) -> &BTreeMap<LayoutId, FunctionSignatureId> {
        &self.closure_layouts
    }

    pub fn vm_services(&self) -> &[CandidateVmService] { &self.vm_services }

    pub fn owner_families(&self) -> &[closure_owner_contract::OwnerFamily] {
        &self.owner_families
    }
}

#[cfg(feature = "raw-encoder-test-api")]
#[doc(hidden)]
pub mod self_checks {
    use super::*;

    const STRING_LAYOUT: LayoutId = LayoutId::from_bytes([5; 32]);

    fn signatures() -> CapabilityImportSignatures {
        CapabilityImportSignatures {
            dir_read_into: FunctionSignatureId::new(1),
            dir_list_into: FunctionSignatureId::new(2),
            dir_exists_at: FunctionSignatureId::new(2),
            dir_write_at: FunctionSignatureId::new(3),
            dir_subtree_at: FunctionSignatureId::new(4),
            dir_read_file: FunctionSignatureId::new(5),
            file_read_into: FunctionSignatureId::new(6),
            env_has_var: FunctionSignatureId::new(7),
            env_get_into: FunctionSignatureId::new(8),
        }
    }

    fn catalog() -> CanonicalAbiCatalog {
        CanonicalAbiCatalog::from_authenticated_prelude(
            FunctionSignatureId::new(0),
            FunctionSignatureId::new(7),
            STRING_LAYOUT,
        )
        .expect("authenticated prelude")
        .with_capability_imports(signatures(), STRING_LAYOUT)
        .expect("authenticated capability imports")
    }




    pub fn compiler_imports_are_authenticated_internal_buffer_services() {
        let catalog = catalog()
            .with_compiler_imports(
                CompilerImportSignatures {
                    footprint_into: FunctionSignatureId::new(8),
                    diff_into: FunctionSignatureId::new(9),
                    doc_into: FunctionSignatureId::new(10),
                    doc_result_json_into: FunctionSignatureId::new(11),
                },
                STRING_LAYOUT,
            )
            .expect("authenticated compiler imports");
        let imports = catalog.imports();
        let footprint = imports.iter()
            .find(|import| import.name == "compiler_footprint_into")
            .expect("footprint import");
        assert!(matches!(footprint.abi_identity, ImportContractIdentity::AuthenticatedAbi(_)));
        assert_eq!(footprint.reentrancy, HostReentrancy::Forbidden);
        assert_eq!(footprint.mutation, HostMutation::WriteBuffers);
        assert_eq!(footprint.parameter_roles.len(), 2);
        assert!(matches!(footprint.parameter_roles[0], HostParameterRole::Buffer { access: AccessMode::Read, extent: HostBufferExtent::Slice, .. }));
        assert!(matches!(footprint.parameter_roles[1], HostParameterRole::Buffer { access: AccessMode::Write, extent: HostBufferExtent::Slice, .. }));
        assert_eq!(footprint.result_roles, vec![HostResultRole::Value(ValueType::Size)]);

        let diff = imports.iter()
            .find(|import| import.name == "compiler_diff_into")
            .expect("diff import");
        assert_eq!(diff.parameter_roles.len(), 3);
        assert!(matches!(diff.parameter_roles[0], HostParameterRole::Buffer { access: AccessMode::Read, .. }));
        assert!(matches!(diff.parameter_roles[1], HostParameterRole::Buffer { access: AccessMode::Read, .. }));
        assert!(matches!(diff.parameter_roles[2], HostParameterRole::Buffer { access: AccessMode::Write, .. }));
        for (name, signature) in [
            ("compiler_doc_into", FunctionSignatureId::new(10)),
            ("compiler_doc_result_json_into", FunctionSignatureId::new(11)),
        ] {
            let doc = imports.iter().find(|import| import.name == name).expect("doc import");
            assert_eq!(doc.signature, signature);
            assert!(matches!(doc.abi_identity, ImportContractIdentity::AuthenticatedAbi(_)));
            assert_eq!(doc.reentrancy, HostReentrancy::Forbidden);
            assert_eq!(doc.mutation, HostMutation::WriteBuffers);
            assert_eq!(doc.parameter_roles, [AccessMode::Read, AccessMode::Read, AccessMode::Write]
                .into_iter().map(|access| HostParameterRole::Buffer {
                    value_type: ValueType::Ref(STRING_LAYOUT), access, extent: HostBufferExtent::Slice,
                }).collect::<Vec<_>>());
            assert_eq!(doc.result_roles, vec![HostResultRole::Value(ValueType::Size)]);
        }
    }

}
