//! Type invariants: stated on a sealed type, assumed wherever a value arrives, shown wherever
//! the declaring file hands one on.
use super::proof::Goal;
use crate::diagnostic::Diagnostic;
const LIBRARY:&str=include_str!("../../library/wasm-vec.wy");
const INVARIANT:&str="    invariant memory.prefix(data, length, capacity)\n";
const PREMISE:&str="(cap = g1) → (g0 ≤ g1) → (∀ j : Nat, j < g0 → cells j ≠ none) → (∀ j : Nat, g0 ≤ j → cells j = none) → ";
const KEEP:&str="fn keep<T>(input: Vec<T>) -> Vec<T>:\n    ensures result.length == old(input.length)\n    proof:\n        auto\n    return move input\n\nfn main():\n    return\n";
/// `client` is checked as a second file after `library`.
fn prepared(library:&str,client:&str)->Result<Vec<Goal>,Diagnostic>{
    let library=&crate::compiler::exported(library);let source=format!("{library}import 0\n{client}");let syntax=crate::compiler::parse_modules(&source,&[0,library.len()])?;
    let logical=super::check::program(&syntax)?;let goals=super::source_vc::prepare(&logical)?;super::concrete::program(logical)?;Ok(goals)
}
fn body<'a>(goals:&'a [Goal],name:&str)->&'a str{&goals.iter().find(|g|g.source.as_ref().is_some_and(|s|s.function==name&&s.kind=="function_body")).unwrap_or_else(||panic!("no body goal for {name}")).statement}
#[test]
fn only_a_sealed_type_states_an_invariant_and_only_over_its_own_fields(){
    let open=LIBRARY.replace("sealed type Vec<T>","type Vec<T>");
    assert!(prepared(&open,"fn main():\n    return\n").err().unwrap().message.contains("only a sealed type can state an invariant"));
    let stray=LIBRARY.replace(INVARIANT,"    invariant memory.prefix(data, size, capacity)\n");
    let message=prepared(&stray,"fn main():\n    return\n").err().unwrap().message;
    assert!(message.contains("names only the type's own fields"),"{message}");
}
#[test]
fn the_declaring_file_touches_the_fields_only_under_a_proof(){
    for unproved in ["fn forge<T>(data: Buffer<T>) -> Vec<T>:\n    return Vec(move data, 7, 7)\n\n",
        "fn stretch<T>(vector: &mut Vec<T>):\n    (*vector).length = (*vector).length + 1\n    return\n\n",
        "fn open<T>(input: Vec<T>) -> Buffer<T>:\n    return move input.data\n\n"]{
        let library=format!("{LIBRARY}{unproved}");
        let explicit=library.replace(unproved,&unproved.replacen(":\n",":\n    ensures true\n",1));
        let name=unproved.strip_prefix("fn ").unwrap().split('<').next().unwrap();
        let check=|s:&str|prepared(s,"fn main():\n    return\n").map(|gs|body(&gs,name).to_owned()).map_err(|e|e.message);
        assert_eq!(check(&library),check(&explicit),"{name}");
    }
    // Reading a field needs nothing.
    prepared(&format!("{LIBRARY}fn peek<T>(vector: &Vec<T>) -> U64:\n    return (*vector).length\n\n"),"fn main():\n    return\n").unwrap();
}
#[test]
fn a_value_that_arrives_satisfies_the_invariant_in_both_proof_models(){
    let goals=prepared(LIBRARY,"fn main():\n    return\n").unwrap();
    // Owned, in the record model; borrowed, in the memory model.
    assert!(body(&goals,"Vec.push_spare").contains(PREMISE));
    let borrowed=body(&goals,"Vec.push_spare_mut");
    for part in ["(g1 = cap)","(g0 ≤ g1)","(∀ j : Nat, ((0 : Nat) ≤ j ∧ j < g0) → cells j ≠ none) →","(∀ j : Nat, g0 ≤ j → cells j = none) →"]{assert!(borrowed.contains(part),"{part}: {borrowed}");}
    let without=LIBRARY.replace(INVARIANT,"");let plain=prepared(&without,"fn main():\n    return\n").unwrap();
    assert!(!body(&plain,"Vec.push_spare").contains(PREMISE));assert_ne!(body(&plain,"Vec.push_spare_mut"),borrowed);
}
#[test]
fn the_declaring_file_shows_the_invariant_on_what_it_hands_back(){
    let goals=prepared(LIBRARY,"fn main():\n    return\n").unwrap();
    // The final state of the exclusive borrow and the returned vector are each checked: a
    // body that forgets the length changes what must be proved.
    let stale=LIBRARY.replace("            (*vector).length = index + 1\n","");assert_ne!(stale,LIBRARY);
    assert_ne!(body(&prepared(&stale,"fn main():\n    return\n").unwrap(),"Vec.push_spare_mut"),body(&goals,"Vec.push_spare_mut"));
    let stale=LIBRARY.replace("            vector.length = index + 1\n","");assert_ne!(stale,LIBRARY);
    assert_ne!(body(&prepared(&stale,"fn main():\n    return\n").unwrap(),"Vec.push_spare"),body(&goals,"Vec.push_spare"));
    // The leaves are there at all only because of the invariant.
    let leaf="j = none) ∧ True)";assert!(body(&goals,"Vec.push_spare_mut").contains(leaf),"{}",body(&goals,"Vec.push_spare_mut"));
    assert!(!body(&prepared(&LIBRARY.replace(INVARIANT,""),"fn main():\n    return\n").unwrap(),"Vec.push_spare_mut").contains(leaf));
}
#[test]
fn another_file_relies_on_the_invariant_and_owes_nothing(){
    // The same function, written in the declaring file and in a client.
    let inside=prepared(&format!("{LIBRARY}{KEEP}"),"").unwrap();let outside=prepared(LIBRARY,KEEP).unwrap();
    let(inside,outside)=(body(&inside,"keep"),body(&outside,"keep"));
    assert!(inside.contains(PREMISE)&&outside.contains(PREMISE));
    // Inside, the returned vector's invariant is a leaf to show; outside it cannot have been broken.
    let leaf="(∀ j : Nat, g0 ≤ j → cells j = none) ∧";assert!(inside.contains(leaf),"{inside}");assert!(!outside.contains(leaf),"{outside}");
}
#[test]
fn one_equality_stated_both_ways_is_kept_once(){
    // A contract clause that repeats part of the invariant the other way round used to
    // send the rewriting step into a loop.
    let doubled=LIBRARY.replace("                output.length == old(input.length) + 1\n                output.capacity == old(input.capacity)\n","                output.length == old(input.length) + 1\n                output.capacity == old(input.capacity)\n                output.capacity == memory.capacity(output.data)\n");assert_ne!(doubled,LIBRARY);
    let goals=prepared(&doubled,"fn main():\n    return\n").unwrap();let push=body(&goals,"Vec.push");
    assert!(push.contains("(True) → "),"{push}");
}
#[test]
fn lending_a_sealed_value_to_a_borrow_proved_helper_shows_its_invariant_inside_its_file(){
    // `vec_get` assumes the invariant of what it borrows. A caller in the declaring file
    // shows it at the call, in the helper's vocabulary; a caller elsewhere owes nothing.
    let lend="fn first<T>(input: Vec<T>) -> Vec<T>:\n    ensures result.length == old(input.length)\n    proof:\n        auto\n    match Vec.get(&input, 0):\n        Ok(item):\n            return move input\n        Err(code):\n            return move input\n\n";
    let inside=prepared(&format!("{LIBRARY}{lend}"),"fn main():\n    return\n").unwrap();
    let outside=prepared(LIBRARY,&format!("{lend}fn main():\n    return\n")).unwrap();
    let leaf="(∀ j : Nat, ((0 : Nat) ≤ j ∧ j < g0) → cells j ≠ none) ∧";
    assert!(body(&inside,"first").contains(leaf),"{}",body(&inside,"first"));assert!(!body(&outside,"first").contains(leaf));
}
#[test]
fn an_unchecked_take_needs_a_proof_and_its_bound_and_occupancy_are_obligations(){
    // Without written clauses the operation still generates the actual body obligation.
    let bare=format!("{LIBRARY}fn grab<T: Any>(buffer: &mut Buffer<T>) -> T:\n    return memory.take_unchecked(buffer, 0)\n\n");
    let implicit=prepared(&bare,"fn main():\n    return\n").unwrap();
    let claim=body(&implicit,"grab");
    assert!(claim.contains("< cap")&&claim.contains("≠ none"),"{claim}");
    // The taken cell must be shown in bounds and holding a value.
    let goals=prepared(LIBRARY,"fn main():\n    return\n").unwrap();
    let borrowed=body(&goals,"Vec.take_last_mut");assert!(borrowed.contains("< cap")&&borrowed.contains("≠ none"),"{borrowed}");
    // Taking a different cell is a different obligation.
    for(name,from,to)in [("Vec.take_last_mut","let item = memory.take_unchecked(&mut (*vector).data, index)","let item = memory.take_unchecked(&mut (*vector).data, length)")]{
        let changed=LIBRARY.replace(from,to);assert_ne!(changed,LIBRARY);
        assert_ne!(body(&prepared(&changed,"fn main():\n    return\n").unwrap(),name),body(&goals,name));
    }
    // It moves the value out, so the element may own; nothing is copied.
    assert!(LIBRARY.contains("impl<T: Any> Vec<T>:")&&LIBRARY.contains("    pub fn take_last(input: Vec<T>)"));
}
#[test]
fn a_proved_body_shows_what_its_proved_callees_require(){
    let goals=prepared(LIBRARY,"fn main():\n    return\n").unwrap();let reserve=body(&goals,"Vec.reserve");
    // Neither call is refused outright any more; each requirement is a leaf of the caller's proof.
    assert!(!reserve.contains("(False)"),"{reserve}");
    for requirement in ["    requires memory.empty_range(target, 0, count)\n","    requires memory.initialized_range(source, 0, count)\n","    requires memory.empty_all(input)\n"]{
        let dropped=LIBRARY.replace(requirement,"");assert_ne!(dropped,LIBRARY);
        assert_ne!(body(&prepared(&dropped,"fn main():\n    return\n").unwrap(),"Vec.reserve"),reserve,"{requirement}");
    }
    // Moving between two buffers is proved over both, each held exclusively.
    let moved=goals.iter().find(|g|g.source.as_ref().is_some_and(|s|s.function=="memory_move_prefix"&&s.kind=="function_body")).unwrap();
    assert!(moved.source.as_ref().unwrap().memory);assert!(moved.statement.contains("cells1"));
    // A callee that calls itself is still never assumed: the unreachable arm must be shown unreachable.
    assert!(body(&goals,"prefix_release_empty").contains("→ False)"),"{}",body(&goals,"prefix_release_empty"));
}
#[test]
fn an_invariant_outside_the_vocabulary_is_refused_once_at_its_clause_saying_what_it_is(){
    // Resolved in each of the two functions that receive a `Gauge`; refused at the clause, once.
    let source="sealed type Gauge:\n    level: U64\n    room: U64\n    invariant level.max(room) <= 32\n\nimpl Gauge:\n    fn bound(g: &Gauge) -> U64:\n        ensures result <= 32\n        proof:\n            auto\n        return (*g).room\n\n    fn other(g: &Gauge) -> U64:\n        ensures true\n        proof:\n            auto\n        return (*g).level\n\nfn main():\n    return\n";
    let syntax=crate::compiler::parse(source).unwrap();
    let refusal=super::check::program(&syntax).err().expect("refused");
    assert!(source[refusal.span.clone()].contains("level.max(room)"),"{:?} {}",refusal.span,refusal.message);
    assert!(refusal.message.contains("a method call with arguments"),"{}",refusal.message);
    assert!(super::check::later_refusals().iter().all(|later|later.span!=refusal.span||later.message!=refusal.message));
}
