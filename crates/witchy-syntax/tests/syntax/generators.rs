    use witchy_syntax::generators::*;
    #[allow(unused_imports)]
    use witchy_syntax::ast::*;

    #[test]
    fn residual_suspension_cfg_fails_closed_instead_of_replaying() {
        let module = witchy_syntax::parser::parse_module(
            "gen fn grid() -> Iter(Int):\n    var outer = 0\n    while outer < 2:\n        var inner = 0\n        while inner < 2:\n            yield outer * 10 + inner\n            inner = inner + 1\n        outer = outer + 1\n",
        )
        .expect("parse nested yielding loops");
        let checked = witchy_syntax::source_check::check(module).expect("source check");
        let error = lower(checked).expect_err("nested yielding loops need an owned transition");
        assert!(error.contains("gen fn `grid` at line 1"), "source identity: {error}");
        assert!(error.contains("owned resumable frame"), "missing frame contract: {error}");
        assert!(!error.contains("from_gen"), "diagnostic must not advertise replay: {error}");
    }

