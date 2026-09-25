    use witchy_syntax::async_lower::*;
    use witchy_syntax::source_check::AsyncLoweredModule;
    #[allow(unused_imports)]
    use witchy_syntax::ast::*;

    fn lower_module(module: Module) -> Result<AsyncLoweredModule, String> {
        let checked = witchy_syntax::source_check::check(module).map_err(|error| error.to_string())?;
        let checked = witchy_syntax::generators::lower(checked)?;
        lower(checked)
    }

    #[test]
    fn lowering_rejects_value_returning_async_main() {
        let source = "async fn main(console: Console) -> Int:\n    1\n";
        let module = witchy_syntax::parser::parse_module(source).expect("parse async main");
        let error =
            lower_module(module).expect_err("the executor cannot surface an async root value");

        assert!(error.contains("async fn `main` returns `Int`"), "{error}");
        assert!(error.contains("Task(())"), "{error}");
    }

    #[test]
    fn lowering_rejects_a_borrowed_view_live_across_await() {
        let source = "mode opt\n\nfn view(text: &String) -> &String:\n    text\n\nasync fn bad(console: Console):\n    let text = \"x\"\n    let w = view(text)\n    let _ = task.done(0).await\n    console.print(w)\n";
        let module = witchy_syntax::parser::parse_module(source).expect("parse borrowed async body");
        let error = lower_module(module).expect_err("a view cannot be carried through a segment");
        assert!(
            error.contains("borrowed value `w` remains live across `await`"),
            "{error}"
        );
        assert!(
            error.contains("materialize a direct view with `.owned()`"),
            "{error}"
        );
    }

    #[test]
    fn lowering_rejects_lifetime_bearing_nominal_async_signatures() {
        let source = "mode opt\n\ntype Holder('a):\n    view: &'a String\n\nasync fn bad(input: (Holder('a), Int)):\n    task.done(0).await\n";
        let module = witchy_syntax::parser::parse_reference_migration(source).expect("parse borrowed nominal signature");
        let error = lower_module(module)
            .expect_err("a nested lifetime-bearing shell cannot enter an async task");

        assert!(error.contains("lifetime-bearing shell"), "{error}");
        assert!(error.contains("parameter or result"), "{error}");
    }

    #[test]
    fn lowering_rejects_an_unannotated_borrowed_nominal_live_across_await() {
        let source = "mode opt\n\ntype Holder('a):\n    view: &'a String\n\nasync fn bad():\n    let text = \"x\"\n    let holder = Holder(text)\n    let _ = task.done(0).await\n    let keep = holder\n";
        let module = witchy_syntax::parser::parse_reference_migration(source).expect("parse borrowed nominal local");
        let error = lower_module(module)
            .expect_err("a lifetime-bearing constructor result cannot cross suspension");

        assert!(
            error.contains("borrowed value `holder` remains live across `await`"),
            "{error}"
        );
        assert!(error.contains("before building the shell"), "{error}");
    }


