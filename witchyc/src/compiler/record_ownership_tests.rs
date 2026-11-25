//! Source controls for allocation ownership; actual kernel/runtime checks belong to Root.
use super::proof::Goal;
use crate::diagnostic::Diagnostic;
fn prepared(text:&str)->Result<Vec<Goal>,Diagnostic>{let syntax=crate::compiler::parse(text)?;let logical=super::check::program(&syntax)?;let goals=super::source_vc::prepare(&logical)?;super::concrete::program(logical)?;Ok(goals)}
fn actual()->String{format!("{}{}fn main():\n    return\n",include_str!("../../library/wasm-vec.wy"),include_str!("../../library/wasm-buffer.wy"))}
fn reserve(goals:&[Goal])->&Goal{goals.iter().find(|g|g.source.as_ref().is_some_and(|s|s.function=="Vec.reserve"&&s.kind=="function_body")).expect("actual reserve body")}
#[test]
fn actual_allocation_and_cleanup_bodies_have_owned_proofs_without_name_rules(){
    let source=actual();let goals=prepared(&source).unwrap();
    for name in ["vec_buffer_close","Vec.close","Vec.reserve"]{assert!(goals.iter().any(|g|g.source.as_ref().is_some_and(|s|s.function==name&&s.kind=="function_body"&&s.record)));}
    let changed=source.replace("Vec","Parcel").replace("vec_","parcel_").replace(".length",".used").replace("    length:","    used:").replace("prefix(data, length, ","prefix(data, used, ");
    let renamed=prepared(&changed).unwrap();let body=renamed.iter().find(|g|g.source.as_ref().is_some_and(|s|s.function=="Parcel.reserve"&&s.kind=="function_body")).unwrap();assert_eq!(reserve(&goals).statement,body.statement);
}
#[test]
fn allocation_failure_cannot_be_replaced_with_success_and_move_posts_are_not_invented(){
    let source=actual();let original=prepared(&source).unwrap();
    let start=source.find("    pub fn reserve(input: Vec<T>").unwrap();let end=source.find("    pub fn push(input: Vec<T>").unwrap();let raw=&source[start..end];
    let wrong=raw.replace("            Err(allocation_error):\n                return Err(move input)","            Err(allocation_error):\n                return Ok(move input)");assert_ne!(raw,wrong);
    let changed=format!("{}{}{}",&source[..start],wrong,&source[end..]);let altered=prepared(&changed).unwrap();assert_ne!(reserve(&original).statement,reserve(&altered).statement);
    let weak=source.replace("    ensures memory.same_prefix(target, old(source), count)\n","    ensures true\n");assert_ne!(weak,source);let weakened=prepared(&weak).unwrap();assert_ne!(reserve(&original).statement,reserve(&weakened).statement);
}
#[test]
fn recursive_ownership_posts_are_never_assumed(){
    let one="fn spin<T>(input: Buffer<T>):\n    ensures memory.released(old(input))\n    proof:\n        auto\n    spin(move input)\nfn main():\n    return\n";
    let goals=prepared(one).unwrap();let body=goals.iter().find(|g|g.source.as_ref().unwrap().kind=="function_body").unwrap();assert!(body.statement.contains("(False) ∧"));
    let two="fn first<T>(input: Buffer<T>):\n    ensures memory.released(old(input))\n    proof:\n        auto\n    second(move input)\nfn second<T>(input: Buffer<T>):\n    ensures memory.released(old(input))\n    proof:\n        auto\n    first(move input)\nfn main():\n    return\n";
    let failure=prepared(two).err().unwrap();assert_eq!(failure.code,"W0401");assert!(failure.message.contains("post import cycle"));
}

