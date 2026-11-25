//! Anonymous records: `.{label: Type}` types, `.{label: value}` values, opened by label.
use crate::diagnostic::Diagnostic;
fn checked(source:&str)->Result<(),Diagnostic>{let syntax=crate::compiler::parse(source)?;let logical=super::check::program(&syntax)?;super::concrete::program(logical)?;Ok(())}
fn refused(source:&str)->String{checked(source).expect_err(source).message.to_string()}
const CLOSE:&str="fn close(buffer: Buffer<U8>):\n    match memory.try_release(move buffer):\n        Ok:\n            return\n        Err(rest):\n            close(move rest)\n\n";
const MAIN:&str="fn main():\n    return\n";
#[test]
fn a_shape_is_one_type_whatever_order_its_labels_are_written_in(){
    checked(&format!("fn make(x: U64, y: U64) -> .{{x: U64, y: U64}}:\n    return .{{y: y, x: x}}\n\nfn sum(point: .{{y: U64, x: U64}}) -> U64:\n    return point.x + point.y\n\nfn both() -> U64:\n    return sum(make(1, 2))\n\n{MAIN}")).unwrap();
    // Shorthand, nesting and literals typed by the expected shape.
    checked(&format!("fn nest(x: U8) -> .{{inner: .{{x: U8}}, tag: U16}}:\n    return .{{inner: .{{x}}, tag: 7}}\n\n{MAIN}")).unwrap();
    assert!(refused(&format!("fn make(x: U64) -> .{{x: U64, y: U64}}:\n    return .{{x: x}}\n\n{MAIN}")).contains("type"));
    assert!(refused(&format!("fn make(x: U64) -> .{{x: U64}}:\n    return .{{x: x, x: x}}\n\n{MAIN}")).contains("duplicate"));
}
#[test]
fn an_anonymous_record_carries_owners_and_is_opened_by_label(){
    checked(&format!("{CLOSE}fn join(left: Buffer<U8>, right: Buffer<U8>, count: U64) -> .{{left: Buffer<U8>, right: Buffer<U8>, count: U64}}:\n    return .{{left: move left, right: move right, count: count}}\n\nfn finish(pair: .{{count: U64, left: Buffer<U8>, right: Buffer<U8>}}) -> U64:\n    let .{{right: second, left, count}} = move pair\n    close(move left)\n    close(move second)\n    return count\n\n{MAIN}")).unwrap();
    let finish=|body:&str|format!("{CLOSE}fn finish(pair: .{{left: Buffer<U8>, right: Buffer<U8>}}):\n{body}\n{MAIN}");
    assert!(refused(&finish("    let .{left, right} = move pair\n    close(move left)\n    return\n")).contains("discard"));
    assert!(refused(&finish("    let .{left} = move pair\n    close(move left)\n    return\n")).contains("every field"));
    assert!(refused(&finish("    let .{left, middle} = move pair\n    return\n")).contains("does not have"));
    assert!(refused(&finish("    let .{left, left: again} = move pair\n    return\n")).contains("once"));
    assert!(refused(&finish("    return\n")).contains("discard"));
}
#[test]
fn it_works_through_generics_and_keeps_named_records_apart(){
    checked(&format!("{CLOSE}fn pair<T: Any>(item: T, spare: Buffer<U8>) -> .{{item: T, spare: Buffer<U8>}}:\n    return .{{item: move item, spare: move spare}}\n\nfn item<T: Any>(pair: .{{item: T, spare: Buffer<U8>}}) -> T:\n    let .{{item, spare}} = move pair\n    close(move spare)\n    return move item\n\nfn round(a: Buffer<U8>, b: Buffer<U8>) -> Buffer<U8>:\n    return item(pair(move a, move b))\n\n{MAIN}")).unwrap();
    checked(&format!("fn swap<T>(pair: .{{first: T, second: T}}) -> .{{first: T, second: T}}:\n    return .{{first: pair.second, second: pair.first}}\n\nfn use_it() -> U64:\n    let pair = swap(.{{first: 1, second: 2}})\n    return pair.first\n\n{MAIN}")).unwrap();
    let named="type Point:\n    x: U64\n    y: U64\n\n";
    assert!(refused(&format!("{named}fn make() -> Point:\n    return .{{x: 1, y: 2}}\n\n{MAIN}")).contains("type"));
    assert!(refused(&format!("{named}fn open(point: Point) -> U64:\n    let .{{x, y}} = point\n    return x\n\n{MAIN}")).contains("anonymous record"));
}
#[test]
fn operands_with_effects_keep_their_written_order(){
    let helper="fn next(buffer: &Buffer<U8>) -> U64:\n    return memory.capacity(buffer)\n\n";
    checked(&format!("{helper}fn make(buffer: &Buffer<U8>) -> .{{a: U64, b: U64}}:\n    return .{{a: next(buffer), b: next(buffer)}}\n\n{MAIN}")).unwrap();
    assert!(refused(&format!("{helper}fn make(buffer: &Buffer<U8>) -> .{{a: U64, b: U64}}:\n    return .{{b: next(buffer), a: next(buffer)}}\n\n{MAIN}")).contains("label order"));
}
#[test]
fn an_alias_is_another_spelling_and_not_a_new_type(){
    let pair="type Pair<T> = .{first: T, second: T}\ntype Bytes = Buffer<U8>\ntype Held = Pair<Bytes>\n\n";
    checked(&format!("{pair}{CLOSE}fn make(a: Bytes, b: Bytes) -> Held:\n    return .{{first: move a, second: move b}}\n\nfn finish(pair: .{{second: Buffer<U8>, first: Buffer<U8>}}):\n    let .{{first, second}} = move pair\n    close(move first)\n    close(move second)\n\nfn round(a: Bytes, b: Bytes):\n    finish(make(move a, move b))\n\nfn swap<T: Any>(pair: Pair<T>) -> Pair<T>:\n    let .{{first, second}} = move pair\n    return .{{first: move second, second: move first}}\n\n{MAIN}")).unwrap();
    assert!(refused(&format!("{pair}fn bad(pair: Pair<U64, U64>) -> U64:\n    return 0\n\n{MAIN}")).contains("argument count"));
    assert!(refused(&format!("type Loop = Loop\nfn bad(value: Loop) -> U64:\n    return 0\n\n{MAIN}")).contains("complexity"));
    assert!(refused(&format!("type Pair = U64\ntype Pair:\n    x: U64\n\n{MAIN}")).contains("duplicate"));
    assert!(refused(&format!("type Size = U64\nfn Size() -> U64:\n    return 0\n\n{MAIN}")).contains("duplicate")||refused(&format!("type Size = U64\nfn Size() -> U64:\n    return 0\n\n{MAIN}")).contains("alias"));
}
