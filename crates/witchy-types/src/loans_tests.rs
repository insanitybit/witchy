    //! RFC-0083 phase-1 loan/lifetime checker tests. Each runs the full public
    //! checker (`check_str`) over a small `mode opt` program and asserts the loan
    //! rule accepts or rejects it with the documented diagnostic.

    use crate::typeck::check_str;

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
    }

    // --- escape rejection (acceptance 7) ------------------------------------

    #[test]
    fn view_captured_by_a_closure_is_rejected() {
        let err = check_str(&opt(
            "    var s = \"hi\"\n    let w = borrow(s)\n    let f = fn(): w\n    console.print(w)\n",
        ))
        .expect_err("a view captured by a closure escapes its owner");
        assert!(err.contains("escapes through a closure"), "{err}");
    }

    #[test]
    fn view_sent_through_a_channel_is_rejected() {
        // A `send(ch, w)` call moves the view out of this activation.
        let err = check_str(&opt(
            "    var s = \"hi\"\n    let w = borrow(s)\n    let _ = send(s, w)\n    console.print(w)\n",
        ))
        .expect_err("a view sent through a channel escapes its owner");
        assert!(err.contains("escapes through a closure, task, or channel"), "{err}");
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
