//! Optional-value provenance controls. Kernel/Empty-alpha checks belong to Root.
use super::{logical_body::*, record_model::Model, record_value::Symbol,
    record_state::State, record_caller::Caller, semantic_types::Types};
use std::collections::{BTreeMap, BTreeSet};
fn state() -> State {
    State {locals:BTreeMap::new(), foci:BTreeMap::new(), path:vec![], dependencies:BTreeSet::new()}
}
#[test]
fn stored_element_requires_positive_local_nonnull_provenance() {
    let types=Types::default();let model=Model::new(&types,None,None,0);
    let stored=Symbol::Stored("slot".into());let mut state=state();
    assert!(stored.text().is_none());
    assert!(model.element_option(&state,&stored).is_err());
    state.path.push(("slot ≠ none".into(),false));
    assert!(model.element_option(&state,&stored).is_err());
    state.path.push(("other_slot ≠ none".into(),true));
    assert!(model.element_option(&state,&stored).is_err());
    state.path.push(("slot ≠ none".into(),true));
    assert!(model.element_option(&state,&stored).is_ok());
    assert!(model.element_option(&state,&Symbol::Nat("slot".into())).is_err());
}
#[test]
fn original_and_stored_elements_share_one_option_domain() {
    let types=Types::default();let model=Model::new(&types,None,None,0);
    let mut state=state();state.path.push(("some original ≠ none".into(),true));
    let original=model.element_option(&state,&Symbol::Element("original".into())).unwrap();
    let stored=model.element_option(&state,&Symbol::Stored("some original".into())).unwrap();
    assert_eq!(original,stored);
}
#[test]
fn opaque_caller_init_payload_does_not_invent_an_entry_assumption() {
    let types=Types::default();let summaries=BTreeMap::new();
    let caller=Caller{functions:&[], sites:BTreeMap::new(), summaries:&summaries};
    let mut model=Model::new(&types,None,Some(caller),0);let mut state=state();
    let literal=Expr{ty:Ty::U64,kind:Value::U64(7)};
    let (slot,success_only)=model.payload(&state,&literal,Ty::U64).unwrap();
    assert!(state.path.is_empty());
    let value=Symbol::Stored(slot);
    assert!(model.element_option(&state,&value).is_err());
    state.path.push((success_only.unwrap(),true));
    assert!(model.element_option(&state,&value).is_ok());
}
#[test]
fn concrete_u64_returned_payload_cannot_be_reinterpreted_as_nat() {
    let library=&include_str!("../../library/wasm-vec.wy").replace("PrefixBuffer<","Buffer<").replace("try_alloc_prefix","try_alloc"); // checked cell operations are a Buffer's
    let declarations=&library[..library.find("impl<T: Any> Vec<T>:").unwrap()].replace("    invariant memory.prefix(data, length, capacity)\n","");
    let checked=include_str!("../../tests/fixtures/ready-vec.wy");
    let body=&checked[checked.find("fn vec_push_spare").unwrap()..checked.find("fn close<").unwrap()];
    let (signature,body)=body.split_once('\n').unwrap();
    let (_,body)=body.split_once("        auto\n").unwrap();
    let clauses=r#"    requires memory.prefix(input.data, input.length, input.capacity)
    ensures match result:
        Ok(out):
            memory.prefix(out.data, out.length, out.capacity)
        Err(back):
            memory.prefix(back.vector.data, back.vector.length, back.vector.capacity)
    proof:
        auto
"#;
    let caller=r#"fn repeat(input: Vec<U64>, first: U64, second: U64) -> Result<Vec<U64>, VecPushError<U64>>:
    match vec_push_spare(move input, first):
        Ok(once):
            return vec_push_spare(move once, second)
        Err(back):
            if back.item == second:
                return vec_push_spare(move back.vector, second)
            return Err(move back)
fn main():
    return
"#;
    let source=format!("{declarations}{signature}\n{clauses}{body}{caller}");
    let syntax=crate::compiler::parse(&source).unwrap();
    let logical=super::check::program(&syntax).unwrap();
    let failure=super::source_vc::prepare(&logical).err().unwrap();
    assert_eq!(failure.code,"W0401");
    assert!(failure.message.contains("opaque generic elements"));
}

fn stack_prepared(source:&str)->Result<Vec<super::proof::Goal>,crate::diagnostic::Diagnostic>{
    let syntax=crate::compiler::parse(source)?;
    let logical=super::check::program(&syntax)?;
    let goals=super::source_vc::prepare(&logical)?;
    super::concrete::program(logical)?;
    Ok(goals)
}

