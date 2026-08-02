    //! RFC-0083 phase-1 loan/lifetime checker tests. Each runs the full public
    //! checker (`check_str`) over a small `mode opt` program and asserts the loan
    //! rule accepts or rejects it with the documented diagnostic.

    use crate::{
        loans::facts,
        typeck::{check, check_str},
    };
    use witchy_syntax::ast::Item;

    use super::{LoanProjection, LoanProjectionStep};

    fn linked_normal(main_body: &str) -> Result<(), crate::typeck::TypeError> {
        fn no_comptime(
            _name: &str,
            _module: &mut witchy_syntax::ast::Module,
            _siblings: &[(String, witchy_syntax::ast::Module)],
        ) -> Result<witchy_syntax::origin::OriginTable, String> {
            Ok(witchy_syntax::origin::OriginTable::default())
        }

        let api = witchy_syntax::parser::parse_module(
            "mode opt\n\npub fn view(xs: let('a) List(Int)) -> View(List(Int), 'a):\n    xs\n",
        )
        .expect("parse opt API");
        let main = witchy_syntax::parser::parse_module(&format!(
            "import api\nimport list\n\nfn consume(own xs: List(Int)) -> Int:\n    list.length(xs)\n\nfn clear(var xs: List(Int)):\n    xs = []\n\nfn main(console: Console):\n{main_body}"
        ))
        .expect("parse normal caller");
        let linked = witchy_syntax::linker::link(
            vec![("main".into(), main), ("api".into(), api)],
            "main",
            no_comptime,
        )
        .expect("link normal caller to opt API");
        check(&linked)
    }

    /// A borrowed-view helper plus a `main` body, as a `mode opt` module. Includes
    /// LOCAL `owned`/`send` helpers so these checker tests need no std linking
    /// (`check_str` does not resolve `import`s); the loan checker recognizes any
    /// callee named `owned`/`send` by its bare name, exactly as it would the std
    /// ones. The end-to-end std versions live in `src/example_tests.rs`.
    fn opt(body: &str) -> String {
        format!(
            "mode opt\n\n\
             fn borrow(text: let('a) String) -> View(String, 'a):\n    text\n\n\
             fn clear(var x: String):\n    x = \"\"\n\n\
             fn owned(x: String) -> String:\n    x\n\n\
             fn send(ch: String, x: String) -> String:\n    ch\n\n\
             fn main(console: Console):\n{body}"
        )
    }

    // --- signature relations (acceptance 1) ---------------------------------

    #[test]
    fn view_syntax_requires_mode_opt() {
        let err = check_str(
            "fn borrow(text: let('a) String) -> View(String, 'a):\n    text\n",
        )
        .expect_err("views outside mode opt are rejected");
        assert!(err.contains("mode opt"), "{err}");
    }

    #[test]
    fn output_view_lifetime_must_be_bound_by_an_input() {
        let err = check_str("mode opt\n\nfn bad(text: String) -> View(String, 'a):\n    text\n")
            .expect_err("an unbound output lifetime is rejected");
        assert!(err.contains("no parameter borrows with that lifetime"), "{err}");
    }

    #[test]
    fn borrowed_param_may_not_be_mutable() {
        let err = check_str(
            "mode opt\n\nfn bad(var text: let('a) String) -> View(String, 'a):\n    text\n",
        )
        .expect_err("a mutable view parameter is a contradiction");
        assert!(err.contains("read-only") || err.contains("cannot be mutated"), "{err}");
    }

    #[test]
    fn well_formed_view_signature_checks() {
        check_str(
            "mode opt\n\nfn first(text: let('a) String) -> View(String, 'a):\n    text\n",
        )
        .expect("a well-formed borrowed view signature checks");
    }

    // --- owner loans: rejection (acceptance 2) ------------------------------

    #[test]
    fn reassigning_a_loaned_owner_is_rejected() {
        let err = check_str(&opt("    var s = \"hi\"\n    let w = borrow(s)\n    s = \"x\"\n    console.print(w)\n"))
            .expect_err("reassigning the owner while a view is live is rejected");
        assert!(err.contains("reassigned"), "{err}");
        assert!(err.contains("still \n             live") || err.contains("still live"), "{err}");
    }

    #[test]
    fn moving_a_loaned_owner_is_rejected() {
        let err = check_str(&opt(
            "    var s = \"hi\"\n    let w = borrow(s)\n    let t = move s\n    console.print(w)\n",
        ))
        .expect_err("moving the owner while a view is live is rejected");
        assert!(err.contains("moved"), "{err}");
    }

    #[test]
    fn passing_a_loaned_owner_to_a_var_parameter_is_rejected() {
        let err = check_str(&opt(
            "    var s = \"hi\"\n    let w = borrow(s)\n    clear(s)\n    console.print(w)\n",
        ))
        .expect_err("passing the owner to a `var` param while a view is live is rejected");
        assert!(err.contains("`var` parameter"), "{err}");
    }

    #[test]
    fn normal_callers_enforce_every_owner_conflict_from_an_opt_result() {
        let cases = [
            (
                "reassign",
                "    var xs = [1]\n    let w = api.view(xs)\n    xs = [2]\n    console.print(\"${list.length(w)}\")\n",
                "reassigned",
            ),
            (
                "move",
                "    let xs = [1]\n    let w = api.view(xs)\n    let moved = move xs\n    console.print(\"${list.length(w) + list.length(moved)}\")\n",
                "moved (`move`)",
            ),
            (
                "own argument",
                "    let xs = [1]\n    let w = api.view(xs)\n    let n = consume(xs)\n    console.print(\"${list.length(w) + n}\")\n",
                "`own` parameter",
            ),
            (
                "var argument",
                "    var xs = [1]\n    let w = api.view(xs)\n    clear(xs)\n    console.print(\"${list.length(w)}\")\n",
                "`var` parameter",
            ),
            (
                "indirect call and mutation",
                "    var xs = [1]\n    let make_view = api.view\n    let w = make_view(xs)\n    list.push(xs, 2)\n    console.print(\"${list.length(w)}\")\n",
                "reassigned",
            ),
        ];

        for (case, body, conflict) in cases {
            let error = linked_normal(body)
                .expect_err(&format!("normal caller must reject {case} while its opt view is live"));
            let message = error.to_string();
            assert!(message.contains(conflict), "{case}: {message}");
            assert!(message.contains("view"), "{case}: {message}");
        }

        linked_normal(
            "    var xs = [1]\n    let make_view = api.view\n    let w = make_view(xs)\n    console.print(\"${list.length(w)}\")\n    list.push(xs, 2)\n    console.print(\"${list.length(xs)}\")\n",
        )
        .expect("an imported indirect view ends its owner loan at the view's last use");
    }

    // --- non-lexical last use + .owned() (acceptance 3) ---------------------

    #[test]
    fn mutating_owner_after_view_last_use_is_allowed() {
        // The view's last mention is before the reassignment, so the loan has
        // ended — a non-lexical window, not the whole scope.
        check_str(&opt(
            "    var s = \"hi\"\n    let w = borrow(s)\n    console.print(w)\n    s = \"x\"\n    console.print(s)\n",
        ))
        .expect("mutating the owner after the view's last use is allowed");
    }

    #[test]
    fn materializing_with_owned_ends_the_loan() {
        // `owned(w)` returns an OWNED value (opens no loan) and is the view's last
        // use, so the owner is free to mutate afterward. Detection is purely
        // result-position, not by callee name; the std spelling is `w.owned()`.
        check_str(&opt(
            "    var s = \"hi\"\n    let w = borrow(s)\n    let keep = owned(w)\n    s = \"x\"\n    console.print(keep)\n    console.print(s)\n",
        ))
        .expect("materializing the view ends the loan");
    }

    // --- wrappers + multiple shared views (acceptance 4) --------------------

    #[test]
    fn multiple_shared_views_coexist() {
        check_str(&opt(
            "    var s = \"hi\"\n    let a = borrow(s)\n    let b = borrow(s)\n    console.print(a)\n    console.print(b)\n",
        ))
        .expect("any number of read-only views may coexist");
    }

    #[test]
    fn rebinding_a_view_transfers_its_owner_loan() {
        let err = check_str(&opt(
            "    var s = \"hi\"\n    let first = borrow(s)\n    let second = first\n    s = \"x\"\n    console.print(second)\n",
        ))
        .expect_err("a view alias keeps the owner loan live");
        assert!(err.contains("reassigned"), "{err}");
        assert!(err.contains("second"), "the active alias is named: {err}");
    }

    #[test]
    fn loan_relation_propagates_through_a_wrapper() {
        // `wrapper` re-returns `inner`'s view; its OWN signature carries the
        // borrow, so a caller still loans the owner (relation survives the call).
        let src = "mode opt\n\n\
             fn inner(text: let('a) String) -> View(String, 'a):\n    text\n\n\
             fn wrapper(text: let('b) String) -> View(String, 'b):\n    inner(text)\n\n\
             fn main(console: Console):\n    var s = \"hi\"\n    let w = wrapper(s)\n    s = \"x\"\n    console.print(w)\n";
        let err = check_str(src).expect_err("a conflict through one wrapper is still caught");
        assert!(err.contains("reassigned"), "{err}");
        assert!(err.contains("wrapper"), "the diagnostic names the borrowing call: {err}");

        let rebound = "mode opt\n\nfn borrow(text: let('a) String) -> View(String, 'a):\n    text\n\nfn forward(text: let('b) String) -> View(String, 'b):\n    text\n\nfn main(console: Console):\n    var s = \"hi\"\n    let first = borrow(s)\n    let second = forward(first)\n    s = \"x\"\n    console.print(second)\n";
        let err = check_str(rebound)
            .expect_err("forwarding a bound view must keep the original owner loaned");
        assert!(err.contains("owner `s` is reassigned"), "{err}");
    }

    #[test]
    fn loan_relation_survives_a_function_value() {
        let src = "mode opt\n\n\
             fn borrow(text: let('a) String) -> View(String, 'a):\n    text\n\n\
             fn main(console: Console):\n    var s = \"hi\"\n    let f = borrow\n    let w = f(s)\n    s = \"x\"\n    console.print(w)\n";
        let err = check_str(src).expect_err("an indirect returned view still loans its owner");
        assert!(err.contains("reassigned"), "{err}");
        assert!(err.contains("`f`") || err.contains("`borrow`"), "{err}");
    }

    #[test]
    fn function_type_may_not_erase_a_borrow_relation() {
        let src = "mode opt\n\n\
             fn borrow(text: let('a) String) -> View(String, 'a):\n    text\n\n\
             fn main(console: Console):\n    let f: fn(String) -> String = borrow\n    console.print(f(\"hi\"))\n";
        let err = check_str(src).expect_err("an ascription may not erase a borrow relation");
        assert!(err.contains("erases or changes its borrow/convention relation"), "{err}");

        let cast = "mode opt\n\nfn borrow(text: let('a) String) -> View(String, 'a):\n    text\n\nfn main(console: Console):\n    let f = borrow as fn(String) -> String\n    console.print(f(\"hi\"))\n";
        let err = check_str(cast).expect_err("a cast may not erase a borrow relation");
        assert!(err.contains("cannot ascribe") || err.contains("function cast erases"), "{err}");

        let argument = "mode opt\n\nfn borrow(text: let('a) String) -> View(String, 'a):\n    text\n\nfn use(f: fn(String) -> String) -> String:\n    f(\"x\")\n\nfn main(console: Console):\n    console.print(use(borrow))\n";
        let err = check_str(argument)
            .expect_err("a higher-order argument may not erase a borrow relation");
        assert!(err.contains("argument 1 passed to `use` erases"), "{err}");

        let returned = "mode opt\n\nfn borrow(text: let('a) String) -> View(String, 'a):\n    text\n\nfn erase() -> fn(String) -> String:\n    borrow\n";
        let err = check_str(returned)
            .expect_err("a returned function value may not erase a borrow relation");
        assert!(err.contains("returned function value erases"), "{err}");
    }

    #[test]
    fn function_value_may_not_erase_an_ownership_qualifier() {
        let src = "mode opt\n\n\
             fn constrained(xs: unique List(Int)) -> Int:\n    list.length(xs)\n\n\
             fn use(f: fn(List(Int)) -> Int) -> Nil:\n    return\n\n\
             fn main():\n    use(constrained)\n";
        let err = check_str(src).expect_err("a higher-order argument may not erase `unique`");
        assert!(err.contains("ownership/access contract"), "{err}");
        assert!(err.contains("Qualifier"), "{err}");
    }

    #[test]
    fn inferred_function_declaration_retains_its_access_contract() {
        let src = "mode opt\n\n\
             fn constrained(xs: unique List(Int)):\n    list.length(xs)\n\n\
             fn use(f: fn(List(Int)) -> Int) -> Nil:\n    return\n\n\
             fn main():\n    use(constrained)\n";
        let err = check_str(src)
            .expect_err("an inferred declaration result may not erase its parameter qualifier");
        assert!(err.contains("ownership/access contract"), "{err}");
        assert!(err.contains("Qualifier"), "{err}");
    }

    #[test]
    fn polymorphic_function_values_are_specialized_before_access_comparison() {
        check_str(
            "fn id(x: a) -> a:\n    x\n\n\
             fn use(f: fn(Int) -> Int) -> Int:\n    f(1)\n\n\
             fn main() -> Int:\n    use(id)\n",
        )
        .expect("the generic identity function has the concrete use-site access signature");
    }

    #[test]
    fn inferred_lambda_parameters_use_the_finalized_callable_type() {
        check_str(
            "fn use(f: fn(Int) -> Int) -> Int:\n    f(1)\n\n\
             fn main() -> Int:\n    use(fn(x) -> Int: x + 1)\n",
        )
        .expect("an inferred lambda parameter comes from its checked type, not a placeholder");
    }

    #[test]
    fn callable_access_contracts_flow_through_patterns_and_branch_joins() {
        let bodies = [
            "    let (f, _) = (constrained, 0)\n    use(f)\n",
            "    let f = if true:\n        constrained\n    else:\n        constrained\n    use(f)\n",
            "    let f = match true:\n        true -> constrained\n        false -> constrained\n    use(f)\n",
        ];

        for body in bodies {
            let src = format!(
                "mode opt\n\n\
                 fn constrained(xs: unique List(Int)) -> Int:\n    list.length(xs)\n\n\
                 fn use(f: fn(List(Int)) -> Int) -> Nil:\n    return\n\n\
                 fn main():\n{body}"
            );
            let err = check_str(&src)
                .expect_err("flow-sensitive bindings may not erase a callable qualifier");
            assert!(err.contains("ownership/access contract"), "{body}: {err}");
            assert!(err.contains("Qualifier"), "{body}: {err}");
        }
    }

    #[test]
    fn nested_local_unique_result_cannot_escape() {
        let err = check_str(
            "mode opt\n\nfn bad() -> (local unique List(Int), Int):\n    ([1], 0)\n",
        )
        .expect_err("local unique is activation-bound even below a result aggregate");
        assert!(err.contains("local unique") && err.contains("escape"), "{err}");
    }

    #[test]
    fn nominal_constructor_cannot_erase_a_callable_access_contract() {
        let err = check_str(
            "mode opt\n\n\
             type Holder:\n    Holder(fn(unique List(Int)) -> Int)\n\n\
             fn plain(xs: List(Int)) -> Int:\n    list.length(xs)\n\n\
             fn main():\n    let holder: Holder = Holder(plain)\n    return\n",
        )
        .expect_err("a nominal field must preserve its callable's unique parameter");
        assert!(err.contains("constructor `Holder`") && err.contains("Qualifier"), "{err}");
    }

    #[test]
    fn nominal_record_construction_cannot_erase_a_callable_access_contract() {
        let err = check_str(
            "mode opt\n\n\
             type Holder:\n    f: fn(unique List(Int)) -> Int\n\n\
             fn plain(xs: List(Int)) -> Int:\n    list.length(xs)\n\n\
             fn main():\n    let holder: Holder = Holder(f: plain)\n    return\n",
        )
        .expect_err("a nominal record field must preserve its callable access contract");
        assert!(err.contains("constructor `Holder`") && err.contains("Qualifier"), "{err}");
    }

    #[test]
    fn nominal_record_update_cannot_erase_a_callable_access_contract() {
        let err = check_str(
            "mode opt\n\n\
             type Holder:\n    f: fn(unique List(Int)) -> Int\n\n\
             fn strict(xs: unique List(Int)) -> Int:\n    list.length(xs)\n\n\
             fn plain(xs: List(Int)) -> Int:\n    list.length(xs)\n\n\
             fn main():\n\
             \x20   let initial: Holder = Holder(f: strict)\n\
             \x20   let updated: Holder = Holder(f: plain, ..initial)\n\
             \x20   return\n",
        )
        .expect_err("a nominal record update must preserve its callable access contract");
        assert!(err.contains("record field `f`") && err.contains("Qualifier"), "{err}");
    }

    #[test]
    fn generic_record_update_preserves_the_instantiated_callable_contract() {
        let err = check_str(
            "mode opt\n\n\
             type Box(a):\n    value: a\n\n\
             fn plain(xs: List(Int)) -> Int:\n    list.length(xs)\n\n\
             fn corrupt(box: Box(fn(unique List(Int)) -> Int)) -> Box(fn(unique List(Int)) -> Int):\n\
             \x20   Box(value: plain, ..box)\n\n\
             fn main():\n    return\n",
        )
        .expect_err("a generic record update must retain its instantiated access identity");
        assert!(err.contains("record field `value`") && err.contains("Qualifier"), "{err}");
    }

    #[test]
    fn nominal_record_fields_keep_their_contract_across_order_projection_and_update() {
        check_str(
            "mode opt\n\n\
             type Holder:\n    count: Int\n    f: fn(unique List(Int)) -> Int\n\n\
             fn strict(xs: unique List(Int)) -> Int:\n    list.length(xs)\n\n\
             fn main():\n\
             \x20   let initial: Holder = Holder(f: strict, count: 1)\n\
             \x20   let replaced: Holder = Holder(f: strict, ..initial)\n\
             \x20   let untouched: Holder = Holder(count: 2, ..replaced)\n\
             \x20   let f = untouched.f\n\
             \x20   f([1])\n",
        )
        .expect("declared record fields, rather than source order, carry access identity");
    }

    #[test]
    fn existential_pack_uses_the_authenticated_target_access_identity() {
        check_str(
            "mode opt\n\n\
             trait Carrier(a):\n    fn get(let self) -> a\n\n\
             type Holder:\n    f: fn(unique List(Int)) -> Int\n\n\
             impl Carrier(fn(unique List(Int)) -> Int) for Holder:\n\
             \x20   fn get(let self) -> fn(unique List(Int)) -> Int:\n        self.f\n\n\
             fn strict(xs: unique List(Int)) -> Int:\n    list.length(xs)\n\n\
             fn erase(value: Holder) -> dyn Carrier(fn(unique List(Int)) -> Int):\n\
             \x20   value as dyn Carrier(fn(unique List(Int)) -> Int)\n\n\
             fn main():\n\
             \x20   let carrier = erase(Holder(f: strict))\n\
             \x20   let f = carrier.get()\n\
             \x20   f([1])\n",
        )
        .expect("an existential pack publishes its authenticated target access identity");
    }

    #[test]
    fn absent_and_repeated_container_values_do_not_invent_erasure() {
        check_str(
            "fn id(x: Int) -> Int:\n    x\n\n\
             fn main():\n\
             \x20   let maybe: Option(fn(Int) -> Int) = None\n\
             \x20   let callbacks: List(fn(Int) -> Int) = [id, id]\n\
             \x20   return\n",
        )
        .expect("an absent option and repeated homogeneous elements preserve static access identity");
    }

    #[test]
    fn nominal_pattern_recovers_the_declared_callable_contract() {
        check_str(
            "type Holder:\n    Holder(fn(Int) -> Int)\n\n\
             fn id(x: Int) -> Int:\n    x\n\n\
             fn unwrap(holder: Holder) -> fn(Int) -> Int:\n\
             \x20   match holder:\n        Holder(f) -> f\n\n\
             fn main():\n    let f = unwrap(Holder(id))\n    return\n",
        )
        .expect("pattern fields use the nominal declaration rather than constructor value shape");
    }

    #[test]
    fn dynamic_trait_call_uses_the_authenticated_callable_parameter_contract() {
        let err = check_str(
            "mode opt\n\n\
             trait Invoke:\n\
             \x20   fn apply(let self, f: fn(unique List(Int)) -> Int) -> Int\n\n\
             type Runner:\n    Runner\n\n\
             impl Invoke for Runner:\n\
             \x20   fn apply(let self, f: fn(unique List(Int)) -> Int) -> Int:\n\
             \x20       f([1])\n\n\
             fn plain(xs: List(Int)) -> Int:\n    list.length(xs)\n\n\
             fn main():\n\
             \x20   let runner: dyn Invoke = Runner\n\
             \x20   runner.apply(plain)\n",
        )
        .expect_err("the witness slot may not erase a callable argument's access contract");
        assert!(err.contains("dynamic method `apply`") && err.contains("Qualifier"), "{err}");
    }

    #[test]
    fn callable_access_contract_survives_list_iteration() {
        check_str(
            "mode opt\n\n\
             fn strict(xs: unique List(Int)) -> Int:\n    list.length(xs)\n\n\
             fn use(f: fn(unique List(Int)) -> Int) -> Nil:\n    return\n\n\
             fn main():\n\
             \x20   let callbacks: List(fn(unique List(Int)) -> Int) = [strict]\n\
             \x20   for f in callbacks:\n        use(f)\n",
        )
        .expect("a list iterator binds the callable access identity of its element");
    }

    #[test]
    fn callable_access_contract_survives_option_patterns() {
        let prefix = "fn id(x: Int) -> Int:\n    x\n\n\
                      fn use(f: fn(Int) -> Int) -> Nil:\n    return\n\n";
        check_str(&format!(
            "{prefix}fn main():\n\
             \x20   let callback: Option(fn(Int) -> Int) = Some(id)\n\
             \x20   match callback:\n\
             \x20       Some(f) -> use(f)\n\
             \x20       None -> return\n"
        ))
        .expect("a standard Option match binds its callable payload identity");
        check_str(&format!(
            "{prefix}fn main():\n\
             \x20   let callback: Option(fn(Int) -> Int) = Some(id)\n\
             \x20   while let Some(f) = callback:\n        use(f)\n"
        ))
        .expect("while let binds its callable payload identity");
    }

    #[test]
    fn inferred_lambda_result_preserves_callable_access_identity() {
        let prefix = "mode opt\n\n\
                      fn strict(xs: unique List(Int)) -> Int:\n    list.length(xs)\n\n\
                      fn use(f: fn(unique List(Int)) -> Int) -> Nil:\n    return\n\n";
        check_str(&format!(
            "{prefix}fn main():\n\
             \x20   let make = fn(): strict\n\
             \x20   use(make())\n"
        ))
        .expect("an inferred expression result retains its callable access identity");
        check_str(&format!(
            "{prefix}fn main():\n\
             \x20   let make = fn():\n        return strict\n\
             \x20   use(make())\n"
        ))
        .expect("an inferred explicit return retains its callable access identity");
    }

    #[test]
    fn borrowed_function_types_require_opt_mode_and_bound_outputs() {
        let no_opt = "fn apply(f: fn(View(String, 'a)) -> View(String, 'a), s: String) -> String:\n    f(s)\n";
        let err = check_str(no_opt).expect_err("borrowed function type requires mode opt");
        assert!(err.contains("mode opt"), "{err}");

        let unbound = "mode opt\n\nfn bad(f: fn(String) -> View(String, 'a)) -> String:\n    \"x\"\n";
        let err = check_str(unbound).expect_err("nested output lifetime must be bound");
        assert!(err.contains("function parameter borrows with that lifetime"), "{err}");
    }

    #[test]
    fn function_returned_callable_preserves_its_borrow_relation() {
        let src = "mode opt\n\nfn borrow(xs: let('a) List(Int)) -> View(List(Int), 'a):\n    xs\n\nfn make() -> fn(View(List(Int), 'a)) -> View(List(Int), 'a):\n    borrow\n\nfn main(console: Console):\n    var xs = [1]\n    let f = make()\n    let w = f(xs)\n    xs = [2]\n    let n = list.length(w)\n    console.print(\"done\")\n";
        let err = check_str(src).expect_err("a returned callable must retain its loan relation");
        assert!(err.contains("reassigned"), "{err}");
    }

    #[test]
    fn lambda_callable_preserves_own_convention() {
        let src = "mode opt\n\nfn borrow(xs: let('a) List(Int)) -> View(List(Int), 'a):\n    xs\n\nfn main(console: Console):\n    let xs = [1]\n    let w = borrow(xs)\n    let consume = fn(own ys: List(Int)) -> Int: list.length(ys)\n    let n = consume(xs)\n    let m = list.length(w)\n    console.print(\"done\")\n";
        let err = check_str(src).expect_err("an indirect own call may not consume a loaned owner");
        assert!(err.contains("own") || err.contains("moved"), "{err}");
    }

    // --- escape rejection (acceptance 7) ------------------------------------

    #[test]
    fn view_captured_by_a_closure_is_rejected() {
        let err = check_str(&opt(
            "    var s = \"hi\"\n    let w = borrow(s)\n    let f = fn(): w\n    console.print(w)\n",
        ))
        .expect_err("a view captured by a closure escapes its owner");
        assert!(err.contains("escapes through a closure"), "{err}");

        let local = "mode opt\n\nfn borrow(text: let('a) String) -> View(String, 'a):\n    text\n\nfn main(console: Console):\n    let f = fn():\n        var s = \"inside\"\n        let w = borrow(s)\n        s = \"mutated\"\n        console.print(w)\n    f()\n";
        let err = check_str(local).expect_err("lambda-local loans must be checked");
        assert!(err.contains("lambda 1 in main"), "{err}");
        assert!(err.contains("owner `s` is reassigned"), "{err}");
    }

    #[test]
    fn view_sent_through_a_channel_is_rejected() {
        // A `send(ch, w)` call moves the view out of this activation.
        let err = check_str(&opt(
            "    var s = \"hi\"\n    let w = borrow(s)\n    let _ = send(s, w)\n    console.print(w)\n",
        ))
        .expect_err("a view sent through a channel escapes its owner");
        assert!(err.contains("escapes through a task or channel"), "{err}");
    }

    #[test]
    fn view_stored_in_owned_aggregate_or_mutable_binding_is_rejected() {
        let aggregate = check_str(&opt(
            "    var s = \"hi\"\n    let w = borrow(s)\n    let boxed = [w]\n    console.print(w)\n",
        ))
        .expect_err("an owned list may not retain a borrowed view");
        assert!(aggregate.contains("owned aggregate"), "{aggregate}");

        let direct_aggregate = check_str(&opt(
            "    var s = \"hi\"\n    let boxed = [borrow(s)]\n    console.print(\"done\")\n",
        ))
        .expect_err("an aggregate may not hide a direct view result");
        assert!(direct_aggregate.contains("stored in an owned aggregate"), "{direct_aggregate}");

        let mutable = check_str(&opt(
            "    var s = \"hi\"\n    let w = borrow(s)\n    var slot = \"\"\n    slot = w\n    console.print(w)\n",
        ))
        .expect_err("a mutable binding may not retain a borrowed view");
        assert!(mutable.contains("mutable binding"), "{mutable}");

        let direct = check_str(&opt(
            "    var s = \"hi\"\n    var slot = borrow(s)\n    console.print(slot)\n",
        ))
        .expect_err("a mutable binding may not receive a direct view result");
        assert!(direct.contains("mutable binding `slot`"), "{direct}");

        let destructured = check_str(&opt(
            "    var s = \"hi\"\n    let (w, n) = (borrow(s), 0)\n    s = \"changed\"\n    console.print(w)\n",
        ))
        .expect_err("destructuring transfers the borrowed tuple slot's owner loan");
        assert!(destructured.contains("reassigned"), "{destructured}");

        let nested = "mode opt\n\nfn borrow(text: let('a) String) -> View(String, 'a):\n    text\n\nfn keep(xs: List(String)) -> Int:\n    list.length(xs)\n\nfn main(console: Console):\n    let text = \"hi\"\n    let n = keep([borrow(text)])\n    console.print(\"done\")\n";
        let err = check_str(nested).expect_err("a call argument may not hide a view in an aggregate");
        assert!(err.contains("owned aggregate"), "{err}");
    }

    #[test]
    fn returned_view_provenance_must_match_the_function_signature() {
        let bad = "mode opt\n\nfn borrow(text: let('a) String) -> View(String, 'a):\n    text\n\nfn hide(text: let('a) String) -> String:\n    let w = borrow(text)\n    w\n";
        let err = check_str(bad).expect_err("an owned return may not hide a borrowed alias");
        assert!(err.contains("does not return a view tied to that input"), "{err}");

        let good = "mode opt\n\nfn borrow(text: let('a) String) -> View(String, 'a):\n    text\n\nfn forward(text: let('a) String) -> View(String, 'a):\n    let w = borrow(text)\n    w\n";
        check_str(good).expect("a declared matching relation may forward a local view alias");

        let nested = "mode opt\n\nfn borrow(text: let('a) String) -> View(String, 'a):\n    text\n\nfn hide(text: let('a) String) -> String:\n    if true:\n        let w = borrow(text)\n        w\n    else:\n        text\n";
        let err = check_str(nested)
            .expect_err("a nested block may not hide borrowed return provenance");
        assert!(err.contains("does not return a view tied to that input"), "{err}");

        let wrong_input = "mode opt\n\nfn wrong(a: let('a) String, b: let('b) String) -> View(String, 'a):\n    b\n";
        let err = check_str(wrong_input)
            .expect_err("a returned view must derive from the declared input lifetime");
        assert!(err.contains("owner `b`"), "{err}");

        let projection = "mode opt\n\ntype Holder:\n    text: List(Int)\n\nfn wrong(a: let('a) Holder, b: let('b) Holder) -> View(List(Int), 'a):\n    b.text\n";
        let err = check_str(projection)
            .expect_err("a returned projection must retain its root owner");
        assert!(err.contains("owner `b`"), "{err}");
    }

    #[test]
    fn owned_materialization_may_be_assigned() {
        let src = "mode opt\n\nfn borrow(text: let('a) String) -> View(String, 'a):\n    text\n\nfn owned(text: String) -> String:\n    text\n\nfn main(console: Console):\n    let text = \"hi\"\n    let w = borrow(text)\n    var slot = \"\"\n    slot = owned(w)\n    console.print(slot)\n";
        check_str(src).expect("an owned result may be stored in a mutable binding");
    }

    // --- regression guards for adversarial-review holes ---------------------

    #[test]
    fn owner_conflict_inside_a_nested_block_is_caught() {
        // A reassignment nested in an `if` body must be caught against the loan
        // inherited from the enclosing block (not dropped by the inner block's
        // own last-use window).
        let err = check_str(&opt(
            "    var s = \"orig\"\n    let w = borrow(s)\n    if true:\n        s = \"mutated\"\n    console.print(w)\n",
        ))
        .expect_err("a reassignment nested in an `if` is still a conflict");
        assert!(err.contains("reassigned"), "{err}");

        let edge = check_str(&opt(
            "    var s = \"orig\"\n    while true:\n        let w = borrow(s)\n        if true:\n            break\n        console.print(w)\n",
        ))
        .expect_err("a loop edge may not bypass a live view's root cleanup");
        assert!(edge.contains("`break` would leave the borrowed view `w`"), "{edge}");
    }

    #[test]
    fn view_bound_through_an_if_expression_still_loans() {
        // The view-producing RHS is an `if` whose branch tails return views; the
        // loan must open from the branch result, not only from a bare call RHS.
        let err = check_str(&opt(
            "    var s = \"hi\"\n    let w = if true:\n        borrow(s)\n    else:\n        borrow(s)\n    s = \"x\"\n    console.print(w)\n",
        ))
        .expect_err("a view bound through an `if` still loans its owner");
        assert!(err.contains("reassigned"), "{err}");
    }

    #[test]
    fn a_plain_read_named_owned_does_not_end_the_loan() {
        // The loan does NOT end just because a callee happens to be named `owned`
        // and merely READS the view (returning an owned Int, not the owner). The
        // view is still live afterward, so mutating the owner is a conflict —
        // materialization is recognized by RESULT TYPE (an owned result opens no
        // loan and is the view's last use), never by callee name.
        let src = "mode opt\n\n\
             fn borrow(text: let('a) String) -> View(String, 'a):\n    text\n\n\
             fn owned(x: String) -> Int:\n    99\n\n\
             fn main(console: Console):\n    var s = \"orig\"\n    let w = borrow(s)\n    let n = owned(w)\n    s = \"mut\"\n    console.print(w)\n    console.print(\"${n}\")\n";
        let err = check_str(src).expect_err("a same-named read is not a materialization");
        assert!(err.contains("reassigned"), "{err}");
    }

    #[test]
    fn materialization_does_not_open_a_loan() {
        // `owned(borrow(s))` — the OUTER call returns an owned value, so the RHS is
        // not a view and no loan opens; mutating the owner right after is fine.
        // (`owned` here returns its argument, standing in for the std `w.owned()`.)
        let src = "mode opt\n\n\
             fn borrow(text: let('a) String) -> View(String, 'a):\n    text\n\n\
             fn owned(x: String) -> String:\n    x\n\n\
             fn main(console: Console):\n    var s = \"orig\"\n    let keep = owned(borrow(s))\n    s = \"mut\"\n    console.print(keep)\n    console.print(s)\n";
        check_str(src).expect("materializing at bind time opens no loan");

        let temporary = "mode opt\n\nfn borrow(text: let('a) String) -> View(String, 'a):\n    text\n\nfn main(console: Console):\n    let w = borrow(\"temporary\")\n    console.print(w)\n";
        let err = check_str(temporary).expect_err("a persistent view needs a stable owner");
        assert!(err.contains("borrowed view of a temporary value"), "{err}");

        let materialized = "mode opt\n\nfn borrow(text: let('a) String) -> View(String, 'a):\n    text\n\nfn owned(text: String) -> String:\n    text\n\nfn main(console: Console):\n    let keep = owned(borrow(\"temporary\"))\n    console.print(keep)\n";
        check_str(materialized).expect("a transient temporary view may be materialized immediately");
    }

    // --- non-view code is unaffected ----------------------------------------

    #[test]
    fn ordinary_owned_code_is_unaffected() {
        // No views anywhere: the loan checker is a no-op and normal mutation and
        // move are fine.
        check_str(
            "fn take(own x: String) -> String:\n    x\n\n\
             fn main(console: Console):\n    var s = \"hi\"\n    s = \"there\"\n    let t = take(move s)\n    console.print(t)\n",
        )
        .expect("code without views is unaffected by the loan checker");
    }

    #[test]
    fn lowering_facts_open_and_close_on_exact_statements() {
        let module = witchy_syntax::parser::parse_module(
            "mode opt\n\n\
             fn borrow(text: let('a) String) -> View(String, 'a):\n    text\n\n\
             fn main(console: Console):\n    let s = \"hi\"\n    let w = borrow(s)\n    console.print(w)\n    0\n",
        )
        .expect("parse");
        let loan_facts = facts(&module).expect("loan facts");
        let main = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(f) if f.name == "main" => Some(f),
                _ => None,
            })
            .expect("main");
        let open = &main.body.stmts[1];
        let last_use = &main.body.stmts[2];
        let after = &main.body.stmts[3];

        assert_eq!(loan_facts.opens_after(open)[0].owner, "s");
        assert_eq!(loan_facts.opens_after(open)[0].view, "w");
        assert_eq!(loan_facts.active_at(last_use)[0].owner, "s");
        assert_eq!(loan_facts.closes_after(last_use)[0].view, "w");
        assert!(loan_facts.active_at(after).is_empty());
        let cloned = open.clone();
        assert!(loan_facts.event_key(open).is_some());
        assert!(loan_facts.event_key(&cloned).is_none(), "a cloned statement has no facts");
    }

    #[test]
    fn persisted_projection_keeps_the_original_root_and_fixed_path() {
        let module = witchy_syntax::parser::parse_module(
            "mode opt\n\n\
             type Pair:\n    left: String\n    right: String\n\n\
             fn borrow(pair: let('a) Pair) -> View(Pair, 'a):\n    pair\n\n\
             fn main(console: Console):\n    var pair = Pair(\"left\", \"right\")\n    let whole = borrow(pair)\n    let left = whole.left\n    console.print(left)\n",
        )
        .expect("parse");
        let loan_facts = facts(&module).expect("a fixed projection may be persisted");
        let main = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == "main" => Some(function),
                _ => None,
            })
            .expect("main");

        let [event] = loan_facts.opens_after(&main.body.stmts[2]) else {
            panic!("the projected binding opens exactly one loan")
        };
        assert_eq!(event.owner, "pair");
        assert_eq!(
            event.projection,
            LoanProjection { steps: vec![LoanProjectionStep::Field("left".into())] }
        );
    }

    #[test]
    fn projection_conflicts_are_field_sensitive() {
        let source = |field: &str| {
            format!(
                "mode opt\n\n\
                 type Pair:\n    left: String\n    right: String\n\n\
                 fn borrow(pair: let('a) Pair) -> View(Pair, 'a):\n    pair\n\n\
                 fn main(console: Console):\n    var pair = Pair(\"left\", \"right\")\n    let whole = borrow(pair)\n    let left = whole.left\n    pair.{field} = \"changed\"\n    console.print(left)\n"
            )
        };

        check_str(&source("right")).expect("a disjoint fixed field may be reassigned");
        let error = check_str(&source("left"))
            .expect_err("the borrowed fixed field may not be reassigned while live");
        assert!(error.contains("owner `pair` is reassigned"), "{error}");
    }

    #[test]
    fn fixed_ranges_are_facts_and_dynamic_projections_do_not_persist() {
        let parse = |projection: &str| {
            witchy_syntax::parser::parse_module(&format!(
                "mode opt\n\n\
                 fn borrow(xs: let('a) List(Int)) -> View(List(Int), 'a):\n    xs\n\n\
                 fn main(console: Console):\n    let xs = [1, 2, 3]\n    let whole = borrow(xs)\n    let window = whole[{projection}]\n    console.print(\"done\")\n"
            ))
            .expect("parse")
        };

        let fixed = parse("0..2");
        let loan_facts = facts(&fixed).expect("a fixed range may be persisted");
        let main = fixed
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == "main" => Some(function),
                _ => None,
            })
            .expect("main");
        assert_eq!(
            loan_facts.opens_after(&main.body.stmts[2])[0].projection,
            LoanProjection {
                steps: vec![LoanProjectionStep::Range { lo: 0, hi: 2, inclusive: false }]
            }
        );

        let dynamic = parse("console.hash()");
        let error = match facts(&dynamic) {
            Ok(_) => panic!("a dynamic borrowed projection stays rejected"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("dynamic index cannot be persisted"), "{error}");

        let mutation = |index: i64| {
            witchy_syntax::parser::parse_module(&format!(
                "mode opt\n\n\
                 fn borrow(xs: let('a) List(Int)) -> View(List(Int), 'a):\n    xs\n\n\
                 fn main(console: Console):\n    var xs = [1, 2, 3]\n    let whole = borrow(xs)\n    let window = whole[0..2]\n    xs[{index}] = 9\n    console.print(\"${{window}}\")\n"
            ))
            .expect("parse")
        };
        facts(&mutation(2)).expect("a fixed index outside the borrowed range is disjoint");
        let error = match facts(&mutation(1)) {
            Ok(_) => panic!("an index inside the borrowed range must conflict"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("owner `xs` is reassigned"), "{error}");
    }

    #[test]
    fn fixed_tuple_owner_sets_select_the_projected_owner() {
        let module = witchy_syntax::parser::parse_module(
            "mode opt\n\n\
             fn pair(left: let('left) String, right: let('right) String) \
                 -> (View(String, 'left), View(String, 'right)):\n    (left, right)\n\n\
             fn main(console: Console):\n    var left = \"left\"\n    var right = \"right\"\n    let both = pair(left, right)\n    let first = both[0]\n    right = \"changed\"\n    console.print(first)\n",
        )
        .expect("parse");
        let loan_facts = facts(&module).expect("a disjoint tuple owner may be reassigned");
        let main = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == "main" => Some(function),
                _ => None,
            })
            .expect("main");

        let mut opened = loan_facts.opens_after(&main.body.stmts[2]).to_vec();
        opened.sort_by(|left, right| left.owner.cmp(&right.owner));
        assert_eq!(opened.len(), 2);
        assert_eq!(opened[0].owner, "left");
        assert_eq!(
            opened[0].borrower_projection,
            LoanProjection { steps: vec![LoanProjectionStep::Tuple(0)] }
        );
        assert_eq!(opened[1].owner, "right");
        assert_eq!(
            opened[1].borrower_projection,
            LoanProjection { steps: vec![LoanProjectionStep::Tuple(1)] }
        );

        let [first] = loan_facts.opens_after(&main.body.stmts[3]) else {
            panic!("projecting tuple slot zero selects exactly one owner")
        };
        assert_eq!(first.owner, "left");
        assert!(first.borrower_projection.steps.is_empty());
    }

    #[test]
    fn fixed_borrowed_nominal_owner_sets_propagate_through_calls_and_fields() {
        let module = witchy_syntax::parser::parse_module(
            "mode opt\n\n\
             type PairView('left, 'right):\n    first: View(String, 'left)\n    second: View(String, 'right)\n\n\
             fn pair(left: let('left) String, right: let('right) String) \
                 -> PairView('left, 'right):\n    PairView(left, right)\n\n\
             fn main(console: Console):\n    var left = \"left\"\n    var right = \"right\"\n    let both = pair(left, right)\n    let first = both.first\n    right = \"changed\"\n    console.print(first)\n",
        )
        .expect("parse");
        let loan_facts = facts(&module).expect("fixed nominal fields preserve exact owner sets");
        let main = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == "main" => Some(function),
                _ => None,
            })
            .expect("main");

        let [first] = loan_facts.opens_after(&main.body.stmts[3]) else {
            panic!("projecting the first borrowed field selects one owner")
        };
        assert_eq!(first.owner, "left");
        assert!(first.borrower_projection.steps.is_empty());
    }
