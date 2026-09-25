//! The checked-source product SafeWIR consumes (RFC-0170).
//!
//! `witchy_types::pipeline::CanonicalCheckedSource` owns a checked program and
//! everything derived from it. SafeWIR, the Wasm emitter and their authority
//! checks read it only through [`CanonicalSourceView`], a borrowed projection
//! defined here so that `witchy-wir` need not depend on the checker. Both
//! [`CanonicalSourceView`] and [`CheckedSourceWitness`] are minted only by the
//! checked frontend; `witchy-wir`'s architecture tests pin their single
//! construction sites.

use std::collections::BTreeMap;

use witchy_syntax::ast::{Expr, Function, Item, Module, Type, TypeDef};

use crate::abi::{CanonicalCallableAbi, CanonicalCallableAbiManifest};
use crate::access::AccessSignature;
use crate::carrier::CarrierElement;
use crate::closure::ClosureWitnessManifest;
use crate::rooted_record::RecordRootPlan;
use crate::specialization::LogicalSpecializationIdentity;

/// Evidence that exact source bytes crossed the checked type-checking boundary.
///
/// Its fields are private so a caller cannot choose a digest and present it as
/// checked-source authority. The one constructor,
/// [`CheckedSourceWitness::mint_for_checked_frontend`], is called only by the
/// source-bearing checked-link entry point in `witchy_types::pipeline`; the
/// `checked_source_authority_has_one_frontend_minting_site` architecture test
/// pins that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedSourceWitness {
    source_bundle_digest: [u8; 32],
    proper_tail_manifest_digest: [u8; 32],
    callable_abi_manifest_digest: [u8; 32],
    closure_witness_digest: [u8; 32],
    launch_contract_digest: [u8; 32],
    obligations_digest: [u8; 32],
    precompiled_declarations_digest: [u8; 32],
}

/// The digests one checked-source witness binds together.
pub struct CheckedSourceDigests {
    pub source_bundle_digest: [u8; 32],
    pub proper_tail_manifest_digest: [u8; 32],
    pub callable_abi_manifest_digest: [u8; 32],
    pub closure_witness_digest: [u8; 32],
    pub launch_contract_digest: [u8; 32],
    pub obligations_digest: [u8; 32],
    pub precompiled_declarations_digest: [u8; 32],
}

impl CheckedSourceWitness {
    /// Checked-frontend only; see the type's documentation.
    #[doc(hidden)]
    pub fn mint_for_checked_frontend(digests: CheckedSourceDigests) -> Self {
        let CheckedSourceDigests {
            source_bundle_digest,
            proper_tail_manifest_digest,
            callable_abi_manifest_digest,
            closure_witness_digest,
            launch_contract_digest,
            obligations_digest,
            precompiled_declarations_digest,
        } = digests;
        Self {
            source_bundle_digest,
            proper_tail_manifest_digest,
            callable_abi_manifest_digest,
            closure_witness_digest,
            launch_contract_digest,
            obligations_digest,
            precompiled_declarations_digest,
        }
    }

    /// Rebind the launch contract after the checked build-recipe frontend
    /// replaced the payload with its version-2 build contract. Called only by
    /// `witchy_types::pipeline::build_recipe` (pinned with the minting site).
    #[doc(hidden)]
    pub fn rebind_launch_contract_for_build_recipe(&mut self, launch_contract_digest: [u8; 32]) {
        self.launch_contract_digest = launch_contract_digest;
    }

    /// Production uses the exact raw bundle identity. A compiler-owned test
    /// driver uses its domain-separated recipe-derived identity; that driver's
    /// TestDriverIdentity separately retains the original raw bundle identity.
    pub fn source_bundle_digest(&self) -> [u8; 32] {
        self.source_bundle_digest
    }

    /// Digest of the sorted `CanonicalObligation` set this checked source
    /// recorded. Bound into SafeWIR authority so the set cannot be edited
    /// between checked-source construction and canonical admission.
    pub fn obligations_digest(&self) -> [u8; 32] {
        self.obligations_digest
    }

