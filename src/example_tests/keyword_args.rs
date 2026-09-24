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

    /// (RFC-0056) A `var` parameter cannot carry a default — there is no caller
    /// variable to write back to. Rejected loudly at parse time, identically for
    /// every consumer (both backends parse the same source).
    #[test]
    fn keyword_args_var_default_is_error() {
        let src = "fn inc(var n: Int = 0) -> ():\n    n = n + 1\n\nfn main(console: Console):\n    console.print(\"x\")\n";
        let err = parser::parse_module(src).expect_err("var + default must be rejected");
        assert!(
            format!("{err:?}").contains("`var` parameter cannot have a default"),
            "{err:?}"
        );
    }

    /// (RFC-0113) Labeled method arguments still validate declaration names and
    /// arity after receiver resolution.
    #[test]
    fn keyword_args_method_label_invalid_binding_is_link_error() {
        let src = "type Boxy:\n    base: Int\n\nimpl Boxy:\n    fn sum(self, a: Int, b: Int) -> Int:\n        self.base + a + b\n\nfn main(console: Console):\n    let b = Boxy(10)\n    console.print(\"${b.sum(a: 5, x: 6)}\")\n";
        let module = parser::parse_module(src).expect("parse");
        let err = crate::pipeline::link(vec![("main".into(), module)], "main")
            .expect_err("unknown method label must fail link");
        assert!(format!("{err}").contains("has no parameter `x`"), "{err}");

        let src = "type Boxy:\n    base: Int\n\nimpl Boxy:\n    fn sum(self, a: Int, b: Int) -> Int:\n        self.base + a + b\n\nfn main(console: Console):\n    let b = Boxy(10)\n    console.print(\"${b.sum(a: 5)}\")\n";
        let module = parser::parse_module(src).expect("parse");
        let err = crate::pipeline::link(vec![("main".into(), module)], "main")
            .expect_err("missing method argument should fail link");
        assert!(format!("{err}").contains("missing argument `b`"), "{err}");
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
