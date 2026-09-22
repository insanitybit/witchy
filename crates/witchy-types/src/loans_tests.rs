    //! RFC-0083 phase-1 loan/lifetime checker tests. Each runs the full public
    //! checker (`check_str`) over a small `mode opt` program and asserts the loan
    //! rule accepts or rejects it with the documented diagnostic.

    use crate::{
        access::AccessSignature,
        loans::facts,
        typeck::{check, check_reference_migration_str as check_str},
    };
    use witchy_syntax::ast::{Convention, Expr, Item, Stmt, Type};

    use super::{
        BorrowKind, BorrowRelationCatalog, BorrowSource, LoanEdgeKind, LoanProjection,
        LoanProjectionStep,
        authenticated_borrow_escape_boundary, authenticated_generic_materializer,
        projections_overlap,
    };

    fn no_comptime(
        _name: &str,
        _module: &mut witchy_syntax::ast::Module,
        _siblings: &[(String, witchy_syntax::ast::Module)],
    ) -> Result<witchy_syntax::origin::OriginTable, String> {
        Ok(witchy_syntax::origin::OriginTable::default())
    }

    fn linked_normal(main_body: &str) -> Result<(), crate::typeck::TypeError> {
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
            no_comptime,
        )
        .map_err(|error| crate::typeck::TypeError { message: error.to_string() })?;
        check(&linked)
    }

    fn linked_main(source: &str) -> Result<(), crate::typeck::TypeError> {
        let main = witchy_syntax::parser::parse_reference_migration(source).expect("parse linked main");
        let linked = witchy_syntax::linker::link(vec![("main".into(), main)], "main", no_comptime)
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
    fn vm_service_copy_callback_preserves_default_and_own_relations() {
        for (left, right) in [("", ""), ("own ", ""), ("", "own "), ("own ", "own ")] {
            let source = service_source(&format!("{left}state: Bytes, {right}request: Bytes"), "handler", "");
            let main = witchy_syntax::parser::parse_reference_migration(&source).unwrap();
            let linked = witchy_syntax::linker::link(vec![("main".into(), main)], "main", no_comptime).unwrap();
            let typed = crate::typeck::annotate_checked_source(linked).expect("service invocation type relation");
            super::facts_with_types(typed.module(), typed.table()).expect("service copied-byte loan relation");
            let handler = typed.module().items.iter().find_map(|item| match item {
                Item::Function(function) if matches!(function.name.as_str(), "handler" | "main.handler") => Some(function),
                _ => None,
            }).unwrap();
            let actual = AccessSignature::from_function(handler).unwrap();
            let expected = AccessSignature::from_parts(
                vec![Type::Named("Bytes".into(), vec![]); 2], Type::Named("Bytes".into(), vec![]), vec![Convention::Let; 2],
            ).unwrap();
            assert_eq!(handler.params[0].convention, if left.is_empty() { Convention::Let } else { Convention::Own });
            assert_eq!(handler.params[1].convention, if right.is_empty() { Convention::Let } else { Convention::Own });
            assert!(super::vm_service_copy_callback_matches(&expected, &actual));
            assert_eq!(actual.verify_exact(&expected).is_ok(), left.is_empty() && right.is_empty(), "ordinary callable equality stays exact");
        }
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

    #[test]
    fn vm_service_copy_callback_access_rejects_borrowed_result_and_nested_contracts() {
        let bytes = Type::Named("Bytes".into(), vec![]);
        let expected = AccessSignature::from_parts(vec![bytes.clone(); 2], bytes.clone(), vec![Convention::Let; 2]).unwrap();
        let borrowed = Type::Qualified(witchy_syntax::ast::TypeQual::Borrow("a".into()), Box::new(bytes.clone()));
        for (params, result, conventions) in [
            (vec![borrowed.clone(), bytes.clone()], borrowed, vec![Convention::Let; 2]),
            (vec![bytes.clone(); 2], bytes.clone(), vec![Convention::Var, Convention::Let]),
            (vec![bytes.clone(); 2], bytes.clone(), vec![Convention::Borrow, Convention::Let]),
            (vec![Type::Fn(vec![bytes.clone()], Box::new(bytes.clone()), vec![Convention::Let], Default::default()), bytes.clone()], bytes, vec![Convention::Let; 2]),
        ] {
            let actual = AccessSignature::from_parts(params, result, conventions).unwrap();
            assert!(!super::vm_service_copy_callback_matches(&expected, &actual));
        }
    }

    /// A borrowed-view helper plus a `main` body, as a `mode opt` module. Includes
    /// LOCAL `owned`/`send` helpers so these checker tests need no std linking
    /// (`check_str` does not resolve `import`s). A local `send` is intentionally
    /// not an authenticated channel boundary; linked std-boundary coverage lives
    /// below.
    fn opt(body: &str) -> String {
        format!(
            "mode opt\n\n\
             fn borrow(text: &'a String) -> &'a String:\n    text\n\n\
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
            "fn borrow(text: &'a String) -> &'a String:\n    text\n",
        )
        .expect_err("views outside mode opt are rejected");
        assert!(err.contains("mode opt"), "{err}");
    }

    #[test]
    fn output_view_lifetime_must_be_bound_by_an_input() {
        let err = check_str("mode opt\n\nfn bad(text: String) -> &'a String:\n    text\n")
            .expect_err("an unbound output lifetime is rejected");
        assert!(err.contains("no parameter borrows with that lifetime"), "{err}");
    }

    #[test]
    fn var_shared_reference_parameter_replaces_its_handle() {
        check_str(
            "mode opt\n\nfn replace(var text: &'a String, other: &'a String) -> &'a String:\n    text = other\n    text\n",
        )
        .expect("var replaces a shared-reference handle without granting referent writes");
    }

    #[test]
    fn well_formed_view_signature_checks() {
        check_str(
            "mode opt\n\nfn first(text: &'a String) -> &'a String:\n    text\n",
        )
        .expect("a well-formed explicit reference signature checks");
    }

    #[test]
    fn explicit_reference_type_requires_mode_opt_without_loan_follow_ons() {
        let err = check_str("fn first(text: &'a String) -> &'a String:\n    text\n")
            .expect_err("references are opt-only");
        assert!(err.contains("mode opt"), "{err}");
        assert!(!err.contains("loan"), "normal-mode error must stop at the mode boundary: {err}");
    }

    #[test]
    fn exclusive_reference_signature_retains_its_affine_contract() {
        check_str(
            "mode opt\n\nfn edit(text: &'a mut String) -> &'a mut String:\n    text\n",
        )
        .expect("exclusive reference signatures are checked, not erased");
    }

    #[test]
    fn owned_exclusive_reference_is_a_consuming_affine_parameter() {
        check_str(
            "mode opt\n\nfn take(own text: &'a mut String) -> &'a mut String:\n    text\n",
        )
        .expect("an exclusive reference may be consumed and returned");

        check_str(
            "mode opt\n\nfn take(own text: &'a String) -> &'a String:\n    text\n",
        )
        .expect("own moves the shared handle, not its referent");
    }

    #[test]
    fn owned_generic_exclusive_reference_releases_an_aggregate_owner() {
        check_str(
            "mode opt\n\ntype Pair:\n    left: Int\n    right: Int\n\nfn consume(own value: &'a mut a) -> ():\n    return\n\nfn main():\n    var pair = Pair(1, 2)\n    let handle = &mut pair\n    consume(handle)\n    pair = Pair(3, 4)\n",
        )
        .expect("an owned generic exclusive reference releases its aggregate owner at return");
    }

    #[test]
    fn frozen_and_unique_reference_qualifiers_follow_the_rfc0167_matrix() {
        check_str(
            "mode opt\n\nfn consume(own text: frozen String) -> frozen String:\n    text\n\nfn replace(var text: frozen String) -> frozen String:\n    text = \"replacement\"\n    text\n\nfn replace_handle(var text: &frozen String, other: &frozen String) -> &frozen String:\n    text = other\n    text\n\nfn consume_unique(own text: unique String) -> unique String:\n    text\n\nfn main():\n    var text: frozen String = \"before\"\n    text = \"after\"\n    var frozen = consume(text)\n    let replacement = replace(frozen)\n    let unique: unique String = \"unique\"\n    let consumed = consume_unique(unique)\n    let _ = replacement\n    let _ = consumed\n",
        )
        .expect("frozen values may move or be replaced, and var may rebind an immutable view carrier");

        check_str(
            "mode opt\n\nfn shared(text: &frozen String) -> &frozen String:\n    text\n\nfn relay(f: fn(own &unique String) -> &unique String, own text: &unique String) -> &unique String:\n    f(text)\n",
        )
        .expect("immutable view payloads retain frozen or unique contracts through callable types");

        for source in [
            "mode opt\n\nfn bad(text: unique &String) -> ():\n    ()\n",
            "mode opt\n\nfn bad(text: frozen &String) -> ():\n    ()\n",
        ] {
            let err = check_str(source).expect_err("outer ownership qualifiers cannot wrap a reference");
            assert!(err.contains("outer") && err.contains("reference qualifier"), "{err}");
        }

        for source in [
            "mode opt\n\nfn bad(text: unique frozen String) -> ():\n    ()\n",
            "mode opt\n\nfn bad(text: frozen unique String) -> ():\n    ()\n",
            "mode opt\n\ntype Holder:\n    value: unique frozen String\n",
            "mode opt\n\nfn bad(f: fn(frozen unique String) -> ()) -> ():\n    ()\n",
        ] {
            let err = check_str(source).expect_err("frozen and unique cannot describe the same value");
            assert!(err.contains("frozen") && err.contains("unique") && err.contains("same value"), "{err}");
        }

    }

    #[test]
    fn unique_reference_targets_survive_aggregate_and_callable_positions() {
        check_str(
            "mode opt\n\nfn pack(text: &'a mut unique String) -> (&'a mut unique String, Int):\n    (text, 1)\n\nfn unpack(pair: (&'a mut unique String, Int)) -> &'a mut unique String:\n    pair.0\n",
        )
        .expect("a unique reference target retains its qualifier below tuple construction and projection");

        check_str(
            "mode opt\n\nfn pass(own text: &'a mut unique String) -> &'a mut unique String:\n    text\n\nfn relay(f: fn(own &'a mut unique String) -> &'a mut unique String, own text: &'a mut unique String) -> &'a mut unique String:\n    f(text)\n",
        )
        .expect("a function value retains the consumed unique-target reference contract");

        let err = check_str(
            "mode opt\n\nfn bad(f: fn(unique &'a mut String) -> ()) -> ():\n    ()\n",
        )
        .expect_err("a function-value parameter cannot hide an outer reference qualifier");
        assert!(err.contains("outer") && err.contains("reference qualifier"), "{err}");
    }

    #[test]
    fn immutable_views_survive_list_transport() {
        check_str(
            "mode opt\n\nfn pack(left: &String, right: &String) -> List(&String):\n    [left, right]\n\nfn select(values: List(&String)) -> &String:\n    values[0]\n",
        )
        .expect("an immutable rooted view survives aggregate construction and projection");
    }

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
    fn frozen_values_cannot_enter_deep_mutating_call_contracts() {
        let err = linked_main(
            "mode opt\n\nimport list\n\nfn main():\n    var xs: frozen List(Int) = [1]\n    list.push(xs, 2)\n",
        )
        .expect_err("list.push mutates a frozen list rather than replacing it");
        assert!(err.message.contains("frozen value `xs`") && err.message.contains("list.push"), "{}", err.message);

        let err = linked_main(
            "mode opt\n\nimport set\n\nfn main():\n    var values: frozen Set(Int) = set.from_list([1])\n    values.insert(2)\n",
        )
        .expect_err("a non-list mutator must use the same var contract");
        assert!(err.message.contains("frozen value `values`") && err.message.contains("non-frozen `var`"), "{}", err.message);

        let err = check_str(
            "mode opt\n\nfn mutate(var xs: List(Int)) -> ():\n    xs = [2]\n\nfn main():\n    var xs: frozen List(Int) = [1]\n    mutate(xs)\n",
        )
        .expect_err("a non-frozen var parameter may deeply mutate its argument");
        assert!(err.contains("frozen value `xs`") && err.contains("non-frozen `var`"), "{err}");

        let err = check_str(
            "mode opt\n\nfn mutate(var xs: List(Int)) -> ():\n    xs = [2]\n\nfn main():\n    var xs: frozen List(Int) = [1]\n    let action = mutate\n    action(xs)\n",
        )
        .expect_err("qualifier erasure on a function value cannot bypass frozen storage");
        assert!(err.contains("frozen value `xs`") && err.contains("erased `var`"), "{err}");

        let err = check_str(
            "mode opt\n\ntype Holder:\n    xs: frozen List(Int)\n\nfn mutate(var xs: List(Int)) -> ():\n    xs = [2]\n\nfn main():\n    var holder = Holder([1])\n    mutate(holder.xs)\n",
        )
        .expect_err("a frozen aggregate field is still frozen storage");
        assert!(err.contains("frozen value `holder`") && err.contains("non-frozen `var`"), "{err}");

        check_str(
            "mode opt\n\ntype Nested:\n    values: List(Int)\n\ntype Holder:\n    xs: frozen List(Int)\n    nested: Nested\n    count: Int\n\nfn main():\n    var holder = Holder([1], Nested([2]), 0)\n    holder.count = 1\n    holder.nested.values[0] = 3\n",
        )
        .expect("a frozen field does not freeze sibling or nested ordinary fields");

        let err = check_str(
            "mode opt\n\ntype Holder:\n    xs: frozen List(Int)\n    count: Int\n\nfn main():\n    var holder = Holder([1], 0)\n    holder.xs[0] = 2\n",
        )
        .expect_err("a frozen aggregate field cannot be mutated through an index");
        assert!(err.contains("cannot mutate through frozen value `holder`"), "{err}");

        check_str(
            "mode opt\n\nfn replace(var xs: frozen List(Int)) -> frozen List(Int):\n    xs = [2]\n    xs\n\nfn main():\n    var xs: frozen List(Int) = [1]\n    let replacement = replace(xs)\n    let _ = replacement\n",
        )
        .expect("a frozen var parameter may replace its frozen value argument");
    }

    #[test]
    fn exclusive_borrow_of_disjoint_record_fields_is_accepted() {
        check_str(
            "mode opt\n\n\
             type Pair:\n    left: String\n    right: String\n\
             fn main(console: Console):\n    var pair = Pair(\"left\", \"right\")\n    let left = &mut pair.left\n    let right = &mut pair.right\n    console.print(left)\n    console.print(right)\n",
        )
        .expect("fixed disjoint record fields are independently borrowable");
    }

    #[test]
    fn exclusive_parameter_requires_an_explicit_exclusive_reference() {
        let err = check_str(
            "mode opt\n\nfn clear(text: &'a mut String) -> ():\n    ()\n\nfn main():\n    var text = \"hello\"\n    clear(text)\n",
        )
        .expect_err("ordinary values must not satisfy an exclusive-reference parameter");
        assert!(err.contains("exclusive reference (`&mut place`)"), "{err}");
    }

    #[test]
    fn exclusive_parameter_accepts_an_explicit_exclusive_reference() {
        check_str(
            "mode opt\n\nfn clear(text: &'a mut String) -> ():\n    ()\n\nfn main():\n    var text = \"hello\"\n    clear(&mut text)\n",
        )
        .expect("the call preserves the exclusive-reference contract");
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
    fn exclusive_function_values_retain_disjoint_place_requirements() {
        let err = check_str(
            "mode opt\n\nfn edit_pair(left: &'a mut String, right: &'a mut String) -> ():\n    ()\n\nfn main():\n    let edit = edit_pair\n    var text = \"hello\"\n    edit(&mut text, &mut text)\n",
        )
        .expect_err("a function value cannot erase an exclusive-place relation");
        assert!(
            err.contains("cannot create exclusive reference") || err.contains("overlapping"),
            "{err}"
        );
    }

    #[test]
    fn exclusive_reference_bindings_move_instead_of_copying() {
        check_str(
            "mode opt\n\nfn main(console: Console):\n    var text = \"hello\"\n    let editable = &mut text\n    let moved = editable\n    console.print(moved)\n",
        )
        .expect("an exclusive reference binding can move to a new local");

        let err = check_str(
            "mode opt\n\nfn main(console: Console):\n    var text = \"hello\"\n    let editable = &mut text\n    let moved = editable\n    console.print(editable)\n    console.print(moved)\n",
        )
        .expect_err("an exclusive reference cannot be used after it moves");
        assert!(err.contains("moved exclusive reference `editable`"), "{err}");

        let err = check_str(
            "mode opt\n\nfn main(console: Console):\n    var text = \"hello\"\n    let editable = &mut text\n    let pair = (editable, editable)\n",
        )
        .expect_err("one exclusive reference cannot occupy two aggregate slots");
        assert!(err.contains("exclusive reference `editable` is used more than once"), "{err}");
    }

    #[test]
    fn explicitly_discarding_a_shared_reference_closes_its_owner_loan() {
        check_str(
            "mode opt\n\nfn main():\n    var text = \"hello\"\n    let view = &text\n    let _ = view\n    text = \"changed\"\n",
        )
        .expect("an explicit discard ends a shared reference loan");
    }

    #[test]
    fn explicitly_discarding_an_exclusive_reference_releases_its_place() {
        check_str(
            "mode opt\n\nfn main():\n    var text = \"hello\"\n    let view = &mut text\n    let _ = view\n    text = \"changed\"\n",
        )
        .expect("an explicit discard releases an exclusive reference place");
    }

    #[test]
    fn explicitly_discarding_a_shared_reference_tuple_closes_each_owner_loan() {
        check_str(
            "mode opt\n\nfn main():\n    var first = \"first\"\n    var second = \"second\"\n    let views = (&first, &second)\n    let _ = views\n    first = \"updated-first\"\n    second = \"updated-second\"\n",
        )
        .expect("discarding a shared-reference tuple closes every owner loan");
    }

    #[test]
    fn explicitly_discarding_a_shared_reference_list_closes_each_owner_loan() {
        check_str(
            "mode opt\n\nfn main():\n    var first = \"first\"\n    var second = \"second\"\n    let views = [&first, &second]\n    let _ = views\n    first = \"updated-first\"\n    second = \"updated-second\"\n",
        )
        .expect("discarding a shared-reference list closes every owner loan");
    }

    #[test]
    fn explicitly_discarding_an_exclusive_reference_tuple_releases_each_place() {
        check_str(
            "mode opt\n\nfn main():\n    var first = \"first\"\n    var second = \"second\"\n    let views = (&mut first, &mut second)\n    let _ = views\n    first = \"updated-first\"\n    second = \"updated-second\"\n",
        )
        .expect("discarding an exclusive-reference tuple releases every place");
    }

    #[test]
    fn explicitly_discarding_a_nullable_exclusive_tuple_list_releases_each_place() {
        check_str(
            "mode opt\n\nfn main():\n    var first = \"first\"\n    var second = \"second\"\n    let selected = if true:\n        Some([(&mut first, &mut second)])\n    else:\n        None\n    let _ = selected\n    first = \"updated-first\"\n    second = \"updated-second\"\n",
        )
        .expect("discarding a nullable exclusive tuple list releases every place");

        let err = check_str(
            "mode opt\n\nfn main():\n    var first = \"first\"\n    var second = \"second\"\n    let selected = if true:\n        Some([(&mut first, &mut second)])\n    else:\n        None\n    first = \"updated-first\"\n    let _ = selected\n    second = \"updated-second\"\n",
        )
        .expect_err("a live nullable exclusive tuple list retains both owner loans");
        assert!(err.contains("reassigned") || err.contains("borrowed"), "{err}");
    }

    #[test]
    fn explicitly_discarding_a_result_exclusive_tuple_releases_each_place() {
        check_str(
            "mode opt\n\nfn main():\n    var first = \"first\"\n    var second = \"second\"\n    let selected = if true:\n        Ok((&mut first, &mut second))\n    else:\n        Err(\"none\")\n    let _ = selected\n    first = \"updated-first\"\n    second = \"updated-second\"\n",
        )
        .expect("discarding a Result exclusive tuple releases every place");

        let err = check_str(
            "mode opt\n\nfn main():\n    var first = \"first\"\n    var second = \"second\"\n    let selected = if true:\n        Ok((&mut first, &mut second))\n    else:\n        Err(\"none\")\n    first = \"updated-first\"\n    let _ = selected\n    second = \"updated-second\"\n",
        )
        .expect_err("a live Result exclusive tuple retains both owner places");
        assert!(err.contains("reassigned") || err.contains("borrowed"), "{err}");
    }

    #[test]
    fn mutable_shared_reference_binding_releases_the_replaced_owner_loan() {
        check_str(
            "mode opt\n\nfn main(console: Console):\n    var first = \"first\"\n    var second = \"second\"\n    var view = &first\n    view = &second\n    first = \"updated-first\"\n    console.print(*view)\n",
        )
        .expect("replacing a mutable shared-reference binding closes the old owner loan");
    }

    #[test]
    fn normal_callers_reject_nonrooted_opt_results_at_the_rooted_view_boundary() {
        let error = linked_normal(
            "    var xs = [1]\n    let w = api.view(xs)\n    console.print(\"${list.length(w)}\")\n",
        )
        .expect_err("normal callers cannot import a legacy reference export as a rooted view");
        assert_nonrooted_opt_boundary(&error.to_string(), "api.view");
    }

    #[test]
    fn normal_callers_reject_nonrooted_legacy_exports_and_alias_hidden_types() {
        let check_import = |api_source: &str, main_source: &str| {
            let api = witchy_syntax::parser::parse_reference_migration(api_source).expect("parse opt API");
            let main = witchy_syntax::parser::parse_reference_migration(main_source).expect("parse normal caller");
            let linked = witchy_syntax::linker::link(
                vec![("main".into(), main), ("api".into(), api)],
                "main",
                no_comptime,
            )
            .map_err(|error| crate::typeck::TypeError { message: error.to_string() });
            match linked {
                Err(error) => error.to_string(),
                Ok(linked) => crate::typeck::check(&linked)
                    .expect_err("normal interface must reject non-rooted legacy references")
                    .to_string(),
            }
        };

        let direct = check_import(
            "mode opt\n\npub fn view(text: &'a String) -> &'a String:\n    text\n",
            "import api\n\nfn main():\n    var text = \"hello\"\n    let view = api.view(&text)\n",
        );
        assert_nonrooted_opt_boundary(&direct, "api.view");

        let alias_hidden = check_import(
            "mode opt\n\ntype Holder('a):\n    value: &'a String\n\npub fn make(text: &'a String) -> Holder('a):\n    Holder(text)\n",
            "import api\n\nfn main():\n    var text = \"hello\"\n    let holder = api.make(&text)\n",
        );
        assert_nonrooted_opt_boundary(&alias_hidden, "api.make");
    }

    #[test]
    fn normal_callers_reject_nested_nominal_nonrooted_exports() {
        let api = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\ntype Pair('a, 'b):\n    left: &'a mut String\n    right: &'b mut String\n\npub fn make(left: &'a mut String, right: &'b mut String) -> Option(List(Pair('a, 'b))):\n    Some([Pair(left, right)])\n",
        )
        .expect("parse nested nominal opt API");
        let main = witchy_syntax::parser::parse_reference_migration(
            "import api\n\nfn main():\n    var left = \"left\"\n    var right = \"right\"\n    let value = api.make(&mut left, &mut right)\n",
        )
        .expect("parse normal nested nominal caller");
        let linked = witchy_syntax::linker::link(
            vec![("main".into(), main), ("api".into(), api)],
            "main",
            no_comptime,
        )
        .map_err(|error| crate::typeck::TypeError { message: error.to_string() });
        let error = match linked {
            Err(error) => error.to_string(),
            Ok(linked) => check(&linked)
                .expect_err("normal interface must reject nested nominal legacy references")
                .to_string(),
        };
        assert_nonrooted_opt_boundary(&error, "api.make");
    }

    #[test]
    fn normal_callers_reject_callable_fields_that_hide_nonrooted_contracts() {
        let api = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\n\
             type Holder:\n    project: fn(&'a String) -> &'a String\n\n\
             fn first(value: &'a String) -> &'a String:\n    value\n\n\
             pub fn make() -> Holder:\n    Holder(project: first)\n",
        )
        .expect("parse opt callable-field API");
        let main = witchy_syntax::parser::parse_reference_migration(
            "import api\n\nfn main():\n    let holder = api.make()\n",
        )
        .expect("parse normal callable-field caller");
        let linked = witchy_syntax::linker::link(
            vec![("main".into(), main), ("api".into(), api)],
            "main",
            no_comptime,
        )
        .map_err(|error| crate::typeck::TypeError { message: error.to_string() });
        let error = match linked {
            Err(error) => error.to_string(),
            Ok(linked) => crate::typeck::check(&linked)
                .expect_err("normal interface must reject callable legacy-reference contracts")
                .to_string(),
        };
        assert_nonrooted_opt_boundary(&error, "api.make");
    }

    // --- non-lexical last use + .owned() (acceptance 3) ---------------------

    #[test]
    fn bundled_owned_authenticates_only_a_direct_view_projection() {
        let generic = Type::Named("a".into(), Vec::new());
        let access = AccessSignature::from_parts(
            vec![generic.clone()],
            generic,
            vec![Convention::Let],
        )
        .expect("derive generic identity access");
        let direct = BorrowSource {
            owner: "owner".into(),
            root_type: None,
            projection: LoanProjection::default(),
            borrower_projection: LoanProjection::default(),
            origin: "view".into(),
            kind: BorrowKind::Shared,
            owner_type: Type::Named("String".into(), Vec::new()),
            temporary: false,
        };

        assert!(authenticated_generic_materializer(
            "borrow.Owned__a__owned",
            0,
            &access,
            std::slice::from_ref(&direct),
        ));
        assert!(authenticated_generic_materializer(
            "main.Owned__a__owned_companion",
            0,
            &access,
            std::slice::from_ref(&direct),
        ));
        assert!(!authenticated_generic_materializer(
            "main.Owned__b__owned_companion",
            0,
            &access,
            std::slice::from_ref(&direct),
        ));

        let shell = BorrowSource {
            borrower_projection: LoanProjection {
                steps: vec![LoanProjectionStep::Field("value".into())],
            },
            ..direct.clone()
        };
        assert!(!authenticated_generic_materializer(
            "borrow.Owned__a__owned",
            0,
            &access,
            &[shell.clone()],
        ));
        assert!(!authenticated_generic_materializer(
            "main.Owned__a__owned_companion",
            0,
            &access,
            &[shell.clone()],
        ));
        assert!(!authenticated_generic_materializer(
            "main.Owned__a__owned",
            0,
            &access,
            std::slice::from_ref(&direct),
        ));
        assert!(!authenticated_generic_materializer(
            "borrow.Owned__a__owned__suffix",
            0,
            &access,
            &[direct],
        ));
    }

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

    #[test]
    fn function_value_may_not_erase_an_ownership_qualifier() {
        let src = "mode opt\n\n\
             fn constrained(xs: unique List(Int)) -> Int:\n    list.length(xs)\n\n\
             fn use(f: fn(List(Int)) -> Int) -> ():\n    return\n\n\
             fn main():\n    use(constrained)\n";
        let err = check_str(src).expect_err("a higher-order argument may not erase `unique`");
        assert!(err.contains("ownership/access contract"), "{err}");
        assert!(err.contains("Qualifier"), "{err}");
    }

    #[test]
    fn inferred_function_declaration_retains_its_access_contract() {
        let src = "mode opt\n\n\
             fn constrained(xs: unique List(Int)):\n    list.length(xs)\n\n\
             fn use(f: fn(List(Int)) -> Int) -> ():\n    return\n\n\
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
                 fn use(f: fn(List(Int)) -> Int) -> ():\n    return\n\n\
                 fn main():\n{body}"
            );
            let err = check_str(&src)
                .expect_err("flow-sensitive bindings may not erase a callable qualifier");
            assert!(err.contains("ownership/access contract"), "{body}: {err}");
            assert!(err.contains("Qualifier"), "{body}: {err}");
        }
    }

    #[test]
    fn retired_local_unique_result_is_rejected_even_when_nested() {
        let err = check_str(
            "mode opt\n\nfn bad() -> (local unique List(Int), Int):\n    ([1], 0)\n",
        )
        .expect_err("local unique is retired even below a result aggregate");
        assert!(err.contains("local unique") && err.contains("retired"), "{err}");
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
             fn use(f: fn(unique List(Int)) -> Int) -> ():\n    return\n\n\
             fn main():\n\
             \x20   let callbacks: List(fn(unique List(Int)) -> Int) = [strict]\n\
             \x20   for f in callbacks:\n        use(f)\n",
        )
        .expect("a list iterator binds the callable access identity of its element");
    }

    #[test]
    fn inferred_empty_callable_list_recovers_checked_iteration_contract() {
        check_str(
            "mode opt\n\n\
             fn strict(xs: List(Int)) -> Int:\n    list.length(xs)\n\n\
             fn use(f: fn(List(Int)) -> Int) -> ():\n    return\n\n\
             fn append(var values: List(a), value: a) -> List(a):\n    values\n\n\
             fn main():\n\
             \x20   var callbacks = []\n\
             \x20   let _ = append(callbacks, strict)\n\
             \x20   for f in callbacks:\n        use(f)\n",
        )
        .expect(
            "an initially empty list uses its finalized checked element type when iteration needs callable access",
        );
    }

    #[test]
    fn callable_access_contract_survives_option_and_result_coalescing() {
        check_str(
            "mode opt\n\n\
             fn strict(xs: unique List(Int)) -> Int:\n    list.length(xs)\n\n\
             fn use(f: fn(unique List(Int)) -> Int) -> ():\n    return\n\n\
             fn main():\n\
             \x20   let absent: Option(fn(unique List(Int)) -> Int) = None\n\
             \x20   use(absent ?? strict)\n\
             \x20   let failed: Result(fn(unique List(Int)) -> Int, String) = Err(\"no\")\n\
             \x20   use(failed ?? strict)\n",
        )
        .expect("Option and Result coalescing preserve their success callable access identity");
    }

    #[test]
    fn callable_access_contract_survives_option_patterns() {
        let prefix = "fn id(x: Int) -> Int:\n    x\n\n\
                      fn use(f: fn(Int) -> Int) -> ():\n    return\n\n";
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
                      fn use(f: fn(unique List(Int)) -> Int) -> ():\n    return\n\n";
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
        let no_opt = "fn apply(f: fn(&'a String) -> &'a String, s: String) -> String:\n    f(s)\n";
        let err = check_str(no_opt).expect_err("borrowed function type requires mode opt");
        assert!(err.contains("mode opt"), "{err}");

        let unbound = "mode opt\n\nfn bad(f: fn(String) -> &'a String) -> String:\n    \"x\"\n";
        let err = check_str(unbound).expect_err("nested output lifetime must be bound");
        assert!(err.contains("function parameter borrows with that lifetime"), "{err}");
    }

    #[test]
    fn lambda_callable_preserves_own_convention() {
        let src = "mode opt\n\nfn main(console: Console):\n    let xs = [1]\n    let consume = fn(own ys: List(Int)) -> Int: list.length(ys)\n    let n = consume(xs)\n    let m = list.length(xs)\n    console.print(\"done\")\n";
        let err = check_str(src).expect_err("an indirect own call consumes its source binding");
        assert!(err.contains("own") || err.contains("moved"), "{err}");
    }

    // --- escape rejection (acceptance 7) ------------------------------------

    #[test]
    fn task_and_channel_escape_boundaries_require_canonical_identity() {
        use super::BorrowEscapeBoundary::{ChannelSend, TaskSpawn};

        assert_eq!(authenticated_borrow_escape_boundary("chan.send"), Some(ChannelSend));
        assert_eq!(
            authenticated_borrow_escape_boundary("chan.send__String"),
            Some(ChannelSend)
        );
        assert_eq!(
            authenticated_borrow_escape_boundary("task.__channel_send"),
            Some(ChannelSend)
        );
        assert_eq!(authenticated_borrow_escape_boundary("chan.spawn"), Some(TaskSpawn));
        assert_eq!(authenticated_borrow_escape_boundary("task.spawn"), Some(TaskSpawn));

        for lookalike in [
            "send",
            "spawn",
            "main.send",
            "main.spawn",
            "server.send",
            "http.send",
            "http.Request__send",
            "channel.send",
            "chan.send_later",
            "task.respawn",
            "other.__channel_send",
        ] {
            assert_eq!(
                authenticated_borrow_escape_boundary(lookalike),
                None,
                "lookalike `{lookalike}` must not become an escape boundary"
            );
        }
    }

    #[test]
    fn shared_reference_cannot_cross_async_suspension() {
        let main = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\nimport task\n\nasync fn bad(input: &'a String) -> task.Task(String):\n    let _ = task.done(0).await\n    *input\n\nfn main(console: Console):\n    console.print(\"done\")\n",
        )
        .expect("parse async shared-reference fixture");
        let err = witchy_syntax::linker::link(
            vec![("main".into(), main)],
            "main",
            no_comptime,
        )
        .expect_err("a shared reference cannot cross an async function boundary");
        assert!(
            err.to_string().contains("async fn `bad` may not expose a borrowed view"),
            "{err}"
        );
    }

    #[test]
    fn shared_reference_cannot_cross_generator_suspension() {
        let main = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\nimport iter\n\ngen fn bad(input: &'a String) -> Iter(String):\n    yield *input\n\nfn main(console: Console):\n    console.print(\"done\")\n",
        )
        .expect("parse generator shared-reference fixture");
        let err = witchy_syntax::linker::link(
            vec![("main".into(), main)],
            "main",
            no_comptime,
        )
        .expect_err("a shared reference cannot cross a generator function boundary");
        assert!(
            err.to_string().contains("gen fn `bad` may not expose a borrowed view"),
            "{err}"
        );
    }

    #[test]
    fn shared_reference_generator_rejects_borrowed_element() {
        let main = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\nimport iter\n\ngen fn bad(input: &'a String) -> Iter(String):\n    yield *input\n\nfn main(console: Console):\n    console.print(\"done\")\n",
        )
        .expect("parse generator shared-reference fixture");
        let err = witchy_syntax::linker::link(
            vec![("main".into(), main)],
            "main",
            no_comptime,
        )
        .expect_err("a shared reference cannot cross a generator function boundary");
        assert!(
            err.to_string().contains("gen fn `bad` may not expose a borrowed view"),
            "{err}"
        );

        let main = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\nimport iter\n\ngen fn bad(input: &'a String) -> Iter(&'a String):\n    yield input\n\nfn main(console: Console):\n    console.print(\"done\")\n",
        )
        .expect("parse borrowed generator-element fixture");
        let err = witchy_syntax::linker::link(
            vec![("main".into(), main)],
            "main",
            no_comptime,
        )
        .expect_err("a generator cannot yield a shared reference");
        assert!(
            err.to_string().contains("gen fn `bad` may not expose a borrowed view"),
            "{err}"
        );
    }

    #[test]
    fn exclusive_reference_cannot_cross_generator_suspension() {
        let main = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\nimport iter\n\ngen fn bad(input: &'a mut String) -> Iter(String):\n    yield *input\n\nfn main(console: Console):\n    console.print(\"done\")\n",
        )
        .expect("parse generator exclusive-reference fixture");
        let err = witchy_syntax::linker::link(
            vec![("main".into(), main)],
            "main",
            no_comptime,
        )
        .expect_err("an exclusive reference cannot cross a generator function boundary");
        assert!(
            err.to_string().contains("gen fn `bad` may not expose a borrowed view"),
            "{err}"
        );
    }

    #[test]
    fn exclusive_reference_generator_rejects_borrowed_element() {
        let main = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\nimport iter\n\ngen fn bad(input: &'a mut String) -> Iter(&'a mut String):\n    yield input\n\nfn main(console: Console):\n    console.print(\"done\")\n",
        )
        .expect("parse exclusive borrowed generator-element fixture");
        let err = witchy_syntax::linker::link(
            vec![("main".into(), main)],
            "main",
            no_comptime,
        )
        .expect_err("a generator cannot yield an exclusive reference");
        assert!(
            err.to_string().contains("gen fn `bad` may not expose a borrowed view"),
            "{err}"
        );
    }

    #[test]
    fn shared_reference_cannot_escape_through_a_closure() {
        let err = linked_main(
            "mode opt\n\nfn bad(input: &'a String) -> fn() -> String:\n    fn():\n        *input\n\nfn main(console: Console):\n    let text = \"value\"\n    let action = bad(&text)\n    console.print(action())\n",
        )
        .expect_err("an escaping closure cannot retain a shared reference");
        // The refusal reason changed with RFC-0167: capture is no longer a loan
        // escape (a view retains its root), so what is refused here is the
        // payload having no admitted carrier.
        assert!(
            err.message.contains("closure capture")
                && err.message.contains("no admitted carrier"),
            "{err}"
        );

        let err = linked_main(
            "mode opt\n\nfn main(console: Console):\n    let make = fn(input: &'a String) -> fn() -> String:\n        fn():\n            *input\n    let text = \"value\"\n    let action = make(&text)\n    console.print(action())\n",
        )
        .expect_err("an escaping nested closure cannot retain a shared reference parameter");
        // The refusal reason changed with RFC-0167: capture is no longer a loan
        // escape (a view retains its root), so what is refused here is the
        // payload having no admitted carrier.
        assert!(
            err.message.contains("closure capture")
                && err.message.contains("no admitted carrier"),
            "{err}"
        );
    }

    #[test]
    fn local_shared_reference_cannot_escape_through_a_closure() {
        let err = linked_main(
            "mode opt\n\nfn main(console: Console):\n    let text = [[1]]\n    let observed = &text\n    let action = fn() -> List(List(Int)):\n        *observed\n    console.print(show(action().length()))\n",
        )
        .expect_err("an escaping closure cannot retain a local shared reference");
        // The refusal reason changed with RFC-0167: capture is no longer a loan
        // escape (a view retains its root), so what is refused here is the
        // payload having no admitted carrier.
        assert!(
            err.message.contains("closure capture")
                && err.message.contains("no admitted carrier"),
            "{err}"
        );
    }

    #[test]
    fn copied_local_shared_reference_cannot_escape_through_a_closure() {
        let err = linked_main(
            "mode opt\n\nfn main(console: Console):\n    let text = [[1]]\n    let observed = &text\n    let copied = observed\n    let action = fn() -> List(List(Int)):\n        *copied\n    console.print(show(action().length()))\n",
        )
        .expect_err("a copied reference cannot escape through a closure");
        // The refusal reason changed with RFC-0167: capture is no longer a loan
        // escape (a view retains its root), so what is refused here is the
        // payload having no admitted carrier.
        assert!(
            err.message.contains("closure capture")
                && err.message.contains("no admitted carrier"),
            "{err}"
        );
    }

    #[test]
    fn exclusive_references_cannot_escape_through_closures() {
        let closure = check_str(
            "mode opt\n\nfn main(console: Console):\n    var text = \"hi\"\n    let editable = &mut text\n    let later = fn(): editable\n    console.print(text)\n",
        )
        .expect_err("an exclusive reference captured by a closure escapes its owner");
        // Refused for the carrier, not for a loan escape: RFC-0167 retired both
        // `&mut` and the lexical-loan reading of capture.
        assert!(
            closure.contains("closure capture `editable`")
                && closure.contains("no admitted carrier"),
            "{closure}"
        );
    }

    #[test]
    fn local_send_lookalike_does_not_create_an_escape_boundary() {
        crate::typeck::check_str(
            "mode opt\n\nfn borrow(text: &String) -> &String:\n    text\n\nfn send(ch: String, value: &String) -> String:\n    ch\n\nfn main(console: Console):\n    var s = \"hi\"\n    let w = borrow(&s)\n    let _ = send(s, w)\n    console.print(*w)\n",
        )
        .expect("a local helper named `send` is not the std channel operation");
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
        let module = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\n\
             fn borrow(text: &'a String) -> &'a String:\n    text\n\n\
             fn main(console: Console):\n    let s = \"hi\"\n    let w = borrow(&s)\n    console.print(w)\n    0\n",
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
    fn explicit_projected_borrow_facts_separate_field_storage_from_root_storage() {
        let projected = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\n\
             type Pair:\n    value: String\n\n\
             fn main():\n    var pair = Pair(\"before\")\n    let handle = &mut pair.value\n    let _ = handle\n",
        )
        .expect("parse projected explicit borrow");
        let projected = crate::typeck::annotate_checked(projected)
            .expect("type-check projected explicit borrow");
        let projected_facts = super::facts_with_types(projected.module(), projected.table())
            .expect("projected explicit borrow facts");
        let projected_main = projected
            .module()
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == "main" => Some(function),
                _ => None,
            })
            .expect("projected main");
        let [projected_event] = projected_facts.opens_after(&projected_main.body.stmts[1]) else {
            panic!("projected explicit borrow opens one loan")
        };
        assert_eq!(projected_event.owner_type, Type::Named("String".into(), Vec::new()));
        assert_eq!(
            projected_event.owner_root().direct_storage_type,
            Some(Type::Named("Pair".into(), Vec::new()))
        );

        let root = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\nfn main():\n    var text = \"before\"\n    let handle = &mut text\n    let _ = handle\n",
        )
        .expect("parse root explicit borrow");
        let root = crate::typeck::annotate_checked(root)
            .expect("type-check root explicit borrow");
        let root_facts = super::facts_with_types(root.module(), root.table())
            .expect("root explicit borrow facts");
        let root_main = root
            .module()
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == "main" => Some(function),
                _ => None,
            })
            .expect("root main");
        let [root_event] = root_facts.opens_after(&root_main.body.stmts[1]) else {
            panic!("root explicit borrow opens one loan")
        };
        assert_eq!(root_event.owner_type, Type::Named("String".into(), Vec::new()));
        assert_eq!(
            root_event.owner_root().direct_storage_type,
            Some(Type::Named("String".into(), Vec::new()))
        );
    }

    #[test]
    fn borrowed_shape_separates_logical_shell_projection_and_owner_root() {
        let module = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\n\
             type Pair:\n    left: String\n    right: String\n\n\
             fn borrow(pair: &'a Pair) -> &'a Pair:\n    pair\n\n\
             fn main(console: Console):\n    let pair = Pair(\"left\", \"right\")\n    let whole = borrow(&pair)\n    let left = whole.left\n    console.print(left)\n",
        )
        .expect("parse");
        let loan_facts = facts(&module).expect("projection facts");
        let main = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == "main" => Some(function),
                _ => None,
            })
            .expect("main");
        let binding = &main.body.stmts[2];
        let [event] = loan_facts.opens_after(binding) else {
            panic!("one projected loan opens")
        };

        assert_eq!(event.view, "left");
        assert_eq!(event.owner_root().local, "pair");
        assert_eq!(event.owner_place().root.local, "pair");
        assert_eq!(
            event.owner_place().projection,
            LoanProjection { steps: vec![LoanProjectionStep::Field("left".into())] }
        );
        let shapes = loan_facts.borrowed_value_shapes_after(binding);
        let [shape] = shapes.as_slice() else {
            panic!("one logical borrowed shell opens")
        };
        assert_eq!(shape.shell, "left");
        assert_eq!(shape.roots.len(), 1);
        assert_eq!(shape.roots[0].ordinal, 0);
        assert_eq!(shape.roots[0].root.local, "pair");
        assert_ne!(shape.roots[0].root.local, shape.shell);
        assert_eq!(shape.roots[0].contributions[0].place.projection, event.projection);
    }

    #[test]
    fn borrowed_shape_coalesces_one_owner_into_one_ordered_root_companion() {
        let module = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\n\
             fn duplicate(text: &'a String) \
                 -> (&'a String, &'a String):\n    (text, text)\n\n\
             fn main(console: Console):\n    let text = \"same owner\"\n    let pair = duplicate(&text)\n    console.print(pair[0])\n    console.print(pair[1])\n",
        )
        .expect("parse");
        let loan_facts = facts(&module).expect("aggregate owner facts");
        let main = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == "main" => Some(function),
                _ => None,
            })
            .expect("main");
        let shapes = loan_facts.borrowed_value_shapes_after(&main.body.stmts[1]);
        let [shape] = shapes.as_slice() else {
            panic!("one borrowed tuple shell opens")
        };
        assert_eq!(shape.roots.len(), 1, "one owner base has one hidden companion");
        assert_eq!(shape.roots[0].root.local, "text");
        assert_eq!(shape.roots[0].contributions.len(), 2);
        assert_eq!(
            shape.roots[0].contributions[0].borrower_projection,
            LoanProjection { steps: vec![LoanProjectionStep::Tuple(0)] }
        );
        assert_eq!(
            shape.roots[0].contributions[1].borrower_projection,
            LoanProjection { steps: vec![LoanProjectionStep::Tuple(1)] }
        );
    }

    #[test]
    fn fixed_borrowed_nominal_construction_copy_and_return_keep_the_checked_root() {
        let module = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\n\
             type Cursor('a):\n    view: &'a String\n    offset: Int\n\n\
             fn make(input: &'a String) -> Cursor('a):\n    Cursor(input, 0)\n\n\
             fn forward(cursor: Cursor('a)) -> Cursor('a):\n    cursor\n\n\
             fn main(console: Console):\n    let input = \"root\"\n    let first = make(&input)\n    let copy = first\n    let next = forward(copy)\n    let actual = next.view\n    console.print(actual)\n    console.print(first.view)\n",
        )
        .expect("parse fixed borrowed nominal fixture");
        let loan_facts = facts(&module).expect("projection-aware nominal facts");
        let main = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == "main" => Some(function),
                _ => None,
            })
            .expect("main");

        for (statement, shell) in [
            (&main.body.stmts[1], "first"),
            (&main.body.stmts[2], "copy"),
            (&main.body.stmts[3], "next"),
        ] {
            let shapes = loan_facts.borrowed_value_shapes_after(statement);
            let [shape] = shapes.as_slice() else {
                panic!("one checked borrowed shape for {shell}")
            };
            assert_eq!(shape.shell, shell);
            assert_eq!(shape.roots.len(), 1);
            assert_eq!(shape.roots[0].root.local, "input");
            assert_eq!(
                shape.roots[0].contributions[0].borrower_projection,
                LoanProjection {
                    steps: vec![LoanProjectionStep::Field("view".into())]
                },
            );
        }

        let actual = &main.body.stmts[4];
        let [projection] = loan_facts.opens_after(actual) else {
            panic!("binding next.view opens one projected loan")
        };
        let Stmt::Let {
            value: Expr::Field { base, field },
            ..
        } = actual
        else {
            panic!("actual binds a fixed field projection")
        };
        assert!(matches!(base.as_ref(), Expr::Var(name) if name == "next"));
        assert_eq!(field, "view");
        assert_eq!(projection.view, "actual");
        assert_eq!(projection.owner_root().local, "input");
        assert_eq!(projection.projection, LoanProjection::default());

        let copy = &main.body.stmts[2];
        let simultaneous = loan_facts.active_at(&main.body.stmts[5]);
        assert!(
            simultaneous.iter().any(|event| event.view == "first"),
            "the original shell remains live after the copy"
        );
        assert!(
            loan_facts.opens_after(copy).iter().any(|event| event.view == "copy"),
            "the copy opens its own logical shell"
        );
    }

    #[test]
    fn borrowed_shell_field_replacement_sequences_distinct_owner_roots() {
        let module = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\n\
             type Cursor('a):\n    view: &'a String\n    offset: Int\n\n\
             fn make(input: &'a String) -> Cursor('a):\n    Cursor(input, 0)\n\n\
             fn replace(left: &'a String, right: &'a String) -> Int:\n    var cursor = make(left)\n    cursor.view = right\n    cursor.offset\n",
        )
        .expect("parse borrowed shell root-transition fixture");
        let typed = crate::typeck::annotate_checked(module)
            .expect("type-check borrowed shell root-transition fixture");
        let loan_facts = crate::loans::facts_with_types(typed.module(), typed.table())
            .expect("publish borrowed shell root-transition facts");
        let replace = typed
            .module()
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == "replace" => Some(function),
                _ => None,
            })
            .expect("replace");
        let update = &replace.body.stmts[1];
        let mutation = loan_facts
            .shell_mutation_after(update)
            .expect("the borrowed-field update publishes a root transition");
        assert_eq!(mutation.roots_before.len(), 1);
        assert_eq!(mutation.roots_before[0].owner_root().local, "left");
        assert_eq!(mutation.roots_after.len(), 1);
        assert_eq!(mutation.roots_after[0].owner_root().local, "right");
        assert!(loan_facts.closes_after(update).contains(&mutation.roots_before[0]));
        assert!(loan_facts.opens_after(update).contains(&mutation.roots_after[0]));
    }

    #[test]
    fn copied_borrowed_shell_keeps_the_original_live_for_owner_conflicts() {
        let module = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\n\
             type Cursor('a):\n    view: &'a String\n\n\
             fn make(input: &'a String) -> Cursor('a):\n    Cursor(input)\n\n\
             fn main(console: Console):\n    var input = \"root\"\n    let first = make(&input)\n    let copy = first\n    console.print(copy.view)\n    input = \"changed\"\n    console.print(first.view)\n",
        )
        .expect("parse simultaneous borrowed shell fixture");
        let error = match facts(&module) {
            Ok(_) => panic!("the still-live original shell must retain the owner conflict"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("owner `input` is reassigned")
                && error.contains("reference `first`")
                && (error.contains("still live") || error.contains("still \n             live")),
            "{error}"
        );
        // (RFC-0112 row 7) The diagnostic names the interior borrowed-aggregate
        // field whose live use keeps the owner borrowed — here `Cursor`'s `.view`
        // field — so the author knows exactly which projection to shorten,
        // destructure, or materialize with `.owned()`.
        assert!(
            error.contains("borrowed-aggregate field `.view`"),
            "row-7 aggregate field locus missing: {error}"
        );
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

    #[test]
    fn fixed_borrowed_nominal_cannot_cross_a_relation_erasing_generic_call() {
        let module = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\n\
             type Holder('a):\n    view: &'a String\n\n\
             fn erase(value: a) -> a:\n    value\n\n\
             fn bad(input: &'a String) -> Holder('a):\n    let holder = Holder(input)\n    erase(holder)\n",
        )
        .expect("parse relation-erasing call fixture");
        let error = match facts(&module) {
            Ok(_) => panic!("a generic parameter must not erase a borrowed shell relation"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("argument 1 passed to `erase`")
                && error.contains("owner relation from `input`")
                && error.contains("parameter type erases that relation"),
            "{error}"
        );

        let hidden_send = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\n\
             type Holder('a):\n    view: &'a String\n\n\
             fn leak(value: a, tx: Sender(a)) -> Task(()):\n    send(tx, value)\n\n\
             fn bad(tx: Sender(Holder('a)), input: &'a String) -> Task(()):\n    let holder = Holder(input)\n    leak(holder, tx)\n",
        )
        .expect("parse relation-erasing send fixture");
        let error = match facts(&hidden_send) {
            Ok(_) => panic!("a generic send helper must not hide a borrowed shell relation"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("argument 1 passed to `leak`")
                && error.contains("owner relation from `input`")
                && error.contains("projection `.view`")
                && error.contains("parameter type erases that relation"),
            "{error}"
        );

        let composite = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\n\
             type Holder('a):\n    view: &'a String\n\n\
             fn discard(value: (a,)) -> ():\n    ()\n\n\
             fn bad(input: &'a String) -> ():\n    let holder = Holder(input)\n    discard((holder,))\n",
        )
        .expect("parse composite relation-erasing fixture");
        let error = match facts(&composite) {
            Ok(_) => panic!("a composite generic parameter must not hide a borrowed shell"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("argument 1 passed to `discard`")
                && error.contains("owner relation from `input`")
                && error.contains("projection `[0].view`")
                && error.contains("parameter type erases that relation"),
            "{error}"
        );

        let mixed = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\n\
             type Holder('a):\n    view: &'a String\n\n\
             fn extract(value: (a, &'b String)) -> a:\n    value[0]\n\n\
             fn bad(input: &'a String, witness: &'b String) -> Holder('a):\n    let holder = Holder(input)\n    extract((holder, witness))\n",
        )
        .expect("parse mixed relation-erasing fixture");
        let error = match facts(&mixed) {
            Ok(_) => panic!("an unrelated borrowed sibling must not authenticate a generic slot"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("argument 1 passed to `extract`")
                && error.contains("owner relation from `input`")
                && error.contains("projection `[0].view`")
                && error.contains("parameter type erases that relation"),
            "{error}"
        );

        let mixed_send = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\n\
             type Holder('a):\n    view: &'a String\n\n\
             fn leak(value: (a, &'b String), tx: Sender(a)) -> Task(()):\n    send(tx, value[0])\n\n\
             fn bad(tx: Sender(Holder('a)), input: &'a String, witness: &'b String) -> Task(()):\n    let holder = Holder(input)\n    leak((holder, witness), tx)\n",
        )
        .expect("parse mixed relation-erasing send fixture");
        let error = match facts(&mixed_send) {
            Ok(_) => panic!("an unrelated borrowed sibling must not hide a generic send"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("argument 1 passed to `leak`")
                && error.contains("owner relation from `input`")
                && error.contains("projection `[0].view`")
                && error.contains("parameter type erases that relation"),
            "{error}"
        );

        let preserving = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\n\
             type Holder('a):\n    view: &'a String\n\n\
             fn inspect(holder: Holder('a)) -> Int:\n    0\n\n\
             fn good(input: &'a String) -> Int:\n    let holder = Holder(input)\n    inspect(holder)\n",
        )
        .expect("parse relation-preserving call fixture");
        facts(&preserving).expect("an exact lifetime-bearing parameter preserves the relation");
    }

    #[test]
    fn multi_owner_companions_preserve_checked_relation_order() {
        let module = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\n\
             fn pair(left: &'left String, right: &'right String) \
                 -> (&'left String, &'right String):\n    (left, right)\n\n\
             fn main(console: Console):\n    let left = \"left\"\n    let right = \"right\"\n    let pair = pair(&left, &right)\n    console.print(pair[0])\n",
        )
        .expect("parse");
        let loan_facts = facts(&module).expect("multi-owner shape facts");
        let main = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == "main" => Some(function),
                _ => None,
            })
            .expect("main");
        let shapes = loan_facts.borrowed_value_shapes_after(&main.body.stmts[2]);
        let [shape] = shapes.as_slice() else { panic!("one tuple shell") };
        assert_eq!(
            shape
                .roots
                .iter()
                .map(|root| (root.ordinal, root.root.local.as_str()))
                .collect::<Vec<_>>(),
            vec![(0, "left"), (1, "right")],
        );
    }

    #[test]
    fn branch_edges_carry_the_same_checked_owner_place_to_each_arm() {
        let module = witchy_syntax::parser::parse_reference_migration(&opt(
            "    let s = \"text\"\n    let w = borrow(&s)\n    if true:\n        console.print(w)\n    else:\n        console.print(w)\n    console.print(w)\n",
        ))
        .expect("parse");
        let loan_facts = facts(&module).expect("branch loan facts");
        let main = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == "main" => Some(function),
                _ => None,
            })
            .expect("main");
        let branch = &main.body.stmts[2];
        for kind in [LoanEdgeKind::BranchThen, LoanEdgeKind::BranchElse] {
            let edge = loan_facts
                .edges_from(branch)
                .iter()
                .find(|edge| edge.kind == kind)
                .expect("each branch arm has an edge");
            assert_eq!(edge.carries.len(), 1);
            assert_eq!(edge.carries[0].owner_root().local, "s");
            assert!(edge.closes.is_empty());
            assert!(edge.transfers.is_empty());
        }
        let kinds: Vec<LoanEdgeKind> = loan_facts
            .edges_from(branch)
            .iter()
            .map(|edge| edge.kind)
            .collect();
        assert_eq!(kinds, vec![LoanEdgeKind::BranchThen, LoanEdgeKind::BranchElse]);
        assert_eq!(
            loan_facts
                .edges_from_completion(branch)
                .iter()
                .map(|edge| edge.kind)
                .collect::<Vec<_>>(),
            vec![LoanEdgeKind::Fallthrough],
        );
    }

    #[test]
    fn branch_result_loan_opens_only_after_the_selected_arm_completes() {
        let module = witchy_syntax::parser::parse_reference_migration(&opt(
            "    let text = \"owner\"\n    let view = if true:\n        borrow(&text)\n    else:\n        borrow(&text)\n    console.print(view)\n",
        ))
        .expect("parse");
        let loan_facts = facts(&module).expect("result binding facts");
        let main = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == "main" => Some(function),
                _ => None,
            })
            .expect("main");
        let binding = &main.body.stmts[1];
        let branch_edges = loan_facts.edges_from(binding);
        assert_eq!(branch_edges.len(), 2, "there is no generic bypass edge");
        assert!(branch_edges.iter().all(|edge| edge.opens.is_empty()));
        assert!(branch_edges
            .iter()
            .all(|edge| edge.to != Some(loan_facts.point(&main.body.stmts[2]))));

        let [completion] = loan_facts.edges_from_completion(binding) else {
            panic!("the completed if result has one continuation edge")
        };
        assert_eq!(completion.kind, LoanEdgeKind::Fallthrough);
        assert_eq!(completion.to, Some(loan_facts.point(&main.body.stmts[2])));
        assert_eq!(completion.opens.len(), 1);
        assert_eq!(completion.opens[0].view, "view");
        assert_eq!(completion.opens[0].owner_root().local, "text");
    }

    #[test]
    fn loop_back_edge_closes_a_body_local_loan_at_its_checked_last_use() {
        let module = witchy_syntax::parser::parse_reference_migration(&opt(
            "    let s = \"text\"\n    while false:\n        let w = borrow(&s)\n        console.print(w)\n    console.print(s)\n",
        ))
        .expect("parse");
        let loan_facts = facts(&module).expect("loop loan facts");
        let main = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == "main" => Some(function),
                _ => None,
            })
            .expect("main");
        let Stmt::Expr(Expr::While { body, .. }) = &main.body.stmts[1] else {
            panic!("while statement")
        };
        let edge = loan_facts
            .edges_from(&body.stmts[1])
            .iter()
            .find(|edge| edge.kind == LoanEdgeKind::LoopBack)
            .expect("the body tail has a loop-back edge");
        assert_eq!(edge.to, Some(loan_facts.point(&main.body.stmts[1])));
        assert!(edge.carries.is_empty());
        assert_eq!(edge.closes.len(), 1);
        assert_eq!(edge.closes[0].owner_root().local, "s");

        let loop_stmt = &main.body.stmts[1];
        assert_eq!(
            loan_facts
                .edges_from(loop_stmt)
                .iter()
                .map(|edge| edge.kind)
                .collect::<Vec<_>>(),
            vec![LoanEdgeKind::LoopEnter, LoanEdgeKind::LoopExit],
        );
        assert_eq!(
            loan_facts
                .edges_from_completion(loop_stmt)
                .iter()
                .map(|edge| edge.kind)
                .collect::<Vec<_>>(),
            vec![LoanEdgeKind::Fallthrough],
        );
    }

    #[test]
    fn break_and_continue_target_exact_loop_completion_and_header_points() {
        let module = witchy_syntax::parser::parse_reference_migration(
            "fn breaks(console: Console):\n    while true:\n        break\n    console.print(\"done\")\n\n\
             fn continues(console: Console):\n    while true:\n        continue\n    console.print(\"done\")\n",
        )
        .expect("parse");
        let loan_facts = facts(&module).expect("loop control facts");
        let function = |name: &str| {
            module
                .items
                .iter()
                .find_map(|item| match item {
                    Item::Function(function) if function.name == name => Some(function),
                    _ => None,
                })
                .expect("function")
        };

        let breaks = function("breaks");
        let Stmt::Expr(Expr::While { body, .. }) = &breaks.body.stmts[0] else {
            panic!("break loop")
        };
        let [edge] = loan_facts.edges_from(&body.stmts[0]) else {
            panic!("one break edge")
        };
        assert_eq!(edge.kind, LoanEdgeKind::Break);
        assert_eq!(edge.to, Some(loan_facts.completion_point(&breaks.body.stmts[0])));

        let continues = function("continues");
        let Stmt::Expr(Expr::While { body, .. }) = &continues.body.stmts[0] else {
            panic!("continue loop")
        };
        let [edge] = loan_facts.edges_from(&body.stmts[0]) else {
            panic!("one continue edge")
        };
        assert_eq!(edge.kind, LoanEdgeKind::Continue);
        assert_eq!(edge.to, Some(loan_facts.point(&continues.body.stmts[0])));
    }

    #[test]
    fn explicit_return_transfers_only_a_checked_borrowed_result_root() {
        let module = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\n\
             fn borrow(text: &'a String) -> &'a String:\n    text\n\n\
             fn forward(text: &'a String) -> &'a String:\n    let view = borrow(text)\n    return view\n",
        )
        .expect("parse");
        let loan_facts = facts(&module).expect("return transfer facts");
        let forward = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == "forward" => Some(function),
                _ => None,
            })
            .expect("forward");
        let edge = loan_facts
            .edges_from(&forward.body.stmts[1])
            .iter()
            .find(|edge| edge.kind == LoanEdgeKind::Return)
            .expect("return edge");
        assert_eq!(edge.transfers.len(), 1);
        assert_eq!(edge.transfers[0].view, "view");
        assert_eq!(edge.transfers[0].owner_root().local, "text");
        assert!(edge.closes.is_empty());
    }

    #[test]
    fn direct_explicit_tail_and_call_returns_publish_checked_root_transfers() {
        let module = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\n\
             fn borrow(text: &'a String) -> &'a String:\n    text\n\n\
             fn explicit(text: &'a String) -> &'a String:\n    return text\n\n\
             fn tail(text: &'a String) -> &'a String:\n    text\n\n\
             fn called(text: &'a String) -> &'a String:\n    return borrow(text)\n",
        )
        .expect("parse");
        let loan_facts = facts(&module).expect("direct return transfer facts");
        for name in ["explicit", "tail", "called"] {
            let function = module
                .items
                .iter()
                .find_map(|item| match item {
                    Item::Function(function) if function.name == name => Some(function),
                    _ => None,
                })
                .expect("returning function");
            let [edge] = loan_facts.edges_from(&function.body.stmts[0]) else {
                panic!("{name} has one return edge")
            };
            assert_eq!(edge.kind, LoanEdgeKind::Return, "{name}");
            assert_eq!(edge.transfers.len(), 1, "{name}");
            assert_eq!(edge.transfers[0].owner_root().local, "text", "{name}");
            assert!(edge.closes.is_empty(), "{name}");
        }
    }

    #[test]
    fn owned_return_closes_instead_of_transferring_a_borrowed_root() {
        let module = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\n\
             fn borrow(text: &'a String) -> &'a String:\n    text\n\n\
             fn owned(text: String) -> String:\n    text\n\n\
             fn copy(text: &'a String) -> String:\n    let view = borrow(text)\n    return view.owned()\n",
        )
        .expect("parse");
        let loan_facts = facts(&module).expect("owned return facts");
        let copy = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == "copy" => Some(function),
                _ => None,
            })
            .expect("copy");
        let edge = loan_facts
            .edges_from(&copy.body.stmts[1])
            .iter()
            .find(|edge| edge.kind == LoanEdgeKind::Return)
            .expect("return edge");
        assert!(edge.transfers.is_empty());
        assert_eq!(edge.closes.len(), 1);
        assert_eq!(edge.closes[0].owner_root().local, "text");
    }

    #[test]
    fn question_mark_has_success_carry_and_failure_cleanup_edges() {
        let module = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\n\
             fn borrow(text: &'a String) -> &'a String:\n    text\n\n\
             fn risky() -> Result(Int, String):\n    Ok(1)\n\n\
             fn run(console: Console) -> Result(Int, String):\n    let text = \"owner\"\n    let view = borrow(&text)\n    let value = risky()?\n    console.print(view)\n    Ok(value)\n",
        )
        .expect("parse");
        let loan_facts = facts(&module).expect("propagation facts");
        let run = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == "run" => Some(function),
                _ => None,
            })
            .expect("run");
        let question = &run.body.stmts[2];
        let propagation = loan_facts
            .edges_from(question)
            .iter()
            .find(|edge| edge.kind == LoanEdgeKind::Propagate)
            .expect("a ? expression has a failure edge");
        assert_eq!(propagation.closes.len(), 1);
        assert_eq!(propagation.closes[0].owner_root().local, "text");
        assert!(propagation.transfers.is_empty());
        let success = loan_facts
            .edges_from(question)
            .iter()
            .find(|edge| edge.kind == LoanEdgeKind::Fallthrough)
            .expect("a ? expression also has a success edge");
        assert_eq!(success.carries.len(), 1);
        assert_eq!(success.carries[0].owner_root().local, "text");
    }

    #[test]
    fn question_mark_inside_a_lambda_does_not_escape_into_the_enclosing_cfg() {
        let module = witchy_syntax::parser::parse_reference_migration(
            "fn risky() -> Result(Int, String):\n    Ok(1)\n\n\
             fn main(console: Console):\n    let callback = fn() -> Result(Int, String):\n        let value = risky()?\n        Ok(value)\n    console.print(\"ready\")\n",
        )
        .expect("parse");
        let loan_facts = facts(&module).expect("lambda propagation facts");
        let main = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == "main" => Some(function),
                _ => None,
            })
            .expect("main");
        let Stmt::Let { value: Expr::Lambda { body, .. }, .. } = &main.body.stmts[0] else {
            panic!("lambda binding")
        };
        assert!(loan_facts
            .edges_from(&main.body.stmts[0])
            .iter()
            .all(|edge| edge.kind != LoanEdgeKind::Propagate));
        assert!(loan_facts
            .edges_from(&body.stmts[0])
            .iter()
            .any(|edge| edge.kind == LoanEdgeKind::Propagate));
    }

    #[test]
    fn persisted_projection_keeps_the_original_root_and_fixed_path() {
        let module = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\n\
             type Pair:\n    left: String\n    right: String\n\n\
             fn borrow(pair: &'a Pair) -> &'a Pair:\n    pair\n\n\
             fn main(console: Console):\n    var pair = Pair(\"left\", \"right\")\n    let whole = borrow(&pair)\n    let left = whole.left\n    console.print(left)\n",
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
    fn fixed_ranges_are_facts_and_dynamic_projections_do_not_persist() {
        let parse = |projection: &str| {
            witchy_syntax::parser::parse_reference_migration(&format!(
                "mode opt\n\n\
                 fn borrow(xs: &'a List(Int)) -> &'a List(Int):\n    xs\n\n\
                 fn main(console: Console):\n    let xs = [1, 2, 3]\n    let whole = borrow(&xs)\n    let window = whole[{projection}]\n    console.print(\"done\")\n"
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
            witchy_syntax::parser::parse_reference_migration(&format!(
                "mode opt\n\n\
                 fn borrow(xs: &'a List(Int)) -> &'a List(Int):\n    xs\n\n\
                 fn main(console: Console):\n    var xs = [1, 2, 3]\n    let whole = borrow(&xs)\n    let window = whole[0..2]\n    xs[{index}] = 9\n    console.print(\"${{window}}\")\n"
            ))
            .expect("parse")
        };
        for index in [1, 2] {
            let error = match facts(&mutation(index)) {
                Ok(_) => panic!("a live range conservatively freezes its owner root"),
                Err(error) => error,
            };
            assert!(error.to_string().contains("owner `xs` is reassigned"), "{error}");
        }
    }

    #[test]
    fn fixed_tuple_owner_sets_select_the_projected_owner() {
        let module = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\n\
             fn pair(left: &'left String, right: &'right String) \
                 -> (&'left String, &'right String):\n    (left, right)\n\n\
             fn main(console: Console):\n    var left = \"left\"\n    var right = \"right\"\n    let both = pair(&left, &right)\n    let first = both[0]\n    right = \"changed\"\n    console.print(first)\n",
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
        let module = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\n\
             type PairView('left, 'right):\n    first: &'left String\n    second: &'right String\n\n\
             fn pair(left: &'left String, right: &'right String) \
                 -> PairView('left, 'right):\n    PairView(left, right)\n\n\
             fn main(console: Console):\n    var left = \"left\"\n    var right = \"right\"\n    let both = pair(&left, &right)\n    let first = both.first\n    right = \"changed\"\n    console.print(first)\n",
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

    #[test]
    fn empty_fixed_ranges_do_not_overlap_storage() {
        let empty = LoanProjection {
            steps: vec![LoanProjectionStep::Range { lo: 2, hi: 2, inclusive: false }],
        };
        let empty_inclusive = LoanProjection {
            steps: vec![LoanProjectionStep::Range { lo: 3, hi: 2, inclusive: true }],
        };
        let point = LoanProjection { steps: vec![LoanProjectionStep::Index(2)] };

        assert!(!projections_overlap(&empty, &empty));
        assert!(!projections_overlap(&empty, &point));
        assert!(!projections_overlap(&empty_inclusive, &point));
    }

    #[test]
    fn generic_nominal_fields_preserve_nested_borrow_slots() {
        let module = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\n\
             type Leaf('leaf):\n    view: &'leaf String\n\n\
             type Wrapper(a, 'scope):\n    item: a\n\n\
             type Envelope(a, 'scope):\n    inner: Wrapper(a, 'scope)\n\n\
             type GenericView(a, 'scope):\n    view: &'scope a\n",
        )
        .expect("borrowed generic declarations parse");
        let catalog = BorrowRelationCatalog::from_module(&module);
        let lifetime = witchy_syntax::ast::Type::Named("'owner".into(), Vec::new());
        let leaf = witchy_syntax::ast::Type::Named("Leaf".into(), vec![lifetime.clone()]);
        let wrapper = witchy_syntax::ast::Type::Named(
            "Envelope".into(),
            vec![leaf, lifetime],
        );

        let slots = catalog.slots(&wrapper);
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].lifetime, "owner");
        assert_eq!(
            slots[0].projection,
            LoanProjection {
                steps: vec![
                    LoanProjectionStep::Field("inner".into()),
                    LoanProjectionStep::Field("item".into()),
                    LoanProjectionStep::Field("view".into()),
                ],
            }
        );
        assert_eq!(
            slots[0].storage_type,
            witchy_syntax::ast::Type::Named("String".into(), Vec::new())
        );

        let direct = witchy_syntax::ast::Type::Named(
            "GenericView".into(),
            vec![
                witchy_syntax::ast::Type::Named("String".into(), Vec::new()),
                witchy_syntax::ast::Type::Named("'owner".into(), Vec::new()),
            ],
        );
        let direct_slots = catalog.slots(&direct);
        assert_eq!(direct_slots.len(), 1);
        assert_eq!(direct_slots[0].lifetime, "owner");
        assert_eq!(
            direct_slots[0].storage_type,
            witchy_syntax::ast::Type::Named("String".into(), Vec::new())
        );
    }

    #[test]
    fn loan_telemetry_summarizes_checked_events_without_exposing_addresses() {
        let module = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\n\
             fn first(text: &'a String) -> &'a String:\n    text\n\n\
             fn main(console: Console):\n    var text = \"value\"\n    let view = first(&text)\n    console.print(view)\n",
        )
        .expect("parse telemetry fixture");
        let facts = facts(&module).expect("check telemetry fixture");
        let telemetry = facts.telemetry();

        assert!(telemetry.active_points > 0, "{telemetry:?}");
        assert!(telemetry.active_events > 0, "{telemetry:?}");
        assert!(telemetry.opens > 0, "{telemetry:?}");
        assert!(telemetry.closes > 0, "{telemetry:?}");
        assert!(telemetry.control_flow_edges > 0, "{telemetry:?}");
        assert_eq!(telemetry.subset_edges, 0, "no subset solver is active yet");
    }
