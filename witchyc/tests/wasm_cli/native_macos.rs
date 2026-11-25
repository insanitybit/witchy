use super::{Command, Fixture, fs};

#[test]
fn macos_native_net_requires_one_explicit_numeric_grant() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("main.wy"),
        r#"fn main(console: Console<Write>, net: Net):
    net.raw_addr_byte(U8.truncate(58))
    net.raw_addr_byte(U8.truncate(58))
    net.raw_addr_byte(U8.truncate(49))
    net.raw_addr_byte(U8.truncate(58))
    net.raw_addr_byte(U8.truncate(49))
    let narrowed = net.raw_only()
    let code = narrowed.raw_status()
    console.print_int(code)
    if code == 0:
        net.raw_addr_byte(U8.truncate(58))
        narrowed.raw_addr_byte(U8.truncate(58))
        narrowed.raw_addr_byte(U8.truncate(58))
        narrowed.raw_addr_byte(U8.truncate(49))
        narrowed.raw_addr_byte(U8.truncate(58))
        narrowed.raw_addr_byte(U8.truncate(50))
        let refused = narrowed.raw_only()
        console.print_int(refused.raw_status())
        let refused_closed = refused.raw_close()
        net.raw_addr_byte(U8.truncate(58))
        net.raw_addr_byte(U8.truncate(49))
        net.raw_addr_byte(U8.truncate(58))
        net.raw_addr_byte(U8.truncate(50))
        let other = net.raw_only()
        console.print_int(other.raw_status())
        let other_closed = other.raw_close()
    let closed = narrowed.raw_close()
"#).unwrap();
    let built = fixture.run(&["build", "main.wy", "-o", "client", "--target=macos-arm64"]);
    assert!(built.status.success(), "{built:?}");
    let client = fixture.0.join("client");
    for grants in [vec![], vec!["--net=invalid/name:1"], vec!["--net=127.0.0.1:1", "--net=127.0.0.1:2"]] {
        let run = Command::new(&client).arg("--output").args(grants).output().unwrap();
        assert_eq!(run.status.code(), Some(2), "{run:?}");
        assert!(run.stdout.is_empty(), "{run:?}");
    }
    for (grant, expected) in [("--net=[::1]:2,[0:0:0:0:0:0:0:1]:1", &b"0\n1\n0\n"[..]),
                              ("--net=[::1]:2", &b"1\n"[..])] {
        let run = Command::new(&client).args(["--output", grant]).output().unwrap();
        assert!(run.status.success(), "{run:?}");
        assert_eq!(run.stdout, expected);
    }
}

fn link_runtime_probe(source: &str, probe: &std::path::Path, flags: &[&str]) {
    let object = probe.with_extension("o");
    let compiled = Command::new(option_env!("RUSTC").unwrap_or("rustc"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["--edition=2024", "--crate-type=lib", "--emit=obj", "-Copt-level=2", "-Cpanic=abort"])
        .args(flags)
        .arg(source)
        .arg("-o")
        .arg(&object)
        .output()
        .unwrap();
    assert!(compiled.status.success(), "{compiled:?}");
    let mut linker = Command::new(option_env!("WITCHY_NATIVE_SANITIZER_CLANG").unwrap_or("/usr/bin/clang"));
    linker.current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["-arch", "arm64", "-Wl,-dead_strip"])
        .args([
            "-O2",
            "-Wall",
            "-Wextra",
            "-Werror",
            concat!(env!("OUT_DIR"), "/", env!("WITCHY_NATIVE_ARCHIVE_FILE")),
            #[cfg(witchy_native_sanitized)]
            "--ld-path=/usr/bin/ld",
            #[cfg(witchy_native_sanitized)]
            "-fsanitize=address,undefined",
        ])
        .arg(&object);
    #[cfg(witchy_native_aws_lc)]
    {
        let lib = std::path::Path::new(env!("WITCHY_NATIVE_AWS_LC_INSTALLED")).join("lib");
        linker.arg(lib.join("libaws_lc_fips_0_14_4_crypto.dylib"))
            .args(["-Xlinker", "-rpath", "-Xlinker"]).arg(&lib)
            .arg("-Wl,-dead_strip_dylibs");
    }
    let built = linker
        .arg("-o")
        .arg(probe)
        .output()
        .unwrap();
    assert!(built.status.success(), "{built:?}");
}

#[test]
fn macos_native_startup_confines_kernel_resources_and_inherited_descriptors() {
    let fixture = Fixture::new();
    for (grants, flags, expected) in [
        ("0", &[][..], &b""[..]),
        ("1", &["--cfg=console"][..], &b"output\n"[..]),
    ] {
        let probe = fixture.0.join(grants);
        link_runtime_probe("tests/fixtures/native-sandbox/seatbelt-probe.rs", &probe, flags);

        let run = Command::new(&probe)
            .current_dir(&fixture.0)
            .arg("--output")
            .output()
            .unwrap();
        assert!(run.status.success(), "{run:?}");
        assert_eq!(run.stdout, expected);
        assert!(!fixture.0.join("sandbox-write-probe").exists());

        if !expected.is_empty() {
            let path = fixture.0.join("duplex");
            fs::write(&path, b"private data").unwrap();
            let duplex = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap();
            let refused = Command::new(&probe)
                .arg("--output")
                .stdout(duplex)
                .output()
                .unwrap();
            assert_eq!(refused.status.code(), Some(2));
            assert_eq!(
                refused.stderr,
                b"native sandbox requires write-only stdout or a terminal\n"
            );
            assert_eq!(fs::read(path).unwrap(), b"private data");
        }
    }
}

#[test]
fn macos_native_input_reads_only_when_granted() {
    let fixture = Fixture::new();
    fs::write(
        fixture.0.join("main.wy"),
        "fn main(input: Console<Read>, out: Console<Write>):\n    out.print_int(input.next_byte())\n    out.print_int(input.next_run() >> 56)\n    out.print_int(input.next_run())\n",
    ).unwrap();
    let built = fixture.run(&["build", "main.wy", "-o", "reader", "--target=macos-arm64"]);
    assert!(built.status.success(), "{built:?}");
    let reader = fixture.0.join("reader");
    let input = fixture.0.join("input");
    fs::write(&input, b"ab\n").unwrap();

    let denied = Command::new(&reader).arg("--output")
        .stdin(fs::File::open(&input).unwrap()).output().unwrap();
    assert_eq!(denied.status.code(), Some(2), "{denied:?}");
    assert!(denied.stdout.is_empty());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("Console<Read> requires --input"));

    let allowed = Command::new(&reader).args(["--output", "--input-stdin"])
        .stdin(fs::File::open(input).unwrap()).output().unwrap();
    assert!(allowed.status.success(), "{allowed:?}");
    assert_eq!(allowed.stdout, b"97\n2\n0\n");
    let launch = |grant| Command::new(&reader)
        .current_dir(&fixture.0)
        .args(["--output", grant])
        .output()
        .unwrap();
    let named = launch("--input=input");
    assert!(named.status.success(), "{named:?}");
    assert_eq!(named.stdout, allowed.stdout);
    let exceeds_file = Command::new(&reader)
        .current_dir(&fixture.0)
        .args(["--output", "--input=input", "--input-bytes=2"])
        .output()
        .unwrap();
    assert_eq!(exceeds_file.status.code(), Some(2), "{exceeds_file:?}");
    assert!(exceeds_file.stdout.is_empty());
    let exceeds_stream = Command::new(&reader)
        .current_dir(&fixture.0)
        .args(["--output", "--input-stdin", "--input-bytes=2"])
        .stdin(fs::File::open(fixture.0.join("input")).unwrap())
        .output()
        .unwrap();
    assert_eq!(exceeds_stream.status.code(), Some(2), "{exceeds_stream:?}");
    assert_eq!(exceeds_stream.stdout, b"97\n");
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::metadata(fixture.0.join("input")).unwrap();
    let identified = |inode| Command::new(&reader)
        .current_dir(&fixture.0)
        .args(["--output", "--input=input"])
        .arg(format!("--input-id={}:{}", metadata.dev(), inode))
        .output()
        .unwrap();
    let matching = identified(metadata.ino());
    assert!(matching.status.success(), "{matching:?}");
    assert_eq!(matching.stdout, allowed.stdout);
    let replaced = identified(metadata.ino() ^ 1);
    assert_eq!(replaced.status.code(), Some(2), "{replaced:?}");
    assert!(replaced.stdout.is_empty());
    std::os::unix::fs::symlink("input", fixture.0.join("input-link")).unwrap();
    let refused = launch("--input=input-link");
    assert_eq!(refused.status.code(), Some(2), "{refused:?}");
    assert!(refused.stdout.is_empty());
}

