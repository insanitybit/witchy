//! Staged typed closure-owner boundary, NOT an executable admission path.
//!
//! An affected family's values are opaque externrefs. Its list elements are
//! actual owner references in typed GC storage, never handles in linear bytes.
//! The owner roots a typed GC payload containing actual capability externrefs
//! and the existing linear environment's strong RC root. Kernel construction
//! allocates/initializes that environment; host sealing performs no guest call.
//!
//! These candidate operations are carried by `Operation::ClosureOwner`.
//! The checker and encoder continue to refuse this representation
//! until source correspondence, private bridge registration, GC storage, and
//! explicit mixed-resource destruction have been reviewed and implemented.
//!
//! Registered owner/list `Gc` call contracts use `ParameterOwnership::Shared`
//! and `CallArgument::Move`: source let clones before transfer, source own
//! transfers its existing token. `ResultContract::Value(ValueType::Gc(id))`
//! transfers one owned opaque token, not a copyable scalar or linear-memory
//! reference. Registration and token liveness must be checked independently.
//! Unregistered GC types and owner/list WriteBack are not admitted by this ABI.

use super::{CapabilityKindId, FunctionId, FunctionSignatureId, GcTypeId, ValueId, ValueType};
use crate::layout::LayoutId;
use witchy_canonical_source::closure::ClosureCaptureTransfer;
use witchy_canonical_source::closure::ClosureWitness;
use witchy_canonical_source::source::CanonicalSourceView;

/// Static candidate metadata, never an SSA operand or guest bridge argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConstructorId(pub u32);

/// Static ordinal in the checked witness's capture vector, not a runtime key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureOrdinal(pub u32);

/// Source capture semantics. Managed includes both existing RC storage and
/// affected-family owner references; projection must establish which applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureTransfer {
    CopyScalar,
    CopyCapability(CapabilityKindId),
    CloneManaged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContractError {
    MissingWitness,
    UnsupportedCapture,
    NotCapabilityCapture,
    PureCapabilityCapture,
}

/// A borrowed view of the existing checked product, not a new receipt/token.
/// Candidate metadata must be compared with this source, never trusted itself.
pub struct SourceConstructor<'a> {
    source: CanonicalSourceView<'a>,
    witness: &'a ClosureWitness,
    transfers: Vec<CaptureTransfer>,
}

