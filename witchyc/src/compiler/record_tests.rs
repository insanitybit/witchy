//! Source-only controls; actual kernel/runtime qualification belongs to the host owner.
use super::proof::Goal;
use crate::diagnostic::Diagnostic;
const CLAUSES:&str="    requires memory.prefix(input.data, input.length, input.capacity)\n    ensures match result:\n        Ok(output):\n            memory.same_allocation(output.data, old(input.data))\n            output.length == old(input.length) + 1\n            output.capacity == old(input.capacity)\n            memory.prefix(output.data, output.length, output.capacity)\n            memory.cell(output.data, old(input.length), item)\n            memory.unchanged_except(output.data, old(input.data), old(input.length))\n        Err(back):\n            back.vector.length == old(input.length)\n            back.vector.capacity == old(input.capacity)\n            old(input.length) == old(input.capacity)\n            back.item == item\n            memory.unchanged(back.vector.data, old(input.data))\n    proof:\n        auto\n";
fn source(clauses:&str)->String {
    // The checked cell operations these programs use are a Buffer's, so the library's record is read as holding one.
    let library=&include_str!("../../library/wasm-vec.wy").replace("PrefixBuffer<","Buffer<").replace("try_alloc_prefix","try_alloc");
    let declarations=&library[..library.find("impl<T: Any> Vec<T>:").unwrap()].replace("    invariant memory.prefix(data, length, capacity)\n","");
    let checked=include_str!("../../tests/fixtures/ready-vec.wy");
    let body=&checked[checked.find("fn vec_push_spare").unwrap()..checked.find("fn close").unwrap()];
    let(signature,body)=body.split_once('\n').unwrap();
    let (_,body)=body.split_once("        auto\n").unwrap();
    format!("{declarations}{signature}\n{clauses}{body}fn main():\n    return\n")
}
fn prepared(source:&str)->Result<Vec<Goal>,Diagnostic> {
    let syntax=crate::compiler::parse(source)?;let logical=super::check::program(&syntax)?;
    let goals=super::source_vc::prepare(&logical)?;super::concrete::program(logical)?;Ok(goals)
}
#[test]
fn checked_record_fixture_has_one_generic_obligation_and_no_implicit_prefix() {
    let original=source(CLAUSES);let goals=prepared(&original).unwrap();assert_eq!(goals.len(),1);
    let id=goals[0].source.as_ref().unwrap();assert!(id.record);assert!(!id.storage);assert_eq!(id.kind,"function_body");
    assert!(goals[0].statement.contains("α Key : Type"));assert!(goals[0].statement.contains("cap = g1"));
    let no_requires=CLAUSES.replace("    requires memory.prefix(input.data, input.length, input.capacity)\n","");
    let changed=prepared(&source(&no_requires)).unwrap();assert_ne!(goals[0].statement,changed[0].statement);
    let bad=prepared(&original.replace("vector.length = index + 1","vector.length = index + 0")).unwrap();assert_ne!(goals[0].statement,bad[0].statement);
    let renamed=original.replace("VecPushError","Refusal").replace("VecTaken","Taken").replace("Vec","Sequence").replace("vec_push_spare","append").replace(".length",".used").replace("    length:","    used:");
    assert!(prepared(&renamed).is_ok());
}
#[test]
fn unused_unknown_headers_and_dead_sites_have_distinct_actual_obligations() {
    for body in ["    return vec_push_spare(move input, item)\n","    if false:\n        return vec_push_spare(move input, item)\n    return Err(VecPushError(move input, item))\n"] {
        let helper=format!("fn helper<T>(input: Vec<T>, item: T) -> Result<Vec<T>, VecPushError<T>>:\n{body}");
        let source=source(CLAUSES).replace("fn main():",&format!("{helper}fn main():"));
        let goals=prepared(&source).unwrap();assert_eq!(goals.len(),2);let goal=&goals[1];let id=goal.source.as_ref().unwrap();
        assert_eq!(id.function,"helper");assert_eq!(id.callee.as_deref(),Some("vec_push_spare"));assert_eq!(id.site,Some(0));assert_eq!(id.kind,"call_precondition");
        if body.contains("if false"){assert!(goal.statement.contains("(False) → False"));}else{assert!(goal.statement.contains("∀ j : Nat"));assert!(!goal.statement.contains("hr0"));}
    }
}
#[test]
fn distinct_sites_and_forked_paths_are_not_deduplicated(){
    let separate="fn choose<T>(input: Vec<T>, item: T, flag: Bool) -> Result<Vec<T>, VecPushError<T>>:\n    if flag:\n        return vec_push_spare(move input, item)\n    return vec_push_spare(move input, item)\n";
    let goals=prepared(&source(CLAUSES).replace("fn main():",&format!("{separate}fn main():"))).unwrap();
    assert_eq!(goals.len(),3);assert_eq!(goals[1].source.as_ref().unwrap().site,Some(0));assert_eq!(goals[2].source.as_ref().unwrap().site,Some(1));
    let forked="fn choose<T>(input: Vec<T>, item: T, flag: Bool) -> Result<Vec<T>, VecPushError<T>>:\n    var vector = move input\n    if flag:\n        vector.length = 0\n    else:\n        vector.length = 1\n    return vec_push_spare(move vector, item)\n";
    let goals=prepared(&source(CLAUSES).replace("fn main():",&format!("{forked}fn main():"))).unwrap();assert_eq!(goals.len(),2);
    let flag=goals[1].statement.split(" : Prop)").next().unwrap().rsplit('(').next().unwrap();
    assert!(goals[1].statement.contains(&format!("({flag}) →")));assert!(goals[1].statement.contains(&format!("(¬ {flag}) →")));
}
fn fresh_caller(prefix:&str,claimed:&str)->String{
    let checked=include_str!("../../tests/fixtures/ready-vec.wy");let cleanup=checked[checked.find("fn close<T>").unwrap()..checked.find("fn populate<T>").unwrap()].replace("close(","vec_buffer_close(").replace("fn close<T>","fn vec_buffer_close<T>")+"fn vec_close<T>(input: Vec<T>):\n    vec_buffer_close(move input.data)\n";
    let caller=format!("fn exercise<T>(console: &Console<Write>, allocator: &Allocator, item: T, requested: U64):\n    let allocation: Result<Buffer<T>, U64> = memory.try_alloc(allocator, requested)\n    match move allocation:\n        Ok(buffer):\n            var data = move buffer\n{prefix}            let vector: Vec<T> = Vec(move data, 0, {claimed})\n            match vec_push_spare(move vector, item):\n                Ok(out):\n                    vec_close(move out)\n                Err(back):\n                    vec_close(move back.vector)\n        Err(code):\n            return\n");
    source(CLAUSES).replace("fn main():",&format!("{cleanup}{caller}fn main():"))
}
#[test]
fn actual_fresh_allocation_has_one_guarded_prefix_goal_and_no_assumed_success(){
    let goals=prepared(&fresh_caller("","requested")).unwrap();assert_eq!(goals.len(),2);let goal=&goals[1];
    assert!(goal.statement.contains("none : Option α"));assert!(goal.statement.contains("(g2) →"));assert_eq!(goal.source.as_ref().unwrap().site,Some(0));
    let bad=prepared(&fresh_caller("","requested + 1")).unwrap();assert_ne!(goal.statement,bad[1].statement);
    let dirty=prepared(&fresh_caller("            let _ = memory.init(&mut data, 0, item)\n","requested")).unwrap();assert!(dirty[1].statement.contains("some"));assert_ne!(goal.statement,dirty[1].statement);
}
#[test]
fn checked_init_suffix_uses_the_same_primary_site_namespace(){
    let initialize="fn initialize<T>(buffer: &mut Buffer<T>, index: U64, item: T) -> Result<(), U64>:\n    ensures true\n    proof:\n        auto\n    return memory.init(buffer, index, item)\n\n";
    let source=fresh_caller("","requested").replace("fn exercise",&format!("{initialize}fn exercise")).replace("                    vec_close(move out)","                    var kept = move out\n                    let _ = initialize(&mut kept.data, 0, item)\n                    vec_close(move kept)");
    let goals=prepared(&source).unwrap();let calls=goals.iter().filter_map(|g|g.source.as_ref()).filter(|s|s.function=="exercise"&&s.kind=="call_precondition").collect::<Vec<_>>();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].site, Some(0));
    assert_eq!(calls[1].site, Some(1));
    assert_eq!(calls[1].callee.as_deref(), Some("initialize"));
    assert!(calls.iter().all(|site| site.record));
}
#[test]
fn unknown_helper_history_and_payload_sensitive_requires_still_refuse(){
    let changed=fresh_caller("", "requested").replace("fn exercise", "fn identity<T>(input: Vec<T>) -> Vec<T>:\n    return move input\n\nfn exercise").replace("match vec_push_spare(move vector, item):","let hidden = identity(move vector)\n            match vec_push_spare(move hidden, item):");
    let error=prepared(&changed).err().unwrap();assert_eq!(error.code,"W0401");
    let sensitive=fresh_caller("","requested").replace("    requires memory.prefix(input.data, input.length, input.capacity)","    requires item == item");
    let error=prepared(&sensitive).err().unwrap();assert_eq!(error.code,"W0401");assert!(error.message.contains("payload-insensitive"));
}
#[test]
fn mutable_helper_before_fresh_allocation_cannot_keep_stale_scalar_facts(){
    let helper="fn change(value: &mut U64):\n    *value += 1\n\n";
    let source=fresh_caller("","requested").replace("fn exercise",&format!("{helper}fn exercise")).replace("    let allocation: Result<Buffer<T>, U64> = memory.try_alloc(allocator, requested)","    var actual = requested\n    change(&mut actual)\n    let allocation: Result<Buffer<T>, U64> = memory.try_alloc(allocator, actual)");
    let error=prepared(&source).err().unwrap();assert_eq!(error.code,"W0401");assert!(error.message.contains("unverified helper effects"));
}
fn nested_caller(expression:&str)->String{
    let consume="fn consume<T>(input: Result<Vec<T>, VecPushError<T>>):\n    match move input:\n        Ok(out):\n            vec_close(move out)\n        Err(back):\n            vec_close(move back.vector)\n\nfn consume_first<T>(ignored: U64, input: Result<Vec<T>, VecPushError<T>>):\n    consume(move input)\n\nfn consume_last<T>(input: Result<Vec<T>, VecPushError<T>>, ignored: U64):\n    consume(move input)\n\nfn change(value: &mut U64) -> U64:\n    *value += 1\n    return *value\n\n";
    fresh_caller("            var scratch: U64 = 0\n","requested").replace("fn exercise",&format!("{consume}fn exercise")).replace("            match vec_push_spare(move vector, item):\n                Ok(out):\n                    vec_close(move out)\n                Err(back):\n                    vec_close(move back.vector)",expression)
}
#[test]
fn ordinary_outer_call_and_later_argument_after_last_nested_site_are_supported(){
    for expression in ["            consume(vec_push_spare(move vector, item))","            consume_last(vec_push_spare(move vector, item), change(&mut scratch))"]{
        let goals=prepared(&nested_caller(expression)).unwrap();assert_eq!(goals.len(),2);let id=goals[1].source.as_ref().unwrap();assert_eq!(id.site,Some(0));assert_eq!(id.callee.as_deref(),Some("vec_push_spare"));
    }
}
#[test]
fn earlier_argument_unknown_effect_cannot_precede_a_nested_required_site(){
    let source=nested_caller("            consume_first(change(&mut scratch), vec_push_spare(move vector, item))");
    let error=prepared(&source).err().unwrap();assert_eq!(error.code,"W0401");assert!(error.message.contains("unverified helper effects"));
}
#[test]
fn outer_call_before_another_statement_site_still_refuses(){
    let helper="fn twice<T>(first: Vec<T>, second: Vec<T>, item: T):\n    consume(vec_push_spare(move first, item))\n    consume(vec_push_spare(move second, item))\n\n";
    let source=nested_caller("            consume(vec_push_spare(move vector, item))").replace("fn exercise",&format!("{helper}fn exercise"));
    let error=prepared(&source).err().unwrap();assert_eq!(error.code,"W0401");assert!(error.message.contains("unverified helper effects"));
}
#[test]
fn unknown_mutation_before_a_branch_cannot_hide_a_false_to_true_site(){
    let flip="fn flip(flag: &mut Bool):\n    *flag = true\n\n";
    let expression="            var active: Bool = false\n            flip(&mut active)\n            if active:\n                consume(vec_push_spare(move vector, item))\n            else:\n                vec_close(move vector)";
    let source=nested_caller(expression).replace("fn exercise",&format!("{flip}fn exercise"));
    let error=prepared(&source).err().unwrap();assert_eq!(error.code,"W0401");assert!(error.message.contains("unverified helper effects"));
}
#[test]
fn add_safety_is_separate_guarded_and_not_assumed_from_result_type() {
    let simple="    ensures true\n    proof:\n        auto\n";
    let no_requires=prepared(&source(simple)).unwrap();assert!(no_requires[0].statement.contains(" + (1 : Nat)"));
    let prefix=format!("    requires memory.prefix(input.data, input.length, input.capacity)\n{simple}");
    let bad=prepared(&source(&prefix).replace("vector.length = index + 1","vector.length = index + 18446744073709551615")).unwrap();
    assert!(bad[0].statement.contains(" + (18446744073709551615 : Nat)"));
    let before_lets=bad[0].statement.split("let v0").next().unwrap();assert!(before_lets.split(|c:char|!c.is_alphanumeric()).all(|word|!word.starts_with('v')));
    let skipped=source(simple).replace("    var vector = move input","    let skipped: Bool = true || input.length + 18446744073709551615 > 0\n    var vector = move input");
    let skipped=prepared(&skipped).unwrap();assert!(!skipped[0].statement.contains(" + (18446744073709551615 : Nat)"));
    let complex=source(simple).replace("fn vec_push_spare", "fn copy<T>(value: T) -> T:\n    return value\n\nfn vec_push_spare").replace("memory.init(&mut vector.data, index, item)","memory.init(&mut vector.data, index, copy(item))");
    let failure=prepared(&complex).err().unwrap();assert_eq!(failure.code,"W0401");assert!(failure.message.starts_with("record contracts Init payload requires a nontrapping Copy parameter or field"));
}