#[test]
fn macos_native_file_reads_from_explicit_grant() {
    let fixture = Fixture::new();
    fs::write(
        fixture.0.join("main.wy"),
        r#"fn main(out: Console<Write>, file: File<Read>):
    var bytes: [U8; 32] = [U8.truncate(0); 32]
    let count = file.raw_read(bytes.slice_mut())
    out.print_int(count)
    if count == 0:
        file.raw_abandon()
        return
    let closed = file.raw_close()
"#,
    )
    .unwrap();
    fs::write(fixture.0.join("input"), "native file\né\n").unwrap();
    fs::write(fixture.0.join("empty"), b"").unwrap();
    let limit = 1024 * 1024;
    for (name, length) in [("at-limit", limit), ("over-limit", limit + 1)] {
        fs::File::create(fixture.0.join(name)).unwrap().set_len(length).unwrap();
    }
    fs::create_dir(fixture.0.join("dir")).unwrap();
    std::os::unix::fs::symlink("../input", fixture.0.join("dir/link")).unwrap();
    std::os::unix::fs::symlink("dir", fixture.0.join("dir-link")).unwrap();
    let built = fixture.run(&["build", "main.wy", "-o", "reader", "--target=macos-arm64"]);
    assert!(built.status.success(), "{built:?}");
    let reader = fixture.0.join("reader");
    let options = ["--output"];
    let allowed = Command::new(&reader)
        .current_dir(&fixture.0)
        .args(options)
        .arg("--file=input")
        .output()
        .unwrap();
    assert!(allowed.status.success(), "{allowed:?}");
    assert_eq!(allowed.stdout, b"15\n");
    let empty = Command::new(&reader)
        .current_dir(&fixture.0)
        .args(options)
        .arg("--file=empty")
        .output()
        .unwrap();
    assert!(empty.status.success(), "{empty:?}");
    assert_eq!(empty.stdout, b"0\n");
    let boundary = Command::new(&reader)
        .current_dir(&fixture.0)
        .args(options)
        .arg("--file=at-limit")
        .output()
        .unwrap();
    assert!(boundary.status.success(), "{boundary:?}");
    assert_eq!(boundary.stdout, b"32\n");
    for grant in [None, Some("--file=dir/link"), Some("--file=dir-link/../input"),
                  Some("--file=over-limit"), Some("--file=dir")] {
        let denied = Command::new(&reader)
            .current_dir(&fixture.0)
            .args(options)
            .args(grant)
            .output()
            .unwrap();
        assert_eq!(denied.status.code(), Some(2), "{denied:?}");
        assert!(denied.stdout.is_empty(), "{denied:?}");
    }
}

#[test]
fn macos_native_calls_pass_and_return_wide_aggregates() {
    let fixture = Fixture::new();
    fs::write(
        fixture.0.join("main.wy"),
        r#"fn update(values: [U64; 256], increment: [U64; 40], index: U64) -> [U64; 256]:
    var result = values
    result[index] = values[0] + increment[39]
    return result

fn main(out: Console<Write>):
    if memory.in_task() || memory.first_word() != 0:
        return
    let values: [U64; 256] = [7; 256]
    let increment: [U64; 40] = [35; 40]
    let result = update(values, increment, 255)
    out.print_int(result[0])
    out.print_int(result[255])
"#,
    )
    .unwrap();
    let built = fixture.run(&["build", "main.wy", "-o", "wide", "--target=macos-arm64"]);
    assert!(built.status.success(), "{built:?}");
    let run = Command::new(fixture.0.join("wide")).arg("--output").output().unwrap();
    assert!(run.status.success(), "{run:?}");
    assert_eq!(run.stdout, b"7\n42\n");
    let capped = |limit| Command::new(fixture.0.join("wide"))
        .arg("--output")
        .arg(format!("--output-bytes={limit}"))
        .output()
        .unwrap();
    let exact = capped(5);
    assert!(exact.status.success(), "{exact:?}");
    assert_eq!(exact.stdout, run.stdout);
    let short = capped(4);
    assert_eq!(short.status.code(), Some(2), "{short:?}");
    assert_eq!(short.stdout, b"7\n");
}

#[test]
fn macos_native_listener_accepts_on_explicit_endpoint() {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::time::{Duration, Instant};

    let fixture = Fixture::new();
    fs::write(
        fixture.0.join("main.wy"),
        r#"fn main(listener: Listener):
    let entry = listener.raw_handle() * 256 + 1
    var entries: [U64; 2] = [entry, 0]
    let ready = listener.raw_poll(entries.slice_mut())
    if ready != 1 || entries[0] != entry + 4 || entries[1] != 0:
        return
    let socket = listener.raw_accept()
    if socket.raw_status() == 0 && socket.raw_handle() != listener.raw_handle():
        var authority: [U8; 32] = [U8.truncate(0); 32]
        let stalled = socket.raw_try_recv(authority.slice_mut())
        let length = socket.raw_accepted_on(authority.slice_mut())
        let address = authority.slice().part(0, length)
        if stalled == 30064771072:
            let wrote = socket.raw_try_send(address)
        let byte = socket.raw_recv_byte()
        let sent = socket.raw_send_byte(U8.truncate(byte + 1))
    let closed = socket.raw_close()
"#,
    )
    .unwrap();
    let built = fixture.run(&["build", "main.wy", "-o", "server", "--target=macos-arm64"]);
    assert!(built.status.success(), "{built:?}");
    let server = fixture.0.join("server");
    let denied = Command::new(&server).output().unwrap();
    assert_eq!(denied.status.code(), Some(2), "{denied:?}");

    let probe = TcpListener::bind("[::1]:0").unwrap();
    let endpoint = probe.local_addr().unwrap();
    drop(probe);
    let mut child = Command::new(&server).arg(format!("--listen=::1:{}", endpoint.port())).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let result = loop {
        match TcpStream::connect(endpoint) {
            Ok(mut peer) => {
                peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                let mut reply = vec![0; endpoint.to_string().len() + 1];
                let end = reply.len() - 1;
                break peer.read_exact(&mut reply[..end])
                    .and_then(|()| peer.write_all(b"a"))
                    .and_then(|()| peer.read_exact(&mut reply[end..]))
                    .map(|()| reply);
            }
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => break Err(error),
        }
    };
    assert!(child.wait().unwrap().success());
    assert_eq!(result.unwrap(), format!("{endpoint}b").as_bytes());
}

#[test]
fn macos_native_try_accept_reports_would_block_without_a_peer() {
    let fixture = Fixture::new();
    fs::write(
        fixture.0.join("main.wy"),
        r#"fn main(listener: Listener, out: Console<Write>, clock: Clock):
    let socket = listener.raw_try_accept()
    out.print_int(socket.raw_status())
    let closed = socket.raw_close()
    let due = clock.now_ms() + 10
    let timer = clock.raw_timer(due)
    let entry = timer * 256 + 1
    var entries: [U64; 2] = [entry, listener.raw_handle() * 256 + 1]
    let ready = listener.raw_poll(entries.slice_mut())
    out.print_int(ready)
    out.print_int(entries[0] - entry)
    clock.raw_timer_end(timer)
"#,
    )
    .unwrap();
    let built = fixture.run(&["build", "main.wy", "-o", "server", "--target=macos-arm64"]);
    assert!(built.status.success(), "{built:?}");
    let run = Command::new(fixture.0.join("server"))
        .args(["--output", "--clock"])
        .arg("--listen=127.0.0.1:0")
        .output()
        .unwrap();
    assert!(run.status.success(), "{run:?}");
    assert_eq!(run.stdout, b"7\n1\n4\n");
}