    pub fn proper_tail_manifest_digest(&self) -> [u8; 32] {
        self.proper_tail_manifest_digest
    }

    pub fn callable_abi_manifest_digest(&self) -> [u8; 32] {
        self.callable_abi_manifest_digest
    }

    pub fn closure_witness_digest(&self) -> [u8; 32] {
        self.closure_witness_digest
    }

    pub fn launch_contract_digest(&self) -> [u8; 32] {
        self.launch_contract_digest
    }

    /// Digest of the authenticated external declaration environment joined to
    /// this exact source link. The empty environment has its own canonical
    /// digest; it is omitted only from the historical bundle-input hash for
    /// backward-compatible empty-source identities.
    pub fn precompiled_declarations_digest(&self) -> [u8; 32] {
        self.precompiled_declarations_digest
    }

    pub fn verifies_closure_witness(&self, manifest: &ClosureWitnessManifest) -> bool {
        self.closure_witness_digest == manifest.digest()
    }
}

/// The finalized access-signature queries SafeWIR authority makes against the
/// checked facts of the exact module in the same view.
pub trait CheckedAccessQuery {
    /// The finalized access identity of a checked direct declaration.
    fn declaration(&self, name: &str) -> Option<&AccessSignature>;
    /// The callable selected at one exact checked call expression of `module`.
    fn call_at(&self, module: &Module, expression: &Expr) -> Option<&AccessSignature>;
}

/// One export of an authenticated precompiled library joined to the source
/// link, as the Wasm import it binds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrecompiledExport {
    pub library_alias: String,
    pub export: String,
    pub library_binding_digest: [u8; 32],
    pub wasm_sha256: [u8; 32],
    pub sidecar_sha256: [u8; 32],
}

/// Everything a view borrows, as the checked frontend assembles it.
pub struct CanonicalSourceParts<'s> {
    pub module: &'s Module,
    pub type_definitions: &'s BTreeMap<String, TypeDef>,
    pub source_witness: &'s CheckedSourceWitness,
    pub closure_witness: &'s ClosureWitnessManifest,
    pub signatures: &'s BTreeMap<String, Type>,
    pub generic_instances: &'s BTreeMap<String, LogicalSpecializationIdentity>,
    pub abi_manifest: &'s CanonicalCallableAbiManifest,
    pub closed_abis: &'s BTreeMap<String, CanonicalCallableAbi>,
    pub runtime_functions: &'s [String],
    pub launch_contract_payload: &'s [u8],
    pub build_entry: Option<&'s str>,
    /// `None` when the checked access facts are unavailable.
    pub access: Option<&'s dyn CheckedAccessQuery>,
    pub loans_checked: bool,
    pub precompiled_exports: &'s [PrecompiledExport],
    /// Recursive carriers derived from exact checked rooted-view formations.
    pub slice_carriers: &'s [CarrierElement],
}

/// A borrowed, read-only view of one checked canonical source.
#[derive(Clone, Copy)]
pub struct CanonicalSourceView<'s> {
    module: &'s Module,
    type_definitions: &'s BTreeMap<String, TypeDef>,
    source_witness: &'s CheckedSourceWitness,
    closure_witness: &'s ClosureWitnessManifest,
    signatures: &'s BTreeMap<String, Type>,
    generic_instances: &'s BTreeMap<String, LogicalSpecializationIdentity>,
    abi_manifest: &'s CanonicalCallableAbiManifest,
    closed_abis: &'s BTreeMap<String, CanonicalCallableAbi>,
    runtime_functions: &'s [String],
    launch_contract_payload: &'s [u8],
    build_entry: Option<&'s str>,
    access: Option<&'s dyn CheckedAccessQuery>,
    loans_checked: bool,
    precompiled_exports: &'s [PrecompiledExport],
    slice_carriers: &'s [CarrierElement],
}

