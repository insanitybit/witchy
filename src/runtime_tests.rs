    use super::*;

    const IMPORTS_PRINT: &str = r#"
        (module
          (import "witchy" "print" (func $print (param i32 i32)))
          (memory (export "memory") 1)
          (func (export "run")))
    "#;

    const RECEIVER: &str = r#"
        (module
          (import "witchy" "recv" (func $recv (param i32 i32) (result i32)))
          (global $got (export "got") (mut i32) (i32.const -2))
          (memory (export "memory") 1)
          (func (export "run")
            (global.set $got (call $recv (i32.const 0) (i32.const 256)))))
    "#;

    fn sender(target: ActorId, len: i32) -> String {
        format!(
            r#"
            (module
              (import "witchy" "send" (func $send (param i32 i32 i32)))
              (memory (export "memory") 1)
              (func (export "run")
                (call $send (i32.const {target}) (i32.const 0) (i32.const {len}))))
            "#
        )
    }

    const SPINNER: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "run") (loop $l (br $l))))
    "#;

    const GREEDY: &str = r#"
        (module (memory (export "memory") 4) (func (export "run")))
    "#;

    /// The core thesis: a capability that was not granted simply does not exist
    /// for the actor, so it cannot even be instantiated.
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
        let mut actor = rt
            .spawn(IMPORTS_PRINT, Capabilities { print: true, ..Default::default() }, 4)
            .unwrap();
        actor.run().unwrap();
    }

    /// Two actors with no shared memory exchange a message purely through the
    /// host-mediated mailbox; the receiver observes the delivered byte count.
    #[test]
    fn message_passing_delivers_across_isolated_vms() {
        let mut rt = Runtime::new().unwrap();
        let mut receiver = rt
            .spawn(RECEIVER, Capabilities::none(), 4)
            .unwrap();
        let mut sender = rt
            .spawn(sender(receiver.id, 5), Capabilities { send: true, ..Default::default() }, 4)
            .unwrap();

        sender.run().unwrap();
        receiver.run().unwrap();

        let got = receiver
            .instance
            .get_global(&mut receiver.store, "got")
            .unwrap()
            .get(&mut receiver.store)
            .i32()
            .unwrap();
        assert_eq!(got, 5, "receiver should have read a 5-byte message");
    }

    #[test]
    fn send_to_unknown_actor_traps() {
        let mut rt = Runtime::new().unwrap();
        let mut sender = rt
            .spawn(sender(9999, 5), Capabilities { send: true, ..Default::default() }, 4)
            .unwrap();
        assert!(sender.run().is_err(), "sending to a nonexistent actor must fail");
    }

    /// Each actor's linear memory is its own; the runtime hands out separate
    /// `Store`s, so one actor's memory is never visible to another.
    #[test]
    fn actors_have_independent_memories() {
        let mut rt = Runtime::new().unwrap();
        let mut a = rt.spawn(RECEIVER, Capabilities::default(), 4).unwrap();
        let mut b = rt.spawn(RECEIVER, Capabilities::default(), 4).unwrap();
        let mem_a = a.instance.get_memory(&mut a.store, "memory").unwrap();
        let mem_b = b.instance.get_memory(&mut b.store, "memory").unwrap();
        // Distinct backing allocations.
        assert_ne!(
            mem_a.data_ptr(&a.store),
            mem_b.data_ptr(&b.store),
            "actors must not share a linear memory"
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

    /// A runaway actor that never yields is forcibly preempted by the scheduler.
    #[test]
    fn runaway_actor_is_preempted() {
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
