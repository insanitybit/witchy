use super::*;
use crate::typeck;

    /// (BUG-305, parity) `"${f}"` on a function value is rejected at CHECK time, so
    /// BOTH backends refuse it identically. The interpreter used to render
    /// `<function/N>` while the compiled backend rejected at codegen with a misleading
    /// "generic record such as `Set`" diagnostic (there was no Set). A function has no
    /// printable form; the message now names the function operand, never `Set`.
    #[test]
    fn interpolating_a_function_value_is_rejected_on_both_backends() {
        let src = "fn main(console: Console):\n    let f = fn(n: Int): n + 1\n    console.print(\"${f}\")\n";
        let err = typeck::check_str(src)
            .expect_err("interpolating a function value must be a type error on both backends");
        assert!(err.contains("function"), "diagnostic must name the function operand: {err}");
        assert!(!err.contains("Set"), "diagnostic must not mention `Set` for a function operand: {err}");
        // Calling the function and interpolating the RESULT still renders on both.
        let ok = "fn main(console: Console):\n    let f = fn(n: Int): n + 1\n    console.print(\"${f(41)}\")\n";
        assert_eq!(link_run(ok), ["42"], "interp renders the call result");
        assert_eq!(
            run_linked_on_wasm(&[("main", ok)], "main"),
            ["42"],
            "compiled renders the call result",
        );
    }

    /// (RFC-0053, parity) Interpolation (`"${x}"`) honors a CUSTOM `Show` impl, exactly
    /// as `say` does — the typed lowering rewrites generated render to `show(x)` when
    /// x's type has a public `Show` model. Primitive-derived values may print the
    /// same bytes as the structural fallback, but they still share the `Show` path
    /// when `show` is linked. Both backends must agree byte-for-byte.
    #[test]
    fn rfc0053_interpolation_honors_custom_show_on_both_backends() {
        let src = "import show\nimport duration\n\ntype P:\n    P(Int)\n\nimpl Show for P:\n    fn show(self) -> String:\n        match self:\n            P(n) -> \"P<${n}>\"\n\ntype Q derive(Show):\n    Q(Int)\n\nfn main(console: Console):\n    console.print(\"${P(5)}\")\n    console.print(\"${[P(1), P(2)]}\")\n    console.print(\"${90000ms}\")\n    console.print(\"${Q(7)}\")\n    console.print(\"${42}\")\n";
        // custom Show honored; container recurses; Duration -> human; primitive
        // derived Show remains constructor-shaped by its generated implementation.
        let expected = ["P<5>", "[P<1>, P<2>]", "1m30s", "Q(7)", "42"];
        assert_eq!(link_run(src), expected, "interp: interpolation honors custom Show");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: interpolation must honor custom Show identically",
        );
    }

    /// RFC-0046 residual cleanup: constructor-pattern bindings retain their
    /// substituted generic field types. A bounded trait call may dispatch directly
    /// on `Some(value)`, `Ok(value)`, or `Err(error)` without routing the binding
    /// through a parameter/loop-shaped helper to recover its type.
    #[test]
    fn generic_match_bindings_dispatch_bounded_traits_on_both_backends() {
        let src = "import reflect\nimport show\nfrom reflect import Mirror\n\ntype P derive(Reflect):\n    value: Int\n\nimpl Show for P:\n    fn show(self) -> String:\n        \"<${self.value}>\"\n\nfn show_option(value: Option(a)) -> String where a: Show:\n    match value:\n        Some(inner) -> show(inner)\n        None -> \"none\"\n\nfn show_result(value: Result(a, e)) -> String where a: Show, e: Show:\n    match value:\n        Ok(inner) -> show(inner)\n        Err(error) -> show(error)\n\nfn reflect_option(value: Option(a)) -> Mirror where a: Reflect:\n    match value:\n        Some(inner) -> MVariant(\"Option\", \"Some\", [reflect(inner)])\n        None -> MVariant(\"Option\", \"None\", [])\n\nfn reflect_result(value: Result(a, e)) -> Mirror where a: Reflect, e: Reflect:\n    match value:\n        Ok(inner) -> MVariant(\"Result\", \"Ok\", [reflect(inner)])\n        Err(error) -> MVariant(\"Result\", \"Err\", [reflect(error)])\n\nfn describe(value: Mirror) -> String:\n    match value:\n        MVariant(owner, variant, payload) -> \"${owner}.${variant}:${list.length(payload)}\"\n        _ -> \"other\"\n\nfn main(console: Console):\n    let ok: Result(P, P) = Ok(P(2))\n    let err: Result(P, P) = Err(P(3))\n    let nested_ok: Result(List(P), List(P)) = Ok([P(5)])\n    let nested_err: Result(List(P), List(P)) = Err([P(7)])\n    console.print(show_option(Some(P(1))))\n    console.print(show_result(ok))\n    console.print(show_result(err))\n    console.print(show_option(Some([P(4)])))\n    console.print(show_result(nested_ok))\n    console.print(describe(reflect_option(Some(P(4)))))\n    console.print(describe(reflect_result(err)))\n    console.print(describe(reflect_option(Some([P(6)]))))\n    console.print(describe(reflect_result(nested_err)))\n    console.print(show.render(Some(P(8))))\n    let std_ok: Result(P, P) = Ok(P(9))\n    let std_err: Result(P, P) = Err(P(10))\n    console.print(show.render(std_ok))\n    console.print(show.render(std_err))\n    console.print(describe(reflect.reflect_option(Some(P(11)))))\n    let std_reflect: Result(P, P) = Err(P(12))\n    console.print(describe(reflect.reflect_result(std_reflect)))\n";
        let expected = [
            "<1>",
            "<2>",
            "<3>",
            "[<4>]",
            "[<5>]",
            "Option.Some:1",
            "Result.Err:1",
            "Option.Some:1",
            "Result.Err:1",
            "Some(<8>)",
            "Ok(<9>)",
            "Err(<10>)",
            "Option.Some:1",
            "Result.Err:1",
        ];
        assert_eq!(link_run(src), expected, "interp: bounded dispatch on generic match bindings");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: bounded dispatch on generic match bindings",
        );
    }

    /// (RFC-0053, D5) f-strings are not a second rendering mechanism. They lower
    /// to the same interpolation path, so they honor `Show` for custom values,
    /// containers, and std domain/scalar display.
    #[test]
    fn rfc0053_f_strings_honor_show_on_both_backends() {
        let src = "import show\nimport duration\n\ntype P:\n    P(Int)\n\nimpl Show for P:\n    fn show(self) -> String:\n        match self:\n            P(n) -> \"P<${n}>\"\n\nfn main(console: Console):\n    console.print(f\"p={P(5)} xs={[P(1), P(2)]} d={90000ms}\")\n";
        let expected = ["p=P<5> xs=[P<1>, P<2>] d=1m30s"];
        assert_eq!(link_run(src), expected, "interp: f-strings honor Show");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: f-strings honor Show",
        );
    }

    /// RFC-0053's interpolation flip is import-independent. `show` is preluded,
    /// so interpolation, `show.render`, and `show.say` agree on Duration's human
    /// form with or without a redundant explicit import.
    #[test]
    fn rfc0053_duration_interpolation_is_import_independent_on_both_backends() {
        let without_import = "fn main(console: Console):\n    console.print(\"${90000ms}\")\n";
        let expected = ["1m30s"];
        assert_eq!(link_run(without_import), expected, "interp: prelude Duration interpolation");
        assert_eq!(
            run_linked_on_wasm(&[("main", without_import)], "main"),
            expected,
            "compiled: prelude Duration interpolation",
        );

        let with_show = "import show\nimport duration\n\nfn main(console: Console):\n    console.print(\"${90000ms}\")\n    console.print(show.render(90000ms))\n    show.say(console, 90000ms)\n";
        let show_expected = ["1m30s", "1m30s", "1m30s"];
        assert_eq!(link_run(with_show), show_expected, "interp: Duration interpolation honors Show");
        assert_eq!(
            run_linked_on_wasm(&[("main", with_show)], "main"),
            show_expected,
            "compiled: Duration interpolation honors Show",
        );
    }

    /// (RFC-0053, coherence) Generic container `Show` impls are part of the same
    /// rendering model as concrete custom impls. In particular, `Set(Int)` has a
    /// structural fallback (`Set([1, 2])`) but a public display form (`{1, 2}`), so
    /// interpolation, `show.render`, and `show.say` must always agree.
    #[test]
    fn rfc0053_interpolation_matches_show_for_generic_containers_on_both_backends() {
        let with_show = "import set\nimport show\n\ntype P:\n    P(Int)\n\nimpl Show for P:\n    fn show(self) -> String:\n        match self:\n            P(n) -> \"P<${n}>\"\n\nfn main(console: Console):\n    let s = set.from_list([1, 1, 2, 3])\n    console.print(\"${s}\")\n    console.print(show.render(s))\n    show.say(console, s)\n    console.print(\"${[s]}\")\n    let ps = [P(1), P(2)]\n    console.print(\"${ps}\")\n    console.print(show.render(ps))\n";
        let expected = [
            "{1, 2, 3}",
            "{1, 2, 3}",
            "{1, 2, 3}",
            "[{1, 2, 3}]",
            "[P<1>, P<2>]",
            "[P<1>, P<2>]",
        ];
        assert_eq!(link_run(with_show), expected, "interp: interpolation matches show.render/say");
        assert_eq!(
            run_linked_on_wasm(&[("main", with_show)], "main"),
            expected,
            "compiled: interpolation matches show.render/say",
        );

        let no_import = "import set\n\nfn main(console: Console):\n    let s = set.from_list([1, 1, 2, 3])\n    console.print(\"${s}\")\n";
        let public_display = ["{1, 2, 3}"];
        assert_eq!(link_run(no_import), public_display, "interp: prelude Show renders Set");
        assert_eq!(
            run_linked_on_wasm(&[("main", no_import)], "main"),
            public_display,
            "compiled: prelude Show renders Set",
        );
    }

    /// (RFC-0053, coherence) `derive(Show)` is not a second rendering protocol.
    /// Its generated body renders fields through `Show`, so interpolation must
    /// agree with `show.say` for derived values containing custom-Show fields and
    /// for containers of those derived values.
    #[test]
    fn rfc0053_derived_show_fields_use_show_in_interpolation_on_both_backends() {
        let src = "import show\n\ntype Label:\n    Label(String)\n\nimpl Show for Label:\n    fn show(self) -> String:\n        match self:\n            Label(s) -> \"<\" + s + \">\"\n\ntype Box derive(Show):\n    label: Label\n\nfn main(console: Console):\n    let b = Box(Label(\"x\"))\n    console.print(\"${Label(\"x\")}\")\n    show.say(console, Label(\"x\"))\n    console.print(\"${b}\")\n    show.say(console, b)\n    console.print(\"${[b]}\")\n    show.say(console, [b])\n";
        let expected = ["<x>", "<x>", "Box(<x>)", "Box(<x>)", "[Box(<x>)]", "[Box(<x>)]"];
        assert_eq!(
            link_run(src),
            expected,
            "interp: derived Show fields must use field Show impls",
        );
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: derived Show fields must use field Show impls",
        );
    }
