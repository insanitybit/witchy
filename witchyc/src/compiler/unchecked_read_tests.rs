//! Reads with no test, by the compiler's own reasoning (docs/wasm-iteration.md): which shapes
//! lose a slice read's bounds test, and which keep it. Read off the trap report, as a user would.

/// The trap report line of `name` in a program whose `main` calls each function on a slice.
fn line(functions: &str, name: &str) -> String {
    let source = format!("{functions}\nfn main():\n    let a: [U64; 4] = [1, 0, 2, 0]\n    let s = a.slice()\n    var b: [U64; 4] = [1, 0, 2, 0]\n    let t = {name}(s)\n    let u = b.slice_mut()\n    return\n");
    let syntax = crate::compiler::parse(&source).expect("parses");
    let logical = super::check::program(&syntax).expect("checks");
    let program = super::concrete::program(logical).expect("lowers");
    super::traps::report(&program).lines().find(|l| l.starts_with(&format!("{name}:"))).unwrap_or("").to_owned()
}
fn untested(functions: &str, name: &str) -> bool { !line(functions, name).contains("index out of range") }

#[test]
fn a_counted_for_up_to_a_shared_slice_reads_untested() {
    assert!(untested("fn f(s: &[U64]) -> U64:\n    var t: U64 = 0\n    for j in 0..s.len():\n        t = t ^ s[j]\n    return t\n", "f"));
    // Another index, or another slice's length, keeps the test.
    assert!(!untested("fn f(s: &[U64]) -> U64:\n    var t: U64 = 0\n    for j in 0..s.len():\n        if j > 0:\n            t = t ^ s[j - 1]\n    return t\n", "f"));
    assert!(!untested("fn f(s: &[U64]) -> U64:\n    var t: U64 = 0\n    for j in 0..5:\n        if j < 4:\n            t = t ^ s[j]\n    return t\n", "f"));
}

#[test]
fn a_test_before_and_keeps_its_reads_untested_unless_the_counter_may_change() {
    assert!(untested("fn f(s: &[U64]) -> U64:\n    var i: U64 = 0\n    while i < s.len() && s[i] == 0:\n        i += 1\n    return i\n", "f"));
    assert!(untested("fn f(s: &[U64]) -> U64:\n    var i: U64 = 0\n    while s.len() > i && s[i] == 0:\n        i += 1\n    return i\n", "f"));
    // `&mut i` anywhere in the expression (`&&` groups to the left), or `<=`, keeps the test.
    assert!(!untested("fn bump(r: &mut U64) -> Bool:\n    *r = 9\n    return true\n\nfn f(s: &[U64]) -> U64:\n    var i: U64 = 0\n    if i < s.len() && bump(&mut i) && s[i] > 0:\n        return 1\n    return 0\n", "f"));
    assert!(!untested("fn f(s: &[U64]) -> U64:\n    var i: U64 = 0\n    if i <= s.len() && s[i] > 0:\n        return 1\n    return 0\n", "f"));
}

#[test]
fn a_while_reads_untested_only_before_the_counter_may_change() {
    assert!(untested("fn f(s: &[U64]) -> U64:\n    var t: U64 = 0\n    var i: U64 = 0\n    while i < s.len():\n        t = t ^ s[i]\n        if s[i] > 1:\n            t = t ^ 1\n        i += 1\n    return t\n", "f"));
    assert!(!untested("fn f(s: &[U64]) -> U64:\n    var t: U64 = 0\n    var i: U64 = 0\n    while i < s.len():\n        if s[i] == 0:\n            i += 1\n        t = t ^ s[i]\n        i += 1\n    return t\n", "f"));
}
