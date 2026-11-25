//! I64. What must hold: an I64 and an unsigned number never meet in one operation, a literal
//! fits its type, and the proof models, which read numbers as naturals, never read an I64.
use crate::diagnostic::Diagnostic;
fn checked(body: &str) -> Result<(), Diagnostic> {
    let source = format!("{body}\nfn main(console: Console<Write>):\n    return\n");
    let logical = super::check::program(&super::parse(&source)?)?; super::concrete::program(logical)?; Ok(())
}
fn refused(body: &str, why: &str) { let message = checked(body).expect_err(body).message.to_string(); assert!(message.contains(why), "{body}: {message}"); }
#[test]
fn signed_values_are_written_computed_compared_and_stored() {
    checked("fn f(a: I64, b: I64) -> I64:\n    return (a + b) * (a - b) / 2 % 7\n").unwrap();
    checked("fn f(a: I64) -> Bool:\n    return a < 0 || a >= 5 && a != 7 || a == -3 || a <= -9223372036854775808 || a > 9223372036854775807\n").unwrap();
    checked("fn f(a: I64) -> I64:\n    let b = -a\n    let c: I64 = 4\n    var d = b + c\n    d += 1\n    d -= -2\n    return d\n").unwrap();
    checked("type Point:\n    x: I64\n    y: I64\n\nfn f(point: &mut Point, cells: &mut [I64; 3], at: U64):\n    point.x += 1\n    point.y = -point.x\n    cells[at] -= point.y\n    cells[2] = cells[0] * 2\n").unwrap();
    checked("fn f(a: U64) -> U64:\n    return I64.bits(I64.from_bits(a) - 1)\n").unwrap();
    // A scalar contract may stand beside I64 values it does not speak of.
    checked("fn f(a: I64, n: U64) -> U64:\n    requires n < 10\n    ensures result == n + 1\n    proof:\n        auto\n    let b = a * 2\n    if b > 5:\n        return n + 1\n    return 1 + n\n").unwrap();
}
#[test]
fn signed_and_unsigned_do_not_mix() {
    refused("fn f(a: I64, b: U64) -> I64:\n    return a + b\n", "");
    refused("fn f(a: I64, b: U64) -> Bool:\n    return a < b\n", "");
    refused("fn f(a: I64) -> U64:\n    return a\n", "");
    refused("fn f(a: U64) -> I64:\n    return a\n", "");
    refused("fn f(a: U64) -> U64:\n    return -a\n", "");
    refused("fn f(a: I64) -> U64:\n    let b: U64 = a + 1\n    return b\n", "");
    refused("fn f(a: U8) -> I64:\n    return I64.from_bits(a)\n", "");
    refused("fn f(a: U64) -> U64:\n    return I64.bits(a)\n", "");
}
#[test]
fn a_literal_fits_its_type() {
    checked("fn f() -> I64:\n    return 9223372036854775807\n").unwrap();
    checked("fn f() -> I64:\n    return -9223372036854775808\n").unwrap();
    refused("fn f() -> I64:\n    return 9223372036854775808\n", "exceeds the range of I64");
    refused("fn f() -> I64:\n    return -9223372036854775809\n", "exceeds the range of I64");
    refused("fn f() -> U64:\n    return -1\n", "");
}
#[test]
fn a_clause_names_signed_values_and_keeps_them_apart_from_unsigned_ones() {
    // What the clauses may say. Whether a claim is true is Lean's to decide (`tests/fixtures/signed`).
    checked("fn f(a: I64) -> I64:\n    requires a > -9223372036854775808\n    requires a <= 9223372036854775807\n    ensures result == -a\n    ensures result >= -9223372036854775807\n    proof:\n        auto\n    return -a\n").unwrap();
    checked("type Point:\n    x: I64\n    y: I64\n\nimpl Point:\n    fn across(self: &Point) -> I64:\n        return self.x\n\nfn f(point: &mut Point, by: I64):\n    requires point.across() >= -5\n    requires point.x <= 5\n    requires by >= -5\n    requires by <= 5\n    ensures point.x == old(point.x) + by\n    proof:\n        auto\n    point.x += by\n").unwrap();
    checked("fn f(n: I64) -> I64:\n    requires n >= 0\n    requires n <= 100\n    ensures result == 0 - n\n    proof:\n        auto\n    var left = n\n    var total: I64 = 0\n    while left > 0:\n        invariant left >= 0\n        invariant total == left - n\n        total -= 1\n        left -= 1\n    return total\n").unwrap();
    const APART: &str = "compare an I64 with an I64";
    refused("fn f(a: I64, n: U64) -> U64:\n    requires a < n\n    ensures result == 1\n    proof:\n        auto\n    return 1\n", APART);
    refused("fn f(a: I64, n: U64) -> U64:\n    requires a + n < 5\n    ensures result == 1\n    proof:\n        auto\n    return 1\n", APART);
    refused("fn f(a: I64, n: U64) -> I64:\n    ensures result == n\n    proof:\n        auto\n    return a\n", APART);
    refused("fn f(n: U64) -> U64:\n    requires n > -1\n    ensures result == 1\n    proof:\n        auto\n    return 1\n", APART);
    refused("type Box:\n    n: I64\n\nimpl Box:\n    fn get(self: &Box) -> I64:\n        return self.n\n\nfn f(box: &Box, k: U64) -> U64:\n    requires box.get() < k\n    ensures result == 1\n    proof:\n        auto\n    return 1\n", APART);
    refused("fn f(a: I64, n: U64) -> U64:\n    ensures result == 1\n    proof:\n        auto\n    var count: U64 = 0\n    while count < n:\n        invariant count <= a\n        count += 1\n    return 1\n", APART);
    // `/` and `%` round toward zero (Lean's `Int.tdiv`, `Int.tmod`); a clause may divide an I64.
    checked("fn f(a: I64) -> I64:\n    ensures result == a / 2\n    proof:\n        auto\n    return a / 2\n").expect("divides");
    checked("fn f(a: I64) -> I64:\n    requires a % 2 == 0\n    ensures result == a\n    proof:\n        auto\n    return a\n").expect("divides");
    refused("fn f(a: I64) -> I64:\n    requires a < 9223372036854775808\n    ensures result == a\n    proof:\n        auto\n    return a\n", "exceeds the range of I64");
    refused("fn f(a: I64) -> I64:\n    requires a > -9223372036854775809\n    ensures result == a\n    proof:\n        auto\n    return a\n", "exceeds the range of I64");
    // The memory model follows only the operations it was written for.
    refused("fn f(buffer: &mut Buffer<U64>, a: I64):\n    requires memory.capacity(buffer) > 0\n    requires memory.empty(buffer, 0)\n    ensures memory.cell(buffer, 0, 1)\n    proof:\n        auto\n    let b = a + 1\n    memory.init_unchecked(buffer, 0, 1)\n", "");
}
