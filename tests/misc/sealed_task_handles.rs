//! Task handles are executor-minted authority, not user-spellable slot ids.

use std::path::Path;
use std::process::Command;
use witchy::runtime::{Capabilities, Runtime};
use witchy::codegen;

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

use super::temp_dir::TempDir;

fn assert_rejected(dir: &Path, label: &str, source: &str, expected: &[&str]) {
    let path = dir.join(format!("{label}.witchy"));
    std::fs::write(&path, source).unwrap();
    let output = Command::new(BIN).args(["check", path.to_str().unwrap()]).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{label} unexpectedly checked");
    for fragment in expected {
        assert!(stderr.contains(fragment), "{label} should contain {fragment:?}: {stderr}");
    }
}

fn assert_checked(dir: &Path, label: &str, source: &str) {
    let path = dir.join(format!("{label}.witchy"));
    std::fs::write(&path, source).unwrap();
    let output = Command::new(BIN).args(["check", path.to_str().unwrap()]).output().unwrap();
    assert!(
        output.status.success(),
        "{label} should check: {}",
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn task_handles_cannot_be_forged_for_join_or_cancel() {
    let dir = TempDir::new("sealed-handle");
    for operation in ["join", "cancel"] {
        let source = format!(
            "import task\nfrom task import Handle\n\nfn main(console: Console):\n    task.run(task.{operation}(Handle(999)))\n"
        );
        assert_rejected(&dir, operation, &source, &["sealed type", "Handle", "construct"]);
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn raw_scheduler_protocol_is_not_user_constructible() {
    let dir = TempDir::new("sealed-handle");
    assert_rejected(
        &dir,
        "raw-cancel",
        "import task\nfrom task import Task, Cancel\n\nfn main(console: Console):\n    task.run(Task(fn(): Cancel(0, fn(_done): task.ready_unit())))\n",
        &["Cancel", "exports no type or function"],
    );
    assert_rejected(
        &dir,
        "raw-step",
        "import task\nfrom task import Cancel\n\nfn main(console: Console):\n    let _step = Cancel(0, fn(_done): task.ready_unit())\n    console.print(\"bad\")\n",
        &["Cancel", "exports no type or function"],
    );
    assert_rejected(
        &dir,
        "raw-channel-id",
        "from task import ChannelId\n\nfn main(console: Console):\n    let _id = ChannelId(0)\n    console.print(\"bad\")\n",
        &["sealed type", "ChannelId", "construct"],
    );
    assert_rejected(
        &dir,
        "private-channel-bridge",
        "import task\n\nfn main(console: Console):\n    let _raw = task.__channel_open(0)\n    console.print(\"bad\")\n",
        &["compiler-private intrinsic", "public stdlib surface"],
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn task_handles_must_be_joined_cancelled_or_returned_on_every_path() {
    let dir = TempDir::new("must-task-handle");
    let prelude = "import chan\n\nasync fn child():\n    ()\n\n";

    assert_rejected(
        &dir,
        "dropped-handle",
        &format!(
            "{prelude}async fn main(console: Console):\n    let handle = chan.spawn(child()).await\n    console.print(\"forgot\")\n"
        ),
        &["must-consume value `handle`"],
    );

    assert_rejected(
        &dir,
        "early-return-handle",
        &format!(
            "{prelude}async fn main(console: Console):\n    let handle = chan.spawn(child()).await\n    if true:\n        return\n    else:\n        chan.join(handle).await\n"
        ),
        &["must-consume value `handle`"],
    );

    assert_rejected(
        &dir,
        "aggregate-handle",
        &format!(
            "{prelude}async fn main(console: Console):\n    let handle = chan.spawn(child()).await\n    let handles = [move handle]\n    console.print(\"forgot aggregate\")\n"
        ),
        &["must-consume value `handles`"],
    );

    assert_checked(
        &dir,
        "disposed-handle",
        &format!(
            "{prelude}async fn main(console: Console):\n    let handle = chan.spawn(child()).await\n    let disposition = if true:\n        chan.join(handle)\n    else:\n        chan.cancel(handle)\n    disposition.await\n    console.print(\"done\")\n"
        ),
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn structured_task_handle_aggregates_run_on_compiled_wasm() {
    let source = r#"
import chan

async fn child():
    chan.yield_now().await

async fn main(console: Console):
    let first = chan.spawn(child()).await
    let second = chan.spawn(child()).await
    let handles = [move first, move second]
    chan.join_all(handles).await
    console.print("joined")
"#;
    let checked = witchy::resolve_std_only_checked(source)
        .expect("structured task-handle fixture checks");
    let wasm = codegen::compile_checked_module_binary(&checked)
        .expect_lowered("structured task-handle fixture compiles to Wasm");
    let mut runtime = Runtime::batch().expect("create compiled-Wasm runtime");
    let mut actor = runtime
        .spawn(
            &wasm,
            Capabilities { print: true, quiet: true, ..Default::default() },
            256,
        )
        .expect("spawn structured task-handle fixture");
    actor.run().expect("run structured task-handle fixture");
    assert_eq!(actor.output(), ["joined"]);
}