impl<'a> SourceConstructor<'a> {
    pub fn from_checked(source: &CanonicalSourceView<'a>, ordinal: ConstructorId) -> Result<Self, ContractError> {
        use witchy_canonical_source::capabilities::{CapabilityKind, CapabilityRepresentation};
        let witness = source.closure_witness().entries().get(ordinal.0 as usize)
            .ok_or(ContractError::MissingWitness)?;
        let definitions = source.type_definitions();
        let transfers = witness.captures().iter().map(|capture| match capture.transfer() {
            ClosureCaptureTransfer::CopyScalar => Ok(CaptureTransfer::CopyScalar),
            ClosureCaptureTransfer::ManagedReferenceByValue => Ok(CaptureTransfer::CloneManaged),
            // The view witness is distinct source authority, while the only
            // executable transfer is a managed retain of that opaque carrier.
            // A `&Float`/`&Bool`/`&Duration` view is carried as its immutable
            // scalar value (`ScalarValue`): it has no carrier to retain.
            ClosureCaptureTransfer::RootedViewByValue if matches!(capture.checked_type(),
                witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::View, inner)
                    if witchy_canonical_source::aggregates::is_scalar_value_view_payload(inner)) =>
            {
                Ok(CaptureTransfer::CopyScalar)
            }
            ClosureCaptureTransfer::RootedViewByValue => Ok(CaptureTransfer::CloneManaged),
            ClosureCaptureTransfer::ExplicitOwnedMove => Err(ContractError::UnsupportedCapture),
            ClosureCaptureTransfer::CopyCapability => {
                let Some(CapabilityRepresentation::Direct(kind)) =
                    witchy_canonical_source::capabilities::representation(capture.checked_type(), &definitions)
                else { return Err(ContractError::UnsupportedCapture) };
                Ok(CaptureTransfer::CopyCapability(match kind {
                    CapabilityKind::Console => CapabilityKindId::new(0),
                    CapabilityKind::Dir => super::DIR_CAPABILITY_KIND,
                    CapabilityKind::Env => super::ENV_CAPABILITY_KIND,
                    CapabilityKind::File => super::FILE_CAPABILITY_KIND,
                    CapabilityKind::Net => super::NET_CAPABILITY_KIND,
                    CapabilityKind::Listener => super::LISTENER_CAPABILITY_KIND,
                    CapabilityKind::Socket => super::SOCKET_CAPABILITY_KIND,
                    CapabilityKind::Exec => super::EXEC_CAPABILITY_KIND,
                    CapabilityKind::Fetch => super::FETCH_CAPABILITY_KIND,
                    CapabilityKind::Clock => super::CLOCK_CAPABILITY_KIND,
                    CapabilityKind::SecretStore => super::SECRET_STORE_CAPABILITY_KIND,
                    CapabilityKind::Secret => super::SECRET_CAPABILITY_KIND,
                    CapabilityKind::BuildOut => super::BUILD_OUT_CAPABILITY_KIND,
                    CapabilityKind::BuildRead => super::BUILD_READ_CAPABILITY_KIND,
                    CapabilityKind::BuildEnv => super::BUILD_ENV_CAPABILITY_KIND,
                    CapabilityKind::BuildExec => super::BUILD_EXEC_CAPABILITY_KIND,
                    CapabilityKind::BuildNet => super::BUILD_NET_CAPABILITY_KIND,
                }))
            }
        }).collect::<Result<Vec<_>, _>>()?;
        if transfers.iter().any(|transfer| matches!(transfer, CaptureTransfer::CopyCapability(_)))
            && matches!(witness.signature().cloned(),
                Some(witchy_syntax::ast::Type::Fn(_, _, _, qualifiers)) if qualifiers.pure)
        {
            return Err(ContractError::PureCapabilityCapture);
        }
        Ok(Self { source: *source, witness, transfers })
    }

    pub fn source(&self) -> &CanonicalSourceView<'a> { &self.source }
    pub fn witness(&self) -> &ClosureWitness { self.witness }
    pub fn transfers(&self) -> &[CaptureTransfer] { &self.transfers }

    pub fn capability_read(&self, capture: CaptureOrdinal) -> Result<BridgeAbi, ContractError> {
        let Some(CaptureTransfer::CopyCapability(kind)) = self.transfers.get(capture.0 as usize) else {
            return Err(ContractError::NotCapabilityCapture);
        };
        Ok(BridgeAbi {
            parameters: vec![BridgeValue::OpaqueOwner],
            results: vec![BridgeValue::Capability(*kind)],
        })
    }
}

/// Semantic roles, not generated import names. Registration must bind these
/// roles AND the full checked witness/signature to the admitted instance.
/// No role converts an integer/address/generation to an owner or capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeValue {
    OpaqueOwner,
    TypedGcPayload,
    Capability(CapabilityKindId),
    /// None for a non-final release; Some exactly once on final release.
    FinalPayload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeAbi {
    pub parameters: Vec<BridgeValue>,
    pub results: Vec<BridgeValue>,
}

impl BridgeAbi {
    /// The payload is constructed inside the atomic Construct kernel lowering;
    /// a generic HostCall or external module must not invoke this bridge.
    /// Sealing roots the actual GC object and captures, checks their kinds, and
    /// binds instance/witness identity. No caller-provided callback is accepted.
    pub fn seal() -> Self {
        Self { parameters: vec![BridgeValue::TypedGcPayload], results: vec![BridgeValue::OpaqueOwner] }
    }

    pub fn retain() -> Self {
        Self { parameters: vec![BridgeValue::OpaqueOwner], results: vec![BridgeValue::OpaqueOwner] }
    }

