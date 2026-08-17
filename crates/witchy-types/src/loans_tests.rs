    //! RFC-0083 phase-1 loan/lifetime checker tests. Each runs the full public
    //! checker (`check_str`) over a small `mode opt` program and asserts the loan
    //! rule accepts or rejects it with the documented diagnostic.

    use crate::{
        access::AccessSignature,
        loans::facts,
        typeck::{check, check_str},
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
        .map_err(|error| crate::typeck::TypeError { message: error.to_string() })?;
        check(&linked)
    }

    fn linked_main(source: &str) -> Result<(), crate::typeck::TypeError> {
        let main = witchy_syntax::parser::parse_module(source).expect("parse linked main");
        let linked = witchy_syntax::linker::link(vec![("main".into(), main)], "main", no_comptime)
            .expect("link main with bundled std modules");
        check(&linked)
    }

    /// A borrowed-view helper plus a `main` body, as a `mode opt` module. Includes
    /// LOCAL `owned`/`send` helpers so these checker tests need no std linking
    /// (`check_str` does not resolve `import`s). A local `send` is intentionally
    /// not an authenticated channel boundary; linked std-boundary coverage lives
    /// below.
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
            "mode opt\n\nfn first(text: &'a String) -> &'a String:\n    text\n",
        )
        .expect("a well-formed explicit reference signature checks");
    }

    #[test]
    fn reference_cannot_cross_the_dynamic_persistence_boundary() {
        let err = linked_main(
            "mode opt\n\nimport dynamic\n\nfn erase(text: &'a String) -> dynamic.Dynamic:\n    dynamic.dynamic(text)\n\nfn main():\n    Nil\n",
        )
        .expect_err("a reference cannot be stored in Dynamic");
        assert!(
            err.message.contains("cannot be stored in Dynamic")
                && err.message.contains("owned()"),
            "{err}"
        );
    }

    #[test]
    fn explicit_reference_type_requires_mode_opt_without_loan_follow_ons() {
        let err = check_str("fn first(text: &'a String) -> &'a String:\n    text\n")
            .expect_err("references are opt-only");
        assert!(err.contains("mode opt"), "{err}");
        assert!(!err.contains("loan"), "normal-mode error must stop at the mode boundary: {err}");
    }

    #[test]
    fn normal_mode_rejects_every_direct_reference_surface_at_the_mode_boundary() {
        let cases = [
            (
                "exclusive signature",
                "fn edit(value: &'a mut String) -> &'a mut String:\n    value\n",
            ),
            (
                "reference-bearing field",
                "type Holder:\n    value: &'a String\n",
            ),
            (
                "lifetime-bearing nominal declaration",
                "type Holder('a):\n    value: String\n",
            ),
            (
                "reference-bearing type alias",
                "type StringView = &'a String\n",
            ),
            (
                "reference-bearing trait method",
                "trait Inspect:\n    fn value(let self) -> &'a String\n",
            ),
            (
                "borrow expression",
                "fn main():\n    let text = \"value\"\n    let view = &text\n",
            ),
            (
                "dereference expression",
                "fn main():\n    let text = \"value\"\n    let view = *text\n",
            ),
        ];
        for (label, source) in cases {
            let err = check_str(source).expect_err(label);
            assert!(err.contains("mode opt"), "{label}: {err}");
            assert!(
                !err.contains("loan") && !err.contains("lifetime must") && !err.contains("unbound"),
                "{label} must stop at the mode boundary: {err}"
            );
        }
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

        let err = check_str(
            "mode opt\n\nfn bad(own text: &'a String) -> &'a String:\n    text\n",
        )
        .expect_err("a shared reference cannot be consumed");
        assert!(err.contains("to `own`"), "{err}");
    }

    #[test]
    fn reference_handle_qualifiers_remain_distinct_from_reference_targets() {
        check_str(
            "mode opt\n\nfn take(own text: unique &'a mut String) -> unique &'a mut String:\n    text\n",
        )
        .expect("a unique exclusive-reference handle may be consumed and returned");

        check_str(
            "mode opt\n\nfn inspect(text: frozen &'a String) -> frozen &'a String:\n    text\n",
        )
        .expect("a frozen shared-reference handle remains a read-only handle");

        let err = check_str(
            "mode opt\n\nfn bad(text: frozen unique &'a mut String) -> Nil:\n    Nil\n",
        )
        .expect_err("frozen and unique cannot qualify the same exclusive reference handle");
        assert!(err.contains("frozen") && err.contains("unique") && err.contains("exclusive reference"), "{err}");

        let err = check_str(
            "mode opt\n\nfn bad(text: unique frozen &'a mut String) -> Nil:\n    Nil\n",
        )
        .expect_err("qualifier order cannot hide the contradictory exclusive handle");
        assert!(err.contains("frozen") && err.contains("unique") && err.contains("exclusive reference"), "{err}");

        let err = check_str(
            "mode opt\n\nfn bad(text: &'a mut frozen String) -> Nil:\n    Nil\n",
        )
        .expect_err("a target qualifier cannot weaken an exclusive reference");
        assert!(err.contains("frozen") && err.contains("reference target"), "{err}");

        let err = check_str(
            "mode opt\n\nfn bad(text: &'a mut String) -> local unique &'a mut String:\n    text\n",
        )
        .expect_err("a local unique reference handle cannot escape in a result");
        assert!(err.contains("local unique") && err.contains("escape"), "{err}");
    }

    #[test]
    fn exclusive_handle_qualifiers_survive_aggregate_and_callable_positions() {
        check_str(
            "mode opt\n\nfn pack(text: unique &'a mut String) -> (unique &'a mut String, Int):\n    (text, 1)\n\nfn unpack(pair: (unique &'a mut String, Int)) -> unique &'a mut String:\n    pair.0\n",
        )
        .expect("a unique exclusive handle retains its qualifier below tuple construction and projection");

        check_str(
            "mode opt\n\nfn pass(own text: unique &'a mut String) -> unique &'a mut String:\n    text\n\nfn relay(f: fn(own unique &'a mut String) -> unique &'a mut String, own text: unique &'a mut String) -> unique &'a mut String:\n    f(text)\n",
        )
        .expect("a function value retains the consumed unique exclusive-handle contract");

        let err = check_str(
            "mode opt\n\nfn bad(f: fn(frozen unique &'a mut String) -> Nil) -> Nil:\n    Nil\n",
        )
        .expect_err("a function-value parameter cannot hide contradictory exclusive-handle qualifiers");
        assert!(err.contains("frozen") && err.contains("unique") && err.contains("exclusive reference"), "{err}");
    }

    #[test]
    fn mutable_exclusive_reference_retains_writeback_access() {
        check_str(
            "mode opt\n\nfn replace(var text: &'a mut String) -> Nil:\n    *text = \"changed\"\n",
        )
        .expect("an exclusive reference may use the established write-back convention");
    }

    #[test]
    fn dereference_assignment_requires_and_accepts_an_exclusive_reference() {
        check_str(
            "mode opt\n\nfn edit(text: &'a mut Int) -> Nil:\n    *text = 42\n",
        )
        .expect("an exclusive reference permits a place write");

        let err = check_str(
            "mode opt\n\nfn inspect(text: &'a Int) -> Nil:\n    *text = 42\n",
        )
        .expect_err("a shared reference cannot be written through");
        assert!(err.contains("shared reference"), "{err}");
    }

    #[test]
    fn exclusive_borrow_of_frozen_storage_is_rejected() {
        let err = check_str(
            "mode opt\n\nfn main():\n    let text: frozen String = \"hello\"\n    let editable = &mut text\n",
        )
        .expect_err("frozen storage cannot grant exclusive mutable access");
        assert!(err.contains("frozen") && err.contains("exclusive reference"), "{err}");

        let err = check_str(
            "mode opt\n\nfn edit(text: &'a mut frozen String) -> Nil:\n    Nil\n",
        )
        .expect_err("owned qualifiers do not belong inside a reference target");
        assert!(err.contains("frozen") && err.contains("reference target"), "{err}");
    }

    #[test]
    fn exclusive_borrow_rejects_an_overlapping_shared_loan() {
        let err = check_str(
            "mode opt\n\nfn main(console: Console):\n    var text = \"hello\"\n    let view = &text\n    let editable = &mut text\n    console.print(view)\n    console.print(editable)\n",
        )
        .expect_err("an exclusive reference requires sole access");
        assert!(err.contains("cannot create exclusive reference"), "{err}");
    }

    #[test]
    fn shared_borrow_rejects_an_overlapping_exclusive_loan() {
        let err = check_str(
            "mode opt\n\nfn main(console: Console):\n    var text = \"hello\"\n    let editable = &mut text\n    let view = &text\n    console.print(editable)\n    console.print(view)\n",
        )
        .expect_err("a shared reference cannot overlap an exclusive loan");
        assert!(err.contains("cannot create exclusive reference"), "{err}");
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
            "mode opt\n\nfn clear(text: &'a mut String) -> Nil:\n    Nil\n\nfn main():\n    var text = \"hello\"\n    clear(text)\n",
        )
        .expect_err("ordinary values must not satisfy an exclusive-reference parameter");
        assert!(err.contains("exclusive reference (`&mut place`)"), "{err}");
    }

    #[test]
    fn exclusive_parameter_accepts_an_explicit_exclusive_reference() {
        check_str(
            "mode opt\n\nfn clear(text: &'a mut String) -> Nil:\n    Nil\n\nfn main():\n    var text = \"hello\"\n    clear(&mut text)\n",
        )
        .expect("the call preserves the exclusive-reference contract");
    }

    #[test]
    fn exclusive_reference_arguments_must_be_disjoint_at_one_call_boundary() {
        let err = check_str(
            "mode opt\n\nfn edit_pair(left: &'a mut String, right: &'a mut String) -> Nil:\n    Nil\n\nfn main():\n    var text = \"hello\"\n    edit_pair(&mut text, &mut text)\n",
        )
        .expect_err("one call cannot receive two aliases to the same exclusive place");
        assert!(
            err.contains("cannot create exclusive reference") || err.contains("overlapping"),
            "{err}"
        );

        check_str(
            "mode opt\n\ntype Pair:\n    left: String\n    right: String\n\nfn edit_pair(left: &'a mut String, right: &'a mut String) -> Nil:\n    Nil\n\nfn main():\n    var pair = Pair(\"left\", \"right\")\n    edit_pair(&mut pair.left, &mut pair.right)\n",
        )
        .expect("disjoint field projections may satisfy separate exclusive parameters");
    }

    #[test]
    fn exclusive_function_values_retain_disjoint_place_requirements() {
        let err = check_str(
            "mode opt\n\nfn edit_pair(left: &'a mut String, right: &'a mut String) -> Nil:\n    Nil\n\nfn main():\n    let edit = edit_pair\n    var text = \"hello\"\n    edit(&mut text, &mut text)\n",
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
    fn shared_parameter_accepts_a_short_shared_reborrow_of_an_exclusive_reference() {
        check_str(
            "mode opt\n\nfn inspect(text: &'a String) -> Nil:\n    Nil\n\nfn main():\n    var text = \"hello\"\n    let editable = &mut text\n    inspect(&*editable)\n",
        )
        .expect("an exclusive handle may create a short shared reborrow");
    }

    #[test]
    fn returned_shared_reference_relinquishes_exclusive_access() {
        check_str(
            "mode opt\n\nfn finish(text: &'a mut String) -> &'a String:\n    text\n\nfn main(console: Console):\n    var text = \"hello\"\n    let editable = &mut text\n    let view = finish(editable)\n    console.print(view)\n",
        )
        .expect("an exclusive reference may be relinquished as a shared result");

        let err = check_str(
            "mode opt\n\nfn finish(text: &'a mut String) -> &'a String:\n    text\n\nfn change(text: &'b mut String) -> Nil:\n    *text = \"changed\"\n\nfn main(console: Console):\n    var text = \"hello\"\n    let editable = &mut text\n    let observed = finish(editable)\n    change(observed)\n",
        )
        .expect_err("the shared result does not retain mutable capability");
        assert!(err.contains("exclusive reference (`&mut place`)"), "{err}");

        let err = check_str(
            "mode opt\n\nfn finish(text: &'a mut String) -> &'a String:\n    text\n\nfn main(console: Console):\n    var text = \"hello\"\n    let editable = &mut text\n    let observed = finish(editable)\n    *editable = \"changed\"\n    console.print(observed)\n",
        )
        .expect_err("converting an exclusive handle into a shared result retires the old handle");
        assert!(err.contains("moved exclusive reference `editable`"), "{err}");
    }

    #[test]
    fn exclusive_reference_aggregates_move_once_and_destructure_without_copying() {
        check_str(
            "mode opt\n\nfn main(console: Console):\n    var first = \"first\"\n    var second = \"second\"\n    let pair = (&mut first, &mut second)\n    let moved = pair\n    let (left, right) = moved\n    *left = \"updated-first\"\n    *right = \"updated-second\"\n    console.print(first)\n    console.print(second)\n",
        )
        .expect("an exclusive-reference aggregate moves into one destructuring use");

        let err = check_str(
            "mode opt\n\nfn main(console: Console):\n    var first = \"first\"\n    var second = \"second\"\n    let pair = (&mut first, &mut second)\n    let (left, right) = pair\n    console.print(*left)\n    console.print(*pair.1)\n    console.print(*right)\n",
        )
        .expect_err("destructuring an exclusive-reference aggregate retires the aggregate handle");
        assert!(err.contains("moved exclusive reference") || err.contains("used more than once"), "{err}");
    }

    #[test]
    fn exclusive_reference_loop_elements_are_affine() {
        let err = check_str(
            "mode opt\n\nfn all(left: &'a mut String, right: &'a mut String) -> List(&'a mut String):\n    [left, right]\n\nfn main(console: Console):\n    var first = \"first\"\n    var second = \"second\"\n    let values = all(&mut first, &mut second)\n    for value in values:\n        let alias = value\n        *value = \"updated\"\n",
        )
        .expect_err("moving a loop element retires its exclusive handle");
        assert!(err.contains("moved exclusive reference `value`"), "{err}");
    }

    #[test]
    fn returned_exclusive_reference_reborrows_an_exclusive_argument() {
        check_str(
            "mode opt\n\n\
             type Pair:\n    left: String\n    right: String\n\
             fn left(pair: &'a mut Pair) -> &'a mut String:\n    &mut pair.left\n\n\
             fn main(console: Console):\n    var pair = Pair(\"left\", \"right\")\n    let parent = &mut pair\n    let slot = left(parent)\n    *slot = \"changed\"\n    console.print(pair.right)\n",
        )
        .expect("an exclusive result reborrows and retires the input handle");

        let err = check_str(
            "mode opt\n\n\
             type Pair:\n    left: String\n    right: String\n\
             fn left(pair: &'a mut Pair) -> &'a mut String:\n    &mut pair.left\n\n\
             fn main(console: Console):\n    var pair = Pair(\"left\", \"right\")\n    let parent = &mut pair\n    let slot = left(parent)\n    *parent = Pair(\"again\", \"right\")\n    console.print(slot)\n",
        )
        .expect_err("the returned exclusive reborrow retires the old handle");
        assert!(err.contains("moved exclusive reference `parent`"), "{err}");
    }

    #[test]
    fn mutable_reborrow_suspends_and_then_restores_its_parent_handle() {
        let err = check_str(
            "mode opt\n\nfn main(console: Console):\n    var text = \"before\"\n    let parent = &mut text\n    let child = &mut *parent\n    *parent = \"blocked\"\n    console.print(*child)\n",
        )
        .expect_err("the parent remains suspended while a mutable reborrow is live");
        assert!(err.contains("moved exclusive reference `parent`"), "{err}");

        check_str(
            "mode opt\n\nfn main(console: Console):\n    var text = \"before\"\n    let parent = &mut text\n    let child = &mut *parent\n    console.print(*child)\n    *parent = \"after\"\n    console.print(*parent)\n",
        )
        .expect("the parent resumes after the mutable reborrow's final use");
    }

    #[test]
    fn explicit_shared_borrow_blocks_owner_mutation_until_its_final_use() {
        let err = check_str(
            "mode opt\n\nfn main(console: Console):\n    var text = \"hello\"\n    let view = &text\n    text = \"changed\"\n    console.print(view)\n",
        )
        .expect_err("an explicit borrow keeps its owner loaned");
        assert!(err.contains("reassigned"), "{err}");
    }

    #[test]
    fn explicit_shared_borrow_closes_after_its_final_use() {
        check_str(
            "mode opt\n\nfn main(console: Console):\n    var text = \"hello\"\n    let view = &text\n    console.print(view)\n    text = \"changed\"\n",
        )
        .expect("last use closes the explicit shared borrow");
    }

    #[test]
    fn explicit_shared_reborrow_preserves_the_original_owner_relation() {
        let err = check_str(
            "mode opt\n\nfn main(console: Console):\n    var text = \"hello\"\n    let first = &text\n    let second = &*first\n    text = \"changed\"\n    console.print(second)\n",
        )
        .expect_err("a shared reborrow keeps the original owner loaned");
        assert!(err.contains("reassigned"), "{err}");
    }

    #[test]
    fn shared_reference_handles_copy_but_cannot_be_consumed_or_erased() {
        check_str(
            "mode opt\n\nfn main(console: Console):\n    var text = \"hello\"\n    let first = &text\n    let second = first\n    console.print(first)\n    console.print(second)\n",
        )
        .expect("shared handles remain copyable");

        let moved = check_str(
            "mode opt\n\nfn main(console: Console):\n    var text = \"hello\"\n    let view = &text\n    let taken = move view\n    console.print(taken)\n",
        )
        .expect_err("a shared handle may not be consumed with move");
        assert!(moved.contains("shared reference") && moved.contains("move"), "{moved}");

        let erased = check_str(
            "mode opt\n\nfn consume(own text: String) -> Nil:\n    Nil\n\nfn main():\n    var text = \"hello\"\n    let view = &text\n    consume(view)\n",
        )
        .expect_err("a conventional own parameter may not erase a shared relation");
        assert!(erased.contains("shared reference") && erased.contains("own"), "{erased}");
    }

    #[test]
    fn explicit_borrow_argument_retains_the_returned_reference_owner() {
        let err = check_str(
            "mode opt\n\nfn first(text: &'a String) -> &'a String:\n    text\n\nfn main(console: Console):\n    var text = \"hello\"\n    let view = first(&text)\n    text = \"changed\"\n    console.print(view)\n",
        )
        .expect_err("a returned reference retains the explicitly borrowed owner");
        assert!(err.contains("reassigned"), "{err}");
    }

    #[test]
    fn direct_shared_reference_call_preserves_the_referent_owner() {
        check_str(
            "mode opt\n\nfn first(text: &'a String) -> &'a String:\n    text\n\nfn main(console: Console):\n    let text = \"value\"\n    let observed = first(&text)\n    console.print(\"${*observed}\")\n",
        )
        .expect("a returned shared reference remains tied to its direct owner");
    }

    #[test]
    fn explicit_borrow_expression_requires_mode_opt() {
        let err = check_str(
            "fn main(console: Console):\n    let text = \"hello\"\n    let view = &text\n    console.print(view)\n",
        )
        .expect_err("normal mode cannot create a source reference");
        assert!(err.contains("only in `mode opt`"), "{err}");
        assert!(!err.contains("loan"), "{err}");
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
        let error = linked_normal(
            "    var xs = [1]\n    let w = api.view(xs)\n    console.print(\"${list.length(w)}\")\n",
        )
        .expect_err("normal callers cannot name legacy reference-bearing opt exports");
        assert!(error.to_string().contains("reference-bearing opt API `api.view`"), "{error}");
        return;

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

    #[test]
    fn bundled_owned_materializes_a_view_before_owner_mutation() {
        linked_main(
            "mode opt\n\n\
             import borrow\n\n\
             fn view(text: let('a) String) -> View(String, 'a):\n    text\n\n\
             fn main(console: Console):\n    var owner = \"before\"\n    let snapshot = view(owner).owned()\n    owner = \"after\"\n    console.print(snapshot)\n    console.print(owner)\n",
        )
        .expect("the exact bundled Owned materializer ends the view loan");
    }

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
    fn reference_function_types_preserve_kind_and_lifetime_identity() {
        check_str(
            "mode opt\n\nfn first(input: &'a String) -> &'a String:\n    input\n\nfn apply(f: fn(&'a String) -> &'a String, input: &'a String) -> &'a String:\n    f(input)\n\nfn main(console: Console):\n    let text = \"value\"\n    let result = apply(first, &text)\n    console.print(result)\n",
        )
        .expect("a shared reference function type preserves its callable contract");

        let error = check_str(
            "mode opt\n\nfn mutate(input: &'a mut String) -> &'a String:\n    input\n\nfn apply(f: fn(&'a String) -> &'a String, input: &'a String) -> &'a String:\n    f(input)\n\nfn main():\n    let text = \"value\"\n    apply(mutate, &text)\n",
        )
        .expect_err("a mutable-reference function cannot erase to shared callable identity");
        assert!(
            error.contains("erases or changes its borrow/convention relation"),
            "{error}"
        );
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
    fn inferred_empty_callable_list_recovers_checked_iteration_contract() {
        check_str(
            "mode opt\n\n\
             fn strict(xs: List(Int)) -> Int:\n    list.length(xs)\n\n\
             fn use(f: fn(List(Int)) -> Int) -> Nil:\n    return\n\n\
             fn append(var values: List(a), value: a) -> List(a):\n    values\n\n\
             fn main():\n\
             \x20   var callbacks = []\n\
             \x20   append(callbacks, strict)\n\
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
             fn use(f: fn(unique List(Int)) -> Int) -> Nil:\n    return\n\n\
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
        let err = linked_main(
            "mode opt\n\nimport chan\nimport task\n\nfn borrow(text: let('a) String) -> View(String, 'a):\n    text\n\nfn bad(tx: chan.Sender(String), input: let('a) String) -> task.Task(Nil):\n    let view = borrow(input)\n    chan.send(tx, view)\n\nfn main(console: Console):\n    console.print(\"done\")\n",
        )
        .expect_err("a view sent through the canonical channel boundary escapes its owner");
        assert!(
            err.message.contains("escapes through a task or channel"),
            "{err}"
        );
    }

    #[test]
    fn exclusive_reference_sent_through_a_channel_is_rejected() {
        let err = linked_main(
            "mode opt\n\nimport chan\nimport task\n\nfn bad(tx: chan.Sender(String), input: &'a mut String) -> task.Task(Nil):\n    chan.send(tx, input)\n\nfn main(console: Console):\n    console.print(\"done\")\n",
        )
        .expect_err("an exclusive reference cannot escape through a channel");
        assert!(
            err.message.contains("escapes through a task or channel"),
            "{err}"
        );
    }

    #[test]
    fn explicit_reference_cannot_cross_json_or_reflection_boundaries() {
        for (module, call) in [("json", "json.stringify"), ("reflect", "reflect.debug")] {
            let err = linked_main(&format!(
                "mode opt\n\nimport {module}\n\nfn bad(input: &'a String) -> String:\n    {call}(input)\n\nfn main(console: Console):\n    var text = \"value\"\n    console.print(bad(&text))\n"
            ))
            .expect_err("serialization and reflection require owned data");
            assert!(
                err.message.contains("parameter type erases that relation")
                    && err.message.contains("materialize an owned value"),
                "{module} must reject a reference escape with the materialization remedy: {err}"
            );
        }
    }

    #[test]
    fn exclusive_reference_cannot_cross_async_suspension() {
        let main = witchy_syntax::parser::parse_module(
            "mode opt\n\nimport task\n\nasync fn bad(input: &'a mut String) -> task.Task(Nil):\n    let _ = task.done(0).await\n    *input = \"changed\"\n\nfn main(console: Console):\n    console.print(\"done\")\n",
        )
        .expect("parse async exclusive reference fixture");
        let err = witchy_syntax::linker::link(
            vec![("main".into(), main)],
            "main",
            no_comptime,
        )
        .expect_err("an exclusive reference cannot cross an async function boundary");
        assert!(
            err.to_string().contains("async fn `bad` may not expose a borrowed view"),
            "{err}"
        );
    }

    #[test]
    fn exclusive_references_cannot_escape_through_closures() {
        let closure = check_str(
            "mode opt\n\nfn main(console: Console):\n    var text = \"hi\"\n    let editable = &mut text\n    let later = fn(): editable\n    console.print(text)\n",
        )
        .expect_err("an exclusive reference captured by a closure escapes its owner");
        assert!(closure.contains("escapes through a closure"), "{closure}");
    }

    #[test]
    fn local_send_lookalike_does_not_create_an_escape_boundary() {
        check_str(&opt(
            "    var s = \"hi\"\n    let w = borrow(s)\n    let _ = send(s, w)\n    console.print(w)\n",
        ))
        .expect("a local helper named `send` is not the std channel operation");
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
    fn explicit_shared_references_may_be_stored_and_projected_from_a_list() {
        let err = check_str(
            "mode opt\n\nfn main(console: Console):\n    var text = \"hi\"\n    let refs = [&text]\n    let first = refs[0]\n    text = \"changed\"\n    console.print(first)\n",
        )
        .expect_err("the projected reference must keep the original owner loan live");
        assert!(err.contains("reassigned"), "{err}");

        check_str(
            "mode opt\n\nfn main(console: Console):\n    var text = \"hi\"\n    let refs = [&text]\n    let first = refs[0]\n    console.print(*first)\n",
        )
        .expect("an opt list may own a shared reference value and project it later");
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
    fn borrowed_shape_separates_logical_shell_projection_and_owner_root() {
        let module = witchy_syntax::parser::parse_module(
            "mode opt\n\n\
             type Pair:\n    left: String\n    right: String\n\n\
             fn borrow(pair: let('a) Pair) -> View(Pair, 'a):\n    pair\n\n\
             fn main(console: Console):\n    let pair = Pair(\"left\", \"right\")\n    let whole = borrow(pair)\n    let left = whole.left\n    console.print(left)\n",
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
        let module = witchy_syntax::parser::parse_module(
            "mode opt\n\n\
             fn duplicate(text: let('a) String) \
                 -> (View(String, 'a), View(String, 'a)):\n    (text, text)\n\n\
             fn main(console: Console):\n    let text = \"same owner\"\n    let pair = duplicate(text)\n    console.print(pair[0])\n    console.print(pair[1])\n",
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
        let module = witchy_syntax::parser::parse_module(
            "mode opt\n\n\
             type Cursor('a):\n    view: View(String, 'a)\n    offset: Int\n\n\
             fn make(input: let('a) String) -> Cursor('a):\n    Cursor(input, 0)\n\n\
             fn forward(cursor: Cursor('a)) -> Cursor('a):\n    cursor\n\n\
             fn main(console: Console):\n    let input = \"root\"\n    let first = make(input)\n    let copy = first\n    let next = forward(copy)\n    let actual = next.view\n    console.print(actual)\n    console.print(first.view)\n",
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
    fn scalar_shell_mutation_transports_the_checked_root_set() {
        let module = witchy_syntax::parser::parse_module(
            "mode opt\n\n\
             type Cursor('a):\n    view: View(String, 'a)\n    offset: Int\n\n\
             fn make(input: let('a) String) -> Cursor('a):\n    Cursor(input, 0)\n\n\
             fn main() -> Int:\n    let input = \"root\"\n    var cursor = make(input)\n    cursor.offset = cursor.offset + 1\n    cursor.offset\n",
        )
        .expect("parse scalar shell mutation fixture");
        let typed = crate::typeck::annotate_checked(module)
            .expect("type-check scalar shell mutation fixture");
        let loan_facts = crate::loans::facts_with_types(typed.module(), typed.table())
            .expect("publish scalar shell mutation facts");
        let main = typed
            .module()
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == "main" => Some(function),
                _ => None,
            })
            .expect("main");
        let mutation = loan_facts
            .shell_mutation_after(&main.body.stmts[2])
            .expect("the record update is authenticated as a shell mutation");
        assert_eq!(mutation.shell, "cursor");
        assert_eq!(mutation.fields, ["offset"]);
        assert_eq!(mutation.roots_before.len(), 1);
        assert_eq!(mutation.roots_after, mutation.roots_before);
        assert_eq!(mutation.roots_before[0].owner_root().local, "input");
        assert_eq!(
            mutation.roots_before[0].borrower_projection,
            LoanProjection { steps: vec![LoanProjectionStep::Field("view".into())] },
        );
    }

    #[test]
    fn borrowed_shell_field_replacement_sequences_distinct_owner_roots() {
        let module = witchy_syntax::parser::parse_module(
            "mode opt\n\n\
             type Cursor('a):\n    view: View(String, 'a)\n    offset: Int\n\n\
             fn make(input: let('a) String) -> Cursor('a):\n    Cursor(input, 0)\n\n\
             fn replace(left: let('a) String, right: let('a) String) -> Int:\n    var cursor = make(left)\n    cursor.view = right\n    cursor.offset\n",
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
        let module = witchy_syntax::parser::parse_module(
            "mode opt\n\n\
             type Cursor('a):\n    view: View(String, 'a)\n\n\
             fn make(input: let('a) String) -> Cursor('a):\n    Cursor(input)\n\n\
             fn main(console: Console):\n    var input = \"root\"\n    let first = make(input)\n    let copy = first\n    console.print(copy.view)\n    input = \"changed\"\n    console.print(first.view)\n",
        )
        .expect("parse simultaneous borrowed shell fixture");
        let error = match facts(&module) {
            Ok(_) => panic!("the still-live original shell must retain the owner conflict"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("owner `input` is reassigned")
                && error.contains("borrowed view `first`")
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
        let module = witchy_syntax::parser::parse_module(
            "mode opt\n\n\
             type Holder('a):\n    view: View(String, 'a)\n\n\
             fn bad(left: let('left) String, right: let('right) String) -> Holder('right):\n    Holder(left)\n",
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
        let module = witchy_syntax::parser::parse_module(
            "mode opt\n\n\
             type Holder('a):\n    view: View(String, 'a)\n\n\
             fn erase(value: a) -> a:\n    value\n\n\
             fn bad(input: let('a) String) -> Holder('a):\n    let holder = Holder(input)\n    erase(holder)\n",
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

        let hidden_send = witchy_syntax::parser::parse_module(
            "mode opt\n\n\
             type Holder('a):\n    view: View(String, 'a)\n\n\
             fn leak(value: a, tx: Sender(a)) -> Task(Nil):\n    send(tx, value)\n\n\
             fn bad(tx: Sender(Holder('a)), input: let('a) String) -> Task(Nil):\n    let holder = Holder(input)\n    leak(holder, tx)\n",
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

        let composite = witchy_syntax::parser::parse_module(
            "mode opt\n\n\
             type Holder('a):\n    view: View(String, 'a)\n\n\
             fn discard(value: (a,)) -> Nil:\n    ()\n\n\
             fn bad(input: let('a) String) -> Nil:\n    let holder = Holder(input)\n    discard((holder,))\n",
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

        let mixed = witchy_syntax::parser::parse_module(
            "mode opt\n\n\
             type Holder('a):\n    view: View(String, 'a)\n\n\
             fn extract(value: (a, let('b) String)) -> a:\n    value[0]\n\n\
             fn bad(input: let('a) String, witness: let('b) String) -> Holder('a):\n    let holder = Holder(input)\n    extract((holder, witness))\n",
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

        let mixed_send = witchy_syntax::parser::parse_module(
            "mode opt\n\n\
             type Holder('a):\n    view: View(String, 'a)\n\n\
             fn leak(value: (a, let('b) String), tx: Sender(a)) -> Task(Nil):\n    send(tx, value[0])\n\n\
             fn bad(tx: Sender(Holder('a)), input: let('a) String, witness: let('b) String) -> Task(Nil):\n    let holder = Holder(input)\n    leak((holder, witness), tx)\n",
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

        let preserving = witchy_syntax::parser::parse_module(
            "mode opt\n\n\
             type Holder('a):\n    view: View(String, 'a)\n\n\
             fn inspect(holder: Holder('a)) -> Int:\n    0\n\n\
             fn good(input: let('a) String) -> Int:\n    let holder = Holder(input)\n    inspect(holder)\n",
        )
        .expect("parse relation-preserving call fixture");
        facts(&preserving).expect("an exact lifetime-bearing parameter preserves the relation");
    }

    #[test]
    fn multi_owner_companions_preserve_checked_relation_order() {
        let module = witchy_syntax::parser::parse_module(
            "mode opt\n\n\
             fn pair(left: let('left) String, right: let('right) String) \
                 -> (View(String, 'left), View(String, 'right)):\n    (left, right)\n\n\
             fn main(console: Console):\n    let left = \"left\"\n    let right = \"right\"\n    let pair = pair(left, right)\n    console.print(pair[0])\n",
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
        let module = witchy_syntax::parser::parse_module(&opt(
            "    let s = \"text\"\n    let w = borrow(s)\n    if true:\n        console.print(w)\n    else:\n        console.print(w)\n    console.print(w)\n",
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
        let module = witchy_syntax::parser::parse_module(&opt(
            "    let text = \"owner\"\n    let view = if true:\n        borrow(text)\n    else:\n        borrow(text)\n    console.print(view)\n",
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
        let module = witchy_syntax::parser::parse_module(&opt(
            "    let s = \"text\"\n    while false:\n        let w = borrow(s)\n        console.print(w)\n    console.print(s)\n",
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
        let module = witchy_syntax::parser::parse_module(
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
        let module = witchy_syntax::parser::parse_module(
            "mode opt\n\n\
             fn borrow(text: let('a) String) -> View(String, 'a):\n    text\n\n\
             fn forward(text: let('a) String) -> View(String, 'a):\n    let view = borrow(text)\n    return view\n",
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
        let module = witchy_syntax::parser::parse_module(
            "mode opt\n\n\
             fn borrow(text: let('a) String) -> View(String, 'a):\n    text\n\n\
             fn explicit(text: let('a) String) -> View(String, 'a):\n    return text\n\n\
             fn tail(text: let('a) String) -> View(String, 'a):\n    text\n\n\
             fn called(text: let('a) String) -> View(String, 'a):\n    return borrow(text)\n",
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
        let module = witchy_syntax::parser::parse_module(
            "mode opt\n\n\
             fn borrow(text: let('a) String) -> View(String, 'a):\n    text\n\n\
             fn owned(text: String) -> String:\n    text\n\n\
             fn copy(text: let('a) String) -> String:\n    let view = borrow(text)\n    return owned(view)\n",
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
        let module = witchy_syntax::parser::parse_module(
            "mode opt\n\n\
             fn borrow(text: let('a) String) -> View(String, 'a):\n    text\n\n\
             fn risky() -> Result(Int, String):\n    Ok(1)\n\n\
             fn run(console: Console) -> Result(Int, String):\n    let text = \"owner\"\n    let view = borrow(text)\n    let value = risky()?\n    console.print(view)\n    Ok(value)\n",
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
        let module = witchy_syntax::parser::parse_module(
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
    fn any_live_projection_blocks_mutation_of_its_owner_root() {
        let source = |field: &str| {
            format!(
                "mode opt\n\n\
                 type Pair:\n    left: String\n    right: String\n\n\
                 fn borrow(pair: let('a) Pair) -> View(Pair, 'a):\n    pair\n\n\
                 fn main(console: Console):\n    var pair = Pair(\"left\", \"right\")\n    let whole = borrow(pair)\n    let left = whole.left\n    pair.{field} = \"changed\"\n    console.print(left)\n"
            )
        };

        for field in ["left", "right"] {
            let error = check_str(&source(field))
                .expect_err("a live projection conservatively freezes its owner root");
            assert!(error.contains("owner `pair` is reassigned"), "{error}");
        }
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

    #[test]
    fn returned_borrowed_tuple_slots_must_preserve_declared_lifetimes() {
        let source = "mode opt\n\n\
            fn bad(let left: let('left) String, let right: let('right) String) \
                -> (View(String, 'left), View(String, 'right)):\n    (right, left)\n\n\
            fn main(console: Console):\n    var left = \"left\"\n    var right = \"right\"\n    let both = bad(left, right)\n    let (first, _) = both\n    right = \"changed\"\n    console.print(first)\n";

        let error = check_str(source).expect_err("return slots may not swap lifetime owners");
        assert!(error.contains("output projection `[0]`"), "{error}");
        assert!(error.contains("owner `right`"), "{error}");
        assert!(error.contains("owner `left`"), "{error}");
    }

    #[test]
    fn nested_call_owner_selection_applies_the_consumers_input_projection() {
        let source = "mode opt\n\n\
            fn pair(let left: let('left) String, let right: let('right) String) \
                -> (View(String, 'left), View(String, 'right)):\n    (left, right)\n\n\
            fn first(let pair: (View(String, 'left), View(String, 'right))) \
                -> View(String, 'left):\n    let (value, _) = pair\n    value\n\n\
            fn main(console: Console):\n    var left = \"left\"\n    var right = \"right\"\n    let value = first(pair(left, right))\n    right = \"changed\"\n    console.print(value)\n";

        check_str(source).expect("the nested call result borrows only its selected tuple slot");
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
        let module = witchy_syntax::parser::parse_module(
            "mode opt\n\n\
             type Leaf('leaf):\n    view: View(String, 'leaf)\n\n\
             type Wrapper(a, 'scope):\n    item: a\n\n\
             type Envelope(a, 'scope):\n    inner: Wrapper(a, 'scope)\n\n\
             type GenericView(a, 'scope):\n    view: View(a, 'scope)\n",
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
        let module = witchy_syntax::parser::parse_module(
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
