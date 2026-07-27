//! Task handles are executor-minted authority, not user-spellable slot ids.

use std::path::Path;
use std::process::Command;

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
