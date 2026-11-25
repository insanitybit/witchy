//! Source VC controls; kernel/runtime qualification is performed by the host owner.
use super::proof::Goal;
use crate::diagnostic::Diagnostic;
const GET:&str="fn memory_get_unchecked<T>(buffer: &Buffer<T>, index: U64) -> T:\n    requires index < memory.capacity(buffer)\n    requires memory.initialized(buffer, index)\n    ensures memory.cell(buffer, index, result)\n    proof:\n        auto\n    return memory.get_unchecked(buffer, index)\n";
const VEC:&str="type Shelf<T>:\n    storage: Buffer<T>\n    length: U64\n    claimed: U64\n\nfn safe_get<T>(value: &Shelf<T>, index: U64) -> Result<T, U64>:\n    ensures true\n    proof:\n        auto\n    let capacity = memory.capacity(&(*value).storage)\n    if index < (*value).length:\n        if index < capacity:\n            if memory.initialized(&(*value).storage, index):\n                return Ok(memory_get_unchecked(&(*value).storage, index))\n    return Err(1)\n";
fn prepared(source:&str)->Result<Vec<Goal>,Diagnostic>{let syntax=crate::compiler::parse(source)?;let logical=super::check::program(&syntax)?;let goals=super::memory_model::prepare(&logical)?;super::concrete::program(logical)?;Ok(goals)}
#[test]
fn generic_unsafe_body_and_actual_safe_caller_have_distinct_nontrivial_vcs(){
    let goals=prepared(&format!("{GET}{VEC}fn main():\n    return\n")).unwrap();assert_eq!(goals.len(),3);
    assert!(goals.iter().all(|g|g.source.as_ref().unwrap().memory));let body=goals.iter().find(|g|g.source.as_ref().unwrap().function=="memory_get_unchecked").unwrap();assert!(body.statement.contains("Nat → Option α"));assert!(body.statement.contains("≠ none"));assert!(!body.statement.contains("Inhabited"));
    let call=goals.iter().find(|g|g.source.as_ref().unwrap().kind=="call_precondition").unwrap();assert_ne!(call.statement,"True");assert_eq!(call.source.as_ref().unwrap().callee.as_deref(),Some("memory_get_unchecked"));assert_eq!(call.source.as_ref().unwrap().site,Some(0));
}
#[test]
fn missing_bounds_init_and_false_posts_remain_real_changed_propositions(){
    let source=format!("{GET}{VEC}fn main():\n    return\n");let ordinary=prepared(&source).unwrap();
    for changed in [source.replace("    requires memory.initialized(buffer, index)\n",""),source.replace("return memory.get_unchecked(buffer, index)","return memory.get_unchecked(buffer, memory.capacity(buffer))"),source.replacen("ensures memory.cell(buffer, index, result)","ensures false",1)]{let altered=prepared(&changed).unwrap();assert_ne!(ordinary[0].statement,altered[0].statement);}
    let changed=source.replace("            if memory.initialized(&(*value).storage, index):\n                return", "            return");let altered=prepared(&changed).unwrap();let original=ordinary.iter().find(|g|g.source.as_ref().unwrap().kind=="call_precondition").unwrap();let call=altered.iter().find(|g|g.source.as_ref().unwrap().kind=="call_precondition").unwrap();assert_ne!(original.statement,call.statement);
}
#[test]
fn implicit_read_contracts_preserve_actual_memory_obligations(){
    for helper in ["fn bad<T>(buffer: &Buffer<T>, index: U64) -> T:\n    return memory.get_unchecked(buffer, index)\n","fn bad<T>(buffer: &Buffer<T>, index: U64) -> T:\n    if false:\n        return memory.get_unchecked(buffer, index)\n    return memory_get_unchecked(buffer, index)\n"]{let source=format!("{GET}{helper}fn main():\n    return\n");
        let explicit=source.replace(helper,&helper.replacen(":\n",":\n    ensures true\n",1));
        let proof=|s:&str|prepared(s).map(|gs|gs.into_iter().filter(|g|g.source.as_ref().is_some_and(|s|s.function=="bad")).map(|g|g.statement).collect::<Vec<_>>()).map_err(|e|e.message);
        let actual=proof(&source);
        if let Ok(goals)=&actual {assert!(goals.iter().any(|g|g.contains("< cap") && g.contains("≠ none")));}
        assert_eq!(actual,proof(&explicit));
    }
}
#[test]
fn read_only_proof_loops_mutations_and_recursive_summaries_fail_closed(){
    for changed in [GET.replace("    return memory.get_unchecked", "    while false:\n        memory.capacity(buffer)\n    return memory.get_unchecked"),GET.replace("return memory.get_unchecked(buffer, index)","return memory_get_unchecked(buffer, index)")]{let error=prepared(&format!("{changed}fn main():\n    return\n")).err().unwrap();assert_eq!(error.code,"W0401");}
}

