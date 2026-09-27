//! RFC-0167 opt higher-order contracts through the production source linker.

const WORKER: &str = r#"mode opt

pub fn plus_one(value: Int) -> Int:
    value + 1

pub fn relay(callback: fn(Int) -> Int) -> fn(Int) -> Int:
    callback
"#;

#[test]
fn normal_callable_cannot_enter_opt_higher_order_parameter() {
    let main = r#"import worker

fn ordinary(value: Int) -> Int:
    value + 1

fn main(console: Console):
    console.print("${worker.relay(ordinary)(41)}")
"#;
    let error = super::source::checked_bundle(vec![("worker", WORKER), ("main", main)], "main")
        .expect_err("normal function cannot satisfy opt callback precondition");
    assert!(
        error.to_string().contains("requires an opt-compatible callable"),
        "{error}"
    );
}

#[test]
fn imported_opt_body_keeps_call_scoped_copy_rule() {
    let worker = r#"mode opt

fn consume(value: String) -> Int:
    value.length()

pub fn inspect(let value: String) -> Int:
    consume(value)
"#;
    let main = "import worker\n\nfn main(console: Console):\n    let _ = worker.inspect(\"hello\")\n    console.print(\"done\")\n";
    let error = super::source::checked_bundle(vec![("worker", worker), ("main", main)], "main")
        .expect_err("an imported opt body cannot copy-repair call-scoped access");
    assert!(error.contains("call-scoped `let` parameter"), "{error}");

    let normal_worker = worker.replacen("mode opt\n\n", "", 1);
    super::source::checked_bundle(vec![("worker", &normal_worker), ("main", main)], "main")
        .expect("normal source may make the checked independent copy");
}

#[test]
fn opt_callback_origin_is_not_inferred_from_record_field_type() {
    let worker = r#"mode opt

type Holder:
    callback: fn(Int) -> Int

fn invoke(callback: fn(Int) -> Int) -> Int:
    callback(41)

pub fn through(holder: Holder) -> Int:
    invoke(holder.callback)
"#;
    let main = r#"import worker

fn ordinary(value: Int) -> Int:
    value + 1

fn main(console: Console):
    let holder = worker.Holder(ordinary)
    console.print("${worker.through(holder)}")
"#;
    let error = super::source::checked_bundle(vec![("worker", worker), ("main", main)], "main")
        .expect_err("a field type cannot authenticate its stored callback origin");
    assert!(error.contains("requires an opt-compatible callable"), "{error}");

    // The match binder shadows an authenticated parameter. Its field comes
    // from an ordinary caller, so the enclosing name cannot bless the result.
    let shadow_worker = r#"mode opt

type Holder:
    callback: fn(Int) -> Int

pub fn plus_one(value: Int) -> Int:
    value + 1

fn invoke(callback: fn(Int) -> Int) -> Int:
    callback(41)

pub fn through_shadow(callback: fn(Int) -> Int, holder: Holder) -> Int:
    let stored = match holder:
        Holder(callback) -> callback
    invoke(stored)
"#;
    let shadow_main = r#"import worker

fn ordinary(value: Int) -> Int:
    value + 1

fn main(console: Console):
    let holder = worker.Holder(ordinary)
    console.print("${worker.through_shadow(worker.plus_one, holder)}")
"#;
    let error = super::source::checked_bundle(
        vec![("worker", shadow_worker), ("main", shadow_main)], "main",
    ).expect_err("a match binder cannot inherit a shadowed outer callback proof");
    assert!(error.contains("requires an opt-compatible callable"), "{error}");

    let local_worker = r#"mode opt

type Holder:
    callback: fn(Int) -> Int

fn plus_one(value: Int) -> Int:
    value + 1

fn invoke(callback: fn(Int) -> Int) -> Int:
    callback(41)

pub fn local_through() -> Int:
    let holder = Holder(plus_one)
    invoke(holder.callback)
"#;
    let local_main = r#"import worker

fn main(console: Console):
    console.print("${worker.local_through()}")
"#;
    super::source::checked_bundle(vec![("worker", local_worker), ("main", local_main)], "main")
        .expect("a checked local record field retains its callback proof");
}

