//! `match` through a `&mut` reference: each arm's name is a `&mut` reference to the value its
//! variant holds. What must hold: while such a name is in use the matched sum is not read,
//! written, replaced or moved, so the name never refers to another variant's value.
use crate::diagnostic::Diagnostic;
const TYPES: &str = "type Light:\n    Red\n    Green(U64)\n\ntype Shape:\n    Circle(U64)\n    Rect(U64, U64)\n    Empty\n\ntype Size:\n    width: U64\n    height: U64\n\ntype Box:\n    Sized(Size)\n    Open\n\ntype Lamp:\n    light: Light\n    level: U64\n\n";
const MAIN: &str = "\nfn main(console: Console<Write>):\n    var light = Green(3)\n    step(&mut light)\n";
fn checked(rest: &str) -> Result<(), Diagnostic> {
    let source = format!("{TYPES}{rest}");
    let logical = super::check::program(&super::parse(&source)?)?; super::concrete::program(logical)?; Ok(())
}
fn step(arm: &str) -> Result<(), Diagnostic> { checked(&format!("fn step(light: &mut Light):\n    match light:\n        Red:\n            *light = Green(3)\n        Green(left):\n{arm}{MAIN}")) }
fn refused(result: Result<(), Diagnostic>, why: &str) { let message = result.expect_err(why).message.to_string(); assert!(message.contains(why), "{message}"); }
#[test]
fn a_variants_value_is_changed_where_it_is() {
    step("            *left -= 1\n").unwrap();
    step("            if *left > 0:\n                *left -= 1\n").unwrap();
    // After the name's last use the sum itself may be replaced.
    step("            if *left > 0:\n                *left -= 1\n                return\n            *light = Red\n").unwrap();
    step("            *left = 1\n            for turn in 0..2:\n                *light = Red\n").unwrap();
    checked("fn widen(shape: &mut Shape):\n    match shape:\n        Circle(radius):\n            *radius += 1\n        Rect(width, _):\n            *width += 1\n        Empty:\n            return\n\nfn main(console: Console<Write>):\n    var shape = Rect(2, 3)\n    widen(&mut shape)\n").unwrap();
    checked("fn widen(shape: &mut Box):\n    match shape:\n        Sized(size):\n            size.width += 1\n            size.height += size.width\n        Open:\n            return\n\nfn main(console: Console<Write>):\n    var shape = Sized(Size(2, 3))\n    widen(&mut shape)\n").unwrap();
    checked("fn main(console: Console<Write>):\n    var lamp = Lamp(Green(3), 2)\n    match &mut lamp.light:\n        Green(left):\n            *left = 9\n        Red:\n            return\n    console.print_int(lamp.level)\n").unwrap();
    // The reference may be returned when it borrows from a parameter.
    checked("fn pick(light: &mut Light) -> Result<&mut U64, U64>:\n    match light:\n        Green(left):\n            return Ok(left)\n        Red:\n            return Err(1)\n\nfn main(console: Console<Write>):\n    var light = Green(3)\n    match pick(&mut light):\n        Ok(left):\n            *left += 4\n        Err(code):\n            return\n").unwrap();
}
#[test]
fn the_sum_is_not_touched_while_a_name_into_it_is_in_use() {
    let lent = "this value is lent";
    refused(step("            *light = Red\n            *left = 5\n"), lent);
    refused(step("            let copy = *light\n            *left = 5\n"), lent);
    refused(step("            if *left == 0:\n                *light = Red\n            else:\n                *left -= 1\n"), lent);
    // A loop comes round again: a use anywhere in it is a use after everything in it.
    refused(step("            var turns: U64 = 0\n            while turns < 2:\n                *left = 5\n                *light = Red\n                turns += 1\n"), lent);
    refused(step("            for turn in 0..2:\n                if turn == 1:\n                    *left = 5\n                else:\n                    *light = Red\n"), lent);
    refused(checked("fn main(console: Console<Write>):\n    var light = Green(3)\n    match &mut light:\n        Green(left):\n            let other = light\n            *left = 4\n        Red:\n            return\n"), lent);
    // Another field may be written meanwhile; the matched field may not.
    checked("fn main(console: Console<Write>):\n    var lamp = Lamp(Green(3), 2)\n    match &mut lamp.light:\n        Green(left):\n            lamp.level = 1\n            *left = 4\n        Red:\n            return\n").unwrap();
    refused(checked("fn main(console: Console<Write>):\n    var lamp = Lamp(Green(3), 2)\n    match &mut lamp.light:\n        Green(left):\n            lamp.light = Red\n            *left = 4\n        Red:\n            return\n"), lent);
}
#[test]
fn a_nested_pattern_reaches_the_inner_value_and_keeps_the_outer_sum_lent() {
    let nested = |arm: &str| checked(&format!("type Option<T>:\n    Some(T)\n    None\n\nfn change(r: &mut Result<Option<U64>, U64>):\n    match r:\n        Ok(Some(x)):\n{arm}        Ok(None):\n            return\n        Err(code):\n            *code = 0\n\nfn main(console: Console<Write>):\n    var value: Result<Option<U64>, U64> = Ok(Some(3))\n    change(&mut value)\n"));
    nested("            *x += 1\n").unwrap();
    refused(nested("            *r = Err(1)\n            *x = 2\n"), "this value is lent");
}
#[test]
fn a_name_does_not_outlive_the_sum_or_reach_where_it_may_not() {
    refused(checked("fn pick() -> Result<&mut U64, U64>:\n    var light = Green(3)\n    match &mut light:\n        Green(left):\n            return Ok(left)\n        Red:\n            return Err(1)\n\nfn main(console: Console<Write>):\n    return\n"), "a returned reference must borrow from a reference parameter");
    // Both values of one variant, changed where they are (2026-11-18: loans through the
    // arm's reference are counted by field).
    checked("fn widen(shape: &mut Shape):\n    match shape:\n        Circle(radius):\n            *radius += 1\n        Rect(width, height):\n            *width += *height\n        Empty:\n            return\n\nfn main(console: Console<Write>):\n    var shape = Rect(2, 3)\n    widen(&mut shape)\n").unwrap();
    // A scalar-proved function may match through `&mut` (the walk treats the sum as lent).
    checked("fn poke(light: &mut Light):\n    ensures true\n    proof:\n        auto\n    match light:\n        Green(left):\n            *left = 0\n        Red:\n            return\n\nfn main(console: Console<Write>):\n    return\n").unwrap();
    // Through `&` the names are shared references: nothing is written through them.
    refused(checked("fn poke(light: &Light):\n    match light:\n        Green(left):\n            *left = 0\n        Red:\n            return\n\nfn main(console: Console<Write>):\n    return\n"), "");
}
