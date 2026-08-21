    use super::*;

    fn check_source(source: &str) -> Result<(), TypeError> {
        let module = witchy_syntax::parser::parse_module(source).expect("source parses");
        check(&module)
    }

    #[test]
    fn pure_named_bodies_allow_local_computation_traps_and_pure_calls() {
        check_source(
            "pure fn increment(x: Int) -> Int:\n    x + 1\n\npure fn compute(x: Int) -> Int:\n    var total = x\n    total = increment(total)\n    total\n\npure fn propagate(value: Result(Int, String)) -> Result(Int, String):\n    let unwrapped = value?\n    Ok(unwrapped)\n\npure fn stop(message: String):\n    fail(message)\n",
        )
        .expect("immutable computation, local mutation, pure calls, control flow, and traps are effect-free");

        let ordinary = check_source(
            "fn increment(x: Int) -> Int:\n    x + 1\n\npure fn compute(x: Int) -> Int:\n    increment(x)\n",
        )
        .expect_err("a pure named function cannot invoke an ordinary named function");
        assert!(ordinary.message.contains("ordinary function `increment`"), "{ordinary:?}");

        let capability = check_str(
            "pure fn log(console: Console):\n    console.print(\"no\")\n",
        )
        .expect_err("a pure body cannot invoke a capability operation");
        assert!(capability.contains("capability operation"), "{capability}");
    }

    #[test]
    fn pure_function_value_calls_require_a_pure_contract() {
        check_source(
            "pure fn invoke(callback: pure fn(Int) -> Int, value: Int) -> Int:\n    callback(value)\n",
        )
        .expect("a pure callable may invoke a callback with a declared-pure contract");

        let ordinary = check_source(
            "pure fn invoke(callback: fn(Int) -> Int, value: Int) -> Int:\n    callback(value)\n",
        )
        .expect_err("ordinary callback effects are opaque at a pure call site");
        assert!(ordinary.message.contains("ordinary function value `callback`"), "{ordinary:?}");

        let applied = check_source(
            "pure fn choose(flag: Bool, left: fn(Int) -> Int, right: fn(Int) -> Int) -> Int:\n    (if flag: left else: right)(1)\n",
        )
        .expect_err("Apply must enforce the callable qualifier too");
        assert!(applied.message.contains("apply an ordinary function value"), "{applied:?}");

        check_source(
            "pure fn choose(flag: Bool, left: pure fn(Int) -> Int, right: pure fn(Int) -> Int) -> Int:\n    (if flag: left else: right)(1)\n",
        )
        .expect("Apply accepts an expression whose joined callable contract remains pure");
    }

    #[test]
    fn pure_intrinsics_reject_dynamic_task_toolchain_and_capability_effects() {
        for (source, expected) in [
            (
                "pure fn inspect():\n    let ignored = __dynamic_runtime_type(\"module\", \"Type\")\n",
                "dynamic behavior",
            ),
            (
                "pure fn schedule():\n    __channel_open(1)\n",
                "task scheduling",
            ),
            (
                "import compiler\n\npure fn audit(source: String) -> String:\n    compiler.footprint(source)\n",
                "compiler/toolchain access",
            ),
            (
                "import secretstore\n\npure fn read(store: SecretStore) -> Option(Secret):\n    secretstore.get(store, \"token\")\n",
                "capability authority",
            ),
        ] {
            let error = check_source(source).expect_err("the intrinsic effect is outside pure");
            assert!(error.message.contains(expected), "{error:?}");
        }
    }

    #[test]
    fn pure_closure_boundaries_reject_authority_and_opaque_behavior_captures() {
        let capability = check_source(
            "pure fn defer(console: Console) -> pure fn() -> Int:\n    pure fn():\n        let ignored = console\n        1\n",
        )
        .expect_err("a pure closure cannot capture a host capability");
        assert!(capability.message.contains("carries capability `Console`"), "{capability:?}");

        let ordinary_callback = check_source(
            "pure fn defer(callback: fn() -> Int) -> fn() -> Int:\n    fn(): callback()\n",
        )
        .expect_err("pure construction cannot delegate captured opaque behavior");
        assert!(ordinary_callback.message.contains("ordinary callable with opaque effects"), "{ordinary_callback:?}");

        let pure_lambda_body = check_source(
            "fn ordinary() -> Int:\n    1\n\nfn factory() -> pure fn() -> Int:\n    pure fn(): ordinary()\n",
        )
        .expect_err("a declared-pure lambda body gets its own enforced pure context");
        assert!(pure_lambda_body.message.contains("ordinary function `ordinary`"), "{pure_lambda_body:?}");

        check_source(
            "fn ordinary() -> Int:\n    1\n\npure fn factory() -> Int:\n    let deferred = fn(): ordinary()\n    0\n",
        )
        .expect("an ordinary nested lambda resets latent purity and may defer opaque behavior without capturing authority");

        check_source(
            "pure fn keep(callback: pure fn(Console) -> Int) -> Int:\n    let deferred = pure fn():\n        let ignored = callback\n        1\n    deferred()\n",
        )
        .expect("a pure callable capture is safe and its capability-bearing signature is not captured storage");

        check_source(
            "pure fn defer(callback: pure fn(Int) -> Int) -> Int:\n    let deferred = pure fn(): callback(1)\n    0\n",
        )
        .expect("a pure closure may invoke a captured callback with an explicit pure contract");

    }

    #[test]
    fn pure_closure_capture_is_not_erased_by_a_later_local_shadow() {
        let shadow_laundering = check_str(
            "pure fn defer(console: Console) -> fn() -> Nil:\n    fn():\n        console.print(\"hidden\")\n        let console = 0\n",
        )
        .expect_err("a later local shadow must not erase an earlier capability capture");
        assert!(shadow_laundering.contains("closure capture `console`"), "{shadow_laundering}");
    }

    #[test]
    fn pure_capture_audit_recurses_through_storage_and_marks_every_user_capability_nominal() {
        let aggregate = check_source(
            "type Holder:\n    Holder(fn() -> Int)\n\npure fn defer(holder: Holder) -> fn() -> Int:\n    fn():\n        let ignored = holder\n        1\n",
        )
        .expect_err("an aggregate that stores an ordinary callable cannot cross a pure boundary");
        assert!(aggregate.message.contains("ordinary callable with opaque effects"), "{aggregate:?}");

        let generic = check_source(
            "pure fn defer(value: a) -> pure fn() -> Int:\n    pure fn():\n        let ignored = value\n        1\n",
        )
        .expect_err("an unresolved stored generic may hide authority");
        assert!(generic.message.contains("generic type may store authority"), "{generic:?}");

        let existential = check_source(
            "trait Inspect:\n    fn inspect(self) -> Int\n\npure fn defer(value: dyn Inspect) -> pure fn() -> Int:\n    pure fn():\n        let ignored = value\n        1\n",
        )
        .expect_err("an existential capture may hide ordinary behavior");
        assert!(existential.message.contains("existential behavior"), "{existential:?}");

        let mut positional = witchy_syntax::parser::parse_module(
            "capability Token:\n    label: String\n\npure fn defer(token: Token) -> pure fn() -> Int:\n    pure fn():\n        let ignored = token\n        1\n",
        )
        .expect("capability source parses");
        let Item::Type(token) = &mut positional.items[0] else {
            panic!("first item is the capability declaration")
        };
        token.sealed = false;
        token.variants[0].field_names.clear();
        let capability = check(&positional)
            .expect_err("positional/unsealed capability identity must not depend on its fields");
        assert!(capability.message.contains("nominal type is declared as a capability"), "{capability:?}");

        check_source(
            "type Grow(a):\n    Value(a)\n    More(Grow(List(a)))\n\npure fn inspect(value: Grow(Int)) -> Int:\n    let deferred = pure fn():\n        let ignored = value\n        1\n    0\n",
        )
        .expect("argument-transforming recursive storage must terminate when it carries only data");

        let transformed = check_source(
            "type Grow(a):\n    Value(a)\n    More(Grow((a, fn() -> Int)))\n\npure fn defer(value: Grow(Int)) -> pure fn() -> Int:\n    pure fn():\n        let ignored = value\n        1\n",
        )
        .expect_err("a transformed recursive stored argument must still expose an ordinary callable");
        assert!(transformed.message.contains("ordinary callable with opaque effects"), "{transformed:?}");
    }

    #[test]
    fn pure_writeback_rejects_var_parameters_but_allows_shadowing_and_locals() {
        let writeback = check_source(
            "pure fn replace(var value: Int):\n    value = value + 1\n",
        )
        .expect_err("assignment to a var parameter writes back to the caller");
        assert!(writeback.message.contains("assign to `var` parameter `value`"), "{writeback:?}");

        let collection_writeback = check_source(
            "pure fn pop(var values: List(Int)) -> Option(Int):\n    list.__pop_extract(values)\n",
        )
        .expect_err("a collection repair cannot write through a pure var parameter");
        assert!(
            collection_writeback
                .message
                .contains("collection repair writes back to the caller"),
            "{collection_writeback:?}"
        );

        let reference_write = check_source(
            "mode opt\n\npure fn replace(target: &'a mut Int):\n    *target = 1\n",
        )
        .expect_err("writing through an explicit reference mutates caller-owned state");
        assert!(reference_write.message.contains("write through an explicit reference"), "{reference_write:?}");

        let lambda_writeback = check_source(
            "fn writer() -> pure fn(var Int) -> Nil:\n    pure fn(var value: Int):\n        value = value + 1\n",
        )
        .expect_err("pure lambda var parameters obey the same writeback rule");
        assert!(lambda_writeback.message.contains("assign to `var` parameter `value`"), "{lambda_writeback:?}");

        check_source(
            "pure fn local(value: Int) -> Int:\n    var result = value\n    result = result + 1\n    result\n",
        )
        .expect("a pure body may mutate its own local storage");

        check_source(
            "pure fn pop_local() -> Option(Int):\n    var values = [1, 2]\n    list.__pop_extract(values)\n",
        )
        .expect("collection write-back into a local variable remains pure");

        check_source(
            "pure fn shadow(var value: Int) -> Int:\n    if true:\n        var value = 1\n        value = value + 1\n        return value\n    0\n",
        )
        .expect("binding identity keeps a shadowing local distinct from the var parameter");
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

        for (source, binding) in [
            (
                "must type Ticket:\n    Ticket(Int)\n\nfn inspect(let ticket: Ticket):\n    let _ = 0\n\nfn defer(let ticket: Ticket) -> fn() -> Nil:\n    fn(): inspect(ticket)\n",
                "ticket",
            ),
            (
                "must type Ticket:\n    Ticket(Int)\n\ntype Envelope:\n    Envelope(Ticket)\n\nfn inspect(let envelope: Envelope):\n    let _ = 0\n\nfn defer(let envelope: Envelope) -> fn() -> Nil:\n    fn(): inspect(envelope)\n",
                "envelope",
            ),
        ] {
            let borrowed = check_source(source)
                .expect_err("an escaping closure cannot hide a borrowed must-consume obligation");
            assert!(
                borrowed.message.contains(&format!(
                    "closure environment carries must-consume `{binding}`; this callable type would erase that obligation"
                )),
                "{borrowed:?}"
            );
        }

        check_source(
            "fn run(own action: fn() -> Int) -> Int:\n    action()\n\nfn main() -> Int:\n    let value = 1\n    run(fn(): value)\n",
        )
        .expect("an owned ordinary closure may still capture ordinary copyable data");
    }

    #[test]
    fn callable_purity_widens_only_toward_ordinary_and_survives_aliases() {
        check_source(
            "pure fn clean(x: Int) -> Int:\n    x\n\nfn invoke(callback: fn(Int) -> Int) -> Int:\n    callback(1)\n\nfn main() -> Int:\n    let widened: fn(Int) -> Int = clean\n    invoke(widened)\n",
        )
        .expect("pure callable should widen to an ordinary reusable callable");

        let narrowing = check_source(
            "pure fn clean(x: Int) -> Int:\n    x\n\nfn main() -> pure fn(Int) -> Int:\n    let widened: fn(Int) -> Int = clean\n    let narrowed: pure fn(Int) -> Int = widened\n    narrowed\n",
        )
        .expect_err("an ordinary alias must not narrow back to pure");
        assert!(narrowing.message.contains("value disagrees"), "{narrowing:?}");

        let ordinary = check_source(
            "fn ordinary(x: Int) -> Int:\n    x\n\nfn require(callback: pure fn(Int) -> Int) -> Int:\n    callback(1)\n\nfn main() -> Int:\n    require(ordinary)\n",
        )
        .expect_err("ordinary callable must not narrow to pure");
        assert!(ordinary.message.contains("expected `pure fn"), "{ordinary:?}");
    }

    #[test]
    fn callable_branch_lub_widens_purity_but_not_cardinality() {
        check_source(
            "pure fn clean(x: Int) -> Int:\n    x\n\nfn ordinary(x: Int) -> Int:\n    x + 1\n\nfn choose(flag: Bool) -> fn(Int) -> Int:\n    let callback = if flag: clean else: ordinary\n    callback\n",
        )
        .expect("mixed-purity branch should join at ordinary callable");

        let cardinality = check_source(
            "pure fn clean(x: Int) -> Int:\n    x\n\nfn choose(flag: Bool, once_callback: once fn(Int) -> Int) -> fn(Int) -> Int:\n    let callback = if flag: clean else: once_callback\n    callback\n",
        )
        .expect_err("reusable and once callables have no branch LUB");
        assert!(
            cardinality.message.contains("invocation cardinality"),
            "{cardinality:?}"
        );
    }

    #[test]
    fn type_table_retains_the_full_callable_signature() {
        use witchy_syntax::ast::Type;

        let module = witchy_syntax::parser::parse_module(
            "pure fn inspect(let text: String) -> Int:\n    0\n",
        )
        .expect("source parses");
        let typed = annotate_checked(module).expect("source type-checks");
        let ast::Type::Fn(parameters, result, conventions, qualifiers) = typed
            .table()
            .function_type("inspect")
            .expect("non-generic function signature")
        else {
            panic!("function table entry must remain callable")
        };
        assert_eq!(parameters, vec![ast::Type::Named("String".into(), Vec::new())]);
        assert_eq!(*result, ast::Type::Named("Int".into(), Vec::new()));
        assert_eq!(conventions, vec![Convention::Borrow]);
        assert_eq!(qualifiers, CallableQualifiers::new(true, false));
    }

    #[test]
    fn once_callable_invocation_consumes_but_scope_exit_may_drop() {
        check_source(
            "fn main():\n    let unused: once fn() -> Int = once fn() -> Int: 1\n",
        )
        .expect("affine once callables are droppable");

        let repeated = check_source(
            "fn main():\n    let callback: once fn() -> Int = once fn() -> Int: 1\n    let first = callback()\n    let second = callback()\n    let _ = first + second\n",
        )
        .expect_err("a once callable may be invoked at most once");
        assert!(
            repeated
                .message
                .contains("consumed by invocation"),
            "{repeated:?}"
        );

        let copied = check_source(
            "fn main():\n    let callback: once fn() -> Int = once fn() -> Int: 1\n    let alias = callback\n    let _ = alias()\n",
        )
        .expect_err("binding a second alias would copy affine storage");
        assert!(copied.message.contains("would be copied"), "{copied:?}");

        let discarded_place = check_source(
            "fn main():\n    let callback: once fn() -> Int = once fn() -> Int: 1\n    callback\n    let _ = callback()\n",
        )
        .expect_err("discarding a place expression must not copy affine storage");
        assert!(
            discarded_place.message.contains("discarded expression")
                && discarded_place.message.contains("would be copied"),
            "{discarded_place:?}"
        );

        let projected = check_source(
            "fn main():\n    let callbacks = [once fn() -> Int: 1]\n    let first = callbacks[0]()\n    let second = callbacks[0]()\n    let _ = first + second\n",
        )
        .expect_err("calling a projected once value consumes its owning root");
        assert!(projected.message.contains("consumed by invocation"), "{projected:?}");

        check_source(
            "fn main():\n    let reusable: fn(once fn() -> Int) -> Int = fn(callback: once fn() -> Int) -> Int: callback()\n    let alias = reusable\n    let _ = alias(once fn() -> Int: 1)\n",
        )
        .expect("a reusable callable merely mentioning a once parameter is not affine storage");
    }

    #[test]
    fn once_parameter_conventions_require_transfer_and_reject_borrowed_invocation_or_var() {
        check_source(
            "fn invoke(callback: once fn() -> Int) -> Int:\n    callback()\n\nfn main() -> Int:\n    let callback: once fn() -> Int = once fn() -> Int: 1\n    invoke(move callback)\n",
        )
        .expect("a default immutable parameter owns a once callback after explicit transfer");

        let copied = check_source(
            "fn invoke(callback: once fn() -> Int) -> Int:\n    callback()\n\nfn main() -> Int:\n    let callback: once fn() -> Int = once fn() -> Int: 1\n    invoke(callback)\n",
        )
        .expect_err("a default once parameter may not copy its caller's binding");
        assert!(copied.message.contains("would be copied"), "{copied:?}");

        let borrowed = check_source(
            "fn inspect(let callback: once fn() -> Int) -> Int:\n    callback()\n",
        )
        .expect_err("an explicit borrow may not consume its caller's once callback");
        assert!(
            borrowed.message.contains("borrowed once-callable")
                && borrowed.message.contains("cannot be invoked"),
            "{borrowed:?}"
        );

        let borrowed_match = check_source(
            "type OnceBox:\n    OnceBox(once fn() -> Int)\n\nfn inspect(let boxed: OnceBox) -> Int:\n    match boxed:\n        OnceBox(_) -> 0\n",
        )
        .expect_err("matching an affine wrapper transfers it and cannot consume a borrow");
        assert!(
            borrowed_match
                .message
                .contains("cannot destructure borrowed affine value"),
            "{borrowed_match:?}"
        );

        let variable = check_source(
            "fn replace(var callback: once fn() -> Int) -> Int:\n    callback()\n",
        )
        .expect_err("var once parameters require definite reinitialization, which is not in v1");
        assert!(variable.message.contains("empty write-back slot"), "{variable:?}");

        for source in [
            "fn both(own left: once fn() -> Int, own right: once fn() -> Int):\n    let _ = 0\n\nfn main():\n    let callback: once fn() -> Int = once fn() -> Int: 1\n    both(callback, callback)\n",
            "fn both(own left: once fn() -> Int, own right: once fn() -> Int):\n    let _ = 0\n\nfn main():\n    let invoke = both\n    let callback: once fn() -> Int = once fn() -> Int: 1\n    invoke(callback, callback)\n",
        ] {
            let duplicate_own = check_source(source)
                .expect_err("two implicit-own arguments may not consume the same root");
            assert!(
                duplicate_own.message.contains("already transferred"),
                "{duplicate_own:?}"
            );
        }

        let existential_duplicate = check_str(
            "trait InvokeBoth:\n    fn both(self, own left: once fn() -> Int, own right: once fn() -> Int)\n\ntype Runner:\n    Runner\n\nimpl InvokeBoth for Runner:\n    fn both(self, own left: once fn() -> Int, own right: once fn() -> Int):\n        let _ = 0\n\nfn main():\n    let dynamic = Runner as dyn InvokeBoth\n    let callback: once fn() -> Int = once fn() -> Int: 1\n    dynamic.both(callback, callback)\n",
        )
        .expect_err("existential convention enforcement must reject duplicate own roots");
        assert!(
            existential_duplicate.contains("already transferred"),
            "{existential_duplicate}"
        );
    }

    #[test]
    fn trait_method_contract_preserves_once_result_consumption() {
        let repeated = check_str(
            "trait Supply:\n    fn supply(self) -> once fn() -> Int\n\ntype Source:\n    Source\n\nimpl Supply for Source:\n    fn supply(self) -> once fn() -> Int:\n        once fn() -> Int: 1\n\nfn main():\n    let callback = Source.supply()\n    let first = callback()\n    let second = callback()\n    let _ = first + second\n",
        )
        .expect_err("trait-backed callable results retain once identity");
        assert!(repeated.contains("consumed by invocation"), "{repeated}");
    }

    #[test]
    fn once_explicit_reference_handles_are_rejected_without_referent_provenance() {
        let shared_aliases = check_source(
            "mode opt\n\nfn main():\n    let callback: once fn() -> Int = once fn() -> Int: 1\n    let left = &callback\n    let right = &callback\n    let first = left()\n    let second = right()\n    let _ = first + second\n",
        )
        .expect_err("copyable shared handles cannot alias a once referent");
        assert!(
            shared_aliases
                .message
                .contains("explicit shared reference to affine once-callable"),
            "{shared_aliases:?}"
        );

        let dereferenced = check_source(
            "mode opt\n\nfn main():\n    let callback: once fn() -> Int = once fn() -> Int: 1\n    let handle = &callback\n    let first = (*handle)()\n    let second = (*handle)()\n    let _ = first + second\n",
        )
        .expect_err("deref application must not make a reference-rooted once value callable");
        assert!(
            dereferenced.message.contains("explicit shared reference"),
            "{dereferenced:?}"
        );

        let exclusive = check_source(
            "mode opt\n\nfn main():\n    var callback: once fn() -> Int = once fn() -> Int: 1\n    let handle = &mut callback\n    let _ = (*handle)()\n",
        )
        .expect_err("exclusive handles also lack referent-consumption state in v1");
        assert!(
            exclusive
                .message
                .contains("explicit exclusive reference to affine once-callable"),
            "{exclusive:?}"
        );

        let direct_parameter = check_source(
            "mode opt\n\nfn inspect(callback: &'a once fn() -> Int) -> Int:\n    0\n",
        )
        .expect_err("signatures may not admit a reference to a once callable");
        assert!(
            direct_parameter
                .message
                .contains("contains an explicit reference to affine once-callable storage"),
            "{direct_parameter:?}"
        );

        let nominal_parameter = check_source(
            "mode opt\n\ntype OnceBox:\n    OnceBox(once fn() -> Int)\n\nfn inspect(boxed: &'a OnceBox) -> Int:\n    0\n",
        )
        .expect_err("signatures may not admit a reference to nominal affine storage");
        assert!(
            nominal_parameter
                .message
                .contains("contains an explicit reference to affine once-callable storage"),
            "{nominal_parameter:?}"
        );

        let field = check_source(
            "mode opt\n\ntype RefBox('a):\n    callback: &'a once fn() -> Int\n",
        )
        .expect_err("nominal fields may not store a reference to a once callable");
        assert!(
            field
                .message
                .contains("contains an explicit reference to affine once-callable storage"),
            "{field:?}"
        );
    }

    #[test]
    fn once_transfer_is_preserved_through_returns_branches_matches_and_aggregates() {
        check_source(
            "type OnceBox:\n    OnceBox(once fn() -> Int)\n\nfn choose(flag: Bool, own left: once fn() -> Int, own right: once fn() -> Int) -> once fn() -> Int:\n    if flag:\n        move left\n    else:\n        move right\n\nfn unwrap(own boxed: OnceBox) -> once fn() -> Int:\n    match boxed:\n        OnceBox(callback) -> move callback\n\nfn main() -> Int:\n    let boxed = OnceBox(choose(true, once fn() -> Int: 1, once fn() -> Int: 2))\n    let callback = unwrap(move boxed)\n    callback()\n",
        )
        .expect("branch, aggregate, match, and return boundaries preserve affine transfer");

        check_source(
            "must type Completion:\n    Completion(once fn() -> Int)\n\nfn finish(own completion: Completion) -> Int:\n    match completion:\n        Completion(callback) -> callback()\n\nfn main() -> Int:\n    let completion = Completion(once fn() -> Int: 1)\n    finish(completion)\n",
        )
        .expect("an own operation may destructure a must wrapper and consume its once payload without an explicit match move");

        let implicit_return = check_source(
            "fn forward(callback: once fn() -> Int) -> once fn() -> Int:\n    callback\n",
        )
        .expect_err("returning a place without move would copy the callback");
        assert!(implicit_return.message.contains("would be copied"), "{implicit_return:?}");

        let aggregate_copy = check_source(
            "type OnceBox:\n    OnceBox(once fn() -> Int)\n\nfn main():\n    let boxed = OnceBox(once fn() -> Int: 1)\n    let alias = boxed\n    let _ = alias\n",
        )
        .expect_err("nominal storage carrying a once callback is affine");
        assert!(aggregate_copy.message.contains("would be copied"), "{aggregate_copy:?}");

        let nested_aggregate_copy = check_source(
            "type Box(a):\n    Box(a)\n\nfn main():\n    let nested = Box(Box(once fn() -> Int: 1))\n    let alias = nested\n    let _ = alias\n",
        )
        .expect_err("argument-changing nominal nesting must retain affine storage");
        assert!(
            nested_aggregate_copy.message.contains("would be copied"),
            "{nested_aggregate_copy:?}"
        );

        check_source(
            "type Phantom(a):\n    Phantom\n\nfn copy(value: Phantom(once fn() -> Int)) -> Phantom(once fn() -> Int):\n    value\n",
        )
        .expect("a phantom generic argument does not make nominal storage affine");

        let transformed_recursive = check_source(
            "type Grow(a):\n    Grow(a, Grow(once fn() -> Int))\n\nfn copy(value: Grow(Int)) -> Grow(Int):\n    value\n",
        )
        .expect_err("a transformed recursive field must expose its stored once callback");
        assert!(
            transformed_recursive.message.contains("would be copied"),
            "{transformed_recursive:?}"
        );

        check_source(
            "type Grow(a):\n    Grow(a, Grow(List(a)))\n\nfn copy(value: Grow(Int)) -> Grow(Int):\n    value\n",
        )
        .expect("argument-growing recursive fields terminate when no once callback is stored");

        let hidden = check_source(
            "fn main():\n    let callback: once fn() -> Int = once fn() -> Int: 1\n    let deferred = fn() -> Int: callback()\n    let _ = deferred\n",
        )
        .expect_err("a closure environment may not hide affine storage");
        assert!(hidden.message.contains("captures affine once-callable"), "{hidden:?}");

        let hidden_aggregate = check_source(
            "type OnceBox:\n    OnceBox(once fn() -> Int)\n\nfn main():\n    let boxed = OnceBox(once fn() -> Int: 1)\n    let deferred = fn() -> OnceBox: move boxed\n    let _ = deferred\n",
        )
        .expect_err("a closure environment may not hide nominal affine storage");
        assert!(
            hidden_aggregate
                .message
                .contains("captures affine once-callable"),
            "{hidden_aggregate:?}"
        );

        let hidden_before_shadow = check_source(
            "fn main():\n    let callback: once fn() -> Int = once fn() -> Int: 1\n    let deferred = fn() -> Int:\n        let first = callback()\n        let callback: once fn() -> Int = once fn() -> Int: 2\n        first\n    let _ = deferred\n",
        )
        .expect_err("a later lambda-local shadow must not hide an earlier affine capture");
        assert!(
            hidden_before_shadow
                .message
                .contains("captures affine once-callable"),
            "{hidden_before_shadow:?}"
        );
    }

    #[test]
    fn once_expected_aggregate_injection_and_yield_boundaries_require_transfer() {
        for (source, context) in [
            (
                "fn pack(callback: once fn() -> Int) -> List(once fn() -> Int):\n    [callback]\n",
                "list construction",
            ),
            (
                "fn pack(callback: once fn() -> Int) -> (once fn() -> Int,):\n    (callback,)\n",
                "tuple construction",
            ),
            (
                "fn pack(callback: once fn() -> Int) -> .[Ready(once fn() -> Int)]:\n    .Ready(callback)\n",
                "anonymous union tag `.Ready`",
            ),
        ] {
            let error = check_source(source)
                .expect_err("an expected aggregate shape must not copy affine payloads");
            assert!(
                error.message.contains("would be copied")
                    && error.message.contains(context),
                "{error:?}"
            );
        }

        let implicit = witchy_syntax::parser::parse_module(
            "type Iter(a):\n    Iter(List(a))\n\ngen fn values(callback: once fn() -> Int) -> Iter(once fn() -> Int):\n    yield callback\n",
        )
        .expect("generator source parses");
        let names = ["values".to_string()].into_iter().collect::<HashSet<_>>();
        let conversions = HashSet::default();
        let error = check_selected_lowered(&implicit, &names, &conversions)
            .expect_err("yielding a frame-held callback without transfer would copy it");
        assert!(
            error.message.contains("generator yield")
                && error.message.contains("would be copied"),
            "{error:?}"
        );

        let transferred = witchy_syntax::parser::parse_module(
            "type Iter(a):\n    Iter(List(a))\n\ngen fn values(callback: once fn() -> Int) -> Iter(once fn() -> Int):\n    yield move callback\n    let _ = callback()\n",
        )
        .expect("generator source parses");
        let error = check_selected_lowered(&transferred, &names, &conversions)
            .expect_err("an explicitly yielded callback remains consumed after resume");
        assert!(error.message.contains("moved or transferred"), "{error:?}");
    }

    #[test]
    fn once_loop_backedges_reject_outer_consumption_but_allow_fresh_elements() {
        let repeated = check_source(
            "fn repeat(callback: once fn() -> Int):\n    while true:\n        let _ = callback()\n",
        )
        .expect_err("a loop backedge could invoke the same callback twice");
        assert!(repeated.message.contains("loop backedge"), "{repeated:?}");

        check_source(
            "fn main():\n    let callbacks = [once fn() -> Int: 1, once fn() -> Int: 2]\n    for callback in move callbacks:\n        let _ = callback()\n",
        )
        .expect("each transferred list element is a fresh affine binding");
    }

    #[test]
    fn once_match_guards_and_arm_results_cannot_duplicate_affine_paths() {
        let guarded = check_source(
            "fn guarded(flag: Bool, callback: once fn() -> Bool) -> Bool:\n    match flag:\n        true if callback() -> true\n        _ -> callback()\n",
        )
        .expect_err("a false guard may fall through after evaluating its callback");
        assert!(guarded.message.contains("match guard cannot consume"), "{guarded:?}");

        let arm_alias = check_source(
            "fn select(flag: Bool, own left: once fn() -> Int, own right: once fn() -> Int) -> once fn() -> Int:\n    match flag:\n        true -> left\n        false -> right\n",
        )
        .expect_err("a match arm result may not copy its affine place");
        assert!(arm_alias.message.contains("match arm result"), "{arm_alias:?}");
    }

    #[test]
    fn once_projection_roots_survive_ascriptions_and_must_aggregates_reject_partial_moves() {
        let projected = check_source(
            "fn main():\n    let private: .{callback: once fn() -> Int, marker: Int} = .{callback: once fn() -> Int: 1, marker: 0}\n    let first = ((private as .{callback: once fn() -> Int}).callback)()\n    let second = ((private as .{callback: once fn() -> Int}).callback)()\n    let _ = first + second\n",
        )
        .expect_err("nested record ascription must preserve the projected affine root");
        assert!(projected.message.contains("consumed by invocation"), "{projected:?}");

        let partial_must = check_source(
            "must type Ticket:\n    Ticket(Int)\n\ntype Envelope:\n    ticket: Ticket\n    callback: once fn() -> Int\n\nfn main():\n    let envelope = Envelope(Ticket(1), once fn() -> Int: 1)\n    let callback = move envelope.callback\n    let _ = callback()\n",
        )
        .expect_err("a projected once move may not discharge an entire must aggregate");
        assert!(partial_must.message.contains("cannot partially move"), "{partial_must:?}");
    }

    #[test]
    fn once_payloads_cannot_be_erased_or_escape_generic_fallback_checking() {
        let ordinary_generic = witchy_syntax::parser::parse_module(
            "fn identity(value: a) -> a:\n    value\n\nfn main() -> Int:\n    identity(1)\n",
        )
        .expect("ordinary generic fixture parses");
        let ordinary_generic = crate::traits::lower_checked(ordinary_generic)
            .expect("non-affine generics retain their ordinary fallback");
        assert!(ordinary_generic.items.iter().all(|item| {
            !matches!(item, ast::Item::Function(function) if function.name.starts_with("identity__"))
        }));

        let erased = check_source(
            "trait Run:\n    fn run(self) -> Int\n\ntype OnceBox:\n    OnceBox(once fn() -> Int)\n\nimpl Run for OnceBox:\n    fn run(self) -> Int:\n        0\n\nfn main():\n    let boxed = OnceBox(once fn() -> Int: 1)\n    let hidden = move boxed as dyn Run\n    let _ = hidden\n",
        )
        .expect_err("existential packing may not erase affine payload storage");
        assert!(erased.message.contains("cannot erase affine once-callable payload"), "{erased:?}");

        for (template, source) in [
            (
                "fn duplicate(value: a) -> (a, a):\n    (value, value)\n",
                "fn duplicate(value: a) -> (a, a):\n    (value, value)\n\nfn main():\n    let callback: once fn() -> Int = once fn() -> Int: 1\n    let pair = duplicate(move callback)\n",
            ),
            (
                "type OnceBox:\n    OnceBox(once fn() -> Int)\n\nfn duplicate(value: a) -> (a, a):\n    (value, value)\n",
                "type OnceBox:\n    OnceBox(once fn() -> Int)\n\nfn duplicate(value: a) -> (a, a):\n    (value, value)\n\nfn main():\n    let boxed = OnceBox(once fn() -> Int: 1)\n    let pair = duplicate(move boxed)\n",
            ),
            (
                "fn duplicate(value: a) -> (a, a):\n    (value, value)\n",
                "fn duplicate(value: a) -> (a, a):\n    (value, value)\n\nfn main():\n    let duplicate_once: fn(once fn() -> Int) -> (once fn() -> Int, once fn() -> Int) = duplicate\n    let callback: once fn() -> Int = once fn() -> Int: 1\n    let pair = duplicate_once(move callback)\n",
            ),
        ] {
            let template = witchy_syntax::parser::parse_module(template)
                .expect("generic template parses");
            check(&template).expect("the unconstrained generic template is valid in isolation");
            let module = witchy_syntax::parser::parse_module(source).expect("source parses");
            let error = crate::traits::lower_checked(module)
                .expect_err("once instantiation must generate and recheck a concrete body");
            assert!(error.contains("would be copied"), "{error}");
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
    fn trait_method_purity_is_a_directed_callable_contract() {
        let missing = "trait Inspect:\n    pure fn inspect(self) -> Int\n\
                       type Box:\n    Box(Int)\n\
                       impl Inspect for Box:\n    fn inspect(self) -> Int:\n        0\n";
        let error = check_str(missing).expect_err("ordinary impl cannot satisfy a pure method");
        assert!(
            error.contains("ordinary, but the trait requires `pure fn`"),
            "{error}"
        );

        let stronger = "trait Inspect:\n    fn inspect(self) -> Int\n\
                        type Box:\n    Box(Int)\n\
                        impl Inspect for Box:\n    pure fn inspect(self) -> Int:\n        0\n";
        check_str(stronger).expect("pure impl may satisfy an ordinary method contract");
        let module = witchy_syntax::parser::parse_module(stronger).expect("parse");
        let lowered = crate::traits::lower_checked(module).expect("lower");
        assert!(lowered.items.iter().any(|item| matches!(
            item,
            witchy_syntax::ast::Item::Function(function)
                if function.name.ends_with("__inspect") && function.pure
        )));

        let pure_dispatch = "trait Inspect:\n    pure fn inspect(self) -> Int\n\
                             type Box:\n    Box(Int)\n\
                             impl Inspect for Box:\n    pure fn inspect(self) -> Int:\n        0\n\
                             pure fn read(boxed: Box) -> Int:\n    boxed.inspect()\n";
        check_str(pure_dispatch)
            .expect("trait lowering must preserve the selected implementation's pure qualifier");

        let existential_dispatch = check_str(
            "trait Inspect:\n    pure fn inspect(self) -> Int\n\npure fn read(value: dyn Inspect) -> Int:\n    value.inspect()\n",
        )
        .expect_err("existential dispatch has no statically selected pure implementation");
        assert!(existential_dispatch.contains("existential method"), "{existential_dispatch}");
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