#[test]
fn macos_native_listener_accepts_again_after_stream_reset() {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::os::fd::AsRawFd;
    use std::time::{Duration, Instant};

    #[repr(C)]
    struct Linger {
        enabled: i32,
        seconds: i32,
    }

    unsafe extern "C" {
        fn setsockopt(fd: i32, level: i32, option: i32, value: *const Linger, size: u32) -> i32;
    }

    let fixture = Fixture::new();
    fs::write(
        fixture.0.join("main.wy"),
        r#"fn main(listener: Listener, out: Console<Write>):
    let first = listener.raw_accept()
    let marker: [U8; 1] = [U8.truncate(65)]
    let sent = first.raw_send(marker.slice())
    out.print_int(first.raw_recv_byte())
    let closed_first = first.raw_close()
    let second = listener.raw_accept()
    out.print_int(second.raw_recv_byte())
    let closed_second = second.raw_close()
"#,
    )
    .unwrap();
    let built = fixture.run(&["build", "main.wy", "-o", "server", "--target=macos-arm64"]);
    assert!(built.status.success(), "{built:?}");
    let probe = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = probe.local_addr().unwrap();
    drop(probe);
    let child = Command::new(fixture.0.join("server"))
        .arg("--output")
        .arg(format!("--listen={endpoint}"))
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let exchange = || -> std::io::Result<()> {
        let timeout = Duration::from_secs(2);
        let deadline = Instant::now() + timeout;
        let mut first = loop {
            match TcpStream::connect(endpoint) {
                Ok(stream) => break stream,
                Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                Err(error) => return Err(error),
            }
        };
        first.set_read_timeout(Some(timeout))?;
        let mut marker = [0];
        first.read_exact(&mut marker)?;
        if marker != *b"A" {
            return Err(std::io::Error::other("incorrect synchronization byte"));
        }
        const SOL_SOCKET: i32 = 0xffff;
        const SO_LINGER: i32 = 0x0080;
        let linger = Linger { enabled: 1, seconds: 0 };
        // Darwin's SO_LINGER with a zero timeout closes the TCP stream with RST.
        if unsafe {
            setsockopt(first.as_raw_fd(), SOL_SOCKET, SO_LINGER, &linger, size_of::<Linger>() as u32)
        } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        drop(first);
        TcpStream::connect_timeout(&endpoint, timeout)?.write_all(b"Z")
    };
    let result = exchange();
    let run = child.wait_with_output().unwrap();
    result.unwrap();
    assert!(run.status.success(), "{run:?}");
    assert_eq!(run.stdout, b"257\n90\n");
}

#[test]
fn macos_native_group_wait_distinguishes_two_live_sockets() {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::time::{Duration, Instant};

    let fixture = Fixture::new();
    fs::write(
        fixture.0.join("main.wy"),
        r#"fn main(listener: Listener, out: Console<Write>):
    let first = listener.raw_accept()
    let second = listener.raw_accept()
    let group = listener.raw_group()
    let first_slot = group.raw_add(move first)
    let second_slot = group.raw_add(move second)
    var entries: [U64; 2] = [first_slot * 256 + 1, second_slot * 256 + 1]
    out.print_int(group.raw_wait(entries.slice_mut()))
    out.print_int((entries[0] / 4) % 2)
    out.print_int((entries[1] / 4) % 2)
    let second_ready = group.raw_take(second_slot)
    let byte = second_ready.raw_recv_byte()
    let sent = second_ready.raw_send_byte(U8.truncate(byte + 1))
    let closed_second = second_ready.raw_close()
    let first_ready = group.raw_take(first_slot)
    out.print_int(first_ready.raw_recv_byte())
    out.print_int(first_ready.raw_send_byte(U8.truncate(66)))
    let closed_first = first_ready.raw_close()
    let closed_group = group.raw_close()
"#,
    )
    .unwrap();
    let built = fixture.run(&["build", "main.wy", "-o", "server", "--target=macos-arm64"]);
    assert!(built.status.success(), "{built:?}");
    let probe = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = probe.local_addr().unwrap();
    drop(probe);
    let mut child = Command::new(fixture.0.join("server"))
        .args(["--output", "--net-bytes=3"])
        .arg(format!("--listen={endpoint}"))
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let exchange = || -> std::io::Result<[u8; 1]> {
        let timeout = Duration::from_secs(2);
        let deadline = Instant::now() + timeout;
        let mut first = loop {
            match TcpStream::connect(endpoint) {
                Ok(stream) => break stream,
                Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                Err(error) => return Err(error),
            }
        };
        let mut second = TcpStream::connect_timeout(&endpoint, timeout)?;
        second.set_read_timeout(Some(timeout))?;
        second.write_all(b"Y")?;
        let mut reply = [0];
        second.read_exact(&mut reply)?;
        first.write_all(b"A")?;
        Ok(reply)
    };
    let reply = exchange();
    if reply.is_err() {
        let _ = child.kill();
    }
    let run = child.wait_with_output().unwrap();
    assert_eq!(reply.unwrap(), *b"Z");
    assert!(run.status.success(), "{run:?}");
    assert_eq!(run.stdout, b"1\n0\n1\n65\n3\n");
}

#[test]
fn macos_native_directory_opens_only_rooted_regular_files() {
    use std::fmt::Write;
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.0.join("root/nested")).unwrap();
    fs::write(fixture.0.join("root/nested/item"), b"A").unwrap();
    fs::write(fixture.0.join("outside"), b"Z").unwrap();
    fs::write(fixture.0.join("root/nested/skip\nme"), b"").unwrap();
    fs::create_dir(fixture.0.join("root/nested/aaa")).unwrap();
    std::os::unix::fs::symlink("../outside", fixture.0.join("root/link")).unwrap();
    std::os::unix::fs::symlink("nested", fixture.0.join("root/alias")).unwrap();

    let mut source = String::from("fn main(dir: Dir<Read>, out: Console<Write>):\n");
    for (index, path) in ["nested/item", "../outside", "link", "alias/item", "nested", "missing"].iter().enumerate() {
        for byte in path.bytes() {
            writeln!(source, "    dir.raw_path_byte(U8.truncate({byte}))").unwrap();
        }
        writeln!(source, "    let file{index} = dir.raw_open_file()").unwrap();
        writeln!(source, "    if file{index}.raw_status() == 0:").unwrap();
        writeln!(source, "        out.print_int(file{index}.raw_read_byte())").unwrap();
        writeln!(source, "    else:").unwrap();
        writeln!(source, "        out.print_int(file{index}.raw_status())").unwrap();
        writeln!(source, "    let closed{index} = file{index}.raw_close()").unwrap();
    }
    for path in ["nested/item", "nested", "alias", "../outside", "nested/.hidden"] {
        for byte in path.bytes() {
            writeln!(source, "    dir.raw_path_byte(U8.truncate({byte}))").unwrap();
        }
        source.push_str("    out.print_int(dir.raw_probe())\n");
    }
    for byte in b"nested" {
        writeln!(source, "    dir.raw_path_byte(U8.truncate({byte}))").unwrap();
    }
    source.push_str("    let child = dir.raw_open_dir()\n");
    for byte in b"nested/item" {
        writeln!(source, "    dir.raw_path_byte(U8.truncate({byte}))").unwrap();
    }
    for byte in b"item" {
        writeln!(source, "    child.raw_path_byte(U8.truncate({byte}))").unwrap();
    }
    source.push_str(
        r#"    out.print_int(dir.raw_probe())
    let closed = dir.raw_close()
    let leaf = child.raw_open_file()
    out.print_int(leaf.raw_read_byte())
    let leaf_closed = leaf.raw_close()
    let listing = child.raw_open_list()
    let child_closed = child.raw_close()
    var byte = listing.raw_read_byte()
    while byte < 256:
        out.print_byte(U8.truncate(byte))
        byte = listing.raw_read_byte()
    let listing_closed = listing.raw_close()
"#,
    );
    fs::write(fixture.0.join("main.wy"), source).unwrap();
    let built = fixture.run(&["build", "main.wy", "-o", "reader", "--target=macos-arm64"]);
    assert!(built.status.success(), "{built:?}");
    let reader = fixture.0.join("reader");
    let denied = Command::new(&reader).arg("--output").output().unwrap();
    assert_eq!(denied.status.code(), Some(2), "{denied:?}");
    fs::write(fixture.0.join("root/nested/.hidden"), b"private").unwrap();
    fs::write(fixture.0.join("root/nested/bad name"), b"private").unwrap();
    let run = Command::new(&reader)
        .current_dir(&fixture.0)
        .args(["--output", "--no-links", "--dir=root"])
        .output()
        .unwrap();
    assert!(run.status.success(), "{run:?}");
    assert_eq!(run.stdout, b"65\n2\n2\n2\n4\n1\n1\n2\n0\n0\n1\n1\n65\n.hidden\naaa/\nbad name\nitem\n");
    let restricted = Command::new(&reader)
        .current_dir(&fixture.0)
        .args(["--output", "--dir=root", "--data-only"])
        .output()
        .unwrap();
    assert!(restricted.status.success(), "{restricted:?}");
    assert_eq!(restricted.stdout, b"65\n2\n2\n2\n4\n1\n1\n2\n0\n0\n0\n1\n65\naaa/\nitem\n");
    let denied = Command::new(reader).arg("--data-only").output().unwrap();
    assert_eq!(denied.status.code(), Some(2), "{denied:?}");

}