const INIT:&str="fn initialize_unchecked<T>(buffer: &mut Buffer<T>, index: U64, item: T):\n    requires index < memory.capacity(buffer)\n    requires memory.empty(buffer, index)\n    ensures memory.cell(buffer, index, item)\n    ensures memory.same_allocation(buffer, old(buffer))\n    ensures memory.capacity(buffer) == memory.capacity(old(buffer))\n    ensures memory.unchanged_except(buffer, old(buffer), index)\n    proof:\n        auto\n    memory.init_unchecked(buffer, index, item)\n";
const PUSH:&str="fn push_in_place<T>(value: &mut Shelf<T>, item: T) -> Result<(), T>:\n    ensures true\n    proof:\n        auto\n    let index = (*value).length\n    let actual = memory.capacity(&(*value).storage)\n    if (*value).claimed == actual:\n        if index < actual:\n            if !memory.initialized(&(*value).storage, index):\n                initialize_unchecked(&mut (*value).storage, index, item)\n                (*value).length = index + 1\n                return Ok(())\n    return Err(item)\n";
fn mutable_source()->String{format!("{GET}{INIT}{VEC}{PUSH}fn main():\n    return\n")}
#[test]
fn generic_initialization_has_actual_update_snapshot_footprint_and_safe_caller(){
    let goals=prepared(&mutable_source()).unwrap();let body=goals.iter().find(|g|g.source.as_ref().unwrap().function=="initialize_unchecked").unwrap();assert!(body.statement.contains("fun j : Nat => if j ="));assert!(body.statement.contains("key = key"));assert!(!body.statement.contains("frame = frame"));assert!(body.statement.contains("j ≠"));assert!(!body.statement.contains("Inhabited"));
    let call=goals.iter().find(|g|g.source.as_ref().unwrap().function=="push_in_place"&&g.source.as_ref().unwrap().kind=="call_precondition").unwrap();assert_ne!(call.statement,"True");assert_eq!(call.source.as_ref().unwrap().callee.as_deref(),Some("initialize_unchecked"));
}
#[test]
fn unchecked_write_empty_index_payload_and_post_changes_affect_real_body_vcs(){
    let original=prepared(&mutable_source()).unwrap();let original=original.iter().find(|g|g.source.as_ref().unwrap().function=="initialize_unchecked").unwrap();
    for changed in [INIT.replace("    requires index < memory.capacity(buffer)\n",""),INIT.replace("    requires memory.empty(buffer, index)\n",""),INIT.replace("memory.init_unchecked(buffer, index, item)","let write_index = memory.capacity(buffer)\n    memory.init_unchecked(buffer, write_index, item)"),INIT.replace("index: U64, item: T)","index: U64, item: T, other: T)").replace("memory.init_unchecked(buffer, index, item)","memory.init_unchecked(buffer, index, other)"),INIT.replace("ensures memory.cell(buffer, index, item)","ensures false")]{
        let goals=prepared(&format!("{changed}fn main():\n    return\n")).unwrap();assert_ne!(original.statement,goals[0].statement);
    }
}
#[test]
fn old_requires_refuses_and_implicit_initialization_needs_a_proof(){
    let invalid=INIT.replace("requires memory.empty(buffer, index)","requires memory.empty(old(buffer), index)");assert_eq!(prepared(&format!("{invalid}fn main():\n    return\n")).err().unwrap().code,"W0401");
    for body in ["    memory.init_unchecked(buffer, index, item)\n","    if false:\n        memory.init_unchecked(buffer, index, item)\n"]{let source=format!("fn missing<T>(buffer: &mut Buffer<T>, index: U64, item: T):\n{body}fn main():\n    return\n");let implicit=prepared(&source).unwrap();
        let explicit=source.replacen(":\n",":\n    ensures true\n",1);
        assert_eq!(implicit[0].statement,prepared(&explicit).unwrap()[0].statement);
        assert!(implicit[0].statement.contains("cells"));
    }
}
#[test]
fn mutable_callee_true_post_does_not_retain_old_empty_or_invent_write_result(){
    let weak=INIT.replace("    ensures memory.cell(buffer, index, item)\n    ensures memory.same_allocation(buffer, old(buffer))\n    ensures memory.capacity(buffer) == memory.capacity(old(buffer))\n    ensures memory.unchanged_except(buffer, old(buffer), index)", "    ensures true");
    let caller="fn again<T>(buffer: &mut Buffer<T>, index: U64, item: T):\n    requires index < memory.capacity(buffer)\n    requires memory.empty(buffer, index)\n    ensures true\n    proof:\n        auto\n    initialize_unchecked(buffer, index, item)\n    memory.init_unchecked(buffer, index, item)\n";
    let goals=prepared(&format!("{weak}{caller}fn main():\n    return\n")).unwrap();let body=goals.iter().find(|g|g.source.as_ref().unwrap().function=="again"&&g.source.as_ref().unwrap().kind=="function_body").unwrap();assert!(body.statement.contains("Nat → Option α"));assert!(body.statement.contains("= none"));let after=body.statement.split(" : Nat → Option α)").nth(1).unwrap().rsplit('(').next().unwrap();assert!(body.statement.contains(&format!("{after} ")));
}
#[test]
fn mutable_record_header_facts_require_a_callee_postcondition() {
    let source = r#"type Shelf<T>:
    storage: Buffer<T>
    length: U64

fn reset<T>(value: &mut Shelf<T>, item: T):
    ensures (*value).length == old((*value).length)
    proof:
        auto
    return

fn unchanged<T>(value: &mut Shelf<T>, item: T):
    ensures (*value).length == old((*value).length)
    proof:
        auto
    reset(value, item)

fn main():
    return
"#;
    let mutating = source.replacen("ensures (*value).length == old((*value).length)", "ensures true", 1)
        .replace("    return\n\nfn unchanged", "    (*value).length = 0\n\nfn unchanged");
    let obligation = |text| {
        prepared(text).unwrap().into_iter()
            .find(|goal| goal.source.as_ref().is_some_and(|source|
                source.function == "unchanged" && source.kind == "function_body"))
            .unwrap().statement
    };
    let preserved = obligation(source);
    let unknown = obligation(&mutating);
    // g0 is the incoming header and g3 is the header after the exclusive call.
    assert!(preserved.contains("((g3 = g0)) → (g3 = g0)"), "{preserved}");
    assert!(unknown.contains("(g3 = g0)"), "{unknown}");
    assert!(!unknown.contains("((g3 = g0)) →"), "{unknown}");
}
#[test]
fn mutable_header_old_snapshot_stays_distinct_from_current_store(){
    let bump="fn bump<T>(value: &mut Shelf<T>):\n    requires (*value).length < 18446744073709551615\n    ensures (*value).length == old((*value).length) + 1\n    proof:\n        auto\n    (*value).length = (*value).length + 1\n";
    let goals=prepared(&format!("{GET}{VEC}{bump}fn main():\n    return\n")).unwrap();let body=goals.iter().find(|g|g.source.as_ref().unwrap().function=="bump").unwrap();assert!(body.statement.contains(" + (1 : Nat)"));assert!(body.statement.contains("let v0 : Nat"));
}
#[test]
fn initialized_read_after_mutable_call_depends_on_declared_current_cell_post(){
    let client="fn init_then_read<T>(buffer: &mut Buffer<T>, index: U64, item: T) -> Result<T, U64>:\n    ensures true\n    proof:\n        auto\n    if index < memory.capacity(buffer):\n        if !memory.initialized(buffer, index):\n            initialize_unchecked(buffer, index, item)\n            return Ok(memory_get_unchecked(buffer, index))\n    return Err(1)\n";
    let strong=prepared(&format!("{GET}{INIT}{client}fn main():\n    return\n")).unwrap();let weak=INIT.replace("    ensures memory.cell(buffer, index, item)\n","");let weak=prepared(&format!("{GET}{weak}{client}fn main():\n    return\n")).unwrap();
    let find=|goals:Vec<Goal>|goals.into_iter().find(|g|{let s=g.source.as_ref().unwrap();s.function=="init_then_read"&&s.callee.as_deref()==Some("memory_get_unchecked")}).unwrap();let strong=find(strong);let weak=find(weak);assert_ne!(strong.statement,weak.statement);assert_ne!(weak.statement,"True");
}
#[test]
fn shared_permission_cannot_authorize_unchecked_initialization(){
    let shared=INIT.replace("buffer: &mut Buffer<T>","buffer: &Buffer<T>");assert_eq!(prepared(&format!("{shared}fn main():\n    return\n")).err().unwrap().code,"W0401");
}