    /// Retire the owner before returning its payload. The enclosing Drop
    /// kernel must destroy its linear RC environment exactly once; GC does not
    /// perform that work. Reads/retains/releases of a retired owner must fail.
    pub fn release() -> Self {
        Self { parameters: vec![BridgeValue::OpaqueOwner], results: vec![BridgeValue::FinalPayload] }
    }
}

// Share only the variant enumeration. Ownership, checking, interpretation, and
// encoding remain separate exhaustive matches over the generated enum.
macro_rules! owner_operations {
    ($( $(#[$attribute:meta])* $variant:ident { $($field:ident: $field_type:ty),* $(,)? } ),* $(,)?) => {
        /// Kernel operations receive typed captures rather than owner handles.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum OwnerOperation {
            $( $(#[$attribute])* $variant { $($field: $field_type),* }, )*
        }
    };
}

owner_operations! {
    /// Clones managed captures; every input remains live.
    Construct { destination: ValueId, constructor: ConstructorId, captures: Vec<ValueId> },
    ReadCapability { destination: ValueId, owner: ValueId, constructor: ConstructorId, capture: CaptureOrdinal },
    /// Reads an immutable nested owner field and returns one retained token of
    /// the field's exact registered GC type.
    ReadOwner { destination: ValueId, owner: ValueId, constructor: ConstructorId, capture: CaptureOrdinal },
    /// Reads an Int through an opaque, read-only place-reference family. The
    /// authenticated constructor metadata selects the root/projection; no
    /// guest operand can select a layout or reinterpret the carrier.
    ReadSharedPlace { destination: ValueId, owner: ValueId, family: FunctionSignatureId },
    /// Reads one authenticated field of an immutable rooted slice carrier.
    /// `Length` is the carrier's checked projection length, never the backing
    /// list's current length; `Start` is its checked offset; `Root` is one
    /// retained token of the backing list, admitted as a shared immutable
    /// object exactly like a shared `List` parameter, so no write can reach it
    /// and a later owner mutation copies. The field is static IR metadata
    /// checked against the family's constructor schema, never a guest-selected
    /// environment field.
    ReadSharedSlice { destination: ValueId, owner: ValueId, family: FunctionSignatureId, field: SharedSliceField },
    /// Reads the authenticated constructor/environment tag without selecting
    /// or granting access to constructor-specific payload fields.
    ReadTag { destination: ValueId, owner: ValueId, family: FunctionSignatureId },
    ReadCallable { destination: ValueId, owner: ValueId, family: FunctionSignatureId },
    Clone { destination: ValueId, owner: ValueId, family: FunctionSignatureId },
    Drop { owner: ValueId, family: FunctionSignatureId },
    /// Clone the environment's strong RC root without exposing capability
    /// fields. The resulting reference is data, never an owner lookup key.
    CloneEnvironment { destination: ValueId, owner: ValueId, constructor: ConstructorId },
    /// Clones each input token into typed reference storage; inputs stay live.
    NewList { destination: ValueId, family: FunctionSignatureId, elements: Vec<ValueId> },
    /// Bounds checked, returns an explicitly retained owner; index selects an
    /// actual reference in an owned list, not authority from integer bytes.
    ListGet { destination: ValueId, list: ValueId, index: ValueId, family: FunctionSignatureId },
    ListLength { destination: ValueId, list: ValueId, family: FunctionSignatureId },
    CloneList { destination: ValueId, list: ValueId, family: FunctionSignatureId },
    /// Consumes list/value tokens; copy-on-write preserves retained aliases.
    ListPush { destination: ValueId, list: ValueId, value: ValueId, family: FunctionSignatureId },
    /// Bounds checked replacement. Consumes list/value tokens and returns one
    /// owned copy-on-write list token.
    ListSet { destination: ValueId, list: ValueId, index: ValueId, value: ValueId, family: FunctionSignatureId },
    /// Final list release explicitly drops each element's owner token.
    DropList { list: ValueId, family: FunctionSignatureId },
}

/// The checked-source authority an opaque owner family is required to match.
/// Aggregate types are normalized concrete checked source identities, not
/// names, candidate constructor IDs, or callable declarations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerFamilySource {
    Closure,
    Aggregate(witchy_syntax::ast::Type),
    /// The exact checked rooted-view source type carried by this opaque family.
    /// The initial slice admits only lifetime-free `&Int`; mutable references
    /// cannot acquire this authority.
    SharedReference(witchy_syntax::ast::Type),
}

/// A list-element reference must recheck its captured index against the
/// root's current logical length when it is read. Construction-time bounds do
/// not grant authority over a subsequently mutated collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedPlaceBounds {
    CurrentLogicalLength,
}

/// The field of a rooted slice carrier that `ReadSharedSlice` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SharedSliceField {
    Root,
    Start,
    Length,
}

