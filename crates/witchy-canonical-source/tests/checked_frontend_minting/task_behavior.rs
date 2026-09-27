use witchy_canonical_source::source::{
    SourceExecutionMode as Mode, TaskBehaviorIdentity, TaskCallableScope, TaskExecutionProof as Proof,
};
use witchy_syntax::ast::Convention;

fn factory(mode: Mode) -> Proof {
    Proof::DeclaredCallable { execution_mode: mode, conventions: Vec::new(),
        required_opt_inputs: Vec::new(), result: Box::new(Proof::Task(Mode::Opt, Box::new(Proof::Unknown))) }
}

#[test]
fn deferred_invocation_keeps_body_mode_separate_from_task_result() {
    let normal = factory(Mode::Normal);
    let opt = factory(Mode::Opt);
    let normal_call = normal.invocation_contract(&[]).expect("known normal factory");
    let opt_call = opt.invocation_contract(&[]).expect("known opt factory");
    assert_eq!(normal_call.result, opt_call.result, "both return opt work");
    assert_eq!(normal_call.execution_mode, Mode::Normal, "normal invocation cannot be relabeled by its result");
    assert_eq!(opt_call.execution_mode, Mode::Opt);
    assert!(normal.clone().join(opt.clone()).invocation_contract(&[]).is_none());
    assert!(Proof::Unknown.invocation_contract(&[]).is_none());
    assert!(opt.invocation_contract(&[Proof::Unknown]).is_none());

    let relay = Proof::Invocation { callable: Box::new(Proof::Parameter(0)), arguments: Vec::new() };
    for (callable, expected_mode) in [(normal, Mode::Normal), (opt, Mode::Opt)] {
        let instantiated = relay.instantiate(&[callable]);
        let Proof::Invocation { callable, arguments } = &instantiated else { panic!("invocation stays symbolic") };
        assert_eq!(callable.invocation_contract(arguments).unwrap().execution_mode, expected_mode);
        assert!(!matches!(instantiated, Proof::Task(Mode::Opt, _)));
    }
}

#[test]
fn recipe_substitution_retains_obligations_and_bounding_is_idempotent() {
    let scope = TaskCallableScope::new("callback".into(), 0);
    let callback = Proof::Callable { scope: scope.clone(), execution_mode: Mode::Opt, parameters: 1,
        required_opt_inputs: vec![Proof::CallableParameter(scope.clone(), 0)],
        result: Box::new(Proof::Awaited(Box::new(Proof::CallableParameter(scope, 0)))) };
    let normal_task = Proof::Task(Mode::Normal, Box::new(Proof::Unknown));
    let contract = callback.invocation_contract(&[normal_task.clone()]).unwrap();
    assert_eq!(contract.required_opt_inputs, vec![normal_task], "caller must discharge this; result is not permission");
    assert_eq!(contract.result, Proof::Unknown);
    let recipe = Proof::Recipe { role: TaskBehaviorIdentity::LinkedDeclaration([9; 32]), poll: Box::new(callback) };
    assert!(recipe.has_any_callable_input_obligation());
    assert_eq!(recipe.clone().bounded(1), Proof::UnprovedCallableObligation);
    for budget in [0, 1, 2, 3, 8, 32] {
        let bounded = recipe.clone().bounded(budget);
        assert_eq!(bounded.clone().bounded(budget), bounded);
    }
    assert!(recipe.invocation_contract(&[]).is_none(), "recipe is not callable or completed Task");

    let declared = Proof::DeclaredCallable { execution_mode: Mode::Opt, conventions: vec![Convention::Own],
        required_opt_inputs: vec![Proof::Parameter(0)], result: Box::new(Proof::Parameter(0)) };
    let outer = declared.instantiate(&[factory(Mode::Normal)]);
    assert_eq!(outer, declared, "outer substitution cannot bind a declared callable's own formals");
}
