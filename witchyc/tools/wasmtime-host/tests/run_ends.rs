//! A run ends at its deadline, or with its launcher, whatever its standard streams are: closed,
//! or pipes nobody reads. Each test starts the real engine on a module assembled here (no
//! compiler), and each hung before its fix (review, 2026-11-22).
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn leb(mut value: u32) -> Vec<u8> {
    let mut out = Vec::new();
    loop { let byte = (value & 0x7f) as u8; value >>= 7; if value == 0 { out.push(byte); return out; } out.push(byte | 0x80); }
}
fn section(id: u8, body: &[u8]) -> Vec<u8> { let mut out = vec![id]; out.extend(leb(body.len() as u32)); out.extend_from_slice(body); out }
fn name(text: &str) -> Vec<u8> { let mut out = leb(text.len() as u32); out.extend_from_slice(text.as_bytes()); out }
/// A scalar-profile module whose `run` is `body`; `printing`: it imports the two output functions.
fn module(printing: bool, body: &[u8]) -> Vec<u8> {
    let mut out = b"\0asm\x01\0\0\0".to_vec();
    out.extend(section(1, &[2, 0x60, 2, 0x7f, 0x7e, 0, 0x60, 0, 0]));
    let imports = if printing { 2 } else { 0 };
    if printing {
        let mut body = leb(2);
        for function in ["print_u64", "print_u8"] { body.extend(name("witchy.scalar.v1")); body.extend(name(function)); body.extend([0, 0]); }
        out.extend(section(2, &body));
    }
    out.extend(section(3, &[1, 1]));
    let mut export = leb(1); export.extend(name("run")); export.push(0); export.extend(leb(imports));
    out.extend(section(7, &export));
    let mut code = leb(1); code.extend(leb(body.len() as u32)); code.extend_from_slice(body);
    out.extend(section(10, &code));
    out
}
/// `loop {}`.
const FOREVER: &[u8] = &[0, 0x03, 0x40, 0x0c, 0, 0x0b, 0x0b];
/// 200000 times `print_u8(1, 'A')`: more than a pipe holds.
const LARGE: &[u8] = &[1, 1, 0x7e, 0x03, 0x40, 0x41, 1, 0x42, 0xc1, 0x00, 0x10, 1, 0x20, 0, 0x42, 1, 0x7c, 0x22, 0, 0x42, 0xc0, 0x9a, 0x0c, 0x54, 0x0d, 0, 0x0b, 0x0b];

fn written(label: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("witchy-run-ends-{}-{label}.wasm", std::process::id()));
    std::fs::write(&path, bytes).unwrap();
    path
}
fn engine(module: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_witchy-wasmtime-host"));
    command.arg(module).stdin(Stdio::null());
    command
}
/// The exit status, if the process ends within `within`; it is killed otherwise.
fn ends(mut child: std::process::Child, within: Duration) -> Option<i32> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() { return status.code(); }
        if started.elapsed() > within { child.kill().unwrap(); child.wait().unwrap(); return None; }
        std::thread::sleep(Duration::from_millis(10));
    }
}
/// A pipe's write end for a child, its read end already closed: every write to it fails.
fn closed() -> Stdio { let (reader, writer) = std::io::pipe().unwrap(); drop(reader); Stdio::from(writer) }

#[test]
fn an_exec_worker_refuses_to_run_without_the_hosts_permission() {
    use rustix::io::{FdFlags, fcntl_setfd};
    use std::os::fd::AsRawFd;
    use std::os::unix::{net::UnixStream, process::CommandExt};

    let (gate, worker) = UnixStream::pair().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_witchy-wasmtime-host"));
    command.args(["--internal-exec-worker", &worker.as_raw_fd().to_string(), "/bin/sh", "-c", "exit 97"]);
    unsafe {
        command.pre_exec(move || fcntl_setfd(&worker, FdFlags::empty()).map_err(Into::into));
    }
    let child = command.spawn().unwrap();
    drop(command);
    drop(gate);
    assert_eq!(ends(child, Duration::from_secs(5)), Some(126));
}

