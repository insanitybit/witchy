use super::*;

fn fake_gate_tools(temp: &TempDir) -> (PathBuf, PathBuf) {
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).expect("create fake tool directory");
    let argv = temp.path().join("cargo-argv");
    fs::write(
        bin.join("cargo"),
        "#!/bin/sh\n\
         if [ -n \"${FAKE_CARGO_ARGV_FILE:-}\" ]; then\n\
           printf 'BEGIN\\n' >>\"$FAKE_CARGO_ARGV_FILE\"\n\
           for arg in \"$@\"; do printf '%s\\n' \"$arg\" >>\"$FAKE_CARGO_ARGV_FILE\"; done\n\
           printf 'END\\n' >>\"$FAKE_CARGO_ARGV_FILE\"\n\
         fi\n\
         if [ \"$1\" = nextest ] && [ \"${2:-}\" = run ]; then\n\
           count=0\n\
           [ ! -f \"${FAKE_RUN_COUNT_FILE:-}\" ] || count=$(cat \"$FAKE_RUN_COUNT_FILE\")\n\
           count=$((count + 1)); printf '%s\\n' \"$count\" >\"$FAKE_RUN_COUNT_FILE\"\n\
           if [ \"${FAKE_FAIL_FIRST_RUN:-0}\" = 1 ] && [ \"$count\" = 1 ]; then exit 7; fi\n\
         fi\n\
         exit 0\n",
    )
    .expect("write fake cargo");
    fs::set_permissions(bin.join("cargo"), fs::Permissions::from_mode(0o755))
        .expect("chmod fake cargo");
    for (name, body) in [
        (
            "git",
            "#!/bin/sh\nif [ \"$1\" = rev-parse ]; then echo 0123456789012345678901234567890123456789; fi\nexit 0\n",
        ),
        (
            "rustup",
            "#!/bin/sh\nif [ \"$1\" = which ]; then echo /usr/bin/true; fi\nexit 0\n",
        ),
    ] {
        fs::write(bin.join(name), body).expect("write fake tool");
        fs::set_permissions(bin.join(name), fs::Permissions::from_mode(0o755))
            .expect("chmod fake tool");
    }
    (bin, argv)
}

fn cargo_invocations(path: &Path) -> Vec<Vec<String>> {
    fs::read_to_string(path)
        .expect("read fake cargo argv")
        .split("BEGIN\n")
        .skip(1)
        .filter_map(|block| block.strip_suffix("END\n"))
        .map(|block| block.lines().map(str::to_owned).collect())
        .collect()
}

fn nextest_runs(invocations: &[Vec<String>]) -> Vec<&[String]> {
    invocations
        .iter()
        .filter(|args| {
            args.first().map(String::as_str) == Some("nextest")
                && args.get(1).map(String::as_str) == Some("run")
        })
        .map(Vec::as_slice)
        .collect()
}

fn option_value<'a>(args: &'a [String], option: &str) -> &'a str {
    let index = args
        .iter()
        .position(|arg| arg == option)
        .expect("option present");
    args.get(index + 1).expect("option has value")
}

