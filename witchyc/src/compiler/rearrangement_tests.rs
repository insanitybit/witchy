//! A helper that only moves values around: its summary, what a caller's proof learns from it,
//! and which lends of a field the checker takes as a write made by the caller.
use super::rearrangement::{summary, Piece, Summary};
use super::proof::Goal;
use crate::diagnostic::Diagnostic;
const TYPES:&str="type Slot<T>:\n    Vacant\n    Full(T)\n\n";
const DRAIN:&str="fn drain<T: Any>(slot: &mut Slot<T>) -> Slot<T>:\n    var previous: Slot<T> = Vacant\n    memory.exchange(&mut *slot, &mut previous)\n    return move previous\n\n";
const REPLACE:&str="fn replace<T: Any>(slot: &mut T, next: T) -> T:\n    var held = move next\n    memory.exchange(slot, &mut held)\n    return move held\n\n";
fn logical(rest:&str)->Result<super::logical_body::Program,Diagnostic>{
    let source=format!("{TYPES}{rest}\nfn main():\n    return\n");super::check::program(&crate::compiler::parse(&source)?)
}
fn of(rest:&str,name:&str)->Option<Summary>{
    let program=logical(rest).unwrap_or_else(|e|panic!("{}",e.message));
    summary(&program.functions,program.functions.iter().position(|f|f.name==name).unwrap_or_else(||panic!("no function {name}")))
}
fn goals(rest:&str)->Result<Vec<Goal>,Diagnostic>{
    let logical=logical(rest)?;let goals=super::source_vc::prepare(&logical)?;super::concrete::program(logical)?;Ok(goals)
}
fn body(goals:&[Goal],name:&str)->String{goals.iter().find(|g|g.source.as_ref().is_some_and(|s|s.function==name&&s.kind=="function_body")).unwrap_or_else(||panic!("no body goal for {name}")).statement.clone()}
fn given(index:u32)->Piece{Piece::Given(index)}
#[test]
fn a_body_of_moves_and_one_exchange_has_a_summary(){
    // `take`: the place holds the variant built from nothing, the result is what was there.
    assert_eq!(of(DRAIN,"drain"),Some(Summary{after:vec![Some(Piece::Variant(0,Box::new(Piece::Unit)))],result:given(0)}));
    // `replace`, with the reference passed on as it is or reborrowed.
    let replace=Some(Summary{after:vec![Some(given(1)),None],result:given(0)});
    assert_eq!(of(REPLACE,"replace"),replace);
    assert_eq!(of(&REPLACE.replace("memory.exchange(slot,","memory.exchange(&mut *slot,"),"replace"),replace);
    // A variant built around the argument; two places exchanged with each other.
    assert_eq!(of("fn stuff<T: Any>(slot: &mut Slot<T>, item: T) -> Slot<T>:\n    var next: Slot<T> = Full(move item)\n    memory.exchange(&mut *slot, &mut next)\n    return move next\n","stuff"),
        Some(Summary{after:vec![Some(Piece::Variant(1,Box::new(given(1)))),None],result:given(0)}));
    assert_eq!(of("fn both<T: Any>(a: &mut T, b: &mut T):\n    memory.exchange(a, b)\n    return\n","both"),
        Some(Summary{after:vec![Some(given(1)),Some(given(0))],result:Piece::Unit}));
}
#[test]
fn anything_else_has_no_summary(){
    for (name,source) in [
        // A branch: the place may or may not have been emptied.
        ("f","fn f<T: Any>(slot: &mut Slot<T>, really: Bool) -> Slot<T>:\n    var previous: Slot<T> = Vacant\n    if really:\n        memory.exchange(&mut *slot, &mut previous)\n    return move previous\n".to_string()),
        // A call of a function that has none (a branch in it), and a call inside a value.
        ("f",format!("{}fn f<T: Any>(slot: &mut Slot<T>) -> Slot<T>:\n    return drain(&mut *slot)\n",DRAIN.replace("    memory.exchange(&mut *slot, &mut previous)\n","    if true:\n        memory.exchange(&mut *slot, &mut previous)\n"))),
        ("f",format!("{DRAIN}fn f<T: Any>(slot: &mut Slot<T>) -> Slot<Slot<T>>:\n    return Full(drain(&mut *slot))\n")),
        // A field of what the reference points at, an assignment through it, arithmetic, a loop.
        ("f","type Pair<T>:\n    left: Slot<T>\n    right: Slot<T>\n\nfn f<T: Any>(pair: &mut Pair<T>) -> Slot<T>:\n    var previous: Slot<T> = Vacant\n    memory.exchange(&mut (*pair).left, &mut previous)\n    return move previous\n".to_string()),
        ("f","fn f(slot: &mut U64, next: U64) -> U64:\n    let old = *slot\n    *slot = next\n    return old\n".to_string()),
        ("f","fn f(slot: &mut U64, next: U64) -> U64:\n    var held = next + 1\n    memory.exchange(slot, &mut held)\n    return held\n".to_string()),
        ("f","fn f(slot: &mut U64, next: U64) -> U64:\n    var held = next\n    while false:\n        held = 0\n    memory.exchange(slot, &mut held)\n    return held\n".to_string()),
        // A match on the value: which arm ran is not known to a summary.
        ("f","fn f<T: Any>(slot: &mut Slot<T>) -> Slot<T>:\n    var previous: Slot<T> = Vacant\n    memory.exchange(&mut *slot, &mut previous)\n    match move previous:\n        Vacant:\n            return Vacant\n        Full(item):\n            return Full(move item)\n".to_string()),
    ]{
        assert_eq!(of(&source,name),None,"{source}");
    }
}
/// A caller that returns what a helper handed back, with a claim only the summary can show.
const CALLER:&str="fn first() -> U64:\n    ensures result == 7\n    proof:\n        auto\n    var place: U64 = 7\n    let old = replace(&mut place, 9)\n    return old\n\nfn second() -> U64:\n    ensures result == 9\n    proof:\n        auto\n    var place: U64 = 7\n    let old = replace(&mut place, 9)\n    return place\n\n";
#[test]
fn a_caller_knows_where_each_value_went(){
    let known=goals(&format!("{REPLACE}{CALLER}")).unwrap();
    let (first,second)=(body(&known,"first"),body(&known,"second"));
    assert!(first.contains("((7 : Nat) = (7 : Nat))"),"{first}");
    assert!(second.contains("((9 : Nat) = (9 : Nat))"),"{second}");
    // The same helper with a branch in it: the caller knows nothing of either value.
    let opaque=REPLACE.replace("    memory.exchange(slot, &mut held)\n","    if true:\n        memory.exchange(slot, &mut held)\n");assert_ne!(opaque,REPLACE);
    let unknown=goals(&format!("{opaque}{CALLER}")).unwrap();
    let (first,second)=(body(&unknown,"first"),body(&unknown,"second"));
    assert!(!first.contains("((7 : Nat) = (7 : Nat))"),"{first}");
    assert!(!second.contains("((9 : Nat) = (9 : Nat))"),"{second}");
}
#[test]
fn a_later_lend_forgets_what_the_helper_left(){
    // After `replace` the place is known to hold 9; lent again to a body that is not followed,
    // it is unknown.
    let stale=CALLER.replace("    return place\n","    let again = touch(&mut place)\n    return place\n");assert_ne!(stale,CALLER);
    let source=format!("{REPLACE}fn touch(slot: &mut U64) -> U64:\n    if true:\n        *slot = 1\n    return 0\n\n{stale}");
    let second=body(&goals(&source).unwrap(),"second");
    assert!(!second.contains("((9 : Nat) = (9 : Nat))"),"{second}");
}
#[test]
fn a_variant_the_helper_built_is_known_and_one_it_did_not_build_is_not(){
    // `unreachable` in the `Full` arm is a goal of False under what is known of the slot.
    let caller="fn emptied(slot: &mut Slot<U64>) -> U64:\n    ensures true\n    proof:\n        auto\n    let old = drain(&mut *slot)\n    match &*slot:\n        Vacant:\n            return 0\n        Full(_):\n            unreachable\n\n";
    let drained=body(&goals(&format!("{DRAIN}{caller}")).unwrap(),"emptied");
    // The slot's variant is the literal 0 where the arm is entered: `0 = 1` is the contradiction.
    assert!(drained.contains("((0 : Nat) = (1 : Nat))"),"{drained}");
    let stuffed=DRAIN.replace("var previous: Slot<T> = Vacant","var previous: Slot<T> = Full(move item)").replace("(slot: &mut Slot<T>)","(slot: &mut Slot<T>, item: T)");
    let full=body(&goals(&format!("{stuffed}{}",caller.replace("drain(&mut *slot)","drain(&mut *slot, 5)"))).unwrap(),"emptied");
    assert!(full.contains("((1 : Nat) = (1 : Nat))")&&!full.contains("((0 : Nat) = (1 : Nat))"),"{full}");
}
/// A sealed type whose invariant reads storage through a method (`cell.pointer()`), as the
/// pointer-backed list's does, in one file with the library it uses.
fn holder(rest:&str)->Result<(),Diagnostic>{
    let cell=include_str!("../../library/wasm-cell.wy").replace("import \"wasm-link.wy\"\n","");
    // The option library alone, with the trait its file imports declared in place.
    let option=include_str!("../../library/wasm-option.wy").replace("import \"wasm-discard-core.wy\"\n","pub trait Discard:\n    fn discard(self: Self)\n");
    assert!(option.contains("pub trait Discard:"));
    let source=format!("{}{cell}{option}sealed type Holder<T>:\n    mark: Weak<T>\n    held: Option<OccupiedCell<T>>\n    invariant when Some(cell) = held:\n        same_target(mark, cell.pointer())\n    invariant when None = held:\n        !has_address(mark)\n\n{rest}\nfn main():\n    return\n",
        include_str!("../../library/wasm-link.wy"));
    let logical=super::check::program(&crate::compiler::parse(&source)?)?;super::source_vc::prepare(&logical)?;super::concrete::program(logical)?;Ok(())
}
const VIEW:&str="to change storage named by a shared view invariant";
#[test]
fn a_field_under_a_view_invariant_is_lent_only_to_a_call_that_cannot_keep_the_reference(){
    let pull=|body:&str|format!("impl<T: Any> Holder<T>:\n    fn pull(holder: &mut Holder<T>) -> Option<OccupiedCell<T>>:\n{body}        (*holder).mark = memory.null_weak()\n        return move cell\n");
    // The library's `take`: its result and parameters hold no reference.
    holder(&pull("        let cell = (*holder).held.take()\n")).unwrap();
    holder(&pull("        let cell = Option.take(&mut (*holder).held)\n")).unwrap();
    // Kept in a local; handed back by the helper, as it is or inside a value.
    for (helper,body) in [
        ("","        let slot = &mut (*holder).held\n        let cell = slot.take()\n"),
        ("fn inside<T: Any>(slot: &mut Option<T>) -> &mut Option<T>:\n    return slot\n\n","        let cell = inside(&mut (*holder).held).take()\n"),
        ("fn keep<T: Any>(slot: &mut Option<T>) -> Option<&mut Option<T>>:\n    return Some(slot)\n\n","        let kept = keep(&mut (*holder).held)\n        let cell = (*holder).held.take()\n"),
    ]{
        let message=holder(&format!("{helper}{}",pull(body))).expect_err(body).message;
        assert!(message.contains(VIEW),"{body}: {message}");
    }
    // Lent whole while the field is out: the rule for a direct write applies.
    let message=holder(&format!("impl<T: Any> Holder<T>:\n    fn count(holder: &Holder<T>) -> U64:\n        return 0\n\n{}",&pull("        let cell = (*holder).held.take()\n        let seen = holder.count()\n")["impl<T: Any> Holder<T>:\n".len()..])).expect_err("lent whole").message;
    assert!(message.contains("fields are written here"),"{message}");
}
#[test]
fn only_the_types_decide_whether_a_borrow_ends_with_the_call(){
    // A function that could hand the reference on is refused whatever its body does, and
    // whatever it is called.
    let message=holder("fn take<T: Any>(slot: &mut Option<T>) -> &mut Option<T>:\n    return slot\n\nimpl<T: Any> Holder<T>:\n    fn pull(holder: &mut Holder<T>) -> U64:\n        let slot = take(&mut (*holder).held)\n        return 0\n").expect_err("named take").message;
    assert!(message.contains(VIEW),"{message}");
}
/// Everything kept between builds is found by what was to be proved: the proof cache and the
/// helper's installed body by the statement (proof_cache.rs `key`, proof_body.rs `plan_current`),
/// a session's analysis and image by the whole source (compiler_session.rs). A summary is safe
/// there only because it is nowhere but in the caller's statement, so a helper whose body
/// changes must change that statement, with the caller's own text the same. With the checker,
/// in one session: tests/wasm_cli.rs, `a_session_never_reuses_a_helper_summary_after_the_helper_*` (one test per changed helper).
#[test]
fn a_changed_helper_body_changes_what_its_caller_must_prove(){
    let caller="type Shelf<T>:\n    slot: Slot<T>\n\nnopanic fn swap_in<T: Any>(shelf: &mut Shelf<T>, item: T) -> Slot<T>:\n    let held = drain(&mut (*shelf).slot)\n    (*shelf).slot = Full(move item)\n    return move held\n\n";
    let exchange="    memory.exchange(&mut *slot, &mut previous)\n";
    // Exchanged twice, the slot holds what it held; with a branch there is no summary at all.
    let twice=DRAIN.replace(exchange,&format!("{exchange}{exchange}"));
    let branch=DRAIN.replace(exchange,&format!("    if true:\n    {exchange}"));
    assert!(twice!=DRAIN&&branch!=DRAIN);
    assert_eq!(of(&twice,"drain"),Some(Summary{after:vec![Some(given(0))],result:Piece::Variant(0,Box::new(Piece::Unit))}));
    assert_eq!(of(&branch,"drain"),None);
    let statements=|helper:&str|goals(&format!("{helper}{caller}")).unwrap().into_iter()
        .filter(|g|g.source.as_ref().is_some_and(|s|s.function=="swap_in")).map(|g|g.statement).collect::<Vec<_>>();
    let emptied=statements(DRAIN);
    assert!(!emptied.is_empty());
    assert_eq!(emptied,statements(DRAIN),"the same source gives the same statements: they can be found again");
    for changed in [&twice,&branch]{
        let now=statements(changed);
        assert!(!now.is_empty()&&now!=emptied,"{changed}");
        // No statement that mentions the slot's state is shared: none can be answered from before.
        let body=|all:&[String]|all.iter().filter(|s|s.contains("(0 : Nat)")).cloned().collect::<Vec<_>>();
        assert!(body(&now).iter().all(|s|!body(&emptied).contains(s)),"{changed}");
    }
}
/// A call matched where it is made is the call bound with `let` and that local matched: the
/// variant the helper built is known in the arms, and with no summary neither form knows it.
#[test]
fn a_match_made_on_the_call_knows_what_the_let_form_knows(){
    let stuffed=DRAIN.replace("var previous: Slot<T> = Vacant","var previous: Slot<T> = Full(move item)").replace("(slot: &mut Slot<T>)","(slot: &mut Slot<T>, item: T)").replace("return move previous","var wrapped: Slot<Slot<T>> = Full(move previous)\n    return move wrapped").replace("-> Slot<T>","-> Slot<Slot<T>>");
    let head="fn old(slot: &mut Slot<U64>) -> U64:\n    ensures true\n    proof:\n        auto\n";
    let arms="        Vacant:\n            unreachable\n        Full(inner):\n            return 0\n\n";
    let bound=format!("{head}    let made = drain(&mut *slot, 5)\n    match made:\n{arms}");
    let direct=format!("{head}    match drain(&mut *slot, 5):\n{arms}");
    let known=|caller:&str,helper:&str|body(&goals(&format!("{helper}{caller}")).unwrap(),"old");
    // The result is `Full(..)`, variant 1: the `Vacant` arm is entered under `1 = 0`.
    for caller in [&bound,&direct]{
        let statement=known(caller,&stuffed);
        assert!(statement.contains("((1 : Nat) = (0 : Nat))"),"{caller}: {statement}");
    }
    // A helper with a branch has no summary: neither form says which variant came back.
    let opaque=stuffed.replace("    memory.exchange(&mut *slot, &mut previous)\n","    if true:\n        memory.exchange(&mut *slot, &mut previous)\n");assert_ne!(opaque,stuffed);
    for caller in [&bound,&direct]{
        let statement=known(caller,&opaque);
        assert!(!statement.contains("((1 : Nat) = (0 : Nat))"),"{caller}: {statement}");
    }
}
/// `memory.vacant(value)` is the match an author writes by hand, with `unreachable` for each
/// variant that holds an owner: the function's goals are the same statements, so nothing is
/// proved by a rule the hand-written form did not already use.
#[test]
fn ending_a_vacant_value_asks_what_the_match_written_by_hand_asks(){
    let head="type Shelf<T>:\n    count: U64\n    slot: Slot<T>\n\nfn unpack<T: Any>(shelf: Shelf<T>) -> Slot<T>:\n    ensures true\n    proof:\n        auto\n    var whole = move shelf\n    var held: Slot<T> = Vacant\n    memory.exchange(&mut whole.slot, &mut held)\n    let Shelf(count, slot) = move whole\n";
    let by_hand=format!("{head}    match move slot:\n        Vacant:\n            ()\n        Full(owner):\n            unreachable\n    return move held\n");
    let vacant=format!("{head}    memory.vacant(move slot)\n    return move held\n");
    let statements=|source:&str|goals(source).unwrap().into_iter().filter(|g|g.source.as_ref().is_some_and(|s|s.function=="unpack")).map(|g|g.statement).collect::<Vec<_>>();
    let asked=statements(&vacant);
    assert!(!asked.is_empty()&&asked.iter().any(|s|s.contains("False")),"{asked:?}");
    assert_eq!(asked,statements(&by_hand));
    // With nothing known of the slot the goal is there all the same, and is not `True`.
    let unknown=statements(&vacant.replace("    memory.exchange(&mut whole.slot, &mut held)\n",""));
    assert!(unknown!=asked&&unknown.iter().any(|s|s.contains("False")),"{unknown:?}");
    // A type with no variant that holds no owner, and a function that could not be proved
    // (`main`), are refused by the checker before any proof.
    let refused=|source:&str|logical(source).expect_err(source).message;
    assert!(refused("type Either<T>:\n    Left(T)\n    Right(T)\n\nfn end<T: Any>(either: Either<T>):\n    memory.vacant(move either)\n").contains("its type has no variant without one"));
    assert!(refused("fn end<T: Any>(slot: &Slot<T>):\n    memory.vacant(slot)\n").contains("not a reference to it"));
}
/// A helper that calls a helper: the callee's summary is composed, each value it was given
/// being the piece this body held there.
#[test]
fn a_call_of_a_summarised_helper_is_composed(){
    let drained=Some(Summary{after:vec![Some(Piece::Variant(0,Box::new(Piece::Unit)))],result:given(0)});
    // Returned at once, bound first, and through the reference handed on as it is.
    assert_eq!(of(&format!("{DRAIN}fn f<T: Any>(slot: &mut Slot<T>) -> Slot<T>:\n    return drain(&mut *slot)\n"),"f"),drained);
    assert_eq!(of(&format!("{DRAIN}fn f<T: Any>(slot: &mut Slot<T>) -> Slot<T>:\n    let first = drain(&mut *slot)\n    return move first\n"),"f"),drained);
    assert_eq!(of(&format!("{DRAIN}fn f<T: Any>(slot: &mut Slot<T>) -> Slot<T>:\n    return drain(slot)\n"),"f"),drained);
    // `stuff` by `replace`: a variant built here around the argument goes where `replace` puts
    // what it is given, and its result is what was there.
    let stuff=format!("{REPLACE}fn stuff<T: Any>(slot: &mut Slot<T>, item: T) -> Slot<T>:\n    return replace(&mut *slot, Full(move item))\n");
    assert_eq!(of(&stuff,"stuff"),Some(Summary{after:vec![Some(Piece::Variant(1,Box::new(given(1)))),None],result:given(0)}));
    // Two calls in a row: drained, then filled; the result is what was there first.
    let refill=format!("{DRAIN}{stuff}fn refill(slot: &mut Slot<U64>, item: U64) -> Slot<U64>:\n    let old = drain(&mut *slot)\n    let empty = stuff(&mut *slot, item)\n    return old\n");
    assert_eq!(of(&refill,"refill"),Some(Summary{after:vec![Some(Piece::Variant(1,Box::new(given(1)))),None],result:given(0)}));
    // A local of the caller lent to the callee, and a call made for what it does to two places.
    let both="fn both<T: Any>(a: &mut T, b: &mut T):\n    memory.exchange(a, b)\n    return\n\n";
    assert_eq!(of(&format!("{both}fn rotate<T: Any>(a: &mut T, b: &mut T, c: T) -> T:\n    var held = move c\n    both(&mut *a, &mut held)\n    both(&mut *b, &mut held)\n    return move held\n"),"rotate"),
        Some(Summary{after:vec![Some(given(2)),Some(given(0)),None],result:given(1)}));
    // Three bodies deep.
    let chain=format!("{DRAIN}fn one<T: Any>(slot: &mut Slot<T>) -> Slot<T>:\n    return drain(slot)\n\nfn two<T: Any>(slot: &mut Slot<T>) -> Slot<T>:\n    return one(slot)\n\nfn three<T: Any>(slot: &mut Slot<T>) -> Slot<T>:\n    return two(slot)\n\nfn four<T: Any>(slot: &mut Slot<T>) -> Slot<T>:\n    return three(slot)\n");
    assert_eq!(of(&chain,"two"),drained);
    assert_eq!(of(&chain,"three"),drained);
    // Deeper than that is not followed.
    assert_eq!(of(&chain,"four"),None);
}
#[test]
fn a_call_that_is_not_a_rearrangement_of_whole_places_is_not_composed(){
    for (name,source) in [
        // The function calls itself, directly and through another: no fixed rearrangement is read.
        ("f","fn f<T: Any>(slot: &mut Slot<T>) -> Slot<T>:\n    return f(slot)\n".to_string()),
        ("f","fn f<T: Any>(slot: &mut Slot<T>) -> Slot<T>:\n    return g(slot)\n\nfn g<T: Any>(slot: &mut Slot<T>) -> Slot<T>:\n    return f(slot)\n".to_string()),
        // A caller of a function that calls itself.
        ("h","fn f<T: Any>(slot: &mut Slot<T>) -> Slot<T>:\n    return f(slot)\n\nfn h<T: Any>(slot: &mut Slot<T>) -> Slot<T>:\n    return f(slot)\n".to_string()),
        // A field of what the reference points at lent to the helper: places are whole.
        ("f",format!("{DRAIN}type Pair<T>:\n    left: Slot<T>\n    right: Slot<T>\n\nfn f<T: Any>(pair: &mut Pair<T>) -> Slot<T>:\n    return drain(&mut (*pair).left)\n")),
        // A shared reference among the arguments: the callee reads through it.
        ("f","fn peek<T: Any>(slot: &mut Slot<T>, other: &Slot<T>) -> Slot<T>:\n    var previous: Slot<T> = Vacant\n    memory.exchange(&mut *slot, &mut previous)\n    return move previous\n\nfn f<T: Any>(slot: &mut Slot<T>, other: &Slot<T>) -> Slot<T>:\n    return peek(slot, other)\n".to_string()),
        // A result that is dropped where it is made, when it is not `()`.
        ("f","fn h(slot: &mut U64, next: U64) -> U64:\n    var held = next\n    memory.exchange(slot, &mut held)\n    return held\n\nfn f(slot: &mut U64, next: U64) -> U64:\n    h(slot, next)\n    return next\n".to_string()),
        // An argument that is computed.
        ("f","fn h(slot: &mut U64, next: U64) -> U64:\n    var held = next\n    memory.exchange(slot, &mut held)\n    return held\n\nfn f(slot: &mut U64, next: U64) -> U64:\n    return h(slot, next + 1)\n".to_string()),
    ]{
        assert_eq!(of(&source,name),None,"{source}");
    }
}
/// What a caller's proof learns through two helpers is what it learns through one: the slot's
/// variant after the call, and nothing when the inner helper is not a rearrangement.
#[test]
fn a_caller_knows_what_a_helper_of_a_helper_left(){
    let outer="fn empty<T: Any>(slot: &mut Slot<T>) -> Slot<T>:\n    return drain(&mut *slot)\n\n";
    let caller="fn emptied(slot: &mut Slot<U64>) -> U64:\n    ensures true\n    proof:\n        auto\n    let old = empty(&mut *slot)\n    match &*slot:\n        Vacant:\n            return 0\n        Full(_):\n            unreachable\n\n";
    let through=body(&goals(&format!("{DRAIN}{outer}{caller}")).unwrap(),"emptied");
    assert!(through.contains("((0 : Nat) = (1 : Nat))"),"{through}");
    // The same statement as when the caller calls the inner helper itself.
    assert_eq!(through,body(&goals(&format!("{DRAIN}{outer}{}",caller.replace("empty(&mut *slot)","drain(&mut *slot)"))).unwrap(),"emptied"));
    // The inner helper changed (a branch): the outer one has no summary, and the caller's
    // statement changes with it, its own text and the outer helper's the same.
    let opaque=DRAIN.replace("    memory.exchange(&mut *slot, &mut previous)\n","    if true:\n        memory.exchange(&mut *slot, &mut previous)\n");assert_ne!(opaque,DRAIN);
    let unknown=body(&goals(&format!("{opaque}{outer}{caller}")).unwrap(),"emptied");
    assert!(!unknown.contains("((0 : Nat) = (1 : Nat))")&&unknown!=through,"{unknown}");
}
/// A kept `&mut` to one field: the proof forgets that field from the lend on and keeps what it
/// knew of the others (scalar_vc `lend_under`). The whole value reborrowed forgets every field.
#[test]
fn a_kept_reference_to_a_field_forgets_that_field_and_no_other(){
    let pair="type Pair:\n    a: U64\n    b: U64\n\n";
    let f=|lend:&str,read:&str|format!("{pair}fn f(p: &mut Pair) -> U64:\n    ensures result == 5\n    (*p).a = 5\n    (*p).b = 5\n    let r = {lend}\n    *r = 6\n    return (*p).{read}\n");
    let known="((5 : Nat) = (5 : Nat))";
    // The sibling is still 5; the lent field is not, and nor is anything once the whole is lent.
    assert!(body(&goals(&f("&mut (*p).a","b")).unwrap(),"f").contains(known));
    assert!(!body(&goals(&f("&mut (*p).a","a")).unwrap(),"f").contains(known));
    let whole=format!("{pair}fn f(p: &mut Pair) -> U64:\n    ensures result == 5\n    (*p).a = 5\n    (*p).b = 5\n    let r = &mut *p\n    (*r).a = 6\n    return (*p).b\n");
    assert!(!body(&goals(&whole).unwrap(),"f").contains(known));
    // A part of a lent record field is under the lent path; a field beside that record is not.
    let nested=|read:&str|format!("{pair}type Outer:\n    inner: Pair\n    n: U64\n\nfn f(o: &mut Outer) -> U64:\n    ensures result == 5\n    (*o).inner.b = 5\n    (*o).n = 5\n    let r = &mut (*o).inner\n    (*r).a = 6\n    return (*o).{read}\n");
    assert!(body(&goals(&nested("n")).unwrap(),"f").contains(known));
    assert!(!body(&goals(&nested("inner.b")).unwrap(),"f").contains(known));
    // An element lent at an index: every element of that array is unknown, and the field beside it is not.
    let grid=|read:&str|format!("type Grid:\n    cells: [U64; 4]\n    n: U64\n\nfn f(g: &mut Grid, i: U64) -> U64:\n    requires i < 4\n    ensures result == 5\n    (*g).cells[1] = 5\n    (*g).n = 5\n    let r = &mut (*g).cells[i]\n    *r = 6\n    return (*g).{read}\n");
    assert!(body(&goals(&grid("n")).unwrap(),"f").contains(known));
    assert!(!body(&goals(&grid("cells[1]")).unwrap(),"f").contains(known));
}
/// A sealed type with an invariant lends, in its own file and under a proof over numbers, only a
/// field that no invariant reads (check.rs `lasting`, `invariant_free_field`).
#[test]
fn a_sealed_type_keeps_a_reference_only_to_a_field_no_invariant_reads(){
    let bag=|invariants:&str,field:&str|format!("type Held:\n    Nothing\n    Some1(U64)\n\nsealed type Bag:\n    spare: U64\n    held: Held\n    count: U64\n    limit: U64\n{invariants}\nfn f(bag: &mut Bag) -> Bool:\n    ensures bag.count == old(bag.count)\n    let r = &mut (*bag).{field}\n    return true\n");
    let refused="cannot be kept or returned outside its file, or at all if the type has an invariant";
    let plain="    invariant count <= limit\n";
    logical(&bag(plain,"spare")).unwrap();
    logical(&bag(plain,"held")).unwrap();
    for (invariants,field) in [
        // Read as a value, on either side; as the subject of a case; given to a function.
        (plain,"count"),(plain,"limit"),
        ("    invariant when Nothing = held:\n        count == 0\n","held"),
        ("    invariant variant_index(held) == 0 || count >= 1\n","held"),
        ("    invariant count <= limit\n    invariant spare + 0 <= 9\n","spare"),
    ]{
        let message=logical(&bag(invariants,field)).expect_err(field).message;
        assert!(message.contains(refused),"{field}: {message}");
    }
    // The case's other fields are still free.
    logical(&bag("    invariant when Nothing = held:\n        count == 0\n","spare")).unwrap();
}
/// A `requires` that reads a part of an argument built in the call is refused at the call.
#[test]
fn a_requires_on_an_argument_built_in_the_call_is_refused_at_the_call(){
    let rest="type Job:\n    id: U64\n\ntype Lease:\n    id: U64\n\nfn fill(lease: &mut Lease, job: Job):\n    requires job.id >= 1\n    (*lease).id = job.id\n\nfn use_it(lease: &mut Lease):\n    fill(&mut *lease, Job(id: 0))\n";
    let Err(refused)=goals(rest) else{panic!("built in the call")};
    let source=format!("{TYPES}{rest}\nfn main():\n    return\n");
    assert!(refused.message.contains("does not pass by name"),"{}",refused.message);
    assert_eq!(&source[refused.span.clone()],"fill(&mut *lease, Job(id: 0))");
}
