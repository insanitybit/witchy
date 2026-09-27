use super::source;
use witchy_canonical_source::source::{
    DeferredExecutionRequirement as Requirement, DeferredExecutionRole as Role,
    SourceExecutionMode as Mode, TaskExecutionProof as Proof,
};
use witchy_syntax::ast::Item;

const WORKER: &str = r#"mode opt
import task

type Envelope(a):
    value: a

pub async fn compatible() -> Int:
    7

pub fn factory() -> task.Task(Int):
    compatible()

pub fn invoke(make: fn() -> task.Task(Int)) -> task.Task(Int):
    make()

fn recursive_scalar() -> Int:
    recursive_scalar()

pub fn unused_recursive_callback() -> fn() -> Int:
    fn(): recursive_scalar()
"#;

const MAIN: &str = r#"import task
import worker

fn normal_factory() -> task.Task(Int):
    worker.compatible()

fn relay(own recipe: task.Task(Int)) -> task.Task(Int):
    recipe

fn invoke_opt_factory() -> task.Task(Int):
    worker.invoke(worker.factory)

fn invoke_stored_opt_factory() -> task.Task(Int):
    let boxed = worker.Envelope(worker.factory)
    worker.invoke(boxed.value)

async fn main(console: Console):
    let _ = worker.unused_recursive_callback()
    let _ = task.ready_unit().await
    let _ = task.yield_now().await
    let first = invoke_opt_factory().await
    let second = invoke_stored_opt_factory().await
    let third = relay(normal_factory()).await
    console.print("${first + second + third}")
"#;

