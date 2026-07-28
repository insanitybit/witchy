use std::io::Write;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

fn temp_dir() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "witchy-rfc0102-console-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn console_rights_gate_io_and_native_input_is_line_based() {
    let dir = temp_dir();
    let program = dir.join("input.witchy");
    std::fs::write(
        &program,
        "fn main(input: Console[Read], output: Console[Write]):\n    let name = input.read_line()\n    output.print(\"hello, ${name}\")\n",
    )
    .unwrap();
    let mut child = Command::new(BIN)
        .arg(program.to_str().unwrap())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"Ada\n").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("hello, Ada"));

    for (name, source, diagnostic) in [
        (
            "read-cannot-print.witchy",
            "fn main(console: Console[Read]):\n    console.print(\"no\")\n",
            "`print` needs `Write`",
        ),
        (
            "write-cannot-read.witchy",
            "fn main(console: Console[Write]):\n    let _ = console.read_line()\n",
            "`read_line` needs `Read`",
        ),
    ] {
        let path = dir.join(name);
        std::fs::write(&path, source).unwrap();
        let output = Command::new(BIN)
            .args(["check", path.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{name} unexpectedly type-checked");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(diagnostic), "wrong diagnostic for {name}: {stderr}");
    }

    std::fs::remove_dir_all(dir).unwrap();
}
