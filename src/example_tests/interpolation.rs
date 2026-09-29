use super::*;
use crate::typeck;

    /// (BUG-305, parity) `"${f}"` on a function value is rejected at CHECK time. The interpreter used to render
    /// `<function/N>` while the compiled backend rejected at codegen with a misleading
    /// "generic record such as `Set`" diagnostic (there was no Set). A function has no
    /// printable form; the message now names the function operand, never `Set`.
    #[test]
    fn interpolating_a_function_value_is_rejected_at_check_time() {
        let src = "fn main(console: Console):\n    let f = fn(n: Int): n + 1\n    console.print(\"${f}\")\n";
        let err = typeck::check_str(src)
            .expect_err("interpolating a function value must be a type error");
        assert!(err.contains("function"), "diagnostic must name the function operand: {err}");
        assert!(!err.contains("Set"), "diagnostic must not mention `Set` for a function operand: {err}");
        // Calling the function and interpolating the result still renders.
        let ok = "fn main(console: Console):\n    let f = fn(n: Int): n + 1\n    console.print(\"${f(41)}\")\n";
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
    /// when `show` is linked.
    #[test]
    fn rfc0053_interpolation_honors_custom_show() {
        let src = "import show\nimport duration\n\ntype P:\n    P(Int)\n\nimpl Show for P:\n    fn show(self) -> String:\n        match self:\n            P(n) -> \"P<${n}>\"\n\ntype Q derive(Show):\n    Q(Int)\n\nfn main(console: Console):\n    console.print(\"${P(5)}\")\n    console.print(\"${[P(1), P(2)]}\")\n    console.print(\"${90000ms}\")\n    console.print(\"${Q(7)}\")\n    console.print(\"${42}\")\n";
        // custom Show honored; container recurses; Duration -> human; primitive
        // derived Show remains constructor-shaped by its generated implementation.
        let expected = ["P<5>", "[P<1>, P<2>]", "1m30s", "Q(7)", "42"];
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: interpolation must honor custom Show",
        );
    }

