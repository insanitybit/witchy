    use super::*;
    use crate::ast::Expr;

    fn fold_expr(src: &str) -> Expr {
        // Parse `fn main(): <src>` and return the single statement's expression,
        // after the optimizer has run over the module.
        let program = format!("fn f():\n    {src}\n");
        let mut m = crate::parser::parse_module(&program).expect("parse");
        optimize(&mut m);
        let Item::Function(f) = &m.items[0] else {
            panic!("expected fn")
        };
        match &f.body.stmts[0] {
            Stmt::Expr(e) => e.clone(),
            other => panic!("expected expr stmt, got {other:?}"),
        }
    }

    /// Parse a multi-statement `fn f()` body (each line already a statement) and
    /// return the LAST statement's expression after optimization — for testing
    /// constant propagation across `let` bindings.
    fn last_expr(body: &str) -> Expr {
        let program = format!("fn f():\n{body}");
        let mut m = crate::parser::parse_module(&program).expect("parse");
        optimize(&mut m);
        let Item::Function(f) = &m.items[0] else {
            panic!("expected fn")
        };
        match f.body.stmts.last().expect("non-empty body") {
            Stmt::Expr(e) => e.clone(),
            other => panic!("expected expr stmt, got {other:?}"),
        }
    }

    #[test]
    fn does_not_propagate_mutable_or_loop_vars() {
        // A `var` may change, so it is never propagated.
        assert!(matches!(
            last_expr("    var n = 10\n    n + 1\n"),
            Expr::Binary { .. }
        ));
        // A loop variable shadows an outer constant: `x` inside the loop is the
        // element, not the outer `5`, so it must NOT be substituted/folded.
        assert!(matches!(
            last_expr("    let x = 5\n    for x in [1, 2]:\n        let z = x + 1\n    x\n"),
            Expr::Int(5) // the trailing `x` is the outer constant (correct)
        ));
    }

    #[test]
    fn does_not_fold_division_or_modulo() {
        // Left intact so a runtime ÷0 trap survives; the node stays a Binary.
        assert!(matches!(fold_expr("8 / 0"), Expr::Binary { .. }));
        assert!(matches!(fold_expr("8 % 3"), Expr::Binary { .. }));
    }
