use std::collections::BTreeSet;

use witchy_cap_model::authority::{
    AUTHORITY_OPERATIONS, AuthorityOperationEffect, AuthorityScope, AuthorityScopeKind,
    AuthorityScopeKindId, CapabilityKindId, CapabilityRightId, DerivationKind, DerivedRights,
    HostReceiverEncoding, InternalCarrierEncoding, RightsSet, RightsSetError, RootOriginPolicy,
    ScopeNarrowingKind, ScopeRestrictionId, TARGET_DOMAIN_CONTRACTS, TargetDomainId,
    authority_scope_kind_from_id, capability_contract, capability_kind_from_id,
    capability_right_from_id, rights_admit, rights_are_monotone_narrowing, rights_are_well_formed,
    root_origin_permits_entry, target_domain_contract,
};
use witchy_cap_model::{CapabilityKind, CapabilityRight};

const ALL_RIGHTS: &[CapabilityRight] = &[
    CapabilityRight::Read,
    CapabilityRight::Write,
    CapabilityRight::Connect,
    CapabilityRight::Listen,
    CapabilityRight::Tcp,
    CapabilityRight::Udp,
    CapabilityRight::Uds,
    CapabilityRight::Reveal,
    CapabilityRight::Seal,
];

#[test]
fn stable_capability_and_right_ids_are_total_unique_and_round_trip() {
    let kind_ids = CapabilityKind::ALL
        .iter()
        .map(|kind| kind.stable_id().get())
        .collect::<BTreeSet<_>>();
    assert_eq!(kind_ids.len(), 18);
    for kind in CapabilityKind::ALL {
        assert_eq!(capability_kind_from_id(kind.stable_id()), Some(*kind));
    }
    assert_eq!(
        capability_kind_from_id(CapabilityKindId::from_raw(0xffff)),
        None
    );

    let right_ids = ALL_RIGHTS
        .iter()
        .map(|right| right.stable_id().get())
        .collect::<BTreeSet<_>>();
    assert_eq!(right_ids.len(), 9);
    for right in ALL_RIGHTS {
        assert_eq!(capability_right_from_id(right.stable_id()), Some(*right));
    }
    assert_eq!(
        capability_right_from_id(CapabilityRightId::from_raw(0xffff)),
        None
    );
}

#[test]
fn target_domain_ids_are_explicit_unique_and_closed() {
    assert_eq!(TARGET_DOMAIN_CONTRACTS.len(), 5);
    assert_eq!(
        TARGET_DOMAIN_CONTRACTS
            .iter()
            .map(|contract| contract.id)
            .collect::<BTreeSet<_>>()
            .len(),
        5
    );
    for contract in TARGET_DOMAIN_CONTRACTS {
        assert_eq!(target_domain_contract(contract.id), Some(contract));
    }
    assert_eq!(
        target_domain_contract(TargetDomainId::from_raw(0xffff)),
        None
    );
    assert!(
        !target_domain_contract(TargetDomainId::INTERPRETER_ORACLE)
            .expect("interpreter domain")
            .shippable
    );
}

#[test]
fn every_capability_has_one_closed_contract() {
    let contracts = CapabilityKind::ALL
        .iter()
        .map(|kind| capability_contract(*kind))
        .collect::<Vec<_>>();
    assert_eq!(contracts.len(), 18);
    assert_eq!(
        contracts
            .iter()
            .map(|contract| contract.id)
            .collect::<BTreeSet<_>>()
            .len(),
        18
    );
    for contract in contracts {
        assert_eq!(contract.id, contract.kind.stable_id());
        assert!(rights_are_well_formed(contract.kind, contract.rights));
    }
}

#[test]
fn erased_host_receivers_retain_ghost_roots_and_reference_carriers() {
    let erased = [
        CapabilityKind::Console,
        CapabilityKind::Clock,
        CapabilityKind::Rand,
        CapabilityKind::SecretStore,
        CapabilityKind::BuildOut,
        CapabilityKind::BuildRead,
        CapabilityKind::BuildEnv,
        CapabilityKind::BuildNet,
        CapabilityKind::BuildExec,
    ];
    for kind in CapabilityKind::ALL {
        let contract = capability_contract(*kind);
        assert_eq!(
            contract.internal_carrier,
            InternalCarrierEncoding::ExternRef
        );
        if erased.contains(kind) {
            assert_eq!(contract.host_receiver, HostReceiverEncoding::ErasedGhost);
            assert_eq!(contract.root_origin, RootOriginPolicy::GhostEntryParameter);
        } else {
            assert_eq!(contract.host_receiver, HostReceiverEncoding::ExternRef);
        }
    }
}

