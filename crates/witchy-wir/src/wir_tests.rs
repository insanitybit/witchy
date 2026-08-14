    use super::*;

    #[test]
    fn place_reference_reborrow_preserves_root_access_and_evaluated_projection() {
        let parent = PlaceReference::new("account", PlaceAccess::Exclusive).reborrow([
            PlaceProjection::Field("profile".into()),
            PlaceProjection::Index {
                coordinate: "__reference_index".into(),
            },
        ]);
        let child = parent.reborrow([PlaceProjection::Field("name".into())]);

        assert_eq!(child.root, "account");
        assert_eq!(child.access, PlaceAccess::Exclusive);
        assert_eq!(
            child.projections,
            vec![
                PlaceProjection::Field("profile".into()),
                PlaceProjection::Index {
                    coordinate: "__reference_index".into(),
                },
                PlaceProjection::Field("name".into()),
            ]
        );
    }

    #[test]
    fn existential_wrapper_keeps_payload_reference_typed() {
        let wrapper = existential_wrapper_struct();
        assert_eq!(
            wrapper.fields,
            [Kind::StructRef, Kind::I32],
            "the payload is an erased GC reference and the witness is a table index"
        );
        assert_eq!(EXISTENTIAL_PAYLOAD_FIELD, 0);
        assert_eq!(EXISTENTIAL_WITNESS_FIELD, 1);
        assert!(
            wrapper.fields[EXISTENTIAL_PAYLOAD_FIELD as usize].is_ref(),
            "the payload must never cross an i64 slot"
        );
    }

    #[test]
    fn single_allocator_invariant_holds_across_helper_registry() {
        // Every name the registry resolves: probe the known lists. (`wir_helper`
        // is a by-name match, so enumerate from the prelude + the registry-only
        // helpers named in this crate; a new helper is reachable only through
        // `wir_helper`, so probing its name covers it.)
        let mut names: Vec<String> = crate::wir_prelude::prelude()
            .funcs
            .iter()
            .map(|f| f.name.clone())
            .collect();
        for extra in [
            "__heap_reclaim", "bump_alloc", "char_count", "crypto_ecdsa_p256_verify_hex_status",
            "crypto_ecdsa_p256_verify_status", "crypto_ed25519_verify_status", "crypto_hmac_sha256",
            "crypto_rsa_pkcs1_sha256_verify_status", "crypto_sha3_256", "crypto_sha512",
            "dir_append", "dir_create", "dir_create_new", "dir_exists", "dir_is_dir",
            "dir_make_dir", "dir_only", "dir_open", "dir_rename", "dir_replace",
            "dir_read_bytes", "dir_subdir", "dir_write", "dir_write_bytes", "exec", "file_write",
            "list_at_view", "list_len_view", "list_set_cap", "list_update_cap",
            "net_accept", "net_close", "net_connect", "net_connect_pinned", "net_deny",
            "net_listen", "net_restrict", "net_send_bytes", "net_send_line",
            "net_try_connect", "net_try_connect_pinned", "now", "now_monotonic",
            "rand_u64", "rc_alloc", "rc_drop", "rc_dup", "rc_free", "rcopy_str",
            "regex_match_spans", "serve_pool", "string_from_code", "vm_par_map",
            "vm_par_map_bytes", "vm_serve", "vm_with_dir", "__galloc",
        ] {
            names.push(extra.to_string());
        }
        let mut funcs: Vec<WirFunc> = names
            .iter()
            .filter_map(|n| crate::wir_helpers::wir_helper(n).map(|s| s.func))
            .collect();
        // `$__galloc` has no registry entry (it is pushed by the assembler); include
        // it directly, plus a representative slice of the `$mk{n}` family.
        funcs.push(crate::wir_helpers::galloc_helper());
        for n in [0usize, 1, 2, 3, 4, 8, 16] {
            funcs.push(crate::wir_helpers::mk_helper(n, false));
            funcs.push(crate::wir_helpers::mk_helper(n, true));
        }
        // The sanitizer variants rebuild several helpers with different bodies;
        // cover the checked `ensure` too.
        funcs.push(crate::wir_helpers::ensure_helper(true));
        assert!(funcs.len() > 100, "expected to resolve the whole helper library, got {}", funcs.len());
        let module = WirModule {
            imports: vec![],
            funcs,
            memory_pages: 1,
            data: vec![],
            globals: vec![],
            table: None,
            exports: vec![],
        };
        let violations = heap_write_violations(&module);
        assert!(
            violations.is_empty(),
            "RFC-0051 I2 violated — these helpers write `$heap` outside `$bump_alloc` \
             (route them through the single ensure-prefixed allocator): {violations:?}"
        );
    }

    #[test]
    fn closure_helpers_use_the_uniform_gc_wrapper_abi() {
        assert!(
            !closure_wrapper_struct().mutable,
            "the closure code/environment identity is immutable after construction"
        );
        for helper in [
            crate::wir_helpers::dict_update_helper(),
            crate::wir_helpers::dict_update_cap_helper(),
            crate::wir_helpers::list_update_cap_helper(),
        ] {
            let closure = helper
                .params
                .iter()
                .find(|param| param.name == "clos")
                .unwrap_or_else(|| panic!("{} must declare a closure parameter", helper.name));
            assert_eq!(
                closure.ty,
                WirTy::GcRef(0),
                "{} must not accept a forgeable linear closure pointer",
                helper.name,
            );
        }

        let trampoline = crate::wir_helpers::call_idx_helper();
        let [WirNode::Push(WirExpr::CallIndirect { signature, args, .. })] =
            trampoline.body.as_slice()
        else {
            panic!("call trampoline must be one indirect call")
        };
        assert_eq!(signature.params.first(), Some(&Kind::GcRef(0)));
        assert!(matches!(args.first(), Some(WirExpr::RefNull(Kind::GcRef(0)))));
    }
