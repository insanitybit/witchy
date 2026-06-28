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

    const GREEDY: &str = r#"
        (module (memory (export "memory") 4) (func (export "run")))
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