#[test]
fn a_pinned_launch_verifies_the_exact_module_before_the_listener_runs() {
    use aws_lc_rs::digest::{SHA384, digest};
    let wire = module(false, &[0, 0x0b]);
    let hex: String = digest(&SHA384, &wire).as_ref().iter().map(|byte| format!("{byte:02x}")).collect();
    let pin = format!("--module-pin=sha384:{hex}");
    let path = written("pinned", &wire);
    let ran = engine(&path).arg(&pin).output().unwrap();
    assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));

    let link = path.with_extension("link");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    let via_link = engine(&link).arg(&pin).output().unwrap();
    assert!(!via_link.status.success());
    assert!(String::from_utf8_lossy(&via_link.stderr).contains("pinned module must be a readable regular file"));

    let repeated = engine(&path).args([&pin, &pin]).output().unwrap();
    assert!(!repeated.status.success());
    assert!(String::from_utf8_lossy(&repeated.stderr).contains("--module-pin given twice"));
    let malformed = engine(&path).arg("--module-pin=sha384:ab").output().unwrap();
    assert!(!malformed.status.success());
    assert!(String::from_utf8_lossy(&malformed.stderr).contains("96 lowercase hex digits"));

    let output = engine(&path).args(["--instance-per-connection", "--listen=127.0.0.1:0", "--output", "--listener-ready-stdout"]).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("exclusive use of standard output"));
    assert!(output.stdout.is_empty());

    std::fs::write(&path, b"changed and not a valid module").unwrap();
    let changed = engine(&path).arg(pin).args(["--listen=127.0.0.1:0", "--instance-per-connection", "--listener-ready-stdout"]).output().unwrap();
    assert!(!changed.status.success());
    assert!(String::from_utf8_lossy(&changed.stderr).contains("module differs from its independent pin"));
    assert!(changed.stdout.is_empty());
    std::fs::remove_file(link).unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn a_pinned_module_descriptor_checks_the_complete_unlinked_file() {
    use aws_lc_rs::digest::{SHA384, digest};
    use rustix::io::{FdFlags, fcntl_setfd};
    use std::io::Seek;
    use std::os::fd::AsRawFd;
    use std::os::unix::{fs::FileExt, process::CommandExt};

    let wire = module(false, &[0, 0x0b]);
    let hex: String = digest(&SHA384, &wire).as_ref().iter().map(|byte| format!("{byte:02x}")).collect();
    let pin = format!("--module-pin=sha384:{hex}");
    let path = written("pinned-descriptor", &wire);
    let mut file = std::fs::OpenOptions::new().read(true).write(true).open(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    file.seek(std::io::SeekFrom::End(0)).unwrap();

    let launch = |source: std::os::fd::OwnedFd, extra: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_witchy-wasmtime-host"));
        command.arg(format!("--module-fd={}", source.as_raw_fd())).args(extra).stdin(Stdio::null());
        unsafe { command.pre_exec(move || fcntl_setfd(&source, FdFlags::empty()).map_err(Into::into)); }
        command.output().unwrap()
    };
    let ran = launch(file.try_clone().unwrap().into(), &[&pin]);
    assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));

    let no_pin = launch(file.try_clone().unwrap().into(), &[]);
    assert!(!no_pin.status.success());
    assert!(String::from_utf8_lossy(&no_pin.stderr).contains("--module-fd requires --module-pin"));
    let both = launch(file.try_clone().unwrap().into(), &[&pin, path.to_str().unwrap()]);
    assert!(!both.status.success());
    assert!(String::from_utf8_lossy(&both.stderr).contains("the only module source"));

    let (reader, _writer) = std::io::pipe().unwrap();
    let fifo = launch(reader.into(), &[&pin]);
    assert!(!fifo.status.success());
    assert!(String::from_utf8_lossy(&fifo.stderr).contains("inherited readable regular file"));

    let shared = file.try_clone().unwrap();
    let secret = format!("--secret-slot=module:seal:fd:{}", shared.as_raw_fd());
    let shared_descriptor = launch(shared.into(), &[&pin, &secret]);
    assert!(!shared_descriptor.status.success());
    assert!(String::from_utf8_lossy(&shared_descriptor.stderr).contains("inherited readable regular file"));

    let aliased = file.try_clone().unwrap();
    let source = file.try_clone().unwrap();
    let secret = format!("--secret-slot=module:seal:fd:{}", source.as_raw_fd());
    let mut command = Command::new(env!("CARGO_BIN_EXE_witchy-wasmtime-host"));
    command.args([format!("--module-fd={}", aliased.as_raw_fd()), pin.clone(), secret]);
    unsafe {
        command.pre_exec(move || {
            fcntl_setfd(&aliased, FdFlags::empty())?;
            fcntl_setfd(&source, FdFlags::empty())?;
            Ok(())
        });
    }
    let shared_object = command.output().unwrap();
    assert!(!shared_object.status.success());
    assert!(String::from_utf8_lossy(&shared_object.stderr).contains("module is a secret slot's source"));

    file.write_all_at(b"changed", 0).unwrap();
    let changed = launch(file.into(), &[&pin, "--listen=127.0.0.1:0", "--instance-per-connection", "--listener-ready-stdout"]);
    assert!(!changed.status.success());
    assert!(String::from_utf8_lossy(&changed.stderr).contains("module differs from its independent pin"));
    assert!(changed.stdout.is_empty());
}

