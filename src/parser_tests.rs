    use super::*;

    fn fn_body(src: &str) -> Vec<Stmt> {
        let m = parse_module(src).expect("should parse");
        match &m.items[0] {
            Item::Function(f) => f.body.stmts.clone(),
            _ => panic!("expected a function"),
        }
    }

    #[test]
    fn parses_function_with_params_and_return() {
        let m = parse_module(r#"
fn add(a: Int, b: Int) -> Int:
    (a + b)
"#).unwrap();
        let Item::Function(f) = &m.items[0] else {
            panic!("expected a function");
        };
        assert_eq!(f.name, "add");
        assert_eq!(f.params.len(), 2);
        assert_eq!(f.ret, Some(Type::Named("Int".into(), vec![])));
    }

    #[test]
    fn impl_trait_param_desugars_to_a_bound() {
        // `fn f(x: impl Show)` becomes a fresh type-var param plus a `Show` bound,
        // so it reuses the whole generics path; two `impl` params get distinct vars.
        let m = parse_module("fn f(x: impl Show, y: impl Ord) -> Int:\n    0\n").unwrap();
        let Item::Function(f) = &m.items[0] else { panic!("expected a function") };
        // Distinct synthetic type vars, in order.
        let p0 = match &f.params[0].ty {
            Some(Type::Named(v, a)) if a.is_empty() => v.clone(),
            other => panic!("expected a type var, got {other:?}"),
        };
        let p1 = match &f.params[1].ty {
            Some(Type::Named(v, a)) if a.is_empty() => v.clone(),
            other => panic!("expected a type var, got {other:?}"),
        };
        assert_ne!(p0, p1, "each impl-Trait param gets its own type variable");
        assert!(f.bounds.contains(&(p0, "Show".to_string(), Vec::new())));
        assert!(f.bounds.contains(&(p1, "Ord".to_string(), Vec::new())));
        // It coexists with an explicit `where`.
        let m2 = parse_module("fn g(x: impl Show, y: a) -> Int where a: Ord:\n    0\n").unwrap();
        let Item::Function(g) = &m2.items[0] else { panic!() };
        assert!(g.bounds.iter().any(|(_, t, _)| t == "Show"));
        assert!(g.bounds.contains(&("a".to_string(), "Ord".to_string(), Vec::new())));
    }

    #[test]
    fn parses_retain_and_without_firewalls() {
        // `without a, b:` and `retain a:` open an ordinary block carrying a
        // `CapRestrict` on `Block.restrict`.
        let stmts = fn_body(
            "fn main(console: Console, clock: Clock):\n    without clock:\n        print(console, \"x\")\n",
        );
        let Stmt::Expr(Expr::Block(b)) = &stmts[0] else {
            panic!("expected a block statement, got {:?}", stmts[0]);
        };
        assert_eq!(
            b.restrict,
            Some(CapRestrict { mode: RestrictMode::Without, names: vec!["clock".into()] })
        );

        let stmts = fn_body(
            "fn main(console: Console, clock: Clock):\n    retain console, clock:\n        print(console, \"x\")\n",
        );
        let Stmt::Expr(Expr::Block(b)) = &stmts[0] else {
            panic!("expected a block statement");
        };
        assert_eq!(
            b.restrict,
            Some(CapRestrict {
                mode: RestrictMode::Retain,
                names: vec!["console".into(), "clock".into()],
            })
        );

        // `retain:` with no names parses to an empty name list (a full sandbox).
        let stmts =
            fn_body("fn main(console: Console):\n    retain:\n        print(console, \"x\")\n");
        let Stmt::Expr(Expr::Block(b)) = &stmts[0] else {
            panic!("expected a block statement");
        };
        assert_eq!(
            b.restrict,
            Some(CapRestrict { mode: RestrictMode::Retain, names: vec![] })
        );
    }

    #[test]
    fn type_def_accepts_explicit_type_parameters() {
        // The conventional `type Name(a, b):` form parses; the parameter names are
        // accepted for clarity and the checker infers them from the field types.
        let m = parse_module(
            r#"
type Pair(a, b):
    Pair(a, b)
"#,
        )
        .expect("explicit type params should parse");
        let Item::Type(td) = &m.items[0] else {
            panic!("expected a type definition");
        };
        assert_eq!(td.name, "Pair");
        assert_eq!(td.variants.len(), 1);
        assert_eq!(td.variants[0].name, "Pair");
        assert_eq!(td.variants[0].fields.len(), 2);
    }

    #[test]
    fn if_let_desugars_to_match() {
        // `if let PAT = e: ... else: ...` becomes a two-arm match: the pattern
        // arm and a wildcard fallback carrying the else block.
        let stmts = fn_body(
            r#"
fn f(o: Option(Int)) -> Int:
    if let Some(x) = o:
        x
    else:
        0
"#,
        );
        let Stmt::Expr(Expr::Match { arms, .. }) = &stmts[0] else {
            panic!("if let should desugar to a match, got {:?}", stmts[0]);
        };
        assert_eq!(arms.len(), 2);
        assert!(matches!(arms[0].pattern, Pattern::Ctor { .. }));
        assert_eq!(arms[1].pattern, Pattern::Wildcard);
    }

    #[test]
    fn while_let_parses_to_node_and_lowers_to_while_true_match() {
        // `while let PAT = e: body` parses to `Expr::WhileLet` (kept for the
        // formatter) and lowers to `while true` over a match whose wildcard arm
        // breaks the loop.
        let stmts = fn_body(
            r#"
fn f(o: Option(Int)):
    while let Some(x) = o:
        o = None
"#,
        );
        let Stmt::Expr(Expr::WhileLet { pattern, scrutinee, body }) = &stmts[0] else {
            panic!("expected a WhileLet node, got {:?}", stmts[0]);
        };
        assert!(matches!(pattern, Pattern::Ctor { .. }));
        assert_eq!(**scrutinee, Expr::Var("o".into()));
        // Lowering produces the `while true` / match / break form.
        let lowered = desugar_while_let(pattern.clone(), (**scrutinee).clone(), body.clone());
        let Expr::While { cond, body } = &lowered else {
            panic!("while let should lower to a while loop");
        };
        assert_eq!(**cond, Expr::Bool(true));
        let Stmt::Expr(Expr::Match { arms, .. }) = &body.stmts[0] else {
            panic!("while let body should be a match");
        };
        assert_eq!(arms.len(), 2);
        assert_eq!(arms[1].pattern, Pattern::Wildcard);
        let Expr::Block(b) = &arms[1].body else {
            panic!("wildcard arm should be a block");
        };
        assert_eq!(b.stmts, vec![Stmt::Break]);
    }

    #[test]
    fn range_pattern_desugars_to_guarded_binding() {
        // `lo..hi` becomes a fresh binding guarded by `>= lo && < hi`; `..=`
        // uses `<=` for the upper bound.
        let stmts = fn_body(
            r#"
fn f(n: Int) -> Int:
    match n:
        1..=3 -> 0
        _ -> 1
"#,
        );
        let Stmt::Expr(Expr::Match { arms, .. }) = &stmts[0] else {
            panic!("expected a match");
        };
        // First arm: a fresh var bound, with an inclusive bounds guard.
        assert!(matches!(arms[0].pattern, Pattern::Var(_)));
        let Some(Expr::Binary { op: BinOp::And, lhs, rhs }) = &arms[0].guard else {
            panic!("range arm should carry an `&&` bounds guard");
        };
        assert!(matches!(**lhs, Expr::Binary { op: BinOp::GtEq, .. }));
        assert!(matches!(**rhs, Expr::Binary { op: BinOp::LtEq, .. }));
        // The wildcard arm is untouched.
        assert_eq!(arms[1].pattern, Pattern::Wildcard);
        assert!(arms[1].guard.is_none());
    }

    #[test]
    fn subscript_parses_to_index_and_lowers_to_at_call() {
        // `xs[i]` parses to `Expr::Index` (kept for the formatter) and lowers to
        // `list.at(xs, i)`; `grid[r][c]` nests.
        let stmts = fn_body(
            r#"
fn f(xs: List(Int)) -> Int:
    xs[2]
"#,
        );
        let Stmt::Expr(Expr::Index { base, index }) = &stmts[0] else {
            panic!("expected an Index node, got {:?}", stmts[0]);
        };
        assert_eq!(**base, Expr::Var("xs".into()));
        assert_eq!(**index, Expr::Int(2));
        // Lowering turns it into the `at` call the rest of the pipeline expects.
        let lowered = desugar_index((**base).clone(), (**index).clone());
        assert_eq!(
            lowered,
            Expr::Call {
                name: "list.at".into(),
                args: vec![Expr::Var("xs".into()), Expr::Int(2)],
            }
        );
        // `grid[0][1]` nests an Index inside an Index.
        let nested = fn_body(
            r#"
fn g(grid: List(List(Int))) -> Int:
    grid[0][1]
"#,
        );
        let Stmt::Expr(Expr::Index { base, .. }) = &nested[0] else {
            panic!("expected an Index node");
        };
        assert!(matches!(&**base, Expr::Index { .. }));
    }

    #[test]
    fn top_level_let_parses_as_const() {
        let m = parse_module(
            r#"
let MAX = 100
"#,
        )
        .expect("top-level let should parse");
        match &m.items[0] {
            Item::Const { name, value } => {
                assert_eq!(name, "MAX");
                assert_eq!(*value, Expr::Int(100));
            }
            other => panic!("expected a const item, got {other:?}"),
        }
    }

    #[test]
    fn type_alias_parses() {
        let m = parse_module(
            r#"
type Id = Int
"#,
        )
        .expect("type alias should parse");
        match &m.items[0] {
            Item::TypeAlias { name, ty } => {
                assert_eq!(name, "Id");
                assert_eq!(*ty, Type::Named("Int".into(), vec![]));
            }
            other => panic!("expected a type alias, got {other:?}"),
        }
    }

    #[test]
    fn compound_assignment_desugars() {
        // `x += 2` becomes `x = x + 2`.
        let stmts = fn_body(r#"
fn f():
    var x = 1
    x = (x + 2)
"#);
        assert_eq!(
            stmts[1],
            Stmt::Assign {
                name: "x".into(),
                value: Expr::Binary {
                    op: BinOp::Add,
                    lhs: Box::new(Expr::Var("x".into())),
                    rhs: Box::new(Expr::Int(2)),
                },
            }
        );
    }

    #[test]
    fn or_patterns_desugar_to_one_arm_per_alternative() {
        // `1 | 2 | 3 -> body` becomes three arms sharing the body.
        let stmts = fn_body(r#"
fn f(n: Int) -> Int:
    match n:
        1 -> 0
        2 -> 0
        3 -> 0
        _ -> 1
"#);
        let Stmt::Expr(Expr::Match { arms, .. }) = &stmts[0] else {
            panic!("expected a match");
        };
        // 1, 2, 3, _  => 4 arms
        assert_eq!(arms.len(), 4);
        assert_eq!(arms[0].pattern, Pattern::Int(1));
        assert_eq!(arms[1].pattern, Pattern::Int(2));
        assert_eq!(arms[2].pattern, Pattern::Int(3));
        assert_eq!(arms[3].pattern, Pattern::Wildcard);
        // The shared body is duplicated to each alternative.
        assert_eq!(arms[0].body, arms[1].body);
        assert_eq!(arms[1].body, arms[2].body);
    }

    #[test]
    fn respects_operator_precedence() {
        let stmts = fn_body(r#"
fn f():
    (1 + (2 * 3))
"#);
        assert_eq!(
            stmts,
            vec![Stmt::Expr(Expr::Binary {
                op: BinOp::Add,
                lhs: Box::new(Expr::Int(1)),
                rhs: Box::new(Expr::Binary {
                    op: BinOp::Mul,
                    lhs: Box::new(Expr::Int(2)),
                    rhs: Box::new(Expr::Int(3)),
                }),
            })]
        );
    }

    #[test]
    fn desugars_pipeline_into_first_argument() {
        let stmts = fn_body(r#"
fn f(x: Int):
    add(double(x), 1)
"#);
        // x |> double() |> add(1)  ==  add(double(x), 1)
        assert_eq!(
            stmts,
            vec![Stmt::Expr(Expr::Call {
                name: "add".into(),
                args: vec![
                    Expr::Call {
                        name: "double".into(),
                        args: vec![Expr::Var("x".into())],
                    },
                    Expr::Int(1),
                ],
            })]
        );
    }

    #[test]
    fn parses_constructors_vs_calls_by_case() {
        let stmts = fn_body(r#"
fn f():
    Click(1, foo())
"#);
        assert_eq!(
            stmts,
            vec![Stmt::Expr(Expr::Ctor {
                name: "Click".into(),
                args: vec![
                    Expr::Int(1),
                    Expr::Call { name: "foo".into(), args: vec![] },
                ],
            })]
        );
    }

    #[test]
    fn parses_match_with_guard_and_ctor_patterns() {
        let src = r#"
fn describe(e: Event) -> String:
    match e:
        Click(x, _) if (x > 0) -> "right"
        Closed -> "bye"
        _ -> "other"
"#;
        let stmts = fn_body(src);
        let Stmt::Expr(Expr::Match { arms, .. }) = &stmts[0] else {
            panic!("expected a match expression");
        };
        assert_eq!(arms.len(), 3);
        assert!(matches!(arms[0].pattern, Pattern::Ctor { .. }));
        assert!(arms[0].guard.is_some());
        assert!(matches!(arms[2].pattern, Pattern::Wildcard));
    }

    #[test]
    fn tuple_pattern_after_ident_body_parses() {
        // A bare-identifier arm body must not swallow the next arm's `(..)`.
        let stmts = fn_body(
            r#"
fn f(p: (Int, Int)) -> Int:
    match p:
        (a, b) -> a
        (x, y) -> y
"#,
        );
        let Stmt::Expr(Expr::Match { arms, .. }) = &stmts[0] else {
            panic!("expected a match expression");
        };
        assert_eq!(arms.len(), 2);
        assert!(matches!(arms[0].pattern, Pattern::Tuple(_)));
        assert!(matches!(arms[0].body, Expr::Var(_)));
    }

    #[test]
    fn parses_negative_patterns_across_newlines() {
        // The `-2` on the next line is a pattern, not `0 - 2` continuing arm 1.
        let stmts = fn_body(r#"
fn f(n: Int) -> Int:
    match n:
        -1 -> 0
        -2 -> 0
        _ -> 1
"#);
        let Stmt::Expr(Expr::Match { arms, .. }) = &stmts[0] else {
            panic!("expected a match expression");
        };
        assert_eq!(arms.len(), 3);
        assert_eq!(arms[0].pattern, Pattern::Int(-1));
        assert_eq!(arms[1].pattern, Pattern::Int(-2));
    }

    #[test]
    fn subtraction_in_an_arm_body_still_parses() {
        // A `-` on the *same* line is ordinary subtraction.
        let stmts = fn_body(r#"
fn f(n: Int) -> Int:
    match n:
        0 -> (n - 1)
        _ -> n
"#);
        let Stmt::Expr(Expr::Match { arms, .. }) = &stmts[0] else {
            panic!("expected a match expression");
        };
        assert!(matches!(arms[0].body, Expr::Binary { op: BinOp::Sub, .. }));
    }

    #[test]
    fn reports_friendly_error_with_location() {
        let err = parse_module("fn f( {").unwrap_err();
        assert!(err.line >= 1);
        assert!(!err.message.is_empty());
    }

    #[test]
    fn parses_parameter_conventions() {
        let m = parse_module(r#"
fn f(var a: Int, own b: Int, c: Int) -> Int:
    c
"#).unwrap();
        let Item::Function(func) = &m.items[0] else {
            panic!("expected a function");
        };
        assert_eq!(func.params[0].convention, Convention::Var);
        assert_eq!(func.params[1].convention, Convention::Sink);
        assert_eq!(func.params[2].convention, Convention::Let);
    }

    #[test]
    fn inout_sink_keyword_aliases_are_removed() {
        // `inout`/`sink` were Hylo-style alias spellings for `var`/`own`; they
        // were removed, so they now lex as ordinary identifiers and a parameter
        // written with them no longer parses as a convention.
        assert!(parse_module("fn f(inout a: Int):\n    a\n").is_err());
        assert!(parse_module("fn f(sink a: Int):\n    a\n").is_err());
    }

