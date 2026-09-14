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
use witchy_types::canonical_closure::{ClosureCaptureTransfer, ClosureWitness};
use witchy_types::pipeline::CanonicalCheckedSource;

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
    source: &'a CanonicalCheckedSource,
    witness: &'a ClosureWitness,
    transfers: Vec<CaptureTransfer>,
}

impl<'a> SourceConstructor<'a> {
    pub fn from_checked(source: &'a CanonicalCheckedSource, ordinal: ConstructorId) -> Result<Self, ContractError> {
        use witchy_types::canonical_capabilities::{CapabilityKind, CapabilityRepresentation};
        let witness = source.closure_witness().entries().get(ordinal.0 as usize)
            .ok_or(ContractError::MissingWitness)?;
        let definitions = witchy_types::canonical_aggregates::type_definitions(source.typed_module().module());
        let transfers = witness.captures().iter().map(|capture| match capture.transfer() {
            ClosureCaptureTransfer::CopyScalar => Ok(CaptureTransfer::CopyScalar),
            ClosureCaptureTransfer::ManagedReferenceByValue => Ok(CaptureTransfer::CloneManaged),
            ClosureCaptureTransfer::ExplicitOwnedMove => Err(ContractError::UnsupportedCapture),
            ClosureCaptureTransfer::CopyCapability => {
                let Some(CapabilityRepresentation::Direct(kind)) =
                    witchy_types::canonical_capabilities::representation(capture.checked_type(), &definitions)
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
            && matches!(witchy_types::typeck::ty_to_ast(witness.signature()),
                Some(witchy_syntax::ast::Type::Fn(_, _, _, qualifiers)) if qualifiers.pure)
        {
            return Err(ContractError::PureCapabilityCapture);
        }
        Ok(Self { source, witness, transfers })
    }

    pub fn source(&self) -> &CanonicalCheckedSource { self.source }
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

        #[cfg(test)]
        impl OwnerOperation {
            pub(super) const VARIANTS: &'static [&'static str] = &[$(stringify!($variant)),*];

            fn variant_name(&self) -> &'static str {
                match self { $(Self::$variant { .. } => stringify!($variant),)* }
            }
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
            | Self::ReadOwner { destination, .. } | Self::ReadTag { destination, .. }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_operation_operand_and_destination_table_is_exhaustive() {
        let destination = ValueId::new(90);
        let owner = ValueId::new(1);
        let list = ValueId::new(2);
        let index = ValueId::new(3);
        let family = FunctionSignatureId::new(4);
        let constructor = ConstructorId(5);
        let capture = CaptureOrdinal(6);
        let cases = [
            (OwnerOperation::Construct { destination, constructor, captures: vec![owner, index] }, vec![owner, index], vec![destination]),
            (OwnerOperation::ReadCapability { destination, owner, constructor, capture }, vec![owner], vec![destination]),
            (OwnerOperation::ReadOwner { destination, owner, constructor, capture }, vec![owner], vec![destination]),
            (OwnerOperation::ReadTag { destination, owner, family }, vec![owner], vec![destination]),
            (OwnerOperation::ReadCallable { destination, owner, family }, vec![owner], vec![destination]),
            (OwnerOperation::Clone { destination, owner, family }, vec![owner], vec![destination]),
            (OwnerOperation::Drop { owner, family }, vec![owner], vec![]),
            (OwnerOperation::CloneEnvironment { destination, owner, constructor }, vec![owner], vec![destination]),
            (OwnerOperation::NewList { destination, family, elements: vec![owner, owner] }, vec![owner, owner], vec![destination]),
            (OwnerOperation::ListGet { destination, list, index, family }, vec![list, index], vec![destination]),
            (OwnerOperation::ListLength { destination, list, family }, vec![list], vec![destination]),
            (OwnerOperation::CloneList { destination, list, family }, vec![list], vec![destination]),
            (OwnerOperation::ListPush { destination, list, value: owner, family }, vec![list, owner], vec![destination]),
            (OwnerOperation::ListSet { destination, list, index, value: owner, family }, vec![list, index, owner], vec![destination]),
            (OwnerOperation::DropList { list, family }, vec![list], vec![]),
        ];
        assert_eq!(cases.len(), OwnerOperation::VARIANTS.len());
        for ((operation, operands, destinations), variant) in cases.iter().zip(OwnerOperation::VARIANTS) {
            assert_eq!(operation.variant_name(), *variant);
            assert_eq!(operation.operands(), *operands, "{variant} operands");
            assert_eq!(operation.destinations(), *destinations, "{variant} destinations");
        }
    }

    #[test]
    fn opaque_closure_contract_keeps_caps_and_managed_captures_typed() {
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".into(), b"fn logging(console: Console, prefix: String):\n    let log = fn(msg: String): console.print(prefix + msg)\n    log(\"ok\")\nfn main(console: Console):\n    logging(console, \"log: \")\n".to_vec())],
            "main", |_, _, _| Ok(Default::default()),
        ).unwrap();
        let source = checked.canonical_source().unwrap();
        let contract = SourceConstructor::from_checked(source, ConstructorId(0)).unwrap();
        assert!(std::ptr::eq(contract.source(), source));
        assert!(std::ptr::eq(contract.witness(), &source.closure_witness().entries()[0]));
        assert_eq!(contract.transfers().len(), 2);
        assert!(contract.transfers().contains(&CaptureTransfer::CloneManaged));
        for (index, transfer) in contract.transfers().iter().enumerate() {
            let read = contract.capability_read(CaptureOrdinal(index as u32));
            match transfer {
                CaptureTransfer::CopyCapability(kind) => {
                    assert_eq!(*kind, CapabilityKindId::new(0));
                    assert_eq!(read.unwrap(), BridgeAbi {
                        parameters: vec![BridgeValue::OpaqueOwner],
                        results: vec![BridgeValue::Capability(*kind)],
                    });
                }
                _ => assert_eq!(read, Err(ContractError::NotCapabilityCapture)),
            }
        }
        assert_eq!(contract.capability_read(CaptureOrdinal(u32::MAX)), Err(ContractError::NotCapabilityCapture));
        assert!(matches!(SourceConstructor::from_checked(source, ConstructorId(u32::MAX)), Err(ContractError::MissingWitness)));
        assert_eq!(BridgeAbi::seal().parameters, vec![BridgeValue::TypedGcPayload]);
        assert_eq!(BridgeAbi::release().results, vec![BridgeValue::FinalPayload]);
        assert_eq!(BridgeAbi::retain().parameters, vec![BridgeValue::OpaqueOwner]);
    }

    #[test]
    fn opaque_closure_contract_does_not_enable_pure_capability_capture() {
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".into(), b"fn main(console: Console):\n    let bad = pure fn(msg: String): console.print(msg)\n    bad(\"no\")\n".to_vec())],
            "main", |_, _, _| Ok(Default::default()),
        );
        assert!(checked.is_err(), "pure capture restrictions remain a source obligation");
    }
}
