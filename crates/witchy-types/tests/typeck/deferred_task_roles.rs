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