#[test]
fn macos_native_write_file_commits_only_on_close() {
    let fixture = Fixture::new();
    fs::write(
        fixture.0.join("main.wy"),
        r#"fn main(file: File<Write>, input: Console<Read>, out: Console<Write>):
    let bytes: [U8; 2] = [U8.truncate(65), U8.truncate(66)]
    out.print_int(file.raw_write_byte(U8.truncate(33)))
    out.print_int(file.raw_write(bytes.slice()))
    if input.next_byte() == 89:
        out.print_int(file.raw_close())
    else:
        file.raw_abandon()
"#,
    )
    .unwrap();
    let built = fixture.run(&["build", "main.wy", "-o", "writer", "--target=macos-arm64"]);
    assert!(built.status.success(), "{built:?}");
    let target = fixture.0.join("target");
    fs::write(&target, b"old").unwrap();
    fs::write(fixture.0.join("abandon"), b"N").unwrap();
    fs::write(fixture.0.join("commit"), b"Y").unwrap();
    std::os::unix::fs::symlink("target", fixture.0.join("alias")).unwrap();
    let run = |input, grant| {
        Command::new(fixture.0.join("writer"))
            .current_dir(&fixture.0)
            .args(["--output", "--input", grant])
            .stdin(fs::File::open(fixture.0.join(input)).unwrap())
            .output()
            .unwrap()
    };
    let abandoned = run("abandon", "--write-file=target");
    assert!(abandoned.status.success(), "{abandoned:?}");
    assert_eq!(fs::read(&target).unwrap(), b"old");
    let refused = run("commit", "--write-file=alias");
    assert!(refused.status.success(), "{refused:?}");
    assert_eq!(refused.stdout, b"0\n2\n5\n");
    assert_eq!(fs::read(&target).unwrap(), b"old");
    let committed = run("commit", "--write-file=target");
    assert!(committed.status.success(), "{committed:?}");
    assert_eq!(committed.stdout, b"0\n2\n0\n");
    assert_eq!(fs::read(&target).unwrap(), b"!AB");
    let bounded = Command::new(fixture.0.join("writer"))
        .current_dir(&fixture.0)
        .args(["--output", "--input", "--write-file=target", "--fs-bytes=2"])
        .stdin(fs::File::open(fixture.0.join("commit")).unwrap())
        .output()
        .unwrap();
    assert!(bounded.status.success(), "{bounded:?}");
    assert_eq!(bounded.stdout, b"0\n12884901889\n0\n");
    assert_eq!(fs::read(&target).unwrap(), b"!A");
    let with_entries = |path| Command::new(fixture.0.join("writer"))
        .current_dir(&fixture.0)
        .args(["--output", "--input", path, "--fs-entries=0"])
        .stdin(fs::File::open(fixture.0.join("commit")).unwrap())
        .output()
        .unwrap();
    let refused_creation = with_entries("--write-file=new");
    assert!(refused_creation.status.success(), "{refused_creation:?}");
    assert_eq!(refused_creation.stdout, b"0\n2\n3\n");
    assert!(!fixture.0.join("new").exists());
    let replacement = with_entries("--write-file=target");
    assert!(replacement.status.success(), "{replacement:?}");
    assert_eq!(replacement.stdout, b"0\n2\n0\n");
    assert_eq!(fs::read(&target).unwrap(), b"!AB");
}

#[test]
fn macos_native_write_directory_creates_only_under_granted_root() {
    use std::fmt::Write;

    let fixture = Fixture::new();
    fs::create_dir_all(fixture.0.join("root/nested")).unwrap();
    fs::create_dir(fixture.0.join("outside")).unwrap();
    std::os::unix::fs::symlink("../outside", fixture.0.join("root/alias")).unwrap();
    let mut source = String::from("fn main(dir: Dir<Write>, out: Console<Write>):\n");
    for path in ["nested/child", "nested/child", "../outside/child", "alias/child"] {
        for byte in path.bytes() {
            writeln!(source, "    dir.raw_path_byte(U8.truncate({byte}))").unwrap();
        }
        source.push_str("    out.print_int(dir.raw_make_dir())\n");
    }
    for (index, path) in ["nested/child/file", "nested/abandoned", "alias/file"].iter().enumerate() {
        for byte in path.bytes() {
            writeln!(source, "    dir.raw_path_byte(U8.truncate({byte}))").unwrap();
        }
        writeln!(source, "    let file{index} = dir.raw_open_file()").unwrap();
        writeln!(source, "    out.print_int(file{index}.raw_status())").unwrap();
        writeln!(source, "    if file{index}.raw_status() == 0:").unwrap();
        writeln!(source, "        let wrote = file{index}.raw_write_byte(U8.truncate(65))").unwrap();
        if index == 1 {
            writeln!(source, "    file{index}.raw_abandon()").unwrap();
        } else {
            writeln!(source, "    let closed{index} = file{index}.raw_close()").unwrap();
        }
    }
    for byte in b"nested/child/file" {
        writeln!(source, "    dir.raw_path_byte(U8.truncate({byte}))").unwrap();
    }
    source.push_str(
        "    let existing = dir.raw_open_new()\n    let wrote = existing.raw_write_byte(U8.truncate(90))\n    out.print_int(existing.raw_close())\n",
    );
    for (index, path) in ["nested/child/file", "nested/child/appended"].iter().enumerate() {
        for byte in path.bytes() {
            writeln!(source, "    dir.raw_path_byte(U8.truncate({byte}))").unwrap();
        }
        writeln!(source, "    let append{index} = dir.raw_open_append()").unwrap();
        writeln!(source, "    let added{index} = append{index}.raw_write_byte(U8.truncate(66))").unwrap();
        writeln!(source, "    out.print_int(append{index}.raw_close())").unwrap();
    }
    for path in ["nested/to-remove", "nested/to-remove", "nested/child", "alias"] {
        for byte in path.bytes() {
            writeln!(source, "    dir.raw_path_byte(U8.truncate({byte}))").unwrap();
        }
        source.push_str("    out.print_int(dir.raw_remove())\n");
    }
    for target in ["alias/renamed", "nested/child/renamed"] {
        for byte in b"nested/child/appended" {
            writeln!(source, "    dir.raw_path_byte(U8.truncate({byte}))").unwrap();
        }
        source.push_str("    dir.raw_rename_from()\n");
        for byte in target.bytes() {
            writeln!(source, "    dir.raw_path_byte(U8.truncate({byte}))").unwrap();
        }
        source.push_str("    out.print_int(dir.raw_rename())\n");
    }
    for byte in b"nested/child" {
        writeln!(source, "    dir.raw_path_byte(U8.truncate({byte}))").unwrap();
    }
    source.push_str("    let child = dir.raw_open_dir()\n    let closed = dir.raw_close()\n");
    for path in ["../escaped", "grandchild"] {
        for byte in path.bytes() {
            writeln!(source, "    child.raw_path_byte(U8.truncate({byte}))").unwrap();
        }
        source.push_str("    out.print_int(child.raw_make_dir())\n");
    }
    source.push_str("    let child_closed = child.raw_close()\n");
    fs::write(fixture.0.join("main.wy"), source).unwrap();
    let built = fixture.run(&["build", "main.wy", "-o", "mkdir", "--target=macos-arm64"]);
    assert!(built.status.success(), "{built:?}");
    let removable = fixture.0.join("root/nested/to-remove");
    fs::write(&removable, b"remove").unwrap();
    let app = fixture.0.join("mkdir");
    let denied = Command::new(&app)
        .current_dir(&fixture.0)
        .args(["--output", "--dir=root"])
        .output()
        .unwrap();
    assert_eq!(denied.status.code(), Some(2));
    assert!(!fixture.0.join("root/nested/child").exists());
    let run = Command::new(&app)
        .current_dir(&fixture.0)
        .args(["--output", "--write-dir=root"])
        .output()
        .unwrap();
    assert!(run.status.success(), "{run:?}");
    assert_eq!(run.stdout, b"0\n6\n2\n5\n0\n0\n2\n6\n0\n0\n0\n1\n4\n4\n5\n0\n2\n0\n");
    assert!(fixture.0.join("root/nested/child").is_dir());
    assert!(!fixture.0.join("outside/child").exists());
    assert_eq!(fs::read(fixture.0.join("root/nested/child/file")).unwrap(), b"AB");
    assert_eq!(fs::read(fixture.0.join("root/nested/child/renamed")).unwrap(), b"B");
    assert!(!fixture.0.join("root/nested/abandoned").exists());
    assert!(!fixture.0.join("outside/file").exists());
    assert!(!fixture.0.join("outside/renamed").exists());
    assert!(!fixture.0.join("root/nested/child/appended").exists());
    assert!(fixture.0.join("root/nested/child/grandchild").is_dir());
    assert!(!fixture.0.join("root/nested/escaped").exists());
    fs::remove_dir_all(fixture.0.join("root/nested/child")).unwrap();
    assert!(!removable.exists());
    fs::write(&removable, b"remove").unwrap();
    let manifest_flags = Command::new(&app)
        .current_dir(&fixture.0)
        .args(["--output", "--dir=root", "--dir-rights=write"])
        .output()
        .unwrap();
    assert!(manifest_flags.status.success(), "{manifest_flags:?}");
    assert_eq!(manifest_flags.stdout, run.stdout);
}

