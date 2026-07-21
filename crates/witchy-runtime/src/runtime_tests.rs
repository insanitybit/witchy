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

    #[test]
    fn null_file_externref_is_rejected() {
        let path = std::env::temp_dir().join(format!(
            "witchy-null-file-externref-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, "host-owned").unwrap();

        let mut rt = Runtime::new().unwrap();
        let mut vm = rt
            .spawn(
                NULL_FILE_READ,
                Capabilities {
                    file_grants: vec![path.clone()],
                    ..Default::default()
                },
                4,
            )
            .unwrap();
        let err = vm.run().unwrap_err();
        let _ = std::fs::remove_file(path);
        let detail = format!("{err:?}");
        assert!(
            detail.contains("File externref is null"),
            "expected null File externref rejection, got: {detail}"
        );
    }

    #[test]
    fn null_dir_externref_is_rejected() {
        let root = std::env::temp_dir().join(format!(
            "witchy-null-dir-externref-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let mut rt = Runtime::new().unwrap();
        let mut vm = rt
            .spawn(
                NULL_DIR_READ,
                Capabilities {
                    dir_root: Some(root.clone()),
                    dir_read: true,
                    ..Default::default()
                },
                4,
            )
            .unwrap();
        let err = vm.run().unwrap_err();
        let _ = std::fs::remove_dir_all(root);
        let detail = format!("{err:?}");
        assert!(
            detail.contains("Dir externref is null"),
            "expected null Dir externref rejection, got: {detail}"
        );
    }

    #[test]
    fn null_net_externref_is_rejected() {
        let mut rt = Runtime::new().unwrap();
        let mut vm = rt
            .spawn(
                NULL_NET_CONNECT,
                Capabilities {
                    net_allow: Some(vec!["127.0.0.1:1".to_string()]),
                    net_connect: true,
                    ..Default::default()
                },
                4,
            )
            .unwrap();
        let err = vm.run().unwrap_err();
        let detail = format!("{err:?}");
        assert!(
            detail.contains("Net externref is null"),
            "expected null Net externref rejection, got: {detail}"
        );
    }

    #[test]
    fn null_socket_externref_is_rejected() {
        let mut rt = Runtime::new().unwrap();
        let mut vm = rt
            .spawn(
                NULL_SOCKET_CLOSE,
                Capabilities {
                    net_allow: Some(vec!["127.0.0.1:1".to_string()]),
                    net_connect: true,
                    ..Default::default()
                },
                4,
            )
            .unwrap();
        let err = vm.run().unwrap_err();
        let detail = format!("{err:?}");
        assert!(
            detail.contains("Socket externref is null"),
            "expected null Socket externref rejection, got: {detail}"
        );
    }

    #[test]
    fn null_listener_externref_is_rejected() {
        let mut rt = Runtime::new().unwrap();
        let mut vm = rt
            .spawn(
                NULL_LISTENER_ACCEPT,
                Capabilities {
                    net_allow: Some(vec!["127.0.0.1:0".to_string()]),
                    net_listen: true,
                    ..Default::default()
                },
                4,
            )
            .unwrap();
        let err = vm.run().unwrap_err();
        let detail = format!("{err:?}");
        assert!(
            detail.contains("Listener externref is null"),
            "expected null Listener externref rejection, got: {detail}"
        );
    }

    #[test]
    fn null_secret_externref_is_rejected_by_reveal() {
        let mut rt = Runtime::new().unwrap();
        let mut vm = rt
            .spawn(NULL_SECRET_REVEAL, Capabilities::default(), 4)
            .unwrap();
        let err = vm.run().unwrap_err();
        let detail = format!("{err:?}");
        assert!(
            detail.contains("Secret externref is null"),
            "expected null Secret externref rejection, got: {detail}"
        );
    }

    #[test]
    fn null_secret_externref_is_rejected_by_sign() {
        let mut rt = Runtime::new().unwrap();
        let mut vm = rt
            .spawn(
                NULL_SECRET_SIGN,
                Capabilities {
                    signing_key: Some([0x41; 32]),
                    secrets: vec![SecretGrant::new("signing", vec![0x41; 32])],
                    ..Default::default()
                },
                4,
            )
            .unwrap();
        let err = vm.run().unwrap_err();
        let detail = format!("{err:?}");
        assert!(
            detail.contains("Secret externref is null"),
            "expected null Secret externref rejection, got: {detail}"
        );
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