#[test]
fn authenticated_step_roles_preserve_passive_results_and_executable_fields() {
    let checked = source::checked_bundle(vec![("worker", WORKER), ("main", MAIN)], "main")
        .expect("opt thunk and a normal relay preserve checked invocation provenance");
    let canonical = checked.canonical_source().expect("canonical checked source");
    let view = canonical.view();
    let role = view.deferred_task_role().expect("authenticated generic toolchain scheduler role");
    let task = view.type_definitions().get(role.task_name()).expect("retained Task declaration");
    let projection = role.poll_projection();
    assert_eq!(task.variants[projection.variant()].fields[projection.field()], *projection.signature(),
        "logical executable slot is authenticated by checked storage, not a physical field guess");
    let step = view.type_definitions().get(role.step_name()).expect("retained Step declaration");
    let ordinal = |name: &str| step.variants.iter().position(|variant|
        variant.name.rsplit('.').next() == Some(name)).expect("known scheduler semantic case");
    let done = ordinal("Done");
    let yielded = ordinal("Yield");
    let fork = ordinal("Fork");
    let frame = ordinal("Pull2Frame");
    assert_eq!(role.step_variants()[&done], vec![Role::Passive]);
    assert_eq!(role.step_variants()[&yielded], vec![Role::Recipe]);
    assert!(matches!(role.step_variants()[&fork].as_slice(),
        [Role::Recipe, Role::Invoke { result, .. }] if **result == Role::Recipe));
    assert!(matches!(&role.step_variants()[&frame][3],
        Role::Invoke { result, .. } if **result == Role::Passive));
    assert_eq!(role.step_variants()[&frame][4], Role::Recipe);

    let normal_task = Proof::Task(Mode::Normal, Box::new(Proof::Unknown));
    let step_value = |ordinal, fields| Proof::Step { role: role.step_identity().clone(),
        value: Box::new(Proof::Variant { ordinal, fields }) };
    assert_eq!(role.step_requirements(&step_value(done, vec![normal_task.clone()])), Some(vec![]),
        "completing with a Task as data does not execute it");
    assert_eq!(role.step_requirements(&step_value(yielded, vec![normal_task.clone()])),
        Some(vec![Requirement::Recipe(normal_task)]), "yield retains the actual normal execution requirement");
    assert!(role.step_requirements(&Proof::Unknown).is_none());
    let fork_requirements = role.step_requirements(&step_value(fork,
        vec![Proof::Unknown, Proof::Unknown])).expect("unknown values retain requirements");
    assert!(matches!(fork_requirements.as_slice(),
        [Requirement::Recipe(Proof::Unknown), Requirement::Invoke { callable, conventions, result }]
            if callable.invocation_contract(&vec![Proof::Unknown; conventions.len()]).is_none()
                && result == &Role::Recipe), "unknown dispatch is not an opt permission");
    let joined = step_value(done, vec![Proof::Unknown]).join(step_value(yielded, vec![Proof::Unknown]));
    assert_eq!(role.step_requirements(&joined), Some(vec![Requirement::Recipe(Proof::Unknown)]),
        "a passive alternative cannot erase executable work from another variant");
    assert!(role.step_requirements(&Proof::Step { role: role.task_identity().clone(),
        value: Box::new(Proof::Variant { ordinal: done, fields: vec![Proof::Unknown] }) }).is_none(),
        "a Task identity cannot authenticate a same-shaped Step");
    assert!(role.step_requirements(&step_value(done, vec![])).is_none());
    assert!(role.step_requirements(&Proof::Step { role: role.step_identity().clone(),
        value: Box::new(Proof::Variants(Default::default())) }).is_none());

    let contract = |name: &str| {
        let ordinal = view.module().items.iter().position(|item|
            matches!(item, Item::Function(function) if function.name == name)).expect("retained factory");
        view.task_behavior_at(ordinal as u32).expect("checked exact factory contract")
    };
    let normal = contract("main.normal_factory");
    let opt = contract("worker.factory");
    assert_eq!(normal.source_mode(), Mode::Normal);
    assert_eq!(opt.source_mode(), Mode::Opt);
    assert_eq!(normal.result(), opt.result(), "same opt Task result does not erase invocation mode");
    for (name, active) in [("task.ready_unit", false), ("task.yield_now", true)] {
        let Proof::Recipe { role: recipe_role, poll } = contract(name).result() else {
            panic!("{name} must export its actual checked polling value")
        };
        assert_eq!(recipe_role, role.task_identity());
        let invocation = poll.invocation_contract(&[]).expect("checked source poll callable");
        assert_eq!(invocation.execution_mode, Mode::Normal, "std wrapper is not an opt mode grant");
        let requirements = role.step_requirements(&invocation.result).expect("checked source Step result");
        assert_eq!(!requirements.is_empty(), active, "only yielded work has an execution obligation");
    }
    let lambda_contract = |name: &str| {
        let item = view.module().items.iter().position(|item|
            matches!(item, Item::Function(function) if function.name == name)).expect("final callable item");
        let mut matches = view.closure_witness().entries().iter().filter(|entry|
            entry.id().item == item as u32 && entry.id().method.is_none());
        let entry = matches.next().expect("checked lambda");
        assert!(matches.next().is_none(), "fixture selects an unambiguous lambda");
        (entry, entry.execution_contract().unwrap_or_else(|| panic!("missing final lambda behavior in {name}")))
    };
    let (thunk_witness, thunk) = lambda_contract("worker.compatible");
    assert!(view.module().items.iter().any(|item| matches!(item, Item::Function(function)
        if function.name == "worker.recursive_scalar")),
        "an unrelated recursive dependency remains present while the simple thunk is proved");
    assert_eq!(thunk.id(), thunk_witness.id());
    assert_eq!(thunk.source_mode(), Mode::Opt);
    assert!(thunk.captures().is_empty());
    let Proof::Recipe { role: task_role, poll } = thunk.result() else {
        panic!("generated thunk returns actual done recipe, not a type-derived opt grant: {:?}", thunk.result())
    };
    assert_eq!(task_role, role.task_identity());
    let invocation = poll.invocation_contract(&[]).expect("actual done poll behavior");
    assert_eq!(invocation.execution_mode, Mode::Normal);
    assert_eq!(role.step_requirements(&invocation.result), Some(vec![]));
    let Item::Function(factory) = &view.module().items[thunk.id().item as usize] else {
        panic!("thunk belongs to the checked factory")
    };
    let [witchy_syntax::ast::Stmt::Expr(witchy_syntax::ast::Expr::Call { name: wrapper, .. })] =
        factory.body.stmts.as_slice() else { panic!("checked async factory dispatch") };
    let (lazy_witness, lazy_poll) = lambda_contract(wrapper);
    assert_eq!(lazy_poll.source_mode(), Mode::Normal);
    assert_eq!(lazy_poll.captures(), lazy_witness.captures().iter()
        .map(|capture| capture.binding().clone()).collect::<Vec<_>>());
    let Proof::Invocation { callable, arguments } = lazy_poll.result() else {
        panic!("poll must preserve actual captured execution: {:?}", lazy_poll.result())
    };
    assert!(arguments.is_empty());
    let Proof::VariantSlot(recipe, variant, field) = callable.as_ref() else {
        panic!("poll callable must project the invoked thunk's returned recipe: {callable:?}")
    };
    assert_eq!((*variant, *field), (projection.variant(), projection.field()));
    let Proof::Invocation { callable: captured, arguments } = recipe.as_ref() else {
        panic!("recipe must be the actual thunk invocation: {recipe:?}")
    };
    assert!(arguments.is_empty());
    assert!(matches!(captured.as_ref(), Proof::Capture(owner, index)
        if owner == lazy_poll.id() && lazy_poll.captures().get(*index)
            .is_some_and(|binding| binding.name() == "thunk")));
}

#[test]
fn normal_thunk_returning_opt_work_cannot_cross_opt_callback_boundary() {
    for expression in ["normal_factory", "worker.Envelope(normal_factory).value"] {
        let main = format!("import task\nimport worker\n\nfn normal_factory() -> task.Task(Int):\n    worker.compatible()\n\nasync fn main(console: Console):\n    let value = worker.invoke({expression}).await\n    console.print(\"${{value}}\")\n");
        let error = source::checked_bundle(vec![("worker", WORKER), ("main", &main)], "main")
            .expect_err("returned opt work and opt-defined storage cannot upgrade a normal thunk");
        assert!(error.contains("opt-compatible") && error.contains("callable"), "{error}");
    }
}