#[test]
fn macos_native_combined_directory_can_read_and_write_one_root() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.0.join("root/a")).unwrap();
    fs::write(fixture.0.join("root/a/b"), b"X").unwrap();
    fs::write(
        fixture.0.join("main.wy"),
        r#"fn read(dir: &Dir<Read>, out: &Console<Write>):
    dir.raw_path_byte(U8.truncate(98))
    let file = dir.raw_open_file()
    out.print_int(file.raw_read_byte())
    let closed = file.raw_close()

fn main(dir: Dir, out: Console<Write>):
    dir.raw_path_byte(U8.truncate(97))
    let child = dir.raw_open_dir()
    let closed = dir.raw_close()
    read(&child, &out)
    child.raw_path_byte(U8.truncate(99))
    out.print_int(child.raw_make_dir())
    let child_closed = child.raw_close()
"#,
    )
    .unwrap();
    let built = fixture.run(&["build", "main.wy", "-o", "mix", "--target=macos-arm64"]);
    assert!(built.status.success(), "{built:?}");
    use std::os::unix::fs::MetadataExt;
    let identity = fs::metadata(fixture.0.join("root")).unwrap();
    let denied = Command::new(fixture.0.join("mix"))
        .current_dir(&fixture.0)
        .args(["--output", "--dir=root", "--dir-rights=read,write", "--no-links", "--data-only"])
        .arg(format!("--dir-id={}:{}", identity.dev(), identity.ino() ^ 1))
        .output()
        .unwrap();
    assert_eq!(denied.status.code(), Some(2), "{denied:?}");
    assert!(!fixture.0.join("root/a/c").exists());
    assert!(!fixture.0.join("root/.witchy-staging").exists());
    let run = Command::new(fixture.0.join("mix"))
        .current_dir(&fixture.0)
        .args(["--output", "--dir=root", "--dir-rights=read,write", "--no-links", "--data-only"])
        .arg(format!("--dir-id={}:{}", identity.dev(), identity.ino()))
        .output()
        .unwrap();
    assert!(run.status.success(), "{run:?}");
    assert_eq!(run.stdout, b"88\n0\n");
    assert!(fixture.0.join("root/a/c").is_dir());
    assert!(fixture.0.join("root/.witchy-staging").is_dir());
}

#[test]
fn macos_native_grantable_records_require_launch_bindings() {
    let fixture = Fixture::new();
    fs::write(
        fixture.0.join("main.wy"),
        r#"grantable capability Policy:
    count: U64
    enabled: Bool

fn main(policy: Policy, out: Console<Write>):
    if policy.enabled:
        out.print_int(policy.count)
"#,
    )
    .unwrap();
    let built = fixture.run(&["build", "main.wy", "-o", "policy", "--target=macos-arm64"]);
    assert!(built.status.success(), "{built:?}");
    let executable = fixture.0.join("policy");
    let run = |grants: &[&str]| Command::new(&executable).arg("--output").args(grants).output().unwrap();
    let allowed = run(&["--grant=Policy.count=42", "--grant=Policy.enabled=true"]);
    assert!(allowed.status.success(), "{allowed:?}");
    assert_eq!(allowed.stdout, b"42\n");
    for refused in [
        run(&["--grant=Policy.enabled=true"]),
        run(&["--grant=Policy.count=42", "--grant=Policy.count=43"]),
        run(&["--grant=Policy.count=18446744073709551616"]),
    ] {
        assert!(!refused.status.success(), "{refused:?}");
        assert!(refused.stdout.is_empty(), "{refused:?}");
    }
}