#[test]
fn actual_reserve_proof_request_fits_bounded_checker(){
    // The library as shipped; the older fixture built vectors by hand, which a type invariant now refuses.
    let source=actual();
    let goals=prepared(&source).unwrap();let refs=goals.iter().collect::<Vec<_>>();
    let marker=format!("WITCHY_BODY_CHECK_V2:{}:{}","0".repeat(64),goals.len());
    let module=super::proof_module::emit(super::proof_module::Namespace::Current,&refs,None).unwrap().audited(&marker).unwrap();
    let sizes=goals.iter().map(|g|format!("{{\"name\":{:?},\"kind\":{:?},\"statement\":{},\"prover\":{}}}",g.source_name,g.source.as_ref().map_or("authored",|s|s.kind),g.statement.len(),g.prover.as_ref().map_or(0,|s|s.len()))).collect::<Vec<_>>();
    let report=format!("{{\"module_bytes\":{},\"limit\":65536,\"goals\":[{}]}}\n",module.len(),sizes.join(","));
    if let Ok(directory)=std::env::var("WITCHY_PROOF_SIZE_DIR"){
        let directory=std::path::Path::new(&directory);std::fs::create_dir_all(directory).unwrap();
        std::fs::write(directory.join("sizes.json"),&report).unwrap();
        if module.len()<=1_048_576{std::fs::write(directory.join("module-shape-probe.lean"),&module).unwrap();}
    }
    let requests=super::proof::request_sizes(&goals).unwrap();
    assert!(requests.iter().all(|bytes|*bytes<=65536),"a reserve proof request exceeds 65536 bytes: {requests:?}");
}

#[test]
fn constructors_have_demonic_allocation_and_no_legacy_owned_post() {
    let source=actual();let syntax=crate::compiler::parse(&source).unwrap();let logical=super::check::program(&syntax).unwrap();
    let f=logical.functions.iter().find(|f|f.name=="Vec.new").unwrap();
    let c=f.contract.as_ref().unwrap().record.as_ref().unwrap();
    let(goal,sealed)=super::record_model::body(f,c,&logical.types).unwrap();
    assert!(sealed.arms.is_empty());
    assert!(goal.statement.contains("(none : Option α)"));
    assert!(goal.statement.contains("¬ g"),"allocation failure is independently selected");
    let renamed=source.replace("Vec","Parcel").replace("vec_","parcel_");
    let goals=prepared(&renamed).unwrap();
    let renamed=goals.iter().find(|g|g.source.as_ref().is_some_and(|s|s.function=="Parcel.new"&&s.kind=="function_body")).unwrap();
    assert_eq!(goal.statement,renamed.statement);
}

#[test]
fn independently_named_buffer_constructor_uses_typed_mixed_result() {
    let source="fn make_storage<T>(allocator: &Allocator, size: U64) -> Result<Buffer<T>, U64>:\n    ensures match result:\n        Ok(storage):\n            memory.prefix(storage, 0, size)\n        Err(code):\n            true\n    proof:\n        auto\n    return memory.try_alloc(allocator, size)\nfn main():\n    return\n";
    let goals=prepared(source).unwrap();
    assert!(goals.iter().any(|g|g.source.as_ref().is_some_and(|s|s.function=="make_storage"&&s.record)));
    let wrong=source.replace("memory.prefix(storage, 0, size)","memory.prefix(storage, 1, size)");
    assert_ne!(goals[0].statement,prepared(&wrong).unwrap()[0].statement);
}

#[test]
fn consuming_mixed_result_remains_outside_constructor_eligibility() {
    // Outside the record model's shapes, a contract is a claim about numbers and is proved as one.
    let source="fn relay<T>(input: Buffer<T>) -> Result<Buffer<T>, U64>:\n    ensures true\n    proof:\n        auto\n    return Ok(move input)\nfn main():\n    return\n";
    let goals=prepared(source).unwrap();
    assert!(!goals.is_empty()&&goals.iter().all(|g|g.source.as_ref().is_some_and(|s|!s.record&&!s.memory&&!s.storage)));
    // A claim the record model would have to make is still refused there.
    let wrong=source.replace("ensures true","ensures memory.prefix(result, 0, 0)");
    assert!(prepared(&wrong).is_err());
}

