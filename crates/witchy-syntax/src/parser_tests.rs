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
    fn function_declaration_lines_are_source_metadata() {
        let first = parse_module("fn answer() -> Int:\n    42\n").expect("first function");
        let moved = parse_module("\n\nfn answer() -> Int:\n    42\n").expect("moved function");
        let Item::Function(first) = &first.items[0] else {
            panic!("expected function")
        };
        let Item::Function(moved) = &moved.items[0] else {
            panic!("expected function")
        };
        assert_eq!(first.line, 1);
        assert_eq!(moved.line, 3);
        let mut relined = first.clone();
        relined.line = 99;
        assert_eq!(first, &relined, "declaration lines are not semantic fields");
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
            "pub comptime:\n    emit(\"x\")\n",
        ] {
            let err = parse_module(src).expect_err("non-function `pub` must be rejected");
            assert!(err.message.contains("`pub` may only precede a function"), "{err}");
        }

        parse_module("pub fn id(x: Int) -> Int:\n    x\n").expect("public function parses");
        parse_module("pub async fn fetch() -> String:\n    \"ok\"\n").expect("public async function parses");
        parse_module("pub gen fn one() -> Iter(Int):\n    yield 1\n").expect("public generator parses");
        parse_module("pub comptime fn tag() -> Int:\n    1\n")
            .expect("public compile-time function parses");
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
    fn quote_expr_lowers_to_compiler_owned_expr_and_imports_meta() {
        let m = parse_module(r#"
fn f():
    quote expr:
        40 + 1 * 2
"#).expect("quote expression should parse");
        assert!(m.imports.contains(&"meta".to_string()), "quote expr imports meta");
        let Item::Function(f) = &m.items[0] else {
            panic!("expected function");
        };
        let Stmt::Expr(Expr::Call { name, args }) = &f.body.stmts[0] else {
            panic!("expected compiler-owned expression call, got {:?}", f.body.stmts[0]);
        };
        assert_eq!(name, crate::intrinsics::COMPILER_QUOTE_EXPR);
        let [Expr::Str(handle), Expr::Str(source)] = args.as_slice() else {
            panic!("expected expression handle and compatibility source");
        };
        assert_eq!(source, "40 + 1 * 2");
        let [syntax] = m.compiler_expr_syntax.as_slice() else {
            panic!("expected one compiler-owned expression payload");
        };
        assert_eq!(handle, &syntax.handle);
        assert!(matches!(syntax.expr, Expr::Binary { .. }));
        assert_eq!(syntax.definition_line, 3);

        let err = parse_module("fn f():\n    quote module:\n        1\n")
            .expect_err("module quotation is not implemented");
        assert!(err.message.contains("`quote module:` is not implemented yet"), "{err}");
    }

    #[test]
    fn compiler_owned_syntax_handles_include_parse_context() {
        let method = parse_module(r#"
fn f(x: X):
    quote expr:
        x.run()
"#).expect("parse method-call quotation");
        let qualified = parse_module(r#"
import x

fn f():
    quote expr:
        x.run()
"#).expect("parse qualified-call quotation");

        let [method_syntax] = method.compiler_expr_syntax.as_slice() else {
            panic!("expected method-call syntax payload");
        };
        let [qualified_syntax] = qualified.compiler_expr_syntax.as_slice() else {
            panic!("expected qualified-call syntax payload");
        };
        assert!(matches!(method_syntax.expr, Expr::MethodCall { .. }));
        assert!(matches!(qualified_syntax.expr, Expr::Call { .. }));
        assert_ne!(method_syntax.handle, qualified_syntax.handle);
    }

    #[test]
    fn quote_expr_holes_keep_a_compiler_owned_template() {
        let m = parse_module(r#"
fn f(x: meta.ExprSyntax, y: meta.ExprSyntax):
    quote expr:
        add(${x}, 2 * ${y})
"#).expect("quote expression with holes should parse");
        assert!(m.imports.contains(&"meta".to_string()), "quote expr imports meta");
        let Item::Function(f) = &m.items[0] else {
            panic!("expected function");
        };
        let Stmt::Expr(Expr::Call { name, args }) = &f.body.stmts[0] else {
            panic!("expected compiler-owned expression-hole call, got {:?}", f.body.stmts[0]);
        };
        assert_eq!(name, crate::intrinsics::COMPILER_QUOTE_EXPR_HOLES);
        assert_eq!(m.compiler_expr_syntax.len(), 1);
        let Expr::Str(handle) = &args[0] else {
            panic!("expected expression syntax handle");
        };
        assert_eq!(handle, &m.compiler_expr_syntax[0].handle);
        assert_eq!(
            &args[1..],
            &[
                Expr::List(vec![
                    Expr::Str("add(".to_string()),
                    Expr::Str(", 2 * ".to_string()),
                    Expr::Str(")".to_string()),
                ]),
                Expr::List(vec![
                    Expr::Call {
                        name: "meta.expr_hole".to_string(),
                        args: vec![Expr::Var("x".to_string())],
                    },
                    Expr::Call {
                        name: "meta.expr_hole".to_string(),
                        args: vec![Expr::Var("y".to_string())],
                    },
                ]),
            ]
        );
    }

    #[test]
    fn quote_expr_holes_are_rejected_outside_quote_expr() {
        let err = parse_module("fn f(x: Int):\n    ${x}\n")
            .expect_err("quote holes outside quote expr are invalid");
        assert!(err.message.contains("only valid inside `quote expr:`"), "{err}");
    }

    #[test]
    fn quote_identifier_is_ordinary_outside_quote_form() {
        let stmts = fn_body(r#"
fn f():
    quote(1)
"#);
        assert_eq!(
            stmts,
            vec![Stmt::Expr(Expr::Call {
                name: "quote".into(),
                args: vec![Expr::Int(1)],
            })]
        );
    }

    #[test]
    fn quote_type_lowers_to_compiler_owned_type() {
        let m = parse_module(r#"
fn f():
    quote type:
        fn(List(Int), json.Json) -> unique File[Read]
"#).expect("quote type should parse");
        assert!(m.imports.contains(&"meta".to_string()), "quote type imports meta");
        let Item::Function(f) = &m.items[0] else {
            panic!("expected function");
        };
        let [Stmt::Expr(Expr::Call { name, args })] = f.body.stmts.as_slice() else {
            panic!("expected compiler-owned type call");
        };
        assert_eq!(name, crate::intrinsics::COMPILER_QUOTE_TYPE);
        let [Expr::Str(handle), Expr::Str(source)] = args.as_slice() else {
            panic!("expected type handle and compatibility source");
        };
        assert_eq!(source, "fn(List(Int), json.Json) -> unique File[Read]");
        let [syntax] = m.compiler_type_syntax.as_slice() else {
            panic!("expected one compiler-owned type payload");
        };
        assert_eq!(handle, &syntax.handle);
        assert!(matches!(syntax.ty, Type::Fn(..)));
        assert_eq!(syntax.definition_line, 3);
    }

    #[test]
    fn quote_type_owns_structural_and_borrowed_types() {
        let records = parse_module("fn f():\n    quote type:\n        .{x: Int, y: String}\n")
            .expect("anonymous record type quote should parse");
        let [record] = records.compiler_type_syntax.as_slice() else {
            panic!("expected one record type payload");
        };
        assert!(matches!(&record.ty, Type::Named(name, _) if name.starts_with("__anon")));

        let compositions = parse_module(
            "fn f():\n    quote type:\n        .{..Base, y: String}\n",
        )
        .expect("structural record composition quote should parse");
        let [composition] = compositions.compiler_type_syntax.as_slice() else {
            panic!("expected one record composition type payload");
        };
        assert!(matches!(
            &composition.ty,
            Type::RecordCompose { base, fields }
                if base.as_ref() == &Type::Named("Base".into(), vec![])
                    && fields == &vec![("y".into(), Type::Named("String".into(), vec![]))]
        ));

        let unions = parse_module("fn f():\n    quote type:\n        .[Ready | Failed(String)]\n")
            .expect("anonymous union type quote should parse");
        let [union] = unions.compiler_type_syntax.as_slice() else {
            panic!("expected one union type payload");
        };
        assert!(matches!(&union.ty, Type::Named(name, _) if name.starts_with("__union")));

        let views = parse_module("fn f():\n    quote type:\n        View(Int, 'a)\n")
            .expect("borrowed view type quote should parse");
        let [view] = views.compiler_type_syntax.as_slice() else {
            panic!("expected one borrowed type payload");
        };
        assert!(matches!(view.ty, Type::Qualified(TypeQual::Borrow(_), _)));
    }

    #[test]
    fn function_types_retain_parameter_conventions() {
        let module = parse_module(
            "fn use(f: fn(var Int, let String, own Bytes, Bool) -> Int):\n    0\n",
        )
        .expect("convention-bearing function type should parse");
        let Item::Function(function) = &module.items[0] else {
            panic!("expected function");
        };
        let Some(Type::Fn(params, ret, conventions)) = &function.params[0].ty else {
            panic!("expected function-typed parameter");
        };
        assert_eq!(params.len(), 4);
        assert_eq!(ret.as_ref(), &Type::Named("Int".into(), Vec::new()));
        assert_eq!(
            conventions,
            &vec![
                Convention::Var,
                Convention::Borrow,
                Convention::Own,
                Convention::Let,
            ]
        );
        assert_eq!(
            crate::format::type_str(function.params[0].ty.as_ref().unwrap()),
            "fn(var Int, let String, own Bytes, Bool) -> Int"
        );
    }

    #[test]
    fn quote_type_holes_keep_a_compiler_owned_template() {
        let m = parse_module(r#"
fn f(ok: meta.TypeSyntax, err: meta.TypeSyntax):
    quote type:
        Result(${ok}, List(${err}))
"#).expect("quote type with holes should parse");
        let Item::Function(f) = &m.items[0] else {
            panic!("expected function");
        };
        assert_eq!(m.compiler_type_syntax.len(), 1);
        let Stmt::Expr(Expr::Call { name, args }) = &f.body.stmts[0] else {
            panic!("expected compiler-owned type-hole call");
        };
        assert_eq!(name, crate::intrinsics::COMPILER_QUOTE_TYPE_HOLES);
        let [Expr::Str(handle), Expr::List(parts), Expr::List(holes)] = args.as_slice() else {
            panic!("expected type handle, parts, and holes");
        };
        assert_eq!(handle, &m.compiler_type_syntax[0].handle);
        assert_eq!(
            parts,
            &vec![
                Expr::Str("Result(".into()),
                Expr::Str(", List(".into()),
                Expr::Str("))".into()),
            ]
        );
        assert_eq!(holes, &vec![Expr::Var("ok".into()), Expr::Var("err".into())]);
    }

    #[test]
    fn quote_pattern_lowers_to_compiler_owned_pattern() {
        let m = parse_module(r#"
fn f():
    quote pattern:
        [1 | 2, ..rest]
"#).expect("quote pattern should parse");
        assert!(m.imports.contains(&"meta".to_string()), "quote pattern imports meta");
        let Item::Function(f) = &m.items[0] else {
            panic!("expected function");
        };
        let [Stmt::Expr(Expr::Call { name, args })] = f.body.stmts.as_slice() else {
            panic!("expected compiler-owned pattern call");
        };
        assert_eq!(name, crate::intrinsics::COMPILER_QUOTE_PATTERN);
        let [Expr::Str(handle), Expr::Str(source)] = args.as_slice() else {
            panic!("expected pattern handle and compatibility source");
        };
        assert_eq!(source, "[1 | 2, ..rest]");
        let [syntax] = m.compiler_pattern_syntax.as_slice() else {
            panic!("expected one compiler-owned pattern payload");
        };
        assert_eq!(handle, &syntax.handle);
        assert!(matches!(syntax.pattern, Pattern::List { .. }));
        assert_eq!(syntax.definition_line, 3);
    }

    #[test]
    fn quote_pattern_holes_keep_a_compiler_owned_template() {
        let m = parse_module(r#"
fn f(payload: meta.PatternSyntax):
    quote pattern:
        Some(${payload}) | None
"#).expect("quote pattern with holes should parse");
        let Item::Function(f) = &m.items[0] else {
            panic!("expected function");
        };
        assert_eq!(m.compiler_pattern_syntax.len(), 1);
        let Stmt::Expr(Expr::Call { name, args }) = &f.body.stmts[0] else {
            panic!("expected compiler-owned pattern-hole call");
        };
        assert_eq!(name, crate::intrinsics::COMPILER_QUOTE_PATTERN_HOLES);
        let [Expr::Str(handle), Expr::List(parts), Expr::List(holes)] = args.as_slice() else {
            panic!("expected pattern handle, parts, and holes");
        };
        assert_eq!(handle, &m.compiler_pattern_syntax[0].handle);
        assert_eq!(
            parts,
            &vec![Expr::Str("Some(".into()), Expr::Str(") | None".into())]
        );
        assert_eq!(holes, &vec![Expr::Var("payload".into())]);
    }

    #[test]
    fn quote_stmt_lowers_to_compiler_owned_statement() {
        let m = parse_module(r#"
fn f():
    quote stmt:
        let x: Int = 5
"#).expect("quote stmt should parse");
        let Item::Function(f) = &m.items[0] else {
            panic!("expected function");
        };
        let [Stmt::Expr(Expr::Call { name, args })] = f.body.stmts.as_slice() else {
            panic!("expected compiler-owned statement call");
        };
        assert_eq!(name, crate::intrinsics::COMPILER_QUOTE_STMT);
        let [Expr::Str(handle), Expr::Str(source)] = args.as_slice() else {
            panic!("expected statement handle and compatibility source");
        };
        assert_eq!(source, "let x: Int = 5");
        let [syntax] = m.compiler_stmt_syntax.as_slice() else {
            panic!("expected one compiler-owned statement payload");
        };
        assert_eq!(handle, &syntax.handle);
        assert!(matches!(syntax.stmt, Stmt::Let { .. }));
        assert_eq!(syntax.definition_line, 3);
    }

    #[test]
    fn quote_stmt_holes_keep_a_compiler_owned_template() {
        let m = parse_module(r#"
fn f(value: meta.ExprSyntax):
    quote stmt:
        let x = ${value}
"#).expect("quote stmt with expression holes should parse");
        let Item::Function(f) = &m.items[0] else {
            panic!("expected function");
        };
        assert_eq!(m.compiler_stmt_syntax.len(), 1);
        let Stmt::Expr(Expr::Call { name, args }) = &f.body.stmts[0] else {
            panic!("expected compiler-owned statement-hole call");
        };
        assert_eq!(name, crate::intrinsics::COMPILER_QUOTE_STMT_HOLES);
        let [Expr::Str(handle), parts, holes] = args.as_slice() else {
            panic!("expected statement handle, parts, and holes");
        };
        assert_eq!(handle, &m.compiler_stmt_syntax[0].handle);
        assert_eq!(
            parts,
            &Expr::List(vec![Expr::Str("let x = ".into()), Expr::Str("".into())])
        );
        assert_eq!(
            holes,
            &Expr::List(vec![Expr::Call {
                name: "meta.expr_hole".into(),
                args: vec![Expr::Var("value".into())],
            }])
        );
    }

    #[test]
    fn quote_stmt_mixed_holes_lower_in_source_order() {
        let m = parse_module(r#"
fn f(ty: meta.TypeSyntax, value: meta.ExprSyntax):
    quote stmt:
        let x: ${ty} = ${value}
"#).expect("quote stmt with mixed holes should parse");
        let Item::Function(f) = &m.items[0] else {
            panic!("expected function");
        };
        let Stmt::Expr(Expr::Call { name, args }) = &f.body.stmts[0] else {
            panic!("expected compiler-owned statement-hole call");
        };
        assert_eq!(name, crate::intrinsics::COMPILER_QUOTE_STMT_HOLES);
        assert_eq!(
            &args[1..],
            &[
                Expr::List(vec![
                    Expr::Str("let x: ".into()),
                    Expr::Str(" = ".into()),
                    Expr::Str("".into()),
                ]),
                Expr::List(vec![
                    Expr::Call {
                        name: "meta.type_hole".into(),
                        args: vec![Expr::Var("ty".into())],
                    },
                    Expr::Call {
                        name: "meta.expr_hole".into(),
                        args: vec![Expr::Var("value".into())],
                    },
                ]),
            ]
        );
    }

    #[test]
    fn quote_block_lowers_to_compiler_owned_block() {
        let m = parse_module(r#"
fn f():
    quote block:
        let x = 5
        x + 1
"#).expect("quote block should parse");
        let Item::Function(f) = &m.items[0] else {
            panic!("expected function");
        };
        let [Stmt::Expr(Expr::Call { name, args })] = f.body.stmts.as_slice() else {
            panic!("expected compiler-owned block call");
        };
        assert_eq!(name, crate::intrinsics::COMPILER_QUOTE_BLOCK);
        let [Expr::Str(handle), Expr::Str(source)] = args.as_slice() else {
            panic!("expected block handle and compatibility source");
        };
        assert_eq!(source, "let x = 5\nx + 1");
        let [syntax] = m.compiler_block_syntax.as_slice() else {
            panic!("expected one compiler-owned block payload");
        };
        assert_eq!(handle, &syntax.handle);
        assert_eq!(syntax.block.stmts.len(), 2);
        assert_eq!(syntax.definition_line, 3);
    }

    #[test]
    fn quote_block_holes_keep_a_compiler_owned_template() {
        let m = parse_module(r#"
fn f(value: meta.ExprSyntax, delta: meta.ExprSyntax):
    quote block:
        let x = ${value}
        x + ${delta}
"#).expect("quote block with expression holes should parse");
        let Item::Function(f) = &m.items[0] else {
            panic!("expected function");
        };
        assert_eq!(m.compiler_block_syntax.len(), 1);
        let Stmt::Expr(Expr::Call { name, args }) = &f.body.stmts[0] else {
            panic!("expected compiler-owned block-hole call");
        };
        assert_eq!(name, crate::intrinsics::COMPILER_QUOTE_BLOCK_HOLES);
        assert_eq!(&args[0], &Expr::Str(m.compiler_block_syntax[0].handle.clone()));
        assert_eq!(
            &args[1..],
            &[
                Expr::List(vec![
                    Expr::Str("let x = ".into()),
                    Expr::Str("\nx + ".into()),
                    Expr::Str("".into()),
                ]),
                Expr::List(vec![
                    Expr::Call {
                        name: "meta.expr_hole".into(),
                        args: vec![Expr::Var("value".into())],
                    },
                    Expr::Call {
                        name: "meta.expr_hole".into(),
                        args: vec![Expr::Var("delta".into())],
                    },
                ]),
            ]
        );
    }

    #[test]
    fn quote_block_mixed_holes_lower_in_source_order() {
        let m = parse_module(r#"
fn f(ty: meta.TypeSyntax, value: meta.ExprSyntax, pat: meta.PatternSyntax, tail: meta.ExprSyntax):
    quote block:
        let x: ${ty} = ${value}
        let ${pat} = x
        ${tail}
"#).expect("quote block with mixed holes should parse");
        let Item::Function(f) = &m.items[0] else {
            panic!("expected function");
        };
        let Stmt::Expr(Expr::Call { name, args }) = &f.body.stmts[0] else {
            panic!("expected compiler-owned block-hole call");
        };
        assert_eq!(name, crate::intrinsics::COMPILER_QUOTE_BLOCK_HOLES);
        assert_eq!(
            &args[1..],
            &[
                Expr::List(vec![
                    Expr::Str("let x: ".into()),
                    Expr::Str(" = ".into()),
                    Expr::Str("\nlet ".into()),
                    Expr::Str(" = x\n".into()),
                    Expr::Str("".into()),
                ]),
                Expr::List(vec![
                    Expr::Call {
                        name: "meta.type_hole".into(),
                        args: vec![Expr::Var("ty".into())],
                    },
                    Expr::Call {
                        name: "meta.expr_hole".into(),
                        args: vec![Expr::Var("value".into())],
                    },
                    Expr::Call {
                        name: "meta.pattern_hole".into(),
                        args: vec![Expr::Var("pat".into())],
                    },
                    Expr::Call {
                        name: "meta.expr_hole".into(),
                        args: vec![Expr::Var("tail".into())],
                    },
                ]),
            ]
        );
    }

    #[test]
    fn quote_item_lowers_to_compiler_owned_item_handle() {
        let m = parse_module(r#"
fn f():
    quote item:
        pub fn generated() -> Int:
            99
"#).expect("quote item should parse");
        assert!(m.imports.contains(&"meta".to_string()), "quote item imports meta");
        let Item::Function(f) = &m.items[0] else {
            panic!("expected function");
        };
        let Stmt::Expr(Expr::Call { name, args }) = &f.body.stmts[0] else {
            panic!("expected compiler-owned item call, got {:?}", f.body.stmts[0]);
        };
        assert_eq!(name, crate::intrinsics::COMPILER_QUOTE_ITEM);
        let [Expr::Str(handle), Expr::Str(canonical_source)] = args.as_slice() else {
            panic!("expected an opaque item handle and formatter source, got {args:?}");
        };
        let [syntax] = m.compiler_item_syntax.as_slice() else {
            panic!("expected one compiler-owned item payload");
        };
        assert_eq!(handle, &syntax.handle);
        assert_eq!(canonical_source, "pub fn generated() -> Int:\n    99\n");
        assert_eq!(syntax.definition_line, 3);
        let Item::Function(generated) = &syntax.item else {
            panic!("expected quoted function payload");
        };
        assert_eq!(generated.name, "generated");
        assert_eq!(generated.body.stmts, [Stmt::Expr(Expr::Int(99))]);
    }

    #[test]
    fn quote_item_mixed_holes_lower_in_source_order() {
        let m = parse_module(r#"
fn f(ty: meta.TypeSyntax, value: meta.ExprSyntax, pat: meta.PatternSyntax, tail: meta.ExprSyntax):
    quote item:
        pub fn generated(x: ${ty}) -> ${ty}:
            let ${pat} = ${value}
            x + ${tail}
"#).expect("quote item with mixed holes should parse");
        let Item::Function(f) = &m.items[0] else {
            panic!("expected function");
        };
        let Stmt::Expr(Expr::Call { name, args }) = &f.body.stmts[0] else {
            panic!("expected compiler-owned item-hole call, got {:?}", f.body.stmts[0]);
        };
        assert_eq!(name, crate::intrinsics::COMPILER_QUOTE_ITEM_HOLES);
        let [Expr::Str(handle), parts, holes] = args.as_slice() else {
            panic!("expected handle, source parts, and typed holes, got {args:?}");
        };
        assert_eq!(
            parts,
            &Expr::List(vec![
                    Expr::Str("pub fn generated(x: ".into()),
                    Expr::Str(") -> ".into()),
                    Expr::Str(":\n    let ".into()),
                    Expr::Str(" = ".into()),
                    Expr::Str("\n    x + ".into()),
                    Expr::Str("\n".into()),
                ])
        );
        assert_eq!(
            holes,
            &Expr::List(vec![
                    Expr::Call {
                        name: "meta.type_hole".into(),
                        args: vec![Expr::Var("ty".into())],
                    },
                    Expr::Call {
                        name: "meta.type_hole".into(),
                        args: vec![Expr::Var("ty".into())],
                    },
                    Expr::Call {
                        name: "meta.pattern_hole".into(),
                        args: vec![Expr::Var("pat".into())],
                    },
                    Expr::Call {
                        name: "meta.expr_hole".into(),
                        args: vec![Expr::Var("value".into())],
                    },
                    Expr::Call {
                        name: "meta.expr_hole".into(),
                        args: vec![Expr::Var("tail".into())],
                    },
                ])
        );
        let [syntax] = m.compiler_item_syntax.as_slice() else {
            panic!("expected one compiler-owned item template");
        };
        assert_eq!(handle, &syntax.handle);
        let Item::Function(generated) = &syntax.item else {
            panic!("expected generated function template");
        };
        assert_eq!(generated.name, "generated");
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
    fn var_convention_is_independent_of_return_shape() {
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
        assert_eq!(funcs[0].params[0].convention, Convention::Var);
        assert_eq!(funcs[1].params[0].convention, Convention::Var);
        assert_eq!(funcs[2].params[0].convention, Convention::Let);
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

    #[test]
    fn borrowed_param_and_result_views_parse() {
        // (RFC-0083) `let('a) T` is a borrowed parameter; `View(T, 'a)` a borrowed
        // result. Both desugar to the same `Qualified(Borrow('a), T)` node.
        let m = parse_module(
            "mode opt\n\nfn first(text: let('a) String) -> View(String, 'a):\n    text\n",
        )
        .expect("borrowed view signature parses");
        let Item::Function(f) = &m.items[0] else { panic!("expected a function") };
        let want = Some(Type::Qualified(
            TypeQual::Borrow("a".into()),
            Box::new(Type::Named("String".into(), vec![])),
        ));
        assert_eq!(f.params[0].ty, want, "`let('a) String` param is a borrowed view");
        assert_eq!(f.ret, want, "`View(String, 'a)` return is a borrowed view");
    }

    #[test]
    fn borrowed_nominal_parameters_and_arguments_keep_their_kinds() {
        let module = parse_module(
            "mode opt\n\ntype Pair(a, 'left, 'right):\n    first: View(a, 'left)\n    second: View(a, 'right)\n\nfn keep(let left: let('left) Int, let right: let('right) Int, pair: Pair(Int, 'left, 'right)) -> Pair(Int, 'left, 'right):\n    pair\n",
        )
        .expect("mixed type/lifetime nominal parameters parse");
        let Item::Type(definition) = &module.items[0] else { panic!("expected type") };
        assert_eq!(definition.params, ["a", "'left", "'right"]);
        let Item::Function(function) = &module.items[1] else { panic!("expected function") };
        let expected = Type::Named(
            "Pair".into(),
            vec![
                Type::Named("Int".into(), vec![]),
                Type::Named("'left".into(), vec![]),
                Type::Named("'right".into(), vec![]),
            ],
        );
        assert_eq!(function.params[2].ty.as_ref(), Some(&expected));
        assert_eq!(function.ret.as_ref(), Some(&expected));
    }

    #[test]
    fn borrowed_single_variant_positional_nominal_parses() {
        let module = parse_module(
            "mode opt\n\ntype Span('a):\n    Span(View(Bytes, 'a), Int, Int)\n",
        )
        .expect("single positional borrowed nominal parses");
        let Item::Type(definition) = &module.items[0] else { panic!("expected type") };
        assert_eq!(definition.params, ["'a"]);
        assert_eq!(definition.variants.len(), 1);
        assert!(definition.variants[0].field_names.is_empty());
    }

    #[test]
    fn borrowed_nominal_sum_parses_for_a_loud_semantic_rejection() {
        parse_module(
            "mode opt\n\ntype MaybeSpan('a):\n    Empty\n    Span(View(Bytes, 'a))\n",
        )
        .expect("the parser retains borrowed sum syntax for the type checker diagnostic");
    }

    #[test]
    fn type_aliases_reject_lifetime_parameters_at_the_declaration() {
        let error = parse_module("type Alias('a) = Bytes\n")
            .expect_err("lifetime-parameterized aliases are outside RFC-0112");
        assert!(
            error.message.contains("only supported on a nominal type declaration")
                && error.message.contains("type aliases accept ordinary type parameters only"),
            "{error}"
        );
    }

    #[test]
    fn bare_let_convention_is_not_a_view() {
        // A plain `let` parameter convention (an immutable borrow keyword) must NOT
        // be misread as the `let('a)` view type — only `let` immediately followed
        // by `('lifetime)` is a view.
        let m = parse_module("fn f(let x: Int) -> Int:\n    x\n").expect("let convention parses");
        let Item::Function(f) = &m.items[0] else { panic!("expected a function") };
        assert_eq!(f.params[0].convention, Convention::Borrow);
        assert_eq!(f.params[0].ty, Some(Type::Named("Int".into(), vec![])));
    }

    #[test]
    fn view_lifetime_must_be_a_lifetime_token() {
        // `View(T, X)` with an ordinary identifier where a lifetime belongs is a
        // parse error — the second slot is a lifetime, not a type.
        let err = parse_module("mode opt\n\nfn f(s: let('a) String) -> View(String, a):\n    s\n")
            .expect_err("non-lifetime second arg rejected");
        assert!(err.message.contains("lifetime"), "{err}");
    }

    #[test]
    fn bare_quote_is_still_rejected() {
        // A stray `'` with no name is not a lifetime; it must still be the old
        // "not a witchy operator" style lex/parse failure, never a silent token.
        assert!(parse_module("fn f() -> Int:\n    '\n").is_err());
    }

    // ---- RFC-0081: existential trait types -------------------------------

    #[test]
    fn dyn_trait_parses_in_every_type_position() {
        let src = "type Boxed = dyn Render\n\nfn page(parts: List(dyn Render), one: dyn Render, pair: (dyn Render, Int), make: fn(dyn Convert(Int)) -> dyn Render) -> dyn Render:\n    one\n";
        let m = parse_module(src).expect("dyn parses in alias, generic-arg, param, tuple, fn-type, and return positions");
        let Item::TypeAlias { ty, .. } = &m.items[0] else { panic!("expected alias") };
        assert_eq!(ty, &Type::Dyn("Render".into(), vec![]));
        let Item::Function(f) = &m.items[1] else { panic!("expected fn") };
        assert_eq!(
            f.params[0].ty,
            Some(Type::Named("List".into(), vec![Type::Dyn("Render".into(), vec![])]))
        );
        assert_eq!(f.ret, Some(Type::Dyn("Render".into(), vec![])));
    }

    #[test]
    fn dyn_trait_args_parse_and_nest() {
        let src = "fn f(c: dyn Convert(Int, List(String))) -> Int:\n    1\n";
        let m = parse_module(src).expect("generic trait instantiation parses");
        let Item::Function(f) = &m.items[0] else { panic!("expected fn") };
        assert_eq!(
            f.params[0].ty,
            Some(Type::Dyn(
                "Convert".into(),
                vec![
                    Type::Named("Int".into(), vec![]),
                    Type::Named("List".into(), vec![Type::Named("String".into(), vec![])]),
                ]
            ))
        );
    }

    #[test]
    fn dyn_stays_an_ordinary_identifier_without_a_trait_head() {
        // Contextual keyword: a bare `dyn` (no uppercase name following) is a
        // plain type variable, exactly like `frozen`.
        let src = "fn f(x: dyn) -> dyn:\n    x\n";
        let m = parse_module(src).expect("bare `dyn` stays a type variable");
        let Item::Function(f) = &m.items[0] else { panic!("expected fn") };
        assert_eq!(f.params[0].ty, Some(Type::Named("dyn".into(), vec![])));
    }

    #[test]
    fn dyn_qualifies_under_ownership_qualifiers_and_views() {
        let src = "mode opt\n\nfn f(a: frozen dyn Render, b: let('a) dyn Render) -> Int:\n    1\n";
        let m = parse_module(src).expect("qualifiers wrap dyn types");
        let Item::Function(f) = &m.items[0] else { panic!("expected fn") };
        assert_eq!(
            f.params[0].ty,
            Some(Type::Qualified(TypeQual::Frozen, Box::new(Type::Dyn("Render".into(), vec![]))))
        );
        assert_eq!(
            f.params[1].ty,
            Some(Type::Qualified(
                TypeQual::Borrow("a".into()),
                Box::new(Type::Dyn("Render".into(), vec![]))
            ))
        );
    }

    #[test]
    fn as_dyn_trait_parses_as_an_explicit_cast() {
        let body = fn_body("fn f(w: Int) -> Int:\n    let e = w as dyn Render\n    1\n");
        let crate::ast::Stmt::Let { value, .. } = &body[0] else { panic!("expected let") };
        let crate::ast::Expr::As { ty, .. } = value else { panic!("expected as-cast") };
        assert_eq!(ty, &Type::Dyn("Render".into(), vec![]));
    }

    #[test]
    fn dyn_trait_head_may_be_module_qualified() {
        let module = parse_module("fn f(x: dyn render.Widget) -> Int:\n    1\n")
            .expect("qualified dyn trait head parses");
        let function = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) => Some(function),
                _ => None,
            })
            .expect("expected function");
        assert_eq!(
            function.params[0].ty,
            Some(Type::Dyn("render.Widget".into(), Vec::new()))
        );

        // An uppercase first segment is a bare trait, not a module qualifier.
        parse_module("fn f(x: dyn Render.Widget) -> Int:\n    1\n")
            .expect_err("uppercase pseudo-module remains malformed");
    }

    #[test]
    fn dyn_with_empty_argument_list_is_a_precise_parse_error() {
        let err = parse_module("fn f(x: dyn Render()) -> Int:\n    1\n")
            .expect_err("empty trait-argument list rejected");
        assert!(
            err.message.contains("empty trait-argument list is malformed"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn runtime_type_parses_a_compiler_owned_type_position() {
        let module = parse_module(
            "import dynamic\n\nfn descriptor() -> dynamic.RuntimeType:\n    dynamic.runtime_type(List(.{name: String, age: Int}))\n",
        )
        .expect("runtime type position parses");
        let function = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) => Some(function),
                _ => None,
            })
            .expect("expected function");
        let Stmt::Expr(Expr::Call { name, args }) = &function.body.stmts[0] else {
            panic!("expected compiler-owned runtime type call")
        };
        assert_eq!(name, crate::intrinsics::DYNAMIC_RUNTIME_TYPE);
        let [Expr::Str(handle), Expr::Str(source)] = args.as_slice() else {
            panic!("expected opaque handle and source projection")
        };
        assert_eq!(source, "List(.{age: Int, name: String})");
        let [syntax] = module.compiler_type_syntax.as_slice() else {
            panic!("expected one stored type node")
        };
        assert!(syntax.runtime_identity);
        assert_eq!(&syntax.handle, handle);
        assert_eq!(crate::format::module(&module, &[]),
            "import dynamic\nimport reflect\n\nfn descriptor() -> dynamic.RuntimeType:\n    dynamic.runtime_type(List(.{age: Int, name: String}))\n");
    }

    #[test]
    fn runtime_type_rejects_value_and_multiple_type_arguments() {
        for source in [
            "import dynamic\nfn f():\n    dynamic.runtime_type()\n",
            "import dynamic\nfn f():\n    dynamic.runtime_type(Int, String)\n",
            "import dynamic\nfn f():\n    dynamic.runtime_type(1)\n",
        ] {
            parse_module(source).expect_err("runtime_type accepts exactly one type");
        }
    }

    #[test]
    fn expression_span_sidecar_retains_exact_nested_source_ranges() {
        let source = "fn calculate() -> Int:\n    add(1, 2 * 3)\n";
        let (module, spans) =
            parse_module_with_expression_spans(source).expect("expression source ranges");
        assert_eq!(module, parse_module(source).expect("ordinary parse"));
        let positions = spans
            .iter()
            .map(|span| {
                (
                    span.source.start.line,
                    span.source.start.column,
                    span.source.end.line,
                    span.source.end.column,
                    span.statement_line,
                )
            })
            .collect::<Vec<_>>();
        for expected in [
            (2, 5, 2, 18, 2),  // add(1, 2 * 3)
            (2, 9, 2, 10, 2),  // 1
            (2, 12, 2, 17, 2), // 2 * 3
            (2, 16, 2, 17, 2), // 3
        ] {
            assert!(
                positions.contains(&expected),
                "missing {expected:?} in {positions:?}"
            );
        }
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    }
