//! Methods: `impl` blocks, receivers, and what each refusal says.
use crate::diagnostic::Diagnostic;
const TYPES:&str="type Counter:\n    count: U64\n\ntype Holder:\n    data: Buffer<U8>\n\ntype Pair<T>:\n    first: T\n    second: T\n\nimpl Counter:\n    fn new() -> Counter:\n        return Counter(0)\n\n    fn value(self: &Counter) -> U64:\n        return (*self).count\n\n    fn bump(self: &mut Counter):\n        (*self).count = (*self).count + 1\n\n    fn twice(self: &mut Counter):\n        self.bump()\n        self.bump()\n\nimpl Holder:\n    fn open(self: Holder) -> Buffer<U8>:\n        return move self.data\n\nimpl<T> Pair<T>:\n    fn swap(self: Pair<T>) -> Pair<T>:\n        return Pair(self.second, self.first)\n\n    fn head(self: &Pair<T>) -> T:\n        return (*self).first\n\n";
fn checked(rest:&str)->Result<(),Diagnostic>{
    let source=format!("{TYPES}{rest}\nfn main():\n    return\n");
    let syntax=super::methods::lift(crate::parser::parse(&source)?)?;
    let logical=super::check::program(&syntax)?;super::concrete::program(logical)?;Ok(())
}
fn refused(rest:&str)->String{checked(rest).expect_err(rest).message.to_string()}
#[test]
fn a_receiver_is_lent_moved_or_copied_as_the_method_asks(){
    checked("fn run() -> U64:\n    var counter = Counter.new()\n    counter.bump()\n    counter.twice()\n    return counter.value()\n").unwrap();
    checked("fn run(pair: Pair<U64>) -> U64:\n    let swapped = pair.swap()\n    return swapped.head() + pair.head()\n").unwrap();
    checked("fn run(counter: &mut Counter) -> U64:\n    counter.bump()\n    return counter.value()\n").unwrap();
    checked("fn run(holder: Holder) -> Buffer<U8>:\n    return holder.open()\n").unwrap();
}
#[test]
fn a_moved_receiver_is_gone(){
    let message=refused("fn run(holder: Holder) -> Buffer<U8>:\n    let data = holder.open()\n    return holder.open()\n");
    assert!(!message.contains("method"),"{message}");
}
#[test]
fn method_calls_name_a_method_of_the_receiver_type(){
    assert!(refused("fn run(counter: Counter) -> U64:\n    return counter.missing()\n").contains("no method of this name"));
    assert!(refused("fn run(counter: Counter) -> Counter:\n    return counter.new()\n").contains("takes no receiver"));
    assert!(refused("fn run(counter: &Counter):\n    counter.bump()\n").len()>0);
}
#[test]
fn an_impl_block_is_for_every_instance_of_a_declared_type_or_for_one(){
    checked("impl Pair<U64>:\n    fn sum(self: Pair<U64>) -> U64:\n        return self.first + self.second\n\nfn run(pair: Pair<U64>) -> U64:\n    return pair.sum()\n").unwrap();
    assert!(refused("impl Pair<U64>:\n    fn sum(self: Pair<U64>) -> U64:\n        return self.first + self.second\n\nfn run(pair: Pair<Bool>) -> U64:\n    return pair.sum()\n").len()>0);
    assert!(refused("impl<T> Pair<U64>:\n    fn sum(self: Pair<U64>) -> U64:\n        return 1\n").contains("every instance"));
    assert!(refused("impl<T> Pair<T>:\n    fn map<T>(self: Pair<T>) -> T:\n        return self.first\n").contains("repeats"));
    assert!(refused("impl Counter:\n    fn value(self: &Counter) -> U64:\n        return 1\n").len()>0);
}
#[test]
fn a_copy_value_with_no_name_is_borrowed_where_it_is_written(){
    checked("fn run(pair: Pair<U64>) -> U64:\n    return pair.swap().head()\n").unwrap();
    checked("fn double(value: &U64) -> U64:\n    return *value * 2\n\nfn run() -> U64:\n    return double(&21) + double(&(1 + 2)) + Counter(4).value()\n").unwrap();
    checked("fn pick(value: &U64) -> &U64:\n    return value\n\nfn run() -> U64:\n    let kept = pick(&7)\n    return *kept\n").unwrap();
}
#[test]
fn a_borrowed_temporary_is_shared_and_copy(){
    assert!(refused("fn bump(value: &mut U64):\n    *value += 1\n\nfn run():\n    bump(&mut 3)\n").contains("bind it with `var`"));
    assert!(refused("fn run():\n    Counter(1).bump()\n").contains("bind it with `var`"));
    assert!(refused("fn size(holder: &Holder) -> U64:\n    return 1\n\nfn pass(holder: Holder) -> Holder:\n    return move holder\n\nfn run(holder: Holder) -> U64:\n    return size(&pass(move holder))\n").contains("must be a Copy value"));
}