#[test]
fn constructor_resource_closure_rejects_a_live_focus_on_code_return() {
    let source=actual();let syntax=crate::compiler::parse(&source).unwrap();let logical=super::check::program(&syntax).unwrap();
    let mut f=logical.functions.iter().find(|f|f.name=="Vec.new").unwrap().clone();
    fn poison(body:&mut [super::logical_body::Stmt]) {
        use super::logical_body::*;
        for stmt in body {match stmt {
            Stmt::Return(value, _, _) if matches!(value.kind,Value::Sum{arm:0,..})=>{
                value.kind=Value::Sum{arm:1,payload:Box::new(Expr{ty:Ty::U64,kind:Value::U64(0)})};
            },
            Stmt::Match{arms,..}=>for arm in arms{poison(&mut arm.body);},_=>{},
        }}
    }
    // Deliberately corrupt an already checked constructor to isolate the VC's
    // independent lifetime closure, beyond SourceFlow's earlier refusal.
    poison(&mut f.body);
    let(goal,sealed)=super::record_model::body(&f,f.contract.as_ref().unwrap().record.as_ref().unwrap(),&logical.types).unwrap();
    assert!(sealed.arms.is_empty());assert!(goal.statement.contains("¬ True"));
}

#[test]
fn constructor_calls_never_invent_heap_history_for_nontrivial_later_calls() {
    let library=actual();let constructor=&library[..library.find("    pub fn len(").unwrap()];
    let helper="fn observe<T>(input: Vec<T>) -> Result<Vec<T>, Vec<T>>:\n    requires memory.prefix(input.data, input.length, input.capacity)\n    ensures true\n    proof:\n        auto\n    return Ok(move input)\n";
    let caller="fn later<T>(allocator: &Allocator) -> Result<Vec<T>, U64>:\n    let made: Result<Vec<T>, U64> = Vec.new(allocator, 1)\n    match move made:\n        Ok(empty):\n            match observe(move empty):\n                Ok(vector):\n                    return Ok(move vector)\n                Err(back):\n                    return Ok(move back)\n        Err(code):\n            return Err(code)\nfn main():\n    return\n";
    let failure=prepared(&format!("{constructor}{helper}{caller}")).err().unwrap();
    assert!(failure.message.contains("actual allocation element/substitution mismatch"),"{}",failure.message);
}

#[test]
fn actual_constructor_roundtrip_request_preserves_the_checker_budget() {
    let source=format!("{}{}{}",include_str!("../../library/wasm-vec.wy"),include_str!("../../library/wasm-buffer.wy"),include_str!("../../tests/fixtures/vec-constructor/roundtrip.wy"));
    let goals=prepared(&source).unwrap();let refs=goals.iter().collect::<Vec<_>>();
    let marker=format!("WITCHY_BODY_CHECK_V2:{}:{}","0".repeat(64),goals.len());
    let module=super::proof_module::emit(super::proof_module::Namespace::Current,&refs,None).unwrap().audited(&marker).unwrap();
    if let Ok(directory)=std::env::var("WITCHY_PROOF_SIZE_DIR"){std::fs::write(std::path::Path::new(&directory).join("constructor-size.json"),format!("{{\"module_bytes\":{},\"goals\":{},\"limit\":65536}}\n",module.len(),goals.len())).unwrap();}
    let requests=super::proof::request_sizes(&goals).unwrap();
    assert!(requests.iter().all(|bytes|*bytes<=65536),"a constructor request exceeds 65536 bytes: {requests:?}");
}

