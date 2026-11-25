//! An owner assigned over a field whose old value is proved to hold none is one store
//! (docs/wasm-exchange.md).
//!
//! The checker writes `place = value` as `held = value; memory.exchange(&mut place, &mut held);
//! match move held:` with an empty arm for each variant that holds no owner and `unreachable`
//! for the others. Once the function's proof is accepted (`proved_unreachable`: every explicit
//! trap in it has its own kernel-checked obligation), the arms that trap are never taken, the
//! others do nothing, and `held` is moved away, so nothing can read it again. What is left is
//! `place = value`.
//!
//! Only statements the checker wrote for an assignment are rewritten: the hidden local they
//! exchange through is marked (`Function::overwrites`), and no source can name or mark one. The
//! same statements written by hand are emitted as written, guarded by their match, whatever was
//! proved (review of 2026-11-21: the rewrite fired on shape alone). Without the proof the
//! checker's own are emitted as written too.
//!
//! `WITCHY_AUDIT_GUARDED_STORES` (set by whoever runs the compiler; nothing in a program, a
//! package or a policy can set it) turns the rewrite off, so the guarded form can be run beside
//! the store form (`scripts/check-guarded-stores.py`).
use super::ir::{Expr, Program, Stmt, Value};
use super::place::Projection;
use std::collections::BTreeSet;

/// The operator's audit switch: emit every owner assignment in its guarded form.
pub(super) const AUDIT: &str = "WITCHY_AUDIT_GUARDED_STORES";
pub(super) fn audit() -> bool { std::env::var_os(AUDIT).is_some_and(|value| !value.is_empty() && value != "0") }

pub(super) fn plain(program: &mut Program) {
    for (function, proved) in program.functions.iter_mut().zip(&program.proved_unreachable) {
        if *proved && !function.overwrites.is_empty() { block(&mut function.body, &function.overwrites); }
    }
}
fn block(body: &mut Vec<Stmt>, marked: &BTreeSet<u32>) {
    let mut at = 0;
    while at < body.len() {
        if at + 2 < body.len() && assignment(&body[at..at + 3], marked) {
            let mut window = body.drain(at..at + 3);
            let (Some(Stmt::Set(_, value)), Some(Stmt::Eval(Expr { kind: Value::Exchange(place, _), .. }))) = (window.next(), window.next()) else { unreachable!("matched above") };
            drop(window);
            body.insert(at, Stmt::StorePlace(place, None, value));
        }
        match &mut body[at] {
            Stmt::If(_, yes, no) => { block(yes, marked); block(no, marked); }
            Stmt::While(_, inner) => block(inner, marked),
            Stmt::Match { arms, .. } | Stmt::MatchRef { arms, .. } => for arm in arms { block(&mut arm.body, marked); },
            _ => {}
        }
        at += 1;
    }
}
/// `held = value`, the exchange of a place reached by fields alone with the whole of `held`,
/// and the match that moves `held` away into arms that do nothing or trap.
fn assignment(window: &[Stmt], marked: &BTreeSet<u32>) -> bool {
    let [Stmt::Set(held, _), Stmt::Eval(Expr { kind: Value::Exchange(place, other), .. }), Stmt::Match { value: Expr { kind: Value::Move { place: moved, .. }, .. }, arms }] = window else { return false };
    // A store finds its place before the value is evaluated, an exchange after: fields of a
    // local or of a reference are the same place either way, an element at an index may not be.
    marked.contains(held) && place.root != *held && place.projections.iter().all(|step| matches!(step, Projection::Field { .. }))
        && other.root == *held && !other.pointer && other.projections.is_empty()
        && moved.root == *held && !moved.pointer && moved.projections.is_empty()
        && arms.iter().all(|arm| arm.body.is_empty() || matches!(arm.body.first(), Some(Stmt::Trap)))
}