#[test]
fn checked_take_and_subtraction_follow_actual_operations_under_renaming(){
    let source=include_str!("../../examples/generic-proof-stack.wy");
    let original=stack_prepared(source).unwrap();
    let renamed=source.replace("Stack","Parcel").replace("stack_","parcel_")
        .replace(".slots",".storage").replace("    slots:","    storage:");
    let changed=stack_prepared(&renamed).unwrap();
    for operation in ["push","pop","clear","buffer_close","close"]{
        let body=|goals:&Vec<super::proof::Goal>,name:&str|goals.iter().find(|g|
            g.source.as_ref().is_some_and(|s|s.function==name&&s.kind=="function_body")).unwrap().statement.clone();
        assert_eq!(body(&original,&format!("stack_{operation}")),body(&changed,&format!("parcel_{operation}")));
    }
}

#[test]
fn compound_subtraction_must_show_it_cannot_underflow(){
    let original=include_str!("../../examples/generic-proof-stack.wy");
    let source=original.replace("stack.used = index\n", "stack.used -= 1\n");assert_ne!(source,original);
    let plain=stack_prepared(original).unwrap();let goals=stack_prepared(&source).unwrap();
    // The compound form adds its own no-underflow obligation to the ones already there.
    let bounds=|goals:&[super::proof::Goal]|goals.iter().map(|g|g.statement.matches("(1 : Nat) ≤ ").count()).sum::<usize>();
    assert!(bounds(&goals)>bounds(&plain));
}

#[test]
fn checked_take_does_not_skip_unsupported_index_effects(){
    let source=include_str!("../../examples/generic-proof-stack.wy");
    // The actual index mutates this same buffer. Neither the old cell snapshot
    // nor an unverified helper's return can be substituted for its effects.
    let helper="fn changed_index<T>(slots: &mut Buffer<T>, index: U64) -> U64:\n    let _ = memory.take(slots, index)\n    return index\n\n";
    let source=format!("{helper}{source}").replace("memory.take(&mut stack.slots, index)",
        "memory.take(&mut stack.slots, changed_index(&mut stack.slots, index))");
    let error=stack_prepared(&source).err().unwrap();
    assert_eq!(error.code,"W0401");
    assert!(error.message.contains("loan")||error.message.contains("borrow"),"{}",error.message);
}

#[test]
fn actual_vec_pop_reuses_proved_take_and_sub_without_name_rules() {
    let library=&include_str!("../../library/wasm-vec.wy").replace("PrefixBuffer<","Buffer<").replace("try_alloc_prefix","try_alloc"); // checked cell operations are a Buffer's
    let source=format!("{library}fn main():\n    return\n");
    let original=stack_prepared(&source).unwrap();
    let renamed=source.replace("Vec","Parcel").replace("vec_","parcel_").replace(".data",".storage").replace("    data:","    storage:").replace("prefix(data, ","prefix(storage, ");
    let renamed=stack_prepared(&renamed).unwrap();
    let body=|goals:&Vec<super::proof::Goal>,name:&str|goals.iter().find(|g|g.source.as_ref().is_some_and(|s|s.function==name&&s.kind=="function_body")).unwrap().statement.clone();
    assert_eq!(body(&original,"Vec.take_last"),body(&renamed,"Parcel.take_last"));
    // The owned take lends its vector to the in-place one and knows only what that one's
    // contract says: a different clause there is a different obligation here.
    for (from,to) in [("                vector.length + 1 == old(vector.length)\n                memory.cell(old(vector.data), vector.length, item)","                vector.length + 2 == old(vector.length)\n                memory.cell(old(vector.data), vector.length, item)"),
        ("memory.cell(old(vector.data), vector.length, item)","memory.cell(old(vector.data), 0, item)")] {
        let changed=source.replace(from,to);assert_ne!(changed,source);
        assert_ne!(body(&original,"Vec.take_last"),body(&stack_prepared(&changed).unwrap(),"Vec.take_last"));
    }
}

#[test]
fn actual_vec_pop_outcomes_keep_every_obligation_within_request_budget() {
    // The library as shipped; the older fixture built vectors by hand, which a type invariant now refuses.
    let source=format!("{}fn main():\n    return\n",include_str!("../../library/wasm-vec.wy"));
    let goals=stack_prepared(&source).unwrap();let refs=goals.iter().collect::<Vec<_>>();
    let marker=format!("WITCHY_BODY_CHECK_V2:{}:{}","0".repeat(64),goals.len());
    let module=super::proof_module::emit(super::proof_module::Namespace::Current,&refs,None).unwrap().audited(&marker).unwrap();
    if let Ok(directory)=std::env::var("WITCHY_PROOF_SIZE_DIR"){std::fs::write(std::path::Path::new(&directory).join("pop-size.json"),format!("{{\"module_bytes\":{},\"goals\":{},\"limit\":65536}}\n",module.len(),goals.len())).unwrap();}
    let requests=super::proof::request_sizes(&goals).unwrap();
    assert!(requests.iter().all(|bytes|*bytes<=65536),"a pop request exceeds 65536 bytes: {requests:?}");
}
