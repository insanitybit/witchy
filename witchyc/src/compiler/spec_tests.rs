//! Static hygiene/type/expansion controls. Real kernel/runtime checks are separate.
use crate::{ast as a,parser};
use super::{spec,check,source_vc};
fn normalize(source:&str)->Result<a::Program,crate::diagnostic::Diagnostic>{let p=parser::parse(source)?;Ok(spec::prepare(&p)?.into_owned())}
fn failure(source:&str)->String{normalize(source).err().expect("must refuse").message.to_string()}
#[test]
fn false_unused_definition_is_valid_and_erased(){
    let mut p=normalize("predicate nope():\n    false\nfn main():\n    return\n").unwrap();
    // The call-once trait is always declared (closure.rs); this path does not lift traits.
    p.declarations.retain(|d|!matches!(d,a::Declaration::Trait{..}));
    assert_eq!(p.declarations.len(),1);let checked=check::program(&p).unwrap();assert!(source_vc::prepare(&checked).unwrap().is_empty());
}
#[test]
fn every_unused_definition_is_typed_and_pure(){
    for body in ["unknown < 1","memory.take(buffer, 0)","index && true","memory.capacity(buffer)","memory.frame_unchanged(old(buffer))","forall j in (0, index): true"]{
        let s=format!("predicate bad<T>(buffer: &Buffer<T>, index: U64):\n    {body}\nfn main():\n    return\n");assert!(normalize(&s).is_err(),"{body}");
    }
}
#[test]
fn recursive_and_unknown_calls_refuse_even_unused(){
    assert!(failure("predicate p(x: U64):\n    p(x)\nfn main():\n    return\n").contains("recursive"));
    assert!(failure("predicate p(x: U64):\n    q(x)\npredicate q(y: U64):\n    p(y)\nfn main():\n    return\n").contains("recursive"));
    assert!(failure("predicate p(x: U64):\n    missing(x)\nfn main():\n    return\n").contains("unknown"));
}
#[test]
fn wrong_argument_type_and_unused_signature_refuse(){
    assert!(failure("predicate p(x: U64):\n    x < 2\nfn f(x: U64) -> U64:\n    ensures p(true)\n    proof:\n        auto\n    return x\nfn main():\n    return\n").contains("type mismatch"));
    assert!(normalize("predicate bad(x: Missing):\n    true\nfn main():\n    return\n").is_err());
}
#[test]
fn formal_ids_do_not_capture_result_or_another_formal(){
    let s="predicate eq(result: U64, x: U64):\n    result == x\nfn f(x: U64) -> U64:\n    ensures eq(result, x)\n    proof:\n        auto\n    return x\nfn main():\n    return\n";
    let p=normalize(s).unwrap();let a::Declaration::Function(f)=&p.declarations[0] else{panic!()};let a::PostconditionKind::Predicate(e)=&f.ensures[0].kind else{panic!()};
    let a::Expression::Binary("==",l,r)=&e.kind else{panic!()};assert!(matches!(&l.kind,a::Expression::Name(n)if n=="result"));assert!(matches!(&r.kind,a::Expression::Name(n)if n=="x"));
    assert!(e.span.start>=s.find("ensures").unwrap());
}
#[test]
fn explicit_formal_binding_cannot_be_called_as_a_global_predicate(){
    assert!(failure("predicate p(x: U64):\n    x < 2\npredicate bad(p: U64):\n    p(0)\nfn main():\n    return\n").contains("not callable"));
}
#[test]
fn snapshot_paths_are_substituted_without_dropping_old(){
    let s="predicate equal_before<T>(buffer: &Buffer<T>, other: &Buffer<T>):\n    memory.same_allocation(old(buffer), other)\nfn f<T>(buffer: &Buffer<T>) -> T:\n    requires false\n    ensures equal_before(buffer, old(buffer))\n    proof:\n        auto\n    return memory.get_unchecked(buffer, 0)\nfn main():\n    return\n";
    let p=normalize(s).unwrap();let a::Declaration::Function(f)=&p.declarations[0]else{panic!()};let a::PostconditionKind::Predicate(e)=&f.ensures[0].kind else{panic!()};
    let a::Expression::Call(_,args)=&e.kind else{panic!()};assert!(args.iter().all(|a|matches!(&a.kind,a::Expression::Call(c,_)if matches!(&c.kind,a::Expression::Name(n)if n=="old"))));
}
#[test]
fn runtime_and_named_loop_invariant_uses_refuse_explicitly(){
    for body in ["    if p(0):\n        return\n", "    var i: U64 = 0\n    while i < 1:\n        invariant p(i)\n        i += 1\n"]{
        let s=format!("predicate p(x: U64):\n    x < 2\nfn main():\n{body}");assert!(failure(&s).contains("specification-only"));
    }
}
#[test]
fn expanded_preconditions_and_vec_post_are_identical_to_handwritten_goals(){
    let library=include_str!("../../library/wasm-vec.wy");
    let named=format!("{library}fn main():\n    return\n");
    let manual=library[library.find("sealed type Vec<T>").unwrap()..].replace("requires readable(buffer, index)","requires index < memory.capacity(buffer) && memory.initialized(buffer, index)").replace("within(output.length, output.capacity)","output.length <= output.capacity");
    let manual=format!("{manual}fn main():\n    return\n");
    let goals=|s:&str|{let a=crate::compiler::parse(s).unwrap();let p=check::program(&a).unwrap();source_vc::prepare(&p).unwrap().into_iter().map(|g|(g.source_name,g.statement,g.prover)).collect::<Vec<_>>()};
    assert_eq!(goals(&named),goals(&manual));
}
#[test]
fn expansion_fanout_is_bounded_before_materialization(){
    let mut source="predicate p0(x: U64):\n    x < 1\n".to_string();
    for n in 1..14{source+=&format!("predicate p{n}(x: U64):\n    p{}(x) && p{}(x)\n",n-1,n-1);}
    source+="fn f(x: U64) -> U64:\n    ensures p13(x)\n    proof:\n        auto\n    return x\nfn main():\n    return\n";
    assert!(failure(&source).contains("expansion budget"));
}

