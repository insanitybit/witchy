use super::*;

fn assert_exact_packed_tuple_transport(linked: &ast::Module, relay_suffix: &str) {
    let wir = codegen::assemble_wir_module(linked)
        .expect_lowered("the packed structural tuple lowers to WIR");
    let relay_name = wir
        .funcs
        .iter()
        .map(|function| function.name.as_str())
        .find(|name| name.ends_with(relay_suffix))
        .unwrap_or_else(|| panic!("missing `{relay_suffix}` in lowered WIR"));
    let wat = witchy_wir::wir::to_wat(&wir);

    let relay_signature = wat
        .lines()
        .find(|line| line.starts_with(&format!("  (func ${relay_name} ")))
        .unwrap_or_else(|| panic!("missing `{relay_name}` WAT signature: {wat}"));
    assert_eq!(
        relay_signature,
        format!("  (func ${relay_name} (param $pair i32) (result i32)"),
        "the direct callable transports one exact tuple pointer",
    );
    assert!(wat.contains(&format!("call ${relay_name}")), "tuple crosses `{relay_name}`: {wat}");

    let tuple_helper_signature = wat
        .lines()
        .find(|line| {
            line.starts_with("  (func $__witchy_packed_record_")
                && line.contains("(param $f0 i32) (param $f1 i32) (result i32)")
        })
        .unwrap_or_else(|| panic!("missing exact `(Point, Bool)` tuple helper: {wat}"));
    let tuple_helper = tuple_helper_signature
        .trim_start()
        .strip_prefix("(func $")
        .expect("tuple helper signature prefix")
        .split_whitespace()
        .next()
        .expect("tuple helper name");
    assert!(
        wat.contains(&format!("call ${tuple_helper}")),
        "the tuple constructor uses its canonical descriptor helper: {wat}",
    );
    assert!(!wat.contains("call $mk2"), "no universal two-slot tuple reshape: {wat}");

    let relay_start = wat
        .find(&format!("  (func ${relay_name} "))
        .expect("relay function start");
    let relay_tail = &wat[relay_start..];
    let relay_end = relay_tail[1..]
        .find("\n  (func $")
        .map(|offset| offset + 1)
        .unwrap_or(relay_tail.len());
    let relay = &relay_tail[..relay_end];
    assert!(relay.contains("local.get $pair"), "relay returns the exact incoming pointer: {relay}");
    assert!(!relay.contains("call $"), "relay performs no boxing or reshape: {relay}");
}

#[test]
fn exact_layout_direct_values_and_ownership_backends_agree() {
    let source = r#"
mode opt

type Point packed:
    x: Int
    y: Int

type Token packed:
    Skip
    Value(Int)

fn make() -> Point:
    Point(7, 11)

fn relay(points: List(Point)) -> List(Point):
    points

fn pass(own token: Token) -> Token:
    token

fn score(let token: Token) -> Int:
    match token:
        Skip -> 9
        Value(value) -> value

fn main(console: Console):
    let points = relay([Point(1, 2), Point(3, 4)])
    let answer = make().x * 1000 + list.at(points, 0).x * 100
        + list.length(points) + score(pass(Value(5)))
    console.print("${answer}")
"#;
    let expected = vec!["7107".to_string()];
    assert_eq!(interp(source), expected, "interpreter oracle");
    assert_eq!(run_linked_on_wasm(&[("main", source)], "main"), expected, "Wasm");
}

