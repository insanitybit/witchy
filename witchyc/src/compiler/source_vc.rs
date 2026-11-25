//! Every proof obligation of a program: record, storage and memory models, then scalar code.
use super::{logical_body::*,proof::Goal};
use crate::diagnostic::Diagnostic;
type Result<T> = std::result::Result<T,Diagnostic>;
pub(super) fn prepare(program:&Program)->Result<Vec<Goal>>{Ok(prepare_with_sites(program)?.goals)}
pub(super) fn prepare_with_sites(program:&Program)->Result<super::proof_body::Prepared>{
    // The record bodies are most of the work and spread over threads of their own; the other
    // models run beside them on one more. Each reads the program only. Results are taken in
    // the order they always were, so the goals, and the first refusal, are as before.
    let(storage,memory,scalar,record)=std::thread::scope(|scope|{
        let others=super::parallel::spawn(scope,||(super::storage_vc::prepare(program),super::memory_model::prepare_proved(program),super::scalar_vc::prepare(program)));
        // On a thread of its own too, so every model runs with the same stack (`parallel::STACK`).
        let bodies=super::parallel::spawn(scope,||super::record_model::bodies(program)).join().unwrap_or_else(|panic|std::panic::resume_unwind(panic));
        let(storage,memory,scalar)=others.join().unwrap_or_else(|panic|std::panic::resume_unwind(panic));
        (storage,memory,scalar,bodies)
    });
    let(memory,shown)=match memory{Ok((goals,shown))=>(Ok(goals),shown),Err(refusal)=>(Err(refusal),Default::default())};
    let mut prepared=super::record_model::finish(program,record,&memory)?;let mut goals=std::mem::take(&mut prepared.goals);goals.extend(storage?);goals.extend(memory?);
    let(scalar,proved)=scalar?;goals.extend(scalar);prepared.goals=goals;prepared.proved_arithmetic.extend(proved);prepared.proved_arithmetic.extend(shown);Ok(prepared)
}

