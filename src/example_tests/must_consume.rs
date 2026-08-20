use super::*;

fn must_error(source: &str) -> String {
    let linked = try_link_std(source).expect("resource fixture links");
    typeck::check(&linked)
        .expect_err("resource lifecycle misuse must be rejected")
        .message
}

#[test]
fn transaction_resource_consumes_success_conflict_rollback_moves_and_aggregates_on_wasm() {
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
"#;

    let expected = vec![
        "move-new", "conflict-old:4:9", "batch-old", "new", "old",
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