#[test]
fn derived_only_kinds_cannot_be_roots_or_root_mint_targets() {
    for kind in CapabilityKind::ALL {
        assert_eq!(
            root_origin_permits_entry(*kind),
            capability_contract(*kind).root_origin != RootOriginPolicy::DerivedOnly
        );
    }

    for kind in [CapabilityKind::Socket, CapabilityKind::Listener] {
        assert!(!root_origin_permits_entry(kind));
    }

    // These are the current represented root capabilities. A later ABI adapter
    // must apply the same predicate to every concrete `mint_*` import mapping.
    for kind in [
        CapabilityKind::Dir,
        CapabilityKind::File,
        CapabilityKind::Net,
        CapabilityKind::Fetch,
        CapabilityKind::Env,
        CapabilityKind::Exec,
        CapabilityKind::Secret,
    ] {
        assert!(root_origin_permits_entry(kind));
    }
}

#[test]
fn the_complete_semantic_operation_catalog_is_unique_and_well_formed() {
    assert_eq!(AUTHORITY_OPERATIONS.len(), 59);
    assert_eq!(
        AUTHORITY_OPERATIONS
            .iter()
            .map(|operation| operation.id)
            .collect::<BTreeSet<_>>()
            .len(),
        59
    );
    assert_eq!(
        AUTHORITY_OPERATIONS
            .iter()
            .map(|operation| operation.name)
            .collect::<BTreeSet<_>>()
            .len(),
        59
    );

    let receivers = AUTHORITY_OPERATIONS
        .iter()
        .map(|operation| operation.receiver)
        .collect::<BTreeSet<_>>();
    assert_eq!(receivers.len(), 18);

    for operation in AUTHORITY_OPERATIONS {
        assert!(rights_are_well_formed(
            operation.receiver,
            operation.required_receiver_rights
        ));
        for requirement in operation.additional_authority_operands {
            assert_ne!(requirement.operand_index, 0, "receiver is already explicit");
            assert!(rights_are_well_formed(requirement.kind, requirement.rights));
        }
        if let AuthorityOperationEffect::Derive(derivation) = operation.effect {
            assert!(
                capability_contract(derivation.result_kind)
                    .permitted_derivations
                    .contains(&derivation.kind)
            );
            if let DerivedRights::Exact(rights) = derivation.result_rights {
                assert!(rights_are_well_formed(derivation.result_kind, rights));
            }
        }
    }
}

#[test]
fn load_bearing_rights_and_origins_are_precise() {
    let operation = |name| {
        AUTHORITY_OPERATIONS
            .iter()
            .find(|operation| operation.name == name)
            .unwrap_or_else(|| panic!("missing {name}"))
    };

    assert_eq!(
        operation("console.print").required_receiver_rights,
        &[CapabilityRight::Write]
    );
    assert_eq!(
        operation("file.read").required_receiver_rights,
        &[CapabilityRight::Read]
    );
    assert_eq!(
        operation("file.write").required_receiver_rights,
        &[CapabilityRight::Write]
    );

    let socket = capability_contract(CapabilityKind::Socket);
    assert_eq!(socket.root_origin, RootOriginPolicy::DerivedOnly);
    assert!(
        socket
            .permitted_derivations
            .contains(&DerivationKind::NetConnect)
    );
    assert!(
        socket
            .permitted_derivations
            .contains(&DerivationKind::ListenerAccept)
    );

    let accepted = operation("listener.accept");
    assert!(matches!(
        accepted.effect,
        AuthorityOperationEffect::Derive(derivation)
            if derivation.kind == DerivationKind::ListenerAccept
                && derivation.result_kind == CapabilityKind::Socket
    ));

    assert_eq!(
        operation("net.connect").required_receiver_rights,
        &[CapabilityRight::Connect, CapabilityRight::Tcp]
    );
    assert_eq!(
        operation("net.listen").required_receiver_rights,
        &[CapabilityRight::Listen, CapabilityRight::Tcp]
    );

    let lookup = operation("secretstore.require");
    assert!(matches!(
        lookup.effect,
        AuthorityOperationEffect::Derive(derivation)
            if derivation.result_rights == DerivedRights::SecretGrantBounded
    ));
}