const OWNED_UNCHECKED:&str=r#"type Shelf<T>:
    data: Buffer<T>
    length: U64
    capacity: U64
type Refusal<T>:
    shelf: Shelf<T>
    item: T
fn append<T>(input: Shelf<T>, item: T) -> Result<Shelf<T>, Refusal<T>>:
    ensures match result:
        Ok(out):
            out.length == old(input.length) + 1
            memory.cell(out.data, old(input.length), item)
            memory.unchanged_except(out.data, old(input.data), old(input.length))
        Err(back):
            back.item == item
            back.shelf.length == old(input.length)
            memory.unchanged(back.shelf.data, old(input.data))
    proof:
        auto
    var shelf = move input
    let index = shelf.length
    let actual = memory.capacity(&shelf.data)
    if index < actual:
        if !memory.initialized(&shelf.data, index):
            memory.init_unchecked(&mut shelf.data, index, item)
            shelf.length = index + 1
            return Ok(move shelf)
    return Err(Refusal(move shelf, item))
"#;
fn owned_unchecked(extra:&str)->String{format!("{OWNED_UNCHECKED}{extra}fn main():\n    return\n")}
#[test]
fn ordinary_owned_body_proves_actual_unsafe_index_empty_payload_and_return_footprint(){
    let source=owned_unchecked("");let original=prepared(&source).unwrap();assert_eq!(original.len(),1);
    let body=&original[0];assert!(body.source.as_ref().unwrap().record);assert!(body.statement.contains("fun j : Nat => if j ="));assert!(body.statement.contains("≠ none"));assert!(body.statement.contains(" = none"));
    for changed in [
        source.replace("    if index < actual:\n        if !memory.initialized(&shelf.data, index):\n            memory.init_unchecked", "    if true:\n        if !memory.initialized(&shelf.data, index):\n            memory.init_unchecked"),
        source.replace("if !memory.initialized(&shelf.data, index):", "if true:"),
        source.replace("memory.init_unchecked(&mut shelf.data, index, item)", "memory.init_unchecked(&mut shelf.data, actual, item)"),
        source.replace("item: T) -> Result<Shelf<T>","item: T, other: T) -> Result<Shelf<T>").replace("memory.init_unchecked(&mut shelf.data, index, item)", "memory.init_unchecked(&mut shelf.data, index, other)"),
        source.replace("memory.unchanged_except(out.data, old(input.data), old(input.length))", "memory.unchanged(out.data, old(input.data))"),
    ]{let changed=prepared(&changed).unwrap();assert_ne!(body.statement,changed[0].statement);}
}
#[test]
fn empty_requires_owned_calls_need_no_invented_history_or_reached_evidence(){
    let helper="fn ordinary<T>(input: Shelf<T>, item: T) -> Result<Shelf<T>, Refusal<T>>:\n    while false:\n        memory.capacity(&input.data)\n    return append(move input, item)\n";
    let source=owned_unchecked(helper);let syntax=crate::compiler::parse(&source).unwrap();let logical=super::check::program(&syntax).unwrap();let proof=super::source_vc::prepare_with_sites(&logical).unwrap();super::concrete::program(logical).unwrap();
    assert_eq!(proof.goals.len(),2);let site=&proof.goals[1];assert_eq!(site.statement,"True");assert_eq!(site.source.as_ref().unwrap().site,Some(0));assert_eq!(site.source.as_ref().unwrap().callee.as_deref(),Some("append"));assert!(proof.sites.is_empty());
    let nontrivial=source.replace("    ensures match result:","    requires input.length < memory.capacity(input.data)\n    ensures match result:");let error=prepared(&nontrivial).err().unwrap();assert_eq!(error.code,"W0401");assert!(error.message.contains("loops"));
}
#[test]
fn dead_owned_unsafe_nodes_are_audited_and_unproved_or_unsupported_contexts_refuse(){
    let source=owned_unchecked("");let dead=source.replace("    var shelf = move input", "    if false:\n        var unused = move input\n        memory.init_unchecked(&mut unused.data, 0, item)\n        return Ok(move unused)\n    var shelf = move input");
    let goals=prepared(&dead).unwrap();assert!(goals[0].statement.contains("(False) → False"));
    let implicit=source.replace("    proof:\n        auto\n", "");assert_eq!(prepared(&implicit).unwrap()[0].statement,prepared(&source).unwrap()[0].statement);
    let unproved="fn hidden<T>(input: Shelf<T>, item: T) -> Result<Shelf<T>, Refusal<T>>:\n    var shelf = move input\n    if false:\n        memory.init_unchecked(&mut shelf.data, 0, item)\n    return Ok(move shelf)\n";let hidden=prepared(&owned_unchecked(unproved)).unwrap();assert!(hidden.iter().any(|g|g.source.as_ref().is_some_and(|s|s.function=="hidden") && g.statement.contains("False")));
    let unsupported=source.replace("memory.init_unchecked(&mut shelf.data, index, item)","memory.get_unchecked(&shelf.data, index)");assert_eq!(prepared(&unsupported).err().unwrap().code,"W0401");
}
