//! Standard input as a stream with a running budget, budgets of a connection's own beside the
//! whole run's, what a stopped run used on SIGINT and SIGTERM, the waiting connections a
//! service holds, and a trap in a connection's instance said as a run's is (dogfooding, third
//! pass, items 12 to 14). Each test starts the real engine on a module assembled here (no
//! compiler), and each failed before the change it tests.
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStderr, Command, Stdio};
use std::time::{Duration, Instant};

fn leb(mut value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop { let byte = (value & 0x7f) as u8; value >>= 7; if value == 0 { out.push(byte); return out; } out.push(byte | 0x80); }
}
fn section(id: u8, body: &[u8]) -> Vec<u8> { let mut out = vec![id]; out.extend(leb(body.len() as u64)); out.extend_from_slice(body); out }
fn name(text: &str) -> Vec<u8> { let mut out = leb(text.len() as u64); out.extend_from_slice(text.as_bytes()); out }

/// The network family's operations after `net_authorize`, in the order a module imports them.
const NET: [&str; 19] = ["net_addr_byte", "net_only", "net_connect", "net_status", "net_send_u8", "net_recv_u8", "net_close", "net_accept", "net_send", "net_recv",
    "net_group", "net_group_add", "net_group_take", "net_wait", "net_try_recv", "net_try_send", "net_try_accept", "net_handle", "net_poll"];
/// Function indices: the print imports first, then input or the network family.
const PRINT_U64: u8 = 0;
const PRINT_U8: u8 = 1;
const READ_U8: u8 = 2;
const NET_AUTHORIZE: u8 = 2;
const fn net(operation: u8) -> u8 { 3 + operation }
const SEND_U8: u8 = net(4);
const RECV_U8: u8 = net(5);
const CLOSE: u8 = net(6);

/// A scalar-profile module that prints, and reads its input (`input`) or takes a connection
/// (otherwise); `run` (named `handler` in the name section) has two I64 locals and is `body`.
fn module(input: bool, body: &[u8]) -> Vec<u8> {
    let mut out = b"\0asm\x01\0\0\0".to_vec();
    // 0: (i32, i64) -> (); 1: (i64) -> i64; 2: (i64, i64) -> i64; 3: () -> ().
    out.extend(section(1, &[4, 0x60, 2, 0x7f, 0x7e, 0, 0x60, 1, 0x7e, 1, 0x7e, 0x60, 2, 0x7e, 0x7e, 1, 0x7e, 0x60, 0, 0]));
    let mut imports: Vec<(&str, u8)> = vec![("print_u64", 0), ("print_u8", 0)];
    if input { imports.push(("read_u8", 1)); } else { imports.push(("net_authorize", 1)); imports.extend(NET.iter().map(|op| (*op, 2))); }
    let mut body_imports = leb(imports.len() as u64);
    for (function, ty) in &imports { body_imports.extend(name("witchy.scalar.v1")); body_imports.extend(name(function)); body_imports.extend([0, *ty]); }
    out.extend(section(2, &body_imports));
    out.extend(section(3, &[1, 3]));
    let run = imports.len() as u64;
    let mut export = leb(1); export.extend(name("run")); export.push(0); export.extend(leb(run));
    out.extend(section(7, &export));
    let mut function = vec![1, 2, 0x7e]; function.extend_from_slice(body);
    let mut code = leb(1); code.extend(leb(function.len() as u64)); code.extend(function);
    out.extend(section(10, &code));
    // The name section: function names, as the compiler writes them.
    let mut names = leb(1); names.extend(leb(run)); names.extend(name("handler"));
    let mut subsection = vec![1]; subsection.extend(leb(names.len() as u64)); subsection.extend(names);
    let mut custom = name("name"); custom.extend(subsection);
    out.extend(section(0, &custom));
    out
}
/// Counts its input's bytes, then prints the count.
const COUNT: &[u8] = &[0x02, 0x40, 0x03, 0x40,
    0x42, 3, 0x10, READ_U8, 0x42, 0x80, 0x02, 0x51, 0x0d, 1,
    0x20, 0, 0x42, 1, 0x7c, 0x21, 0, 0x0c, 0, 0x0b, 0x0b,
    0x41, 1, 0x20, 0, 0x10, PRINT_U64, 0x0b];