#[test]
fn the_deadline_ends_a_run_whose_standard_error_is_closed() {
    let path = written("forever", &module(false, FOREVER));
    let child = engine(&path).args(["--unmetered", "--deadline-ms=300"]).stdout(Stdio::null()).stderr(closed()).spawn().unwrap();
    assert_eq!(ends(child, Duration::from_secs(5)), Some(124));
    // And says so, on one that is open: each line with the engine's prefix.
    let ran = engine(&path).args(["--unmetered", "--deadline-ms=300"]).output().unwrap();
    assert_eq!(ran.status.code(), Some(124));
    let said = String::from_utf8(ran.stderr).unwrap();
    assert!(said.contains("witchy: limit reached: deadline: the run was stopped after its 300 ms"), "{said}");
    assert!(said.lines().all(|line| line.starts_with("witchy: ")), "{said}");
    let _ = std::fs::remove_file(path);
}

#[test]
fn a_run_ends_with_its_launcher_though_standard_error_is_closed() {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    let path = written("lifeline", &module(false, FOREVER));
    let (reader, writer) = std::io::pipe().unwrap();
    let mut command = engine(&path);
    command.args(["--unmetered", "--deadline-ms=none", "--lifeline=fd:9"]).stdout(Stdio::null()).stderr(closed());
    let from = reader.as_raw_fd();
    unsafe extern "C" { fn dup2(from: i32, to: i32) -> i32; }
    // The launcher's end of the lifeline is descriptor 9 of the engine (dup2 is async-signal-safe).
    unsafe { command.pre_exec(move || if dup2(from, 9) == 9 { Ok(()) } else { Err(std::io::Error::last_os_error()) }) };
    let mut child = command.spawn().unwrap();
    drop(reader);
    std::thread::sleep(Duration::from_millis(300));
    assert!(child.try_wait().unwrap().is_none(), "the run goes on while its launcher holds the lifeline");
    drop(writer);
    assert_eq!(ends(child, Duration::from_secs(5)), Some(125));
    let _ = std::fs::remove_file(path);
}

#[test]
fn the_deadline_ends_a_run_stopped_writing_output_nobody_reads() {
    let path = written("large", &module(true, LARGE));
    // Not streamed: the whole output is written when the program has finished, into a pipe
    // that holds less and is never read.
    let mut child = engine(&path).args(["--output", "--unmetered", "--output-bytes=1000000", "--deadline-ms=600"]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let unread = child.stdout.take().unwrap();
    let mut error = child.stderr.take().unwrap();
    assert_eq!(ends(child, Duration::from_secs(5)), Some(124));
    let mut said = String::new();
    error.read_to_string(&mut said).unwrap();
    assert!(said.contains("witchy: limit reached: deadline"), "{said}");
    drop(unread);
    // Read, it is all there and the run succeeds.
    let ran = engine(&path).args(["--output", "--unmetered", "--output-bytes=1000000", "--deadline-ms=5000"]).output().unwrap();
    assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));
    assert_eq!(ran.stdout.len(), 200_000);
    let _ = std::io::stderr().flush();
    let _ = std::fs::remove_file(path);
}

#[test]
fn a_command_line_that_is_not_text_is_refused_in_one_line() {
    use std::os::unix::ffi::OsStrExt;
    let path = written("words", &module(false, &[0, 0x0b]));
    let ran = engine(&path).args(["--args=2:64"]).arg(std::ffi::OsStr::from_bytes(b"--arg=\xff\xfe")).output().unwrap();
    assert_eq!(ran.status.code(), Some(1));
    assert_eq!(String::from_utf8(ran.stderr).unwrap(), "witchy: invalid grant: argument 3 of the command line is not UTF-8 text\n");
    let _ = std::fs::remove_file(path);
}