#[test]
fn opt_async_task_parameter_requires_opt_execution_origin() {
    let worker = r#"mode opt

import task

pub async fn execute(own recipe: task.Task(Int)) -> Int:
    recipe.await

pub async fn execute_indirect(own recipe: task.Task(Int)) -> Int:
    let relay = execute
    relay(recipe).await

pub async fn execute_local_lambda(own recipe: task.Task(Int)) -> Int:
    let relay = fn(own input: task.Task(Int)): execute(input)
    relay(recipe).await

pub async fn execute_nested_local_lambda(own recipe: task.Task(Int)) -> Int:
    let relay = fn(own input: task.Task(Int)):
        let nested = fn(): execute(input)
        nested()
    relay(recipe).await

pub fn relay_callback(callback: fn(own task.Task(Int)) -> task.Task(Int), own recipe: task.Task(Int)) -> task.Task(Int):
    callback(recipe)

pub fn relay_projected_callback(callback: fn(own task.Task(Int)) -> task.Task(Int), own recipe: task.Task(Int)) -> task.Task(Int):
    let pair = (callback, 0)
    pair.0(recipe)

pub fn pass_pair(own pair: (task.Task(Int), Int)) -> task.Task(Int):
    pair.0

pub fn relay_pair_callback(callback: fn(own (task.Task(Int), Int)) -> task.Task(Int), own pair: (task.Task(Int), Int)) -> task.Task(Int):
    callback(pair)

pub async fn compatible() -> Int:
    7

pub async fn execute_local_callback() -> Int:
    let make = fn(): compatible()
    let alias = make
    let first = alias().await
    let pair = (fn(): compatible(), 0)
    let second = pair.0().await
    first * 10 + second

pub async fn execute_parameter_callback() -> Int:
    let relay = fn(own recipe: task.Task(Int)): recipe
    let first = relay(compatible()).await
    let pair = (fn(own recipe: task.Task(Int)): recipe, 0)
    let second = pair.0(compatible()).await
    first * 10 + second

pub async fn execute_pair(own pair: (task.Task(Int), Int)) -> Int:
    let (recipe, _) = pair
    recipe.await

pub async fn execute_nested_pair(own pair: ((task.Task(Int), Int), Int)) -> Int:
    pair.0.0.await

pub async fn nested() -> task.Task(Int):
    compatible()

pub async fn execute_nested() -> Int:
    let inner = nested().await
    inner.await

pub async fn execute_option(own recipe: Option(task.Task(Int))) -> Option(Int):
    match recipe:
        Some(recipe_value) -> Some(recipe_value.await)
        None -> None

pub async fn execute_try(own recipe: Option(task.Task(Int))) -> Option(Int):
    let recipe_value = recipe?
    Some(recipe_value.await)

pub async fn execute_conditional(choose: Bool) -> Option(Int):
    let wrapped = if choose: Some(compatible()) else: None
    match wrapped:
        Some(recipe_value) -> Some(recipe_value.await)
        None -> None
"#;
    // Rejection cases need only the producer and relay being rejected. The
    // complete worker is checked once below for the positive composition.
    let entry_worker = r#"mode opt
import task

pub async fn execute(own recipe: task.Task(Int)) -> Int:
    recipe.await

pub fn relay_callback(callback: fn(own task.Task(Int)) -> task.Task(Int), own recipe: task.Task(Int)) -> task.Task(Int):
    callback(recipe)

pub async fn execute_option(own recipe: Option(task.Task(Int))) -> Option(Int):
    match recipe:
        Some(recipe_value) -> Some(recipe_value.await)
        None -> None

pub async fn execute_try(own recipe: Option(task.Task(Int))) -> Option(Int):
    let recipe_value = recipe?
    Some(recipe_value.await)
"#;
    let main = r#"import worker

async fn ordinary() -> Int:
    7

async fn main(console: Console):
    let answer = worker.execute(ordinary()).await
    console.print("${answer}")
"#;
    let result = super::source::checked_bundle(vec![("worker", entry_worker), ("main", main)], "main");
    let error = result.expect_err("a normal recipe cannot acquire opt execution provenance at the call site");
    assert!(error.contains("opt-compatible task value provenance"), "{error}");

    let normal_public_callback_main = r#"import worker

async fn ordinary() -> Int:
    7

async fn main(console: Console):
    let _ = worker.relay_callback(worker.execute, ordinary()).await
    console.print("unreachable")
"#;
    let error = super::source::checked_bundle(
        vec![("worker", entry_worker), ("main", normal_public_callback_main)], "main",
    ).expect_err("an opt callback formal cannot receive a normal task through a public relay");
    assert!(error.contains("opt-compatible task value provenance"), "{error}");

    let normal_option_main = r#"import worker

async fn ordinary() -> Int:
    7

async fn main(console: Console):
    let _ = worker.execute_try(Some(ordinary())).await
    let _ = worker.execute_option(Some(ordinary())).await
    console.print("done")
"#;
    let error = super::source::checked_bundle(
        vec![("worker", entry_worker), ("main", normal_option_main)], "main",
    ).expect_err("a sum wrapper cannot erase normal task execution mode");
    assert!(error.contains("opt-compatible task value provenance"), "{error}");

    let compatible_main = r#"import task
import worker

fn public_callback_recipe() -> task.Task(Int):
    worker.relay_callback(worker.execute, worker.compatible())

fn projected_callback_recipe() -> task.Task(Int):
    worker.relay_projected_callback(worker.execute, worker.compatible())

fn pair_callback_recipe() -> task.Task(Int):
    worker.relay_pair_callback(worker.pass_pair, (worker.compatible(), 0))

fn identity(own recipe: task.Task(Int)) -> task.Task(Int):
    recipe

fn choose(flag: Bool, left: fn(own task.Task(Int)) -> task.Task(Int), right: fn(own task.Task(Int)) -> task.Task(Int)) -> fn(own task.Task(Int)) -> task.Task(Int):
    if flag: left else: right

async fn main(console: Console):
    let answer = worker.execute(worker.compatible()).await
    let indirect_answer = worker.execute_indirect(worker.compatible()).await
    let local_lambda_answer = worker.execute_local_lambda(worker.compatible()).await
    let nested_local_lambda_answer = worker.execute_nested_local_lambda(worker.compatible()).await
    let public_callback_answer = public_callback_recipe().await
    let projected_callback_answer = projected_callback_recipe().await
    let pair_callback_answer = pair_callback_recipe().await
    let pair_answer = worker.execute_pair((worker.compatible(), 1)).await
    let nested_pair_answer = worker.execute_nested_pair(((worker.compatible(), 1), 2)).await
    let inner = worker.nested().await
    let nested_answer = inner.await
    let opt_nested_answer = worker.execute_nested().await
    let opt_option_answer = worker.execute_option(Some(worker.compatible())).await
    let opt_try_answer = worker.execute_try(Some(worker.compatible())).await
    let opt_conditional_answer = worker.execute_conditional(true).await
    let opt_local_callback_answer = worker.execute_local_callback().await
    let opt_parameter_callback_answer = worker.execute_parameter_callback().await
    let normal_relay = choose(true, identity, identity)
    let normal_callback_answer = normal_relay(task.done(7)).await
    console.print("${answer}")
    console.print("${indirect_answer}")
    console.print("${local_lambda_answer}")
    console.print("${nested_local_lambda_answer}")
    console.print("${public_callback_answer}")
    console.print("${projected_callback_answer}")
    console.print("${pair_callback_answer}")
    console.print("${pair_answer}")
    console.print("${nested_pair_answer}")
    console.print("${nested_answer}")
    console.print("${opt_nested_answer}")
    console.print("${opt_option_answer}")
    console.print("${opt_try_answer}")
    console.print("${opt_conditional_answer}")
    console.print("${opt_local_callback_answer}")
    console.print("${opt_parameter_callback_answer}")
    console.print("${normal_callback_answer}")
"#;
    let result = super::source::checked_bundle(vec![("worker", worker), ("main", compatible_main)], "main");
    assert!(result.is_ok(), "{result:?}");

    let normal_callback_worker = worker.replace(
        "let make = fn(): compatible()",
        "let make = fn(): task.done(7)",
    );
    let error = super::source::checked_bundle(
        vec![("worker", &normal_callback_worker), ("main", compatible_main)], "main",
    ).expect_err("a local callback cannot upgrade a normal task producer");
    assert!(error.contains("opt-compatible task value provenance"), "{error}");

    let normal_argument_worker = worker.replace(
        "relay(compatible()).await",
        "relay(task.done(7)).await",
    );
    let error = super::source::checked_bundle(
        vec![("worker", &normal_argument_worker), ("main", compatible_main)], "main",
    ).expect_err("a callback formal cannot upgrade a normal task argument");
    assert!(error.contains("opt-compatible task value provenance"), "{error}");

    let escaped_lambda_worker = format!("{worker}\n\npub fn escape_lambda() -> fn(own task.Task(Int)) -> task.Task(Int):\n    let relay = fn(own input: task.Task(Int)): execute(input)\n    relay\n");
    let error = super::source::checked_bundle(
        vec![("worker", &escaped_lambda_worker), ("main", compatible_main)], "main",
    ).expect_err("a lambda input obligation cannot escape through an unchecked function type");
    assert!(error.contains("local callback with an opt task input obligation"), "{error}");

    let joined_lambda_worker = format!("{worker}\n\npub async fn join_lambda(flag: Bool) -> Int:\n    let relay = if flag: fn(own input: task.Task(Int)): execute(input) else: fn(own input: task.Task(Int)): execute(input)\n    relay(compatible()).await\n");
    let error = super::source::checked_bundle(
        vec![("worker", &joined_lambda_worker), ("main", compatible_main)], "main",
    ).expect_err("joining different local lambda scopes cannot erase their opt input obligation");
    assert!(error.contains("local callback with an opt task input obligation"), "{error}");

    let aggregate_worker = format!("{worker}\n\npub fn factory() -> (fn(own task.Task(Int)) -> task.Task(Int), Int):\n    (execute, 0)\n");
    let aggregate_main = r#"import task
import worker

async fn main(console: Console):
    let pair = worker.factory()
    let noop = fn(): 0
    let _ = noop()
    let (relay, _) = pair
    let _ = relay(task.done(7)).await
    console.print("unreachable")
"#;
    let error = super::source::checked_bundle(
        vec![("worker", &aggregate_worker), ("main", aggregate_main)], "main",
    ).expect_err("invalidation cannot erase an opt task input obligation inside a tuple");
    assert!(error.contains("local callback with an opt task input obligation"), "{error}");

    let opaque_callback_main = r#"import task
import worker

fn relay(callback: fn(own task.Task(Int)) -> task.Task(Int), own recipe: task.Task(Int)) -> task.Task(Int):
    callback(recipe)

async fn main(console: Console):
    let pair = (worker.execute, 0)
    let _ = relay(pair.0, task.done(7)).await
    console.print("unreachable")
"#;
    let error = super::source::checked_bundle(
        vec![("worker", entry_worker), ("main", opaque_callback_main)], "main",
    ).expect_err("an ordinary function-value ABI cannot erase an opt task-input requirement");
    assert!(error.contains("callback with an opt task input obligation"), "{error}");

    let propagated_callback_worker = format!("{worker}\n\npub fn choose(flag: Bool) -> Result(Int, fn(own task.Task(Int)) -> task.Task(Int)):\n    if flag: Ok(1) else: Err(execute)\n\npub fn propagate(flag: Bool) -> Result(Int, fn(own task.Task(Int)) -> task.Task(Int)):\n    let value = choose(flag)?\n    Ok(value)\n");
    let error = super::source::checked_bundle(
        vec![("worker", &propagated_callback_worker), ("main", compatible_main)], "main",
    ).expect_err("early error propagation cannot hide a callback task-input obligation");
    assert!(error.contains("propagate through `?`"), "{error}");
}