#[test]
fn rights_and_scope_narrowing_are_monotone() {
    use CapabilityRight::{Connect, Listen, Reveal, Seal, Tcp, Write};

    assert!(rights_admit(
        CapabilityKind::Secret,
        &[Reveal, Seal],
        &[Seal]
    ));
    assert!(!rights_admit(
        CapabilityKind::Secret,
        &[Seal],
        &[Reveal, Seal]
    ));
    assert!(!rights_are_well_formed(CapabilityKind::Secret, &[Reveal]));
    assert!(rights_are_monotone_narrowing(
        CapabilityKind::Net,
        &[Connect, Listen, Tcp],
        &[Connect, Tcp]
    ));
    assert!(!rights_are_monotone_narrowing(
        CapabilityKind::Net,
        &[Connect, Tcp],
        &[Connect, Listen, Tcp]
    ));
    assert!(!rights_are_monotone_narrowing(
        CapabilityKind::File,
        &[],
        &[Write]
    ));

    for operation in AUTHORITY_OPERATIONS {
        if let AuthorityOperationEffect::Attenuate(kind) = operation.effect {
            assert!(matches!(
                kind,
                ScopeNarrowingKind::EnvOnly
                    | ScopeNarrowingKind::ExecOnly
                    | ScopeNarrowingKind::DirOnly
                    | ScopeNarrowingKind::NetOnly
                    | ScopeNarrowingKind::NetDeny
                    | ScopeNarrowingKind::FetchOnly
            ));
        }
    }
}

#[test]
fn dynamic_right_sets_normalize_source_and_reject_noncanonical_bytes() {
    use CapabilityRight::{Read, Reveal, Seal, Write};

    let normalized = RightsSet::from_rights(CapabilityKind::Secret, &[Reveal])
        .expect("source Reveal closes over Seal");
    assert!(normalized.contains(Reveal));
    assert!(normalized.contains(Seal));

    let unclosed_reveal = Reveal.canonical_bit();
    assert_eq!(
        RightsSet::from_canonical_bits(CapabilityKind::Secret, unclosed_reveal),
        Err(RightsSetError::RevealWithoutSeal)
    );
    assert!(matches!(
        RightsSet::from_canonical_bits(CapabilityKind::Console, 1 << 15),
        Err(RightsSetError::UnknownBits(_))
    ));
    assert!(matches!(
        RightsSet::from_rights(CapabilityKind::Console, &[Read, Write, Reveal]),
        Err(RightsSetError::RightNotAvailable {
            kind: CapabilityKind::Console,
            right: Reveal
        })
    ));

    let full = RightsSet::from_rights(CapabilityKind::Secret, &[Reveal, Seal]).unwrap();
    let sealed = RightsSet::from_rights(CapabilityKind::Secret, &[Seal]).unwrap();
    assert!(full.admits(CapabilityKind::Secret, sealed));
    assert!(RightsSet::is_monotone_narrowing(
        CapabilityKind::Secret,
        full,
        sealed
    ));
    assert!(!RightsSet::is_monotone_narrowing(
        CapabilityKind::Secret,
        sealed,
        full
    ));
}

#[test]
fn scope_identity_rejects_unknown_kinds_mismatches_and_widening() {
    let first = ScopeRestrictionId::from_bytes([1; 32]);
    let second = ScopeRestrictionId::from_bytes([2; 32]);
    let unrestricted_dir = AuthorityScope::unrestricted(AuthorityScopeKind::DirectoryTree);
    let restricted_dir = AuthorityScope::restricted(AuthorityScopeKind::DirectoryTree, first);
    let other_dir = AuthorityScope::restricted(AuthorityScopeKind::DirectoryTree, second);
    let restricted_net = AuthorityScope::restricted(AuthorityScopeKind::NetworkAddresses, first);

    assert!(AuthorityScope::is_monotone_narrowing(unrestricted_dir, restricted_dir).unwrap());
    assert!(!AuthorityScope::is_monotone_narrowing(restricted_dir, unrestricted_dir).unwrap());
    assert!(restricted_dir.admits(restricted_dir).unwrap());
    assert!(!restricted_dir.admits(other_dir).unwrap());
    assert!(restricted_dir.admits(restricted_net).is_err());

    for kind in CapabilityKind::ALL {
        let scope = capability_contract(*kind).scope;
        assert_eq!(authority_scope_kind_from_id(scope.stable_id()), Some(scope));
    }
    assert_eq!(
        authority_scope_kind_from_id(AuthorityScopeKindId::from_raw(0xffff)),
        None
    );
}
