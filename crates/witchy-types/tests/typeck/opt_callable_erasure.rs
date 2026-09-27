//! Fact-losing source boundaries cannot forget opt entry preconditions.
const WORKER: &str = r#"mode opt

type Holder:
    callback: fn(Int) -> Int

pub fn plus_one(value: Int) -> Int:
    value + 1

pub fn apply(callback: fn(Int) -> Int) -> Int:
    callback(41)

pub fn invoke(holder: Holder) -> Int:
    (holder.callback)(41)
"#;

const NORMAL: &str = r#"import worker

fn ordinary(value: Int) -> Int:
    value + 1

fn plain(callback: fn(Int) -> Int) -> Int:
    callback(41)

fn plain_holder(holder: worker.Holder) -> Int:
    (holder.callback)(41)
"#;

fn check(body: &str) -> Result<witchy_types::pipeline::CheckedModule, String> {
    let main = format!("{NORMAL}\nfn main() -> Int:\n{body}");
    check_source(&main)
}

fn check_source(main: &str) -> Result<witchy_types::pipeline::CheckedModule, String> {
    super::source::checked_bundle(vec![("worker", WORKER), ("main", &main)], "main")
}

fn reject_all(bodies: &[&str]) {
    let mut accepted = Vec::new();
    for body in bodies {
        match check(body) {
            Ok(_) => accepted.push(*body),
            Err(error) => assert!(error.contains("cannot erase an opt-compatible callable"), "{error}"),
        }
    }
    assert!(accepted.is_empty(), "source boundary erased opt preconditions in: {accepted:?}");
}

#[test]
fn mutable_creation_cannot_erase_direct_or_nested_opt_preconditions() {
    reject_all(&[
        "    var relay = worker.apply\n    relay(ordinary)\n",
        "    var relay = worker.invoke\n    relay(worker.Holder(ordinary))\n",
    ]);
}

#[test]
fn mutable_replacement_cannot_erase_direct_or_nested_opt_preconditions() {
    reject_all(&[
        "    var relay = plain\n    relay = worker.apply\n    relay(ordinary)\n",
        "    var relay = plain_holder\n    relay = worker.invoke\n    relay(worker.Holder(ordinary))\n",
    ]);
}

#[test]
fn immutable_opt_and_mutable_leaf_or_normal_callbacks_remain_legal() {
    check("    let relay = worker.apply\n    var leaf = worker.plus_one\n    var ordinary_relay = plain\n    relay(worker.plus_one) + leaf(41) + ordinary_relay(ordinary)\n")
        .expect("only a lost higher-order precondition requires refusal");
}

#[test]
fn branch_join_cannot_hide_possible_opt_preconditions() {
    reject_all(&[
        "    let relay = if true:\n        worker.apply\n    else:\n        plain\n    relay(ordinary)\n",
        "    var relay = if true:\n        plain_holder\n    else:\n        worker.invoke\n    relay(worker.Holder(ordinary))\n",
    ]);
}

#[test]
fn ordinary_callable_results_cannot_erase_opt_preconditions() {
    let declarations = [
        "fn publish() -> fn(fn(Int) -> Int) -> Int:\n    worker.apply\n",
        "fn publish() -> fn(fn(Int) -> Int) -> Int:\n    return worker.apply\n",
        "fn publish() -> fn(fn(Int) -> Int) -> Int:\n    let make = fn():\n        worker.apply\n    make()\n",
    ];
    let mut accepted = Vec::new();
    for declaration in declarations {
        let main = format!("{NORMAL}\n{declaration}\nfn main() -> Int:\n    let relay = publish()\n    relay(ordinary)\n");
        match check_source(&main) {
            Ok(_) => accepted.push(declaration),
            Err(error) => assert!(error.contains("cannot erase an opt-compatible callable"), "{error}"),
        }
    }
    assert!(accepted.is_empty(), "ordinary results erased preconditions: {accepted:?}");
}

#[test]
fn leaf_joins_and_ordinary_leaf_results_remain_legal() {
    let main = format!("{NORMAL}\nfn leaf() -> fn(Int) -> Int:\n    worker.plus_one\n\nfn main() -> Int:\n    let choice = if true:\n        worker.plus_one\n    else:\n        ordinary\n    let make = fn():\n        ordinary\n    choice(41) + leaf()(41) + make()(41)\n");
    check_source(&main).expect("leaf origin loss carries no higher-order precondition");
}

#[test]
fn closure_snapshot_preserves_actual_origin_without_blessing_normal_values() {
    let error = check("    let relay = worker.apply\n    let invoke = fn():\n        relay(ordinary)\n    invoke()\n")
        .expect_err("rebinding a captured HOF must preserve its precondition");
    assert!(error.contains("opt-compatible callable"), "{error}");
    check("    let relay = worker.apply\n    let normal = plain\n    let invoke = fn():\n        relay(worker.plus_one) + normal(ordinary)\n    invoke()\n")
        .expect("snapshot preserves each actual source contract, including ordinary absence");
}
