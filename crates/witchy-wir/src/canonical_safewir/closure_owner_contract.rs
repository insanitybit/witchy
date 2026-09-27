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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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
    /// Transfer the sole managed token into a move-bearing aggregate owner.
    MoveManaged,
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
        let transfers = witness.captures().iter().map(|capture| {
            if ClosureCaptureTransfer::classify(capture.checked_type(), &definitions)
                != Some(capture.transfer())
            {
                return Err(ContractError::UnsupportedCapture);
            }
            match capture.transfer() {
            ClosureCaptureTransfer::CopyScalar => Ok(CaptureTransfer::CopyScalar),
            ClosureCaptureTransfer::ManagedReferenceByValue => Ok(CaptureTransfer::CloneManaged),
            // The view witness is distinct source authority, while the only
            // executable transfer is a managed retain of that opaque carrier.
            // A `&Float`/`&Bool`/`&Duration` view is carried as its immutable
            // scalar value (`ScalarValue`): it has no carrier to retain.
            ClosureCaptureTransfer::RootedViewByValue if matches!(capture.checked_type(),
                witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::View, inner)
                    if witchy_canonical_source::aggregates::is_inline_scalar_view_payload(inner)) =>
            {
                Ok(CaptureTransfer::CopyScalar)
            }
            ClosureCaptureTransfer::RootedViewByValue => Ok(CaptureTransfer::CloneManaged),
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
    /// Clones copyable managed captures and consumes source-proved moved
    /// captures according to the exact constructor transfer policy.
    Construct { destination: ValueId, constructor: ConstructorId, captures: Vec<ValueId> },
    /// Retire an exact, sole owner and transfer its linear environment and
    /// typed owner fields. `captures` lists OwnerField results in declaration
    /// order; the caller takes affine environment slots before dropping it.
    Deconstruct { owner: ValueId, constructor: ConstructorId, environment: ValueId, captures: Vec<ValueId> },
    ReadCapability { destination: ValueId, owner: ValueId, constructor: ConstructorId, capture: CaptureOrdinal },
    /// Reads an immutable nested owner field and returns one retained token of
    /// the field's exact registered GC type.
    ReadOwner { destination: ValueId, owner: ValueId, constructor: ConstructorId, capture: CaptureOrdinal },
    /// Copy one authenticated, copyable environment capture. The owner stays
    /// live; no environment token is exposed and managed children are retained.
    ReadEnvironmentField { destination: ValueId, owner: ValueId, constructor: ConstructorId, capture: CaptureOrdinal },
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

/// Structural identity of a checked rooted slice. This is independent of a
/// candidate's owner ID and of how a source type is printed.
pub fn rooted_slice_carrier(source_type: &witchy_syntax::ast::Type) -> Option<witchy_canonical_source::carrier::CarrierElement> {
    match witchy_canonical_source::storage::StoragePlan::classify(
        source_type, &std::collections::BTreeMap::new(), |_| None,
    ) {
        witchy_canonical_source::storage::StoragePlan::RootedView(
            witchy_canonical_source::rooted_view::RootedViewStorage::RetainedProjection(element)
        ) => Some(element),
        _ => None,
    }
}

/// Match every element's physical storage and ownership recursively. An owned
/// pointer slot is required at each managed value edge; every List body must
/// have an RC header. No nullable or borrowed slot can pass this relation.
pub(crate) fn carrier_element_layout_matches(
    layouts: &crate::layout::LayoutInterner,
    layout: crate::layout::LayoutId,
    carrier: &witchy_canonical_source::carrier::CarrierElement,
) -> bool {
    if !carrier.within_resource_bounds() {
        return false;
    }
    if matches!(layouts.get(layout).map(|descriptor| descriptor.kind()),
        Some(crate::layout::LayoutKind::OpaqueNominal { .. })) { return false; }
    use crate::layout::{LayoutId, LayoutInterner, LayoutKind, PointerSlot, RcHeader, ScalarKind};
    use witchy_canonical_source::abi::CanonicalScalarKind;
    use witchy_canonical_source::carrier::{CarrierLayoutQuery, CarrierSlot};

    struct Query<'a>(&'a LayoutInterner);
    impl Query<'_> {
        fn owned_pointee(&self, layout: LayoutId, slot: CarrierSlot) -> Option<LayoutId> {
            let LayoutKind::RuntimePointerValueSlot { pointee, semantics } =
                self.0.get(layout)?.kind() else { return None };
            let expected = match slot {
                CarrierSlot::Owned => PointerSlot::Owned,
                CarrierSlot::OwnedNullable => PointerSlot::OwnedNullable,
            };
            if *semantics != expected { return None; }
            if matches!(self.0.get(*pointee)?.kind(), LayoutKind::OpaqueNominal { .. }) {
                return None;
            }
            self.0.resolve_nominal(*pointee).ok()
        }
    }
    impl CarrierLayoutQuery for Query<'_> {
        type Layout = LayoutId;

        fn scalar(&self, layout: LayoutId) -> Option<CanonicalScalarKind> {
            match self.0.get(layout)?.kind() {
                LayoutKind::Scalar(ScalarKind::Unit) => Some(CanonicalScalarKind::Unit),
                LayoutKind::Scalar(ScalarKind::Int) => Some(CanonicalScalarKind::Int),
                LayoutKind::Scalar(ScalarKind::Float) => Some(CanonicalScalarKind::Float),
                LayoutKind::Scalar(ScalarKind::Bool) => Some(CanonicalScalarKind::Bool),
                LayoutKind::Scalar(ScalarKind::Duration) => Some(CanonicalScalarKind::Duration),
                _ => None,
            }
        }

        fn owned_byte_buffer(&self, layout: LayoutId, slot: CarrierSlot) -> bool {
            let Some(physical) = self.owned_pointee(layout, slot) else { return false };
            let Some(LayoutKind::RuntimeBuffer { element: byte }) =
                self.0.get(physical).map(|descriptor| descriptor.kind()) else { return false };
            matches!(self.0.get(*byte).map(|descriptor| descriptor.kind()),
                Some(LayoutKind::Scalar(ScalarKind::RawByte)))
        }

        fn owned_retained_list_element(&self, layout: LayoutId, slot: CarrierSlot) -> Option<LayoutId> {
            let physical = self.owned_pointee(layout, slot)?;
            let LayoutKind::PackedList { element, rc: RcHeader::Required } =
                self.0.get(physical)?.kind() else { return None };
            Some(*element)
        }

        fn owned_record_fields(&self, layout: LayoutId, slot: CarrierSlot) -> Option<Vec<LayoutId>> {
            let physical = self.owned_pointee(layout, slot)?;
            let LayoutKind::PackedRecord { fields } = self.0.get(physical)?.kind() else {
                return None;
            };
            Some(fields.clone())
        }

        fn owned_tagged_fields(&self, layout: LayoutId, slot: CarrierSlot) -> Option<Vec<LayoutId>> {
            let physical = self.owned_pointee(layout, slot)?;
            let LayoutKind::PackedRecord { fields } = self.0.get(physical)?.kind() else {
                return None;
            };
            let [tag, payload @ ..] = fields.as_slice() else { return None };
            (self.scalar(*tag) == Some(CanonicalScalarKind::Int)).then(|| payload.to_vec())
        }
    }
    carrier.matches_layout(&Query(layouts), layout)
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
    /// Candidate element proof for a rooted slice. The initial checker uses
    /// its physical relation; checked-source binding authenticates it against
    /// the exact checked view formation before admission.
    pub slice_carrier: Option<witchy_canonical_source::carrier::CarrierElement>,
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

impl OwnerCapture {
    pub(crate) fn copyable_environment_field(&self) -> Option<u32> {
        let CaptureStorage::EnvironmentField(field) = self.storage else { return None; };
        match (self.value_type, self.transfer) {
            (ValueType::Ref(_), CaptureTransfer::CloneManaged)
            | (ValueType::Int | ValueType::Float | ValueType::Bool | ValueType::Unit | ValueType::Size,
                CaptureTransfer::CopyScalar) => Some(field),
            _ => None,
        }
    }
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
            Self::Deconstruct { environment, captures, .. } => std::iter::once(*environment)
                .chain(captures.iter().copied()).collect(),
            Self::Construct { destination, .. } | Self::ReadCapability { destination, .. }
            | Self::ReadOwner { destination, .. } | Self::ReadSharedPlace { destination, .. }
            | Self::ReadEnvironmentField { destination, .. }
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
            Self::Deconstruct { owner, .. } => vec![*owner],
            Self::NewList { elements, .. } => elements.clone(),
            Self::ReadCapability { owner, .. } | Self::ReadOwner { owner, .. }
            | Self::ReadEnvironmentField { owner, .. }
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