/// Writes each byte of its input as it reads it.
const CAT: &[u8] = &[0x02, 0x40, 0x03, 0x40,
    0x42, 3, 0x10, READ_U8, 0x22, 1, 0x42, 0x80, 0x02, 0x51, 0x0d, 1,
    0x41, 1, 0x20, 1, 0x10, PRINT_U8, 0x0c, 0, 0x0b, 0x0b, 0x0b];
/// Takes its connection and reads a byte: `t` traps; `o` prints 17 bytes; anything else prints
/// the byte's number (`107\n` for `k`, four bytes). Then sends `k` and closes.
const HANDLER: &[u8] = &[
    0x42, 2, 0x10, NET_AUTHORIZE, 0x21, 0,
    0x20, 0, 0x42, 0, 0x10, RECV_U8, 0x21, 1,
    0x20, 1, 0x42, 0xf4, 0x00, 0x51, 0x04, 0x40, 0x00, 0x0b,
    0x20, 1, 0x42, 0xef, 0x00, 0x51, 0x04, 0x40, 0x41, 1, 0x42, 0x80, 0x80, 0x9a, 0xa6, 0xea, 0xaf, 0xe3, 0x01, 0x10, PRINT_U64, 0x05, 0x41, 1, 0x20, 1, 0x10, PRINT_U64, 0x0b,
    0x20, 0, 0x42, 0xeb, 0x00, 0x10, SEND_U8, 0x1a,
    0x20, 0, 0x42, 0, 0x10, CLOSE, 0x1a,
    0x0b];
/// Prints a readiness line, then loops without reading standard input.
const READY_THEN_LOOP: &[u8] = &[
    0x41, 1, 0x42, 42, 0x10, PRINT_U64,
    0x03, 0x40, 0x0c, 0, 0x0b, 0x0b,
];

fn written(label: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("witchy-budgets-{}-{label}.wasm", std::process::id()));
    std::fs::write(&path, bytes).unwrap();
    path
}
fn engine(module: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_witchy-wasmtime-host"));
    command.arg(module).stdin(Stdio::null());
    command
}
/// The exit status, if the process ends within `within`; it is killed otherwise.
fn ends(child: &mut Child, within: Duration) -> Option<i32> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() { return status.code(); }
        if started.elapsed() > within { child.kill().unwrap(); child.wait().unwrap(); return None; }
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn all_said(said: &str) { assert!(said.lines().all(|line| line.starts_with("witchy: ")), "{said}"); }

