    //! RFC-0083 phase-1 loan/lifetime checker tests. Each runs the full public
    //! checker (`check_str`) over a small `mode opt` program and asserts the loan
    //! rule accepts or rejects it with the documented diagnostic.

    use witchy_types::{
        loans::facts,
        typeck::{check, check_reference_migration_str as check_str},
    };


    fn no_comptime(
        _name: &str,
        _module: &mut witchy_syntax::ast::Module,
        _siblings: &[(String, witchy_syntax::ast::Module)],
    ) -> Result<witchy_syntax::origin::OriginTable, String> {
        Ok(witchy_syntax::origin::OriginTable::default())
    }

    fn linked_normal(main_body: &str) -> Result<(), witchy_types::typeck::TypeError> {
        let api = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\npub fn view(xs: &'a List(Int)) -> &'a List(Int):\n    xs\n",
        )
        .expect("parse opt API");
        let main = witchy_syntax::parser::parse_reference_migration(&format!(
            "import api\nimport list\n\nfn consume(own xs: List(Int)) -> Int:\n    list.length(xs)\n\nfn clear(var xs: List(Int)):\n    xs = []\n\nfn main(console: Console):\n{main_body}"
        ))
        .expect("parse normal caller");
        let linked = witchy_syntax::linker::link(
            vec![("main".into(), main), ("api".into(), api)],
            "main",
            witchy_syntax::linker::ComptimeExpander::new(no_comptime),
        )
        .map_err(|error| witchy_types::typeck::TypeError { message: error.to_string() })?;
        check(&linked)
    }

    fn linked_main(source: &str) -> Result<(), witchy_types::typeck::TypeError> {
        let main = witchy_syntax::parser::parse_reference_migration(source).expect("parse linked main");
        let linked = witchy_syntax::linker::link(vec![("main".into(), main)], "main", witchy_syntax::linker::ComptimeExpander::new(no_comptime))
            .expect("link main with bundled std modules");
        check(&linked)
    }

    fn assert_nonrooted_opt_boundary(error: &str, api: &str) {
        assert!(
            error.contains(&format!("non-rooted-reference opt API `{api}`")),
            "{error}"
        );
        assert!(
            error.contains("only immutable rooted-view APIs cross this boundary"),
            "{error}"
        );
    }

    fn service_source(params: &str, callback: &str, setup: &str) -> String {
        format!("import vm\nfn handler({params}) -> Bytes:\n    request\nfn run(init: Bytes, requests: List(Bytes)) -> List(Bytes):\n{setup}    vm.serve(init, requests, {callback})\nfn main() -> Int:\n    0\n")
    }

    #[test]
    fn vm_service_copy_callback_rejects_writeback_borrow_and_local_values() {
        for params in ["var state: Bytes, request: Bytes", "let state: Bytes, request: Bytes"] {
            let error = linked_main(&service_source(params, "handler", "")).expect_err("not owned copied inputs");
            assert!(error.message.contains("default or own Bytes"), "{error:?}");
        }
        for (callback, setup) in [
            ("local", "    let local = handler\n"),
            ("handler", "    let handler = fn(state: Bytes, request: Bytes): request\n"),
            ("fn(state: Bytes, request: Bytes): request", ""),
        ] {
            let error = linked_main(&service_source("state: Bytes, request: Bytes", callback, setup)).expect_err("must remain bare declaration");
            assert!(error.message.contains("bare top-level function callback"), "{error:?}");
        }
        let source = "fn handler(own state: Bytes, own request: Bytes) -> Bytes:\n    request\nfn serve(init: Bytes, requests: List(Bytes), handler: fn(Bytes, Bytes) -> Bytes) -> List(Bytes):\n    requests\nfn run(init: Bytes, requests: List(Bytes)) -> List(Bytes):\n    serve(init, requests, handler)\n";
        check_str(source).expect_err("an ordinary same-short-name function gets no service relation");
    }

    // --- signature relations (acceptance 1) ---------------------------------

    #[test]
    fn frozen_values_allow_replacement_but_reject_deep_mutation() {
        let err = check_str(
            "mode opt\n\nfn main():\n    var values: frozen List(Int) = [1]\n    values[0] = 2\n",
        )
        .expect_err("a frozen list cannot be mutated through an index assignment");
        assert!(err.contains("cannot mutate through frozen value `values`"), "{err}");

        let err = check_str(
            "mode opt\n\ntype State:\n    count: Int\n\nfn reset(var state: frozen State) -> ():\n    state.count = 0\n",
        )
        .expect_err("a frozen record parameter cannot be mutated through a field assignment");
        assert!(err.contains("cannot mutate through frozen value `state`"), "{err}");
    }

    #[test]
    fn exclusive_reference_arguments_must_be_disjoint_at_one_call_boundary() {
        let err = check_str(
            "mode opt\n\nfn edit_pair(left: &'a mut String, right: &'a mut String) -> ():\n    ()\n\nfn main():\n    var text = \"hello\"\n    edit_pair(&mut text, &mut text)\n",
        )
        .expect_err("one call cannot receive two aliases to the same exclusive place");
        assert!(
            err.contains("cannot create exclusive reference") || err.contains("overlapping"),
            "{err}"
        );

        check_str(
            "mode opt\n\ntype Pair:\n    left: String\n    right: String\n\nfn edit_pair(left: &'a mut String, right: &'a mut String) -> ():\n    ()\n\nfn main():\n    var pair = Pair(\"left\", \"right\")\n    edit_pair(&mut pair.left, &mut pair.right)\n",
        )
        .expect("disjoint field projections may satisfy separate exclusive parameters");
    }

    #[test]
    fn normal_callers_reject_nonrooted_opt_results_at_the_rooted_view_boundary() {
        let error = linked_normal(
            "    var xs = [1]\n    let w = api.view(xs)\n    console.print(\"${list.length(w)}\")\n",
        )
        .expect_err("normal callers cannot import a legacy reference export as a rooted view");
        assert_nonrooted_opt_boundary(&error.to_string(), "api.view");
    }

    // --- non-lexical last use + .owned() (acceptance 3) ---------------------

    // --- wrappers + multiple shared views (acceptance 4) --------------------

    #[test]
    fn function_type_may_not_erase_a_borrow_relation() {
        let src = "mode opt\n\n\
             fn borrow(text: &'a String) -> &'a String:\n    text\n\n\
             fn main(console: Console):\n    let f: fn(String) -> String = borrow\n    console.print(f(\"hi\"))\n";
        let err = check_str(src).expect_err("an ascription may not erase a borrow relation");
        assert!(err.contains("erases or changes its borrow/convention relation"), "{err}");

        let cast = "mode opt\n\nfn borrow(text: &'a String) -> &'a String:\n    text\n\nfn main(console: Console):\n    let f = borrow as fn(String) -> String\n    console.print(f(\"hi\"))\n";
        let err = check_str(cast).expect_err("a cast may not erase a borrow relation");
        assert!(err.contains("cannot ascribe") || err.contains("function cast erases"), "{err}");

        let argument = "mode opt\n\nfn borrow(text: &'a String) -> &'a String:\n    text\n\nfn use(f: fn(String) -> String) -> String:\n    f(\"x\")\n\nfn main(console: Console):\n    console.print(use(borrow))\n";
        let err = check_str(argument)
            .expect_err("a higher-order argument may not erase a borrow relation");
        assert!(err.contains("argument 1 passed to `use` erases"), "{err}");

        let returned = "mode opt\n\nfn borrow(text: &'a String) -> &'a String:\n    text\n\nfn erase() -> fn(String) -> String:\n    borrow\n";
        let err = check_str(returned)
            .expect_err("a returned function value may not erase a borrow relation");
        assert!(err.contains("returned function value erases"), "{err}");
    }

    // --- escape rejection (acceptance 7) ------------------------------------

    #[test]
    fn shared_reference_cannot_cross_async_suspension() {
        let main = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\nimport task\n\nasync fn bad(input: &'a String) -> task.Task(String):\n    let _ = task.done(0).await\n    *input\n\nfn main(console: Console):\n    console.print(\"done\")\n",
        )
        .expect("parse async shared-reference fixture");
        let err = witchy_syntax::linker::link(
            vec![("main".into(), main)],
            "main",
            witchy_syntax::linker::ComptimeExpander::new(no_comptime),
        )
        .expect_err("a shared reference cannot cross an async function boundary");
        assert!(
            err.to_string().contains("async fn `bad` may not expose a retired borrow"),
            "{err}"
        );
    }

    #[test]
    fn shared_reference_cannot_escape_through_a_closure() {
        let err = linked_main(
            "mode opt\n\nfn bad(input: &'a String) -> fn() -> String:\n    fn():\n        *input\n\nfn main(console: Console):\n    let text = \"value\"\n    let action = bad(&text)\n    console.print(action())\n",
        )
        .expect_err("an escaping closure cannot retain a shared reference");
        // This retired explicit lifetime never constructs a rooted view. The
        // read fails before a closure-capture storage proof is relevant.
        assert!(
            err.message.contains("immutable read dereference requires a rooted view"),
            "{err}"
        );

        let err = linked_main(
            "mode opt\n\nfn main(console: Console):\n    let make = fn(input: &'a String) -> fn() -> String:\n        fn():\n            *input\n    let text = \"value\"\n    let action = make(&text)\n    console.print(action())\n",
        )
        .expect_err("an escaping nested closure cannot retain a shared reference parameter");
        // The nested retired reference also fails at the immutable read,
        // before a closure-capture storage proof is relevant.
        assert!(
            err.message.contains("immutable read dereference requires a rooted view"),
            "{err}"
        );
    }

    #[test]
    fn returned_view_provenance_must_match_the_function_signature() {
        let bad = "mode opt\n\nfn borrow(text: &'a String) -> &'a String:\n    text\n\nfn hide(text: &'a String) -> String:\n    let w = borrow(text)\n    w\n";
        let err = check_str(bad).expect_err("an owned return may not hide a borrowed alias");
        assert!(err.contains("does not return a view tied to that input"), "{err}");

        let good = "mode opt\n\nfn borrow(text: &'a String) -> &'a String:\n    text\n\nfn forward(text: &'a String) -> &'a String:\n    let w = borrow(text)\n    w\n";
        check_str(good).expect("a declared matching relation may forward a local view alias");

        let nested = "mode opt\n\nfn borrow(text: &'a String) -> &'a String:\n    text\n\nfn hide(text: &'a String) -> String:\n    if true:\n        let w = borrow(text)\n        w\n    else:\n        text\n";
        let err = check_str(nested)
            .expect_err("a nested block may not hide borrowed return provenance");
        assert!(err.contains("does not return a view tied to that input"), "{err}");

        let wrong_input = "mode opt\n\nfn wrong(a: &'a String, b: &'b String) -> &'a String:\n    b\n";
        let err = check_str(wrong_input)
            .expect_err("a returned view must derive from the declared input lifetime");
        assert!(err.contains("owner `b`"), "{err}");

        let projection = "mode opt\n\ntype Holder:\n    text: List(Int)\n\nfn wrong(a: &'a Holder, b: &'b Holder) -> &'a List(Int):\n    b.text\n";
        let err = check_str(projection)
            .expect_err("a returned projection must retain its root owner");
        assert!(err.contains("owner `b`"), "{err}");
    }

    #[test]
    fn fixed_borrowed_nominal_return_cannot_relabel_an_owner_relation() {
        let module = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\n\
             type Holder('a):\n    view: &'a String\n\n\
             fn bad(left: &'left String, right: &'right String) -> Holder('right):\n    Holder(left)\n",
        )
        .expect("parse relation mismatch fixture");
        let error = match facts(&module) {
            Ok(_) => panic!("the checked owner relation must reject relabeling"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("output projection `.view`")
                && error.contains("owner `left`")
                && error.contains("owner `right`"),
            "{error}"
        );
    }