const COPY_LOOP:&str=r#"fn transfer<T>(source: &Buffer<T>, target: &mut Buffer<T>, count: U64) -> Result<(), U64>:
    ensures memory.unchanged(source, old(source))
    ensures memory.same_allocation(target, old(target))
    ensures memory.capacity(target) == memory.capacity(old(target))
    ensures memory.unchanged_from(target, old(target), count)
    ensures match result:
        Ok:
            memory.same_prefix(target, source, count)
        Err(code):
            true
    proof:
        auto
    if count > memory.capacity(source):
        return Err(1)
    if count > memory.capacity(target):
        return Err(2)
    var index: U64 = 0
    while index < count:
        invariant index <= count
        invariant memory.same_prefix(target, source, index)
        invariant memory.unchanged_from(target, old(target), index)
        if memory.initialized(source, index):
            if !memory.initialized(target, index):
                let item = memory.get_unchecked(source, index)
                memory.init_unchecked(target, index, item)
                index += 1
            else:
                return Err(3)
        else:
            return Err(4)
    return Ok(())
"#;
#[test]
fn copy_loop_has_two_real_foci_and_inductive_current_state_obligations(){
    let goals=prepared(&format!("{COPY_LOOP}fn main():\n    return\n")).unwrap();assert_eq!(goals.len(),1);let g=&goals[0];
    assert!(g.statement.contains("key1 : Key"));assert!(g.statement.contains("cap1 : Nat"));assert!(g.statement.contains("cells1 : Nat → Option α"));assert!(g.statement.contains("fun j : Nat => if j ="));assert!(g.statement.contains("∀ j : Nat, j <"));assert!(!g.statement.contains("Inhabited"));
    // A ghost cell map represents an arbitrary mutable loop head, not one unrolled iteration.
    assert!(g.statement.contains("(g2 : Nat → Option α)"));
    for changed in [COPY_LOOP.replace("invariant index <= count","invariant index == 1"),COPY_LOOP.replace("index += 1","index += 2"),COPY_LOOP.replace("if !memory.initialized(target, index):","if true:"),COPY_LOOP.replace("return Err(4)","return Ok(())"),COPY_LOOP.replace("memory.init_unchecked(target, index, item)","memory.init_unchecked(target, count, item)")]{
        let altered=prepared(&format!("{changed}fn main():\n    return\n")).unwrap();assert_ne!(g.statement,altered[0].statement);
    }
}
#[test]
fn loop_invariants_require_proof_and_supported_mutation_guard_and_snapshot_context(){
    let unproved="fn bad<T>(source: &Buffer<T>, count: U64):\n    while false:\n        invariant count == count\n        memory.capacity(source)\nfn main():\n    return\n";
    assert!(prepared(unproved).err().unwrap().message.contains("loop invariants require"));
    for changed in [COPY_LOOP.replace("    var index: U64 = 0","    var flag = true\n    var index: U64 = 0"),COPY_LOOP.replace("invariant index <= count","invariant index == old(index)"),COPY_LOOP.replace("        if memory.initialized(source, index):","        while false:\n            invariant true\n            memory.capacity(source)\n        if memory.initialized(source, index):")]{
        assert_eq!(prepared(&format!("{changed}fn main():\n    return\n")).err().unwrap().code,"W0401");
    }
    let guard="fn change(index: &mut U64) -> Bool:\n    *index += 1\n    return true\n";
    let changed=COPY_LOOP.replace("while index < count:","while change(&mut index):");let error=prepared(&format!("{guard}{changed}fn main():\n    return\n")).err().unwrap();assert_eq!(error.code,"W0401");assert!(error.message.contains("pure guard"));
}
#[test]
fn multi_focus_identity_is_not_fabricated_and_the_removed_frame_clause_refuses(){
    let crossed=COPY_LOOP.replace("memory.same_allocation(target, old(target))","memory.same_allocation(target, old(source))");let goals=prepared(&format!("{crossed}fn main():\n    return\n")).unwrap();assert!(goals[0].statement.contains("key1 = key"));
    let framed=COPY_LOOP.replace("    ensures memory.unchanged(source, old(source))",concat!("    ensures memory.unchanged(source, old(source))\n    ensures memory.frame_","unchanged(old(source))"));let error=prepared(&format!("{framed}fn main():\n    return\n")).err().unwrap();assert!(error.message.contains("was removed: it stated nothing"),"{}",error.message);
}