impl<'s> CanonicalSourceView<'s> {
    /// Checked-frontend only: `CanonicalCheckedSource::view` is the single
    /// caller (pinned by an architecture test).
    #[doc(hidden)]
    pub fn from_checked_frontend(parts: CanonicalSourceParts<'s>) -> Self {
        let CanonicalSourceParts {
            module,
            type_definitions,
            source_witness,
            closure_witness,
            signatures,
            generic_instances,
            abi_manifest,
            closed_abis,
            runtime_functions,
            launch_contract_payload,
            build_entry,
            access,
            loans_checked,
            precompiled_exports,
            slice_carriers,
        } = parts;
        Self {
            module,
            type_definitions,
            source_witness,
            closure_witness,
            signatures,
            generic_instances,
            abi_manifest,
            closed_abis,
            runtime_functions,
            launch_contract_payload,
            build_entry,
            access,
            loans_checked,
            precompiled_exports,
            slice_carriers,
        }
    }

    /// The exact checked AST every other fact in this view is keyed on.
    pub fn module(&self) -> &'s Module { self.module }

    /// The declared types of the checked module, keyed by bare and
    /// entry-qualified name (see `aggregates::type_definitions`).
    pub fn type_definitions(&self) -> &'s BTreeMap<String, TypeDef> { self.type_definitions }

    /// A candidate slice family must match one of these exact checked
    /// formation payloads before its layout can be considered.
    pub fn admits_slice_carrier(&self, element: &CarrierElement) -> bool {
        self.slice_carriers.contains(element)
    }

    pub fn slice_carriers(&self) -> &'s [CarrierElement] { self.slice_carriers }

    /// Classify a record-root payload against this exact checked module's
    /// definitions and nominal ordinals. SafeWIR must use the payload from a
    /// checked source fact or signature; a candidate's own type is not proof.
    pub fn record_root_plan(&self, payload: &Type) -> Option<RecordRootPlan> {
        RecordRootPlan::from_checked_type(payload, self.type_definitions, |name| {
            self.module.items.iter().enumerate().find_map(|(ordinal, item)| {
                match item {
                    Item::Type(definition) if definition.name == name => u32::try_from(ordinal).ok(),
                    _ => None,
                }
            })
        })
    }

    pub fn source_witness(&self) -> &'s CheckedSourceWitness { self.source_witness }

    pub fn closure_witness(&self) -> &'s ClosureWitnessManifest { self.closure_witness }

    pub fn entry_declaration(&self) -> &'s str { self.build_entry.unwrap_or("main") }
    pub fn is_build_recipe(&self) -> bool { self.build_entry.is_some() }

    pub fn launch_contract_payload(&self) -> &'s [u8] { self.launch_contract_payload }

    /// The checked callable signature of a concrete named declaration.
    pub fn callable_signature(&self, name: &str) -> Option<&'s Type> { self.signatures.get(name) }

    pub fn generic_specialization(&self, name: &str) -> Option<&'s LogicalSpecializationIdentity> {
        self.generic_instances.get(name)
    }

    pub fn callable_abi_manifest(&self) -> &'s CanonicalCallableAbiManifest { self.abi_manifest }

    pub fn closed_callable_abi(&self, name: &str) -> Option<&'s CanonicalCallableAbi> {
        self.closed_abis.get(name)
    }

    pub fn closed_callable_signature(&self, ordinal: u32) -> Option<&'s Type> {
        let name = self.runtime_functions.get(ordinal as usize)?;
        self.signatures.get(name)
    }

    /// The exact retained source declaration for a closed callable. Unlike the
    /// checked callable signature, this preserves source loan spelling needed
    /// by independent ABI correspondence.
    pub fn closed_callable_declaration(&self, ordinal: u32) -> Option<&'s Function> {
        let name = self.runtime_functions.get(ordinal as usize)?;
        self.module.items.iter().find_map(|item| match item {
            Item::Function(function) if &function.name == name => Some(function),
            _ => None,
        })
    }

    /// The finalized access facts of [`Self::module`], if the checker produced
    /// them.
    pub fn access_facts(&self) -> Option<&'s dyn CheckedAccessQuery> { self.access }

    /// Whether the checked loan graph for [`Self::module`] is available.
    pub fn loans_checked(&self) -> bool { self.loans_checked }

    pub fn precompiled_exports(&self) -> &'s [PrecompiledExport] { self.precompiled_exports }
}
