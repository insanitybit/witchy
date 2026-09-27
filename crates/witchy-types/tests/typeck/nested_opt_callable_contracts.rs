//! Opt entry promises apply recursively to actual stored callable values.

const WORKER: &str = r#"mode opt

type Holder(a):
    value: a
    callback: fn(Int) -> Int

pub fn plus_one(value: Int) -> Int:
    value + 1

pub fn invoke(holder: Holder(a)) -> Int:
    (holder.callback)(41)

pub fn nested(pair: (Holder(Int), Int)) -> Int:
    (pair.0.callback)(41) + pair.1

pub fn relay(holder: Holder(Int)) -> Holder(Int):
    holder

pub fn early(flag: Bool, holder: Holder(Int)) -> Holder(Int):
    if flag:
        return holder
    holder

pub fn lambda_relay(holder: Holder(Int)) -> Holder(Int):
    let relay = fn(value: Holder(Int)) -> Holder(Int): value
    relay(holder)

type Route:
    Routed(Holder(Int))
    Empty

pub fn routed(route: Route) -> Int:
    match route:
        Routed(holder) -> (holder.callback)(41)
        Empty -> 0

pub fn choose(flag: Bool, holder: Holder(Int)) -> Route:
    if flag: Routed(holder) else: Empty

pub fn sequence(callbacks: List(fn(Int) -> Int)) -> Int:
    callbacks[0](41)

pub fn fallback(callback: Option(fn(Int) -> Int)) -> Int:
    let selected = callback ?? plus_one
    selected(41)
"#;

fn check(main: &str) -> Result<witchy_types::pipeline::CheckedModule, String> {
    super::source::checked_bundle(vec![("worker", WORKER), ("main", main)], "main")
}

#[test]
fn normal_caller_supplies_actual_opt_callback_through_generic_holder_and_tuple() {
    check(r#"import worker

fn main() -> Int:
    let holder = worker.Holder(7, worker.plus_one)
    worker.invoke(holder) + worker.nested((holder, 3))
"#).expect("typed stored slots preserve actual opt origin through a generic nominal and tuple");
}

#[test]
fn checked_opt_result_preserves_aggregate_callback_contract() {
    check(r#"import worker

fn main() -> Int:
    let holder = worker.lambda_relay(worker.early(true, worker.relay(worker.Holder(7, worker.plus_one))))
    worker.invoke(holder)
"#).expect("the opt return contract preserves its checked stored callable requirement");
}

#[test]
fn unsupported_sealed_transport_cannot_publish_a_stored_result_contract() {
    for body in [
        "    return hidden.holder\n",
        "    let relay = fn(value: Hidden) -> Holder(Int): value.holder\n    relay(hidden)\n",
    ] {
        let worker = format!("{WORKER}\nsealed type Hidden:\n    holder: Holder(Int)\n\npub fn hide(holder: Holder(Int)) -> Hidden:\n    Hidden(holder)\n\npub fn reveal(hidden: Hidden) -> Holder(Int):\n{body}");
        let error = super::source::checked_bundle(vec![("worker", &worker), ("main", r#"import worker

fn main() -> Int:
    worker.invoke(worker.reveal(worker.hide(worker.Holder(7, worker.plus_one))))
"#)], "main").err().expect("unsupported sealed transport cannot mint a stored callback proof on return");
        assert!(error.contains("opt-compatible callable"), "{error}");
    }
}

#[test]
fn sum_and_sequence_requirements_follow_stored_payloads() {
    check(r#"import worker

fn main() -> Int:
    worker.routed(worker.choose(true, worker.Holder(7, worker.plus_one))) + worker.sequence([worker.plus_one]) + worker.fallback(Some(worker.plus_one))
"#).expect("variant and sequence proofs authenticate their actual stored callback slots");
    for expression in [
        "worker.routed(worker.Routed(worker.Holder(7, ordinary)))",
        "worker.sequence([ordinary])",
        "worker.fallback(Some(ordinary))",
    ] {
        reject(&format!("import worker\n\nfn ordinary(value: Int) -> Int:\n    value + 1\n\nfn main() -> Int:\n    {expression}\n"));
    }
}

#[test]
fn record_updates_preserve_only_unchanged_authenticated_slots() {
    check(r#"import worker

fn main() -> Int:
    let holder = worker.Holder(7, worker.plus_one)
    worker.invoke(worker.Holder(value: 8, ..holder))
"#).expect("record update keeps the unchanged callback's actual origin");
    reject(r#"import worker

fn ordinary(value: Int) -> Int:
    value + 1

fn main() -> Int:
    let holder = worker.Holder(7, worker.plus_one)
    worker.invoke(worker.Holder(callback: ordinary, ..holder))
"#);
}

#[test]
fn ordinary_boundary_cannot_erase_a_nested_requirement_after_a_branch() {
    let error = check(r#"import worker

fn ordinary(value: Int) -> Int:
    value + 1

fn unchecked(callback: fn((worker.Holder(Int), Int)) -> Int) -> Int:
    callback((worker.Holder(7, ordinary), 0))

fn main() -> Int:
    unchecked(if true: worker.nested else: worker.nested)
"#).err().expect("a branch cannot hide the nested callable precondition");
    assert!(error.contains("cannot erase an opt-compatible callable"), "{error}");
}

fn reject(main: &str) {
    let error = check(main).err().expect("unknown or ordinary stored behavior cannot cross the opt entry");
    assert!(error.contains("opt-compatible callable"), "wrong refusal boundary: {error}");
}

#[test]
fn nominal_identity_does_not_authenticate_normal_callback() {
    for expression in [
        "worker.invoke(worker.Holder(7, ordinary))",
        "worker.invoke(worker.relay(worker.Holder(7, ordinary)))",
    ] {
        reject(&format!("import worker\n\nfn ordinary(value: Int) -> Int:\n    value + 1\n\nfn main() -> Int:\n    {expression}\n"));
    }
}

#[test]
fn mutation_and_shadowing_cannot_recover_an_old_stored_proof() {
    for body in [
        "    var holder = worker.Holder(7, worker.plus_one)\n    holder = worker.Holder(7, ordinary)\n    worker.invoke(holder)\n",
        "    let holder = worker.Holder(7, worker.plus_one)\n    let wrapped = (worker.Holder(7, ordinary), 0)\n    let (holder, _) = wrapped\n    worker.invoke(holder)\n",
    ] {
        reject(&format!("import worker\n\nfn ordinary(value: Int) -> Int:\n    value + 1\n\nfn main() -> Int:\n{body}"));
    }
}