/// (RFC-0111 criterion 10) The joint cross-lever acceptance slice: one program
/// that exercises every specialized-layout feature — packed records, a packed
/// `List(P)` stream, a fixed-layout closed sum, a generic helper specialized on
/// the packed type, and a destination-built unique result — run under the full
/// runtime contract at once:
///   * interpreter oracle vs compiled Wasm parity;
///   * the compiled runtime's always-on checked heap (trailing-redzone poison
///     sweep after `actor.run()` in Runtime::run) — a packed write past an
///     object end or a use-after-free of a reclaimed packed buffer traps here;
///   * a cross-lever de-opt sweep: `OptSet::none()`, every single lever, and
///     `all()` must all reproduce the exact oracle value, so no lever (or lever
///     interaction) silently changes a specialized-layout result.
/// Artifact/descriptor-bundle compatibility (criterion 5) is proven by the
/// `witchy-wir::layout` canonical encode/decode/import tests; this slice is the
/// runtime half of criterion 10.
#[test]
fn cross_lever_specialized_layout_slice_is_green_on_every_lever_and_backend() {
    // Exercises packed records, a packed `List(P)` built and traversed, a
    // fixed-layout closed sum matched twice, and packed-field reads — the
    // specialized-layout surface without an in-place packed mutation (that path,
    // which is only supported with the `inplace` lever on, is covered by
    // `exact_packed_list_stream_backends_agree`; toggling `inplace` off there is
    // a fail-closed rejection, not a value change, so it is intentionally not in
    // this every-lever value-equivalence sweep).
    let source = r#"
mode opt
import list

type Point packed:
    x: Int
    y: Int

type Token packed:
    Skip
    Value(Int)

fn make_origin() -> Point:
    Point(3, 4)

fn score(let token: Token) -> Int:
    match token:
        Skip -> 100
        Value(value) -> value

fn main(console: Console):
    let points = [Point(0, 0), Point(1, 2), Point(2, 4), Point(3, 6), Point(4, 8)]
    var total = 0
    for point in points:
        total = total + point.x * 5 + point.y
    let answer = total * 1000
        + make_origin().x * 100
        + list.length(points) * 10
        + score(Value(7)) + score(Skip)
    console.print("${answer}")
"#;
    // Independent oracle: the interpreter, which has no specialized layouts.
    let expected = interp(source);
    // Cross-lever de-opt sweep: none, all, and each single lever toggled off from
    // all. Every configuration must reproduce the exact oracle value, so no lever
    // (or its removal) silently changes a specialized-layout result. The `all`
    // variant also supplies the compiled-Wasm parity and checked-heap run: the
    // print-only runtime still performs Runtime::run's trailing-redzone poison
    // sweep. Linking only depends on the Fold lever; share its two checked
    // snapshots and compile the rest of the independent configurations in
    // bounded parallel batches.
    use crate::opt::{self, Opt, OptSet};
    let link_with = |set| {
        opt::set_for_tests(Some(set));
        let module = parser::parse_module(source).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        opt::set_for_tests(None);
        linked
    };
    let unfolded_linked = link_with(OptSet::none());
    let folded_linked = link_with(OptSet::all());
    let mut sets: Vec<(String, OptSet, bool)> = vec![
        ("none".to_string(), OptSet::none(), false),
        ("all".to_string(), OptSet::all(), true),
    ];
    for opt in Opt::ALL {
        sets.push((
            format!("all-without-{}", opt.name()),
            OptSet::all().without(opt),
            opt != Opt::Fold,
        ));
    }
    let outputs = std::thread::scope(|scope| {
        let workers = sets
            .chunks(6)
            .map(|batch| {
                let unfolded_linked = &unfolded_linked;
                let folded_linked = &folded_linked;
                scope.spawn(move || {
                    batch
                        .iter()
                        .map(|(label, set, folded)| {
                            crate::opt::set_for_tests(Some(*set));
                            let linked = if *folded { folded_linked } else { unfolded_linked };
                            let bytes = codegen::compile_module_binary(linked)
                                .expect_lowered("specialized layout lowers under every lever");
                            let got = run_bytes_print_only(&bytes);
                            crate::opt::set_for_tests(None);
                            (label.clone(), got)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .flat_map(|worker| worker.join().expect("optimizer sweep worker"))
            .collect::<Vec<_>>()
    });
    for (label, got) in outputs {
        assert_eq!(
            got, expected,
            "specialized-layout result diverges under WITCHY_OPT={label}"
        );
    }
}