#[test]
fn sqlite_request_is_a_bounded_pipe_snapshot() {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let root = std::env::temp_dir().canonicalize().unwrap().join(format!("witchy-sqlite-request-{}", std::process::id()));
    std::fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    let database = root.join("db");
    std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&database).unwrap();
    let guest = root.join("count.wasm");
    std::fs::write(&guest, module(true, COUNT)).unwrap();
    let grant = format!("--sqlite={}", database.display());
    let run = |flag, limit, bytes: &[u8]| {
        let mut child = engine(&guest).args([&grant, "--sqlite-read=items", "--output", flag, limit])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        if let Err(error) = child.stdin.take().unwrap().write_all(bytes) {
            assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
        }
        child.wait_with_output().unwrap()
    };
    let accepted = run("--sqlite-request-stdin", "--input-bytes=3", b"abc");
    assert!(accepted.status.success(), "{accepted:?}");
    assert_eq!(accepted.stdout, b"3\n");
    for (flag, limit, bytes, refused) in [
        ("--sqlite-request-stdin", "--input-bytes=2", b"abc".as_slice(), "input-bytes"),
        ("--sqlite-request-stdin", "--input-bytes=unlimited", b"".as_slice(), "16777216 bytes"),
        ("--input-stdin", "--input-bytes=3", b"abc".as_slice(), "SQLite requires a one-shot run"),
    ] {
        let result = run(flag, limit, bytes);
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
        assert!(String::from_utf8_lossy(&result.stderr).contains(refused), "{result:?}");
    }
    let result = engine(&guest).args([&grant, "--sqlite-read=items", "--output", "--sqlite-request-stdin"])
        .stdin(std::fs::File::open(&database).unwrap()).output().unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("needs a pipe"), "{result:?}");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn standard_input_longer_than_16_mib_is_read_as_a_stream() {
    let path = written("count", &module(true, COUNT));
    let size = 20 * 1024 * 1024 + 7;
    let mut child = engine(&path).args(["--output", "--input-stdin", "--input-bytes=unlimited", "--unmetered", "--deadline-ms=60000", "--usage"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let feeder = std::thread::spawn(move || { let block = vec![b'x'; 1 << 20]; for _ in 0..20 { stdin.write_all(&block).unwrap(); } stdin.write_all(b"1234567").unwrap(); });
    let ran = child.wait_with_output().unwrap();
    feeder.join().unwrap();
    let said = String::from_utf8(ran.stderr).unwrap();
    assert!(ran.status.success(), "{said}");
    assert_eq!(String::from_utf8(ran.stdout).unwrap(), format!("{size}\n"));
    assert!(said.contains(&format!("input {size} of unlimited bytes (--input-bytes)")), "{said}");
    all_said(&said);
    // A number above 16 MiB is a running budget too, and only for standard input.
    let ran = engine(&path).args(["--output", "--input=/dev/null", "--input-bytes=16777217"]).output().unwrap();
    assert_eq!(ran.status.code(), Some(1));
    assert!(String::from_utf8(ran.stderr).unwrap().contains("is for --input-stdin"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn a_tool_in_a_pipe_answers_before_its_input_ends() {
    let path = written("cat", &module(true, CAT));
    let mut child = engine(&path).args(["--output", "--stream-output", "--input-stdin", "--input-bytes=1000", "--unmetered", "--deadline-ms=20000"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"first\n").unwrap();
    stdin.flush().unwrap();
    // The first line comes back while standard input is still open.
    let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
    let (sender, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || { let mut line = String::new(); stdout.read_line(&mut line).unwrap(); sender.send((line, stdout)).unwrap(); });
    let (line, mut stdout) = received.recv_timeout(Duration::from_secs(10)).expect("no line came back before the input ended");
    assert_eq!(line, "first\n");
    stdin.write_all(b"second\n").unwrap();
    drop(stdin);
    assert_eq!(ends(&mut child, Duration::from_secs(10)), Some(0));
    let mut rest = String::new();
    stdout.read_to_string(&mut rest).unwrap();
    assert_eq!(rest, "second\n");
    let _ = std::fs::remove_file(path);
}

#[test]
fn a_streamed_input_ends_the_run_one_byte_past_its_budget() {
    let path = written("count-over", &module(true, COUNT));
    let mut child = engine(&path).args(["--output", "--input-stdin", "--input-bytes=1000", "--unmetered", "--deadline-ms=20000", "--limit-name=--input-bytes=[run] input-bytes"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let feeder = std::thread::spawn(move || { let _ = stdin.write_all(&[b'y'; 1001]); });
    let ran = child.wait_with_output().unwrap();
    feeder.join().unwrap();
    let said = String::from_utf8(ran.stderr).unwrap();
    assert_eq!(ran.status.code(), Some(1), "{said}");
    assert!(ran.stdout.is_empty(), "the count was printed though the input was over its budget");
    assert!(said.contains("witchy: limit reached: input bytes: standard input holds more than the 1000 bytes granted ([run] input-bytes)"), "{said}");
    assert!(said.contains("raise [run] input-bytes"), "{said}");
    all_said(&said);
    // Exactly the budget is not over it.
    let mut child = engine(&path).args(["--output", "--input-stdin", "--input-bytes=1000", "--unmetered", "--deadline-ms=20000"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let feeder = std::thread::spawn(move || { stdin.write_all(&[b'y'; 1000]).unwrap(); });
    let ran = child.wait_with_output().unwrap();
    feeder.join().unwrap();
    assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));
    assert_eq!(ran.stdout, b"1000\n");
    let _ = std::fs::remove_file(path);
}

#[test]
fn the_deadline_holds_while_a_streamed_input_stays_open() {
    let path = written("count-wait", &module(true, COUNT));
    let mut child = engine(&path).args(["--output", "--input-stdin", "--input-bytes=unlimited", "--unmetered", "--deadline-ms=300"])
        .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap();
    let held = child.stdin.take().unwrap();
    let started = Instant::now();
    assert_eq!(ends(&mut child, Duration::from_secs(5)), Some(124));
    assert!(started.elapsed() < Duration::from_secs(3));
    drop(held);
    let mut said = String::new();
    child.stderr.take().unwrap().read_to_string(&mut said).unwrap();
    assert!(said.contains("witchy: limit reached: deadline"), "{said}");
    let _ = std::fs::remove_file(path);
}

#[test]
fn standard_input_is_never_a_stream_beside_a_secret() {
    let path = written("count-secret", &module(true, COUNT));
    let ran = engine(&path).env("WITCHY_TEST_KEY", "k").args(["--output", "--input-stdin", "--input-bytes=unlimited", "--secret-slot=key:reveal-bytes:env:WITCHY_TEST_KEY"]).output().unwrap();
    let said = String::from_utf8(ran.stderr).unwrap();
    assert_eq!(ran.status.code(), Some(1), "{said}");
    assert!(said.contains("never granted beside a secret"), "{said}");
    let _ = std::fs::remove_file(path);
}

/// SIGINT (Ctrl-C) and SIGTERM: what the run used is said, then why it ended; a second signal
/// (Grimoire passes Ctrl-C on as SIGTERM) does not cut that short.
#[test]
fn a_run_stopped_by_a_signal_says_what_it_used() {
    let path = written("forever", &module(true, READY_THEN_LOOP));
    for (signals, status, why) in [(&[2][..], 130, "interrupted (SIGINT)"), (&[15][..], 124, "terminated (SIGTERM)"), (&[2, 15][..], 130, "interrupted (SIGINT)")] {
        let mut child = engine(&path).args(["--output", "--stream-output", "--input-stdin", "--input-bytes=unlimited", "--unmetered", "--deadline-ms=20000", "--usage"])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
        let (ready, readiness) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut line = String::new();
            let _ = ready.send(stdout.read_line(&mut line).map(|_| line));
        });
        let line = readiness.recv_timeout(Duration::from_secs(10));
        if !matches!(&line, Ok(Ok(line)) if line == "42\n") {
            let _ = child.kill();
            let _ = child.wait();
            reader.join().unwrap();
            panic!("expected readiness from the guest, got {line:?}");
        }
        reader.join().unwrap();
        unsafe extern "C" { fn kill(pid: i32, signal: i32) -> i32; }
        for number in signals { assert_eq!(unsafe { kill(child.id() as i32, *number) }, 0); std::thread::sleep(Duration::from_millis(20)); }
        assert_eq!(ends(&mut child, Duration::from_secs(5)), Some(status));
        let mut said = String::new();
        child.stderr.take().unwrap().read_to_string(&mut said).unwrap();
        assert!(said.contains(&format!("witchy: stopped: {why}; the run ends")), "{said}");
        assert!(said.contains("witchy: limit: used: input 0 of unlimited bytes (--input-bytes)"), "{said}");
        all_said(&said);
    }
    let _ = std::fs::remove_file(path);
}

struct Service {
    child: Child,
    stderr: BufReader<ChildStderr>,
}

/// A service run with `flags`, an instance for each connection, ending after `connections`.
fn service(label: &str, connections: usize, flags: &[&str]) -> (Service, String, std::path::PathBuf) {
    let path = written(label, &module(false, HANDLER));
    let mut command = engine(&path);
    command.args(["--output", "--stream-output", "--instance-per-connection", "--fuel=100000", "--connection-deadline-ms=5000", "--deadline-ms=60000", "--usage"])
        .arg("--listen=127.0.0.1:0").arg(format!("--connections={connections}")).args(flags)
        .stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut stderr = BufReader::new(stderr);
        let mut ready = String::new();
        let read = stderr.read_line(&mut ready);
        let _ = send.send((stderr, ready, read));
    });
    let ready = receive.recv_timeout(Duration::from_secs(10));
    if ready.is_err() {
        let _ = child.kill();
        child.wait().unwrap();
    }
    reader.join().unwrap();
    let (stderr, ready, read) = ready.expect("the service did not announce a listener");
    read.unwrap();
    let address = ready.trim_end().strip_prefix("witchy: listening on ")
        .unwrap_or_else(|| panic!("the service did not announce a listener: {ready:?}"));
    let listener: std::net::SocketAddr = address.parse().unwrap();
    assert!(listener.ip().is_loopback() && listener.port() != 0, "{listener}");
    // Keep the original empty probe connection in the guest's accounting.
    drop(std::net::TcpStream::connect_timeout(&listener, Duration::from_secs(2)).unwrap());
    (Service { child, stderr }, address.to_owned(), path)
}
/// One client: sends `byte`, and reads what comes back to the end.
fn ask(address: &str, byte: u8) -> Vec<u8> {
    let Ok(mut stream) = std::net::TcpStream::connect(address) else { return b"refused".to_vec() };
    stream.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    if stream.write_all(&[byte]).is_err() { return b"reset".to_vec(); }
    let mut answer = Vec::new();
    if stream.read_to_end(&mut answer).is_err() { return b"reset".to_vec(); }
    answer
}
fn finished(Service { mut child, mut stderr }: Service) -> (String, String) {
    assert_eq!(ends(&mut child, Duration::from_secs(20)), Some(0));
    let (mut out, mut said) = (String::new(), String::new());
    child.stdout.take().unwrap().read_to_string(&mut out).unwrap();
    stderr.read_to_string(&mut said).unwrap();
    all_said(&said);
    (out, said)
}

/// The probe connection `service` made while waiting is the first (it sends nothing: `256`,
/// the end of its input, is printed); then each asks in turn.
#[test]
fn each_connection_has_its_own_output_and_network_budgets() {
    // Thirty connections of four output bytes and two network bytes each: far past a whole
    // run's 64 bytes, within each connection's own. (And the probe's: four bytes of output,
    // `256`, and one of network, the end it read.)
    let (child, address, path) = service("own", 31, &["--output-bytes=unlimited", "--output-bytes-per-connection=10", "--net-bytes=unlimited", "--net-bytes-per-connection=2"]);
    let answers: Vec<Vec<u8>> = (0..30).map(|_| ask(&address, b'k')).collect();
    assert!(answers.iter().all(|answer| answer == b"k"), "{answers:?}");
    let (out, said) = finished(child);
    assert_eq!(out.lines().filter(|line| *line == "107").count(), 30, "{out}");
    assert!(said.contains("output 124 of unlimited bytes (--output-bytes), at most 4 of 10 in one connection (--output-bytes-per-connection)"), "{said}");
    assert!(said.contains("network 61 of unlimited bytes (--net-bytes), at most 2 of 2 in one connection (--net-bytes-per-connection)"), "{said}");
    // One connection past its own output budget ends alone, and says whose budget it was.
    let (child, address, path2) = service("own-over", 3, &["--output-bytes=1000", "--output-bytes-per-connection=10"]);
    assert_eq!(ask(&address, b'o'), b"");
    assert_eq!(ask(&address, b'k'), b"k");
    let (out, said) = finished(child);
    assert_eq!(out, "256\n107\n");
    assert!(said.contains("witchy: limit reached: output bytes: a connection's instance has written its 10 bytes (--output-bytes-per-connection); that connection is closed and the server goes on"), "{said}");
    // Where what is left of the run's is less than a connection's own, the run's is what is
    // said: the probe's 4 bytes and one answer's leave 2 of 10.
    let (child, address, path3) = service("rest", 4, &["--output-bytes=10", "--output-bytes-per-connection=5"]);
    assert_eq!(ask(&address, b'k'), b"k");
    assert_eq!(ask(&address, b'k'), b"");
    let _ = ask(&address, b'k');
    let (out, said) = finished(child);
    assert_eq!(out, "256\n107\n");
    assert!(said.contains("output bytes: the run has written its 10 granted bytes (--output-bytes)"), "{said}");
    for path in [path, path2, path3] { let _ = std::fs::remove_file(path); }
}

#[test]
fn a_run_without_a_ceiling_needs_a_budget_for_each_connection() {
    let path = written("handler", &module(false, HANDLER));
    for (flags, said) in [
        (&["--output-bytes=unlimited"][..], "--output-bytes=unlimited needs --instance-per-connection and --output-bytes-per-connection=N"),
        (&["--instance-per-connection", "--listen=127.0.0.1:1", "--net-bytes=unlimited"][..], "--net-bytes=unlimited needs --instance-per-connection and --net-bytes-per-connection=N"),
        (&["--instance-per-connection", "--listen=127.0.0.1:1", "--fs-entries=unlimited", "--net-bytes-per-connection=5"][..], "--fs-entries=unlimited needs"),
        (&["--output-bytes-per-connection=5"][..], "a budget per connection and --waiting-connections are for --instance-per-connection"),
    ] {
        let ran = engine(&path).arg("--output").args(flags).output().unwrap();
        assert_eq!(ran.status.code(), Some(1));
        let text = String::from_utf8(ran.stderr).unwrap();
        assert!(text.contains(said), "{flags:?}: {text}");
    }
    let _ = std::fs::remove_file(path);
}

#[test]
fn a_trap_in_a_connection_says_where_it_stopped() {
    let (child, address, path) = service("trap", 3, &[]);
    assert_eq!(ask(&address, b't'), b"");
    assert_eq!(ask(&address, b'k'), b"k");
    let (_, said) = finished(child);
    assert!(said.contains("witchy: connection 2 ended: wasm trap: wasm `unreachable` instruction executed; it is closed and the server goes on"), "{said}");
    assert!(said.contains("witchy: connection 2 stopped in `handler`: a check failed"), "{said}");
    let _ = std::fs::remove_file(path);
}

/// Two hundred clients at once, each connected before any sends its request (so the first
/// instance waits while the rest arrive): every one is answered, none refused or reset, though
/// the system's own queue (128 on macOS) holds fewer.
#[test]
fn two_hundred_clients_at_once_are_all_answered() {
    let (child, address, path) = service("many", 201, &[]);
    let start = std::sync::Arc::new(std::sync::Barrier::new(200));
    let clients: Vec<_> = (0..200).map(|_| { let (start, address) = (start.clone(), address.clone()); std::thread::spawn(move || {
        let connected = std::net::TcpStream::connect_timeout(&address.parse().unwrap(), Duration::from_secs(3));
        start.wait();
        let Ok(mut stream) = connected else { return b"refused".to_vec() };
        stream.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        if stream.write_all(b"k").is_err() { return b"reset".to_vec(); }
        let mut answer = Vec::new();
        if stream.read_to_end(&mut answer).is_err() { return b"reset".to_vec(); }
        answer
    }) }).collect();
    let answers: Vec<Vec<u8>> = clients.into_iter().map(|client| client.join().unwrap()).collect();
    let answered = answers.iter().filter(|answer| *answer == b"k").count();
    assert_eq!(answered, 200, "{:?}", answers.iter().filter(|answer| *answer != b"k").map(|answer| String::from_utf8_lossy(answer).into_owned()).collect::<Vec<_>>());
    let _ = finished(child);
    let _ = std::fs::remove_file(path);
}
