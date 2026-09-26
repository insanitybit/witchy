//! RFC-0167 row K: checker-only resource, callable-effect, erased-payload,
//! and suspension composition contracts.

use super::source::checked;

fn assert_rejected(label: &str, source: &str, needles: &[&str]) {
    let error = checked(source).expect_err(label).to_lowercase();
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

        assert_rejected(
            "a replayable recipe cannot capture unique storage hidden in a record",
            &format!("{mode}type Envelope:\n    values: unique List(Int)\n\nasync fn child(own envelope: Envelope) -> Int:\n    envelope.values.length()\n\nasync fn main():\n    let answer = child(Envelope([1])).await\n"),
            &["affine unique storage", "replayable Task recipe"],
        );

        assert_rejected(
            "a lazy recipe cannot retain call-scoped access",
            &format!("{mode}async fn child(let items: List(Int)) -> Int:\n    items.length()\n\nasync fn main():\n    let answer = child([1]).await\n"),
            &["call-scoped `let` parameter", "lazy Task recipe"],
        );
    }
}
