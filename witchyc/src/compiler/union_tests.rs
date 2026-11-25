//! Anonymous sums: tags, widening, and what each refusal says.
use crate::diagnostic::Diagnostic;
const TYPES:&str="type Small = .[Empty | TooBig(U64)]\ntype Wide = .[Empty | Missing | TooBig(U64)]\ntype Other = .[Empty | TooBig(U8)]\ntype Held = .[None | Some(Buffer<U8>)]\n\n";
fn checked(rest:&str)->Result<(),Diagnostic>{
    let source=format!("{TYPES}{rest}\nfn main():\n    return\n");
    let logical=super::check::program(&crate::compiler::parse(&source)?)?;super::concrete::program(logical)?;Ok(())
}
fn refused(rest:&str)->String{checked(rest).expect_err(rest).message.to_string()}
const CODE:&str="fn code(e: Wide) -> U64:\n    match e:\n        .Empty:\n            return 1\n        .Missing:\n            return 2\n        .TooBig(n):\n            return n\n";
#[test]
fn tags_build_values_of_the_expected_sum_and_a_match_takes_them_apart(){
    checked(CODE).unwrap();
    checked("fn make(n: U64) -> Small:\n    if n == 0:\n        return .Empty\n    return .TooBig(n)\n").unwrap();
    checked("fn make() -> Result<U64, Small>:\n    return Err(.TooBig(3))\n").unwrap();
    checked("fn same(e: .[TooBig(U64) | Empty]) -> Small:\n    return e\n").unwrap();
    checked("fn pair<T>(value: T) -> .[One(T) | Two(T)]:\n    return .Two(value)\n\nfn use_it() -> .[One(U64) | Two(U64)]:\n    return pair(4)\n").unwrap();
    checked("fn hold(data: Buffer<U8>) -> Held:\n    return .Some(move data)\n").unwrap();
}
#[test]
fn a_smaller_sum_widens_at_arguments_returns_bindings_and_the_question_mark(){
    checked(&format!("{CODE}fn run(e: Small) -> U64:\n    return code(e)\n")).unwrap();
    checked("fn widen(e: Small) -> Wide:\n    return e\n").unwrap();
    checked("fn widen(e: Small) -> Wide:\n    let wide: Wide = e\n    return wide\n").unwrap();
    checked("fn small() -> Result<U64, Small>:\n    return Ok(1)\n\nfn wide() -> Result<U64, Wide>:\n    let n = small()?\n    return Ok(n + 1)\n").unwrap();
    checked("fn held() -> Result<U64, Held>:\n    return Ok(1)\n\nfn wide() -> Result<U64, .[None | Some(Buffer<U8>) | Late]>:\n    let n = held()?\n    return Ok(n)\n").unwrap();
}
#[test]
fn widening_never_drops_a_tag_or_changes_what_one_holds(){
    assert!(refused("fn narrow(e: Wide) -> Small:\n    return e\n").contains("becomes a wider one only"));
    assert!(refused("fn change(e: Small) -> Other:\n    return e\n").contains("becomes a wider one only"));
    assert!(refused("fn wide() -> Result<U64, Wide>:\n    return Ok(1)\n\nfn small() -> Result<U64, Small>:\n    let n = wide()?\n    return Ok(n)\n").contains("same error type"));
}
#[test]
fn a_tag_needs_an_expected_sum_that_has_it(){
    assert!(refused("fn f() -> U64:\n    let e = .Empty\n    return 1\n").contains("takes its type from where it is used"));
    assert!(refused("fn f() -> Small:\n    return .Missing\n").contains("has no such tag"));
    assert!(refused("fn f() -> Small:\n    return .TooBig\n").contains("holds a value"));
    assert!(refused("fn f() -> Small:\n    return .Empty(1)\n").contains("holds nothing"));
    assert!(refused("fn f(e: .[A | A(U64)]) -> U64:\n    return 1\n").contains("names each tag once"));
}
#[test]
fn a_match_on_an_anonymous_sum_names_every_tag_with_its_dot(){
    assert!(refused(&CODE.replace("        .Missing:\n            return 2\n","")).contains("missing: .Missing"));
    assert!(refused(&CODE.replace(".Missing:","Missing:")).contains("does not have"));
    assert!(refused("fn drop(h: Held) -> U64:\n    match move h:\n        .None:\n            return 0\n        _:\n            return 1\n").contains("would lose the owner"));
}
#[test]
fn a_proof_over_numbers_passes_anonymous_sums_by_and_a_memory_proof_refuses_them(){
    checked("fn f(n: U64) -> Small:\n    ensures n == n\n    proof:\n        auto\n    return .Empty\n").unwrap();
    assert!(refused("fn f(buffer: &mut Buffer<U8>) -> U64:\n    ensures memory.same_allocation(buffer, old(buffer))\n    proof:\n        auto\n    let e: Small = .Empty\n    return 0\n").contains("anonymous sum"));
}
