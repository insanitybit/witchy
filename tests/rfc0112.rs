//! RFC-0112 structural callable-owner evidence.

use std::panic::{self, AssertUnwindSafe};
use std::sync::Mutex;

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter};
use witchy_syntax::ast::{Expr, Function, Item, Module, Stmt, Type};
use witchy_types::access::{
    AccessKind, AccessQualifier, AccessSignature, LoanProjection, LoanProjectionStep,
    OwnershipStateClass, checked_facts,
};
use witchy_types::{traits, typeck};

const CALLABLE_OWNER_MATRIX: &str = r#"
mode opt

type Holder('a):
    view: View(String, 'a)

type Catalog:
    marker: Int

trait SuppliesCallable:
    fn callable() -> fn(let View(String, 'supplied), let View(Holder('supplied), 'supplied)) -> View(String, 'supplied)

impl SuppliesCallable for Catalog:
    fn callable() -> fn(let View(String, 'supplied), let View(Holder('supplied), 'supplied)) -> View(String, 'supplied):
        direct

fn direct(let owner: let('a) String, let holder: let('a) Holder('a)) -> View(String, 'a):
    owner

fn matrix() -> Int:
    let direct_value: fn(let View(String, 'direct), let View(Holder('direct), 'direct)) -> View(String, 'direct) = direct
    let alpha_value: fn(let View(String, 'renamed), let View(Holder('renamed), 'renamed)) -> View(String, 'renamed) = direct
    let closure_value: fn(let View(String, 'closure), let View(Holder('closure), 'closure)) -> View(String, 'closure) = fn(let owner: let('inner) String, let holder: let('inner) Holder('inner)) -> View(String, 'inner):
        owner
    let static_value: fn(let View(String, 'static), let View(Holder('static), 'static)) -> View(String, 'static) = Catalog.callable()

    let direct_observed = direct_value
    let alpha_observed = alpha_value
    let closure_observed = closure_value
    let static_observed = static_value
    0
"#;

const RELATION_CHANGING_ASCRIPTION: &str = r#"
mode opt

type Holder('a):
    view: View(String, 'a)

fn direct(let owner: let('a) String, let holder: let('a) Holder('a)) -> View(String, 'a):
    owner

fn matrix() -> Int:
    let wrong: fn(let View(String, 'left), let View(Holder('right), 'right)) -> View(String, 'left) = direct
    0
"#;

fn function<'module>(module: &'module Module, name: &str) -> &'module Function {
    module
        .items
        .iter()
        .find_map(|item| match item {
            Item::Function(function) if function.name == name => Some(function),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing function `{name}`"))
}

fn run_compiled_with_runtime_checks(
    checked: &witchy_types::pipeline::CheckedModule,
    env: RuntimeCheckEnv,
) -> (Vec<String>, i64, i64) {
    with_runtime_check_env(env, || {
        let bytes = codegen::compile_checked_module_binary(checked)
            .expect_lowered("compile RFC-0112 parity fixture");
        let mut runtime = Runtime::batch().expect("runtime");
        let mut actor = runtime
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    quiet: true,
                    ..Default::default()
                },
                256,
            )
            .expect("spawn RFC-0112 parity fixture");
        actor.run().expect("run RFC-0112 parity fixture");
        let packed_alloc_calls = actor.packed_alloc_calls().unwrap_or(0);
        let packed_alloc_bytes = actor.packed_alloc_bytes().unwrap_or(0);
        (actor.output(), packed_alloc_calls, packed_alloc_bytes)
    })
}

fn run_compiled(checked: &witchy_types::pipeline::CheckedModule) -> Vec<String> {
    run_compiled_with_runtime_checks(checked, RuntimeCheckEnv::default()).0
}

fn binding<'module>(module: &'module Module, name: &str) -> &'module Expr {
    function(module, "matrix")
        .body
        .stmts
        .iter()
        .find_map(|statement| match statement {
            Stmt::Let {
                name: binding,
                value,
                ..
            } if binding == name => Some(value),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing matrix binding `{name}`"))
}

fn assert_same_access_identity(required: &AccessSignature, candidate: &AccessSignature) {
    required
        .verify_exact(candidate)
        .expect("the callable shape must preserve the declaration's access identity");
    candidate
        .verify_exact(required)
        .expect("callable access identity must be symmetric under lifetime alpha-renaming");
}

fn assert_holder_owner_contract(signature: &AccessSignature) {
    assert!(signature.callable_qualifiers().is_empty());
    assert_eq!(signature.params().len(), 2);

    let [relation] = signature.borrow_relations() else {
        panic!("the callable result must retain one exact borrowed-storage relation")
    };
    let lifetime = relation.lifetime();

    for parameter in signature.params() {
        assert_eq!(parameter.kind(), AccessKind::SharedBorrow);
        assert_eq!(
            parameter.qualifiers(),
            &[AccessQualifier::Borrow(lifetime.to_string())]
        );
        assert_eq!(
            parameter
                .borrow_lifetimes()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            [lifetime]
        );
        assert_eq!(
            parameter.ownership().input(),
            Some(&OwnershipStateClass::BorrowedOwnerRoot {
                lifetime: lifetime.to_string()
            })
        );
        assert!(parameter.ownership().writeback().is_none());
    }

    assert_eq!(
        signature.result().qualifiers(),
        &[AccessQualifier::Borrow(lifetime.to_string())]
    );
    assert_eq!(
        signature
            .result()
            .borrow_lifetimes()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [lifetime]
    );
    assert_eq!(
        signature.result().ownership_output(),
        Some(&OwnershipStateClass::BorrowedOwnerRoot {
            lifetime: lifetime.to_string()
        })
    );

    assert_eq!(relation.output_projection(), &LoanProjection::default());
    assert_eq!(
        relation.storage_type(),
        &Type::Named("String".into(), Vec::new())
    );
    let owners = relation.owners();
    assert_eq!(
        owners.len(),
        2,
        "owner roots must neither disappear nor multiply"
    );
    assert_eq!(owners[0].position(), 0);
    assert_eq!(owners[0].input_projection(), &LoanProjection::default());
    assert_eq!(owners[1].position(), 1);
    assert_eq!(
        owners[1].input_projection(),
        &LoanProjection {
            steps: vec![LoanProjectionStep::Field("view".into())]
        }
    );
}

fn assert_runtime_holder_construction_is_gated(module: &Module, static_name: &str) {
    let direct = function(module, "direct");
    assert!(
        matches!(direct.body.stmts.as_slice(), [Stmt::Expr(Expr::Var(name))] if name == "owner"),
        "direct may describe Holder ownership, but must not construct or return a Holder value"
    );

    let provider = function(module, static_name);
    assert!(
        matches!(provider.body.stmts.as_slice(), [Stmt::Expr(Expr::Var(name))] if name == "direct"),
        "the static provider must return only callable metadata, never a Holder value"
    );

    let closure = binding(module, "closure_value");
    assert!(
        matches!(closure, Expr::Lambda { body, .. }
            if matches!(body.stmts.as_slice(), [Stmt::Expr(Expr::Var(name))] if name == "owner")),
        "the closure may return the ordinary String view only"
    );

    let static_call = binding(module, "static_value");
    assert!(
        matches!(static_call, Expr::Call { args, .. } if args.is_empty()),
        "resolved static evidence must take zero runtime arguments, so it cannot transport Holder"
    );

    for name in [
        "direct_value",
        "alpha_value",
        "direct_observed",
        "alpha_observed",
        "closure_observed",
        "static_observed",
    ] {
        assert!(
            matches!(binding(module, name), Expr::Var(_)),
            "`{name}` must transport only a callable identity"
        );
    }

    // This test intentionally stops at checked structural facts. Do not add an
    // interpreter or codegen invocation until runtime Holder construction has a
    // separately accepted lowering contract.
}

#[derive(Copy, Clone)]
struct RuntimeCheckEnv {
    heap_check: bool,
    uaf_check: bool,
}

impl RuntimeCheckEnv {
    const fn default() -> Self {
        Self {
            heap_check: false,
            uaf_check: false,
        }
    }

    const fn checked_heap() -> Self {
        Self {
            heap_check: true,
            uaf_check: false,
        }
    }

    const fn checked_heap_uaf() -> Self {
        Self {
            heap_check: true,
            uaf_check: true,
        }
    }
}

static RUNTIME_ENV_LOCK: Mutex<()> = Mutex::new(());

fn with_runtime_check_env<R>(env: RuntimeCheckEnv, action: impl FnOnce() -> R) -> R {
    let _guard = RUNTIME_ENV_LOCK.lock().unwrap();

    let previous = [
        ("WITCHY_HEAP_CHECK".to_string(), std::env::var_os("WITCHY_HEAP_CHECK")),
        ("WITCHY_UAF_CHECK".to_string(), std::env::var_os("WITCHY_UAF_CHECK")),
    ];

    if env.heap_check {
        unsafe {
            std::env::set_var("WITCHY_HEAP_CHECK", "1");
        }
    } else {
        unsafe {
            std::env::remove_var("WITCHY_HEAP_CHECK");
        }
    }
    if env.uaf_check {
        unsafe {
            std::env::set_var("WITCHY_UAF_CHECK", "1");
        }
    } else {
        unsafe {
            std::env::remove_var("WITCHY_UAF_CHECK");
        }
    }

    let outcome = panic::catch_unwind(AssertUnwindSafe(action));

    for (name, previous) in previous {
        match previous {
            Some(value) => unsafe {
                std::env::set_var(name, value);
            },
            None => unsafe {
                std::env::remove_var(name);
            },
        }
    }

    match outcome {
        Ok(value) => value,
        Err(payload) => panic::resume_unwind(payload),
    }
}

const PARSER_ITERATOR_WORKLOAD: &str = r#"mode opt
type Parser('a):
    input: View(String, 'a)
    offset: Int

type TokenIter('a):
    input: View(String, 'a)
    index: Int

type Token('a):
    text: View(String, 'a)
    width: Int

fn parser(input: let('a) String) -> Parser('a):
    Parser(input, 2)

fn tokens(input: let('a) String) -> TokenIter('a):
    TokenIter(input, 3)

fn scan(input: let('a) String) -> Int:
    let p = parser(input)
    let it = tokens(p.input)
    let values: List(Token('a)) = [Token(p.input, p.offset), Token(it.input, it.index)]
    var total = 0
    for token in values:
        total = total + token.width
    total

fn main(console: Console):
    console.print("${scan(\"source\")}")
"#;

const ROOT_LIFECYCLE_WORKLOAD: &str = r#"mode opt
type Cursor('a):
    view: View(String, 'a)
    offset: Int

fn make(input: let('a) String) -> Cursor('a):
    Cursor(input, 7)

fn early(input: let('a) String) -> Int:
    var cursor = make(input)
    return cursor.offset

fn branch(input: let('a) String, take: Bool) -> Int:
    var cursor = make(input)
    if take:
        return cursor.offset
    cursor.offset

fn looped(input: let('a) String) -> Int:
    var cursor = make(input)
    var i = 0
    while (i < 3):
        i = i + 1
    cursor.offset + i

fn fail() -> Result(Int, String):
    Err("stop")

fn finish(input: let('a) String) -> Result(Int, String):
    var cursor = make(input)
    let value = fail()?
    Ok(cursor.offset + value)

fn main(console: Console):
    let input = "root"
    console.print("${early(input)}")
    console.print("${branch(input, true)}")
    console.print("${branch(input, false)}")
    console.print("${looped(input)}")
    let result = match finish(input):
        Ok(value) -> value
        Err(_) -> 0
    console.print("${result}")
"#;

#[test]
fn callable_shapes_share_one_exact_structural_owner_identity() {
    let parsed = witchy_syntax::parser::parse_module(CALLABLE_OWNER_MATRIX)
        .expect("parse RFC-0112 callable-owner matrix");
    let lowered = traits::lower_checked(parsed).expect("resolve the static trait method");
    let typed = typeck::annotate_checked(lowered).expect("typecheck callable-owner matrix");
    let module = typed.module();
    let facts = checked_facts(module, typed.table()).expect("one final checked access authority");

    let direct = facts
        .declaration("direct")
        .expect("direct declaration access identity");
    assert_holder_owner_contract(direct);

    let mut shapes = Vec::new();
    for name in [
        "direct_observed",
        "alpha_observed",
        "closure_value",
        "closure_observed",
        "static_value",
        "static_observed",
    ] {
        let signature = facts
            .callable_at(module, binding(module, name))
            .unwrap_or_else(|| panic!("missing checked callable identity for `{name}`"));
        assert_holder_owner_contract(signature);
        shapes.push(signature);
    }

    for signature in shapes {
        assert_same_access_identity(direct, signature);
    }

    let Expr::Call {
        name: static_name,
        args,
    } = binding(module, "static_value")
    else {
        panic!("trait lowering must resolve Catalog.callable() to a direct call")
    };
    assert!(args.is_empty());
    assert!(
        static_name.contains("SuppliesCallable")
            && static_name.contains("Catalog")
            && static_name.ends_with("callable"),
        "the static call must retain its concrete trait-impl identity: {static_name}"
    );
    let selected = facts
        .call_at(module, binding(module, "static_value"))
        .expect("checked resolved-static call identity");
    let declared = facts
        .declaration(static_name)
        .expect("lowered static trait declaration identity");
    assert_same_access_identity(declared, selected);

    assert_runtime_holder_construction_is_gated(module, static_name);
}

#[test]
fn relation_changing_callable_ascription_is_rejected() {
    let parsed = witchy_syntax::parser::parse_module(RELATION_CHANGING_ASCRIPTION)
        .expect("parse relation-changing callable ascription");
    let lowered = traits::lower_checked(parsed).expect("lower relation-changing fixture");
    let typed = typeck::annotate_checked(lowered)
        .expect("ordinary type shape remains valid before access-identity checking");
    let Err(error) = checked_facts(typed.module(), typed.table()) else {
        panic!("splitting one callable lifetime across owner positions must be rejected")
    };
    let diagnostic = error.to_string();
    assert_eq!(
        diagnostic,
        "function value `wrong` erases or changes its ownership/access contract (parameter 1 does not preserve BorrowRelation)"
    );
}

#[test]
fn borrowed_parser_and_iterator_shells_match_interpreter_and_wasm_without_materialization() {
    let checked = witchy::resolve_std_only_checked(PARSER_ITERATOR_WORKLOAD)
        .expect("resolve parser/iterator fixture");
    let interpreted = interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("interpreter RFC-0112 parser/iterator fixture");

    let runs = [
        ("default", RuntimeCheckEnv::default()),
        ("checked-heap", RuntimeCheckEnv::checked_heap()),
        ("checked-heap-and-uaf", RuntimeCheckEnv::checked_heap_uaf()),
    ];

    for (label, env) in runs {
        let (compiled, packed_alloc_calls, packed_alloc_bytes) =
            run_compiled_with_runtime_checks(&checked, env);
        assert_eq!(
            compiled,
            interpreted,
            "compiled parser/iterator fixture diverges from interpreter oracle under {label}"
        );
        assert_eq!(
            packed_alloc_calls,
            0,
            "packed allocation calls must stay zero for parser/iterator shell under {label}"
        );
        assert_eq!(
            packed_alloc_bytes,
            0,
            "packed allocation bytes must stay zero for parser/iterator shell under {label}"
        );
    }
}

#[test]
fn borrowed_shell_root_lifecycle_holds_under_checked_heap_and_uaf_modes() {
    let checked = witchy::resolve_std_only_checked(ROOT_LIFECYCLE_WORKLOAD)
        .expect("resolve borrowed shell lifecycle fixture");
    let interpreted = interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("interpreter RFC-0112 root-lifecycle fixture");
    let expected = vec!["7", "7", "7", "10", "0"];
    assert_eq!(interpreted, expected, "interpreter RFC-0112 root-lifecycle fixture");

    let runs = [
        ("checked-heap", RuntimeCheckEnv::checked_heap()),
        ("checked-heap-and-uaf", RuntimeCheckEnv::checked_heap_uaf()),
    ];
    for (label, env) in runs {
        let compiled = run_compiled_with_runtime_checks(&checked, env).0;
        assert_eq!(
            compiled,
            expected,
            "compiled root-lifecycle fixture diverges from interpreter under {label}"
        );
    }

    let compiled = run_compiled(&checked);
    assert_eq!(compiled, expected, "default compiled root-lifecycle fixture diverges from interpreter");
}