/// The exact element names a rooted slice carrier admits: scalar kinds carried
/// inline, or String and exact List(scalar) children behind owned pointer slots.
pub const ROOTED_SLICE_ELEMENTS: [&str; 9] = [
    "Int", "Float", "Bool", "Duration", "String", "List(Int)",
    "List(Float)", "List(Bool)", "List(Duration)",
];

/// The element name of an exact `&[T]` rooted slice source type, if `T` is an
/// admitted slice element.
pub fn rooted_slice_element(source_type: &witchy_syntax::ast::Type) -> Option<&'static str> {
    let witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::View, inner) = source_type else {
        return None;
    };
    // The checker spells a finalized slice payload as its private `Slice(T)`
    // nominal; it is the same checked payload as the surface `[T]`.
    let element = match inner.as_ref() {
        witchy_syntax::ast::Type::Slice(element) => element.as_ref(),
        witchy_syntax::ast::Type::Named(name, arguments) if name == "Slice" && arguments.len() == 1 => &arguments[0],
        _ => return None,
    };
    if let witchy_syntax::ast::Type::Named(name, arguments) = element
        && name == "List"
        && let [witchy_syntax::ast::Type::Named(inner, inner_arguments)] = arguments.as_slice()
        && inner_arguments.is_empty()
    {
        return match inner.as_str() {
            "Int" => Some("List(Int)"),
            "Float" => Some("List(Float)"),
            "Bool" => Some("List(Bool)"),
            "Duration" => Some("List(Duration)"),
            _ => None,
        };
    }
    let witchy_syntax::ast::Type::Named(name, arguments) = element else { return None };
    if !arguments.is_empty() {
        return None;
    }
    // A nominal called `List(Float)` is not the structural `List(Float)` type.
    match name.as_str() {
        "Int" => Some("Int"),
        "Float" => Some("Float"),
        "Bool" => Some("Bool"),
        "Duration" => Some("Duration"),
        "String" => Some("String"),
        _ => None,
    }
}

/// Whether `element` (the element layout of a slice's backing `PackedList`)
/// is exactly the physical representation of the admitted source element.
pub(crate) fn slice_element_layout_matches(
    layouts: &crate::layout::LayoutInterner,
    element: crate::layout::LayoutId,
    name: &str,
) -> bool {
    use crate::layout::{LayoutKind, PointerSlot, RcHeader, ScalarKind};
    let kind = layouts.get(element).map(|descriptor| descriptor.kind());
    match name {
        "Int" => matches!(kind, Some(LayoutKind::Scalar(ScalarKind::Int))),
        "Float" => matches!(kind, Some(LayoutKind::Scalar(ScalarKind::Float))),
        "Bool" => matches!(kind, Some(LayoutKind::Scalar(ScalarKind::Bool))),
        "Duration" => matches!(kind, Some(LayoutKind::Scalar(ScalarKind::Duration))),
        "String" => {
            let Some(LayoutKind::RuntimePointerValueSlot { pointee, semantics: PointerSlot::Owned }) = kind else {
                return false;
            };
            let Some(LayoutKind::RuntimeBuffer { element: byte }) = layouts.get(*pointee).map(|descriptor| descriptor.kind()) else {
                return false;
            };
            matches!(layouts.get(*byte).map(|descriptor| descriptor.kind()), Some(LayoutKind::Scalar(ScalarKind::RawByte)))
        }
        "List(Int)" | "List(Float)" | "List(Bool)" | "List(Duration)" => {
            let scalar = match name {
                "List(Int)" => ScalarKind::Int,
                "List(Float)" => ScalarKind::Float,
                "List(Bool)" => ScalarKind::Bool,
                "List(Duration)" => ScalarKind::Duration,
                _ => unreachable!("matched exact nested scalar family"),
            };
            let Some(LayoutKind::RuntimePointerValueSlot { pointee, semantics: PointerSlot::Owned }) = kind else {
                return false;
            };
            let Ok(physical) = layouts.resolve_nominal(*pointee) else { return false };
            let Some(LayoutKind::PackedList { element: inner, rc: RcHeader::Required }) = layouts.get(physical).map(|descriptor| descriptor.kind()) else {
                return false;
            };
            matches!(layouts.get(*inner).map(|descriptor| descriptor.kind()),
                Some(LayoutKind::Scalar(actual)) if *actual == scalar)
        }
        _ => false,
    }
}