const CLEAR_LOOP:&str=r#"fn clear_cells<T>(buffer: &mut Buffer<T>):
    ensures memory.empty_all(buffer)
    ensures memory.same_allocation(buffer, old(buffer))
    ensures memory.capacity(buffer) == memory.capacity(old(buffer))
    proof:
        auto
    let capacity = memory.capacity(buffer)
    var index: U64 = 0
    while index < capacity:
        invariant index <= capacity
        invariant memory.empty_range(buffer, 0, index)
        invariant memory.unchanged_from(buffer, old(buffer), index)
        let _ = memory.take(buffer, index)
        index += 1
    return
"#;
const EMPTY_TAIL:&str=r#"fn check_tail<T>(buffer: &Buffer<T>, start: U64) -> Result<(), U64>:
    ensures memory.unchanged(buffer, old(buffer))
    ensures match result:
        Ok:
            memory.empty_from(buffer, start)
        Err(code):
            true
    proof:
        auto
    let capacity = memory.capacity(buffer)
    if start > capacity:
        return Ok(())
    var index = start
    while index < capacity:
        invariant start <= index
        invariant index <= capacity
        invariant memory.empty_range(buffer, start, index)
        if memory.initialized(buffer, index):
            return Err(1)
        index += 1
    return Ok(())
