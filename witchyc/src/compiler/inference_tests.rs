//! Type inference recalls what it answered within one inference (`check.rs` `infer_hint`): an
//! operator on a program's own type is written as a call whose arguments are read again at
//! every level, and that made a chain of n operators take about 3^n steps.
use crate::diagnostic::Diagnostic;
const MONEY:&str="type Money:\n    cents: U64\n\ntrait Add:\n    fn add(self: Self, other: Self) -> Self\n\nimpl Add for Money:\n    fn add(self: Money, other: Money) -> Money:\n        return Money(self.cents + other.cents)\n\n";
fn checked(body:&str)->Result<String,Diagnostic>{
    let source=format!("{MONEY}fn total(m: Money, n: Money) -> Money:\n    return {body}\n\nfn main():\n    return\n");
    let logical=super::check::program(&crate::compiler::parse(&source)?)?;
    let total=logical.functions.iter().find(|f|f.name=="total").expect("total");
    // Where the `return` is written is for refusals only; the body is compared without it.
    let body=total.body.iter().cloned().map(|s|match s{super::logical_body::Stmt::Return(e,x,_)=>super::logical_body::Stmt::Return(e,x,None),s=>s}).collect::<Vec<_>>();
    Ok(format!("{body:?}"))
}
fn chain(terms:usize)->String{vec!["m";terms].join(" + ")}

// These tests call the checker directly at its nesting limit. Give it the
// same stack as witchyc, regardless of the Rust test runner's thread size.
fn with_compiler_stack(test: impl FnOnce() + Send) {
    std::thread::scope(|scope| {
        super::parallel::spawn(scope, test).join().unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    });
}

#[test]
fn a_long_chain_of_a_programs_operator_is_typed_in_steps_in_proportion_to_its_length(){
    with_compiler_stack(|| {
        // 20 terms took 92 s, and were refused by the bound on work after 14; this is immediate.
        let started=std::time::Instant::now();
        for terms in [2,14,20,32,41]{checked(&chain(terms)).unwrap_or_else(|e|panic!("{terms} terms: {}",e.message));}
        assert!(started.elapsed()<std::time::Duration::from_secs(20),"{:?}",started.elapsed());
        // One inference goes 120 expressions deep, as before: three for each such operator.
        assert!(checked(&chain(42)).expect_err("42 terms").message.contains("generic inference complexity limit"));
    });
}
#[test]
fn a_chain_is_the_calls_it_stands_for(){
    with_compiler_stack(|| {
        // What is recalled is what would have been read again: the body is the body of the calls
        // written out, operand for operand.
        let by_hand=|terms:usize|(1..terms).fold("m".to_string(),|left,_|format!("Add.add({left}, m)"));
        for terms in [2,3,6,12]{assert_eq!(checked(&chain(terms)).unwrap(),checked(&by_hand(terms)).unwrap(),"{terms} terms");}
        // The same expression with another meaning is not recalled as the first: `n` in place of one
        // `m`, and a subtree that appears twice with different operands around it.
        assert_ne!(checked("m + m + n + m").unwrap(),checked("m + m + m + m").unwrap());
        assert_eq!(checked("(m + n) + (m + n)").unwrap(),checked("Add.add(Add.add(m, n), Add.add(m, n))").unwrap());
        assert_ne!(checked("(m + n) + (n + m)").unwrap(),checked("(m + n) + (m + n)").unwrap());
    });
}
