use super::{Command, Fixture, Output, fs};

fn success(result: Output) -> Vec<u8> {
    assert!(result.status.success(), "{result:?}");
    result.stdout
}

#[test]
fn linux_relay_sandbox_keeps_only_the_broker_channel() {
    let fixture = Fixture::new();
    let executable = fixture.0.join("relay-probe");
    success(
        Command::new(option_env!("RUSTC").unwrap_or("rustc"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(["--edition=2024", "-Copt-level=1", "-Cpanic=abort"])
            .arg("tests/fixtures/native-sandbox/linux-relay.rs")
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap(),
    );
    assert_eq!(
        success(Command::new(executable).output().unwrap()),
        b"Linux relay confinement passed\n"
    );
}

#[test]
fn linux_broker_sandbox_limits_filesystem_and_sockets() {
    let fixture = Fixture::new();
    let executable = fixture.0.join("broker-probe");
    success(
        Command::new(option_env!("RUSTC").unwrap_or("rustc"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(["--edition=2024", "-Copt-level=1", "-Cpanic=abort"])
            .arg("tests/fixtures/native-sandbox/linux-broker.rs")
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap(),
    );
    assert_eq!(
        success(Command::new(executable).output().unwrap()),
        b"Linux broker confinement passed\n"
    );
}

#[test]
fn linux_launch_sources_use_opened_descriptors_and_boundaries() {
    let fixture = Fixture::new();
    let executable = fixture.0.join("launch-tests");
    success(
        Command::new(option_env!("RUSTC").unwrap_or("rustc"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(["--edition=2024", "--test", "-Adead_code", "-Copt-level=1"])
            .arg("tests/fixtures/native-sandbox/linux-plan.rs")
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap(),
    );
    let output = success(Command::new(executable).output().unwrap());
    assert!(
        String::from_utf8_lossy(&output).contains("resolves_parent_under_bound_directory ... ok")
    );
}

#[test]
fn linux_socket_primitives_handle_addresses_and_loopback() {
    let fixture = Fixture::new();
    let executable = fixture.0.join("socket-tests");
    success(
        Command::new(option_env!("RUSTC").unwrap_or("rustc"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(["--edition=2024", "--test", "-Adead_code", "-Copt-level=1"])
            .arg("tests/fixtures/native-sandbox/linux-socket.rs")
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap(),
    );
    let output = success(Command::new(executable).output().unwrap());
    assert!(
        String::from_utf8_lossy(&output)
            .contains("local_listener_and_dial_use_native_sockaddr_layout ... ok")
    );
}

#[test]
fn linux_broker_transport_checks_frames_and_eof() {
    let fixture = Fixture::new();
    let executable = fixture.0.join("transport-tests");
    success(
        Command::new(option_env!("RUSTC").unwrap_or("rustc"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(["--edition=2024", "--test", "-Adead_code", "-Copt-level=1"])
            .arg("tests/fixtures/native-sandbox/linux-transport.rs")
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap(),
    );
    let output = success(Command::new(executable).output().unwrap());
    assert!(
        String::from_utf8_lossy(&output)
            .contains("transport_delivers_exact_frame_and_detects_eof ... ok")
    );
}

#[test]
fn linux_file_grants_refuse_symlinks_and_hard_links() {
    let fixture = Fixture::new();
    let executable = fixture.0.join("file-tests");
    success(
        Command::new(option_env!("RUSTC").unwrap_or("rustc"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(["--edition=2024", "--test", "-Adead_code", "-Copt-level=1"])
            .arg("tests/fixtures/native-sandbox/linux-file.rs")
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap(),
    );
    let output = success(Command::new(executable).output().unwrap());
    let output = String::from_utf8_lossy(&output);
    assert!(output.contains("opening_rejects_symlinks_in_every_path_component ... ok"));
    assert!(output.contains("opened_snapshot_refuses_hard_links ... ok"));
}

#[test]
fn linux_directory_broker_protects_files_and_secrets() {
    let fixture = Fixture::new();
    let executable = fixture.0.join("directory-tests");
    success(
        Command::new(option_env!("RUSTC").unwrap_or("rustc"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(["--edition=2024", "--test", "-Adead_code", "-Copt-level=1"])
            .arg("tests/fixtures/native-sandbox/linux-directory.rs")
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap(),
    );
    let output = success(
        Command::new(executable)
            .arg("--test-threads=1")
            .output()
            .unwrap(),
    );
    let output = String::from_utf8_lossy(&output);
    assert!(output.contains("staged_publication_replaces_file_and_refuses_hard_links ... ok"));
    assert!(output.contains("protected_tree_finds_secrets_inside_nested_directory ... ok"));
}

#[test]
fn linux_exec_retains_the_granted_executable_identity() {
    let fixture = Fixture::new();
    let executable = fixture.0.join("exec-program-tests");
    success(
        Command::new(option_env!("RUSTC").unwrap_or("rustc"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(["--edition=2024", "--test", "-Adead_code", "-Copt-level=1"])
            .arg("tests/fixtures/native-sandbox/linux-exec-programs.rs")
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap(),
    );
    let output = success(
        Command::new(executable)
            .arg("--test-threads=1")
            .output()
            .unwrap(),
    );
    assert!(
        String::from_utf8_lossy(&output)
            .contains("setup_records_opened_executable_identity ... ok")
    );
}

#[test]
fn linux_endpoints_resolve_numeric_and_localhost_addresses() {
    let fixture = Fixture::new();
    let executable = fixture.0.join("endpoint-tests");
    success(
        Command::new(option_env!("RUSTC").unwrap_or("rustc"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(["--edition=2024", "--test", "-Adead_code", "-Copt-level=1"])
            .arg("tests/fixtures/native-sandbox/linux-endpoint.rs")
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap(),
    );
    let output = success(Command::new(executable).output().unwrap());
    assert!(
        String::from_utf8_lossy(&output)
            .contains("numeric_and_localhost_dns_resolve_with_correct_abi ... ok")
    );
}

#[test]
fn linux_deadline_delivers_alarm() {
    let fixture = Fixture::new();
    let archive = fixture.0.join("deadline.a");
    let executable = fixture.0.join("deadline-probe");
    success(
        Command::new(option_env!("RUSTC").unwrap_or("rustc"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args([
                "--edition=2024",
                "--crate-type=staticlib",
                "-Adead_code",
                "-Copt-level=1",
                "-Cpanic=abort",
            ])
            .arg("tests/fixtures/native-sandbox/linux-deadline.rs")
            .arg("-o")
            .arg(&archive)
            .output()
            .unwrap(),
    );
    success(
        Command::new("cc")
            .arg(&archive)
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap(),
    );
    assert_eq!(
        success(Command::new(executable).output().unwrap()),
        b"Linux deadline delivered SIGALRM\n"
    );
}

#[test]
fn linux_exec_drops_inheritable_as_well_as_effective_capabilities() {
    let fixture = Fixture::new();
    let executable = fixture.0.join("exec-security-tests");
    success(
        Command::new(option_env!("RUSTC").unwrap_or("rustc"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(["--edition=2024", "--test", "-Adead_code", "-Copt-level=1"])
            .arg("tests/fixtures/native-sandbox/linux-exec-security.rs")
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap(),
    );
    let output = success(Command::new(executable).output().unwrap());
    assert!(
        String::from_utf8_lossy(&output)
            .contains("inheritable_set_is_cleared_when_permitted_set_is_empty ... ok")
    );
}

#[test]
fn linux_exec_process_limits_count_the_real_uid() {
    let fixture = Fixture::new();
    let executable = fixture.0.join("exec-limits-tests");
    success(
        Command::new(option_env!("RUSTC").unwrap_or("rustc"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(["--edition=2024", "--test", "-Adead_code", "-Copt-level=1"])
            .arg("tests/fixtures/native-sandbox/linux-exec-limits.rs")
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap(),
    );
    let output = success(
        Command::new(executable)
            .arg("--test-threads=1")
            .output()
            .unwrap(),
    );
    assert!(
        String::from_utf8_lossy(&output).contains("parses_only_real_uid_and_thread_count ... ok")
    );
}

#[test]
fn linux_exec_runner_passes_only_one_owned_descriptor() {
    let fixture = Fixture::new();
    let executable = fixture.0.join("exec-transport-tests");
    success(
        Command::new(option_env!("RUSTC").unwrap_or("rustc"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(["--edition=2024", "--test", "-Adead_code", "-Copt-level=1"])
            .arg("tests/fixtures/native-sandbox/linux-exec-transport.rs")
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap(),
    );
    let output = success(
        Command::new(executable)
            .arg("--test-threads=1")
            .output()
            .unwrap(),
    );
    let output = String::from_utf8_lossy(&output);
    assert!(output.contains("transfers_directory_descriptor_and_sets_cloexec ... ok"));
    assert!(output.contains("refuses_multiple_descriptors_without_leaking_them ... ok"));
}

#[test]
fn linux_exec_subreaper_stops_its_children() {
    let fixture = Fixture::new();
    let executable = fixture.0.join("exec-children-tests");
    success(
        Command::new(option_env!("RUSTC").unwrap_or("rustc"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(["--edition=2024", "--test", "-Adead_code", "-Copt-level=1"])
            .arg("tests/fixtures/native-sandbox/linux-exec-children.rs")
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap(),
    );
    let output = success(
        Command::new(executable)
            .arg("--test-threads=1")
            .output()
            .unwrap(),
    );
    let output = String::from_utf8_lossy(&output);
    assert!(output.contains("subreaper_stops_its_own_child ... ok"));
    assert!(output.contains("runner_stops_a_child_that_created_a_new_session ... ok"));
}

#[test]
fn linux_exec_captures_output_and_enforces_limits() {
    let fixture = Fixture::new();
    let executable = fixture.0.join("exec-capture-tests");
    success(
        Command::new(option_env!("RUSTC").unwrap_or("rustc"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(["--edition=2024", "--test", "-Adead_code", "-Copt-level=1"])
            .arg("tests/fixtures/native-sandbox/linux-exec-capture.rs")
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap(),
    );
    let output = success(
        Command::new(executable)
            .arg("--test-threads=1")
            .output()
            .unwrap(),
    );
    let output = String::from_utf8_lossy(&output);
    assert!(output.contains("collects_both_streams_and_exit_status ... ok"));
    assert!(output.contains("enforces_output_and_time_limits ... ok"));
}

#[test]
fn linux_exec_removes_private_program_copies() {
    let fixture = Fixture::new();
    let executable = fixture.0.join("exec-temp-tests");
    success(
        Command::new(option_env!("RUSTC").unwrap_or("rustc"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(["--edition=2024", "--test", "-Adead_code", "-Copt-level=1"])
            .arg("tests/fixtures/native-sandbox/linux-exec-temp.rs")
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap(),
    );
    let output = success(
        Command::new(executable)
            .arg("--test-threads=1")
            .output()
            .unwrap(),
    );
    assert!(
        String::from_utf8_lossy(&output)
            .contains("private_directory_is_removed_and_names_are_checked ... ok")
    );
}

#[test]
fn linux_exec_handler_runs_a_granted_program() {
    let fixture = Fixture::new();
    let executable = fixture.0.join("exec-runner-tests");
    success(
        Command::new(option_env!("RUSTC").unwrap_or("rustc"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(["--edition=2024", "--test", "-Adead_code", "-Copt-level=1"])
            .arg("tests/fixtures/native-sandbox/linux-exec-runner.rs")
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap(),
    );
    let output = success(
        Command::new(executable)
            .args([
                "--exact",
                "exec_broker::linux_runner_executes_granted_program",
                "--test-threads=1",
            ])
            .output()
            .unwrap(),
    );
    assert!(
        String::from_utf8_lossy(&output).contains("linux_runner_executes_granted_program ... ok")
    );
}

#[test]
fn linux_exec_warden_handles_run_and_runner_death() {
    let fixture = Fixture::new();
    let executable = fixture.0.join("exec-warden-tests");
    success(
        Command::new(option_env!("RUSTC").unwrap_or("rustc"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(["--edition=2024", "--test", "-Adead_code", "-Copt-level=1"])
            .arg("tests/fixtures/native-sandbox/linux-exec-runner.rs")
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap(),
    );
    for name in [
        "warden_runs_granted_program_and_reaps_runner",
        "warden_cleans_up_after_runner_is_killed",
    ] {
        let exact = format!("exec_linux_runner::{name}");
        let output = success(
            Command::new(&executable)
                .args(["--exact", &exact, "--test-threads=1"])
                .output()
                .unwrap(),
        );
        assert!(String::from_utf8_lossy(&output).contains(&format!("{name} ... ok")));
    }
}

#[test]
fn linux_confined_broker_handles_directory_network_and_exec() {
    let fixture = Fixture::new();
    let executable = fixture.0.join("broker-e2e-tests");
    success(
        Command::new(option_env!("RUSTC").unwrap_or("rustc"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(["--edition=2024", "--test", "-Adead_code", "-Copt-level=1"])
            .arg("tests/fixtures/native-sandbox/linux-broker-e2e.rs")
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap(),
    );
    for name in [
        "broker_cleanup_failure_is_reported",
        "broker_reads_only_the_granted_directory",
        "confined_broker_connects_to_the_granted_endpoint",
        "confined_broker_runs_only_the_granted_executable",
    ] {
        let output = success(
            Command::new(&executable)
                .args(["--exact", name, "--test-threads=1"])
                .output()
                .unwrap(),
        );
        assert!(String::from_utf8_lossy(&output).contains(&format!("{name} ... ok")));
    }
}

#[test]
fn linux_native_build_runs_with_the_checked_grants() {
    let fixture = Fixture::new();
    fs::write(
        fixture.0.join("input.wy"),
        include_str!("../fixtures/native-layout/packed.wy"),
    )
    .unwrap();
    success(
        Command::new(env!("CARGO_BIN_EXE_witchyc"))
            .current_dir(&fixture.0)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .args(["build", "input.wy", "-o", "program", "--target=linux-arm64"])
            .output()
            .unwrap(),
    );
    let run = |args: &[&str]| {
        Command::new(fixture.0.join("program"))
            .args(args)
            .output()
            .unwrap()
    };
    let refused = run(&["--output"]);
    assert_eq!(refused.status.code(), Some(2));
    assert!(refused.stdout.is_empty());
    assert_eq!(refused.stderr, b"Allocator requires --allocator\n");

    let result = run(&["--output", "--allocator"]);
    assert_eq!(
        success(result),
        include_bytes!("../fixtures/native-layout/packed.expected")
    );
}

/// Build a fixture that supplies the guest ABI and runs through native startup.
fn link_runtime_probe(source: &str, executable: &std::path::Path, flags: &[&str]) {
    let object = executable.with_extension("o");
    success(Command::new(option_env!("RUSTC").unwrap_or("rustc"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["--edition=2024", "--crate-type=lib", "--emit=obj", "-Copt-level=2", "-Cpanic=abort"])
        .args(flags)
        .arg(source)
        .arg("-o")
        .arg(&object)
        .output()
        .unwrap());
    let mut linker = Command::new("cc");
    linker.args(["-O2", "-Wl,-z,now", concat!(env!("OUT_DIR"), "/", env!("WITCHY_NATIVE_ARCHIVE_FILE"))])
        .arg(&object);
    #[cfg(witchy_native_aws_lc)]
    linker.arg(concat!(env!("WITCHY_NATIVE_AWS_LC_INSTALLED"), "/lib/libaws_lc_fips_0_14_4_crypto.a"))
        .arg("-lpthread");
    success(linker.arg("-o").arg(executable).output().unwrap());
}

#[test]
fn linux_native_startup_enforces_each_grant_profile() {
    let fixture = Fixture::new();
    for (grants, flags, expected) in [
        ("0", &[][..], &b""[..]),
        ("1", &["--cfg=console"][..], &b"output\n"[..]),
        ("2", &["--cfg=allocator"][..], &b""[..]),
        (
            "3",
            &["--cfg=console", "--cfg=allocator"][..],
            &b"output\n"[..],
        ),
    ] {
        let executable = fixture.0.join(grants);
        link_runtime_probe("tests/fixtures/native-sandbox/probe.rs", &executable, flags);
        assert_eq!(
            success(
                Command::new(executable)
                    .args(["--output", "--allocator"])
                    .output()
                    .unwrap()
            ),
            expected
        );
    }
}

#[test]
fn linux_native_refuses_an_unavailable_argument_grant() {
    let fixture = Fixture::new();
    let executable = fixture.0.join("program");
    link_runtime_probe("tests/fixtures/native-sandbox/probe.rs", &executable, &["--cfg=args"]);
    let result = Command::new(executable).output().unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(result.stdout.is_empty());
    assert_eq!(
        result.stderr,
        b"Args requires --args=COUNT:BYTES covering all --arg values\n"
    );
}

#[test]
fn linux_native_never_ignores_an_embedded_launch_plan() {
    let fixture = Fixture::new();
    let executable = fixture.0.join("program");
    link_runtime_probe("tests/fixtures/native-sandbox/probe.rs", &executable, &["--cfg=manifest"]);
    let result = Command::new(executable).output().unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr).contains("launch plan: malformed"));
}

#[test]
fn linux_native_uses_embedded_output_grant() {
    let fixture = Fixture::new();
    let executable = fixture.0.join("program");
    link_runtime_probe("tests/fixtures/native-sandbox/probe.rs", &executable, &["--cfg=valid_manifest", "--cfg=console"]);
    assert_eq!(
        success(Command::new(&executable).output().unwrap()),
        b"output\n"
    );
    let refused = Command::new(executable).arg("--output").output().unwrap();
    assert!(!refused.status.success());
    assert!(refused.stdout.is_empty());
}

#[test]
fn linux_native_uses_embedded_directory_grant() {
    let fixture = Fixture::new();
    let executable = fixture.0.join("program");
    std::fs::create_dir(fixture.0.join("data")).unwrap();
    std::fs::write(fixture.0.join("data/allowed"), b"safe").unwrap();
    link_runtime_probe("tests/fixtures/native-sandbox/linux-manifest-dir.rs", &executable, &[]);
    assert_eq!(
        success(
            Command::new(&executable)
                .current_dir(&fixture.0)
                .output()
                .unwrap()
        ),
        b"safe"
    );
    let refused = Command::new(&executable)
        .current_dir(&fixture.0)
        .arg("--output")
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(refused.stdout.is_empty());
}

#[test]
fn linux_native_serves_a_sandboxed_connection() {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::time::{Duration, Instant};
    let fixture = Fixture::new();
    let executable = fixture.0.join("server");
    link_runtime_probe("tests/fixtures/native-sandbox/linux-server-guest.rs", &executable, &[]);
    let reserved = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = reserved.local_addr().unwrap();
    drop(reserved);
    let mut server = Command::new(executable)
        .args([
            format!("--listen={endpoint}"),
            "--instance-per-connection".into(),
            "--connections=1".into(),
            "--deadline-ms=3000".into(),
        ])
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut client = loop {
        match TcpStream::connect(endpoint) {
            Ok(client) => break client,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                let _ = server.kill();
                let _ = server.wait();
                panic!("{error}");
            }
        }
    };
    client
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    client.write_all(b"x").unwrap();
    let mut reply = [0];
    client.read_exact(&mut reply).unwrap();
    assert_eq!(&reply, b"x");
    assert!(server.wait().unwrap().success());
}

#[cfg(witchy_native_aws_lc)]
#[test]
fn linux_native_signs_from_an_embedded_descriptor_grant() {
    let fixture = Fixture::new();
    let executable = fixture.0.join("signer");
    let seed = fixture.0.join("seed");
    fs::write(&seed, [0x46, 0x47, 0x56, 0xa9, 0x85, 0xe5, 0xdf, 0x03, 0x73, 0x9d, 0x95, 0xdd, 0x30, 0x9c, 0x1e, 0xd9, 0xc5, 0xb0, 0x42, 0x54, 0xcc, 0x29, 0x4e, 0x7e, 0x7e, 0xb9, 0xb9, 0x36, 0x5e, 0xe1, 0x51, 0x17]).unwrap();
    link_runtime_probe("tests/fixtures/native-sandbox/linux-signer-guest.rs", &executable, &[]);
    let run = || Command::new("sh").args(["-c", "exec \"$1\" 3<\"$2\"", "signer"])
        .arg(&executable).arg(&seed).output().unwrap();
    assert_eq!(success(run()), b"signed\n");
    fs::write(&seed, [1u8; 31]).unwrap();
    let refused = run();
    assert!(!refused.status.success());
    assert!(refused.stdout.is_empty());
}