#[cfg(test)]
mod tests{
    use super::*;
    const BUMP:&str="fn bump(x: U64) -> U64:\n    requires x < 18446744073709551615\n    ensures result == x + 1\n    proof:\n        auto\n    return x + 1\n";
    fn prepared(source:&str)->Result<Vec<Goal>>{
        let syntax=crate::compiler::parse(source)?;let logical=super::super::check::program(&syntax)?;
        let goals=prepare(&logical)?;super::super::concrete::program(logical)?;Ok(goals)
    }
    #[test]
    fn uncontracted_unused_bodies_still_require_memory_proofs() {
        let source = "fn read(buffer: &Buffer<U64>) -> U64:\n    return memory.get_unchecked(buffer, 0)\nfn main():\n    return\n";
        let error = prepared(source).err().unwrap();
        assert_eq!(error.code, "W0401");
        assert!(error.message.contains("unchecked memory operation requires an explicit supported contract"));
    }
    #[test]
    fn concurrent_diagnostics_keep_admission_goals_intact() {
        let source = format!("{}fn main():\n    bump(9)\n", BUMP.replace("    ensures", "    requires x != 2\n    ensures"));
        let syntax = crate::compiler::parse(&source).unwrap();
        let logical = super::super::check::program(&syntax).unwrap();
        let (admission, diagnostic) = std::thread::scope(|scope| {
            let diagnostic = scope.spawn(|| super::super::scalar_vc::diagnostic_parts(&logical));
            (prepare(&logical).unwrap(), diagnostic.join().unwrap().unwrap())
        });
        let count = |goals: &[Goal], kind| goals.iter().filter(|goal| {
            goal.source.as_ref().is_some_and(|identity| identity.kind == kind)
        }).count();
        assert_eq!(count(&admission, "function_body"), 1);
        assert_eq!(count(&admission, "call_precondition"), 1);
        assert_eq!(count(&admission, "function_body_part"), 0);
        assert_eq!(count(&admission, "call_precondition_part"), 0);
        assert!(count(&diagnostic, "function_body_part") > 0);
        assert_eq!(count(&diagnostic, "call_precondition_part"), 2);
    }
    #[test]
    fn body_return_and_each_actual_callsite_are_bound_without_running_lean(){
        let goals=prepared(&format!("{BUMP}fn main():\n    bump(9)\n    bump(18446744073709551615)\n")).unwrap();
        assert_eq!(goals.len(),3);assert_eq!(goals[0].source.as_ref().unwrap().kind,"function_body");
        assert!(goals[0].statement.contains("p0"));assert!(goals[0].statement.contains(" + (1 : Nat)"));
        assert!(goals[0].statement.contains("≤"));
        for(i,g)in goals[1..].iter().enumerate(){let id=g.source.as_ref().unwrap();assert_eq!(id.kind,"call_precondition");assert_eq!(id.function,"main");assert_eq!(id.callee.as_deref(),Some("bump"));assert_eq!(id.site,Some(i as u32));}
        assert_ne!(goals[1].statement,goals[2].statement);
        let changed=prepared(&format!("{}fn main():\n    return\n",BUMP.replace("    return x + 1","    return x + 2"))).unwrap();
        assert_ne!(goals[0].statement,changed[0].statement);assert!(changed[0].statement.contains("(2 : Nat)"));
    }
    #[test]
    fn clause_roles_cannot_capture_future_result_or_runtime_let(){
        for source in [
            "fn bad(x: U64) -> U64:\n    requires result == x\n    ensures true\n    proof:\n        auto\n    return x\nfn main():\n    return\n",
            "fn bad(result: U64) -> U64:\n    ensures result == 0\n    proof:\n        auto\n    return result\nfn main():\n    return\n",
            "fn bad(x: U64) -> U64:\n    ensures hidden == x\n    proof:\n        auto\n    let hidden = x\n    return hidden\nfn main():\n    return\n",
        ]{let e=prepared(source).err().unwrap();assert_eq!(e.code,"W0401");assert!(e.message.contains("contracts"));}
        let source="fn actual(x: U64) -> U64:\n    requires x <= 18446744073709551613\n    ensures result == x + 1\n    proof:\n        auto\n    let result = x + 2\n    return x + 1\nfn main():\n    return\n";
        let actual=prepared(source).unwrap();let changed=prepared(&source.replace("    return x + 1","    return result")).unwrap();
        assert_ne!(actual[0].statement,changed[0].statement);
        assert!(actual[0].statement.contains("(2 : Nat)")); // unused local addition still needs its own range check
    }
    #[test]
    fn modular_posts_are_available_after_each_distinct_call_only(){
        let source=format!("{BUMP}fn twice(x: U64) -> U64:\n    requires x <= 18446744073709551613\n    ensures result == x + 2\n    proof:\n        auto\n    let first = bump(x)\n    return bump(first)\nfn main():\n    return\n");
        let goals=prepared(&source).unwrap();let calls=goals.iter().filter(|g|g.source.as_ref().is_some_and(|m|m.function=="twice"&&m.kind=="call_precondition")).collect::<Vec<_>>();
        assert_eq!(calls.len(),2);assert!(!calls[0].statement.contains("v"));assert!(calls[1].statement.contains("v"));
        assert!(calls[1].statement.contains(" + (1 : Nat)"));
        let body=goals.iter().find(|g|g.source.as_ref().is_some_and(|m|m.function=="twice"&&m.kind=="function_body")).unwrap();
        assert!(body.statement.contains("(2 : Nat)"));
    }
    #[test]
    fn constant_calls_do_not_accumulate_irrelevant_modular_post_contexts(){
        let calls=(0..100).map(|_|"    bump(9)\n").collect::<String>();let goals=prepared(&format!("{BUMP}fn main():\n{calls}")).unwrap();
        assert_eq!(goals.len(),101);for(i,g)in goals[1..].iter().enumerate(){assert_eq!(g.statement,goals[1].statement);assert_eq!(g.source.as_ref().unwrap().site,Some(i as u32));}
        assert!(goals.iter().map(|g|g.statement.len()).sum::<usize>()<65536);
    }
    #[test]
    fn missing_directives_generics_and_cycles_fail_closed(){
        // A contract without a `proof:` block is discharged automatically (f9c4e93d): the
        // goals are those of the same function with `ensures true` and `auto` written out,
        // and a false claim is still a goal for the kernel, never dropped.
        let statements=|source:&str|prepared(source).unwrap().into_iter().map(|g|g.statement).collect::<Vec<_>>();
        for (implicit,explicit) in [
            ("    requires x == x\n","    requires x == x\n    ensures true\n    proof:\n        auto\n"),
            ("    ensures result == x\n","    ensures result == x\n    proof:\n        auto\n"),
            ("    ensures result == x + 1\n","    ensures result == x + 1\n    proof:\n        auto\n"),
        ]{
            let source=|clauses:&str|format!("fn bad(x: U64) -> U64:\n{clauses}    return x\nfn main():\n    return\n");
            let goals=statements(&source(implicit));assert_eq!(goals.len(),1,"{implicit}");assert_eq!(goals,statements(&source(explicit)),"{implicit}");
        }
        assert!(statements("fn bad(x: U64) -> U64:\n    ensures result == x + 1\n    return x\nfn main():\n    return\n")[0].ends_with("(p0 = (p0 + (1 : Nat)))"));
        for (source,message) in [
            ("fn bad(x: U64) -> U64:\n    ensures result == x\n    proof:\n        have true\n        auto\n    return x\nfn main():\n    return\n","an explicit function proof takes one auto step and may appear only once"),
            ("fn recurse(x: U64) -> U64:\n    ensures result == x\n    return recurse(x)\nfn main():\n    return\n","function contracts require an acyclic selected contract graph"),
            ("fn left(x: U64) -> U64:\n    ensures result == x\n    proof:\n        auto\n    return right(x)\nfn right(x: U64) -> U64:\n    ensures result == x\n    proof:\n        auto\n    return left(x)\nfn main():\n    return\n","function contracts require an acyclic selected contract graph"),
            ("fn left(x: U64) -> U64:\n    ensures result == x\n    return right(x)\nfn right(x: U64) -> U64:\n    ensures result == x\n    return left(x)\nfn main():\n    return\n","function contracts require an acyclic selected contract graph"),
        ]{let e=prepared(source).err().unwrap();assert_eq!(e.code,"W0401");assert!(e.message.starts_with(message),"{}",e.message);}
        assert!(prepared("fn id<T>(x: T) -> T:\n    return x\nfn main():\n    let n: U8 = id(1)\n").unwrap().is_empty());
    }
    #[test]
    fn calls_in_branches_and_loops_carry_the_facts_on_the_way_there(){
        let source=format!("{BUMP}fn main():\n    var value = 0\n    while value < 9:\n        if value > 3:\n            bump(value)\n        value += 1\n");
        let goals=prepared(&source).unwrap();
        let call=goals.iter().find(|g|g.source.as_ref().is_some_and(|m|m.function=="main"&&m.kind=="call_precondition")).unwrap();
        // The loop assigned `value`, so the call sees a fresh name under the loop and branch conditions.
        assert!(call.statement.contains("< (9 : Nat)")&&call.statement.contains("> (3 : Nat)"),"{}",call.statement);
        assert!(!call.statement.contains("(0 : Nat)"),"{}",call.statement);
    }
    #[test]
    fn loops_show_invariants_on_entry_and_after_each_pass_and_forget_assigned_locals(){
        let source="fn count(limit: U64) -> U64:\n    ensures result == limit\n    proof:\n        auto\n    var counter = 0\n    while counter < limit:\n        invariant counter <= limit\n        counter += 1\n    return counter\nfn main():\n    return\n";
        let goals=prepared(source).unwrap();assert_eq!(goals.len(),1);
        let body=&goals[0];
        // entry, overflow of the increment, preservation, ensures
        assert_eq!(body.prover.as_deref().map(|p|p.matches("?_").count()),Some(4),"{}",body.statement);
        assert!(body.statement.starts_with("(((0 : Nat) ≤ p0))")||body.statement.starts_with("(∀"),"{}",body.statement);
        let without=prepared(&source.replace("        invariant counter <= limit\n","")).unwrap();
        assert_eq!(without[0].prover.as_deref().map(|p|p.matches("?_").count()),Some(2));
    }
    #[test]
    fn a_local_lent_with_mut_is_unknown_after_the_call(){
        let source=format!("{BUMP}fn change(x: &mut U64):\n    *x = 18446744073709551615\nfn main():\n    var value = 1\n    change(&mut value)\n    bump(value)\n");
        let goals=prepared(&source).unwrap();
        let call=goals.iter().find(|g|g.source.as_ref().is_some_and(|m|m.function=="main")).unwrap();
        assert!(!call.statement.contains("(1 : Nat) <"),"{}",call.statement);
    }
}
