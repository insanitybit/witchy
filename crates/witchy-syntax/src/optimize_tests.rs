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
    fn propagates_immutable_let_constants() {
        // `let n = 10` propagates into `n * n`, which then folds to 100.
        assert_eq!(last_expr("    let n = 10\n    n * n\n"), Expr::Int(100));
        // A `let`-bound string literal propagates and the concat folds away
        // (the allocation Cranelift cannot see is eliminated at compile time).
        assert_eq!(
            last_expr("    let g = \"Hi, \"\n    g + \"World\"\n"),
            Expr::Str("Hi, World".into())
        );
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
    fn folds_integer_arithmetic_with_wrapping() {
        assert_eq!(fold_expr("2 + 3 * 4"), Expr::Int(14));
        assert_eq!(fold_expr("(1 + 2) + 3"), Expr::Int(6));
        assert_eq!(fold_expr("10 - 4 - 1"), Expr::Int(5));
        // Bitwise ops fold; shifts and division do not.
        assert_eq!(fold_expr("12 & 10"), Expr::Int(8));
        assert_eq!(fold_expr("12 | 1"), Expr::Int(13));
        assert_eq!(fold_expr("6 ^ 3"), Expr::Int(5));
        // Wrapping overflow matches the runtime's two's-complement semantics.
        assert_eq!(
            fold_expr("9223372036854775807 + 1"),
            Expr::Int(i64::MIN)
        );
    }

    #[test]
    fn folds_comparisons_and_booleans() {
        assert_eq!(fold_expr("2 > 1"), Expr::Bool(true));
        assert_eq!(fold_expr("3 == 4"), Expr::Bool(false));
        assert_eq!(fold_expr("true && false"), Expr::Bool(false));
        assert_eq!(fold_expr("false || true"), Expr::Bool(true));
        assert_eq!(fold_expr("1 < 2 && 4 > 3"), Expr::Bool(true));
    }

    #[test]
    fn folds_unary_literals() {
        assert_eq!(fold_expr("-(3 + 4)"), Expr::Int(-7));
        assert_eq!(fold_expr("!(2 > 5)"), Expr::Bool(true));
        assert_eq!(fold_expr("- -5"), Expr::Int(5));
    }

    #[test]
    fn folds_string_concatenation() {
        assert_eq!(fold_expr("\"a\" + \"b\""), Expr::Str("ab".into()));
        assert_eq!(fold_expr("\"x\" == \"x\""), Expr::Bool(true));
    }

    #[test]
    fn does_not_fold_division_or_modulo() {
        // Left intact so a runtime ÷0 trap survives; the node stays a Binary.
        assert!(matches!(fold_expr("8 / 0"), Expr::Binary { .. }));
        assert!(matches!(fold_expr("8 % 3"), Expr::Binary { .. }));
    }

    #[test]
    fn leaves_non_constant_expressions_alone() {
        // A variable operand is not folded (its type is unknown pre-typeck).
        assert!(matches!(fold_expr("n + 0"), Expr::Binary { .. }));
    }
