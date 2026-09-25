    use witchy_syntax::source_check::*;
    #[allow(unused_imports)]
    use witchy_syntax::ast::*;

    fn parse(source: &str) -> Module {
        witchy_syntax::parser::parse_module(source).expect("parse source-check fixture")
    }

    #[test]
    fn generator_region_error_precedes_destructive_lowering() {
        let module = parse("gen fn bad() -> Iter(Int):\n  region:\n    yield 1\n    0\n");
        let error = check(module).expect_err("source check must reject yield in region");
        assert!(
            error.message.contains("cannot `yield` inside `region:`"),
            "{error}"
        );
    }

