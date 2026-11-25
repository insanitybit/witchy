//! Records that hold `&` references. What must hold: such a value keeps everything it was made
//! from lent for as long as it, a copy of it, or anything read out of it is in use; it is never
//! stored, never pointed at, and never returned past what it borrows.
use crate::diagnostic::Diagnostic;
const PRELUDE: &str = "type Point:\n    x: U64\n    y: U64\n\ntype Seen:\n    point: &Point\n    n: U64\n\nfn sum(seen: Seen) -> U64:\n    let point = seen.point\n    return point.x + point.y + seen.n\n\nfn bump(point: &mut Point):\n    point.x += 1\n\nfn make(point: &Point) -> Seen:\n    return Seen(point, 2)\n\nfn find(point: &Point, n: U64) -> Result<Seen, U64>:\n    if n > 9:\n        return Err(n)\n    return Ok(Seen(point, n))\n\n";
const LENT: &str = "this value is lent";
fn checked(body: &str) -> Result<(), Diagnostic> {
    let source = format!("{PRELUDE}{body}\nfn main(console: Console<Write>):\n    return\n");
    let logical = super::check::program(&super::parse(&source)?)?; super::concrete::program(logical)?; Ok(())
}
fn refused(body: &str, why: &str) { let message = checked(body).expect_err(body).message.to_string(); assert!(message.contains(why), "{body}: {message}"); }
#[test]
fn a_view_is_made_copied_read_passed_and_returned() {
    checked("fn run() -> U64:\n    var point = Point(3, 4)\n    let seen = Seen(&point, 5)\n    let again = seen\n    let first = sum(seen) + sum(again)\n    bump(&mut point)\n    return first + sum(Seen(&point, 1))\n").unwrap();
    checked("fn run(point: &Point) -> Seen:\n    let seen = Seen(point, 2)\n    return seen\n").unwrap();
    checked("fn run(seen: Seen) -> Seen:\n    return Seen(seen.point, seen.n + 1)\n").unwrap();
    checked("fn run(seen: Seen) -> &Point:\n    return seen.point\n").unwrap();
    checked("fn run() -> U64:\n    let point = Point(3, 4)\n    var seen = make(&point)\n    seen.n = 7\n    seen.n += 1\n    return sum(seen)\n").unwrap();
    checked("fn run() -> U64:\n    let point = Point(3, 4)\n    match find(&point, 3):\n        Ok(seen):\n            return sum(seen)\n        Err(code):\n            return code\n").unwrap();
    checked("type Holder:\n    seen: Seen\n    count: U64\n\nfn run() -> U64:\n    let point = Point(3, 4)\n    let holder = Holder(Seen(&point, 1), 2)\n    return sum(holder.seen) + holder.count\n").unwrap();
    checked("fn run() -> U64:\n    let point = Point(3, 4)\n    let pair = .{count: 2, seen: Seen(&point, 1)}\n    return sum(pair.seen) + pair.count\n").unwrap();
    checked("type Slice<T>:\n    item: &T\n    n: U64\n\nfn head<T>(slice: Slice<T>) -> &T:\n    return slice.item\n\nfn run() -> U64:\n    let point = Point(3, 4)\n    let slice: Slice<Point> = Slice(&point, 1)\n    let inner = head(slice)\n    return inner.x\n").unwrap();
    checked("impl Seen:\n    fn total(self: Seen) -> U64:\n        return sum(self)\n\nfn run() -> U64:\n    let point = Point(3, 4)\n    return Seen(&point, 1).total()\n").unwrap();
}
#[test]
fn what_a_view_borrows_stays_lent_while_the_view_is_in_use() {
    refused("fn run() -> U64:\n    var point = Point(3, 4)\n    let seen = Seen(&point, 5)\n    bump(&mut point)\n    return sum(seen)\n", LENT);
    refused("fn run() -> U64:\n    var point = Point(3, 4)\n    let seen = Seen(&point, 5)\n    point.x = 9\n    return sum(seen)\n", LENT);
    // A copy, a reference read out of it, a record or a sum around it, and a view made from it by a call.
    refused("fn run() -> U64:\n    var point = Point(3, 4)\n    let seen = Seen(&point, 5)\n    let again = seen\n    bump(&mut point)\n    return sum(again)\n", LENT);
    refused("fn run() -> U64:\n    var point = Point(3, 4)\n    let seen = Seen(&point, 5)\n    let inner = seen.point\n    bump(&mut point)\n    return inner.x\n", LENT);
    refused("type Holder:\n    seen: Seen\n    count: U64\n\nfn run() -> U64:\n    var point = Point(3, 4)\n    let holder = Holder(Seen(&point, 1), 2)\n    bump(&mut point)\n    return holder.count\n", LENT);
    refused("fn run() -> U64:\n    var point = Point(3, 4)\n    let pair = .{count: 2, seen: Seen(&point, 1)}\n    bump(&mut point)\n    return pair.count\n", LENT);
    refused("fn run() -> U64:\n    var point = Point(3, 4)\n    let found = find(&point, 3)\n    bump(&mut point)\n    match found:\n        Ok(seen):\n            return sum(seen)\n        Err(code):\n            return code\n", LENT);
    refused("fn run() -> U64:\n    var point = Point(3, 4)\n    match find(&point, 3):\n        Ok(seen):\n            bump(&mut point)\n            return sum(seen)\n        Err(code):\n            return code\n", LENT);
    refused("fn more(seen: Seen) -> Seen:\n    return Seen(seen.point, seen.n + 1)\n\nfn run() -> U64:\n    var point = Point(3, 4)\n    let seen = more(make(&point))\n    bump(&mut point)\n    return sum(seen)\n", LENT);
    refused("fn inner(seen: Seen) -> &Point:\n    return seen.point\n\nfn run() -> U64:\n    var point = Point(3, 4)\n    let inner = inner(make(&point))\n    bump(&mut point)\n    return inner.x\n", LENT);
    // A view declared before a loop is in use through the whole loop.
    refused("fn run() -> U64:\n    var point = Point(3, 4)\n    let seen = Seen(&point, 5)\n    var total: U64 = 0\n    for round in 0..3:\n        total += sum(seen)\n        bump(&mut point)\n    return total\n", LENT);
    // Beside a `&mut` to the same value in one call.
    refused("fn both(seen: Seen, point: &mut Point) -> U64:\n    return 0\n\nfn run() -> U64:\n    var point = Point(3, 4)\n    let seen = Seen(&point, 5)\n    return both(seen, &mut point)\n", "");
    // From a `&mut` parameter, lent on as `&`.
    refused("fn run(point: &mut Point) -> U64:\n    let seen = Seen(&*point, 5)\n    bump(point)\n    return sum(seen)\n", "");
    checked("fn run(point: &mut Point) -> U64:\n    let seen = Seen(&*point, 5)\n    let first = sum(seen)\n    bump(point)\n    return first\n").unwrap();
}
#[test]
fn a_view_is_not_returned_past_what_it_borrows() {
    const ORIGIN: &str = "must borrow from a reference parameter";
    refused("fn run() -> Seen:\n    let point = Point(3, 4)\n    return Seen(&point, 1)\n", ORIGIN);
    refused("fn run() -> Seen:\n    let point = Point(3, 4)\n    let seen = Seen(&point, 1)\n    return seen\n", ORIGIN);
    refused("fn run(point: Point) -> Seen:\n    return Seen(&point, 1)\n", ORIGIN);
    refused("fn run() -> Result<Seen, U64>:\n    let point = Point(3, 4)\n    return Ok(Seen(&point, 1))\n", ORIGIN);
    refused("fn run() -> &Point:\n    let point = Point(3, 4)\n    let seen = Seen(&point, 1)\n    return seen.point\n", ORIGIN);
    refused("fn run() -> .{count: U64, seen: Seen}:\n    let point = Point(3, 4)\n    return .{count: 1, seen: Seen(&point, 1)}\n", ORIGIN);
}
#[test]
fn a_view_is_never_stored_pointed_at_or_rebound() {
    // A shared reference to a view is a type (docs/design-references-to-views.md); `&mut` is not yet.
    checked("fn run(seen: &Seen) -> U64:\n    return 0\n").unwrap();
    checked("fn run(seen: &mut Seen) -> U64:\n    (*seen).n += 1\n    return 0\n").unwrap();
    refused("fn run(seen: &mut Seen, other: Seen):\n    *seen = other\n", "only its fields that hold none are changed");
    // A part that holds no reference is lent like any field (the view has a place in the frame).
    checked("fn look(n: &U64) -> U64:\n    return *n\n\nfn run(point: &Point) -> U64:\n    let seen = Seen(point, 1)\n    return look(&seen.n)\n").unwrap();
    checked("fn set(n: &mut U64):\n    *n = 1\n\nfn run(point: &Point) -> U64:\n    var seen = Seen(point, 1)\n    set(&mut seen.n)\n    return seen.n\n").unwrap();
    checked("fn run(point: &Point) -> U64:\n    let seen = Seen(point, 1)\n    let kept = &seen.n\n    return *kept\n").unwrap();
    refused("type Cursor:\n    point: &mut Point\n\nfn run() -> U64:\n    return 0\n", "holds a `&` reference, not `&mut`");
    refused("fn run(a: &Point, b: &Point) -> U64:\n    var seen = Seen(a, 1)\n    seen.point = b\n    return sum(seen)\n", "bound once");
    // Opened into names, each of which keeps what the whole value borrowed to its own last
    // use (docs/decisions.md, 2026-11-22); never with `var`, which could be bound again.
    checked("fn run(point: &Point) -> U64:\n    let seen = Seen(point, 1)\n    let Seen(inner, n) = seen\n    return n + inner.x\n").unwrap();
    checked("fn run(point: &Point) -> U64:\n    let pair = .{count: 2, seen: Seen(point, 1)}\n    let .{count, seen} = pair\n    return count + sum(seen)\n").unwrap();
    checked("fn run() -> U64:\n    var point = Point(3, 4)\n    let Seen(inner, n) = Seen(&point, 1)\n    let x = inner.x\n    bump(&mut point)\n    return n + x\n").unwrap();
    refused("fn run() -> U64:\n    var point = Point(3, 4)\n    let Seen(inner, n) = Seen(&point, 1)\n    bump(&mut point)\n    return n + inner.x\n", "");
    refused("fn run() -> U64:\n    var point = Point(3, 4)\n    let pair = .{count: 2, seen: Seen(&point, 1)}\n    let .{count, seen} = pair\n    bump(&mut point)\n    return count + sum(seen)\n", "");
    refused("fn run(point: &Point) -> U64:\n    let pair = (2, Seen(point, 1))\n    var (count, seen) = pair\n    return count\n", "opened with `let`, not `var`");
    assert!(checked("fn run(items: Buffer<Seen>) -> Buffer<Seen>:\n    return move items\n").is_err());
    assert!(checked("fn run(point: &Point) -> U64:\n    let pair: [Seen; 2] = [Seen(point, 1), Seen(point, 2)]\n    return 0\n").is_err());
    assert!(checked("type Loop:\n    next: &Loop\n    n: U64\n\nfn run() -> U64:\n    return 0\n").is_err());
    refused("type Outer:\n    inner: &Seen\n\nfn run() -> U64:\n    return 0\n", "not at a value that holds a reference");
    refused("fn run(point: &Point) -> U64:\n    var a = Seen(point, 1)\n    var b = Seen(point, 2)\n    memory.exchange(&mut a, &mut b)\n    return 0\n", "");
    // A view may be a copied type argument (docs/design-format.md): what it borrows stays lent
    // through the call, and generic code that would borrow it, or store it, is refused where it
    // is made concrete.
    checked("fn same<T>(value: T) -> T:\n    return value\n\nfn run(point: &Point) -> U64:\n    return sum(same(Seen(point, 1)))\n").unwrap();
    refused("fn same<T>(value: T) -> T:\n    return value\n\nfn run() -> U64:\n    var point = Point(1, 2)\n    let seen = same(Seen(&point, 1))\n    bump(&mut point)\n    return seen.n\n", LENT);
    // (Generic code that borrows a view is refused when its code is made:
    // tests/fixtures/references/view-borrowed-generic.refused.wy.)
    // A record holding a reference beside an owner is no copied view: generic code would
    // duplicate the owner.
    assert!(checked("type Mixed:\n    point: &Point\n    items: Buffer<U64>\n\nfn twice<T>(x: T) -> T:\n    return x\n\nfn run(point: &Point, items: Buffer<U64>) -> U64:\n    let m = twice(Mixed(point, move items))\n    return 0\n").is_err());
    assert!(checked("fn keep<T>(x: T) -> Buffer<T>:\n    unreachable\n\nfn run(point: &Point) -> U64:\n    let b = keep(Seen(point, 1))\n    return 0\n").is_err());
}
#[test]
fn a_reference_in_a_view_is_used_by_name_and_no_proof_reads_a_view() {
    const NAME: &str = "used by name";
    refused("fn run(seen: Seen) -> U64:\n    return seen.point.x\n", NAME);
    refused("type Pick:\n    item: &U64\n\nfn run(pick: Pick) -> U64:\n    return *pick.item\n", NAME);
    const PROOF: &str = "cannot use a record that holds a reference yet";
    // Given one whole, a proof over numbers hands it on to calls and knows nothing of it
    // (`check.rs` `opaque_view`); its clauses do not name it.
    const C: &str = ":\n    ensures result == n\n    proof:\n        auto\n";
    checked(&format!("fn run(seen: Seen, n: U64) -> U64{C}    return n\n")).unwrap();
    checked(&format!("fn run(seen: Seen, n: U64) -> U64{C}    let total = sum(seen)\n    return n\n")).unwrap();
    refused("fn run(seen: Seen, n: U64) -> U64:\n    ensures result == seen.n\n    proof:\n        auto\n    return seen.n\n", PROOF);
    refused("fn run(seen: Seen, n: U64) -> U64:\n    requires seen.n == n\n    ensures result == n\n    proof:\n        auto\n    return n\n", PROOF);
    refused(&format!("fn run(seen: Seen, n: U64) -> U64{C}    var i: U64 = 0\n    while i < n:\n        invariant seen.n == seen.n\n        i += 1\n    return n\n"), PROOF);
    refused(&format!("fn run(seen: Seen, n: U64) -> U64{C}    assert seen.n == seen.n\n    return n\n"), PROOF);
    // Its numbers are read; a reference in it is not, bound or borrowed.
    checked(&format!("fn run(seen: Seen, n: U64) -> U64{C}    return n + seen.n - seen.n\n")).unwrap();
    refused(&format!("fn run(seen: Seen, n: U64) -> U64{C}    let point = seen.point\n    return n + point.x - point.x\n"), PROOF);
    refused(&format!("fn at(point: &&Point) -> U64:\n    return 0\n\nfn run(seen: Seen, n: U64) -> U64{C}    return n + at(&seen.point)\n"), "");
    // Review: a copy, or the record taken apart, gave the reference under another name.
    refused(&format!("fn run(seen: Seen, n: U64) -> U64{C}    let w = seen\n    let p = w.point\n    return n + p.x - p.x\n"), PROOF);
    refused(&format!("fn run(seen: Seen, n: U64) -> U64{C}    let w = seen\n    return n\n"), PROOF);
    refused(&format!("fn run(seen: Seen, n: U64) -> U64{C}    let Seen(inner, m) = seen\n    return n + inner.x - inner.x\n"), PROOF);
    refused(&format!("fn run(seen: Seen, n: U64) -> U64{C}    match seen:\n        other:\n            return n\n"), "");
    // Review: a `for` loop's invariant is a clause too.
    refused(&format!("fn run(seen: Seen, n: U64) -> U64{C}    var t: U64 = 0\n    for i in 0..n:\n        invariant seen.n == seen.n\n        t = i\n    return n\n"), PROOF);
    // Not a copy of it, a view made from it, one in a record or a sum, or one returned.
    refused(&format!("fn run(seen: Seen, n: U64) -> U64{C}    let again = seen\n    return n + sum(again) - sum(again)\n"), PROOF);
    refused(&format!("fn run(seen: Seen, n: U64) -> U64{C}    let more = Seen(seen.point, 1)\n    return n\n"), PROOF);
    refused(&format!("type Holder:\n    seen: Seen\n    count: U64\n\nfn run(seen: Seen, n: U64) -> U64{C}    let holder = Holder(seen, 1)\n    return n\n"), PROOF);
    refused(&format!("type Holder:\n    seen: Seen\n    count: U64\n\nfn run(holder: Holder, n: U64) -> U64{C}    let total = sum(holder.seen)\n    return n\n"), PROOF);
    refused(&format!("fn run(found: Result<Seen, U64>, n: U64) -> U64{C}    return n\n"), PROOF);
    refused(&format!("type Either:\n    Left(Seen)\n    Right(U64)\n\nfn run(either: Either, n: U64) -> U64{C}    return n\n"), PROOF);
    refused(&format!("type Wrap:\n    inner: Result<Seen, U64>\n\nfn run(wrap: Wrap, n: U64) -> U64{C}    return n\n"), PROOF);
    refused("fn run(seen: Seen, n: U64) -> Seen:\n    ensures n == n\n    proof:\n        auto\n    return seen\n", PROOF);
    refused("fn run(seen: Seen, n: U64) -> Result<Seen, U64>:\n    ensures n == n\n    proof:\n        auto\n    return Ok(seen)\n", PROOF);
    // Not one that can change what it refers to, and not one whose type states an invariant.
    refused(&format!("type Hand:\n    point: &mut Point\n\nfn run(hand: Hand, n: U64) -> U64{C}    return n\n"), "");
    refused(&format!("type Both:\n    seen: Seen\n    hand: &mut Point\n\nfn run(both: Both, n: U64) -> U64{C}    return n\n"), "");
    refused("fn run(point: &Point, n: U64) -> U64:\n    ensures result == n\n    proof:\n        auto\n    let seen = Seen(point, n)\n    return n\n", PROOF);
    refused("fn run(point: &Point, n: U64) -> U64:\n    ensures result == n\n    proof:\n        auto\n    match find(point, n):\n        Ok(seen):\n            return n\n        Err(code):\n            return n\n", PROOF);
    refused("sealed type Span:\n    point: &Point\n    from: U64\n    invariant from <= 4\n\nfn run() -> U64:\n    return 0\n", "states no invariant");
}
#[test]
fn a_view_variable_is_assigned_only_what_borrows_from_what_it_already_borrows() {
    const SAME: &str = "borrows from what it already borrows";
    checked("fn more(seen: Seen) -> Seen:\n    return Seen(seen.point, seen.n + 1)\n\nfn run(point: &Point) -> U64:\n    var seen = Seen(point, 0)\n    while seen.n < 5:\n        seen = more(seen)\n    seen = Seen(point, 9)\n    seen = Seen(seen.point, 8)\n    return sum(seen)\n").unwrap();
    checked("fn run() -> U64:\n    var point = Point(3, 4)\n    var seen = Seen(&point, 0)\n    seen = Seen(&point, 2)\n    let total = sum(seen)\n    bump(&mut point)\n    return total\n").unwrap();
    // A by-value parameter, and a view held in a sum.
    checked("fn run(given: Seen) -> U64:\n    var seen = given\n    seen = Seen(seen.point, 2)\n    return sum(seen)\n").unwrap();
    checked("fn run(point: &Point) -> U64:\n    var seen = Seen(point, 0)\n    match find(seen.point, 3):\n        Ok(found):\n            seen = found\n        Err(code):\n            return code\n    return sum(seen)\n").unwrap();
    // Another source would have to be lent from before the assignment, on every pass of a loop.
    refused("fn run(a: &Point, b: &Point) -> U64:\n    var seen = Seen(a, 1)\n    seen = Seen(b, 2)\n    return sum(seen)\n", SAME);
    refused("fn run() -> U64:\n    let a = Point(1, 2)\n    var b = Point(3, 4)\n    var seen = Seen(&a, 1)\n    var total: U64 = 0\n    for round in 0..3:\n        total += sum(seen)\n        bump(&mut b)\n        seen = Seen(&b, 2)\n    return total\n", SAME);
    refused("fn run(a: &Point) -> U64:\n    var seen = Seen(a, 1)\n    let local = Point(3, 4)\n    seen = Seen(&local, 2)\n    return sum(seen)\n", SAME);
    // The loans it holds do not end at the assignment.
    refused("fn run() -> U64:\n    var point = Point(3, 4)\n    var seen = Seen(&point, 0)\n    seen = Seen(seen.point, 2)\n    bump(&mut point)\n    return sum(seen)\n", LENT);
    // Only a whole variable: a reference field, a view inside a record and a sum stay bound once.
    refused("fn run(a: &Point, b: &Point) -> U64:\n    var seen = Seen(a, 1)\n    seen.point = a\n    return sum(seen)\n", "bound once");
    refused("type Holder:\n    seen: Seen\n    count: U64\n\nfn run(a: &Point) -> U64:\n    var holder = Holder(Seen(a, 1), 2)\n    holder.seen = Seen(a, 3)\n    return holder.count\n", "bound once");
    refused("fn run(a: &Point) -> U64:\n    var found = find(a, 1)\n    found = find(a, 2)\n    return 0\n", "bound once");
}
#[test]
fn views_through_try_and_over_values_with_no_name() {
    checked("fn run(point: &Point) -> Result<U64, U64>:\n    let seen = find(point, 3)?\n    return Ok(sum(seen))\n").unwrap();
    refused("fn run() -> Result<U64, U64>:\n    var point = Point(3, 4)\n    let seen = find(&point, 3)?\n    bump(&mut point)\n    return Ok(sum(seen))\n", LENT);
    refused("fn run() -> Result<Seen, U64>:\n    let point = Point(3, 4)\n    let seen = find(&point, 3)?\n    return Ok(seen)\n", "must borrow from a reference parameter");
    // A view is made of named values: a value with no name has no place to point at.
    assert!(checked("fn run() -> U64:\n    let seen = Seen(&Point(1, 2), 3)\n    return sum(seen)\n").is_err());
    assert!(checked("fn run() -> Seen:\n    return Seen(&Point(1, 2), 3)\n").is_err());
    // Where a call makes the place, the view still cannot leave or be kept past it.
    refused("fn run() -> Seen:\n    return make(&Point(1, 2))\n", "must borrow from a reference parameter");
    refused("fn run(point: &Point) -> U64:\n    var seen = Seen(point, 3)\n    for round in 0..3:\n        seen = make(&Point(round, 2))\n    return sum(seen)\n", "borrows from what it already borrows");
}
