    use super::*;

    const IMPORTS_PRINT: &str = r#"
        (module
          (import "witchy" "print" (func $print (param i32 i32)))
          (memory (export "memory") 1)
          (func (export "run")))
    "#;

    const BARE: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "run")))
    "#;

    const SPINNER: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "run") (loop $l (br $l))))
    "#;

    const TRAP_ON_FIRST_RUN: &str = r#"
        (module
          (memory (export "memory") 1)
          (global $entered (mut i32) (i32.const 0))
          (func (export "run")
            (if (i32.eqz (global.get $entered))
              (then
                (global.set $entered (i32.const 1))
                unreachable))))
    "#;

    const GREEDY: &str = r#"
        (module (memory (export "memory") 4) (func (export "run")))
    "#;

    #[test]
    fn trapped_vm_is_terminal() {
        let mut runtime = Runtime::new().unwrap();
        let mut vm = runtime
            .spawn(TRAP_ON_FIRST_RUN, Capabilities::default(), 1)
            .unwrap();

        vm.run().expect_err("the first call traps");
        let err = vm
            .run()
            .expect_err("a trapped VM must not resume with abandoned roots");
        assert!(err.to_string().contains("aborted Witchy VM cannot be run again"), "{err}");
    }

    #[test]
    fn optimized_wasm_cache_envelope_rejects_corruption_and_swaps() {
        let input_hash = sha256(b"original input wasm");
        let payload = b"optimized wasm payload";
        let envelope = encode_optimized_wasm(input_hash, payload);
        assert_eq!(decode_optimized_wasm(&input_hash, &envelope), Some(payload.as_slice()));

        let other_input = sha256(b"different input wasm");
        assert!(decode_optimized_wasm(&other_input, &envelope).is_none(), "a cache file cannot move between input keys");

        let mut corrupt = envelope.clone();
        *corrupt.last_mut().unwrap() ^= 0x80;
        assert!(decode_optimized_wasm(&input_hash, &corrupt).is_none(), "payload corruption must fail before validation");
        assert!(decode_optimized_wasm(&input_hash, &envelope[..envelope.len() - 1]).is_none(), "truncation must be rejected");

        let mut bad_magic = envelope;
        bad_magic[0] ^= 0xff;
        assert!(decode_optimized_wasm(&input_hash, &bad_magic).is_none(), "unknown cache formats must be ignored");
    }

    const NULL_FILE_READ: &str = r#"
        (module
          (import "witchy" "file_read_len" (func $file_read_len (param externref) (result i32)))
          (func (export "run")
            (drop (call $file_read_len (ref.null extern)))))
    "#;

    const NULL_DIR_READ: &str = r#"
        (module
          (import "witchy" "dir_read_len" (func $dir_read_len (param externref i32) (result i32)))
          (memory (export "memory") 1)
          (func (export "run")
            (drop (call $dir_read_len (ref.null extern) (i32.const 0)))))
    "#;

    const NULL_NET_CONNECT: &str = r#"
        (module
          (import "witchy" "net_connect" (func $net_connect (param externref i32) (result externref)))
          (memory (export "memory") 1)
          (func (export "run")
            (drop (call $net_connect (ref.null extern) (i32.const 0)))))
    "#;

    const NULL_SOCKET_CLOSE: &str = r#"
        (module
          (import "witchy" "net_close" (func $net_close (param externref)))
          (func (export "run")
            (call $net_close (ref.null extern))))
    "#;

    const NULL_LISTENER_ACCEPT: &str = r#"
        (module
          (import "witchy" "net_accept" (func $net_accept (param externref) (result externref)))
          (func (export "run")
            (drop (call $net_accept (ref.null extern)))))
    "#;

    const NULL_SECRET_REVEAL: &str = r#"
        (module
          (import "witchy" "crypto_reveal_len" (func $crypto_reveal_len (param externref) (result i32)))
          (func (export "run")
            (drop (call $crypto_reveal_len (ref.null extern)))))
    "#;

    const NULL_SECRET_SIGN: &str = r#"
        (module
          (import "witchy" "crypto.sign" (func $crypto_sign (param externref i32 i32)))
          (memory (export "memory") 1)
          (func (export "run")
            (call $crypto_sign (ref.null extern) (i32.const 0) (i32.const 0))))
    "#;

    const DIRECT_FILE_WRITE: &str = r#"
        (module
          (import "witchy" "mint_file" (func $mint_file (param i32) (result externref)))
          (import "witchy" "file_write" (func $file_write (param externref i32)))
          (memory (export "memory") 1)
          (func (export "run")
            (call $file_write (call $mint_file (i32.const 0)) (i32.const 0))))
    "#;

    /// The core thesis: a capability that was not granted simply does not exist
    /// for the VM, so it cannot even be instantiated.
    #[test]
    fn ungranted_capability_is_unreachable() {
        let mut rt = Runtime::new().unwrap();
        let err = rt
            .spawn(IMPORTS_PRINT, Capabilities::none(), 4)
            .map(|_| ())
            .unwrap_err();
        assert!(
            err.to_string().contains("unknown import"),
            "expected an unknown-import error, got: {err}"
        );
    }

    #[test]
    fn granted_capability_instantiates() {
        let mut rt = Runtime::new().unwrap();
        let mut vm = rt
            .spawn(IMPORTS_PRINT, Capabilities { print: true, ..Default::default() }, 4)
            .unwrap();
        vm.run().unwrap();
    }

    /// Each VM's linear memory is its own; the runtime hands out separate
    /// `Store`s, so one VM's memory is never visible to another.
    #[test]
    fn vms_have_independent_memories() {
        let mut rt = Runtime::new().unwrap();
        let mut a = rt.spawn(BARE, Capabilities::default(), 4).unwrap();
        let mut b = rt.spawn(BARE, Capabilities::default(), 4).unwrap();
        let mem_a = a.instance.get_memory(&mut a.store, "memory").unwrap();
        let mem_b = b.instance.get_memory(&mut b.store, "memory").unwrap();
        // Distinct backing allocations.
        assert_ne!(
            mem_a.data_ptr(&a.store),
            mem_b.data_ptr(&b.store),
            "VMs must not share a linear memory"
        );
    }

    #[test]
    fn memory_budget_is_enforced() {
        let mut rt = Runtime::new().unwrap();
        let err = rt.spawn(GREEDY, Capabilities::none(), 1).map(|_| ()).unwrap_err();
        assert!(
            err.to_string().contains("memory"),
            "expected a memory-limit error, got: {err}"
        );
    }

    fn assert_null_externref_rejected(module: &str, capabilities: Capabilities, kind: &str) {
        let mut rt = Runtime::new().unwrap();
        let mut vm = rt.spawn(module, capabilities, 4).unwrap();
        let err = vm.run().unwrap_err();
        let detail = format!("{err:?}");
        let expected = format!("{kind} externref is null");
        assert!(
            detail.contains(&expected),
            "expected null {kind} externref rejection, got: {detail}"
        );
    }

    #[test]
    fn null_externrefs_are_rejected_by_every_capability_family() {
        let root = std::env::temp_dir().join(format!(
            "witchy-null-externrefs-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("file.txt");
        std::fs::write(&file, "host-owned").unwrap();

        let cases = [
            (
                NULL_FILE_READ,
                Capabilities {
                    file_grants: vec![file],
                    ..Default::default()
                },
                "File",
            ),
            (
                NULL_DIR_READ,
                Capabilities {
                    dir_root: Some(root.clone()),
                    dir_read: true,
                    ..Default::default()
                },
                "Dir",
            ),
            (
                NULL_NET_CONNECT,
                Capabilities {
                    net_allow: Some(vec!["127.0.0.1:1".to_string()]),
                    net_connect: true,
                    ..Default::default()
                },
                "Net",
            ),
            (
                NULL_SOCKET_CLOSE,
                Capabilities {
                    net_allow: Some(vec!["127.0.0.1:1".to_string()]),
                    net_connect: true,
                    ..Default::default()
                },
                "Socket",
            ),
            (
                NULL_LISTENER_ACCEPT,
                Capabilities {
                    net_allow: Some(vec!["127.0.0.1:0".to_string()]),
                    net_listen: true,
                    ..Default::default()
                },
                "Listener",
            ),
            (NULL_SECRET_REVEAL, Capabilities::default(), "Secret"),
            (
                NULL_SECRET_SIGN,
                Capabilities {
                    signing_key: Some([0x41; 32]),
                    secrets: vec![SecretGrant::new("signing", vec![0x41; 32])],
                    ..Default::default()
                },
                "Secret",
            ),
        ];
        for (module, capabilities, kind) in cases {
            assert_null_externref_rejected(module, capabilities, kind);
        }

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn read_only_direct_file_does_not_link_write() {
        let path = std::env::temp_dir().join(format!(
            "witchy-readonly-file-grant-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, "host-owned").unwrap();

        let mut rt = Runtime::new().unwrap();
        let err = rt
            .spawn(
                DIRECT_FILE_WRITE,
                Capabilities {
                    file_grants: vec![path.clone()],
                    file_rights: vec![FsRights::new(true, false)],
                    ..Default::default()
                },
                4,
            )
            .map(|_| ())
            .unwrap_err();
        let _ = std::fs::remove_file(path);
        assert!(
            err.to_string().contains("unknown import"),
            "read-only File grants must not link file_write, got: {err}"
        );
    }

    /// A runaway VM that never yields is forcibly preempted by the scheduler.
    #[test]
    fn runaway_vm_is_preempted() {
        let mut rt = Runtime::new().unwrap();
        let mut spinner = rt.spawn(SPINNER, Capabilities::none(), 4).unwrap();
        let err = rt
            .run_with_budget(&mut spinner, Duration::from_millis(20))
            .unwrap_err();
        assert!(
            matches!(err.downcast_ref::<wasmtime::Trap>(), Some(wasmtime::Trap::Interrupt)),
            "expected an interrupt trap, got: {err}"
        );
    }

    #[test]
    fn read_wstr_list_huge_count_fails_closed_not_aborts() {
        // SEC-033: a guest claiming i32::MAX elements in a tiny buffer must NOT pre-allocate
        // ~51GB and abort the host — the capacity hint is capped at the slots that fit in
        // memory, and the read fails closed (out-of-bounds Err) on the first slot past the end.
        let mut data = vec![0u8; 8];
        data[0..4].copy_from_slice(&i32::MAX.to_le_bytes());
        assert!(
            read_wstr_list(&data, 0).is_err(),
            "a bogus huge list count must fail closed, not allocate/abort"
        );
    }

    // RFC-0023 checked heap: a `heap_register(start,end)` poisons the trailing
    // redzone; the post-run sweep proves it survived. A clean object passes; a write
    // past the object end into the redzone — in-bounds for the linear memory, so
    // invisible to wasmtime — is caught.
    const HEAP_OK: &str = r#"
        (module
          (import "witchy" "heap_register" (func $reg (param i32 i32)))
          (memory (export "memory") 1)
          (func (export "run")
            (call $reg (i32.const 16) (i32.const 32))))
    "#;

    const HEAP_OVERRUN: &str = r#"
        (module
          (import "witchy" "heap_register" (func $reg (param i32 i32)))
          (memory (export "memory") 1)
          (func (export "run")
            (call $reg (i32.const 16) (i32.const 32))
            (i32.store (i32.const 32) (i32.const 0))))
    "#;

    // SEC-035: read_line_capped consumes exactly one line per call (leaving the rest for
    // the next recv — line protocols depend on this) and returns the remainder at EOF.
    #[test]
    fn read_line_capped_splits_one_line_per_call() {
        use std::io::BufReader;
        let data = b"hello\nworld\ntail-no-newline";
        let mut r = BufReader::new(&data[..]);
        assert_eq!(crate::net::read_line_capped(&mut r).unwrap(), b"hello\n");
        assert_eq!(crate::net::read_line_capped(&mut r).unwrap(), b"world\n");
        assert_eq!(crate::net::read_line_capped(&mut r).unwrap(), b"tail-no-newline");
        assert_eq!(crate::net::read_line_capped(&mut r).unwrap(), b""); // EOF
    }

    #[test]
    fn heap_check_clean_redzone_passes() {
        let mut rt = Runtime::new().unwrap();
        let mut vm = rt.spawn(HEAP_OK, Capabilities::none(), 4).unwrap();
        vm.run().expect("an untouched redzone must pass the sweep");
    }

    #[test]
    fn heap_check_overrun_redzone_traps() {
        let mut rt = Runtime::new().unwrap();
        let mut vm = rt.spawn(HEAP_OVERRUN, Capabilities::none(), 4).unwrap();
        let err = vm.run().map(|_| ()).unwrap_err();
        assert!(
            err.to_string().contains("HEAP CHECK"),
            "an overrun into the redzone must trap the sweep, got: {err}"
        );
    }

    // A region/watermark reset moves $heap back and reuses the space. `heap_frontier(wm)`
    // is the reclaim signal (emitted by the checked `$ensure` and the region copy-out): it
    // drops every redzone reaching `wm`, so the reused space's legitimate overwrite (here,
    // the i64 store over A's old redzone) is NOT later mistaken for an overrun.
    const HEAP_REGION_REUSE: &str = r#"
        (module
          (import "witchy" "heap_register" (func $reg (param i32 i32)))
          (import "witchy" "heap_frontier" (func $reclaim (param i32)))
          (memory (export "memory") 1)
          (func (export "run")
            (call $reg (i32.const 16) (i32.const 20))   ;; A [16,20), redzone [20,28)
            (call $reclaim (i32.const 16))              ;; reset to wm=16 reclaims A's redzone
            (i64.store (i32.const 20) (i64.const 123))  ;; reused space overwritten (legit)
            (call $reg (i32.const 16) (i32.const 28))))  ;; B re-registered at the base
    "#;

    #[test]
    fn heap_check_region_reuse_does_not_false_positive() {
        let mut rt = Runtime::new().unwrap();
        let mut vm = rt.spawn(HEAP_REGION_REUSE, Capabilities::none(), 4).unwrap();
        vm.run().expect("a reclaim of the watermark must drop the stale redzone");
    }

    // The reclaim must NOT drop a redzone that lies entirely below the watermark — a
    // pre-region object stays guarded across the region.
    const HEAP_RECLAIM_KEEPS_BELOW: &str = r#"
        (module
          (import "witchy" "heap_register" (func $reg (param i32 i32)))
          (import "witchy" "heap_frontier" (func $reclaim (param i32)))
          (memory (export "memory") 1)
          (func (export "run")
            (call $reg (i32.const 16) (i32.const 20))   ;; A [16,20), redzone [20,28)
            (call $reclaim (i32.const 28))              ;; wm=28 is above A's whole redzone
            (i32.store (i32.const 20) (i32.const 0))))   ;; corrupt A's still-guarded redzone
    "#;

    #[test]
    fn heap_check_reclaim_keeps_redzone_below_watermark() {
        let mut rt = Runtime::new().unwrap();
        let mut vm = rt.spawn(HEAP_RECLAIM_KEEPS_BELOW, Capabilities::none(), 4).unwrap();
        let err = vm.run().map(|_| ()).unwrap_err();
        assert!(
            err.to_string().contains("HEAP CHECK"),
            "a redzone below the reclaim watermark must stay guarded, got: {err}"
        );
    }

    const HEAP_UNREGISTER_IDEMPOTENT: &str = r#"
        (module
          (import "witchy" "heap_register" (func $reg (param i32 i32)))
          (import "witchy" "heap_unregister" (func $unreg (param i32)))
          (memory (export "memory") 1)
          (func (export "run")
            (call $reg (i32.const 16) (i32.const 20))
            (call $reg (i32.const 32) (i32.const 36))
            (call $unreg (i32.const 16))
            (call $unreg (i32.const 16))
            ;; Retired A's redzone may be overwritten; B's remains intact.
            (i64.store (i32.const 20) (i64.const 0))))
    "#;

    const HEAP_UNREGISTER_KEEPS_ADJACENT: &str = r#"
        (module
          (import "witchy" "heap_register" (func $reg (param i32 i32)))
          (import "witchy" "heap_unregister" (func $unreg (param i32)))
          (memory (export "memory") 1)
          (func (export "run")
            (call $reg (i32.const 16) (i32.const 20))
            (call $reg (i32.const 32) (i32.const 36))
            (call $unreg (i32.const 16))
            ;; Exact retirement must leave B's higher redzone guarded.
            (i32.store (i32.const 36) (i32.const 0))))
    "#;

    #[test]
    fn heap_unregister_is_idempotent_for_one_exact_object() {
        let mut rt = Runtime::new().unwrap();
        let mut vm = rt.spawn(HEAP_UNREGISTER_IDEMPOTENT, Capabilities::none(), 4).unwrap();
        vm.run().expect("retiring the same exact object twice must be harmless");
    }

    #[test]
    fn heap_unregister_keeps_higher_adjacent_redzones_guarded() {
        let mut rt = Runtime::new().unwrap();
        let mut vm = rt.spawn(HEAP_UNREGISTER_KEEPS_ADJACENT, Capabilities::none(), 4).unwrap();
        let err = vm.run().expect_err("retiring A must not retire adjacent B");
        assert!(err.to_string().contains("HEAP CHECK"), "adjacent B lost protection: {err}");
    }

    fn checked_uaf_module(double_free: bool) -> Vec<u8> {
        use witchy_wir::wir::{
            BinOp, GlobalInit, Kind, WirExpr as E, WirFunc, WirGlobal, WirImport, WirLocal,
            WirModule, WirNode as N, WirTy,
        };

        let gl = |name: &str| E::GetLocal(name.into());
        let i32_bin = |op, lhs, rhs| E::Binary {
            op,
            kind: Kind::I32,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        };
        let assert_i32 = |lhs, rhs| N::If {
            cond: i32_bin(BinOp::Eq, lhs, rhs),
            then_: vec![],
            els: vec![N::Unreachable],
            result: None,
        };
        let assert_i64 = |lhs, rhs| N::If {
            cond: E::Binary {
                op: BinOp::Eq,
                kind: Kind::I64,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
            then_: vec![],
            els: vec![N::Unreachable],
            result: None,
        };
        let alloc = || E::Call { func: "rc_alloc".into(), args: vec![E::ConstI32(24)] };
        let register = |name: &str| N::Do(E::CallHost {
            import: "heap_register".into(),
            args: vec![gl(name), i32_bin(BinOp::Add, gl(name), E::ConstI32(16))],
        });
        let free = |name: &str| N::Do(E::Call {
            func: "rc_free".into(),
            args: vec![gl(name)],
        });
        let store = |name: &str, offset, value| N::Store {
            ptr: gl(name),
            value: E::ConstI32(value),
            kind: Kind::I32,
            offset,
        };
        let load = |name: &str, offset| E::Load {
            ptr: Box::new(gl(name)),
            kind: Kind::I32,
            offset,
        };

        let mut body = vec![
            N::SetLocal { local: "first".into(), value: alloc() },
            store("first", 0, 101),
            store("first", 4, 102),
            store("first", 8, 103),
            store("first", 12, 104),
            register("first"),
            N::SetLocal { local: "second".into(), value: alloc() },
            store("second", 0, 201),
            store("second", 4, 202),
            store("second", 8, 203),
            store("second", 12, 204),
            register("second"),
            free("first"),
        ];
        if double_free {
            body.push(free("first"));
        } else {
            body.push(N::SetLocal { local: "fresh".into(), value: alloc() });
            body.push(assert_i32(
                i32_bin(BinOp::Eq, gl("first"), gl("fresh")),
                E::ConstI32(0),
            ));
            for offset in [0, 4, 8, 12, 16, 20] {
                body.push(assert_i32(load("first", offset), E::ConstI32(0xDEAD_BEEFu32 as i32)));
            }
            body.extend([
                assert_i32(
                    E::Load {
                        ptr: Box::new(i32_bin(BinOp::Sub, gl("second"), E::ConstI32(8))),
                        kind: Kind::I32,
                        offset: 0,
                    },
                    E::ConstI32(1),
                ),
                assert_i32(
                    E::Load {
                        ptr: Box::new(i32_bin(BinOp::Sub, gl("second"), E::ConstI32(4))),
                        kind: Kind::I32,
                        offset: 0,
                    },
                    E::ConstI32(24),
                ),
                assert_i32(load("second", 0), E::ConstI32(201)),
                assert_i32(load("second", 4), E::ConstI32(202)),
                assert_i32(load("second", 8), E::ConstI32(203)),
                assert_i32(load("second", 12), E::ConstI32(204)),
                assert_i64(
                    E::Load { ptr: Box::new(gl("second")), kind: Kind::I64, offset: 16 },
                    E::ConstI64(i64::from_le_bytes([0xDB; 8])),
                ),
                assert_i32(E::GetGlobal("rc_freelist".into()), E::ConstI32(0)),
                assert_i64(E::GetGlobal("__witchy_rc_reuse_calls".into()), E::ConstI64(0)),
            ]);
        }

        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: ["first", "second", "fresh"]
                .into_iter()
                .map(|name| WirLocal { name: name.into(), ty: WirTy::Bool })
                .collect(),
            body,
            raw_body: None,
        };
        let mut funcs = witchy_wir::wir_helpers::rc_allocator_helpers_for_test(true, true);
        funcs.push(run);
        let module = WirModule {
            imports: vec![
                WirImport { name: "heap_register".into(), params: vec![Kind::I32, Kind::I32], results: vec![] },
                WirImport { name: "heap_unregister".into(), params: vec![Kind::I32], results: vec![] },
                WirImport { name: "heap_frontier".into(), params: vec![Kind::I32], results: vec![] },
            ],
            funcs,
            memory_pages: 1,
            data: vec![],
            globals: vec![
                WirGlobal { name: "heap".into(), kind: Kind::I32, mutable: true, init: GlobalInit::I32(2048), export: None },
                WirGlobal { name: "heap_base".into(), kind: Kind::I32, mutable: false, init: GlobalInit::I32(2048), export: None },
                WirGlobal { name: "rc_freelist".into(), kind: Kind::I32, mutable: true, init: GlobalInit::I32(0), export: None },
                WirGlobal { name: "__rc_reused_bytes".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_live_cells".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_rc_alloc_calls".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_bump_alloc_calls".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_rc_reuse_calls".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
                WirGlobal { name: "__witchy_rc_free_calls".into(), kind: Kind::I64, mutable: true, init: GlobalInit::I64(0), export: None },
            ],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        witchy_wir::wir_encode::try_encode_with_gc(&module, &[], &[])
            .expect("combined checked-UAF fixture must encode")
    }

    #[test]
    fn checked_uaf_unregisters_only_the_freed_cell_before_full_poison() {
        let mut rt = Runtime::new().unwrap();
        let mut vm = rt.spawn(checked_uaf_module(false), Capabilities::none(), 4).unwrap();
        vm.run().expect("full-cell poison must retire only the freed object's redzone");
    }

    #[test]
    fn checked_uaf_double_free_still_traps() {
        let mut rt = Runtime::new().unwrap();
        let mut vm = rt.spawn(checked_uaf_module(true), Capabilities::none(), 4).unwrap();
        vm.run().expect_err("the zero RC quarantine marker must trap a repeated free");
    }

#[test]
fn concrete_grants_have_one_complete_confinement_policy() {
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use witchy_confinement::{FsAccess, FsRule, FsScope, SyscallClass};

    let caps = Capabilities {
        dir_root: Some(PathBuf::from("/read")),
        dir_roots: vec![PathBuf::from("/write")],
        dir_rights: vec![
            FsRights::new(true, false),
            FsRights::new(false, true),
        ],
        file_grants: vec![PathBuf::from("/config/app.toml")],
        file_rights: vec![FsRights::new(true, false)],
        exec: true,
        net_allow: Some(vec!["db.example:5432".into()]),
        net_connect: true,
        net_grants: vec![vec!["127.0.0.1:8080".into()]],
        net_listen: true,
        fetch_grants: vec![vec!["https://api.example".into()]],
        build_out: Some(PathBuf::from("/generated")),
        build_read_roots: vec![PathBuf::from("/schema")],
        build_exec_tools: vec![("tool".into(), PathBuf::from("/bin/tool"))],
        build_exec_runtime_roots: vec![PathBuf::from("/lib")],
        build_net_allow: Some(vec!["cache.example:8443".into()]),
        ..Default::default()
    };

    let policy = caps.confinement_policy();
    assert_eq!(
        policy.filesystem,
        vec![
            FsRule {
                path: PathBuf::from("/bin/tool"),
                scope: FsScope::File,
                access: FsAccess::new(true, false, true),
            },
            FsRule {
                path: PathBuf::from("/config/app.toml"),
                scope: FsScope::File,
                access: FsAccess::new(true, false, false),
            },
            FsRule {
                path: PathBuf::from("/generated"),
                scope: FsScope::Tree,
                access: FsAccess::new(false, true, false),
            },
            FsRule {
                path: PathBuf::from("/lib"),
                scope: FsScope::Tree,
                access: FsAccess::new(true, false, true),
            },
            FsRule {
                path: PathBuf::from("/read"),
                scope: FsScope::Tree,
                access: FsAccess::new(true, false, true),
            },
            FsRule {
                path: PathBuf::from("/schema"),
                scope: FsScope::Tree,
                access: FsAccess::new(true, false, false),
            },
            FsRule {
                path: PathBuf::from("/write"),
                scope: FsScope::Tree,
                access: FsAccess::new(false, true, false),
            },
        ]
    );
    assert_eq!(
        policy.network.connect_tcp_ports,
        BTreeSet::from([443, 5432, 8080, 8443])
    );
    assert_eq!(
        policy.network.bind_tcp_ports,
        BTreeSet::from([5432, 8080])
    );
    assert_eq!(
        policy.syscall_classes,
        BTreeSet::from([
            SyscallClass::Base,
            SyscallClass::FsOpen,
            SyscallClass::Network,
            SyscallClass::Listen,
            SyscallClass::Process,
        ])
    );
}

#[test]
fn empty_build_grants_do_not_widen_outer_authority() {
    use witchy_confinement::SyscallClass;

    let policy = Capabilities {
        exec_allow: Some(Vec::new()),
        build_net_allow: Some(Vec::new()),
        ..Default::default()
    }
    .confinement_policy();

    assert!(!policy.network.connect_requested);
    assert!(!policy.syscall_classes.contains(&SyscallClass::Network));
    assert!(!policy.syscall_classes.contains(&SyscallClass::Process));
}

#[test]
fn exec_child_paths_widen_only_the_read_only_outer_fence() {
    use witchy_confinement::{FsAccess, FsRule, FsScope};

    let policy = Capabilities {
        exec_child_paths: vec![PathBuf::from("/child-config")],
        ..Default::default()
    }
    .confinement_policy();

    assert_eq!(
        policy.filesystem,
        vec![FsRule {
            path: PathBuf::from("/child-config"),
            scope: FsScope::Path,
            access: FsAccess::new(true, false, false),
        }]
    );
}

#[test]
fn empty_network_grant_remains_distinct_from_no_network_grant() {
    use witchy_confinement::SyscallClass;

    let absent = Capabilities::none().confinement_policy();
    let empty = Capabilities {
        net_allow: Some(Vec::new()),
        net_connect: true,
        ..Default::default()
    }
    .confinement_policy();

    assert!(!absent.network.connect_requested);
    assert!(!absent.syscall_classes.contains(&SyscallClass::Network));
    assert!(empty.network.connect_requested);
    assert!(empty.network.connect_tcp_ports.is_empty());
    assert!(empty.syscall_classes.contains(&SyscallClass::Network));
}

#[cfg(feature = "test-fixtures")]
#[test]
fn compiled_basic_fixtures_use_one_shared_host_and_transcript() {
    use witchy_testkit::{
        ClockFixture, ConsoleFixture, EnvFixture, Expectations, FixtureFamily, FixtureOutcome,
        FixturePlan, FixtureStep, FixtureValue, RandFixture, U64Text,
    };

    const MODULE: &str = r#"
        (module
          (import "witchy" "print" (func $print (param i32 i32)))
          (import "witchy" "console_read_len" (func $read (result i32)))
          (import "witchy" "now" (func $now (result i64)))
          (import "witchy" "now_monotonic" (func $mono (result i64)))
          (import "witchy" "rand_u64" (func $rand (result i64)))
          (import "witchy" "mint_env" (func $mint_env (result externref)))
          (import "witchy" "env_len" (func $env_len (param externref i32) (result i32)))
          (import "witchy" "args_size" (func $args_size (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 0) "hello\0a")
          (data (i32.const 16) "\04\00\00\00MODE")
          (func (export "run") (local $env externref)
            i32.const 0
            i32.const 6
            call $print
            call $read
            drop
            call $now
            drop
            call $mono
            drop
            call $rand
            drop
            call $mint_env
            local.set $env
            local.get $env
            i32.const 16
            call $env_len
            drop
            call $args_size
            drop))
    "#;
    let plan = FixturePlan {
        version: 1,
        console: Some(ConsoleFixture {
            script: vec![
                console_output_step("hello"),
                FixtureStep {
                    operation: "console_read_len".to_owned(),
                    target: None,
                    arguments: std::collections::BTreeMap::new(),
                    effective_rights: Some(vec!["Read".to_owned()]),
                    outcome: FixtureOutcome::Return {
                        value: FixtureValue::String("fixture input".to_owned()),
                    },
                    required: true,
                },
            ],
        }),
        clock: Some(ClockFixture {
            start_ns: Some(U64Text::new(2_000_000)),
            step_ns: Some(U64Text::new(1_000_000)),
            repeat_last: false,
            script: Vec::new(),
        }),
        rand: Some(RandFixture {
            seed: Some(U64Text::new(7)),
            script: Vec::new(),
        }),
        env: Some(EnvFixture {
            values: std::collections::BTreeMap::from([(
                "MODE".to_owned(),
                "fixture".to_owned(),
            )]),
            allow: vec!["MODE".to_owned()],
            script: Vec::new(),
        }),
        argv: Some(vec!["one".to_owned()]),
        expectations: Expectations::default(),
        ..FixturePlan::default()
    };
    let mut runtime = Runtime::batch().expect("runtime");
    let outcome = runtime
        .run_fixtures(MODULE, plan, 2)
        .expect("compiled fixture run");
    assert!(matches!(outcome.result, FixtureWasmResult::Passed));
    assert_eq!(outcome.output, vec!["hello"]);
    assert_eq!(outcome.transcript.stdout, vec!["hello"]);
    assert_eq!(
        outcome
            .transcript
            .events
            .iter()
            .map(|event| (event.family, event.operation.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (FixtureFamily::Env, "mint_env"),
            (FixtureFamily::Console, "print"),
            (FixtureFamily::Console, "console_read_len"),
            (FixtureFamily::Clock, "now"),
            (FixtureFamily::Clock, "now"),
            (FixtureFamily::Rand, "rand_u64"),
            (FixtureFamily::Env, "env_len"),
            (FixtureFamily::Argv, "args"),
        ]
    );
}

#[cfg(feature = "test-fixtures")]
#[test]
fn compiled_filesystem_fixture_uses_opaque_handles_and_shared_state() {
    use witchy_testkit::{
        FilesystemEntry, FilesystemFixture, FixtureFamily, FixturePlan,
    };

    const MODULE: &str = r#"
        (module
          (import "witchy" "mint_dir" (func $mint_dir (param i32) (result externref)))
          (import "witchy" "dir_read_len"
            (func $dir_read_len (param externref i32) (result i32)))
          (import "witchy" "dir_write"
            (func $dir_write (param externref i32 i32)))
          (import "witchy" "dir_exists"
            (func $dir_exists (param externref i32) (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 0) "\08\00\00\00seed.txt")
          (data (i32.const 32) "\07\00\00\00new.txt")
          (data (i32.const 64) "\03\00\00\00new")
          (func (export "run") (local $dir externref)
            i32.const 0
            call $mint_dir
            local.set $dir
            local.get $dir
            i32.const 0
            call $dir_read_len
            drop
            local.get $dir
            i32.const 32
            i32.const 64
            call $dir_write
            local.get $dir
            i32.const 32
            call $dir_exists
            drop
            local.get $dir
            i32.const 32
            call $dir_read_len
            drop))
    "#;
    let plan = FixturePlan {
        version: 1,
        filesystem: Some(FilesystemFixture {
            entries: std::collections::BTreeMap::from([(
                "seed.txt".to_owned(),
                FilesystemEntry::File {
                    hex: "6f6c64".to_owned(),
                },
            )]),
            rights: vec!["Read".to_owned(), "Write".to_owned()],
            entry_policy: None,
            script: Vec::new(),
        }),
        ..FixturePlan::default()
    };
    let mut runtime = Runtime::batch().expect("runtime");
    let outcome = runtime
        .run_fixtures(MODULE, plan, 2)
        .expect("compiled fixture run");
    assert!(matches!(outcome.result, FixtureWasmResult::Passed));
    assert_eq!(
        outcome
            .transcript
            .events
            .iter()
            .map(|event| (event.family, event.operation.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (FixtureFamily::Filesystem, "mint_dir"),
            (FixtureFamily::Filesystem, "dir_read_len"),
            (FixtureFamily::Filesystem, "dir_write"),
            (FixtureFamily::Filesystem, "dir_exists"),
            (FixtureFamily::Filesystem, "dir_read_len"),
        ]
    );
}

#[cfg(feature = "test-fixtures")]
#[test]
fn compiled_fetch_fixture_stages_success_and_provider_failures_without_network() {
    use witchy_testkit::{
        ConsoleFixture, FetchFixture, FixtureErrorCode, FixtureFailure, FixtureFamily,
        FixtureOutcome, FixturePlan, FixtureStep, FixtureValue,
    };

    const URL: &str = "https://example.com/data";
    const RESPONSE: &str = "HTTP/1.1 200\r\nX-Test: fixture\r\n\r\nok";
    const TIMEOUT: &str = "WITCHY_FETCH_ERROR:timeout:configured timeout";
    const MODULE: &str = r#"
        (module
          (import "witchy" "mint_fetch" (func $mint_fetch (param i32) (result externref)))
          (import "witchy" "fetch_send_len"
            (func $fetch_send_len (param externref i32 i32 i32 i32) (result i32)))
          (import "witchy" "fill_pending" (func $fill_pending (param i32)))
          (import "witchy" "print" (func $print (param i32 i32)))
          (memory (export "memory") 1)
          (data (i32.const 0) "\03\00\00\00GET")
          (data (i32.const 32) "\18\00\00\00https://example.com/data")
          (data (i32.const 96) "\00\00\00\00")
          (data (i32.const 104) "\00\00\00\00")
          (func (export "run") (local $fetch externref) (local $length i32)
            i32.const 0
            call $mint_fetch
            local.set $fetch
            local.get $fetch
            i32.const 0
            i32.const 32
            i32.const 96
            i32.const 104
            call $fetch_send_len
            local.set $length
            i32.const 256
            call $fill_pending
            i32.const 256
            local.get $length
            call $print
            local.get $fetch
            i32.const 0
            i32.const 32
            i32.const 96
            i32.const 104
            call $fetch_send_len
            local.set $length
            i32.const 256
            call $fill_pending
            i32.const 256
            local.get $length
            call $print))
    "#;
    let request_arguments = std::collections::BTreeMap::from([
        ("method".to_owned(), FixtureValue::String("GET".to_owned())),
        ("headers".to_owned(), FixtureValue::List(Vec::new())),
        ("body".to_owned(), FixtureValue::Bytes(String::new())),
    ]);
    let plan = FixturePlan {
        version: 1,
        console: Some(ConsoleFixture {
            script: vec![
                console_output_step(RESPONSE),
                console_output_step(TIMEOUT),
            ],
        }),
        fetch: Some(FetchFixture {
            origins: vec!["https://example.com:443".to_owned()],
            script: vec![
                FixtureStep {
                    operation: "fetch_send_len".to_owned(),
                    target: Some(URL.to_owned()),
                    arguments: request_arguments.clone(),
                    effective_rights: Some(vec!["https://example.com:443".to_owned()]),
                    outcome: FixtureOutcome::Return {
                        value: FixtureValue::Map(std::collections::BTreeMap::from([
                            ("status".to_owned(), FixtureValue::String("200".to_owned())),
                            (
                                "headers".to_owned(),
                                FixtureValue::List(vec![FixtureValue::Map(
                                    std::collections::BTreeMap::from([
                                        (
                                            "name".to_owned(),
                                            FixtureValue::String("X-Test".to_owned()),
                                        ),
                                        (
                                            "value".to_owned(),
                                            FixtureValue::String("fixture".to_owned()),
                                        ),
                                    ]),
                                )]),
                            ),
                            ("body".to_owned(), FixtureValue::Bytes("6f6b".to_owned())),
                        ])),
                    },
                    required: true,
                },
                FixtureStep {
                    operation: "fetch_send_len".to_owned(),
                    target: Some(URL.to_owned()),
                    arguments: request_arguments,
                    effective_rights: Some(vec!["https://example.com:443".to_owned()]),
                    outcome: FixtureOutcome::Fail {
                        error: FixtureFailure {
                            code: FixtureErrorCode::Timeout,
                            message: "configured timeout".to_owned(),
                        },
                    },
                    required: true,
                },
            ],
        }),
        ..FixturePlan::default()
    };
    let mut runtime = Runtime::batch().expect("runtime");
    let outcome = runtime
        .run_fixtures(MODULE, plan, 2)
        .expect("compiled fixture run");
    assert!(matches!(outcome.result, FixtureWasmResult::Passed));
    assert_eq!(outcome.output, vec![RESPONSE, TIMEOUT]);
    assert_eq!(
        outcome
            .transcript
            .events
            .iter()
            .filter(|event| event.family == FixtureFamily::Fetch)
            .map(|event| event.operation.as_str())
            .collect::<Vec<_>>(),
        vec!["mint_fetch", "fetch_send_len", "fetch_send_len"]
    );
}

#[cfg(feature = "test-fixtures")]
#[test]
fn compiled_secret_fixture_keeps_material_opaque_and_scripts_crypto() {
    use witchy_testkit::{
        ConsoleFixture, FixtureFamily, FixtureOutcome, FixturePlan, FixtureStep, FixtureValue,
        SecretFixture, SecretStoreFixture, SecretUsage,
    };

    const MODULE: &str = r#"
        (module
          (import "witchy" "secretstore_lookup"
            (func $lookup (param i32) (result externref)))
          (import "witchy" "crypto_reveal_len"
            (func $reveal (param externref) (result i32)))
          (import "witchy" "crypto.sign"
            (func $sign (param externref i32 i32)))
          (import "witchy" "crypto.public_key"
            (func $public_key (param externref i32)))
          (import "witchy" "fill_pending" (func $fill_pending (param i32)))
          (import "witchy" "print" (func $print (param i32 i32)))
          (memory (export "memory") 1)
          (data (i32.const 0) "\05\00\00\00token")
          (data (i32.const 32) "\07\00\00\00signing")
          (data (i32.const 64) "\07\00\00\00payload")
          (func (export "run") (local $secret externref) (local $length i32)
            i32.const 0
            call $lookup
            local.set $secret
            local.get $secret
            call $reveal
            local.set $length
            i32.const 256
            call $fill_pending
            i32.const 256
            local.get $length
            call $print
            i32.const 32
            call $lookup
            local.set $secret
            local.get $secret
            i32.const 64
            i32.const 512
            call $sign
            i32.const 512
            i32.const 9
            call $print
            local.get $secret
            i32.const 600
            call $public_key
            i32.const 600
            i32.const 6
            call $print))
    "#;
    let plan = FixturePlan {
        version: 1,
        console: Some(ConsoleFixture {
            script: vec![
                console_output_step("top-secret"),
                console_output_step("signature"),
                console_output_step("public"),
            ],
        }),
        secrets: Some(SecretStoreFixture {
            entries: std::collections::BTreeMap::from([
                (
                    "token".to_owned(),
                    SecretFixture {
                        hex: "746f702d736563726574".to_owned(),
                        usage: SecretUsage::Revealable,
                    },
                ),
                (
                    "signing".to_owned(),
                    SecretFixture {
                        hex: "11".repeat(32),
                        usage: SecretUsage::Signing,
                    },
                ),
            ]),
            script: vec![
                FixtureStep {
                    operation: "secretstore_lookup".to_owned(),
                    target: Some("token".to_owned()),
                    arguments: std::collections::BTreeMap::new(),
                    effective_rights: None,
                    outcome: FixtureOutcome::Return {
                        value: FixtureValue::String("Secret".to_owned()),
                    },
                    required: true,
                },
                FixtureStep {
                    operation: "crypto_reveal_len".to_owned(),
                    target: None,
                    arguments: std::collections::BTreeMap::new(),
                    effective_rights: None,
                    outcome: FixtureOutcome::Return {
                        value: FixtureValue::Map(std::collections::BTreeMap::from([
                            ("redacted".to_owned(), FixtureValue::Bool(true)),
                            ("length".to_owned(), FixtureValue::String("10".to_owned())),
                            (
                                "usage".to_owned(),
                                FixtureValue::String("revealable".to_owned()),
                            ),
                        ])),
                    },
                    required: true,
                },
                FixtureStep {
                    operation: "secretstore_lookup".to_owned(),
                    target: Some("signing".to_owned()),
                    arguments: std::collections::BTreeMap::new(),
                    effective_rights: None,
                    outcome: FixtureOutcome::Return {
                        value: FixtureValue::String("Secret".to_owned()),
                    },
                    required: true,
                },
                FixtureStep {
                    operation: "crypto.sign".to_owned(),
                    target: None,
                    arguments: std::collections::BTreeMap::from([(
                        "message".to_owned(),
                        FixtureValue::String("payload".to_owned()),
                    )]),
                    effective_rights: None,
                    outcome: FixtureOutcome::Return {
                        value: FixtureValue::String("signature".to_owned()),
                    },
                    required: true,
                },
                FixtureStep {
                    operation: "crypto.public_key".to_owned(),
                    target: None,
                    arguments: std::collections::BTreeMap::new(),
                    effective_rights: None,
                    outcome: FixtureOutcome::Return {
                        value: FixtureValue::String("public".to_owned()),
                    },
                    required: true,
                },
            ],
        }),
        ..FixturePlan::default()
    };
    let mut runtime = Runtime::batch().expect("runtime");
    let outcome = runtime
        .run_fixtures(MODULE, plan, 2)
        .expect("compiled fixture run");
    assert!(matches!(outcome.result, FixtureWasmResult::Passed));
    assert_eq!(outcome.output, vec!["top-secret", "signature", "public"]);
    let secret_events = outcome
        .transcript
        .events
        .iter()
        .filter(|event| event.family == FixtureFamily::SecretStore)
        .collect::<Vec<_>>();
    let secret_evidence = format!("{secret_events:?}");
    assert!(!secret_evidence.contains("top-secret"));
    assert!(!secret_evidence.contains("746f702d736563726574"));
    assert_eq!(secret_events.len(), 6);
}

#[cfg(feature = "test-fixtures")]
#[test]
fn compiled_exec_fixture_uses_allowlisted_script_without_spawning() {
    use witchy_testkit::{
        ConsoleFixture, ExecFixture, FilesystemEntry, FilesystemFixture, FixtureFamily,
        FixtureOutcome, FixturePlan, FixtureStep, FixtureValue,
    };

    const PAYLOAD: &str = "7\nouterr";
    const MODULE: &str = r#"
        (module
          (import "witchy" "mint_dir" (func $mint_dir (param i32) (result externref)))
          (import "witchy" "mint_exec" (func $mint_exec (result externref)))
          (import "witchy" "exec_run"
            (func $exec_run (param externref externref i32 i32 i32) (result i32)))
          (import "witchy" "fill_pending" (func $fill_pending (param i32)))
          (import "witchy" "print" (func $print (param i32 i32)))
          (memory (export "memory") 1)
          (data (i32.const 0) "\04\00\00\00tool")
          (data (i32.const 32) "\07\00\00\00--check")
          (data (i32.const 64) "\05\00\00\00input")
          (func (export "run")
            (local $dir externref)
            (local $exec externref)
            (local $length i32)
            i32.const 0
            call $mint_dir
            local.set $dir
            call $mint_exec
            local.set $exec
            local.get $exec
            local.get $dir
            i32.const 0
            i32.const 32
            i32.const 64
            call $exec_run
            local.set $length
            i32.const 256
            call $fill_pending
            i32.const 256
            local.get $length
            call $print))
    "#;
    let plan = FixturePlan {
        version: 1,
        console: Some(ConsoleFixture {
            script: vec![console_output_step(PAYLOAD)],
        }),
        filesystem: Some(FilesystemFixture {
            entries: std::collections::BTreeMap::from([(
                "tool".to_owned(),
                FilesystemEntry::File {
                    hex: "66697874757265".to_owned(),
                },
            )]),
            rights: vec!["Read".to_owned()],
            entry_policy: None,
            script: Vec::new(),
        }),
        exec: Some(ExecFixture {
            tools: vec!["tool".to_owned()],
            script: vec![FixtureStep {
                operation: "exec_run".to_owned(),
                target: Some("tool".to_owned()),
                arguments: std::collections::BTreeMap::from([
                    (
                        "args".to_owned(),
                        FixtureValue::List(vec![FixtureValue::String("--check".to_owned())]),
                    ),
                    (
                        "stdin".to_owned(),
                        FixtureValue::String("input".to_owned()),
                    ),
                ]),
                effective_rights: Some(vec!["exec:tool".to_owned(), "dir:Read".to_owned()]),
                outcome: FixtureOutcome::Return {
                    value: FixtureValue::Map(std::collections::BTreeMap::from([
                        (
                            "exit_code".to_owned(),
                            FixtureValue::String("7".to_owned()),
                        ),
                        ("stdout".to_owned(), FixtureValue::String("out".to_owned())),
                        ("stderr".to_owned(), FixtureValue::String("err".to_owned())),
                    ])),
                },
                required: true,
            }],
        }),
        ..FixturePlan::default()
    };
    let mut runtime = Runtime::batch().expect("runtime");
    let outcome = runtime
        .run_fixtures(MODULE, plan, 2)
        .expect("compiled fixture run");
    assert!(matches!(outcome.result, FixtureWasmResult::Passed));
    assert_eq!(outcome.output, vec![PAYLOAD]);
    assert_eq!(
        outcome
            .transcript
            .events
            .iter()
            .map(|event| (event.family, event.operation.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (FixtureFamily::Filesystem, "mint_dir"),
            (FixtureFamily::Exec, "mint_exec"),
            (FixtureFamily::Exec, "exec_run"),
            (FixtureFamily::Console, "print"),
        ]
    );
}

#[cfg(feature = "test-fixtures")]
fn console_output_step(text: &str) -> witchy_testkit::FixtureStep {
    use witchy_testkit::{FixtureOutcome, FixtureStep, FixtureValue};

    FixtureStep {
        operation: "print".to_owned(),
        target: None,
        arguments: std::collections::BTreeMap::from([(
            "text".to_owned(),
            FixtureValue::String(text.to_owned()),
        )]),
        effective_rights: Some(vec!["Write".to_owned()]),
        outcome: FixtureOutcome::Return {
            value: FixtureValue::Null,
        },
        required: true,
    }
}
use std::path::PathBuf;
