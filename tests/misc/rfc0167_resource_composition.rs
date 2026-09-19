//! RFC-0167 row K: resources, callable effects, erased payloads, and
//! suspension retain one composition contract in normal and opt source.
//!
//! The executable evidence is the RFC-0167 triad: an independent expected
//! result, the canonical SafeWIR interpreter, and admitted unoptimized Wasm.
//! The source tree-walker interpreter is deliberately not an acceptance oracle.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, opt};
use witchy_wir::canonical_interpreter::{interpret_program, InterpreterValue};
use witchy_wir::canonical_safewir::FunctionId;

struct OptReset;

impl Drop for OptReset {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

const COMPOSED_SUCCESS: &str = r#"
must sealed type Completion:
    Completion(pure once fn(Int) -> Int)

fn finish(own completion: Completion, value: Int) -> Int:
    match completion:
        Completion(callback) -> callback(value)

fn main(console: Console):
    let completion = Completion(pure once fn(value: Int): value + 1)
    console.print("${finish(completion, 41)}")
"#;

fn checked(source: &str) -> witchy_types::pipeline::CheckedModule {
    witchy::resolve_std_only_checked(source)
        .unwrap_or_else(|error| panic!("RFC-0167 resource composition must check: {error}"))
}

fn assert_rejected(label: &str, source: &str, needles: &[&str]) {
    let error = witchy::resolve_std_only_checked(source)
        .expect_err(label)
        .to_string()
        .to_lowercase();
    assert!(
        needles.iter().any(|needle| error.contains(&needle.to_lowercase())),
        "{label} must fail at its composed contract boundary with one of {needles:?}: {error}",
    );
}

fn assert_composed_success(label: &str, source: &str, expected: &[&str]) {
    let checked = checked(source);
    let expected: Vec<String> = expected.iter().map(|line| (*line).to_owned()).collect();
    let (artifact, oracle) = codegen::compile_checked_module_artifact_with_observer(
        &checked,
        |validated, artifact| {
            let entry = artifact.entry().expect("resource composition entry");
            let function = FunctionId::new(
                entry
                    .export()
                    .strip_prefix('f')
                    .expect("numeric entry export")
                    .parse()
                    .expect("numeric entry function"),
            );
            let arguments = entry
                .capabilities()
                .iter()
                .copied()
                .map(InterpreterValue::entry_capability)
                .collect();
            interpret_program(validated, function, arguments, Default::default())
                .unwrap_or_else(|error| panic!("{label} canonical SafeWIR execution: {error}"))
        },
    )
    .expect_lowered(&format!("{label} resource composition admits through canonical SafeWIR"));

    assert_eq!(
        oracle.output, expected,
        "{label} canonical SafeWIR matches the independent expectation",
    );
    assert_eq!(
        oracle.leaked_objects, 0,
        "{label} canonical SafeWIR releases retained roots",
    );

    let mut runtime = Runtime::batch().expect("create compiled runtime");
    let mut actor = runtime
        .spawn_admitted(
            &artifact,
            Capabilities {
                print: true,
                print_int: true,
                quiet: true,
                ..Capabilities::none()
            },
            128,
        )
        .unwrap_or_else(|error| panic!("{label} admitted Wasm spawn: {error}"));
    actor
        .run()
        .unwrap_or_else(|error| panic!("{label} admitted Wasm execution: {error}"));
    assert_eq!(
        actor.output(), expected,
        "{label} admitted unoptimized Wasm matches the independent expectation",
    );
    assert_eq!(
        actor.canonical_heap_leaks().expect("canonical heap audit"),
        Some(0),
        "{label} admitted Wasm releases retained roots",
    );
}

#[test]
fn must_pure_once_composition_matches_canonical_interpreter_and_unoptimized_wasm_in_both_modes() {
    let _reset = OptReset;
    opt::set_for_tests(Some(opt::OptSet::none()));
    assert_eq!(
        opt::active_schema_key().enabled_mask(),
        0,
        "resource composition uses the unoptimized Wasm baseline",
    );

    for (label, source) in [
        ("normal", COMPOSED_SUCCESS.to_owned()),
        ("opt", format!("mode opt\n\n{COMPOSED_SUCCESS}")),
    ] {
        assert_composed_success(label, &source, &["42"]);
    }
}

#[test]
fn linked_std_callbacks_do_not_inherit_the_entry_opt_contract() {
    checked(
        "mode opt\n\nfn main(console: Console):\n    let mapped = list.map([41], fn(value): value + 1)\n    console.print(\"${list.at(mapped, 0)}\")\n",
    );

    assert_rejected(
        "an entry-owned callback boundary rejects a normal std callback",
        "mode opt\n\nimport math\n\nfn apply(callback: fn(Int) -> Int, value: Int) -> Int:\n    callback(value)\n\nfn main():\n    apply(math.abs, -41)\n",
        &["requires an opt-compatible callable"],
    );

    checked(
        "mode opt\n\nfn apply(callback: fn(Int) -> Int, value: Int) -> Int:\n    callback(value)\n\nfn main():\n    apply(fn(value): value + 1, 41)\n",
    );

    let forged = witchy::parser::parse_module(
        "@__compiler_linked_source_module\nfn forged() -> Int:\n    1\n",
    )
    .expect_err("source cannot forge linker callable provenance");
    assert!(
        forged.message.contains("unknown declaration attribute"),
        "{forged:?}"
    );
}

#[test]
fn qualifier_erasure_cannot_hide_must_once_or_capability_contracts() {
    for mode in ["", "mode opt\n\n"] {
        assert_rejected(
            "pure once callable cannot capture effect authority",
            &format!(
                "{mode}fn main(console: Console):\n    let callback: pure once fn() -> () = pure once fn(): console.print(\"hidden\")\n    callback()\n"
            ),
            &["pure", "console", "capability"],
        );

        assert_rejected(
            "Dynamic cannot erase a must/once obligation",
            &format!(
                "{mode}import dynamic\n\nmust sealed type Completion:\n    Completion(once fn() -> ())\n\nfn main():\n    let hidden = dynamic.dynamic(Completion(once fn(): ()))\n"
            ),
            &["reflect", "must", "once", "dynamic"],
        );

        assert_rejected(
            "Dynamic cannot erase capability authority",
            &format!(
                "{mode}import dynamic\n\nfn main(console: Console):\n    let hidden = dynamic.dynamic(console)\n"
            ),
            &["capability", "console", "dynamic", "reflect"],
        );

        assert_rejected(
            "generic duplication cannot erase invocation cardinality",
            &format!(
                "{mode}fn duplicate(value: a) -> (a, a):\n    (value, value)\n\nfn main():\n    let callback: once fn() -> Int = once fn(): 1\n    let pair = duplicate(callback)\n"
            ),
            &["once", "affine", "copy", "move"],
        );
    }
}

#[test]
fn replayable_async_recipe_cannot_hide_must_or_once_captures() {
    // The source rule is mode-independent: mode opt must reach these same
    // replayable-task boundaries rather than failing in linked async support.
    for mode in ["", "mode opt\n\n"] {
        assert_rejected(
            "lazy task recipe cannot hide a must capture",
            &format!(
                "{mode}import task\n\nmust type Ticket:\n    Ticket(Int)\n\nfn finish(own ticket: Ticket):\n    match ticket:\n        Ticket(_) -> ()\n\nasync fn worker(own ticket: Ticket):\n    finish(ticket)\n\nfn main():\n    ()\n"
            ),
            &["must-consume", "lazy task", "replayable"],
        );

        assert_rejected(
            "affine once callable cannot enter a replayable task recipe",
            &format!(
                "{mode}import task\n\nasync fn worker(own callback: once fn() -> Int) -> Int:\n    callback()\n\nfn main():\n    ()\n"
            ),
            &["once-callable", "replayable", "task recipe", "affine"],
        );
    }
}

#[test]
fn cancellation_cannot_drop_live_must_or_once_task_captures() {
    // A spawned task has a cancellation handle, unlike the compiler-generated
    // root task. These sources deliberately construct that handle and cancel
    // it, so the refusal is tied to the cancellable frame rather than merely
    // to an isolated async declaration. The ordinary composition witness above
    // remains the executable interpreter/admitted-Wasm control: neither
    // cancellation nor this rejection adds a backend-specific cleanup path.
    for mode in ["", "mode opt\n\n"] {
        assert_rejected(
            "cancellation cannot abandon a live must capture",
            &format!("{mode}import chan\n\nasync fn leaf():\n    chan.yield_now().await\n\nasync fn supervisor():\n    let nested = chan.spawn(leaf()).await\n    chan.yield_now().await\n    chan.join(nested).await\n\nasync fn main():\n    let parent = chan.spawn(supervisor()).await\n    chan.cancel(parent).await\n"),
            &[
                "must-consume `nested` remains live across `await`",
                "spawned tasks are shallow-cancellable",
            ],
        );

        assert_rejected(
            "cancellation cannot capture an affine once callback",
            &format!("{mode}import chan\n\nasync fn child(own callback: once fn() -> ()):\n    chan.yield_now().await\n    callback()\n\nasync fn main():\n    let parent = chan.spawn(child(once fn(): ())).await\n    chan.cancel(parent).await\n"),
            &["once-callable", "replayable Task recipe", "affine"],
        );
    }
}
