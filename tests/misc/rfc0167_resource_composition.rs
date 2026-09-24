//! RFC-0167 row K: resources, callable effects, erased payloads, and
//! suspension retain one composition contract in normal and opt source.
//!
//! The executable evidence is the RFC-0167 triad: an independent expected
//! result, the canonical SafeWIR interpreter, and admitted unoptimized Wasm.
//! The source tree-walker interpreter is deliberately not an acceptance oracle.

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
