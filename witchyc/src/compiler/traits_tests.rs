//! Traits: declaring, implementing, bounding, and what each refusal says.
use crate::diagnostic::Diagnostic;
const TYPES:&str="trait Area:\n    fn area(self: &Self) -> U64\n\ntrait Same:\n    fn same(self: &Self, other: &Self) -> Bool\n\ntype Square:\n    side: U64\n\ntype Plain:\n    value: U64\n\ntype Holder:\n    data: Buffer<U8>\n\ntype Pair<T>:\n    first: T\n    second: T\n\nimpl Area for Square:\n    fn area(self: &Square) -> U64:\n        return (*self).side\n\nimpl Area for Holder:\n    fn area(self: &Holder) -> U64:\n        return 1\n\nimpl Same for Square:\n    fn same(self: &Square, other: &Square) -> Bool:\n        return (*self).side == (*other).side\n\nimpl<T: Same> Same for Pair<T>:\n    fn same(self: &Pair<T>, other: &Pair<T>) -> Bool:\n        return (*self).first.same(&(*other).first)\n\nfn twice<T: Area>(shape: &T) -> U64:\n    return shape.area() + shape.area()\n\n";
fn checked(rest:&str)->Result<(),Diagnostic>{
    let source=format!("{TYPES}{rest}\nfn main():\n    return\n");
    let logical=super::check::program(&super::parse(&source)?)?;super::concrete::program(logical)?;Ok(())
}
fn refused(rest:&str)->String{checked(rest).expect_err(rest).message.to_string()}
#[test]
fn bounded_code_calls_the_methods_of_its_bounds(){
    checked("fn run(square: Square) -> U64:\n    return twice(&square) + square.area()\n").unwrap();
    checked("fn both<T: Area + Same>(left: &T, right: &T) -> U64:\n    if left.same(right):\n        return left.area()\n    return twice(right)\n\nfn run(square: Square) -> U64:\n    return both(&square, &square)\n").unwrap();
    // `Any` and a trait together: an owner is passed through and still measured.
    checked("fn keep<T: Any + Area>(shape: T) -> T:\n    let size = shape.area()\n    return move shape\n\nfn run(holder: Holder) -> Holder:\n    return keep(move holder)\n").unwrap();
    checked("fn run(left: Pair<Square>, right: Pair<Square>) -> Bool:\n    return left.same(&right)\n").unwrap();
}
#[test]
fn a_bound_is_met_by_an_impl_or_by_the_callers_own_bound(){
    assert!(refused("fn run(plain: Plain) -> U64:\n    return twice(&plain)\n").contains("`Plain` does not implement `Area`"));
    assert!(refused("fn outer<T>(shape: &T) -> U64:\n    return twice(shape)\n").contains("does not implement `Area`"));
    assert!(refused("fn outer<T: Same>(shape: &T) -> U64:\n    return twice(shape)\n").contains("does not implement `Area`"));
    // A conditional impl holds only where its own bound does.
    assert!(refused("fn run(left: Pair<Plain>, right: Pair<Plain>) -> Bool:\n    return left.same(&right)\n").contains("`Plain` does not implement `Same`"));
    assert!(refused("fn outer<T: Missing>(shape: &T) -> U64:\n    return 0\n").contains("names a declared trait"));
}
#[test]
fn a_type_parameter_has_only_the_methods_of_its_bounds(){
    assert!(refused("fn outer<T>(shape: &T) -> U64:\n    return shape.area()\n").contains("no trait this type parameter is bound by"));
    assert!(refused("fn outer<T: Same>(shape: &T) -> U64:\n    return shape.area()\n").contains("no trait this type parameter is bound by"));
}
#[test]
fn an_impl_matches_its_trait(){
    assert!(refused("impl Area for Plain:\n    fn area(self: &Plain, extra: U64) -> U64:\n        return 1\n").contains("must match the trait's"));
    assert!(refused("impl Area for Plain:\n    fn area(self: Plain) -> U64:\n        return 1\n").contains("must match the trait's"));
    assert!(refused("impl Area for Plain:\n    fn area(self: &Plain) -> Bool:\n        return true\n").contains("must match the trait's"));
    assert!(refused("impl Same for Plain:\n    fn other(self: &Plain) -> U64:\n        return 1\n").contains("not one of the trait's"));
    assert!(refused("trait Two:\n    fn one(self: &Self) -> U64\n    fn two(self: &Self) -> U64\n\nimpl Two for Plain:\n    fn one(self: &Plain) -> U64:\n        return 1\n").contains("provides every method"));
    assert!(refused("impl Missing for Plain:\n    fn area(self: &Plain) -> U64:\n        return 1\n").contains("names a declared trait"));
    assert!(refused("impl Area for Square:\n    fn area(self: &Square) -> U64:\n        return 2\n").contains("implements a trait once"));
    assert!(refused("impl Area for Plain:\n    fn area(self: &Plain) -> U64:\n        requires self.value > 0\n        ensures result == 1\n        proof:\n            auto\n        return 1\n").contains("cannot add `requires`"));
}
#[test]
fn signatures_and_bodies_are_where_they_belong(){
    assert!(checked("trait Bad:\n    fn area(self: &Self) -> U64:\n        return 1\n").is_err());
    assert!(checked("fn bare(value: U64) -> U64\n").is_err());
    assert!(checked("impl Area for Plain:\n    fn area(self: &Plain) -> U64\n").is_err());
}
