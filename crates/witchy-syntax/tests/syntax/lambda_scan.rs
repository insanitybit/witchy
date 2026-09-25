    use witchy_syntax::lambda_scan::*;
    #[allow(unused_imports)]
    use witchy_syntax::ast::*;

    fn outer_lambda(source: &str) -> (Vec<Param>, Block) {
        let module = witchy_syntax::parser::parse_module(source).expect("lambda scan fixture parses");
        let function = match module.items.into_iter().next().expect("one function") {
            Item::Function(function) => function,
            other => panic!("expected function, found {other:?}"),
        };
        let expression = match function.body.stmts.into_iter().last().expect("function tail") {
            Stmt::Expr(expression) => expression,
            other => panic!("expected expression tail, found {other:?}"),
        };
        let Expr::Lambda { params, body, .. } = expression else {
            panic!("expected lambda tail, found {expression:?}");
        };
        (params, body)
    }

    fn scan(source: &str) -> LambdaScan {
        let (params, body) = outer_lambda(source);
        scan_lambda(&params, &body)
    }

    #[test]
    fn read_before_later_shadow_remains_an_authority_capture() {
        let scanned = scan(
            "fn factory(console: Console) -> fn() -> ():\n    fn():\n        console.print(\"effect\")\n        let console = 0\n",
        );
        assert_eq!(scanned.captures(), ["console"]);
    }

