//! Source-bound pattern decisions, checked and executed in normal and opt modes.

use super::short_circuit_tests::assert_execution;

#[test]
fn range_patterns_backends_agree() {
    assert_execution(
        "fn classify(n: Int) -> Int:\n    match n:\n        0..10 -> 1\n        10..=20 -> 2\n        _ -> 3\n\nfn main() -> Int:\n    classify(5) + classify(10) * 10 + classify(20) * 100 + classify(99) * 1000\n",
        3221,
    );
}

#[test]
fn nested_or_patterns_backends_agree() {
    assert_execution(
        "fn f(o: Option(Int)) -> Int:\n    match o:\n        Some(1 | 2 | 3) -> 1\n        Some(n) -> n\n        None -> 0\n\nfn main() -> Int:\n    f(Some(2)) + f(Some(9)) * 10 + f(None) * 100\n",
        91,
    );
}

#[test]
fn binding_or_patterns_backends_agree() {
    assert_execution(
        "type Shape:\n    Circle(Int)\n    Square(Int)\n\nfn size(s: Shape) -> Int:\n    match s:\n        Circle(n) | Square(n) -> n\n\nfn main() -> Int:\n    size(Circle(3)) + size(Square(7)) * 10\n",
        73,
    );
}

#[test]
fn range_endpoints_and_nested_ranges_do_not_overflow() {
    assert_execution(
        "fn edge(n: Int) -> Int:\n    match n:\n        -9223372036854775808..=-9223372036854775807 -> 1\n        9223372036854775806..=9223372036854775807 -> 2\n        -2..0 -> 3\n        _ -> 4\n\nfn nested(o: Option(Int)) -> Int:\n    match o:\n        Some(1..=3) | Some(7..9) -> 1\n        _ -> 0\n\nfn main() -> Int:\n    edge(-9223372036854775808) + edge(-9223372036854775807) * 10 + edge(9223372036854775807) * 100 + edge(-1) * 1000 + edge(0) * 10000 + nested(Some(8)) * 100000 + nested(Some(9)) * 1000000\n",
        143211,
    );
}

#[test]
fn overlapping_alternatives_select_first_bindings_and_never_retry_a_guard() {
    assert_execution(
        "fn select(p: (Int, Int)) -> Int:\n    match p:\n        (x, 2) | (1, x) -> x\n        _ -> 9\n\nfn guarded(p: (Int, Int)) -> Int:\n    match p:\n        (x, 2) | (1, x) if x == 2 -> x\n        _ -> 9\n\nfn main() -> Int:\n    select((1, 2)) + select((1, 3)) * 10 + guarded((1, 2)) * 100 + guarded((2, 2)) * 1000\n",
        2931,
    );
}

#[test]
fn nested_product_alternatives_keep_active_variant_and_list_length_guards() {
    assert_execution(
        "fn pick(o: Option((Int, List(Int)))) -> Int:\n    match o:\n        Some((1 | 2, [3 | 4, ..tail])) ->\n            match tail:\n                [9] -> 7\n                [] -> 6\n                _ -> 0\n        _ -> 0\n\nfn main() -> Int:\n    pick(Some((2, [4, 9]))) + pick(Some((1, [3]))) * 10 + pick(Some((2, []))) * 100 + pick(None) * 1000 + pick(Some((9, [4, 9]))) * 10000\n",
        67,
    );
}

#[test]
fn reference_binding_phis_preserve_snapshots_and_release_owned_payloads() {
    assert_execution(
        "type Choice:\n    Left(List(String))\n    Right(List(String))\n    Empty\n\nfn pick(c: Choice) -> List(String):\n    match c:\n        Left(xs) | Right(xs) -> xs\n        Empty -> []\n\nfn main() -> Int:\n    var original = [\"a\"]\n    let saved = original\n    let left = Left(original)\n    let right = Right([\"b\", \"c\"])\n    let a = pick(left)\n    let b = pick(right)\n    let empty_ok = match pick(Empty):\n        [] -> true\n        _ -> false\n    original[0] = \"changed\"\n    if saved[0] == \"a\" && a[0] == \"a\" && b[0] == \"b\" && b[1] == \"c\" && original[0] == \"changed\" && empty_ok: 1 else: 0\n",
        1,
    );
}

#[test]
fn tail_or_patterns_keep_one_body_and_one_source_tail_site() {
    assert_execution(
        "fn descend(n: Int) -> Int:\n    match n:\n        0 | 1 -> n\n        _ -> descend(n - 1)\n\nfn main() -> Int:\n    descend(40)\n",
        1,
    );
}

#[test]
fn nested_binding_alternatives_and_guard_failures_release_reference_tokens() {
    assert_execution(
        "type Choice:\n    Left(List(String))\n    Right(List(String))\n\ntype Envelope:\n    Wrap(Choice)\n    Absent\n\nfn pick(e: Envelope) -> String:\n    match e:\n        Wrap(Left(xs) | Right(xs)) if xs[0] == \"take\" -> xs[1]\n        Wrap(_) -> \"skip\"\n        Absent -> \"none\"\n\nfn main() -> Int:\n    var original = [\"take\", \"old\"]\n    let saved = original\n    let envelope = Wrap(Left(original))\n    let first = pick(envelope)\n    original[1] = \"new\"\n    let second = pick(Wrap(Right([\"skip\", \"bad\"])))\n    let third = pick(Absent)\n    if first == \"old\" && saved[1] == \"old\" && original[1] == \"new\" && second == \"skip\" && third == \"none\": 1 else: 0\n",
        1,
    );
}
