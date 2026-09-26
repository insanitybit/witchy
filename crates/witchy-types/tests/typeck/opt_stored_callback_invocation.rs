//! A nominal module does not authenticate the executable behavior in its fields.

const WORKER: &str = r#"mode opt

type Holder:
    callback: fn(Int) -> Int

pub fn plus_one(value: Int) -> Int:
    value + 1
"#;

const ORDINARY: &str = r#"import worker

fn ordinary(value: Int) -> Int:
    value + 1
"#;

fn check(worker: &str, main: &str) -> Result<witchy_types::pipeline::CheckedModule, String> {
    super::source::checked_bundle(vec![("worker", worker), ("main", main)], "main")
}

#[test]
fn normal_code_may_construct_opt_holder_and_invoke_its_normal_callback() {
    let main = format!("{ORDINARY}\nfn main() -> Int:\n    let holder = worker.Holder(ordinary)\n    let callback = holder.callback\n    callback(41)\n");
    check(WORKER, &main).expect("normal invocation does not require opt provenance from the holder type");
}

#[test]
fn locally_authenticated_opt_holder_callback_may_be_invoked() {
    let worker = format!("{WORKER}\npub fn through() -> Int:\n    let holder = Holder(plus_one)\n    let callback = holder.callback\n    callback(41) + (holder.callback)(41)\n");
    let main = "import worker\n\nfn main() -> Int:\n    worker.through()\n";
    check(&worker, main).expect("both invocation forms preserve a locally authenticated opt callback");
}

#[test]
fn opt_formals_results_and_branch_block_values_keep_invocation_proof() {
    let worker = format!("{WORKER}\n{}", r#"pub fn relay(callback: fn(Int) -> Int) -> fn(Int) -> Int:
    callback

pub fn through(choose: Bool) -> Int:
    let returned = relay(plus_one)
    let selected = if choose:
        let local = plus_one
        local
    else:
        plus_one
    returned(41) + selected(41) + (if choose: plus_one else: plus_one)(41)
"#);
    let main = "import worker\n\nfn main() -> Int:\n    worker.through(true) + worker.relay(worker.plus_one)(41)\n";
    check(&worker, main).expect("checked formals, returned callbacks and inferred branch/block proofs remain invocable");
}

fn unknown_holder_invocation_must_reject(body: &str) {
    let worker = format!("{WORKER}\npub fn through(holder: Holder) -> Int:\n{body}\n");
    let main = format!("{ORDINARY}\nfn main() -> Int:\n    worker.through(worker.Holder(ordinary))\n");
    let error = check(&worker, &main)
        .err().expect("an opt body must not invoke an unauthenticated callback from a nominal field");
    assert!(error.contains("opt-compatible callable"), "wrong refusal boundary: {error}");
}

#[test]
fn opt_body_cannot_invoke_unknown_holder_callback_through_unannotated_alias() {
    unknown_holder_invocation_must_reject("    let callback = holder.callback\n    callback(41)");
}

#[test]
fn opt_body_cannot_directly_apply_unknown_holder_callback() {
    unknown_holder_invocation_must_reject("    (holder.callback)(41)");
}

#[test]
fn tuple_projection_preserves_only_checked_callback_slots() {
    let worker = format!("{WORKER}\npub fn through() -> Int:\n    let pair = (0, (plus_one, 1))\n    pair.1.0(41)\n");
    let main = "import worker\n\nfn main() -> Int:\n    worker.through()\n";
    check(&worker, main).expect("nested tuple projection carries its checked callback slot");

    let worker = format!("{WORKER}\npub fn through(pair: (Int, (fn(Int) -> Int, Int))) -> Int:\n    pair.1.0(41)\n");
    let main = format!("{ORDINARY}\nfn main() -> Int:\n    worker.through((0, (ordinary, 1)))\n");
    let error = check(&worker, &main).err().expect("a tuple type cannot authenticate an unknown callback slot");
    assert!(error.contains("opt-compatible callable"), "{error}");
}