#[test]
fn macos_native_deadline_terminates_blocking_input() {
    use std::os::unix::process::ExitStatusExt;
    use std::process::Stdio;

    let fixture = Fixture::new();
    fs::write(
        fixture.0.join("main.wy"),
        "fn main(input: Console<Read>):\n    let byte = input.next_byte()\n",
    )
    .unwrap();
    let built = fixture.run(&["build", "main.wy", "-o", "reader", "--target=macos-arm64"]);
    assert!(built.status.success(), "{built:?}");
    let mut child = Command::new(fixture.0.join("reader"))
        .args(["--input-stdin", "--deadline-ms=40"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let pipe = child.stdin.take().unwrap();
    let bound = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= bound {
            child.kill().unwrap();
            break child.wait().unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    drop(pipe);
    assert_eq!(status.signal(), Some(14), "{status:?}");
}

#[test]
fn macos_native_data_directory_stages_writes_and_refuses_protected_names() {
    use std::fmt::Write;

    let fixture = Fixture::new();
    let root = fixture.0.join("root");
    fs::create_dir(&root).unwrap();
    let mut source = String::from("fn main(dir: Dir<Write>, out: Console<Write>):\n");
    for (index, path) in ["payload.txt", "Main.WY", "witchy.lock"].iter().enumerate() {
        for byte in path.bytes() {
            writeln!(source, "    dir.raw_path_byte(U8.truncate({byte}))").unwrap();
        }
        writeln!(source, "    let file{index} = dir.raw_open_file()").unwrap();
        writeln!(source, "    out.print_int(file{index}.raw_status())").unwrap();
        writeln!(source, "    if file{index}.raw_status() == 0:").unwrap();
        writeln!(source, "        let wrote = file{index}.raw_write_byte(U8.truncate(65))").unwrap();
        writeln!(source, "    let closed{index} = file{index}.raw_close()").unwrap();
    }
    fs::create_dir(root.join("protected")).unwrap();
    fs::write(root.join("protected/Main.WY"), b"source").unwrap();
    for (from, to) in [
        ("payload.txt", "renamed.txt"),
        ("renamed.txt", "witchy.toml"),
        ("protected", "moved"),
    ] {
        for byte in from.bytes() {
            writeln!(source, "    dir.raw_path_byte(U8.truncate({byte}))").unwrap();
        }
        source.push_str("    dir.raw_rename_from()\n");
        for byte in to.bytes() {
            writeln!(source, "    dir.raw_path_byte(U8.truncate({byte}))").unwrap();
        }
        source.push_str("    out.print_int(dir.raw_rename())\n");
    }
    source.push_str("    let done = dir.raw_close()\n");
    fs::write(fixture.0.join("main.wy"), source).unwrap();
    let built = fixture.run(&["build", "main.wy", "-o", "writer", "--target=macos-arm64"]);
    assert!(built.status.success(), "{built:?}");
    let staging = root.join(".witchy-staging");
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(&staging).unwrap();
    let launch = || {
        Command::new(fixture.0.join("writer"))
            .current_dir(&fixture.0)
            .args(["--output", "--dir=root", "--dir-rights=write", "--data-only"])
            .output()
            .unwrap()
    };
    let temporary = staging.join("123-0");
    fs::write(&temporary, b"in flight").unwrap();
    let held = fs::File::open(&staging).unwrap();
    held.lock_shared().unwrap();
    let concurrent = launch();
    assert!(concurrent.status.success(), "{concurrent:?}");
    assert_eq!(fs::read(&temporary).unwrap(), b"in flight");
    drop(held);

    let run = launch();
    assert!(run.status.success(), "{run:?}");
    assert_eq!(run.stdout, b"0\n2\n2\n0\n2\n2\n");
    assert_eq!(fs::read(root.join("renamed.txt")).unwrap(), b"A");
    assert!(!root.join("Main.WY").exists());
    assert!(!root.join("witchy.lock").exists());
    assert_eq!(fs::read_dir(root.join(".witchy-staging")).unwrap().count(), 0);
    fs::write(staging.join("unrecognized"), b"preserve").unwrap();
    let refused = launch();
    assert_eq!(refused.status.code(), Some(2), "{refused:?}");
    assert_eq!(fs::read(staging.join("unrecognized")).unwrap(), b"preserve");
}

#[test]
fn macos_native_build_session_publishes_edits_and_refuses_stale_identity() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    let fixture = Fixture::new();
    let mut child = Command::new(env!("CARGO_BIN_EXE_witchyc"))
        .current_dir(&fixture.0)
        .args(["build-session", "main.wy", "-o", "app", "--target=macos-arm64"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let mut requests = child.stdin.take().unwrap();
    let mut replies = BufReader::new(child.stdout.take().unwrap());
    let app = fixture.0.join("app");

    for (value, digest) in [(41, "374e9e47d7adaf0f5787fb744457121a24d8cb0735d42dd36af5e44ed7dd3216"), (42, "9fd3e15dc7d9420482eebfcce832fa31f6b449df9146a85d1a15864a7b3354e7")] {
        fs::write(fixture.0.join("main.wy"),
            format!("fn main(out: Console<Write>):\n    out.print_int({value})\n")).unwrap();
        writeln!(requests, "build req{value} {digest}").unwrap();
        let mut reply = String::new();
        replies.read_line(&mut reply).unwrap();
        assert!(reply.contains(r#""schema":"witchy.native-build.v1""#), "{reply}");
        assert!(reply.contains(&format!(r#""request_id":"req{value}""#)), "{reply}");
        let run = Command::new(&app).arg("--output").output().unwrap();
        assert!(run.status.success(), "{run:?}");
        assert_eq!(run.stdout, format!("{value}\n").into_bytes());
    }
    let published = fs::read(&app).unwrap();
    writeln!(requests, "build stale {:064}", 0).unwrap();
    drop(requests);
    assert!(!child.wait().unwrap().success());
    assert_eq!(fs::read(app).unwrap(), published);
}

/// A socket-entry worker's connection deadline starts after its sandbox install (finding
/// 2026-11-23-socket-worker-setup-inside-connection-deadline): the setup bound ends a stalled
/// setup with its own status, and the re-arm after `sandbox_init` takes effect.
#[test]
fn macos_worker_setup_bound_and_connection_deadline_rearm_under_seatbelt() {
    let fixture = Fixture::new();
    let archive = fixture.0.join("setup-deadline.a");
    let probe = fixture.0.join("setup-deadline");
    let compiled = Command::new(option_env!("RUSTC").unwrap_or("rustc"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["--edition=2024", "--crate-type=staticlib", "-Adead_code", "-Copt-level=1", "-Cpanic=abort"])
        .arg("tests/fixtures/native-sandbox/macos-setup-deadline.rs")
        .arg("-o")
        .arg(&archive)
        .output()
        .unwrap();
    assert!(compiled.status.success(), "{compiled:?}");
    let linked = Command::new("/usr/bin/clang").arg("-Wl,-dead_strip").arg(&archive).arg("-o").arg(&probe).output().unwrap();
    assert!(linked.status.success(), "{linked:?}");
    let ran = Command::new(&probe).output().unwrap();
    assert!(ran.status.success(), "{ran:?}");
    assert_eq!(ran.stdout, b"setup bound exits 124; connection deadline re-armed under Seatbelt\n");
}

#[test]
fn macos_native_socket_entry_runs_once_per_connection() {
    use super::native_manifest::await_loopback_listener;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::{Duration, Instant};

    let fixture = Fixture::new();
    fs::write(fixture.0.join("main.wy"), r#"fn main(socket: Socket, out: Console<Write>):
    let byte = socket.raw_recv_byte()
    let sent = socket.raw_send_byte(U8.truncate(byte + 1))
    let extra = socket.raw_send_byte(U8.truncate(33))
    let closed = socket.raw_close()
    out.print_byte(U8.truncate(byte))
    out.print_byte(U8.truncate(33))
"#).unwrap();
    let built = fixture.run(&["build", "main.wy", "-o", "server", "--target=macos-arm64"]);
    assert!(built.status.success(), "{built:?}");
    let server = fixture.0.join("server");
    let denied = Command::new(&server).arg("--listen=127.0.0.1:0").output().unwrap();
    assert_eq!(denied.status.code(), Some(2), "{denied:?}");
    let mut idle = Command::new(&server)
        .args(["--listen=127.0.0.1:0", "--instance-per-connection", "--deadline-ms=50", "--limit-name=--deadline-ms=[run] deadline-ms", "--output"])
        .stderr(std::process::Stdio::piped())
        .spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let status = loop {
        if let Some(status) = idle.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = idle.kill();
            let _ = idle.wait();
            panic!("idle server outlived its run deadline");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(status.code(), Some(2), "{status:?}");
    let mut diagnostic = String::new();
    idle.stderr.take().unwrap().read_to_string(&mut diagnostic).unwrap();
    let mut lines = diagnostic.lines();
    let bound = lines.next().unwrap().strip_prefix("witchy: listening on ").unwrap();
    let bound: std::net::SocketAddr = bound.parse().unwrap();
    assert!(bound.ip().is_loopback() && bound.port() != 0, "{bound}");
    assert_eq!(lines.next(), Some("witchy: limit reached: [run] deadline-ms"));
    assert_eq!(lines.next(), None);

    // Each server below picks its own port and announces it: a port probed here and released
    // could be taken by another test's listener first, whose client then saw a reset.
    let stalled_log = fixture.0.join("stalled.stderr");
    let mut stalled = Command::new(&server)
        .args(["--listen=127.0.0.1:0", "--instance-per-connection", "--connections=2", "--waiting-connections=1", "--connection-deadline-ms=500",
               "--deadline-ms=20000", "--output"])
        .stderr(fs::File::create(&stalled_log).unwrap())
        .spawn().unwrap();
    let stalled_endpoint = await_loopback_listener(&mut stalled, &stalled_log, Duration::from_secs(10));
    let mut peer = TcpStream::connect_timeout(&stalled_endpoint, Duration::from_secs(10)).unwrap();
    // Each worker's sandbox install is outside its connection deadline and can take most of a
    // second on a loaded host (450-950 ms measured with three suites running), so the clients
    // wait longer than the deadline itself. The deadline is below those installs, so a
    // deadline that counted them again would fail here under load; it is above the guest's own
    // time under the same load (up to 83 ms measured), which at 200 ms it sometimes exceeded.
    peer.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
    let mut resumed = TcpStream::connect(stalled_endpoint).unwrap();
    resumed.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
    resumed.write_all(b"m").unwrap();
    let closed = peer.read(&mut [0]);
    drop(peer);
    assert_eq!(closed.unwrap(), 0);
    let mut reply = [0; 2];
    resumed.read_exact(&mut reply).unwrap();
    assert_eq!(&reply, b"n!");
    let stopped = stalled.wait().unwrap();
    assert!(stopped.success(), "{stopped:?}");
    assert_eq!(fs::read_to_string(&stalled_log).unwrap(),
        format!("witchy: listening on {stalled_endpoint}\nwitchy: limit reached: --connection-deadline-ms\n"));

    // A worker whose sandbox install fails (here: standard output open for reading too) fails
    // its own connection, reported, and the next queued client gets a worker of its own; a
    // failure three times in a row stops the service rather than resetting every client.
    let both_ways = fs::OpenOptions::new().read(true).write(true).create(true).truncate(true)
        .open(fixture.0.join("both-ways")).unwrap();
    let failing_log = fixture.0.join("failing.stderr");
    let mut failing = Command::new(&server)
        .args(["--listen=127.0.0.1:0", "--instance-per-connection", "--connections=5", "--waiting-connections=4", "--deadline-ms=20000", "--output"])
        .stdout(both_ways)
        .stderr(fs::File::create(&failing_log).unwrap())
        .spawn().unwrap();
    let refused_endpoint = await_loopback_listener(&mut failing, &failing_log, Duration::from_secs(10));
    let deadline = Instant::now() + Duration::from_secs(10);
    let peers: Vec<TcpStream> = (0..3).map(|_| loop {
        match TcpStream::connect(refused_endpoint) {
            Ok(mut peer) => {
                peer.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
                let _ = peer.write_all(b"a");
                break peer;
            }
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => panic!("could not connect to the failing server: {error}"),
        }
    }).collect();
    for mut peer in peers {
        let mut reply = Vec::new();
        let _ = peer.read_to_end(&mut reply);
        assert!(reply.is_empty(), "{reply:?}");
    }
    let failed = failing.wait().unwrap();
    assert_eq!(failed.code(), Some(2), "{failed:?}");
    let refusal = "native sandbox requires write-only stdout or a terminal\n\
        witchy: a connection's worker could not install its deadline or sandbox; that connection is closed\n";
    assert_eq!(fs::read_to_string(&failing_log).unwrap(),
        format!("witchy: listening on {refused_endpoint}\n{refusal}{refusal}{refusal}witchy: workers failed to install their sandbox three times in a row: the service stops\n"));

    let diagnostics = fixture.0.join("budget-server.stderr");
    let mut child = Command::new("/bin/sh")
        .args(["-c", "ulimit -n 16 || exit 1; exec 0<&-; exec \"$@\"", "socket-fixture"])
        .arg(&server)
        .args(["--listen=127.0.0.1:0", "--instance-per-connection", "--connections=3", "--waiting-connections=4096", "--connection-deadline-ms=1000", "--net-bytes=4", "--net-bytes-per-connection=2",
               "--output", "--output-bytes=2", "--output-bytes-per-connection=1", "--deadline-ms=15000"])
        .stdout(std::process::Stdio::piped())
        .stderr(fs::File::create(&diagnostics).unwrap())
        .spawn().unwrap();
    // Three workers in turn, each installing its sandbox first (most of a second each on a
    // loaded host): the run's deadline and the clients' waits leave room for that.
    let result = (|| -> std::io::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let endpoint: std::net::SocketAddr = loop {
            if let Some(address) = fs::read_to_string(&diagnostics)?.split_inclusive('\n').find_map(|line| {
                line.strip_suffix('\n')?.strip_prefix("witchy: listening on ")?.parse().ok()
            }) {
                break address;
            }
            if child.try_wait()?.is_some() || Instant::now() >= deadline {
                return Err(std::io::Error::other("budget server did not announce its listener"));
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        for sent in [Some(b'a'), Some(b'x'), None] {
            let mut peer = loop {
                match TcpStream::connect(endpoint) {
                    Ok(peer) => break peer,
                    Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                    Err(error) => return Err(error),
                }
            };
            peer.set_read_timeout(Some(Duration::from_secs(10)))?;
            let mut reply = [0];
            if let Some(sent) = sent {
                peer.write_all(&[sent])?;
                peer.read_exact(&mut reply)?;
                assert_eq!(reply, [sent + 1]);
                assert_eq!(peer.read(&mut reply)?, 0, "connection budget was exceeded");
            } else {
                peer.shutdown(std::net::Shutdown::Write)?;
                assert_eq!(peer.read(&mut reply)?, 0, "run budget was reset");
            }
        }
        Ok(())
    })();
    if result.is_err() { let _ = child.kill(); }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success() && result.is_ok(), "{result:?} {output:?}: {}", fs::read_to_string(&diagnostics).unwrap());
    assert_eq!(output.stdout, b"ax");
}

#[test]
fn macos_native_plain_output_marks_lines_and_escapes_bytes() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("main.wy"), r#"fn main(out: Console<Write>):
    out.print_byte(U8.truncate(27))
    out.print_byte(U8.truncate(13))
    out.print_byte(U8.truncate(255))
    out.print_byte(U8.truncate(10))
    out.print_int(42)
"#).unwrap();
    let built = fixture.run(&["build", "main.wy", "-o", "output", "--target=macos-arm64"]);
    assert!(built.status.success(), "{built:?}");
    let executable = fixture.0.join("output");
    let denied = Command::new(&executable).args(["--output", "--plain-output"]).output().unwrap();
    assert_eq!(denied.status.code(), Some(2));
    let metered = Command::new(&executable).args(["--output", "--fuel=1000"]).output().unwrap();
    assert_eq!(metered.status.code(), Some(2));
    assert!(metered.stdout.is_empty());
    assert!(String::from_utf8_lossy(&metered.stderr).contains("native instruction fuel is unavailable"));
    let limited = Command::new(&executable)
        .args(["--output", "--output-bytes=0", "--limit-name=--output-bytes=[run] output-bytes"])
        .output().unwrap();
    assert_eq!(limited.status.code(), Some(2));
    assert!(limited.stdout.is_empty());
    let run = Command::new(executable)
        .args(["--output", "--stream-output", "--plain-output", "--output-bytes=7", "--limit-name=--output-bytes=[run] output-bytes", "--unmetered"])
        .output().unwrap();
    assert!(run.status.success(), "{run:?}");
    assert_eq!(run.stdout, b"| \\x1b\\x0d\\xff\n| 42\n");
}

#[test]
fn macos_native_public_digests_match_shared_vectors() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("main.wy"), r#"fn main(out: Console<Write>):
    let abc: [U8; 3] = [97, 98, 99]
    var small: [U8; 32] = [0; 32]
    var wide: [U8; 48] = [0; 48]
    out.print_int(provider.raw_sha256(abc.slice(), small.slice_mut()))
    for index in 0..32:
        out.print_byte(small[index])
    out.print_int(provider.raw_sha256(abc.slice().part(0, 0), small.slice_mut()))
    for index in 0..32:
        out.print_byte(small[index])
    out.print_int(provider.raw_sha384(abc.slice(), wide.slice_mut()))
    for index in 0..48:
        out.print_byte(wide[index])
    out.print_int(provider.raw_sha384(abc.slice(), small.slice_mut()))
"#).unwrap();
    let built = fixture.run(&["build", "main.wy", "-o", "digests", "--target=macos-arm64"]);
    assert!(built.status.success(), "{built:?}");
    #[cfg(witchy_native_aws_lc)]
    {
        let libraries = Command::new("/usr/bin/otool")
            .arg("-L").arg(fixture.0.join("digests")).output().unwrap();
        assert!(libraries.status.success(), "{libraries:?}");
        assert!(String::from_utf8_lossy(&libraries.stdout).contains("libaws_lc_fips"), "{libraries:?}");
    }
    let run = Command::new(fixture.0.join("digests")).arg("--output").output().unwrap();
    assert!(run.status.success(), "{run:?}");
    let expected = include_str!("../fixtures/crypto/sha256.expected");
    let digest = |index| expected.lines().nth(index).unwrap().as_bytes().chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(run.stdout, [b"0\n".to_vec(), digest(0), b"0\n".to_vec(), digest(1), b"0\n".to_vec(), digest(3), b"2\n".to_vec()].concat());
}

#[cfg(witchy_native_aws_lc)]
#[test]
fn macos_native_pkix_checks_identity_and_der_signature() {
    let fixture = Fixture::new();
    let probe = fixture.0.join("pkix");
    link_runtime_probe("tests/fixtures/crypto/pkix-kat/native.rs", &probe, &[]);
    let run = Command::new(&probe).output().unwrap();
    assert!(run.status.success(), "{run:?}");
}

#[cfg(witchy_native_aws_lc)]
#[test]
fn macos_native_p384_agrees_and_rejects_off_curve_points() {
    // The Wasm fixture's exchanges, reporting each passed check by number through the console
    // intrinsic instead of the text library: the subject is native P-384 and its grant, and
    // without the proved library the build needs no proof checker (the library's native builds
    // are the buffered-I/O and Exec tests).
    let mut source = include_str!("../fixtures/crypto/p384-agreement.wy").to_owned();
    let expected = include_str!("../fixtures/crypto/p384-agreement.expected");
    assert!(source.starts_with("import console\n"));
    source.replace_range(.."import console\n".len(), "");
    for (index, line) in expected.lines().enumerate() {
        let call = format!("out.line(\"{line}\")");
        assert_eq!(source.matches(&call).count(), 1, "{call}");
        source = source.replace(&call, &format!("out.print_int({})", index + 1));
    }
    assert!(!source.contains("out.line"));
    let fixture = Fixture::new();
    fs::write(fixture.0.join("main.wy"), source).unwrap();
    let built = fixture.run(&["build", "main.wy", "-o", "agreement", "--target=macos-arm64"]);
    assert!(built.status.success(), "{built:?}");

    let executable = fixture.0.join("agreement");
    let without_rand = Command::new(&executable).arg("--output").output().unwrap();
    assert!(!without_rand.status.success(), "{without_rand:?}");
    assert!(without_rand.stdout.is_empty(), "{without_rand:?}");
    let granted = Command::new(&executable).args(["--output", "--rand"]).output().unwrap();
    assert!(granted.status.success(), "{granted:?}");
    let numbered: String = (1..=expected.lines().count()).map(|n| format!("{n}\n")).collect();
    assert_eq!(String::from_utf8_lossy(&granted.stdout), numbered);
}

#[cfg(witchy_native_aws_lc)]
#[test]
fn macos_native_signer_binds_each_checked_parameter() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("main.wy"), r#"fn main(out: Console<Write>, targets: Signer, root: Signer):
    var short: [U8; 1] = [0]
    out.print_int(targets.raw_public_key(short.slice_mut()))
    out.print_int(root.raw_public_key(short.slice_mut()))
"#).unwrap();
    let built = fixture.run(&["build", "main.wy", "-o", "signers", "--target=macos-arm64"]);
    assert!(built.status.success(), "{built:?}");
    fs::write(fixture.0.join("targets-seed"), [1; 32]).unwrap();
    fs::write(fixture.0.join("root-seed"), [2; 32]).unwrap();
    let launch = |bindings: &[&str], root_seed: &str| {
        Command::new("/bin/sh")
            .current_dir(&fixture.0)
            .args(["-c", "exec 3<targets-seed 4<\"$1\"; shift; exec ./signers --output \"$@\"", "signer-fixture"])
            .arg(root_seed)
            .args(bindings)
            .output().unwrap()
    };
    let targets = "--secret-slot=targets:sign/witchy-2/targets:fd:3";
    let root = "--secret-slot=root:sign/witchy-2/root:fd:4";
    let valid = launch(&[root, targets], "root-seed");
    assert!(valid.status.success(), "{valid:?}");
    assert_eq!(valid.stdout, b"2\n2\n");

    for bindings in [
        vec![targets],
        vec![targets, targets, root],
        vec![targets, "--secret-slot=other:sign/witchy-2/root:fd:4"],
    ] {
        let refused = launch(&bindings, "root-seed");
        assert_eq!(refused.status.code(), Some(2), "{refused:?}");
        assert!(refused.stdout.is_empty(), "{refused:?}");
    }
    let alias = launch(&[targets, root], "targets-seed");
    assert_eq!(alias.status.code(), Some(2), "{alias:?}");
    assert!(alias.stdout.is_empty(), "{alias:?}");
}

#[cfg(witchy_native_aws_lc)]
#[test]
fn macos_native_environment_secrets_are_exclusive_of_env_grants() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("main.wy"), r#"fn main(out: Console<Write>, token: Secret<Reveal>, key: Secret<Seal>, values: Env):
    out.print_int(token.raw_byte(0))
    out.print_int(token.raw_byte(1))
    out.print_int(key.raw_hmac())
    for index in 0..32:
        out.print_byte(U8.truncate(key.raw_digest_byte(index)))
"#).unwrap();
    let built = fixture.run(&["build", "main.wy", "-o", "secrets", "--target=macos-arm64"]);
    assert!(built.status.success(), "{built:?}");
    let launch = |flags: &[&str]| Command::new(fixture.0.join("secrets"))
        .env_clear()
        .env("TOKEN", "A").env("KEY", "demo")
        .arg("--output").args(flags).output().unwrap();
    let reveal = "--secret-slot=token:reveal-bytes:env:TOKEN";
    let seal = "--secret-slot=key:seal:env:KEY";
    let valid = launch(&[reveal, seal, "--env=OTHER=visible"]);
    assert!(valid.status.success(), "{valid:?}");
    let digest = "49611dcdf795fccf2070d8817b36e3b251c6ede27f32481721f8e7c8dac4f776";
    let expected: Vec<_> = digest.as_bytes().chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect();
    assert_eq!(valid.stdout, [b"65\n256\n0\n".as_slice(), &expected].concat());

    for flags in [["--env=KEY", reveal, seal], [reveal, seal, "--env=KEY"]] {
        let refused = launch(&flags);
        assert_eq!(refused.status.code(), Some(2), "{refused:?}");
        assert!(refused.stdout.is_empty(), "{refused:?}");
        assert!(String::from_utf8_lossy(&refused.stderr).contains("also granted through Env"));
    }
}

#[cfg(witchy_native_aws_lc)]
#[test]
fn macos_native_secret_store_preserves_lookup_rights() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("main.wy"), r#"fn main(out: Console<Write>, store: SecretStore):
    store.raw_name_byte(97)
    let full = store.raw_get()
    out.print_int(full.raw_status())
    out.print_int(full.raw_byte(0))
    store.raw_name_byte(98)
    let refused = store.raw_get()
    out.print_int(refused.raw_status())
    store.raw_name_byte(98)
    let key = store.raw_get_sealed()
    out.print_int(key.raw_status())
    out.print_int(key.raw_hmac())
    for index in 0..32:
        out.print_byte(U8.truncate(key.raw_digest_byte(index)))
    store.raw_name_byte(99)
    let missing = store.raw_get()
    out.print_int(missing.raw_status())
"#).unwrap();
    let built = fixture.run(&["build", "main.wy", "-o", "secrets", "--target=macos-arm64"]);
    assert!(built.status.success(), "{built:?}");
    let run = Command::new(fixture.0.join("secrets"))
        .env_clear()
        .env("a", "visible")
        .args(["--output", "--secret=a", "--secret-seal=b=demo"])
        .output().unwrap();
    assert!(run.status.success(), "{run:?}");
    let digest = "49611dcdf795fccf2070d8817b36e3b251c6ede27f32481721f8e7c8dac4f776";
    let expected: Vec<_> = digest.as_bytes().chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect();
    assert_eq!(run.stdout, [b"0\n118\n2\n0\n0\n".as_slice(), &expected, b"1\n"].concat());
}

#[test]
fn macos_native_buffered_file_copy_preserves_the_old_file_until_commit() {
    use std::io::Read;

    let binding = std::env::var_os("WITCHY_WASM_TEST_READY_BINDING");
    let lean = std::env::var_os("WITCHY_WASM_TEST_LEAN_ROOT");
    if binding.is_none() && lean.is_none() {
        eprintln!("skipped: buffered I/O needs a configured proof checker");
        return;
    }

    let fixture = Fixture::new();
    let input = b"\x00Witchy\xff\n";
    fs::write(fixture.0.join("input"), input).unwrap();
    fs::write(fixture.0.join("output"), b"existing").unwrap();
    fs::write(
        fixture.0.join("copy.wy"),
        include_str!("../fixtures/bufio/files.wy"),
    )
    .unwrap();
    let mut compiler = Command::new(env!("CARGO_BIN_EXE_witchyc"));
    if let Some(binding) = binding {
        compiler.env("WITCHY_WASM_READY_BINDING", binding);
    }
    if let Some(lean) = lean {
        compiler.env("WITCHY_WASM_LEAN_ROOT", lean);
    }
    let turn = super::share_library_proofs(&mut compiler);
    let built = compiler
        .current_dir(&fixture.0)
        .args(["build", "copy.wy", "-o", "copy", "--target=macos-arm64"])
        .output()
        .unwrap();
    drop(turn);
    assert!(built.status.success(), "{built:?}");

    let run = |flags: &[&str]| {
        Command::new(fixture.0.join("copy"))
            .current_dir(&fixture.0)
            .args(["--allocator", "--output", "--file=input"])
            .args(flags)
            .output()
            .unwrap()
    };
    let missing = run(&[]);
    assert_eq!(missing.status.code(), Some(2), "{missing:?}");
    assert!(missing.stdout.is_empty());
    assert_eq!(fs::read(fixture.0.join("output")).unwrap(), b"existing");

    // Nine bytes for the input snapshot, then room for only four output bytes.
    let refused = run(&["--write-file=output", "--fs-bytes=13"]);
    assert!(refused.status.success(), "{refused:?}");
    assert!(refused.stdout.is_empty());
    assert_eq!(fs::read(fixture.0.join("output")).unwrap(), b"existing");

    let mut existing = fs::File::open(fixture.0.join("output")).unwrap();
    let copied = run(&["--write-file=output"]);
    assert!(copied.status.success(), "{copied:?}");
    assert_eq!(copied.stdout, b"copied\n");
    assert_eq!(fs::read(fixture.0.join("output")).unwrap(), input);
    let mut previous_bytes = Vec::new();
    existing.read_to_end(&mut previous_bytes).unwrap();
    assert_eq!(previous_bytes, b"existing");
}
