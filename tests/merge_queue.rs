use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct ProcessGroupGuard(i32);

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "witchy-merge-queue-daemon-{}-{nonce}",
            std::process::id(),
        ));
        fs::create_dir(&path).expect("create temporary state directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-TERM", &format!("-{}", self.0)])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn process_group(pid: i32) -> i32 {
    let output = Command::new("ps")
        .args(["-o", "pgid=", "-p", &pid.to_string()])
        .output()
        .expect("run ps");
    assert!(output.status.success(), "ps failed for pid {pid}");
    String::from_utf8(output.stdout)
        .expect("ps output is utf8")
        .trim()
        .parse()
        .expect("parse process group")
}

fn process_is_alive(pid: i32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[test]
fn daemon_enters_an_independent_process_group() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp = TempDir::new();
    let state = temp.path().join("state");
    let gate_worktree = temp.path().join("unused-gate-worktree");

    let output = Command::new(root.join("scripts/merge-queue.sh"))
        .arg("daemon")
        .env("MERGE_QUEUE_STATE_DIR", &state)
        .env("MERGE_QUEUE_GATE_WT", &gate_worktree)
        .env("MERGE_QUEUE_GATE_CMD", "true")
        .output()
        .expect("start isolated coordinator daemon");
    assert!(
        output.status.success(),
        "daemon failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let pid_path = state.join("coordinator.pid");
    let deadline = Instant::now() + Duration::from_secs(5);
    let pid = loop {
        if let Ok(text) = fs::read_to_string(&pid_path) {
            break text.trim().parse::<i32>().expect("parse coordinator pid");
        }
        assert!(Instant::now() < deadline, "coordinator pid was not written");
        thread::sleep(Duration::from_millis(25));
    };
    let guard = ProcessGroupGuard(pid);

    assert!(
        process_is_alive(pid),
        "coordinator {pid} did not survive daemon return"
    );
    assert_eq!(
        process_group(pid),
        pid,
        "the coordinator must lead a new process group so launcher cleanup cannot kill it"
    );
    assert_ne!(
        process_group(std::process::id() as i32),
        pid,
        "the coordinator remained in the test runner's process group"
    );

    drop(guard);
    let deadline = Instant::now() + Duration::from_secs(5);
    while process_is_alive(pid) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        !process_is_alive(pid),
        "coordinator {pid} ignored process-group termination"
    );
}