/// Static interpretation of a shared-reference constructor. This is derived
/// exclusively from the registered constructor schema; it is never encoded as
/// a guest tag, pointer, layout ID, or projection selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedReferenceConstructor {
    WholeIntCell { root: CaptureOrdinal },
    ListIntElement {
        root: CaptureOrdinal,
        index: CaptureOrdinal,
        bounds: SharedPlaceBounds,
    },
    /// An `Int` projection from an immutable rooted `[Int]` carrier. The
    /// carrier is held in an opaque owner field; the local checked coordinate
    /// cannot select or expose its backing root.
    ListIntElementFromSlice {
        slice: CaptureOrdinal,
        index: CaptureOrdinal,
        bounds: SharedPlaceBounds,
    },
    /// An immutable `[T]` projection over a retained `List(T)` root, for
    /// every admitted slice element.  `start` and `length` are captured
    /// checked `Size` values; only `ReadSharedSlice` exposes them or the
    /// retained root, and the root it exposes is shared and immutable.
    ListSlice {
        root: CaptureOrdinal,
        start: CaptureOrdinal,
        length: CaptureOrdinal,
    },
}

/// Candidate schema, not authority. Admission independently binds the source
/// witness, complete function signatures, capture order/types, and environment
/// layout. IDs are static IR metadata, never guest authority selectors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerFamily {
    pub source: OwnerFamilySource,
    pub owner_type: GcTypeId,
    pub list_type: GcTypeId,
    /// Body ABI: leading owner parameter followed by declaration parameters.
    pub signature: FunctionSignatureId,
    pub declaration_signature: FunctionSignatureId,
    pub environment: LayoutId,
    pub constructors: Vec<OwnerConstructor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerConstructor {
    pub id: ConstructorId,
    pub function: FunctionId,
    /// None is a named-function adapter, authenticated by adapter bindings.
    pub witness: Option<u32>,
    pub environment_tag: i64,
    pub captures: Vec<OwnerCapture>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerCapture {
    pub value_type: ValueType,
    pub storage: CaptureStorage,
    pub transfer: CaptureTransfer,
}

impl OwnerConstructor {
    /// Recognize the only two authenticated constructor schemas in the first
    /// shared-place slice. Layout identity is checked independently by source
    /// authority and the initial checker.
    pub fn shared_reference_constructor(&self) -> Option<SharedReferenceConstructor> {
        match self.captures.as_slice() {
            [OwnerCapture {
                value_type: ValueType::Ref(_),
                storage: CaptureStorage::EnvironmentField(root_field),
                transfer: CaptureTransfer::CloneManaged,
            }] if *root_field > 0 => Some(SharedReferenceConstructor::WholeIntCell {
                root: CaptureOrdinal(0),
            }),
            [OwnerCapture {
                value_type: ValueType::Ref(_),
                storage: CaptureStorage::EnvironmentField(root_field),
                transfer: CaptureTransfer::CloneManaged,
            }, OwnerCapture {
                value_type: ValueType::Size,
                storage: CaptureStorage::EnvironmentField(index_field),
                transfer: CaptureTransfer::CopyScalar,
            }] if *root_field > 0 && *index_field > 0 && root_field != index_field => Some(SharedReferenceConstructor::ListIntElement {
                root: CaptureOrdinal(0),
                index: CaptureOrdinal(1),
                bounds: SharedPlaceBounds::CurrentLogicalLength,
            }),
            [OwnerCapture {
                value_type: ValueType::Gc(_),
                storage: CaptureStorage::OwnerField(slice_field),
                transfer: CaptureTransfer::CloneManaged,
            }, OwnerCapture {
                value_type: ValueType::Size,
                storage: CaptureStorage::EnvironmentField(index_field),
                transfer: CaptureTransfer::CopyScalar,
            }] if *slice_field >= 2 && *index_field > 0 => Some(SharedReferenceConstructor::ListIntElementFromSlice {
                slice: CaptureOrdinal(0),
                index: CaptureOrdinal(1),
                bounds: SharedPlaceBounds::CurrentLogicalLength,
            }),
            [OwnerCapture {
                value_type: ValueType::Ref(_),
                storage: CaptureStorage::EnvironmentField(root_field),
                transfer: CaptureTransfer::CloneManaged,
            }, OwnerCapture {
                value_type: ValueType::Size,
                storage: CaptureStorage::EnvironmentField(start_field),
                transfer: CaptureTransfer::CopyScalar,
            }, OwnerCapture {
                value_type: ValueType::Size,
                storage: CaptureStorage::EnvironmentField(length_field),
                transfer: CaptureTransfer::CopyScalar,
            }] if *root_field > 0 && *start_field > 0 && *length_field > 0
                && root_field != start_field && root_field != length_field && start_field != length_field =>
            {
                Some(SharedReferenceConstructor::ListSlice {
                    root: CaptureOrdinal(0), start: CaptureOrdinal(1), length: CaptureOrdinal(2),
                })
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureStorage {
    EnvironmentField(u32),
    /// Actual typed GC field. Payload fields 0/1 are environment/code; owner
    /// fields start at 2 and never occupy linear-memory bytes.
    OwnerField(u32),
}

impl OwnerOperation {
    pub fn destinations(&self) -> Vec<ValueId> {
        match self {
            Self::Construct { destination, .. } | Self::ReadCapability { destination, .. }
            | Self::ReadOwner { destination, .. } | Self::ReadSharedPlace { destination, .. }
            | Self::ReadSharedSlice { destination, .. }
            | Self::ReadTag { destination, .. }
            | Self::ReadCallable { destination, .. } | Self::Clone { destination, .. }
            | Self::CloneEnvironment { destination, .. } | Self::NewList { destination, .. }
            | Self::ListGet { destination, .. } | Self::ListLength { destination, .. }
            | Self::CloneList { destination, .. } | Self::ListPush { destination, .. }
            | Self::ListSet { destination, .. } => vec![*destination],
            Self::Drop { .. } | Self::DropList { .. } => Vec::new(),
        }
    }

    pub fn operands(&self) -> Vec<ValueId> {
        match self {
            Self::Construct { captures, .. } => captures.clone(),
            Self::NewList { elements, .. } => elements.clone(),
            Self::ReadCapability { owner, .. } | Self::ReadOwner { owner, .. }
            | Self::ReadSharedPlace { owner, .. }
            | Self::ReadSharedSlice { owner, .. }
            | Self::ReadTag { owner, .. }
            | Self::ReadCallable { owner, .. }
            | Self::Clone { owner, .. } | Self::CloneEnvironment { owner, .. } | Self::Drop { owner, .. } => vec![*owner],
            Self::ListGet { list, index, .. } => vec![*list, *index],
            Self::ListLength { list, .. } | Self::CloneList { list, .. } | Self::DropList { list, .. } => vec![*list],
            Self::ListPush { list, value, .. } => vec![*list, *value],
            Self::ListSet { list, index, value, .. } => vec![*list, *index, *value],
        }
    }
}

// Closure-contract cases that need a real checked source (RFC-0170 P4).
#[cfg(feature = "checked-source-test-api")]
#[doc(hidden)]
pub mod checked_source_cases;
