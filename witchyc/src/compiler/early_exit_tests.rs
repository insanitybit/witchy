//! `?` and refutable `let`: what is accepted, and what each refusal says.
use crate::diagnostic::Diagnostic;
const HELPERS:&str="fn half(value: U64) -> Result<U64, U64>:\n    if value % 2 == 0:\n        return Ok(value / 2)\n    return Err(value)\n\nfn flag(value: U64) -> Result<U64, Bool>:\n    return Err(true)\n\nfn unit(value: U64) -> Result<(), U64>:\n    return Ok(())\n\nfn keep(data: Buffer<U8>) -> Result<Buffer<U8>, U64>:\n    return Ok(move data)\n\n";
fn checked(function:&str)->Result<(),Diagnostic>{
    let source=format!("{HELPERS}{function}\nfn main():\n    return\n");
    let logical=super::check::program(&crate::compiler::parse(&source)?)?;super::concrete::program(logical)?;Ok(())
}
fn refused(function:&str)->String{checked(function).expect_err(function).message.to_string()}
#[test]
fn a_question_mark_binds_the_value_or_hands_the_error_back(){
    checked("fn f(v: U64) -> Result<U64, U64>:\n    let a = half(v)?\n    let b = half(a)?\n    return Ok(b)\n").unwrap();
    checked("fn f(v: U64) -> Result<U64, U64>:\n    unit(v)?\n    return Ok(v)\n").unwrap();
    checked("fn f(v: U64) -> Result<U64, U64>:\n    var n = v\n    while n > 4:\n        let h = half(n)?\n        n = h\n    return Ok(n)\n").unwrap();
    checked("fn f(data: Buffer<U8>) -> Result<Buffer<U8>, U64>:\n    let kept = keep(move data)?\n    return Ok(move kept)\n").unwrap();
}
#[test]
fn a_refutable_let_names_what_the_other_arm_does(){
    checked("fn f(v: U64) -> U64:\n    let Ok(a) = half(v) else Err(e):\n        return e + 7\n    return a\n").unwrap();
    assert!(refused("fn f(v: U64) -> U64:\n    let Ok(a) = half(v) else Err(e):\n        let unused = e\n    return a\n").contains("fall through"));
}
#[test]
fn a_question_mark_needs_a_result_function_with_the_same_error(){
    assert!(refused("fn f(v: U64) -> U64:\n    let a = half(v)?\n    return a\n").contains("must return a Result"));
    assert!(refused("fn f(v: U64) -> Result<U64, U64>:\n    let a = flag(v)?\n    return Ok(a)\n").contains("same error type"));
    assert!(refused("fn f(v: U64) -> Result<U64, U64>:\n    let a = v?\n    return Ok(a)\n").contains("follows a Result"));
}
#[test]
fn a_question_mark_works_inside_a_value_when_nothing_runs_before_it(){
    checked("fn f(v: U64) -> Result<U64, U64>:\n    var a = half(v)?\n    a = half(a)?\n    return Ok(a)\n").unwrap();
    checked("fn f(v: U64) -> Result<U64, U64>:\n    let a = half(v)? + half(half(v)?)?\n    return Ok(a)\n").unwrap();
    checked("fn f(v: U64) -> Result<U64, U64>:\n    if half(v)? > 1:\n        return Ok(1)\n    return Ok(half(v)?)\n").unwrap();
    assert!(refused("fn f(v: U64) -> Result<U64, U64>:\n    let a = unit(v) == unit(v) && half(v)? > 1\n    return Ok(v)\n").contains("may not run"));
    assert!(refused("fn f(v: U64) -> Result<U64, Bool>:\n    let a = half(v)? + 1\n    return Ok(a)\n").contains("same error"));
    // A number the success holds may be left unused; an owner may not (`tests/fixtures/try/unused-owner.refused.wy`).
    checked("fn f(v: U64) -> Result<U64, U64>:\n    half(v)?\n    return Ok(v)\n").unwrap();
    // A loop's condition is asked before each pass.
    checked("fn f(v: U64) -> Result<U64, U64>:\n    var n = v\n    while half(n)? > 1:\n        n = n / 2\n    return Ok(n)\n").unwrap();
}
#[test]
fn a_question_mark_cannot_leave_an_owner_behind(){
    // `data` is still owned when the error leaves; nothing closes it.
    let message=refused("fn f(data: Buffer<U8>, v: U64) -> Result<Buffer<U8>, U64>:\n    let a = half(v)?\n    return Ok(move data)\n");
    // The ownership refusal, placed at the `?` that leaves and naming what it leaves behind.
    assert!(message.contains("this `?` returns early while `data` still holds an owner"),"{message}");
}
