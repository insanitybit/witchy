use super::*;
use witchy_syntax::parser::parse_module;

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

