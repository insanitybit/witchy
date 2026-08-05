use super::*;
use witchy_syntax::parser::parse_module;

struct ScalarResolver;

impl witchy_wir::layout::ClosedTypeResolver for ScalarResolver {
    fn resolve_named<'a>(
        &'a self,
        name: &str,
        _arguments: &[Type],
    ) -> Option<witchy_wir::layout::ResolvedNamed<'a>> {
        match name {
            "Int" => Some(witchy_wir::layout::ResolvedNamed::Scalar(
                witchy_wir::layout::ScalarKind::Int,
            )),
            "Bool" => Some(witchy_wir::layout::ResolvedNamed::Scalar(
                witchy_wir::layout::ScalarKind::Bool,
            )),
            _ => None,
        }
    }
}

fn known_layouts() -> (LayoutInterner, LayoutId, LayoutId) {
    let mut layouts = LayoutInterner::new();
    let int = layouts
        .intern_type(&Type::Named("Int".into(), Vec::new()), &ScalarResolver)
        .expect("canonical Int descriptor");
    let boolean = layouts
        .intern_type(&Type::Named("Bool".into(), Vec::new()), &ScalarResolver)
        .expect("canonical Bool descriptor");
    (layouts, int, boolean)
}

fn assert_layout_id_in(diagnostic: &str) {
    assert!(
        diagnostic
            .as_bytes()
            .windows(64)
            .any(|candidate| candidate.iter().all(u8::is_ascii_hexdigit)),
        "diagnostic must carry a canonical LayoutId: {diagnostic}",
    );
}

#[test]
fn callable_layout_classifier_distinguishes_exact_mismatch_and_unknown() {
    let (layouts, int, boolean) = known_layouts();
    let producer = CallableLayoutSignature::new(vec![Some(int)], Some(boolean));
    let exact = CallableLayoutSignature::new(vec![Some(int)], Some(boolean));
    let mismatch = CallableLayoutSignature::new(vec![Some(boolean)], Some(boolean));
    let unknown_id = LayoutId::from_bytes([0xff; 32]);
    let unknown = CallableLayoutSignature::new(vec![Some(unknown_id)], Some(boolean));

    assert_eq!(
        callable_layout::classify_callable_layouts(&layouts, &producer, &exact),
        callable_layout::CallableLayoutClassification::Exact,
    );
    assert_eq!(
        callable_layout::classify_callable_layouts(&layouts, &producer, &mismatch),
        callable_layout::CallableLayoutClassification::Mismatch,
    );
    assert_eq!(
        callable_layout::classify_callable_layouts(&layouts, &producer, &unknown),
        callable_layout::CallableLayoutClassification::Unknown(unknown_id),
    );
}

#[test]
fn named_function_value_reports_an_exact_specialized_signature_before_rejection() {
    let module = parse_module(
        r#"
mode opt
type Point packed:
    x: Int
fn relay(point: Point) -> Point:
    point
fn invoke(f: fn(Point) -> Point, point: Point) -> Point:
    f(point)
fn main() -> Int:
    invoke(relay, Point(7)).x
"#,
    )
    .expect("parse named function-value fixture");
    let error = compile_module_binary(&module)
        .expect_rejected("specialized named function values remain fail-closed");
    let diagnostic = error.to_string();

    assert!(
        diagnostic.contains("first-class function call")
            && diagnostic.contains("callable-layout=exact")
            && diagnostic.contains("params=[")
            && diagnostic.contains("result="),
        "{diagnostic}",
    );
    assert_layout_id_in(&diagnostic);
}

#[test]
fn lambda_application_reports_an_exact_specialized_signature_before_rejection() {
    let module = parse_module(
        r#"
mode opt
type Point packed:
    x: Int
fn invoke(f: fn(Point) -> Point, point: Point) -> Point:
    f(point)
fn main() -> Int:
    invoke(fn(point: Point) -> Point: point, Point(7)).x
"#,
    )
    .expect("parse lambda application fixture");
    let error = compile_module_binary(&module)
        .expect_rejected("specialized lambda application remains fail-closed");
    let diagnostic = error.to_string();

    assert!(
        diagnostic.contains("first-class function call")
            && diagnostic.contains("callable-layout=exact"),
        "{diagnostic}",
    );
    assert_layout_id_in(&diagnostic);
}

#[test]
fn specialized_closure_capture_is_rejected_with_its_exact_layout() {
    let module = parse_module(
        r#"
mode opt
type Point packed:
    x: Int
fn main() -> Int:
    let point = Point(7)
    let read = fn() -> Int: point.x
    read()
"#,
    )
    .expect("parse specialized capture fixture");
    let indirect = witchy_syntax::opt::OptSet::default_set()
        .without(witchy_syntax::opt::Opt::DirectCall)
        .without(witchy_syntax::opt::Opt::ClosureElide);
    witchy_syntax::opt::set_for_tests(Some(indirect));
    let result = compile_module_binary(&module);
    witchy_syntax::opt::set_for_tests(None);
    let error = result
        .expect_rejected("a specialized value cannot enter the legacy closure environment");
    let diagnostic = error.to_string();

    assert!(
        diagnostic.contains("closure capture")
            && diagnostic.contains("specialized capture `point`")
            && diagnostic.contains("callable-layout LayoutId"),
        "{diagnostic}",
    );
    assert_layout_id_in(&diagnostic);
}

#[test]
fn existential_call_keeps_rejecting_with_an_unresolved_physical_signature() {
    let module = parse_module(
        r#"
mode opt
type Point packed:
    x: Int
trait Maker:
    fn make(let self, value: Int) -> Point
type Seed:
    Seed
impl Maker for Seed:
    fn make(let self, value: Int) -> Point:
        Point(value)
fn main() -> Int:
    let maker: dyn Maker = Seed
    maker.make(11).x
"#,
    )
    .expect("parse existential-call fixture");
    let error = compile_module_binary(&module)
        .expect_rejected("specialized witness calls remain fail-closed");
    let diagnostic = error.to_string();

    assert!(
        diagnostic.contains("trait/existential method `make`")
            && diagnostic.contains("callable-layout=trait-existential-unresolved"),
        "{diagnostic}",
    );
}
