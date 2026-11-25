//! Shared references: what keeps a value lent, when the loan ends, and what each refusal says.
use crate::diagnostic::Diagnostic;
const TYPES:&str="type Shelf:\n    items: Buffer<U64>\n    count: U64\n\nfn first(shelf: &Shelf) -> &U64:\n    return memory.ref_unchecked(&(*shelf).items, 0)\n\nfn at(shelf: &Shelf, index: U64) -> Result<&U64, U64>:\n    if index < (*shelf).count:\n        return Ok(memory.ref_unchecked(&(*shelf).items, index))\n    return Err(1)\n\nfn pass(item: &U64) -> &U64:\n    return item\n\nfn bump(shelf: &mut Shelf):\n    (*shelf).count += 1\n    return\n\n";
fn checked(rest:&str)->Result<(),Diagnostic>{
    let source=format!("{TYPES}{rest}\nfn main():\n    return\n");
    let logical=super::check::program(&super::parse(&source)?)?;super::concrete::program(logical)?;Ok(())
}
fn refused(rest:&str)->String{checked(rest).expect_err(rest).message.to_string()}
const LENT:&str="this value is lent";
#[test]
fn a_reference_is_read_passed_on_and_returned(){
    checked("fn run(shelf: &Shelf) -> U64:\n    let item = first(shelf)\n    return *item\n").unwrap();
    checked("fn run(shelf: &Shelf) -> &U64:\n    let item = first(shelf)\n    return pass(item)\n").unwrap();
    checked("fn run(shelf: &Shelf) -> Result<U64, U64>:\n    let item = at(shelf, 0)?\n    return Ok(*item)\n").unwrap();
    checked("fn run(shelf: &Shelf) -> U64:\n    match at(shelf, 2):\n        Ok(item):\n            return *item\n        Err(code):\n            return code\n").unwrap();
}
#[test]
fn the_loan_ends_at_the_last_use_of_the_reference(){
    checked("fn run(shelf: &mut Shelf) -> U64:\n    let item = first(shelf)\n    let seen = *item\n    bump(shelf)\n    return seen\n").unwrap();
    checked("fn run(shelf: Shelf) -> Shelf:\n    let item = first(&shelf)\n    let seen = *item\n    return move shelf\n").unwrap();
    checked("fn run(shelf: &mut Shelf) -> U64:\n    match at(shelf, 0):\n        Ok(item):\n            let seen = *item\n            bump(shelf)\n            return seen\n        Err(code):\n            bump(shelf)\n            return code\n").unwrap();
    checked("fn run(shelf: &mut Shelf) -> U64:\n    var total: U64 = 0\n    while total < 3:\n        let item = first(shelf)\n        total += *item\n        bump(shelf)\n    return total\n").unwrap();
}
#[test]
fn a_lent_value_is_not_written_moved_or_lent_exclusively(){
    assert!(refused("fn run(shelf: &mut Shelf) -> U64:\n    let item = first(shelf)\n    bump(shelf)\n    return *item\n").contains(LENT));
    assert!(refused("fn run(shelf: Shelf) -> Shelf:\n    let item = first(&shelf)\n    let moved = move shelf\n    let seen = *item\n    return move moved\n").contains(LENT));
    assert!(refused("fn run(shelf: &mut Shelf) -> U64:\n    let item = first(shelf)\n    (*shelf).count = 0\n    return *item\n").contains(LENT));
    assert!(refused("fn run(shelf: &mut Shelf) -> U64:\n    match at(shelf, 0):\n        Ok(item):\n            bump(shelf)\n            return *item\n        Err(code):\n            return code\n").contains(LENT));
    assert!(refused("fn run(shelf: &mut Shelf) -> U64:\n    var total: U64 = 0\n    while total < 3:\n        let item = first(shelf)\n        bump(shelf)\n        total += *item\n    return total\n").contains(LENT));
    // A Result that holds a reference lends like the reference.
    assert!(refused("fn run(shelf: &mut Shelf) -> U64:\n    let found = at(shelf, 0)\n    bump(shelf)\n    match found:\n        Ok(item):\n            return *item\n        Err(code):\n            return code\n").contains(LENT));
}
#[test]
fn a_copy_of_a_reference_keeps_the_loan(){
    assert!(refused("fn run(shelf: &mut Shelf) -> U64:\n    let item = first(shelf)\n    let same = item\n    bump(shelf)\n    return *same\n").contains(LENT));
    assert!(refused("fn run(shelf: &mut Shelf) -> U64:\n    let item = first(shelf)\n    let same = pass(item)\n    bump(shelf)\n    return *same\n").contains(LENT));
}
#[test]
fn a_reference_does_not_outlive_what_it_borrows(){
    assert!(refused("fn run(shelf: Shelf) -> &U64:\n    return first(&shelf)\n").contains("must borrow from a reference parameter"));
    assert!(refused("fn run(shelf: Shelf) -> &U64:\n    let item = first(&shelf)\n    return item\n").contains("must borrow from a reference parameter"));
    assert!(refused("fn run(shelf: &Shelf, other: &Shelf) -> U64:\n    var item = at(shelf, 0)\n    item = at(other, 0)\n    return 0\n").contains("bound once"));
    assert!(refused("fn run(shelf: &mut Shelf) -> &U64:\n    return memory.ref_unchecked(&(*shelf).items, 0)\n").contains("borrows only with `&`"));
    // A record may hold a `&` reference (`view_tests`); it is still never stored or pointed at.
    // A shared reference to a view is a type (docs/design-references-to-views.md); `&mut` is not yet.
    checked("type Keep:\n    item: &U64\n\nfn run(keep: &Keep) -> U64:\n    return 0\n").unwrap();
    // `&mut` to one may change what holds no reference, never the reference itself (step 2).
    checked("type Keep:\n    item: &U64\n\nfn run(keep: &mut Keep) -> U64:\n    return 0\n").unwrap();
    assert!(checked("type Keep:\n    item: &U64\n\nfn run(keep: &mut Keep, other: &U64):\n    (*keep).item = other\n").is_err());
    assert!(checked("type Keep:\n    item: &mut U64\n\nfn run(keep: Keep) -> U64:\n    return 0\n").is_err());
    assert!(checked("fn run(items: Buffer<&U64>) -> Buffer<&U64>:\n    return move items\n").is_err());
}
#[test]
fn a_checked_cell_reference_lends_its_buffer(){
    checked("fn run(shelf: &mut Shelf) -> U64:\n    match memory.ref(&(*shelf).items, 0):\n        Ok(item):\n            let seen = *item\n            bump(shelf)\n            return seen\n        Err(code):\n            return code\n").unwrap();
    assert!(refused("fn run(shelf: &mut Shelf) -> U64:\n    match memory.ref(&(*shelf).items, 0):\n        Ok(item):\n            bump(shelf)\n            return *item\n        Err(code):\n            return code\n").contains(LENT));
    // Another field may be written meanwhile (fields are lent separately); the whole may not.
    checked("fn run(shelf: &mut Shelf) -> U64:\n    let found = memory.ref(&(*shelf).items, 0)\n    (*shelf).count = 0\n    match found:\n        Ok(item):\n            return *item\n        Err(code):\n            return code\n").unwrap();
    assert!(refused("fn run(shelf: &mut Shelf) -> U64:\n    let found = memory.ref(&(*shelf).items, 0)\n    bump(shelf)\n    match found:\n        Ok(item):\n            return *item\n        Err(code):\n            return code\n").contains(LENT));
    checked("fn run(shelf: &Shelf, index: U64) -> Result<&U64, U64>:\n    return memory.ref(&(*shelf).items, index)\n").unwrap();
    assert!(refused("fn run(shelf: Shelf) -> Result<&U64, U64>:\n    return memory.ref(&shelf.items, 0)\n").contains("must borrow from a reference parameter"));
}
#[test]
fn a_refused_cell_fill_hands_the_owner_back(){
    let owner="type Box:\n    cells: Buffer<U64>\n\nfn run(slots: &mut Buffer<Box>, item: Box) -> Result<(), Box>:\n    return memory.try_init(slots, 0, move item)\n";
    checked(owner).unwrap();
    // The value that comes back is an owner like any other: dropping it is refused.
    assert!(checked("type Box:\n    cells: Buffer<U64>\n\nfn run(slots: &mut Buffer<Box>, item: Box):\n    match memory.try_init(slots, 0, move item):\n        Ok:\n            return\n        Err(back):\n            return\n").is_err());
}
#[test]
fn a_reference_to_a_place_is_a_value_that_lends_it(){
    // Through a reference: the new reference borrows what the old one does.
    checked("type Pair:\n    left: U64\n    right: U64\n\nfn right(pair: &Pair) -> &U64:\n    return &(*pair).right\n").unwrap();
    checked("fn run(shelf: &Shelf) -> &U64:\n    return &(*shelf).count\n").unwrap();
    assert!(refused("fn run(shelf: &mut Shelf) -> U64:\n    let count = &(*shelf).count\n    bump(shelf)\n    return *count\n").contains(LENT));
    checked("fn run(shelf: &mut Shelf) -> U64:\n    let count = &(*shelf).count\n    let seen = *count\n    bump(shelf)\n    return seen\n").unwrap();
    // Of an owned local: the local is lent until the reference's last use, and cannot be returned.
    assert!(refused("fn run(shelf: Shelf) -> Shelf:\n    let count = &shelf.count\n    let moved = move shelf\n    let seen = *count\n    return move moved\n").contains(LENT));
    assert!(refused("fn run(shelf: Shelf) -> &U64:\n    return &shelf.count\n").contains("must borrow from a reference parameter"));
    // Reached through a reference held in a local: the loan of what that reference borrows is kept.
    assert!(refused("fn run(shelf: &mut Shelf) -> U64:\n    match at(shelf, 0):\n        Ok(item):\n            let same = &*item\n            bump(shelf)\n            return *same\n        Err(code):\n            return code\n").contains(LENT));
}
const SHAPES:&str="type Shape:\n    Circle(U64)\n    Named(Shelf)\n    Empty\n\ntype Tagged:\n    shape: Shape\n    weight: U64\n\nfn reset(tagged: &mut Tagged):\n    (*tagged).weight = 0\n\n";
#[test]
fn a_match_through_a_reference_binds_references_and_leaves_the_value(){
    let read="fn read(shape: &Shape) -> U64:\n    match shape:\n        Circle(radius):\n            return *radius\n        Named(shelf):\n            return (*shelf).count\n        Empty:\n            return 0\n";
    checked(&format!("{SHAPES}{read}")).unwrap();
    // The value is still whole afterwards, and a binder can be handed back.
    checked(&format!("{SHAPES}{read}fn twice(shape: Shape) -> Shape:\n    let seen = read(&shape) + read(&shape)\n    return move shape\n")).unwrap();
    checked(&format!("{SHAPES}fn shelf(shape: &Shape) -> Result<&Shelf, U64>:\n    match shape:\n        Circle(radius):\n            return Err(1)\n        Named(shelf):\n            return Ok(shelf)\n        Empty:\n            return Err(2)\n")).unwrap();
    checked(&format!("{SHAPES}fn weigh(tagged: &Tagged) -> U64:\n    match &(*tagged).shape:\n        Circle(radius):\n            return *radius + (*tagged).weight\n        Named(shelf):\n            return 1\n        Empty:\n            return 0\n")).unwrap();
}
#[test]
fn a_binder_of_a_match_through_a_reference_keeps_the_value_lent(){
    assert!(refused(&format!("{SHAPES}fn run(tagged: &mut Tagged) -> U64:\n    match &(*tagged).shape:\n        Circle(radius):\n            reset(tagged)\n            return *radius\n        Named(shelf):\n            return 1\n        Empty:\n            return 0\n")).contains(LENT));
    checked(&format!("{SHAPES}fn run(tagged: &mut Tagged) -> U64:\n    match &(*tagged).shape:\n        Circle(radius):\n            let seen = *radius\n            reset(tagged)\n            return seen\n        Named(shelf):\n            return 1\n        Empty:\n            return 0\n")).unwrap();
    // An owner inside the variant is only borrowed: it cannot be moved out.
    assert!(refused(&format!("{SHAPES}fn steal(shape: &Shape) -> U64:\n    match shape:\n        Circle(radius):\n            return 0\n        Named(shelf):\n            let taken = *shelf\n            return 1\n        Empty:\n            return 0\n")).contains("cannot be copied"));
    // A function with a proof does not read a sum through a reference yet.
    assert!(refused(&format!("{SHAPES}fn proved(shape: &Shape, data: &Buffer<U64>) -> U64:\n    ensures memory.unchanged(data, old(data))\n    proof:\n        auto\n    match shape:\n        Circle(radius):\n            return 1\n        Named(shelf):\n            return 2\n        Empty:\n            return 3\n")).contains("function contracts require"));
}