"#;
#[test]
fn discarded_take_clear_loop_checks_actual_updates_ranges_and_final_empty_state(){
    let source=format!("{CLEAR_LOOP}fn main():\n    return\n");let goals=prepared(&source).unwrap();assert_eq!(goals.len(),1);let g=&goals[0];
    assert!(g.statement.contains("then none else"));assert!(g.statement.contains("Nat → Option α"));assert!(g.statement.contains("∀ j : Nat,"));assert!(g.statement.contains("= none"));
    for changed in [source.replace("        let _ = memory.take(buffer, index)\n",""),source.replace("memory.take(buffer, index)","memory.take(buffer, capacity)"),source.replace("memory.empty_range(buffer, 0, index)","memory.empty_range(buffer, 0, index + 1)")]{
        let altered=prepared(&changed).unwrap();assert_ne!(g.statement,altered[0].statement);
    }
    let named=source.replace("clear_cells","drain_storage").replace("buffer","storage");assert!(prepared(&named).is_ok());
}
#[test]
fn readonly_tail_result_domain_and_error_branches_are_actual_and_checked_init_uses_record_proof(){
    let source=format!("{EMPTY_TAIL}fn main():\n    return\n");let g=prepared(&source).unwrap();assert_eq!(g.len(),1);assert!(g[0].source.as_ref().unwrap().memory);assert!(g[0].statement.contains("∀ j : Nat,"));
    let altered=prepared(&source.replace("return Err(1)","return Ok(())")).unwrap();assert_ne!(g[0].statement,altered[0].statement);
    let checked="fn checked<T>(buffer: &mut Buffer<T>, index: U64, item: T) -> Result<(), U64>:\n    ensures true\n    proof:\n        auto\n    return memory.init(buffer, index, item)\nfn main():\n    return\n";
    let syntax = crate::compiler::parse(checked).unwrap();
    let logical = super::check::program(&syntax).unwrap();
    let goals = super::source_vc::prepare(&logical).unwrap();
    assert_eq!(goals.len(), 1);
    assert!(goals[0].source.as_ref().unwrap().record);
    assert!(goals[0].statement.contains("then some g1 else cells j"));
}
#[test]
fn take_result_and_shared_mutation_contexts_refuse_and_callee_true_does_not_invent_empty(){
    let bound=CLEAR_LOOP.replace("        let _ = memory.take(buffer, index)","        let removed = memory.take(buffer, index)");let error=prepared(&format!("{bound}fn main():\n    return\n")).err().unwrap();assert_eq!(error.code,"W0401");assert!(error.message.contains("unsupported expression"));
    let shared=CLEAR_LOOP.replace("buffer: &mut Buffer<T>","buffer: &Buffer<T>");assert_eq!(prepared(&format!("{shared}fn main():\n    return\n")).err().unwrap().code,"W0401");
    let caller="fn finish<T>(buffer: &mut Buffer<T>):\n    ensures memory.empty_all(buffer)\n    proof:\n        auto\n    clear_cells(buffer)\n    return\n";
    let strong=prepared(&format!("{CLEAR_LOOP}{caller}fn main():\n    return\n")).unwrap();let weak=CLEAR_LOOP.replace("ensures memory.empty_all(buffer)","ensures true");let weak=prepared(&format!("{weak}{caller}fn main():\n    return\n")).unwrap();
    let body=|v:Vec<Goal>|v.into_iter().find(|g|g.source.as_ref().is_some_and(|s|s.function=="finish"&&s.kind=="function_body")).unwrap().statement;
    assert_ne!(body(strong),body(weak));
}
#[test]
fn a_forall_body_speaks_of_single_cells_so_no_inner_quantifier_captures_its_variable(){
    // `initialized_range` is itself a quantified statement over cells; inside a forall its
    // own variable would capture the outer one.
    let nested="fn bad<T>(buffer: &Buffer<T>, count: U64) -> Bool:\n    requires count <= memory.capacity(buffer)\n    requires forall j in (0, count): memory.initialized_range(buffer, 0, j)\n    ensures true\n    proof:\n        auto\n    return true\nfn main():\n    return\n";
    assert!(prepared(nested).err().unwrap().message.contains("a forall body speaks of single cells"));
    let pointwise=nested.replace("memory.initialized_range(buffer, 0, j)","memory.initialized(buffer, j) == (j < count)");
    assert!(prepared(&pointwise).is_ok());
}