fn body<'a>(goals:&'a [Goal],name:&str)->&'a Goal{goals.iter().find(|g|g.source.as_ref().is_some_and(|s|s.function==name&&s.kind=="function_body")).expect("body goal")}
#[test]
fn owning_loops_are_checked_through_their_invariants_and_not_by_name(){
    let source=actual();let goals=prepared(&source).unwrap();let truncate=body(&goals,"Vec.truncate");
    assert!(truncate.source.as_ref().unwrap().record);
    // Dropping an invariant changes what the body must establish; dropping all of them is refused.
    let fewer=source.replace("            invariant vector.capacity == old(input.capacity)\n","");assert_ne!(fewer,source);
    assert_ne!(body(&prepared(&fewer).unwrap(),"Vec.truncate").statement,truncate.statement);
    let start=source.find("    pub fn truncate(").unwrap();
    let none=format!("{}{}",&source[..start],source[start..].lines().filter(|line|!line.trim_start().starts_with("invariant ")).collect::<Vec<_>>().join("\n")+"\n");
    assert!(prepared(&none).err().unwrap().message.contains("loops require invariants"));
    let renamed=source.replace("Vec","Parcel").replace("vec_","parcel_").replace(".length",".used").replace("    length:","    used:").replace("prefix(data, length, ","prefix(data, used, ").replace("vector","parcel");
    assert_eq!(body(&prepared(&renamed).unwrap(),"Parcel.truncate").statement,truncate.statement);
}
#[test]
fn a_consumed_owner_is_released_only_by_the_callees_word(){
    let source=actual();let goals=prepared(&source).unwrap();let push=body(&goals,"Vec.push");
    // The imported callee facts, not the callee bodies, are what vec_push is checked against.
    let weaker=source.replace("                memory.cell(output.data, old(input.length), item)\n                memory.unchanged_except(output.data, old(input.data), old(input.length))","                memory.unchanged_except(output.data, old(input.data), old(input.length))");assert_ne!(weaker,source);
    assert_ne!(body(&prepared(&weaker).unwrap(),"Vec.push").statement,push.statement);
    let start=source.find("    pub fn push(input: Vec<T>, allocator").unwrap();
    let claimed=format!("{}{}",&source[..start],source[start..].replacen("                memory.cell(output.data, old(input.length), item)\n","                memory.cell(output.data, old(input.length), item)\n                memory.released(old(input.data))\n",1));
    let claim=body(&prepared(&claimed).unwrap(),"Vec.push").statement.clone();
    // The claim must rest on the callee's own word or the successor's fate. Reading the
    // consumed owner's dead handle as a release would state it as `(¬ False)`.
    assert_ne!(claim,push.statement);assert!(!claim.contains("((¬ False))"),"{claim}");
}

#[test]
fn a_fixed_element_type_gets_the_same_ownership_proofs_as_a_generic_one(){
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/borrowed-str/owned-view.wy");
    let (source, parts) = super::files::read_bundle(&fixture).unwrap();
    let starts = super::files::starts(&parts);
    let prepared = |text: &str, starts: &[usize]| -> Result<Vec<Goal>, Diagnostic> {
        let syntax = crate::compiler::parse_modules(text, starts)?;
        let logical = super::check::program(&syntax)?;
        let goals = super::source_vc::prepare(&logical)?;
        super::concrete::program(logical)?;
        Ok(goals)
    };

    let goals = prepared(&source, &starts).unwrap();
    for name in [
        "String.new", "String.into_bytes", "String.close", "String.push_ascii_spare",
        "String.try_release", "String.from_utf8",
    ] {
        let goal = body(&goals, name);
        assert!(goal.source.as_ref().unwrap().record, "{name}");
        assert!(!goal.statement.contains('α'), "{name} still abstracts over an element type");
    }
    for name in ["String.byte_len", "String.byte_at", "input_is_utf8", "String.as_str"] {
        let goal = body(&goals, name);
        assert!(goal.source.as_ref().unwrap().memory, "{name}");
        assert!(!goal.statement.contains('α'), "{name} still abstracts over an element type");
    }
    // String.byte_at depends on Vec.get's contract. Preserve the bundle's module offsets.
    let weaker = source.replacen(
        "                code == 1\n                index >= value.length",
        "                code >= 0\n                index >= value.length", 1,
    );
    assert_ne!(weaker, source);
    assert_eq!(weaker.len(), source.len());
    let weaker = prepared(&weaker, &starts).unwrap();
    assert_ne!(body(&weaker, "String.byte_at").statement, body(&goals, "String.byte_at").statement);
    // U64 elements would be indistinguishable from lengths, so they are not accepted.
    let narrow=format!("{}import 0\ntype Parcel:\n    items: Vec<U16>\nfn open(input: Parcel) -> Vec<U16>:\n    ensures result.length == old(input.items.length)\n    proof:\n        auto\n    return move input.items\nfn main():\n    return\n",include_str!("../../library/wasm-vec.wy"));
    let narrow_starts = [0, include_str!("../../library/wasm-vec.wy").len()];
    assert!(body(&prepared(&narrow, &narrow_starts).unwrap(),"open").source.as_ref().unwrap().record);
    let failure=prepared(&narrow.replace("U16","U64"), &narrow_starts).err().unwrap();assert!(failure.message.contains("function contracts"),"{}",failure.message);
}
