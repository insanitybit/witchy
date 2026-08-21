    use super::*;

    fn check_source(source: &str) -> Result<(), TypeError> {
        let module = witchy_syntax::parser::parse_module(source).expect("source parses");
        check(&module)
    }

    #[test]
    fn must_consume_requires_disposition_on_every_path() {
        let prelude = "must type Ticket:\n    Ticket(Int)\n\nfn make() -> Ticket:\n    Ticket(1)\n\nfn finish(own ticket: Ticket):\n    let _ = 0\n\n";

        let missing = check_source(&format!(
            "{prelude}fn main():\n    let ticket = make()\n"
        ))
        .expect_err("scope exit must reject a live obligation");
        assert!(missing.message.contains("must-consume value `ticket`"));

        let one_branch = check_source(&format!(
            "{prelude}fn run(flag: Bool):\n    let ticket = make()\n    if flag:\n        finish(ticket)\n\nfn main():\n    run(true)\n"
        ))
        .expect_err("one branch cannot discharge an all-path obligation");
        assert!(one_branch.message.contains("must-consume value `ticket`"));

        check_source(&format!(
            "{prelude}fn run(flag: Bool):\n    let ticket = make()\n    if flag:\n        finish(ticket)\n    else:\n        finish(ticket)\n\nfn main():\n    run(true)\n"
        ))
        .expect("both branches consume the obligation");

        check_source(&format!(
            "{prelude}fn score(own ticket: Ticket) -> Int:\n    1\n\nfn run(flag: Bool) -> Int:\n    let ticket = make()\n    let result: Int = if flag:\n        score(ticket)\n    else:\n        score(ticket)\n    result\n\nfn main():\n    let _ = run(true)\n"
        ))
        .expect("expected-type checking isolates moves made by sibling branches");
    }

    #[test]
    fn must_consume_cfg_join_excludes_terminating_branches() {
        let prelude = "must type Ticket:\n    Ticket(Int)\n\nfn make() -> Ticket:\n    Ticket(1)\n\nfn finish(own ticket: Ticket):\n    let _ = 0\n\n";

        check_source(&format!(
            "{prelude}fn run(flag: Bool) -> Int:\n    let ticket = make()\n    if flag:\n        finish(ticket)\n        return 1\n    finish(ticket)\n    2\n\nfn main():\n    let _ = run(true)\n"
        ))
        .expect("a terminating branch does not move from its fallthrough successor");

        check_source(&format!(
            "{prelude}fn run(flag: Bool) -> Int:\n    let ticket = make()\n    match flag:\n        true ->\n            finish(ticket)\n            return 1\n        false -> ()\n    finish(ticket)\n    2\n\nfn main():\n    let _ = run(false)\n"
        ))
        .expect("match joins also exclude terminating arm state");

        let abandoned = check_source(&format!(
            "{prelude}fn run(flag: Bool) -> Int:\n    let ticket = make()\n    if flag:\n        return 1\n    finish(ticket)\n    2\n\nfn main():\n    let _ = run(true)\n"
        ))
        .expect_err("a terminating branch must discharge every live obligation");
        assert!(
            abandoned
                .message
                .contains("return leaves must-consume value `ticket` undisposed"),
            "{}",
            abandoned.message
        );
    }

    #[test]
    fn must_consume_question_mark_checks_the_error_return_edge_after_call_effects() {
        let prelude = "must type Ticket:\n    Ticket(Int)\n\nfn make() -> Ticket:\n    Ticket(1)\n\nfn validate() -> Result(Int, String):\n    Err(\"invalid\")\n\nfn finish(own ticket: Ticket) -> Result(Int, String):\n    Err(\"finished\")\n\n";

        let abandoned = check_source(&format!(
            "{prelude}fn run() -> Result(Int, String):\n    let ticket = make()\n    let value = validate()?\n    Ok(value)\n\nfn main():\n    let _ = run()\n"
        ))
        .expect_err("the error edge of `?` may not abandon a live obligation");
        assert!(
            abandoned
                .message
                .contains("return leaves must-consume value `ticket` undisposed"),
            "{abandoned:?}"
        );

        check_source(&format!(
            "{prelude}fn run() -> Result(Int, String):\n    let ticket = make()\n    let value = finish(ticket)?\n    Ok(value)\n\nfn main():\n    let _ = run()\n"
        ))
        .expect("an own call discharges its obligation before `?` propagates its error");
    }

    #[test]
    fn must_consume_transfers_without_copying_and_propagates_through_aggregates() {
        let returned = "must type Ticket:\n    Ticket(Int)\n\nfn make() -> Ticket:\n    Ticket(1)\n\nfn forward() -> Ticket:\n    let ticket = make()\n    ticket\n\nfn finish(own ticket: Ticket):\n    let _ = 0\n\nfn main():\n    finish(forward())\n";
        check_source(returned).expect("return and own-call boundaries transfer obligations");

        let copied = check_source(
            "must type Ticket:\n    Ticket(Int)\n\nfn make() -> Ticket:\n    Ticket(1)\n\nfn finish(own ticket: Ticket):\n    let _ = 0\n\nfn main():\n    let first = make()\n    let second = first\n    finish(second)\n",
        )
        .expect_err("a linear obligation cannot be copied");
        assert!(copied.message.contains("would copy must-consume value `first`"));

        let aggregate = check_source(
            "must type Ticket:\n    Ticket(Int)\n\ntype Envelope:\n    Envelope(Ticket)\n\nfn make() -> Ticket:\n    Ticket(1)\n\nfn main():\n    let envelope = Envelope(make())\n",
        )
        .expect_err("an aggregate containing a must value carries the obligation");
        assert!(aggregate.message.contains("must-consume value `envelope`"));
    }

    #[test]
    fn must_consume_own_calls_discharge_at_attempt_and_shadowing_keeps_binding_identity() {
        let source = "must type Ticket:\n    Ticket(Int)\n\nfn make() -> Ticket:\n    Ticket(1)\n\nfn try_finish(own ticket: Ticket) -> Bool:\n    false\n\nfn finish(own ticket: Ticket):\n    let _ = 0\n\nfn main():\n    let ticket = make()\n    if true:\n        let ticket = make()\n        finish(ticket)\n    let attempted = try_finish(ticket)\n    let _ = attempted\n";

        check_source(source).expect(
            "an own call discharges on invocation even when its result reports failure, and a shadowed obligation remains distinct",
        );
    }

    #[test]
    fn owned_function_values_cannot_erase_must_consume_closure_captures() {
        let prefix = "must type Ticket:\n    Ticket(Int)\n\nfn make() -> Ticket:\n    Ticket(1)\n\nfn finish(own ticket: Ticket):\n    let _ = 0\n\nfn run(own action: fn() -> Nil):\n    action()\n\n";
        let cases = [
            format!(
                "{prefix}fn main():\n    let ticket = make()\n    run(fn(): finish(ticket))\n"
            ),
            format!(
                "{prefix}fn main():\n    let invoke = run\n    let ticket = make()\n    invoke(fn(): finish(ticket))\n"
            ),
        ];

        for source in cases {
            let error = check_source(&source).expect_err(
                "an opaque callable may be dropped, so `own fn` cannot hide a must-consume capture",
            );
            assert!(
                error.message.contains(
                    "closure environment carries must-consume `ticket`; this callable type would erase that obligation"
                ),
                "{error:?}"
            );
        }
    }

    #[test]
    fn must_consume_borrows_require_a_live_owner_and_only_own_operations_may_destructure() {
        let temporary_borrow = check_source(
            "must type Ticket:\n    Ticket(Int)\n\nfn make() -> Ticket:\n    Ticket(1)\n\nfn inspect(let ticket: Ticket) -> Bool:\n    true\n\nfn main():\n    inspect(make())\n",
        )
        .expect_err("borrowing a temporary must value would lose its obligation");
        assert!(temporary_borrow.message.contains("borrows a temporary must-consume value"));

        check_source(
            "must type Ticket:\n    Ticket(Int)\n\nfn make() -> Ticket:\n    Ticket(1)\n\nfn inspect(let ticket: Ticket) -> Bool:\n    true\n\nfn finish(own ticket: Ticket):\n    match ticket:\n        Ticket(_) -> ()\n\nfn main():\n    let consume = finish\n    let ticket = make()\n    let seen = inspect(ticket)\n    let _ = seen\n    consume(ticket)\n",
        )
        .expect("callables are not obligations, a live owner may be borrowed, and an own operation may inspect consumed state");

        let consume_borrow = check_source(
            "must type Ticket:\n    Ticket(Int)\n\nfn finish(own ticket: Ticket):\n    let _ = 0\n\nfn invalid(let ticket: Ticket):\n    finish(ticket)\n\nfn main():\n    let ticket = Ticket(1)\n    invalid(ticket)\n    finish(ticket)\n",
        )
        .expect_err("a borrowed must value cannot cross an own boundary");
        assert!(consume_borrow.message.contains("cannot consume borrowed must-consume value `ticket`"));
    }

    #[test]
    fn must_consume_generic_propagation_follows_owning_field_positions() {
        let deferred = "must type Ticket:\n    Ticket(Int)\n\ntype Boxed(a):\n    Boxed(a)\n\ntype Recipe(a):\n    Recipe(fn() -> a)\n\nfn finish(own ticket: Ticket):\n    let _ = 0\n\nfn main():\n    let recipe = Recipe(fn(): Ticket(1))\n    let _ = recipe\n    finish(Ticket(2))\n";
        check_source(deferred)
            .expect("a callable result type is not storage owned by its enclosing recipe");

        let stored = check_source(
            "must type Ticket:\n    Ticket(Int)\n\ntype Boxed(a):\n    Boxed(a)\n\nfn main():\n    let boxed = Boxed(Ticket(1))\n",
        )
        .expect_err("a generic field that stores its parameter propagates the obligation");
        assert!(stored.message.contains("must-consume value `boxed`"));
    }

    #[test]
    fn suspension_frame_own_parameters_assume_must_obligations() {
        let mut module = witchy_syntax::parser::parse_module(
            "must type Ticket:\n    Ticket(Int)\n\nfn segment(own ticket: Ticket):\n    let _ = 0\n\nfn main():\n    let _ = 0\n",
        )
        .expect("frame-obligation fixture parses");
        let witchy_syntax::ast::Item::Function(segment) = &mut module.items[1] else {
            panic!("expected segment function")
        };
        segment
            .attributes
            .push(witchy_syntax::suspension::FRAME_FUNCTION_ATTRIBUTE.into());

        let error = check(&module).expect_err("a frame slot may not drop its transferred obligation");
        assert!(error.message.contains("must-consume value `ticket`"), "{error:?}");
    }

    #[test]
    fn typed_module_rebuilds_address_keyed_facts_after_structural_rewrite() {
        use witchy_syntax::ast::{Expr, Item, Stmt};

        fn tail(module: &witchy_syntax::ast::Module) -> &Expr {
            let Item::Function(main) = &module.items[0] else {
                panic!("expected main function")
            };
            let Some(Stmt::Expr(expr)) = main.body.stmts.last() else {
                panic!("expected tail expression")
            };
            expr
        }

        let module = witchy_syntax::parser::parse_module("fn value() -> Int:\n    1\n")
            .expect("parse");
        let typed = annotate(module);
        assert_eq!(typed.table().type_of(tail(typed.module())), Some(&Ty::Int));

        let typed = typed.rewrite_and_reannotate(|_, module| {
            let Item::Function(main) = &mut module.items[0] else {
                panic!("expected main function")
            };
            *main.body.stmts.last_mut().expect("tail statement") =
                Stmt::Expr(Expr::Str("now a string".into()));
            main.ret = Some(witchy_syntax::ast::Type::Named("String".into(), Vec::new()));
        });

        assert_eq!(
            typed.table().type_of(tail(typed.module())),
            Some(&Ty::String)
        );
    }

    #[test]
    fn resolved_capability_types_preserve_rights_when_converted_to_ast() {
        let named = |name: &str| ast::Type::Named(name.to_string(), Vec::new());
        assert_eq!(
            ty_to_ast(&Ty::Dir(DirRights { read: true, write: false })),
            Some(ast::Type::Named("Dir".into(), vec![named("Read")]))
        );
        assert_eq!(
            ty_to_ast(&Ty::File(FileRights { read: false, write: true })),
            Some(ast::Type::Named("File".into(), vec![named("Write")]))
        );
        assert_eq!(
            ty_to_ast(&Ty::Net(NetRights {
                connect: true,
                listen: false,
                tcp: true,
                udp: false,
                uds: false,
            })),
            Some(ast::Type::Named(
                "Net".into(),
                vec![named("Connect"), named("Tcp")],
            ))
        );
        assert_eq!(
            ty_to_ast(&Ty::Dir(DirRights::full())),
            Some(named("Dir")),
        );
        assert_eq!(
            ty_to_ast(&Ty::Dir(DirRights { read: false, write: false })),
            None,
        );
        assert_eq!(
            ty_to_ast(&Ty::File(FileRights { read: false, write: false })),
            None,
        );
        assert_eq!(
            ty_to_ast(&Ty::Net(NetRights {
                connect: false,
                listen: false,
                tcp: true,
                udp: false,
                uds: false,
            })),
            None,
        );
        assert_eq!(
            ty_to_ast(&Ty::Net(NetRights {
                connect: true,
                listen: false,
                tcp: false,
                udp: false,
                uds: false,
            })),
            None,
        );
    }

    // (BUG-009 / RFC-0011 + RFC-0005 hardening #4) Policy narrowing preserves the
    // rights set at the type level, and a handle carrying narrowed rights cannot be
    // re-widened by a cast after passing through `net.only` / `dir.only`.
    //
    // NOTE: the *address/entry policy* that `only`/`deny` apply is enforced only at
    // runtime (host-side) and has NO type-level representation — the return type is
    // `Net[rights]` / `Dir[rights]` with the same rights, no policy component. So the
    // only type-level re-widening surface is the rights axis, which is what these
    // assertions cover; the address-set policy itself cannot be "re-widened at the
    // type level" because it is not in the type at all. (`NetPolicy`/`DirPolicy` are
    // declared locally here because `check_str` type-checks a single module without
    // linking `std/confine`; they unify by name with the builtin op expectations.)
    #[test]
    fn compiler_generated_structural_impls_accept_resolved_trait_identity() {
        let record_reflect = ast::ImplDef {
            origin: ast::ImplOrigin::CompilerGenerated,
            trait_name: Some("reflect.Reflect".into()),
            trait_args: Vec::new(),
            type_name: "__anon123".into(),
            target_args: Vec::new(),
            bounds: Vec::new(),
            methods: Vec::new(),
        };
        assert!(is_compiler_generated_structural_impl(&record_reflect));

        let source_lookalike = ast::ImplDef {
            origin: ast::ImplOrigin::Source,
            ..record_reflect
        };
        assert!(!is_compiler_generated_structural_impl(&source_lookalike));
    }

    #[test]
    fn imported_borrowed_nominal_container_signature_rejects_before_lowering() {
        fn no_comptime(
            _name: &str,
            _module: &mut witchy_syntax::ast::Module,
            _siblings: &[(String, witchy_syntax::ast::Module)],
        ) -> Result<witchy_syntax::origin::OriginTable, String> {
            Ok(witchy_syntax::origin::OriginTable::default())
        }

        let views = witchy_syntax::parser::parse_module(
            "mode opt\n\ntype Holder('a):\n    view: View(String, 'a)\n",
        )
        .expect("parse borrowed nominal module");
        let main = witchy_syntax::parser::parse_module(
            "mode opt\n\nimport views\n\nfn bad(let owner: let('a) String, let holders: List(views.Holder('a))) -> Int:\n    0\n",
        )
        .expect("parse imported borrowed container use");
        let error = crate::pipeline::link_checked(
            vec![("views".into(), views), ("main".into(), main)],
            "main",
            no_comptime,
        )
        .expect_err("cross-module borrowed containers stay behind the descriptor stage");
        let error = error.to_string();
        assert!(
            error.contains("stores a borrowed nominal relation inside `List`")
                && error.contains("descriptor/root-lowering stage"),
            "{error}"
        );
    }

    #[test]
    fn imported_borrowed_nominal_runtime_use_rejects_before_lowering() {
        fn no_comptime(
            _name: &str,
            _module: &mut witchy_syntax::ast::Module,
            _siblings: &[(String, witchy_syntax::ast::Module)],
        ) -> Result<witchy_syntax::origin::OriginTable, String> {
            Ok(witchy_syntax::origin::OriginTable::default())
        }

        let views = witchy_syntax::parser::parse_module(
            "mode opt\n\ntype Cursor('a):\n    view: View(String, 'a)\n    offset: Int\n",
        )
        .expect("parse borrowed nominal module");
        let main = witchy_syntax::parser::parse_module(
            "mode opt\n\nimport views\n\nfn bad(let owner: let('a) String, let cursor: views.Cursor('a)) -> Int:\n    cursor.offset\n",
        )
        .expect("parse imported borrowed runtime use");
        let error = crate::pipeline::link_checked(
            vec![("views".into(), views), ("main".into(), main)],
            "main",
            no_comptime,
        )
        .expect_err("cross-module borrowed values stay behind owner-root lowering")
        .to_string();
        assert!(
            error.contains("borrowed nominal type")
                && error.contains("runtime owner-root lowering"),
            "{error}"
        );
    }

    #[test]
    fn same_named_trait_methods_dispatch_by_trait_identity() {
        use witchy_syntax::ast::{Expr, Item, Stmt};

        let src = "trait Label:\n    fn name(self) -> String\n\
                   trait DebugName:\n    fn name(self) -> String\n\
                   type User:\n    User(String)\n\
                   impl Label for User:\n    fn name(self) -> String:\n        \"label\"\n\
                   impl DebugName for User:\n    fn name(self) -> String:\n        \"debug\"\n\
                   fn label(x: a) -> String where a: Label:\n    name(x)\n\
                   fn debug_name(x: a) -> String where a: DebugName:\n    name(x)\n\
                   fn main(console: Console):\n    console.print(label(User(\"u\")) + debug_name(User(\"u\")))\n";
        check_str(src).expect("same-named trait methods are scoped by the active bound");

        let module = witchy_syntax::parser::parse_module(src).expect("parse");
        let lowered = crate::traits::lower_checked(module).expect("lower");
        let lowered_call = |prefix: &str| -> String {
            lowered
                .items
                .iter()
                .find_map(|item| match item {
                    Item::Function(f) if f.name.starts_with(prefix) => match f.body.stmts.last() {
                        Some(Stmt::Expr(Expr::Call { name, .. })) => Some(name.clone()),
                        _ => None,
                    },
                    _ => None,
                })
                .unwrap_or_else(|| panic!("missing lowered call for {prefix}"))
        };
        assert_eq!(lowered_call("label__"), "Label__User__name");
        assert_eq!(lowered_call("debug_name__"), "DebugName__User__name");

        let ambiguous = "trait Label:\n    fn name(self) -> String\n\
                         trait DebugName:\n    fn name(self) -> String\n\
                         type User:\n    User(String)\n\
                         impl Label for User:\n    fn name(self) -> String:\n        \"label\"\n\
                         impl DebugName for User:\n    fn name(self) -> String:\n        \"debug\"\n\
                         fn bad(u: User) -> String:\n    u.name()\n";
        let err = check_str(ambiguous).unwrap_err();
        assert!(err.contains("ambiguous") && err.contains("Label") && err.contains("DebugName"), "{err}");
    }

    #[test]
    fn capability_methods_keep_method_origin_after_lowering() {
        use witchy_syntax::ast::{Expr, Item, Stmt};

        let src = r#"
fn read(f: File[Read]) -> String:
    "shadow"

fn main(console: Console, f: File[Read]):
    console.print(f.read())
"#;
        check_str(src).expect("file read capability method should type-check");

        let module = witchy_syntax::parser::parse_module(src).expect("parse");
        let lowered = crate::traits::lower_checked(module).expect("lower");
        let main = lowered
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(f) if f.name == "main" => Some(f),
                _ => None,
            })
            .expect("main");
        let Some(Stmt::Expr(Expr::Call { name, args })) = main.body.stmts.last() else {
            panic!("expected lowered console.print call");
        };
        assert_eq!(name, "__capop.print");
        let Some(Expr::Call { name: inner, .. }) = args.get(1) else {
            panic!("expected lowered f.read call");
        };
        assert_eq!(inner, "__capop.read");
    }

    #[test]
    fn capability_methods_prefer_host_ops_over_std_owner_modules() {
        use witchy_syntax::ast::{Expr, Item, Stmt};

        let src = r#"
fn main(rand: Rand, net: Net[Listen, Tcp]):
    rand.rand_u64()
    let listener = net.listen("127.0.0.1:0")
    listener.serve_pool()
"#;
        assert!(check_str(src).is_ok());

        let module = witchy_syntax::parser::parse_module(src).expect("parse");
        let lowered = crate::traits::lower_checked(module).expect("lower");
        let main = lowered
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(f) if f.name == "main" => Some(f),
                _ => None,
            })
            .expect("main");
        let Some(Stmt::Expr(Expr::Call { name: rand_name, .. })) = main.body.stmts.first() else {
            panic!("expected lowered rand.rand_u64 call");
        };
        assert_eq!(rand_name, "__capop.rand_u64");
        let Some(Stmt::Expr(Expr::Call { name: serve_name, .. })) = main.body.stmts.last() else {
            panic!("expected lowered listener.serve_pool call");
        };
        assert_eq!(serve_name, "__capop.serve_pool");
    }

    #[test]
    fn capability_op_chains_preserve_receiver_kind() {
        use witchy_syntax::ast::{Expr, Item, Stmt};

        let src = r#"
type DirPolicy:
    Any

fn load(dir: Dir[Read], policy: DirPolicy) -> String:
    dir.only(policy).read("config.txt")
"#;
        check_str(src).expect("Dir.only(...).read(...) should type-check");

        let module = witchy_syntax::parser::parse_module(src).expect("parse");
        let lowered = crate::traits::lower_checked(module).expect("lower");
        let load = lowered
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(f) if f.name == "load" => Some(f),
                _ => None,
            })
            .expect("load");
        let Some(Stmt::Expr(Expr::Call { name: read_name, args: read_args })) =
            load.body.stmts.last()
        else {
            panic!("expected lowered dir.read call");
        };
        assert_eq!(read_name, "__capop.read");
        let Some(Expr::Call { name: only_name, .. }) = read_args.first() else {
            panic!("expected lowered dir.only receiver");
        };
        assert_eq!(only_name, "__capop.only");
    }

    #[test]
    fn dequalify_home_strips_only_the_home_module() {
        // BUG-292: a home-module name renders bare; a cross-module name keeps its
        // qualifier (it disambiguates a same-named type from another module).
        assert_eq!(dequalify_home("t_file.Color", "t_file"), "Color");
        assert_eq!(dequalify_home("helper.Token", "t_file"), "helper.Token");
        assert_eq!(dequalify_home("Bool", "t_file"), "Bool");
        assert_eq!(dequalify_home("t_file.Color", ""), "t_file.Color");
    }

    #[test]
    fn strip_home_qualifiers_keeps_cross_module_names() {
        // Home-module type in a mismatch renders bare...
        assert_eq!(
            strip_home_qualifiers("expected `String`, found `app.Point`", "app"),
            "expected `String`, found `Point`"
        );
        // ...while two cross-module same-named types keep BOTH qualifiers (the exact
        // `expected Token, found Token` confusion RFC-0042 forbids).
        assert_eq!(
            strip_home_qualifiers("expected `helper_b.Token`, found `helper_a.Token`", "main"),
            "expected `helper_b.Token`, found `helper_a.Token`"
        );
        // The `module.fn` location prefix (lowercase suffix) is never stripped.
        assert_eq!(strip_home_qualifiers("in `app.go`: boom", "app"), "in `app.go`: boom");
        // An unknown home is a no-op.
        assert_eq!(strip_home_qualifiers("found `app.Point`", ""), "found `app.Point`");
    }
