//! `T: Any` type parameters: values that may own, moved exactly once.
use crate::diagnostic::Diagnostic;
fn checked(source:&str)->Result<(),Diagnostic>{let syntax=crate::compiler::parse(source)?;let logical=super::check::program(&syntax)?;super::concrete::program(logical)?;Ok(())}
fn refused(source:&str)->String{checked(source).expect_err(source).message.to_string()}
const BOX:&str="type Box:\n    data: Buffer<U8>\n    length: U64\n\n";
const SIZE_ANY:&str="fn size<T: Any>(buffer: &Buffer<T>) -> U64:\n    return memory.capacity(buffer)\n\n";
const SIZE_COPY:&str="fn size<T>(buffer: &Buffer<T>) -> U64:\n    return memory.capacity(buffer)\n\n";
fn main_with_buffer_of_boxes()->&'static str{"fn main(allocator: Allocator):\n    let made: Result<Buffer<Box>, U64> = memory.try_alloc(&allocator, 2)\n    match move made:\n        Ok(buffer):\n            let seen = size(&buffer)\n            hold(move buffer)\n        Err(code):\n            return\n\nfn hold(buffer: Buffer<Box>):\n    match memory.try_release(move buffer):\n        Ok:\n            return\n        Err(rest):\n            hold(move rest)\n"}
#[test]
fn an_owner_can_be_a_buffer_element_and_pass_through_any_parameters(){
    checked(&format!("{BOX}{SIZE_ANY}{}",main_with_buffer_of_boxes())).unwrap();
    checked("fn pass<T: Any>(item: T) -> T:\n    return move item\n\nfn main():\n    let x: U64 = pass(3)\n    return\n").unwrap();
}
#[test]
fn a_plain_type_parameter_still_means_copy_and_refuses_owners(){
    assert!(refused(&format!("{BOX}{SIZE_COPY}{}",main_with_buffer_of_boxes())).contains("Copy storage"));
}
#[test]
fn an_any_body_cannot_copy_or_drop_its_values(){
    assert!(refused("fn twice<T: Any>(item: T) -> T:\n    return item\n\nfn main():\n    return\n").contains("explicit move"));
    assert!(refused("fn lose<T: Any>(item: T):\n    return\n\nfn main():\n    return\n").contains("discard"));
    assert!(refused("fn both<T: Any>(item: T, sink: &mut Buffer<T>):\n    memory.init_unchecked(sink, 0, move item)\n    memory.init_unchecked(sink, 1, move item)\n    return\n\nfn main():\n    return\n").contains("moved"));
}
#[test]
fn only_take_and_unchecked_init_touch_cells_that_may_own(){
    for body in ["    return memory.get_unchecked(buffer, 0)\n","    match memory.read(buffer, 0):\n        Ok(item):\n            return move item\n        Err(code):\n            return move spare\n"]{
        assert!(refused(&format!("fn peek<T: Any>(buffer: &Buffer<T>, spare: T) -> T:\n{body}\nfn main():\n    return\n")).contains("Copy storage"),"{body}");}
    for body in ["    let _ = memory.init(buffer, 0, move spare)\n    return\n","    memory.swap(buffer, 0, move spare)\n    return\n"]{
        assert!(refused(&format!("fn put<T: Any>(buffer: &mut Buffer<T>, spare: T):\n{body}\nfn main():\n    return\n")).contains("Copy storage"),"{body}");}
    checked("fn out<T: Any>(buffer: &mut Buffer<T>) -> Result<T, U64>:\n    return memory.take(buffer, 0)\n\nfn main():\n    return\n").unwrap();
    assert!(refused("fn spill<T: Any>(buffer: &mut Buffer<T>):\n    let _ = memory.take(buffer, 0)\n    return\n\nfn main():\n    return\n").contains("discard"));
}
const PAIR:&str="type Pair<T>:\n    left: Buffer<U8>\n    right: T\n\n";
const CLOSE:&str="fn close(buffer: Buffer<U8>):\n    match memory.try_release(move buffer):\n        Ok:\n            return\n        Err(rest):\n            close(move rest)\n\n";
#[test]
fn a_record_with_several_owners_is_built_and_opened_whole(){
    checked(&format!("{PAIR}{CLOSE}fn join(left: Buffer<U8>, right: Buffer<U8>) -> Pair<Buffer<U8>>:\n    return Pair(move left, move right)\n\nfn finish(pair: Pair<Buffer<U8>>):\n    let Pair(left, right) = move pair\n    close(move left)\n    close(move right)\n\nfn swap<T: Any>(pair: Pair<T>, other: Buffer<U8>) -> Pair<T>:\n    let Pair(left, right) = move pair\n    close(move left)\n    return Pair(move other, move right)\n\nfn main():\n    return\n")).unwrap();
    // A Copy field is simply read.
    checked(&format!("{PAIR}{CLOSE}fn right(pair: Pair<U64>) -> U64:\n    let Pair(left, right) = move pair\n    close(move left)\n    return right\n\nfn main():\n    return\n")).unwrap();
}
#[test]
fn every_owner_in_an_opened_record_is_still_owed(){
    let finish=|body:&str|format!("{PAIR}{CLOSE}fn finish(pair: Pair<Buffer<U8>>):\n{body}\nfn main():\n    return\n");
    assert!(refused(&finish("    let Pair(left, right) = move pair\n    close(move left)\n    return\n")).contains("discard"));
    assert!(refused(&finish("    let Pair(left, right) = move pair\n    close(move left)\n    close(move right)\n    close(move left)\n    return\n")).contains("moved"));
    assert!(refused(&finish("    let Pair(left, right) = move pair\n    let Pair(a, b) = move pair\n    return\n")).contains("moved"));
    // Taking one owner out would lose the other.
    assert!(refused(&finish("    let left = move pair.left\n    close(move left)\n    return\n")).contains("several owners"));
    assert!(refused(&finish("    let Pair(left) = move pair\n    return\n")).contains("every field"));
    assert!(refused(&format!("{PAIR}type Other:\n    left: Buffer<U8>\n    right: Buffer<U8>\n\n{CLOSE}fn finish(pair: Pair<Buffer<U8>>):\n    let Other(left, right) = move pair\n    return\n\nfn main():\n    return\n")).contains("named record type"));
}
#[test]
fn a_bound_is_any_or_a_declared_trait(){
    assert!(refused("fn f<T: Copy>(item: T):\n    return\n\nfn main():\n    return\n").contains("names a declared trait or `Any`"));
}
