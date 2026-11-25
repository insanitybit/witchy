//! Unique references as values: `&mut T` returned, kept in a `let` or a variant, and passed on.
//! What must hold: while one is in use, nothing else reads, writes or lends what it borrows,
//! and it is never duplicated.
use crate::diagnostic::Diagnostic;
const TYPES:&str="type Pair:\n    a: U64\n    b: U64\n\nfn first(pair: &mut Pair) -> &mut U64:\n    return &mut (*pair).a\n\nfn bump(value: &mut U64):\n    *value += 1\n\nfn two(left: &mut U64, right: &mut U64):\n    *left += *right\n\nfn show(value: &U64) -> U64:\n    return *value\n\n";
fn checked(rest:&str)->Result<(),Diagnostic>{
    let source=format!("{TYPES}{rest}");
    let logical=super::check::program(&super::parse(&source)?)?;super::concrete::program(logical)?;Ok(())
}
fn refused(rest:&str)->String{checked(rest).expect_err(rest).message.to_string()}
#[test]
fn a_unique_reference_is_returned_kept_written_through_and_passed_on(){
    checked("fn main(console: Console<Write>):\n    var pair = Pair(1, 2)\n    let r = first(&mut pair)\n    *r = 5\n    bump(r)\n    bump(r)\n    console.print_int(*r)\n    console.print_int(pair.a + pair.b)\n").unwrap();
    checked("fn again(pair: &mut Pair) -> &mut U64:\n    let inner = first(pair)\n    bump(inner)\n    return inner\n\nfn main(console: Console<Write>):\n    var pair = Pair(1, 2)\n    let r = again(&mut pair)\n    *r += 10\n    console.print_int(pair.a)\n").unwrap();
    checked("fn main(console: Console<Write>):\n    var x: U64 = 1\n    let r = &mut x\n    *r = 4\n    let s = r\n    *s += 1\n    *r += 1\n    console.print_int(x)\n").unwrap();
    checked("fn main(console: Console<Write>):\n    var pair = Pair(1, 2)\n    let r = first(&mut pair)\n    console.print_int(show(r))\n    *r = 9\n    console.print_int(pair.a)\n").unwrap();
    checked("fn main(console: Console<Write>):\n    var pair = Pair(1, 2)\n    for index in 0..3:\n        let s = first(&mut pair)\n        *s += index\n    console.print_int(pair.a)\n").unwrap();
    checked("fn find(pair: &mut Pair, which: U64) -> Result<&mut U64, U64>:\n    if which == 0:\n        return Ok(&mut (*pair).a)\n    if which == 1:\n        return Ok(&mut (*pair).b)\n    return Err(which)\n\nfn main(console: Console<Write>):\n    var pair = Pair(1, 2)\n    match find(&mut pair, 1):\n        Ok(r):\n            *r = 20\n        Err(code):\n            console.print_int(code)\n    match find(&mut pair, 2):\n        Ok(r):\n            *r = 30\n        Err(code):\n            console.print_int(code)\n    console.print_int(pair.a + pair.b)\n").unwrap();
    checked("fn whole(pair: &mut Pair) -> &mut Pair:\n    let same = pair\n    (*same).b += 1\n    return same\n\nfn main(console: Console<Write>):\n    var pair = Pair(1, 2)\n    let r = whole(&mut pair)\n    (*r).a = 10\n    console.print_int(pair.a + pair.b)\n").unwrap();
    checked("sealed type Counter:\n    n: U64\n\nfn same(counter: &mut Counter) -> &mut Counter:\n    return counter\n\nfn main(console: Console<Write>):\n    var counter = Counter(1)\n    let r = same(&mut counter)\n    console.print_int(1)\n").unwrap();
    checked("type Option<T>:\n    Some(T)\n    None\n\nfn maybe(pair: &mut Pair, yes: Bool) -> Option<&mut U64>:\n    if yes:\n        return Some(&mut (*pair).b)\n    return None\n\nfn main(console: Console<Write>):\n    var pair = Pair(1, 2)\n    match maybe(&mut pair, true):\n        Some(r):\n            bump(r)\n            *r *= 5\n        None:\n            console.print_int(0)\n    console.print_int(pair.b)\n").unwrap();
}
#[test]
fn a_value_is_not_touched_while_a_unique_reference_into_it_is_in_use(){
    assert!(refused("fn main(console: Console<Write>):\n    var pair = Pair(1, 2)\n    let r = first(&mut pair)\n    let s = first(&mut pair)\n    *r = 5\n    *s = 6\n").contains("this value is lent"));
    assert!(refused("fn main(console: Console<Write>):\n    var pair = Pair(1, 2)\n    let r = first(&mut pair)\n    console.print_int(pair.a)\n    *r = 5\n").contains("this value is lent"));
    assert!(refused("fn main(console: Console<Write>):\n    var pair = Pair(1, 2)\n    let r = first(&mut pair)\n    pair.b = 7\n    *r = 5\n").contains("this value is lent"));
    assert!(refused("fn main(console: Console<Write>):\n    var pair = Pair(1, 2)\n    let r = first(&mut pair)\n    for index in 0..3:\n        let s = first(&mut pair)\n        *s += 1\n    *r = 0\n").contains("this value is lent"));
    assert!(refused("fn find(pair: &mut Pair, which: U64) -> Result<&mut U64, U64>:\n    if which == 0:\n        return Ok(&mut (*pair).a)\n    return Err(which)\n\nfn main(console: Console<Write>):\n    var pair = Pair(1, 2)\n    match find(&mut pair, 0):\n        Ok(r):\n            pair.b = 3\n            *r = 20\n        Err(code):\n            console.print_int(code)\n").contains("this value is lent"));
    assert!(refused("fn main(console: Console<Write>):\n    var pair = Pair(1, 2)\n    var other = Pair(3, 4)\n    let r = first(&mut pair)\n    while true:\n        pair.a = 5\n        break\n    *r = 1\n").contains("this value is lent"));
}
#[test]
fn a_unique_reference_is_not_duplicated(){
    assert!(refused("fn main(console: Console<Write>):\n    var pair = Pair(1, 2)\n    let r = first(&mut pair)\n    let s = r\n    *r = 1\n    *s = 2\n").contains("this value is lent"));
    assert!(refused("fn main(console: Console<Write>):\n    var pair = Pair(1, 2)\n    let r = first(&mut pair)\n    let s = &*r\n    *r = 1\n    console.print_int(*s)\n").contains("this value is lent"));
    assert!(refused("fn whole(pair: &mut Pair) -> &mut Pair:\n    let same = pair\n    (*pair).b += 1\n    return same\n\nfn main(console: Console<Write>):\n    var pair = Pair(1, 2)\n    let r = whole(&mut pair)\n").contains("this value is lent"));
}
#[test]
fn a_unique_reference_is_not_handed_on_twice_in_one_call(){
    assert!(refused("fn main(console: Console<Write>):\n    var pair = Pair(1, 2)\n    let r = first(&mut pair)\n    two(r, r)\n").contains("overlapping unique"));
    assert!(refused("fn main(console: Console<Write>):\n    var pair = Pair(1, 2)\n    let r = first(&mut pair)\n    memory.exchange(r, r)\n").contains("overlapping unique"));
}
#[test]
fn a_matched_unique_reference_is_consumed_once(){
    assert!(refused("fn find(pair: &mut Pair, which: U64) -> Result<&mut U64, U64>:\n    if which == 0:\n        return Ok(&mut (*pair).a)\n    return Err(which)\n\nfn main(console: Console<Write>):\n    var pair = Pair(1, 2)\n    let found = find(&mut pair, 0)\n    match move found:\n        Ok(r):\n            match move found:\n                Ok(s):\n                    *s = 1\n                Err(code):\n                    console.print_int(code)\n            *r = 2\n        Err(code):\n            console.print_int(code)\n").contains("use of a moved value"));
}
#[test]
fn a_unique_reference_does_not_outlive_what_it_borrows(){
    assert!(refused("fn make() -> &mut U64:\n    var x: U64 = 1\n    return &mut x\n\nfn main(console: Console<Write>):\n    let r = make()\n    console.print_int(*r)\n").contains("must borrow from a reference parameter"));
    assert!(refused("fn make(pair: Pair) -> &mut U64:\n    var mine = pair\n    return &mut mine.a\n\nfn main(console: Console<Write>):\n    let r = make(Pair(1, 2))\n    console.print_int(*r)\n").contains("must borrow from a reference parameter"));
}
#[test]
fn a_unique_reference_is_not_made_from_a_shared_one(){
    assert!(refused("fn sneak(pair: &Pair) -> &mut U64:\n    return &mut (*pair).a\n\nfn main(console: Console<Write>):\n    let pair = Pair(1, 2)\n    let r = sneak(&pair)\n    *r = 3\n").contains("insufficient root permission"));
}
#[test]
fn a_kept_unique_reference_reaches_inside_a_sealed_type_in_its_own_file_unless_it_has_an_invariant(){
    checked("sealed type Counter:\n    n: U64\n\nfn slot(counter: &mut Counter) -> &mut U64:\n    return &mut (*counter).n\n\nfn main(console: Console<Write>):\n    var pair = Pair(1, 2)\n").unwrap();
    checked("sealed type Counter:\n    pair: Pair\n\nfn slot(counter: &mut Counter) -> &mut U64:\n    return first(&mut (*counter).pair)\n\nfn main(console: Console<Write>):\n    var pair = Pair(1, 2)\n").unwrap();
    assert_eq!(refused("sealed type Box:\n    data: Buffer<U64>\n    length: U64\n    capacity: U64\n    invariant memory.prefix(data, length, capacity)\n\nfn slot(box: &mut Box) -> &mut U64:\n    return &mut (*box).length\n\nfn main(console: Console<Write>):\n    var pair = Pair(1, 2)\n"),
        // The protected field makes the function's proof required (f9c4e93d), and no proof
        // model covers a returned `&mut` into a record: still refused, in these words.
        "a function with a contract returns a `&mut` reference only when its proof is about a Buffer");
}
#[test]
fn a_scalar_proved_function_holds_a_unique_reference(){
    // The scalar walk follows it like a `&mut` parameter; what it lends is unknown afterwards.
    checked("fn fixed(pair: &mut Pair) -> U64:\n    ensures result == 5\n    proof:\n        auto\n    let r = first(pair)\n    *r = 5\n    return 5\n\nfn main(console: Console<Write>):\n    var pair = Pair(1, 2)\n    console.print_int(fixed(&mut pair))\n").unwrap();
}
#[test]
fn a_unique_reference_is_not_stored_or_used_as_a_type_argument(){
    refused("type Holder:\n    slot: &mut U64\n\nfn main(console: Console<Write>):\n    var x: U64 = 1\n    let h = Holder(&mut x)\n");
    refused("type Both<T>:\n    left: T\n    right: T\n\nfn dup<T>(value: T) -> Both<T>:\n    return Both(value, value)\n\nfn main(console: Console<Write>):\n    var pair = Pair(1, 2)\n    let r = first(&mut pair)\n    let both = dup(r)\n");
    refused("fn pick<T>(value: T, other: T) -> T:\n    return value\n\nfn main(console: Console<Write>):\n    var pair = Pair(1, 2)\n    let r = first(&mut pair)\n    let s = pick(r, r)\n");
    refused("fn id<T>(value: T) -> T:\n    return value\n\nfn main(console: Console<Write>):\n    var pair = Pair(1, 2)\n    let r = first(&mut pair)\n    let s = id(r)\n    *s = 4\n    console.print_int(pair.a)\n");
    refused("type Option<T>:\n    Some(T)\n    None\n\nfn same<T>(value: T) -> Option<T>:\n    let again = value\n    let more = value\n    return Some(more)\n\nfn main(console: Console<Write>):\n    var pair = Pair(1, 2)\n    let r = first(&mut pair)\n    match same(r):\n        Some(s):\n            *s = 3\n        None:\n            console.print_int(0)\n");
}