#[test]
fn unused_acyclic_definition_depth_has_its_own_bound(){
    let mut source=String::new();
    for n in 0..66{source+=&format!("predicate p{n}(x: U64):\n    p{}(x)\n",n+1);}
    source+="predicate p66(x: U64):\n    true\nfn main():\n    return\n";
    assert!(failure(&source).contains("definition dependency depth"));
}

#[test]
fn preflight_counts_memory_and_old_rendering_scaffolding(){
    let p=parser::parse("predicate p<T>(b: &Buffer<T>):\n    memory.same_allocation(b, old(b))\nfn main():\n    return\n").unwrap();
    let c=super::spec_resolve::Catalog::new(&p).unwrap();let d=&c.definitions[0];
    let sizes=d.binders.iter().map(|b|(*b,super::spec_expand::Cost{nodes:1,bytes:1})).collect();
    let cost=super::spec_expand::cost(c.bodies[0].as_ref().unwrap(),&c,&sizes,0).unwrap();
    // memory Call+Field+Name, b, old Call+Name, b = seven rendered nodes.
    assert_eq!(cost.nodes,7);assert!(cost.bytes>="memory.same_allocation(b, old(b))".len());
    let max=super::spec_expand::Cost{nodes:4096,bytes:0};
    assert!(max.add(super::spec_expand::Cost{nodes:1,bytes:0},&(0..1)).is_err());
}

#[test]
fn guard_uses_declared_tag_after_renaming_and_reordering() {
    for (variants, expected) in [("    Empty\n    Entry(U64)", 1), ("    Entry(U64)\n    Empty", 0)] {
        for name in ["positive", "above_zero"] {
            let source = format!(
                "type Choice:\n{variants}\npredicate {name}(n: U64):\n    n > 0\nfn keep(x: Choice) -> U64:\n    requires when Entry(item) = x:\n        {name}(item)\n    return 1\nfn main():\n    return\n"
            );
            let program = normalize(&source).unwrap();
            let f = program
                .declarations
                .iter()
                .find_map(|d| match d {
                    a::Declaration::Function(f) if f.name == "keep" => Some(f),
                    _ => None,
                })
                .unwrap();
            let mut tags = vec![];
            f.requires[0].clone().visit_mut(&mut |e| {
                if let a::Expression::Call(c, args) = &e.kind
                    && matches!(&c.kind,a::Expression::Field(_,n) if n=="variant_value")
                {
                    tags.push(args[1].kind.clone());
                }
                false
            });
            assert_eq!(tags, [a::Expression::Integer(expected)]);
            let parsed = crate::compiler::parse(&source).unwrap();
            let typed = check::program(&parsed).unwrap();
            assert!(!source_vc::prepare(&typed).unwrap().is_empty());
            assert!(normalize(&source.replace("when Entry(item)", "when Missing(item)")).is_err());
        }
    }
}

#[test]
fn imported_predicate_can_observe_only_its_own_private_projection() {
    let lib =
        "pub sealed type Meter:\n    reading: U64\npub predicate at_zero(m: &Meter):\n    m.reading == 0\n";
    let client = "fn test(m: &Meter) -> U64:\n    requires at_zero(m)\n    ensures result == 0\n    return 0\nfn main():\n    return\n";
    let check = |lib: &str, client: &str| {
        let program = crate::compiler::parse_modules(&format!("{lib}import 0\n{client}"), &[0, lib.len()])?;
        let logical = check::program(&program)?;
        source_vc::prepare(&logical)
    };
    assert!(check(lib, client).is_ok());
    assert!(check(lib, &client.replace("at_zero(m)", "m.reading == 0")).is_err());
    assert!(check(lib, &format!("predicate stolen(m: &Meter):\n    m.reading == 0\n{client}")).is_err());
    assert!(check(&lib.replace("pub predicate", "predicate"), client).is_err());
}

#[test]
fn every_method_in_an_unused_predicate_must_be_a_checked_view() {
    let source = "type Meter:\n    reading: U64\nimpl Meter:\n    fn choose(m: &Meter) -> &U64:\n        unreachable\npredicate hidden(m: &Meter):\n    *m.choose() == 0\nfn main():\n    return\n";
    let program = crate::compiler::parse(source).unwrap();
    let e = check::program(&program).err().expect("invalid projection in unused predicate");
    assert!(e.message.contains("proof view"), "{e:?}");
}

#[test]
fn malformed_explicit_proofs_still_refuse() {
    for steps in ["        auto\n        auto\n", "        auto\n    proof:\n        auto\n", "        have false\n"] {
        let source=format!("fn f(x: U64) -> U64:\n    ensures result == x\n    proof:\n{steps}    return x\nfn main():\n    return\n");
        let checked=crate::compiler::parse(&source).and_then(|p|check::program(&p));
        assert!(checked.is_err(),"{steps}");
    }
}
