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
    fn grantable_capability_marker_parses() {
        // `grantable capability X:` (RFC-0038) is a sealed capability flagged
        // grantable; an ordinary `capability`/`type` is not.
        let m = parse_module("grantable capability UiRoot:\n    policy: String\n").unwrap();
        let Item::Type(t) = &m.items[0] else { panic!("expected a type") };
        assert_eq!(t.name, "UiRoot");
        assert!(t.sealed, "capability record is sealed");
        assert!(t.grantable, "the `grantable` marker is recorded");

        // An ordinary capability record is sealed but not grantable.
        let m2 = parse_module("capability Session:\n    token: String\n").unwrap();
        let Item::Type(t2) = &m2.items[0] else { panic!("expected a type") };
        assert!(t2.sealed && !t2.grantable, "an ordinary capability is not grantable");

        // `grantable` before anything but `capability` is an error.
        assert!(parse_module("grantable type Foo:\n    Foo\n").is_err());
    }

    #[test]
    fn sealed_type_marker_parses() {
        // `sealed type X:` (RFC-0065) sets the same `sealed` flag a `capability`
        // sets, but is NOT a capability (so it renders as `sealed type`, not
        // `capability`, and reports a "sealed type" diagnostic).
        let m = parse_module("sealed type Box(a):\n    BoxData(a)\n").unwrap();
        let Item::Type(t) = &m.items[0] else { panic!("expected a type") };
        assert_eq!(t.name, "Box");
        assert!(t.sealed, "`sealed type` is sealed");
        assert!(!t.is_capability, "`sealed type` is not a capability");
        assert!(!t.grantable, "`sealed type` is not grantable");
        assert_eq!(t.variants[0].name, "BoxData");

        // An ordinary `type` is neither sealed nor a capability.
        let m2 = parse_module("type Plain(a):\n    Plain(a)\n").unwrap();
        let Item::Type(t2) = &m2.items[0] else { panic!("expected a type") };
        assert!(!t2.sealed && !t2.is_capability, "an ordinary type is not sealed");

        // A `capability` record is sealed AND a capability.
        let m3 = parse_module("capability Session:\n    token: String\n").unwrap();
        let Item::Type(t3) = &m3.items[0] else { panic!("expected a type") };
        assert!(t3.sealed && t3.is_capability, "a capability is a sealed capability");

        // `sealed` before anything but `type` is an error.
        assert!(parse_module("sealed fn foo():\n    1\n").is_err());
    }

    #[test]
    fn pub_only_marks_function_declarations() {
        for src in [
            "pub let ANSWER = 42\n",
            "pub type Box:\n    Box(Int)\n",
            "pub sealed type Box:\n    Box(Int)\n",
            "pub capability Token:\n    label: String\n",
            "pub grantable capability Token:\n    label: String\n",
            "pub trait Label:\n    fn label(self) -> String\n",
            "pub impl Box:\n    pub fn new(value: Int) -> Box:\n        Box(value)\n",
        ] {
            let err = parse_module(src).expect_err("non-function `pub` must be rejected");
            assert!(err.message.contains("`pub` may only precede a function"), "{err}");
        }

        parse_module("pub fn id(x: Int) -> Int:\n    x\n").expect("public function parses");
        parse_module("pub async fn fetch() -> String:\n    \"ok\"\n").expect("public async function parses");
        parse_module("pub gen fn one() -> Iter(Int):\n    yield 1\n").expect("public generator parses");
        parse_module(
            "type Box:\n    Box(Int)\nimpl Box:\n    pub fn new(value: Int) -> Box:\n        Box(value)\n",
        )
        .expect("public impl methods still parse");
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
    fn anonymous_union_type_syntax_is_tag_shaped() {
        parse_module("type LoadErr = .[NotFound | BadPort(Int)]\n").expect("union type parses");

        let lower = parse_module("type Bad = .[notFound]\n").expect_err("lowercase tag rejected");
        assert!(lower.message.contains("must start with an uppercase"), "{lower}");

        let dup = parse_module("type Bad = .[Missing | Missing(String)]\n")
            .expect_err("duplicate tag rejected");
        assert!(dup.message.contains("listed more than once"), "{dup}");

        let empty_payload = parse_module("type Bad = .[Empty()]\n")
            .expect_err("empty payload parens rejected");
        assert!(empty_payload.message.contains("empty payload parens"), "{empty_payload}");
    }

    #[test]
    fn anonymous_union_injection_syntax_is_tag_shaped() {
        let stmts = fn_body(
            r#"
fn f() -> .[BadPort(Int) | NotFound]:
    .BadPort(70000)
"#,
        );
        let Stmt::Expr(Expr::AnonCtor { tag, args }) = &stmts[0] else {
            panic!("expected anonymous union injection, got {:?}", stmts[0]);
        };
        assert_eq!(tag, "BadPort");
        assert_eq!(args.as_slice(), [Expr::Int(70000)]);

        let nullary = fn_body(
            r#"
fn f() -> .[BadPort(Int) | NotFound]:
    .NotFound
"#,
        );
        let Stmt::Expr(Expr::AnonCtor { tag, args }) = &nullary[0] else {
            panic!("expected nullary anonymous union injection, got {:?}", nullary[0]);
        };
        assert_eq!(tag, "NotFound");
        assert!(args.is_empty());

        let lower = parse_module("fn f():\n    .bad\n").expect_err("lowercase tag rejected");
        assert!(lower.message.contains("must start with an uppercase"), "{lower}");

        let labeled = parse_module("fn f():\n    .Bad(label: 1)\n")
            .expect_err("labeled payload rejected");
        assert!(labeled.message.contains("takes positional payloads"), "{labeled}");
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
    fn range_pattern_parses_to_real_int_range_node() {
        // (RFC-0052) `lo..hi` / `lo..=hi` parse to a real `Pattern::IntRange`
        // node — no longer a fresh binding + synthesized bounds guard — so
        // exhaustiveness can reason about them and they nest anywhere.
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
        assert!(
            matches!(arms[0].pattern, Pattern::IntRange { lo: 1, hi: 3, inclusive: true }),
            "range arm should be an inclusive IntRange node, got {:?}",
            arms[0].pattern
        );
        assert!(arms[0].guard.is_none(), "range is a pattern, not a guard");
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
            Item::TypeAlias { name, params, ty } => {
                assert_eq!(name, "Id");
                assert!(params.is_empty());
                assert_eq!(*ty, Type::Named("Int".into(), vec![]));
            }
            other => panic!("expected a type alias, got {other:?}"),
        }
    }

    #[test]
    fn generic_type_alias_head_parses() {
        let m = parse_module(
            r#"
type Pair(a) = (a, a)
"#,
        )
        .expect("generic type alias should parse");
        match &m.items[0] {
            Item::TypeAlias { name, params, ty } => {
                assert_eq!(name, "Pair");
                assert_eq!(params, &vec!["a".to_string()]);
                assert_eq!(
                    *ty,
                    Type::Tuple(vec![Type::Named("a".into(), vec![]), Type::Named("a".into(), vec![])])
                );
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
    fn parses_nested_calls_as_arguments() {
        let stmts = fn_body(r#"
fn f(x: Int):
    add(double(x), 1)
"#);
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
    fn parses_anonymous_union_patterns() {
        let src = r#"
fn describe(e: .[BadPort(Int) | Missing(String) | NotFound]) -> String:
    match e:
        .BadPort(p) | .Missing(p) -> p
        .NotFound -> "missing"
"#;
        let stmts = fn_body(src);
        let Stmt::Expr(Expr::Match { arms, .. }) = &stmts[0] else {
            panic!("expected a match expression");
        };
        assert_eq!(arms.len(), 2);
        let Pattern::Or(alts) = &arms[0].pattern else {
            panic!("expected an or-pattern");
        };
        assert!(matches!(alts[0], Pattern::AnonCtor { ref tag, .. } if tag == "BadPort"));
        assert!(matches!(alts[1], Pattern::AnonCtor { ref tag, .. } if tag == "Missing"));
        assert!(matches!(arms[1].pattern, Pattern::AnonCtor { ref tag, .. } if tag == "NotFound"));
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
        assert_eq!(func.params[1].convention, Convention::Own);
        assert_eq!(func.params[2].convention, Convention::Let);
    }

    #[test]
    fn var_receiver_classifies_as_a_mutator() {
        // (RFC-0043) A `var` first param plus a return of that param's type is a
        // mutator; the same `var` param returning Nil (or nothing) is a procedure
        // channel; a plain first param is neither.
        let m = parse_module(
            "fn push(var xs: List(a), x: a) -> List(a):\n    xs\n\nfn bump(var n: Int):\n    n = n + 1\n\nfn map(xs: List(a), f: fn(a) -> b) -> List(b):\n    xs\n",
        )
        .unwrap();
        let funcs: Vec<&Function> = m
            .items
            .iter()
            .filter_map(|it| match it {
                Item::Function(f) => Some(f),
                _ => None,
            })
            .collect();
        // push: var receiver + self-typed return => mutator, not a procedure.
        assert!(funcs[0].is_mutator(), "push is a mutator");
        assert!(!funcs[0].is_var_procedure(), "push is not a procedure channel");
        // bump: var param + Nil return => procedure, not a mutator.
        assert!(!funcs[1].is_mutator(), "bump is not a mutator");
        assert!(funcs[1].is_var_procedure(), "bump is a procedure channel");
        // map: no var param => neither.
        assert!(!funcs[2].is_mutator(), "map is not a mutator");
        assert!(!funcs[2].is_var_procedure(), "map is not a procedure channel");
    }

    #[test]
    fn var_receiver_round_trips_through_fmt() {
        // (RFC-0043) The `var` receiver must survive `witchy fmt` — dropping it
        // would silently demote a mutator to a plain value function.
        let src = "pub fn push(var xs: List(a), x: a) -> List(a):\n    xs\n";
        let out = crate::format::reformat(src).expect("var receiver round-trips");
        assert!(out.contains("var xs: List(a)"), "var receiver must survive fmt: {out}");
        assert_eq!(
            crate::format::reformat(&out).as_deref(),
            Some(out.as_str()),
            "formatting is idempotent"
        );
    }

    #[test]
    fn inout_sink_keyword_aliases_are_removed() {
        // `inout`/`sink` were Hylo-style alias spellings for `var`/`own`; they
        // were removed, so they now lex as ordinary identifiers and a parameter
        // written with them no longer parses as a convention.
        assert!(parse_module("fn f(inout a: Int):\n    a\n").is_err());
        assert!(parse_module("fn f(sink a: Int):\n    a\n").is_err());
    }

    #[test]
    fn bare_name_tail_does_not_swallow_a_following_interpolation() {
        // Regression: an inline `else:` whose branch is a bare identifier, followed
        // by a line that starts with a string interpolation, used to mis-parse the
        // identifier as a call — because `"${...}"` lexes to a leading `(`, and the
        // initial call rule didn't require the `(` on the same line as the name.
        let m = parse_module(
            "fn f(c: Int) -> String:\n    let b = if c < 0: 0 - c else: c\n    \"${b}\"\n",
        )
        .expect("a newline-led interpolation is the next statement, not call args");
        let Item::Function(f) = &m.items[0] else { panic!("expected a function") };
        // The `let b = ...` and the trailing interpolation are TWO statements.
        assert_eq!(f.body.stmts.len(), 2, "{:?}", f.body.stmts);
        // A genuine call keeps its `(` on the same line, so this still parses as a call.
        parse_module("fn g(x: Int) -> Int:\n    x\nfn main(console: Console):\n    console.print(\"${g(1)}\")\n")
            .expect("a same-line call is unaffected");
    }

    #[test]
    fn rustism_diagnostics_point_at_the_witchy_form() {
        // `let mut x` (Rust) — suggest `var`, instead of "expected `=`, found `x`".
        let let_mut = parse_module("fn main(console: Console):\n    let mut x = 0\n    x = x + 1\n")
            .unwrap_err();
        assert!(let_mut.to_string().contains("var"), "{let_mut}");
        // `List<Int>` (Rust/TS) — suggest parentheses, not "expected `=`, found `<`".
        let angle = parse_module("fn main(console: Console):\n    let xs: List<Int> = []\n    console.print(\"x\")\n")
            .unwrap_err();
        assert!(angle.to_string().contains("List(…)"), "{angle}");
        // `var mut x` is NOT mistaken for the Rust form (mut is a real binding name here).
        parse_module("fn main(console: Console):\n    var mut = 0\n    mut = mut + 1\n    console.print(\"${mut}\")\n")
            .expect("`mut` is a valid identifier on its own");
    }

    // SEC-041: deeply-nested untrusted source must return a `ParseError`, never
    // overflow the native stack (an uncatchable SIGABRT when the parser runs in a
    // wasmtime host fn — `compiler.footprint`/`doc`/`diff` on the supply-chain gate).
    //
    // NOTE: the parser is exercised here on a large-stack thread. In a *debug* build
    // the per-recursion frame is ~10x a release build's, so a debug test thread's
    // 2 MiB stack overflows near paren-depth ~48 — below the guard at
    // MAX_PARSE_DEPTH. That is a *test-harness* artifact; the shipped binary is
    // release, where the guard fires with wide margin on every production stack
    // (release overflows a 2 MiB worker stack only near depth ~470). Giving the test
    // ample stack lets it observe the *guard* firing, which is the invariant.
    fn parse_deep(src: String) -> Result<Module, ParseError> {
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(move || parse_module(&src))
            .unwrap()
            .join()
            .expect("the parser must not overflow/abort — it must return a Result")
    }

    #[test]
    fn deeply_nested_parens_error_instead_of_overflowing() {
        let depth = (super::MAX_PARSE_DEPTH as usize) + 50;
        let src = format!(
            "fn main(console: Console):\n    let x = {}0{}\n    console.print(\"ok\")\n",
            "(".repeat(depth),
            ")".repeat(depth),
        );
        let err = parse_deep(src).expect_err("over-deep nesting must be a clean parse error");
        assert!(err.to_string().contains("nests too deeply"), "{err}");
    }

    // The reported bug shape: many nested parens with no closing side. It must still
    // error cleanly (via the depth guard) rather than abort the host process.
    #[test]
    fn unbounded_nested_parens_error_cleanly() {
        let src = format!("fn main():\n    {}", "(".repeat(10_000));
        let err = parse_deep(src).expect_err("must be a clean parse error, not an abort");
        assert!(err.to_string().contains("nests too deeply"), "{err}");
    }

    // Nesting well within the limit still parses (the guard doesn't reject legitimate
    // programs).
    #[test]
    fn moderately_nested_parens_still_parse() {
        let depth = (super::MAX_PARSE_DEPTH as usize) - 20;
        let src = format!(
            "fn main(console: Console):\n    let x = {}0{}\n    console.print(\"ok\")\n",
            "(".repeat(depth),
            ")".repeat(depth),
        );
        parse_deep(src).expect("nesting within the limit parses");
    }

    #[test]
    fn yield_inside_a_lambda_in_a_gen_fn_is_rejected() {
        // BUG-183: a `yield` in a nested lambda used to `check` clean but fail only
        // in codegen (the generator lowering does not descend into closures). It is
        // now a parse error, so `check` is a reliable gate for both backends.
        let err = parse_module(
            "import iter\n\ngen fn bad() -> Iter(Int):\n    let f = fn():\n        yield 1\n    yield 2\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("`yield`"), "{err}");
        // A `gen fn` method with a nested-lambda yield is rejected the same way.
        assert!(parse_module(
            "type Box:\n    value: Int\n\nimpl Box:\n    gen fn bad(self) -> Iter(Int):\n        let f = fn():\n            yield self.value\n        yield 2\n"
        )
        .is_err());
        // Control: a block-nested `yield` in the same generator still parses.
        parse_module("gen fn ok() -> Iter(Int):\n    if true:\n        yield 1\n    yield 2\n")
            .expect("block-nested yield parses");
    }