#[test]
fn workspace_preflight_and_main_partition_one_identical_context() {
    let root = workspace_root();
    let temp = TempDir::new();
    let (bin, argv) = fake_gate_tools(&temp);
    let count = temp.path().join("run-count");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let replay = "test(/^(?:.*::)?owned_probe$/) or test(/^(?:.*::)?bounds_probe$/)";
    let output = check_sh(&root, &["--fast"])
        .env("PATH", path)
        .env("CARGO_TARGET_DIR", temp.path().join("target"))
        .env("WITCHY_GATE_SCOPE", "all")
        .env("WITCHY_GATE_PREFLIGHT_EXPR", replay)
        .env("FAKE_CARGO_ARGV_FILE", &argv)
        .env("FAKE_RUN_COUNT_FILE", &count)
        .env("WITCHY_STAGE_HEARTBEAT_INTERVAL", "0")
        .output()
        .expect("run fake workspace gate");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let invocations = cargo_invocations(&argv);
    let runs = nextest_runs(&invocations);
    assert_eq!(runs.len(), 2, "{runs:?}");
    for args in &runs {
        assert_eq!(
            args.iter().filter(|arg| *arg == "-E").count(),
            1,
            "{args:?}"
        );
        assert_eq!(option_value(args, "-j"), "8");
        assert!(args.iter().any(|arg| arg == "--workspace"), "{args:?}");
        assert_eq!(option_value(args, "--features"), "witchy-wir/native");
    }
    let preflight = option_value(runs[0], "-E");
    let main = option_value(runs[1], "-E");
    assert!(
        preflight.contains(&format!("and ({replay})")),
        "{preflight}"
    );
    assert!(main.contains(&format!("and not ({replay})")), "{main}");
    assert!(preflight.contains("not binary(merge_queue)"), "{preflight}");
    assert!(main.contains("not binary(merge_queue)"), "{main}");
}

#[test]
fn scoped_preflight_keeps_the_complete_main_selection() {
    let root = workspace_root();
    let temp = TempDir::new();
    let (bin, argv) = fake_gate_tools(&temp);
    let count = temp.path().join("run-count");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let replay = "test(/^(?:.*::)?only_test$/)";
    let output = check_sh(&root, &["--fast"])
        .env("PATH", path)
        .env("CARGO_TARGET_DIR", temp.path().join("target"))
        .env("WITCHY_GATE_SCOPE", "all")
        .env("WITCHY_GATE_NEXTEST", "-p witchy-lower -p witchy")
        .env(
            "WITCHY_GATE_NEXTEST_EXPR",
            "package(witchy-lower) or test(only_test)",
        )
        .env("WITCHY_GATE_PREFLIGHT_EXPR", replay)
        .env("FAKE_CARGO_ARGV_FILE", &argv)
        .env("FAKE_RUN_COUNT_FILE", &count)
        .env("WITCHY_STAGE_HEARTBEAT_INTERVAL", "0")
        .output()
        .expect("run fake scoped gate");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let invocations = cargo_invocations(&argv);
    let runs = nextest_runs(&invocations);
    assert_eq!(runs.len(), 2, "{runs:?}");
    let preflight = option_value(runs[0], "-E");
    let main = option_value(runs[1], "-E");
    assert!(
        preflight.contains(&format!("and ({replay})")),
        "{preflight}"
    );
    assert!(
        !main.contains(replay),
        "scoped main must not subtract replay: {main}"
    );
    for args in &runs {
        assert_eq!(option_value(args, "--features"), "witchy-wir/native");
        assert_eq!(
            args.iter().filter(|arg| *arg == "-p").count(),
            2,
            "{args:?}"
        );
    }
}

#[test]
fn red_preflight_stops_before_the_main_run() {
    let root = workspace_root();
    let temp = TempDir::new();
    let (bin, argv) = fake_gate_tools(&temp);
    let count = temp.path().join("run-count");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = check_sh(&root, &["--fast"])
        .env("PATH", path)
        .env("CARGO_TARGET_DIR", temp.path().join("target"))
        .env("WITCHY_GATE_SCOPE", "all")
        .env("WITCHY_GATE_PREFLIGHT_EXPR", "test(/^(?:.*::)?red_probe$/)")
        .env("FAKE_CARGO_ARGV_FILE", &argv)
        .env("FAKE_RUN_COUNT_FILE", &count)
        .env("FAKE_FAIL_FIRST_RUN", "1")
        .env("WITCHY_STAGE_HEARTBEAT_INTERVAL", "0")
        .output()
        .expect("run failing fake preflight");
    assert_eq!(
        output.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let invocations = cargo_invocations(&argv);
    assert_eq!(nextest_runs(&invocations).len(), 1, "{invocations:?}");
}
