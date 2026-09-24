use super::*;

#[test]
fn packed_list_header_places_refuse_non_header_or_non_packed_claims() {
    use witchy_wir::wir::WirExpr as W;

    let mut layouts = LayoutInterner::new();
    let scalar = layouts
        .intern_scalar(ScalarKind::Int)
        .expect("an Int scalar descriptor");
    let packed = layouts
        .intern_packed_list(scalar, RcHeader::Required)
        .expect("a packed Int list descriptor");
    let root = W::GetLocal("root".into());
    let capacity = W::GetLocal("cap".into());

    assert!(packed_list_header_place(
        &layouts,
        packed,
        root.clone(),
        capacity.clone(),
        0,
    )
    .is_some(), "the descriptor-authenticated length header is claimable");
    assert!(packed_list_header_place(
        &layouts,
        packed,
        root.clone(),
        capacity.clone(),
        1,
    )
    .is_some(), "the descriptor-authenticated capacity header is claimable");
    assert!(packed_list_header_place(&layouts, packed, root.clone(), capacity.clone(), 2).is_none(),
        "an invented third header component must not mint a SafeWIR place");
    assert!(packed_list_header_place(&layouts, scalar, root, capacity, 0).is_none(),
        "a scalar descriptor must not be reinterpreted as a packed-list header");
}

#[test]
fn packed_list_scalar_element_places_require_descriptor_authenticated_addresses() {
    use witchy_wir::wir::WirExpr as W;

    let mut layouts = LayoutInterner::new();
    let scalar = layouts
        .intern_scalar(ScalarKind::Int)
        .expect("an Int scalar descriptor");
    let packed = layouts
        .intern_packed_list(scalar, RcHeader::Required)
        .expect("a packed Int list descriptor");
    let root = W::GetLocal("root".into());
    let index = W::GetLocal("index".into());
    let capacity = W::GetLocal("cap".into());

    assert!(packed_list_element_place(
        &layouts,
        packed,
        root.clone(),
        index.clone(),
        capacity.clone(),
        AccessMode::Write,
    )
    .is_some(), "a scalar element has an exact descriptor-derived place");
    assert!(packed_list_payload_slice_place(
        &layouts,
        packed,
        root,
        W::ConstI32(0),
        index,
        capacity,
        AccessMode::Read,
    )
    .is_some(), "a descriptor-strided payload prefix has an exact SafeWIR slice");
}

    #[test]
    fn missing_checked_root_type_is_a_hard_codegen_error_not_an_omitted_root() {
        use witchy_types::loans::{LoanEvent, LoanOwnerRoot, LoanPlace, LoanProjection};

        let event = LoanEvent::from_checked_place(
            "view".into(),
            LoanPlace {
                root: LoanOwnerRoot {
                    local: "owner".into(),
                    direct_storage_type: None,
                },
                projection: LoanProjection::default(),
                storage_type: Type::Named("Int".into(), vec![]),
            },
            LoanProjection::default(),
            "view".into(),
        );
        let error = match Codegen::loan_root(&event) {
            Err(error) => error,
            Ok(_) => panic!("a missing checked root type must not silently omit retain/release"),
        };
        assert!(
            error.message.contains("has no exact checked root-local type"),
            "missing root-layout evidence must be a hard codegen error: {error}",
        );

        let scalar_event = LoanEvent::from_checked_place(
            "scalar_view".into(),
            LoanPlace {
                root: LoanOwnerRoot {
                    local: "scalar".into(),
                    direct_storage_type: Some(Type::Named("Int".into(), vec![])),
                },
                projection: LoanProjection::default(),
                storage_type: Type::Named("Int".into(), vec![]),
            },
            LoanProjection::default(),
            "scalar_view".into(),
        );
        assert!(
            matches!(Codegen::loan_root(&scalar_event), Ok(None)),
            "a checked scalar root is intentionally non-retainable, unlike missing evidence",
        );
    }

    #[test]
    fn scheduler_frame_allocation_whitelist_requires_the_sealed_step_layout() {
        assert_eq!(
            task_compiler_owned_frame_entry_ctor("PullReadyFrameLoop", "N9:task.Step[N3:Nil[]]"),
            Some(true)
        );
        assert_eq!(
            task_compiler_owned_frame_entry_ctor("Pull2FrameLoop", "N9:task.Step[N3:Nil[]]"),
            Some(false)
        );
        assert_eq!(
            task_compiler_owned_frame_entry_ctor("PullReadyFrameLoop", "N5:Other[]"),
            None,
            "a source spelling alone must never enter scheduler allocation attribution"
        );
        assert_eq!(
            task_compiler_owned_frame_entry_ctor("WaitFrameLoop", "N9:task.Step[N3:Nil[]]"),
            None,
            "in-place parked-frame retags are not allocations"
        );
    }
