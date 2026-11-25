//! `for`: what it walks and what it refuses.
use crate::diagnostic::Diagnostic;
const TYPES:&str="type Shelf:\n    items: Buffer<U64>\n    count: U64\n\nimpl Shelf:\n    fn at(self: &Shelf, index: U64) -> Result<&U64, U64>:\n        if index < (*self).count:\n            return Ok(memory.ref_unchecked(&(*self).items, index))\n        return Err(1)\n\n    fn bump(self: &mut Shelf):\n        (*self).count += 1\n        return\n\ntype Plain:\n    value: U64\n\n";
fn checked(rest:&str)->Result<(),Diagnostic>{
    let source=format!("{TYPES}{rest}\nfn main():\n    return\n");
    let logical=super::check::program(&super::parse(&source)?)?;super::concrete::program(logical)?;Ok(())
}
fn refused(rest:&str)->String{checked(rest).expect_err(rest).message.to_string()}
#[test]
fn a_range_counts_and_a_container_is_walked_by_reference(){
    checked("fn run(low: U64, high: U64) -> U64:\n    var total: U64 = 0\n    for value in low..high:\n        total += value\n    return total\n").unwrap();
    checked("fn run(shelf: &Shelf) -> U64:\n    var total: U64 = 0\n    for item in shelf:\n        total += *item\n    return total\n").unwrap();
    checked("fn run(shelf: Shelf) -> Shelf:\n    var total: U64 = 0\n    for item in &shelf:\n        for other in &shelf:\n            total += *item + *other\n    return move shelf\n").unwrap();
    // Two loops in one block keep their counters apart.
    checked("fn run() -> U64:\n    var total: U64 = 0\n    for a in 0..2:\n        total += a\n    for a in 0..3:\n        total += a\n    return total\n").unwrap();
}
#[test]
fn the_container_stays_lent_while_an_element_is_in_use(){
    assert!(refused("fn run(shelf: &mut Shelf) -> U64:\n    var total: U64 = 0\n    for item in shelf:\n        shelf.bump()\n        total += *item\n    return total\n").contains("this value is lent"));
    // After the element's last use the container is free again, and the next step asks it afresh.
    checked("fn run(shelf: &mut Shelf) -> U64:\n    var total: U64 = 0\n    for item in shelf:\n        total += *item\n        if total > 100:\n            shelf.bump()\n    return total\n").unwrap();
}
#[test]
fn for_needs_a_range_of_u64_or_a_type_with_at(){
    assert!(refused("fn run(plain: Plain) -> U64:\n    for item in &plain:\n        return 1\n    return 0\n").contains("no method of this name"));
    assert!(checked("fn run(flag: Bool) -> U64:\n    for value in flag..3:\n        return value\n    return 0\n").is_err());
    // The loop variable is a plain `let`: it cannot be assigned.
    assert!(checked("fn run() -> U64:\n    for value in 0..3:\n        value = 7\n    return 0\n").is_err());
}
#[test]
fn break_and_continue_belong_to_a_loop_and_keep_its_ownership(){
    let check=|source:&str|->Result<(),crate::diagnostic::Diagnostic>{
        let source=format!("{source}\nfn main():\n    return\n");
        let logical=super::check::program(&super::parse(&source)?)?;super::concrete::program(logical)?;Ok(())
    };
    let refused=|source:&str|check(source).expect_err(source).message.to_string();
    check("fn run(limit: U64) -> U64:\n    var count: U64 = 0\n    while true:\n        count += 1\n        if count == limit:\n            break\n        if count % 2 == 0:\n            continue\n    return count\n").unwrap();
    assert!(refused("fn run() -> U64:\n    break\n    return 0\n").contains("inside a loop"));
    assert!(refused("fn run() -> U64:\n    if true:\n        continue\n    return 0\n").contains("inside a loop"));
    // An owner made in the body must be consumed before the jump, as at the body's end.
    let held="fn run(data: Buffer<U8>) -> Buffer<U8>:\n    var count: U64 = 0\n    while count < 3:\n        count += 1\n        let moved = move data\n        JUMP\n    return move data\n";
    assert!(refused(&held.replace("JUMP","break")).contains("discard"));
    let owner="fn make() -> Result<Buffer<U8>, U64>:\n    return Err(1)\n\nfn run() -> U64:\n    var count: U64 = 0\n    while count < 3:\n        count += 1\n        match make():\n            Ok(data):\n                JUMP\n            Err(code):\n                count += code\n    return count\n";
    assert!(refused(&owner.replace("JUMP","break")).contains("discard"));
    assert!(refused(&owner.replace("JUMP","continue")).contains("discard"));
    // An owner from outside the loop must be where the loop expects it.
    let outer="fn run(data: Buffer<U8>) -> U64:\n    var count: U64 = 0\n    while count < 3:\n        count += 1\n        if count == 2:\n            match memory.try_release(move data):\n                Ok:\n                    JUMP\n                Err(stuck):\n                    return 1\n    return count\n";
    assert!(refused(&outer.replace("JUMP","continue")).contains("loop backedge changes outer ownership"));
    assert!(refused(&outer.replace("JUMP","break")).contains("loop backedge changes outer ownership"));
    // A loop that a proof reasons about ends only by its condition.
    // A proof over numbers follows a loop that jumps, with invariants or without; a body a
    // memory model follows cannot jump yet.
    check("fn run(limit: U64) -> U64:\n    ensures result <= limit\n    proof:\n        auto\n    var count: U64 = 0\n    while count < limit:\n        break\n    return count\n").unwrap();
    let prepared=|source:&str|{let source=format!("{source}\nfn main():\n    return\n");super::source_vc::prepare(&super::check::program(&super::parse(&source).unwrap()).unwrap())};
    assert!(prepared("fn run(limit: U64) -> U64:\n    ensures result <= limit\n    proof:\n        auto\n    var count: U64 = 0\n    while count < limit:\n        invariant count <= limit\n        break\n    return count\n").is_ok());
    assert!(refused("fn run(buffer: &mut Buffer<U8>, limit: U64):\n    ensures memory.same_allocation(buffer, old(buffer))\n    proof:\n        auto\n    var count: U64 = 0\n    while count < limit:\n        break\n").contains("cannot use `break` or `continue`"));
}
