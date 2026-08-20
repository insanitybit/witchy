use super::*;

fn must_error(source: &str) -> String {
    let linked = try_link_std(source).expect("resource fixture links");
    typeck::check(&linked)
        .expect_err("resource lifecycle misuse must be rejected")
        .message
}

#[test]
fn transaction_resource_consumes_success_conflict_rollback_moves_aggregates_and_cfg_early_return_on_wasm() {
    let source = r#"
import transaction
from transaction import Transaction, CommitError

type Batch:
    Batch(Transaction)

fn finish_batch(own batch: Batch) -> String:
    match batch:
        Batch(pending) -> transaction.rollback(pending)

fn resolve(flag: Bool) -> String:
    let pending = transaction.begin("old", "new", 7)
    if flag:
        match transaction.commit(pending, 7):
            Ok(value) -> value
            Err(_error) -> "unexpected"
    else:
        transaction.rollback(pending)

fn resolve_early(rollback: Bool) -> String:
    let pending = transaction.begin("early-old", "early-new", 5)
    if rollback:
        let original = transaction.rollback(pending)
        return original
    match transaction.commit(pending, 5):
        Ok(value) -> value
        Err(_error) -> "unexpected"

fn main(console: Console):
    let first = transaction.begin("move-old", "move-new", 2)
    let transferred = move first
    match transaction.commit(transferred, 2):
        Ok(value) -> console.print(value)
        Err(_error) -> console.print("unexpected")

    let conflicted = transaction.commit(transaction.begin("conflict-old", "conflict-new", 4), 9)
    match conflicted:
        Ok(_value) -> console.print("unexpected")
        Err(Conflict(original, expected, actual)) -> console.print("${original}:${expected}:${actual}")

    let batch = Batch(transaction.begin("batch-old", "batch-new", 1))
    console.print(finish_batch(batch))
    console.print(resolve(true))
    console.print(resolve(false))
    console.print(resolve_early(true))
    console.print(resolve_early(false))
"#;

    let expected = vec![
        "move-new",
        "conflict-old:4:9",
        "batch-old",
        "new",
        "old",
        "early-old",
        "early-new",
    ];
    assert_eq!(run_on_wasm(source), expected, "compiled Wasm resource lifecycle");
    assert_eq!(link_run(source), expected, "static obligation erases before backend execution");
}

#[test]
fn transaction_resource_rejects_lifecycle_loss_on_scope_branch_move_and_aggregate_paths() {
    let scope = must_error(
        "import transaction\n\nfn main():\n    let pending = transaction.begin(\"old\", \"new\", 1)\n",
    );
    assert!(scope.contains("must-consume value `pending`"), "{scope}");

    let branch = must_error(
        "import transaction\n\nfn run(commit: Bool):\n    let pending = transaction.begin(\"old\", \"new\", 1)\n    if commit:\n        let _ = transaction.commit(pending, 1)\n\nfn main():\n    run(true)\n",
    );
    assert!(branch.contains("must-consume value `pending`"), "{branch}");

    let early = must_error(
        "import transaction\n\nfn run(stop: Bool) -> String:\n    let pending = transaction.begin(\"old\", \"new\", 1)\n    if stop:\n        return \"lost\"\n    transaction.rollback(pending)\n\nfn main():\n    let _ = run(true)\n",
    );
    assert!(early.contains("return leaves must-consume value `pending` undisposed"), "{early}");

    let copied = must_error(
        "import transaction\n\nfn main():\n    let first = transaction.begin(\"old\", \"new\", 1)\n    let copied = first\n    let _ = transaction.rollback(copied)\n",
    );
    assert!(copied.contains("would copy must-consume value `first`"), "{copied}");

    let aggregate = must_error(
        "import transaction\nfrom transaction import Transaction\n\ntype Batch:\n    Batch(Transaction)\n\nfn main():\n    let batch = Batch(transaction.begin(\"old\", \"new\", 1))\n",
    );
    assert!(aggregate.contains("must-consume value `batch`"), "{aggregate}");
}

#[test]
fn transaction_resource_disposes_before_normal_and_error_exit_on_wasm() {
    let source = r#"
import transaction
from transaction import CommitError

fn validate(ok: Bool) -> Result(String, String):
    if ok:
        Ok("validated")
    else:
        Err("rejected")

fn rollback_then_validate(ok: Bool) -> Result(String, String):
    let original = transaction.rollback(transaction.begin("rollback-old", "rollback-new", 3))
    let _validated = validate(ok)?
    Ok(original)

fn commit_or_conflict(actual_revision: Int) -> Result(String, CommitError):
    let published = transaction.commit(transaction.begin("commit-old", "commit-new", 4), actual_revision)?
    Ok(published)

fn main(console: Console):
    match rollback_then_validate(true):
        Ok(value) -> console.print("normal:${value}")
        Err(error) -> console.print("unexpected:${error}")
    match rollback_then_validate(false):
        Ok(value) -> console.print("unexpected:${value}")
        Err(error) -> console.print("error:${error}")
    match commit_or_conflict(4):
        Ok(value) -> console.print("commit:${value}")
        Err(_error) -> console.print("unexpected-conflict")
    match commit_or_conflict(9):
        Ok(value) -> console.print("unexpected:${value}")
        Err(_error) -> console.print("conflict")
"#;

    let expected = vec![
        "normal:rollback-old",
        "error:rejected",
        "commit:commit-new",
        "conflict",
    ];
    assert_eq!(run_on_wasm(source), expected, "compiled Wasm normal and error exits");
    assert_eq!(link_run(source), expected, "must obligations erase before backend execution");
}

#[test]
fn transaction_resource_rejects_question_mark_exit_with_live_obligation() {
    let error = must_error(
        r#"
import transaction

fn validate(ok: Bool) -> Result(String, String):
    if ok:
        Ok("validated")
    else:
        Err("rejected")

fn update(ok: Bool) -> Result(String, String):
    let pending = transaction.begin("old", "new", 1)
    let _validated = validate(ok)?
    Ok(transaction.rollback(pending))

fn main():
    let _ = update(false)
"#,
    );
    assert!(
        error.contains("return leaves must-consume value `pending` undisposed"),
        "{error}"
    );
}
