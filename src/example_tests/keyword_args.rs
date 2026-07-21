use super::*;
use crate::{parser, typeck};

    /// (RFC-0056) Keyword arguments at a direct call site reorder to the callee's
    /// declared parameter order — resolved at the link layer, so both backends see
    /// the same positional call and agree. `label(n: 7, name: "ada")` binds `name`
    /// and `n` correctly despite the reversed written order.
    #[test]
    fn keyword_args_reorder_backends_agree() {
        let src = "fn label(name: String, n: Int) -> String:\n    \"${name}#${n}\"\n\nfn main(console: Console):\n    console.print(label(n: 7, name: \"ada\"))\n    console.print(label(\"bob\", n: 3))\n";
        let expected = ["ada#7", "bob#3"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (BUG-208, parity) A REORDERED labeled call whose reorder crosses a `var`
    /// parameter must still write back. The desugar temp-bound every reordered
    /// argument to an immutable `let __kwN`, so a `var` argument became ill-typed
    /// ("must be a mutable `var`") and leaked the synthetic `__kwN` into the error —
    /// legality depended on the order the labels were written. A `var` argument is a
    /// bare mutable variable with no evaluation effect, so it is now passed directly.
    #[test]
    fn keyword_args_var_reorder_writes_back() {
        // Reordered (`by:` before `xs:`) and in-order both mutate the caller's `var`.
        let reordered = "fn bump(var xs: List(Int), by: Int):\n    xs.push(by)\n    let _ = 0\n\nfn main(console: Console):\n    var xs: List(Int) = []\n    bump(by: 5, xs: xs)\n    bump(by: 7, xs: xs)\n    console.print(\"${xs}\")\n";
        assert_eq!(link_run(reordered), ["[5, 7]"], "interp reordered var write-back");
        assert_eq!(
            run_linked_on_wasm(&[("main", reordered)], "main"),
            ["[5, 7]"],
            "compiled reordered var write-back must agree",
        );
        // A reordered `own`/`move` argument still moves correctly (temp path intact).
        let owned = "fn eat(own s: String, n: Int) -> String:\n    s.repeat(n)\n\nfn main(console: Console):\n    let s = \"ab\"\n    console.print(eat(n: 3, s: move s))\n";
        assert_eq!(link_run(owned), ["ababab"], "interp reordered own/move");
        assert_eq!(
            run_linked_on_wasm(&[("main", owned)], "main"),
            ["ababab"],
            "compiled reordered own/move must agree",
        );
        // A genuinely non-mutable argument to a `var` param is still rejected — but
        // the diagnostic names the USER's variable, never a synthetic `__kwN` temp.
        let bad = "fn bump(var xs: List(Int), by: Int):\n    xs.push(by)\n    let _ = 0\n\nfn main(console: Console):\n    let ys: List(Int) = []\n    bump(by: 5, xs: ys)\n    console.print(\"${ys}\")\n";
        let module = parser::parse_module(bad).expect("parse");
        // `keyword_args::resolve` runs inside `pipeline::link`; the reorder now passes
        // the `var` argument directly, so typeck (not the desugar) reports the error.
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        let err = typeck::check(&linked)
            .expect_err("a `let` bound to a `var` param must be rejected")
            .message;
        assert!(err.contains("ys"), "diagnostic must name the user's variable: {err}");
        assert!(!err.contains("__kw"), "diagnostic must not leak a `__kwN` temp: {err}");
    }

    /// (RFC-0056) A labeled call evaluates its arguments in SOURCE order, not
    /// declared order: the desugar binds each written argument to a temp in the
    /// order written, then passes the temps in declared order. Here `b:` is written
    /// before `a:` but binds to the later parameter — the two effectful `side`
    /// calls must still print "first" before "second", identically on both backends.
    #[test]
    fn keyword_args_source_order_backends_agree() {
        let src = "fn record(console: Console, a: String, b: String) -> Nil:\n    console.print(\"a=${a} b=${b}\")\n\nfn side(console: Console, tag: String, ret: String) -> String:\n    console.print(\"eval ${tag}\")\n    ret\n\nfn main(console: Console):\n    record(console, b: side(console, \"first\", \"B\"), a: side(console, \"second\", \"A\"))\n";
        let expected = ["eval first", "eval second", "a=A b=B"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (RFC-0056) A closed-constant default parameter is spliced in for an omitted
    /// argument at a direct call site. `connect("h", tls: false)` keeps the default
    /// `port = 443`; `connect("h", 8080)` overrides it positionally. Both backends
    /// see the fully-applied positional call and agree.
    #[test]
    fn keyword_args_default_backends_agree() {
        let src = "fn connect(host: String, port: Int = 443, tls: Bool = true) -> String:\n    \"${host}:${port} tls=${tls}\"\n\nfn main(console: Console):\n    console.print(connect(\"example.com\"))\n    console.print(connect(\"h\", tls: false))\n    console.print(connect(\"h\", 8080))\n";
        let expected = ["example.com:443 tls=true", "h:443 tls=false", "h:8080 tls=true"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (BUG-211) Named-field record construction is the same closed-constant
    /// shape as positional construction when every field value is closed. It is
    /// valid as a default argument and lowers before either backend sees it.
    #[test]
    fn keyword_args_default_accepts_named_field_record_constructor() {
        let src = "type Pt:\n    x: Int\n    y: Int\n\nfn score(p: Pt = Pt(y: 2, x: 40)) -> Int:\n    p.x + p.y\n\nfn main(console: Console):\n    console.print(\"${score()}\")\n    console.print(\"${score(Pt(x: 1, y: 2))}\")\n";
        let expected = ["42", "3"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (RFC-0056) A `var` parameter cannot carry a default — there is no caller
    /// variable to write back to. Rejected loudly at parse time, identically for
    /// every consumer (both backends parse the same source).
    #[test]
    fn keyword_args_var_default_is_error() {
        let src = "fn inc(var n: Int = 0) -> Nil:\n    n = n + 1\n\nfn main(console: Console):\n    console.print(\"x\")\n";
        let err = parser::parse_module(src).expect_err("var + default must be rejected");
        assert!(
            format!("{err:?}").contains("`var` parameter cannot have a default"),
            "{err:?}"
        );
    }

    /// (RFC-0056 v1) Keyword labels are excluded on UFCS method calls — the method
    /// callee resolves later (by receiver type, in traits.rs), so labels have no
    /// declaration to bind against yet. Rejected at parse time.
    #[test]
    fn keyword_args_method_label_is_error() {
        let src = "fn main(console: Console):\n    let s = \"hello\"\n    console.print(s.substring(start: 1))\n";
        let err = parser::parse_module(src).expect_err("method-call label must be rejected");
        assert!(
            format!("{err:?}").contains("not supported on method calls"),
            "{err:?}"
        );
    }

    /// (RFC-0056) A missing argument with no default is a link error naming the
    /// unbound parameter (the same shape record construction already reports for a
    /// missing field).
    #[test]
    fn keyword_args_missing_argument_is_link_error() {
        let src = "fn f(a: Int, b: Int) -> Int:\n    a + b\n\nfn main(console: Console):\n    print_int(f(a: 1))\n";
        let module = parser::parse_module(src).expect("parse");
        let err = crate::pipeline::link(vec![("main".into(), module)], "main")
            .expect_err("missing argument must be a link error");
        assert!(format!("{err}").contains("missing argument `b`"), "{err}");
    }
