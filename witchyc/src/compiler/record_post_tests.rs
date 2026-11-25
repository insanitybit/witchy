//! Source and private dependency controls; no kernel/runtime claim from these tests.
use super::{logical_body::*, proof::Goal, record_caller::Site, record_post};
use std::collections::{BTreeMap, BTreeSet};
const POSTS: &str = "    requires memory.prefix(input.data, input.length, input.capacity)\n    ensures match result:\n        Ok(out):\n            out.length == old(input.length) + 1\n            out.capacity == old(input.capacity)\n            memory.prefix(out.data, out.length, out.capacity)\n        Err(back):\n            true\n    proof:\n        auto\n";
fn source(posts: &str) -> String {
    let library = &include_str!("../../library/wasm-vec.wy").replace("PrefixBuffer<","Buffer<").replace("try_alloc_prefix","try_alloc"); // checked cell operations are a Buffer's
    let declarations = &library[..library.find("impl<T: Any> Vec<T>:").unwrap()].replace("    invariant memory.prefix(data, length, capacity)\n","");
    let checked = include_str!("../../tests/fixtures/ready-vec.wy");
    let body = &checked[checked.find("fn vec_push_spare").unwrap()..checked.find("fn close").unwrap()];
    let (signature, body) = body.split_once('\n').unwrap();
    let (_, body) = body.split_once("        auto\n").unwrap();
    let caller = r#"fn twice<T>(input: Vec<T>, first: T, second: T) -> Result<Vec<T>, VecPushError<T>>:
    match vec_push_spare(move input, first):
        Ok(once):
            return vec_push_spare(move once, second)
        Err(back):
            return Err(move back)
fn main():
    return
"#;
    format!("{declarations}{signature}\n{posts}{body}{caller}")
}
fn program(source: &str) -> Program {
    super::check::program(&crate::compiler::parse(source).unwrap()).unwrap()
}
fn prepared(source: &str) -> Vec<Goal> {
    let program = program(source);
    let goals = super::source_vc::prepare(&program).unwrap();
    super::concrete::program(program).unwrap();
    goals
}
#[test]
fn two_actual_sites_reuse_only_predecessor_ok_posts() {
    let original = source(POSTS);
    let goals = prepared(&original);
    assert_eq!(goals.len(), 3);
    assert_eq!(goals[0].source.as_ref().unwrap().kind, "function_body");
    assert_eq!(goals[1].source.as_ref().unwrap().site, Some(0));
    assert_eq!(goals[2].source.as_ref().unwrap().site, Some(1));
    assert!(!goals[1].prover.as_ref().unwrap().contains(record_post::MARKER));
    assert!(goals[2].prover.as_ref().unwrap().starts_with(record_post::MARKER));
    assert_ne!(goals[1].statement, goals[2].statement);
    let renamed = original.replace("VecPushError", "Refusal").replace("Vec", "Sequence")
        .replace("vec_push_spare", "append").replace(".length", ".used").replace("    length:", "    used:");
    assert_eq!(prepared(&renamed).len(), 3);
    let omitted = POSTS.replace("            memory.prefix(out.data, out.length, out.capacity)\n", "");
    let missing = prepared(&source(&omitted));
    assert_ne!(goals[2].statement, missing[2].statement);
    let old = POSTS.replace("memory.prefix(out.data, out.length, out.capacity)",
        "memory.prefix(old(input.data), old(input.length), old(input.capacity))");
    assert_ne!(goals[2].statement, prepared(&source(&old))[2].statement);
    let corrupted = original.replace("            return vec_push_spare(move once, second)",
        "            var changed = move once\n            changed.length = 0\n            return vec_push_spare(move changed, second)");
    assert_ne!(goals[2].statement, prepared(&corrupted)[2].statement);
}
#[test]
fn err_history_and_unknown_helper_cannot_feed_a_later_site() {
    let err = source(POSTS).replace("            return Err(move back)",
        "            return vec_push_spare(move back.vector, second)");
    let failure = super::source_vc::prepare(&program(&err)).err().unwrap();
    assert!(failure.message.contains("known allocation history"));
    let helper = "fn hidden<T>(input: Vec<T>) -> Vec<T>:\n    return move input\n\n";
    let unknown = source(POSTS).replace("fn twice", &format!("{helper}fn twice"))
        .replace("            return vec_push_spare(move once, second)",
            "            let changed = hidden(move once)\n            return vec_push_spare(move changed, second)");
    let failure = super::source_vc::prepare(&program(&unknown)).err().unwrap();
    assert!(failure.message.contains("unverified helper effects"));
}
#[test]
fn whole_conjunct_projection_never_erases_inside_or_or_negation() {
    use super::record_contract::Predicate;
    let mixed = Predicate::Binary("∨", Box::new(Predicate::Bool(true)), Box::new(Predicate::Bool(false)));
    let mut out = vec![];
    record_post::projection(&mixed, &mut out);
    record_post::projection(&Predicate::Not(Box::new(Predicate::Bool(false))), &mut out);
    assert!(out.is_empty());
}
#[test]
fn actual_return_seal_and_atomic_dependency_failures_refuse() {
    use super::record_value::Symbol;
    let mut seal = record_post::OwnedPost::default();
    assert!(seal.returned(0, &Symbol::Buffer(1), 0).is_err());
    assert!(seal.returned(0, &Symbol::Record(vec![Symbol::Buffer(0), Symbol::Buffer(0)]), 0).is_err());
    let program = program(&source(POSTS));
    let function = program.functions.iter().find(|f| f.name == "twice").unwrap();
    let callee = program.functions.iter().find(|f| f.name == "vec_push_spare").unwrap();
    let contract = callee.contract.as_ref().unwrap().record.as_ref().unwrap();
    let (_, seal) = super::record_model::body(callee, contract, &program.types).unwrap();
    let summaries = BTreeMap::from([(callee.definition.0, seal)]);
    let goals = super::source_vc::prepare(&program).unwrap();
    let refs = goals.iter().collect::<Vec<_>>();
    let site = |order, dependencies| Site {callee: callee.definition, parts: vec![], reached: true,
        order: Some(order), dependencies};
    let sites = BTreeMap::from([(0, site(0, BTreeSet::new())), (1, site(1, BTreeSet::from([0])))]);
    let validate = |sites: &BTreeMap<u32, Site>, goals: &[&Goal]| record_post::dependencies(
        function, &program.functions, sites, &summaries, goals);
    assert!(validate(&sites, &refs).is_ok());
    assert!(validate(&sites, &refs[1..]).is_err()); // body missing
    let missing = refs.iter().copied().filter(|g| g.source.as_ref().unwrap().site != Some(0)).collect::<Vec<_>>();
    assert!(validate(&sites, &missing).is_err());
    let mut bad = sites.clone(); bad.remove(&0);
    assert!(validate(&bad, &refs).is_err());
    let mut bad = sites.clone(); bad.get_mut(&0).unwrap().dependencies.insert(1);
    assert!(validate(&bad, &refs).is_err()); // cycle/future
    let mut bad = sites.clone(); bad.get_mut(&0).unwrap().order = Some(2);
    assert!(validate(&bad, &refs).is_err()); // future without cycle
    let mut bad = sites.clone(); bad.get_mut(&1).unwrap().dependencies.insert(1);
    assert!(validate(&bad, &refs).is_err()); // self
}
fn err_posts() -> String {
    POSTS.replace("        Err(back):\n            true", "        Err(back):\n            memory.prefix(back.vector.data, back.vector.length, back.vector.capacity)")
}
#[test]
fn explicit_returned_err_prefix_enables_both_paths_without_implicit_history() {
    let caller = |posts: &str| source(posts).replace("            return Err(move back)",
        "            return vec_push_spare(move back.vector, second)");
    let original = caller(&err_posts());
    let goals = prepared(&original);
    assert_eq!(goals.len(), 4); // universal body and all three actual sites
    assert_eq!(goals[3].source.as_ref().unwrap().site, Some(2));
    let renamed = original.replace("VecPushError", "Refusal").replace("Vec", "Sequence")
        .replace("vec_push_spare", "append").replace(".length", ".used").replace("    length:", "    used:");
    assert_eq!(prepared(&renamed).len(), 4);
    // Old entry Prefix and valid headers alone cannot enable returned occupancy.
    let old = err_posts().replace("memory.prefix(back.vector.data, back.vector.length, back.vector.capacity)",
        "memory.prefix(old(input.data), old(input.length), old(input.capacity))");
    for posts in [POSTS.to_string(), old] {
        let failure = super::source_vc::prepare(&program(&caller(&posts))).err().unwrap();
        assert!(failure.message.contains("known allocation history"));
    }
    let changed = original.replace("            return vec_push_spare(move back.vector, second)",
        "            var retry = move back.vector\n            retry.length = 0\n            return vec_push_spare(move retry, second)");
    assert_ne!(goals[3].statement, prepared(&changed)[3].statement);
    let false_post = err_posts().replace("back.vector.length, back.vector.capacity)",
        "back.vector.length + 1, back.vector.capacity)");
    assert_ne!(goals[0].statement, prepared(&caller(&false_post))[0].statement);
}
#[test]
fn only_active_after_owner_prefix_crosses_the_err_history_barrier() {
    use super::record_contract::{Path, Root, Predicate, Term, RecordContract};
    let sealed = record_post::OwnedPost { arms: BTreeMap::from([(0, vec![0]), (1, vec![0, 0])]) };
    let path = Path {root: Root::Arm(1), fields: vec![0, 0], ty: Ty::Buffer(0), before: false};
    let prefix = |path| Predicate::Prefix(path, Term::Literal(0), Term::Literal(1));
    let eligible = |selected, predicate| sealed.returned_prefix(&RecordContract {
        requires: vec![], ensures: vec![(selected, predicate)], nodes: 0,
        element: Ty::Param { owner: FunctionId(0), index: 0 }, assumes: vec![], maintains: vec![], module: 0,
        mutable_owner: None}, 1);
    assert!(eligible(Some(1), prefix(path.clone())));
    assert!(!eligible(Some(0), prefix(path.clone())));
    assert!(!eligible(None, prefix(path.clone())));
    let mut wrong = path.clone(); wrong.root = Root::Parameter(0);
    assert!(!eligible(Some(1), prefix(wrong)));
    let mut wrong = path.clone(); wrong.root = Root::Arm(0);
    assert!(!eligible(Some(1), prefix(wrong)));
    let mut wrong = path.clone(); wrong.before = true;
    assert!(!eligible(Some(1), prefix(wrong)));
    let mut wrong = path.clone(); wrong.fields = vec![0];
    assert!(!eligible(Some(1), prefix(wrong)));
    let mut wrong = path.clone(); wrong.ty = Ty::U64;
    assert!(!eligible(Some(1), prefix(wrong)));
    assert!(!eligible(Some(1), Predicate::Bool(true)));
    assert!(!eligible(Some(1), Predicate::Same(path.clone(), path.clone())));
    assert!(!eligible(Some(1), Predicate::Unchanged(path.clone(), path.clone())));
    assert!(!eligible(Some(1), Predicate::Not(Box::new(prefix(path.clone())))));
    assert!(!eligible(Some(1), Predicate::Binary("∨", Box::new(prefix(path.clone())), Box::new(Predicate::Bool(true)))));
    assert!(eligible(Some(1), Predicate::Binary("∧", Box::new(prefix(path)), Box::new(Predicate::Bool(true)))));
}