#[cfg(test)]
mod tests {
    use super::*;
    const TYPES: &str = "type Slot<T>:\n    Vacant\n    Full(T)\n\ntype Either<T>:\n    Left(T)\n    Right(T)\n\ntype Shelf<T>:\n    count: U64\n    slot: Slot<T>\n    side: Either<T>\n\n";
    const REFILL: &str = "fn refill<T: Any>(shelf: &mut Shelf<T>, item: T) -> Slot<T>:\n    var held: Slot<T> = Vacant\n    memory.exchange(&mut (*shelf).slot, &mut held)\n    (*shelf).slot = Full(move item)\n    return move held\n\n";
    fn checked(body: &str) -> Result<Program, crate::diagnostic::Diagnostic> {
        let source = format!("{TYPES}{body}");
        super::super::concrete::program(super::super::check::program(&crate::compiler::parse(&source)?)?)
    }
    fn exchanges(body: &[Stmt]) -> usize {
        body.iter().map(|stmt| match stmt {
            Stmt::Eval(Expr { kind: Value::Exchange(..), .. }) => 1,
            Stmt::If(_, yes, no) => exchanges(yes) + exchanges(no),
            Stmt::While(_, inner) => exchanges(inner),
            Stmt::Match { arms, .. } | Stmt::MatchRef { arms, .. } => arms.iter().map(|arm| exchanges(&arm.body)).sum(),
            _ => 0,
        }).sum()
    }
    #[test]
    fn an_owner_assignment_is_one_store_only_where_the_function_is_proved() {
        let mut program = checked(&format!("{REFILL}fn main():\n    var shelf: Shelf<U64> = Shelf(0, Full(7), Left(1))\n    let old = refill(&mut shelf, 9)\n")).unwrap();
        let count = |program: &Program| program.functions.iter().map(|f| exchanges(&f.body)).sum::<usize>();
        // The emptying exchange and the one the assignment is written as.
        assert_eq!(count(&program), 2);
        program.proved_unreachable = vec![false; program.functions.len()];
        plain(&mut program);
        assert_eq!(count(&program), 2);
        program.proved_unreachable = vec![true; program.functions.len()];
        plain(&mut program);
        assert_eq!(count(&program), 1);
    }
    /// The assignment written out by hand, as the checker writes it: the same three statements,
    /// through a local of the author's own.
    const BY_HAND: &str = "fn refill<T: Any>(shelf: &mut Shelf<T>, item: T) -> Slot<T>:\n    var held: Slot<T> = Vacant\n    memory.exchange(&mut (*shelf).slot, &mut held)\n    var incoming: Slot<T> = Full(move item)\n    memory.exchange(&mut (*shelf).slot, &mut incoming)\n    match move incoming:\n        Vacant:\nARM        Full(lost):\n            unreachable\n    return move held\n\n";
    const MAIN: &str = "fn main():\n    var shelf: Shelf<U64> = Shelf(0, Full(7), Left(1))\n    let old = refill(&mut shelf, 9)\n";
    #[test]
    fn only_the_checkers_own_statements_become_a_store() {
        let count = |program: &Program| program.functions.iter().map(|f| exchanges(&f.body)).sum::<usize>();
        // The checker's: marked, and one store once proved.
        let mut program = checked(&format!("{REFILL}{MAIN}")).unwrap();
        assert_eq!(program.functions.iter().map(|f| f.overwrites.len()).sum::<usize>(), 1);
        // By hand, in the same shape (every arm traps), and with an arm that does something: no mark, and both exchanges stay whatever is proved.
        let trapping = "fn refill<T: Any>(shelf: &mut Shelf<T>, item: T) -> Slot<T>:\n    var incoming: Slot<T> = Full(move item)\n    memory.exchange(&mut (*shelf).slot, &mut incoming)\n    match move incoming:\n        Vacant:\n            unreachable\n        Full(lost):\n            unreachable\n\n".to_string();
        for (arm, by_hand, left) in [("every arm traps", trapping, 1), ("an arm with an effect", BY_HAND.replace("ARM", "            (*shelf).count = 77\n"), 2)] {
            let mut by_hand = checked(&format!("{by_hand}{MAIN}")).unwrap();
            assert!(by_hand.functions.iter().all(|f| f.overwrites.is_empty()));
            by_hand.proved_unreachable = vec![true; by_hand.functions.len()];
            plain(&mut by_hand);
            assert_eq!(count(&by_hand), left, "{arm}");
        }
        // A mark alone is not enough: the statements must be the checker's shape. Marking every
        // local of the hand-written function with an effect in its arm changes nothing.
        let mut effect = checked(&format!("{}{MAIN}", BY_HAND.replace("ARM", "            (*shelf).count = 77\n"))).unwrap();
        for function in &mut effect.functions { function.overwrites = (0..function.locals.len() as u32).collect(); }
        effect.proved_unreachable = vec![true; effect.functions.len()];
        plain(&mut effect);
        assert_eq!(count(&effect), 2);
        // And the checker's own without its mark is left as written.
        for function in &mut program.functions { function.overwrites.clear(); }
        program.proved_unreachable = vec![true; program.functions.len()];
        plain(&mut program);
        assert_eq!(count(&program), 2);
    }
    #[test]
    fn an_owner_assignment_is_refused_where_it_must_lose_an_owner_or_cannot_be_proved() {
        let refused = |body: &str| checked(body).expect_err(body).message.to_string();
        assert!(refused("fn put<T: Any>(shelf: &mut Shelf<T>, item: T):\n    (*shelf).side = Left(move item)\n\nfn main():\n    return\n").contains("no variant without one"));
        assert!(refused("fn put<T: Any>(shelf: &mut Shelf<T>, other: Shelf<T>):\n    *shelf = move other\n\nfn main():\n    return\n").contains("no variant without one"));
        // The same authority as the exchange it is written as: not through a shared reference.
        assert!(refused("fn put<T: Any>(shelf: &Shelf<T>, item: T):\n    (*shelf).slot = Full(move item)\n\nfn main():\n    return\n").contains("shared reference"));
        // The value moved the shelf away: the place is asked for again after it.
        assert!(refused("fn eat<T: Any>(shelf: Shelf<T>, item: T) -> T:\n    unreachable\n\nfn put<T: Any>(shelf: Shelf<T>, item: T):\n    var mine = move shelf\n    mine.slot = Full(eat(move mine, move item))\n    unreachable\n\nfn main():\n    return\n").contains("moved"));
    }
}
