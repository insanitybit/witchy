use super::*;

#[test]
fn zero_whole_gate_timeout_does_not_kill_a_progressing_gate() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");

    let output = fixture
        .mq_command(&["run", "--once"], "printf 'gate started\\n'; sleep 0.2")
        .env("MERGE_QUEUE_GATE_TIMEOUT", "0")
        .output()
        .expect("run gate with the whole-gate ceiling disabled");

    assert!(
        output.status.success(),
        "disabled whole-gate ceiling rejected a progressing gate: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(fixture.root.join("a.txt").exists(), "green candidate did not land");
    assert!(
        fixture
            .journal()
            .iter()
            .any(|event| event["event"] == "merged" && event["branch"] == "a"),
        "green candidate was not journaled as merged",
    );
    assert!(
        fixture
            .journal()
            .iter()
            .all(|event| event["event"] != "timeout"),
        "disabled whole-gate ceiling still produced a timeout",
    );

    let invalid = fixture
        .mq_command(&["status"], "true")
        .env("MERGE_QUEUE_GATE_TIMEOUT", "forever")
        .output()
        .expect("reject invalid whole-gate ceiling");
    assert!(!invalid.status.success());
    assert!(
        String::from_utf8_lossy(&invalid.stderr)
            .contains("MERGE_QUEUE_GATE_TIMEOUT must be a non-negative integer")
    );
}

#[test]
fn workspace_test_budget_kills_chatty_execution_without_invalidating_its_build() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");

    // The fake gate plants one workspace and one dependency fingerprint in
    // its cargo target, and reports where, so the kill's cleanup can be seen.
    let probe = fixture.state.join("fingerprint-probe");
    let gate = "\
fp=\"$PWD/$CARGO_TARGET_DIR/debug/.fingerprint\"
mkdir -p \"$fp/witchy-wir-0123456789abcdef\" \"$fp/serde-0123456789abcdef\"
printf '%s' \"$fp\" > \"$FINGERPRINT_PROBE\"
printf '==> [1] tests (workspace) (t+0s)\\n'
printf 'Nextest run ID 01234567-89ab-cdef-0123-456789abcdef with nextest profile: default\\n'
i=0
while :; do
  i=$((i+1))
  printf '        PASS [  0.010s] (%d/9999) witchy-types types::keeps_printing\\n' \"$i\"
  printf '[1] tests (workspace) still running (heartbeat %d)\\n' \"$i\"
  sleep 0.2
done
";

    let output = fixture
        .mq_command(&["run", "--once"], gate)
        .env("MERGE_QUEUE_WORKSPACE_TEST_BUDGET", "1")
        .env("MERGE_QUEUE_STALL_TIMEOUT", "600")
        .env("MERGE_QUEUE_BUSY_SILENCE_MAX", "1800")
        .env("MERGE_QUEUE_TEST_TERMINATE_GRACE", "0.05")
        .env("FINGERPRINT_PROBE", &probe)
        .output()
        .expect("run gate past the workspace-test budget");
    assert!(
        fixture
            .journal()
            .iter()
            .any(|event| event["event"] == "timeout" && event["branch"] == "a"),
        "chatty over-budget stage was not journaled as timeout: {:?}",
        fixture.journal(),
    );
    let reason = fixture
        .journal()
        .iter()
        .find(|event| event["event"] == "timeout")
        .and_then(|event| event["reason"].as_str())
        .unwrap_or("")
        .to_owned();
    assert!(
        reason.contains("MERGE_QUEUE_WORKSPACE_TEST_BUDGET"),
        "timeout reason did not name the workspace-test budget: {reason}",
    );
    let gate_elapsed = fixture
        .journal()
        .iter()
        .find(|event| event["event"] == "timeout")
        .and_then(|event| event["gate_elapsed_s"].as_str())
        .and_then(|elapsed| elapsed.parse::<u64>().ok())
        .expect("timeout event recorded gate elapsed time");
    assert!(
        gate_elapsed < 20,
        "workspace-test budget did not fire promptly inside the gate: {gate_elapsed}s",
    );
    assert!(
        fixture
            .journal()
            .iter()
            .all(|event| event["event"] != "merged"),
        "over-budget stage was still merged",
    );

    assert!(
        String::from_utf8_lossy(&output.stderr).contains("preserved workspace fingerprints"),
        "coordinator did not report preserving the completed build: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    // Cargo completed before nextest announced the run, so preserve both
    // workspace and dependency fingerprints for the retry.
    let fingerprints = std::path::PathBuf::from(
        std::fs::read_to_string(&probe).expect("fake gate reported its fingerprint dir"),
    );
    assert!(
        fingerprints.join("witchy-wir-0123456789abcdef").exists(),
        "completed workspace build was invalidated after nextest started",
    );
    assert!(
        fingerprints.join("serde-0123456789abcdef").exists(),
        "the kill invalidated a dependency's fingerprint",
    );

    let invalid = fixture
        .mq_command(&["status"], "true")
        .env("MERGE_QUEUE_WORKSPACE_TEST_BUDGET", "forever")
        .output()
        .expect("reject invalid workspace-test budget");
    assert!(!invalid.status.success());
    assert!(
        String::from_utf8_lossy(&invalid.stderr)
            .contains("MERGE_QUEUE_WORKSPACE_TEST_BUDGET must be a non-negative integer")
    );
}

#[test]
fn workspace_test_timeout_after_nextest_starts_preserves_cargo_fingerprints() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");

    let probe = fixture.state.join("fingerprint-probe");
    let gate = "\
fp=\"$PWD/$CARGO_TARGET_DIR/debug/.fingerprint\"
mkdir -p \"$fp/witchy-wir-0123456789abcdef\" \"$fp/serde-0123456789abcdef\"
printf '%s' \"$fp\" > \"$FINGERPRINT_PROBE\"
printf '==> [1] tests (workspace) (t+0s)\\n'
sleep 3
printf '    Finished `test` profile [unoptimized + debuginfo] target(s) in 1.00s\\n'
printf 'Nextest run ID 01234567-89ab-cdef-0123-456789abcdef with nextest profile: default\\n'
printf reached > \"$NEXTEST_REACHED\"
i=0
while :; do
  i=$((i+1))
  printf '        PASS [  0.010s] (%d/9999) witchy-types types::keeps_printing\\n' \"$i\"
  sleep 0.2
done
";

    let output = fixture
        .mq_command(&["run", "--once"], gate)
        .env("MERGE_QUEUE_WORKSPACE_TEST_BUDGET", "1")
        .env("MERGE_QUEUE_MONITOR_INTERVAL", "0.1")
        .env("MERGE_QUEUE_STALL_TIMEOUT", "600")
        .env("MERGE_QUEUE_TEST_TERMINATE_GRACE", "0.05")
        .env("FINGERPRINT_PROBE", &probe)
        .env("NEXTEST_REACHED", fixture.state.join("nextest-reached"))
        .output()
        .expect("run nextest execution past the workspace-test budget");

    assert!(
        fixture
            .journal()
            .iter()
            .any(|event| event["event"] == "timeout" && event["branch"] == "a"),
        "over-budget nextest run was not journaled as timeout: {:?}",
        fixture.journal(),
    );
    assert!(
        fixture.state.join("nextest-reached").exists(),
        "compilation time consumed the test-execution budget before nextest started",
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("preserved workspace fingerprints"),
        "coordinator did not report preserving the completed build: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    let fingerprints = std::path::PathBuf::from(
        std::fs::read_to_string(&probe).expect("fake gate reported its fingerprint dir"),
    );
    assert!(
        fingerprints.join("witchy-wir-0123456789abcdef").exists(),
        "completed workspace build was invalidated after nextest started",
    );
    assert!(
        fingerprints.join("serde-0123456789abcdef").exists(),
        "dependency fingerprint was invalidated after nextest started",
    );
}

#[test]
fn whole_gate_timeout_during_compilation_invalidates_workspace_fingerprints() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");

    let probe = fixture.state.join("fingerprint-probe");
    let gate = "\
fp=\"$PWD/$CARGO_TARGET_DIR/debug/.fingerprint\"
mkdir -p \"$fp/witchy-wir-0123456789abcdef\" \"$fp/serde-0123456789abcdef\"
printf '%s' \"$fp\" > \"$FINGERPRINT_PROBE\"
printf '==> [1] tests (workspace) (t+0s)\\n'
while :; do printf '   Compiling witchy-wir v0.1.0\\n'; sleep 0.2; done
";

    let output = fixture
        .mq_command(&["run", "--once"], gate)
        .env("MERGE_QUEUE_GATE_TIMEOUT", "1")
        .env("MERGE_QUEUE_WORKSPACE_TEST_BUDGET", "1")
        .env("MERGE_QUEUE_MONITOR_INTERVAL", "0.1")
        .env("MERGE_QUEUE_TEST_TERMINATE_GRACE", "0.05")
        .env("FINGERPRINT_PROBE", &probe)
        .output()
        .expect("time out a foreground Cargo build");

    assert!(
        fixture
            .journal()
            .iter()
            .any(|event| event["event"] == "timeout" && event["branch"] == "a"),
        "compile timeout was not journaled: {:?}\nstderr: {}",
        fixture.journal(),
        String::from_utf8_lossy(&output.stderr),
    );
    let fingerprints = std::path::PathBuf::from(
        std::fs::read_to_string(&probe).expect("fake gate reported its fingerprint dir"),
    );
    assert!(
        !fingerprints.join("witchy-wir-0123456789abcdef").exists(),
        "a workspace fingerprint survived a killed compiler",
    );
    assert!(
        fingerprints.join("serde-0123456789abcdef").exists(),
        "compile-time cleanup invalidated a dependency fingerprint",
    );
}

#[test]
fn zero_workspace_test_budget_does_not_kill_a_progressing_test_stage() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");

    let output = fixture
        .mq_command(
            &["run", "--once"],
            "printf '==> [1] tests (workspace) (t+0s)\\n'; sleep 0.2; printf '    [1] tests (workspace) took 0s\\n'",
        )
        .env("MERGE_QUEUE_WORKSPACE_TEST_BUDGET", "0")
        .output()
        .expect("run gate with the workspace-test budget disabled");

    assert!(
        output.status.success(),
        "disabled workspace-test budget rejected a progressing stage: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        fixture
            .journal()
            .iter()
            .any(|event| event["event"] == "merged" && event["branch"] == "a"),
        "green candidate was not journaled as merged",
    );
    assert!(
        fixture
            .journal()
            .iter()
            .all(|event| event["event"] != "timeout"),
        "disabled workspace-test budget still produced a timeout",
    );
}

#[test]
fn docs_safe_landing_does_not_wait_on_the_full_gate_lock() {
    let fixture = QueueFixture::stack(&["rfcs/note.md"]);
    fixture.mq_ok(&["submit", "a"], "true");

    let mut holder = Command::new("sh")
        .args(["-c", "sleep 30"])
        .spawn()
        .expect("start lock-holder fixture");
    let holder_pid = holder.id();
    let gate_lock = fixture.state.join("gate.lock");
    fs::create_dir(&gate_lock).expect("create full-gate lock");
    fs::write(gate_lock.join("pid"), format!("{holder_pid}\n")).expect("write live lock pid");
    fs::write(gate_lock.join("what"), "full gate: other\n").expect("write lock description");

    let wait_marker = fixture._temp.path().join("lock-waited");
    let env_log = fixture._temp.path().join("gate-env");
    let gate = format!(
        "printf 'scope=%s skip_book=%s nextest=%s\\n' \
         \"${{WITCHY_GATE_SCOPE:-}}\" \"${{WITCHY_GATE_SKIP_BOOK:-}}\" \"${{WITCHY_GATE_NEXTEST:-}}\" >{}",
        env_log.display(),
    );
    let output = fixture
        .mq_command(&["run", "--once"], &gate)
        .env("MERGE_QUEUE_TEST_LOCK_WAIT_MARKER", &wait_marker)
        .output()
        .expect("run docs-safe landing against a held full-gate lock");

    assert!(
        output.status.success(),
        "docs-safe landing failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        fixture.root.join("rfcs/note.md").exists(),
        "docs-safe candidate did not land",
    );
    assert!(
        !wait_marker.exists(),
        "docs-safe landing waited on gate.lock held by a full code gate",
    );
    assert!(
        gate_lock.exists(),
        "docs-safe landing stole or released the full-gate lock",
    );
    assert!(
        fixture
            .journal()
            .iter()
            .any(|event| event["event"] == "merged" && event["branch"] == "a"),
        "docs-safe landing was not journaled as merged",
    );
    let env = fs::read_to_string(&env_log).unwrap_or_default();
    assert!(
        env.contains("scope=docs"),
        "docs-safe batch was not classified WITCHY_GATE_SCOPE=docs: {env}",
    );
    let _ = holder.kill();
    let _ = holder.wait();
}

#[test]
fn docs_dir_landing_is_docs_safe() {
    let fixture = QueueFixture::stack(&["docs/playbook.md"]);
    fixture.mq_ok(&["submit", "a"], "true");
    let env_log = fixture._temp.path().join("gate-env");
    let gate = format!(
        "printf 'scope=%s\\n' \"${{WITCHY_GATE_SCOPE:-}}\" >{}",
        env_log.display(),
    );
    let output = fixture.mq_ok(&["run", "--once"], &gate);
    let env = fs::read_to_string(&env_log).expect("read classified gate env");
    assert!(
        env.contains("scope=docs"),
        "docs/ batch was not classified WITCHY_GATE_SCOPE=docs: {env}",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("docs-safe"),
        "docs/ landing did not skip the full-gate lock: {stderr}",
    );
}

/// Agent-instruction files steer how agents work; no test, build, or lint
/// reads them. Before GATE_NO_TEST_PATHS covered them, an edit to a skill
/// under `.claude/` ran the whole 4880-test product suite -- 235s that could
/// not observe the diff -- and took the full-gate lock while doing it.
#[test]
fn agent_instruction_landing_is_docs_safe() {
    for path in [".claude/skills/land/SKILL.md", "CLAUDE.md", ".codex/notes.md"] {
        let fixture = QueueFixture::stack(&[path]);
        fixture.mq_ok(&["submit", "a"], "true");
        let env_log = fixture._temp.path().join("gate-env");
        let gate = format!(
            "printf 'scope=%s\\n' \"${{WITCHY_GATE_SCOPE:-}}\" >{}",
            env_log.display(),
        );
        let output = fixture.mq_ok(&["run", "--once"], &gate);
        let env = fs::read_to_string(&env_log).expect("read classified gate env");
        assert!(
            env.contains("scope=docs"),
            "{path} batch was not classified WITCHY_GATE_SCOPE=docs: {env}",
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("docs-safe"),
            "{path} landing did not skip the full-gate lock: {stderr}",
        );
    }
}

/// The classifier fails closed: one product path in the batch retains the
/// complete gate however much agent prose rides along with it.
#[test]
fn agent_instruction_paths_do_not_excuse_a_product_change() {
    // `stack` gives one branch per file; branch "b" carries both commits, so
    // its diff against master is agent prose plus a compiler source edit.
    let fixture = QueueFixture::stack(&["CLAUDE.md", "crates/witchy-lower/src/analysis.rs"]);
    fixture.mq_ok(&["submit", "b"], "true");
    let env_log = fixture._temp.path().join("gate-env");
    let gate = format!(
        "printf 'scope=%s\\n' \"${{WITCHY_GATE_SCOPE:-}}\" >{}",
        env_log.display(),
    );
    fixture.mq_ok(&["run", "--once"], &gate);
    let env = fs::read_to_string(&env_log).expect("read classified gate env");
    assert!(
        env.contains("scope=all"),
        "a batch carrying a compiler source edit was not fully gated: {env}",
    );
}

#[test]
fn docs_only_master_move_does_not_regate_a_green_code_candidate() {
    let fixture = QueueFixture::stack(&["code.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");

    let started = fixture._temp.path().join("gate-started");
    let release = fixture._temp.path().join("gate-release");
    let env_log = fixture._temp.path().join("gate-env");
    let gate = format!(
        "printf 'scope=%s skip_book=%s nextest=[%s]\\n' \
         \"${{WITCHY_GATE_SCOPE:-}}\" \"${{WITCHY_GATE_SKIP_BOOK:-}}\" \"${{WITCHY_GATE_NEXTEST:-}}\" >{env}\n\
         printf started >{started}\n\
         while [ ! -e {release} ]; do sleep 0.01; done\n\
         exit 0\n",
        env = env_log.display(),
        started = started.display(),
        release = release.display(),
    );

    let mut coordinator = fixture
        .mq_command(&["run", "--once"], &gate)
        .spawn()
        .expect("start coordinator for docs-only master-move fixture");

    let deadline = Instant::now() + Duration::from_secs(15);
    while !started.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(started.exists(), "code gate never started");

    fs::create_dir_all(fixture.root.join("rfcs")).expect("create rfcs on master");
    fs::write(fixture.root.join("rfcs/other.md"), "docs\n").expect("write docs-only master commit");
    run_git(&fixture.root, &["add", "rfcs/other.md"]);
    run_git(&fixture.root, &["commit", "-m", "docs only"]);
    fs::write(&release, "release\n").expect("release code gate after master moves");

    let status = coordinator.wait().expect("reap coordinator");
    assert!(
        status.success(),
        "coordinator failed after a docs-only master move: {}",
        fs::read_to_string(fixture.state.join("journal.jsonl")).unwrap_or_default(),
    );
    assert!(
        fixture.root.join("code.txt").exists(),
        "green code candidate did not land after a docs-only master move",
    );
    assert!(
        fixture.root.join("rfcs/other.md").exists(),
        "docs-only master commit was lost",
    );
    assert!(
        fixture
            .journal()
            .iter()
            .any(|event| event["event"] == "merged" && event["branch"] == "a"),
        "green code candidate was not journaled as merged",
    );
    assert!(
        fixture.journal().iter().all(|event| {
            event["event"] != "requeued"
                || event["reason"].as_str() != Some("master moved")
        }),
        "docs-only master move forced a full re-gate: {:?}",
        fixture.journal(),
    );
    let env = fs::read_to_string(&env_log).unwrap_or_default();
    assert!(
        env.contains("skip_book=1"),
        "code-only batch still invoked the book validator path: {env}",
    );
}

#[test]
fn types_only_batch_classifies_a_focused_nextest_selection() {
    let fixture = QueueFixture::stack(&["crates/witchy-types/src/lib.rs"]);
    fixture.mq_ok(&["submit", "a"], "true");
    let env_log = fixture._temp.path().join("gate-env");
    let gate = format!(
        "printf 'scope=%s skip_book=%s skip_rust=%s skip_wasm=%s skip_compile=%s skip_fmt=%s nextest=[%s] expr=[%s]\\n' \
         \"${{WITCHY_GATE_SCOPE:-}}\" \"${{WITCHY_GATE_SKIP_BOOK:-}}\" \
         \"${{WITCHY_GATE_SKIP_RUST_CLASS:-}}\" \"${{WITCHY_GATE_SKIP_WASM:-}}\" \
         \"${{WITCHY_GATE_SKIP_COMPILE:-}}\" \"${{WITCHY_GATE_SKIP_FMT:-}}\" \
         \"${{WITCHY_GATE_NEXTEST:-}}\" \"${{WITCHY_GATE_NEXTEST_EXPR:-}}\" >{}",
        env_log.display(),
    );
    let output = fixture.mq_ok(&["run", "--once"], &gate);
    let env = fs::read_to_string(&env_log).expect("read classified gate env");
    assert!(
        env.contains("scope=all"),
        "types-only batch was not a product gate: {env}",
    );
    assert!(
        env.contains("skip_book=1") && env.contains("skip_rust=1") && env.contains("skip_wasm=1")
            && env.contains("skip_fmt=1"),
        "types-only batch did not skip book/rust-class/wasm/fmt: {env}",
    );
    assert!(
        env.contains("skip_compile=0"),
        "types-only batch skipped compile legs it can change: {env}",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("skipped generator build (batch cannot stale census/stdlib snapshots)"),
        "types-only prepare still ran the generator cargo build: {stderr}",
    );
    assert!(
        env.contains("nextest=[-p witchy-types") && env.contains("expr=[package(witchy-types)"),
        "types-only batch did not select the crate package: {env}",
    );
    assert!(
        // Front-end edits stop at the checker boundary; the whole example
        // matrix must not ride along and pull in native conformance.
        !env.contains("test(/^example_tests::/)"),
        "types-only batch still auto-attached the example_tests matrix: {env}",
    );
    assert!(
        !env.contains("nextest=[--workspace") && !env.contains("nextest=[]"),
        "types-only batch launched an unfiltered --workspace nextest: {env}",
    );
}

#[test]
fn queue_script_batch_is_queue_infra_only_not_workspace_nextest() {
    let fixture = QueueFixture::stack(&["scripts/check.sh"]);
    fixture.mq_ok(&["submit", "a"], "true");
    let env_log = fixture._temp.path().join("gate-env");
    let gate = format!(
        "printf 'infra=%s infra_only=%s nextest=[%s] skip_compile=%s\\n' \
         \"${{WITCHY_GATE_QUEUE_INFRA:-}}\" \"${{WITCHY_GATE_QUEUE_INFRA_ONLY:-}}\" \
         \"${{WITCHY_GATE_NEXTEST:-}}\" \"${{WITCHY_GATE_SKIP_COMPILE:-}}\" >{}",
        env_log.display(),
    );
    let output = fixture.mq_ok(&["run", "--once"], &gate);
    let env = fs::read_to_string(&env_log).expect("read classified gate env");
    assert!(
        env.contains("infra=1") && env.contains("infra_only=1"),
        "check.sh-only batch was not queue-infra-only: {env}",
    );
    assert!(
        !env.contains("nextest=[--workspace"),
        "check.sh-only batch still selected unfiltered --workspace nextest: {env}",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("queue-infra-only=1"),
        "gating note did not mark queue-infra-only: {stderr}",
    );
}

#[test]
fn mbx_policy_batch_is_queue_infra_only_not_workspace_nextest() {
    let fixture = QueueFixture::stack(&[".mbx.toml"]);
    fixture.mq_ok(&["submit", "a"], "true");
    let env_log = fixture._temp.path().join("gate-env");
    let gate = format!(
        "printf 'infra=%s infra_only=%s nextest=[%s] skip_compile=%s\\n' \
         \"${{WITCHY_GATE_QUEUE_INFRA:-}}\" \"${{WITCHY_GATE_QUEUE_INFRA_ONLY:-}}\" \
         \"${{WITCHY_GATE_NEXTEST:-}}\" \"${{WITCHY_GATE_SKIP_COMPILE:-}}\" >{}",
        env_log.display(),
    );
    fixture.mq_ok(&["run", "--once"], &gate);
    let env = fs::read_to_string(&env_log).expect("read classified gate env");
    assert!(
        env.contains("infra=1") && env.contains("infra_only=1"),
        "MBX policy batch was not queue-infra-only: {env}",
    );
    assert!(
        env.contains("nextest=[--test worktree") && !env.contains("nextest=[--workspace"),
        "MBX policy batch selected product tests: {env}",
    );
}

#[test]
fn nextest_config_batch_does_not_append_queue_fixtures() {
    let fixture = QueueFixture::stack(&[".config/nextest.toml"]);
    fixture.mq_ok(&["submit", "a"], "true");
    let env_log = fixture._temp.path().join("gate-env");
    let gate = format!(
        "printf 'infra=%s infra_only=%s scope=%s nextest=[%s]\n' \
         \"${{WITCHY_GATE_QUEUE_INFRA:-}}\" \"${{WITCHY_GATE_QUEUE_INFRA_ONLY:-}}\" \
         \"${{WITCHY_GATE_SCOPE:-}}\" \"${{WITCHY_GATE_NEXTEST:-}}\" >{}",
        env_log.display(),
    );
    let output = fixture.mq_ok(&["run", "--once"], &gate);
    let env = fs::read_to_string(&env_log).expect("read classified gate env");
    assert!(
        env.contains("infra=0") && env.contains("infra_only=0") && env.contains("scope=all"),
        "nextest config did not stay on the product-only gate: {env}",
    );
    assert!(
        env.contains("nextest=[]"),
        "nextest config must exercise the workspace selection: {env}",
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("queue-infra=0"),
        "gating note still requested queue fixtures: {}",
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn opportunistic_batches_do_not_mix_queue_infra_and_product_scopes() {
    let fixture = QueueFixture::stack(&["scripts/check.sh"]);
    run_git(&fixture.root, &["switch", "-c", "product", "master"]);
    let product = fixture.root.join("crates/witchy-types/src/scope.rs");
    fs::create_dir_all(product.parent().expect("product path has a parent"))
        .expect("create product path");
    fs::write(&product, "product\n").expect("write product change");
    run_git(&fixture.root, &["add", "crates/witchy-types/src/scope.rs"]);
    run_git(&fixture.root, &["commit", "-m", "product change"]);
    run_git(&fixture.root, &["switch", "master"]);

    fixture.mq_ok(&["submit", "--front", "a"], "true");
    fixture.mq_ok(&["submit", "product"], "true");
    fixture.mq_ok(
        &["run", "--once"],
        "test \"$WITCHY_GATE_QUEUE_INFRA_ONLY\" = 1",
    );

    assert!(run_git(&fixture.root, &["show", "master:scripts/check.sh"])
        .status
        .success());
    assert!(
        !git_output(&fixture.root, &["show", "master:crates/witchy-types/src/scope.rs"])
            .status
            .success(),
        "product change rode along with the queue-infrastructure gate",
    );
    let merged: Vec<_> = fixture
        .journal()
        .into_iter()
        .filter(|event| event["event"] == "merged")
        .collect();
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0]["branch"], "a");
    assert_eq!(merged[0]["batch"], "1");
    assert_eq!(
        fixture.change("product")["state"],
        "red",
        "the product change should be gated separately and fail the queue-only gate assertion",
    );
}

#[test]
fn example_tests_only_batch_classifies_focused_nextest_and_skips_heavy_legs() {
    let fixture = QueueFixture::stack(&["src/example_tests/equality.rs"]);
    fixture.mq_ok(&["submit", "a"], "true");
    let env_log = fixture._temp.path().join("gate-env");
    let gate = format!(
        "printf 'scope=%s skip_book=%s skip_rust=%s skip_compile=%s skip_wasm=%s skip_fmt=%s nextest=[%s] expr=[%s]\\n' \
         \"${{WITCHY_GATE_SCOPE:-}}\" \"${{WITCHY_GATE_SKIP_BOOK:-}}\" \
         \"${{WITCHY_GATE_SKIP_RUST_CLASS:-}}\" \"${{WITCHY_GATE_SKIP_COMPILE:-}}\" \
         \"${{WITCHY_GATE_SKIP_WASM:-}}\" \"${{WITCHY_GATE_SKIP_FMT:-}}\" \
         \"${{WITCHY_GATE_NEXTEST:-}}\" \"${{WITCHY_GATE_NEXTEST_EXPR:-}}\" >{}",
        env_log.display(),
    );
    let output = fixture.mq_ok(&["run", "--once"], &gate);
    let env = fs::read_to_string(&env_log).expect("read classified gate env");
    assert!(
        env.contains("scope=all"),
        "example_tests-only batch was not a product gate: {env}",
    );
    assert!(
        env.contains("skip_book=1")
            && env.contains("skip_rust=1")
            && env.contains("skip_compile=1")
            && env.contains("skip_wasm=1")
            && env.contains("skip_fmt=1"),
        "example_tests-only batch did not skip book/rust-class/compile/wasm/fmt: {env}",
    );
    assert!(
        env.contains("nextest=[-p witchy")
            && env.contains("test(/^example_tests::equality::/)")
            && !env.contains("expr=[test(/^example_tests::/)"),
        "example_tests-only batch did not partition to the touched module: {env}",
    );
    assert!(
        !env.contains("nextest=[--workspace") && !env.contains("nextest=[]"),
        "example_tests-only batch launched an unfiltered --workspace nextest: {env}",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("skipped generator build (batch cannot stale census/stdlib snapshots)"),
        "example_tests-only prepare still ran the generator cargo build: {stderr}",
    );
    assert!(
        stderr.contains("skip-sweeps=1"),
        "example_tests-only batch still enabled corpus sweeps: {stderr}",
    );
}

#[test]
fn crate_plus_example_tests_batch_keeps_compile_legs_and_unions_nextest() {
    let fixture = QueueFixture::stack(&["crates/witchy-interp/src/lib.rs"]);
    run_git(&fixture.root, &["switch", "a"]);
    fs::create_dir_all(fixture.root.join("src/example_tests")).expect("create example_tests dir");
    fs::write(
        fixture.root.join("src/example_tests/equality.rs"),
        "examples\n",
    )
    .expect("write example_tests file");
    run_git(&fixture.root, &["add", "src/example_tests/equality.rs"]);
    run_git(&fixture.root, &["commit", "-m", "add example_tests"]);
    run_git(&fixture.root, &["switch", "master"]);
    fixture.mq_ok(&["submit", "a"], "true");
    let env_log = fixture._temp.path().join("gate-env");
    let gate = format!(
        "printf 'skip_rust=%s skip_compile=%s skip_wasm=%s nextest=[%s] extra=[%s] expr=[%s] check=[%s]\\n' \
         \"${{WITCHY_GATE_SKIP_RUST_CLASS:-}}\" \"${{WITCHY_GATE_SKIP_COMPILE:-}}\" \
         \"${{WITCHY_GATE_SKIP_WASM:-}}\" \"${{WITCHY_GATE_NEXTEST:-}}\" \
         \"${{WITCHY_GATE_NEXTEST_EXTRA:-}}\" \"${{WITCHY_GATE_NEXTEST_EXPR:-}}\" \
         \"${{WITCHY_GATE_CHECK_PACKAGES:-}}\" >{}",
        env_log.display(),
    );
    fixture.mq_ok(&["run", "--once"], &gate);
    let env = fs::read_to_string(&env_log).expect("read classified gate env");
    assert!(
        env.contains("skip_rust=1") && env.contains("skip_compile=0") && env.contains("skip_wasm=1"),
        "interp+example_tests should skip rust-class/wasm and keep compile: {env}",
    );
    assert!(
        env.contains("-p witchy-interp")
            && env.contains("test(/^example_tests::equality::/)"),
        "crate+example_tests did not union the mapped crate with the touched module: {env}",
    );
    assert!(
        env.contains("extra=[]") && !env.contains("witchy-caps"),
        "interpreter edit still selected the unchanged caps test partition: {env}",
    );
    assert!(
        !env.contains("expr=[package(witchy-interp) or package(witchy-caps) or test(/^example_tests::/)"),
        "crate+file mapping still selected the whole example_tests matrix: {env}",
    );
    assert!(
        env.contains("check=[-p witchy-interp") && !env.contains("check=[-p witchy]"),
        "compile-check packages should be mapped crates, not the witchy umbrella: {env}",
    );
    assert!(
        !env.contains("nextest=[--workspace") && !env.contains("nextest=[]"),
        "crate+example_tests launched an unfiltered --workspace nextest: {env}",
    );
}

#[test]
fn book_only_batch_selects_documentation_examples_not_the_matrix() {
    let fixture = QueueFixture::stack(&["book/src/tour.md"]);
    fixture.mq_ok(&["submit", "a"], "true");
    let env_log = fixture._temp.path().join("gate-env");
    let gate = format!(
        "printf 'nextest=[%s] expr=[%s]\\n' \
         \"${{WITCHY_GATE_NEXTEST:-}}\" \"${{WITCHY_GATE_NEXTEST_EXPR:-}}\" >{}",
        env_log.display(),
    );
    fixture.mq_ok(&["run", "--once"], &gate);
    let env = fs::read_to_string(&env_log).expect("read classified gate env");
    assert!(
        env.contains("documentation_examples_are_valid"),
        "book-only batch did not select documentation_examples_are_valid: {env}",
    );
    assert!(
        !env.contains("expr=[test(/^example_tests::/)"),
        "book-only batch still selected the full example_tests matrix: {env}",
    );
}

#[test]
fn crate_plus_integration_test_unions_without_workspace() {
    let fixture = QueueFixture::stack(&["crates/witchy-lower/src/lib.rs"]);
    run_git(&fixture.root, &["switch", "a"]);
    fs::create_dir_all(fixture.root.join("tests")).expect("create tests dir");
    fs::write(fixture.root.join("tests/rfc0122.rs"), "telemetry\n").expect("write rfc0122 test");
    run_git(&fixture.root, &["add", "tests/rfc0122.rs"]);
    run_git(&fixture.root, &["commit", "-m", "add rfc0122 test"]);
    run_git(&fixture.root, &["switch", "master"]);
    fixture.mq_ok(&["submit", "a"], "true");
    let env_log = fixture._temp.path().join("gate-env");
    let gate = format!(
        "printf 'nextest=[%s] expr=[%s] check=[%s]\\n' \
         \"${{WITCHY_GATE_NEXTEST:-}}\" \"${{WITCHY_GATE_NEXTEST_EXPR:-}}\" \
         \"${{WITCHY_GATE_CHECK_PACKAGES:-}}\" >{}",
        env_log.display(),
    );
    fixture.mq_ok(&["run", "--once"], &gate);
    let env = fs::read_to_string(&env_log).expect("read classified gate env");
    assert!(
        env.contains("-p witchy-lower") && env.contains("binary(rfc0122)"),
        "crate+integration test did not union the crate with the binary: {env}",
    );
    assert!(
        env.contains("check=[-p witchy-lower") && !env.contains("check=[-p witchy]"),
        "compile-check packages should stay mapped crates: {env}",
    );
    assert!(
        !env.contains("nextest=[--workspace") && !env.contains("nextest=[]"),
        "crate+integration test launched an unfiltered --workspace nextest: {env}",
    );
}

fn classify_crate_skips(path: &str) -> (String, String) {
    let fixture = QueueFixture::stack(&[path]);
    fixture.mq_ok(&["submit", "a"], "true");
    let env_log = fixture._temp.path().join("gate-env");
    let gate = format!(
        "printf 'skip_rust=%s skip_compile=%s skip_wasm=%s skip_fmt=%s nextest=[%s]\\n' \
         \"${{WITCHY_GATE_SKIP_RUST_CLASS:-}}\" \"${{WITCHY_GATE_SKIP_COMPILE:-}}\" \
         \"${{WITCHY_GATE_SKIP_WASM:-}}\" \"${{WITCHY_GATE_SKIP_FMT:-}}\" \
         \"${{WITCHY_GATE_NEXTEST:-}}\" >{}",
        env_log.display(),
    );
    fixture.mq_ok(&["run", "--once"], &gate);
    (
        fs::read_to_string(&env_log).expect("read classified gate env"),
        path.to_owned(),
    )
}

#[test]
fn lower_and_syntax_batches_skip_rust_class_and_wasm() {
    thread::scope(|scope| {
        let cases = [
            "crates/witchy-lower/src/lib.rs",
            "crates/witchy-syntax/src/lib.rs",
            "crates/witchy-wir/src/lib.rs",
        ]
        .map(|path| {
            scope.spawn(move || {
                let (env, label) = classify_crate_skips(path);
                let want_fmt = if path.contains("witchy-syntax") {
                    "skip_fmt=0"
                } else {
                    "skip_fmt=1"
                };
                assert!(
                    env.contains("skip_rust=1")
                        && env.contains("skip_wasm=1")
                        && env.contains("skip_compile=0")
                        && env.contains(want_fmt),
                    "{label} should skip rust-class/wasm, keep compile, and classify fmt ({want_fmt}): {env}",
                );
                assert!(
                    !env.contains("nextest=[--workspace") && !env.contains("nextest=[]"),
                    "{label} launched unfiltered --workspace nextest: {env}",
                );
                if path.contains("witchy-lower") {
                    assert!(
                        env.contains("nextest=[-p witchy-lower")
                            && !env.contains("witchy-wir")
                            && !env.contains("witchy-runtime"),
                        "{label} still attached wir/runtime to a lower-only batch: {env}",
                    );
                }
            })
        });
        for case in cases {
            assert!(
                case.join().is_ok(),
                "crate classification fixture thread failed",
            );
        }
    });
}

#[test]
fn runtime_batch_skips_rust_class_and_the_browser_build() {
    // The wasm playground build is docs-site work and is never gated
    // (decided 2026-09-21); the compile check still runs.
    let (env, _) = classify_crate_skips("crates/witchy-runtime/src/lib.rs");
    assert!(
        env.contains("skip_rust=1") && env.contains("skip_wasm=1") && env.contains("skip_compile=0"),
        "runtime-only should skip rust-class and the browser build, keep compile: {env}",
    );
    assert!(
        !env.contains("nextest=[--workspace"),
        "runtime-only launched unfiltered --workspace nextest: {env}",
    );
}

#[test]
fn stale_gate_lock_reaps_its_recorded_process_group_before_regating() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");

    let mut orphan = Command::new("sh")
        .args(["-c", "sleep 30"])
        .process_group(0)
        .spawn()
        .expect("start abandoned gate fixture");
    let orphan_pgid = orphan.id() as i32;
    let _guard = ProcessGroupGuard(orphan_pgid);
    let gate_lock = fixture.state.join("gate.lock");
    fs::create_dir(&gate_lock).expect("create stale gate lock");
    fs::write(gate_lock.join("pid"), "999999\n").expect("write dead coordinator pid");
    fs::write(gate_lock.join("gate_pgid"), format!("{orphan_pgid}\n"))
        .expect("write abandoned gate process group");
    fs::write(gate_lock.join("what"), "abandoned full gate\n")
        .expect("write lock description");

    fixture.mq_ok(&["run", "--once"], "true");
    let status = orphan.wait().expect("reap abandoned gate fixture");
    assert!(!status.success(), "abandoned gate process group was not terminated");
    assert!(fixture.root.join("a.txt").exists(), "replacement gate did not land");
}

#[test]
fn coordinator_start_recovers_an_orphaned_gating_claim() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");
    let change_path = fixture.state.join("changes/a.json");
    let mut change: serde_json::Value = serde_json::from_slice(
        &fs::read(&change_path).expect("read queued change record"),
    )
    .expect("change record is JSON");
    change["state"] = serde_json::Value::String("gating".to_owned());
    fs::write(
        &change_path,
        serde_json::to_vec(&change).expect("serialize orphaned change record"),
    )
    .expect("write orphaned change record");

    fixture.mq_ok(&["run", "--once"], "true");

    assert!(fixture.root.join("a.txt").exists(), "recovered candidate did not land");
    assert!(
        fixture
            .journal()
            .iter()
            .any(|event| event["event"] == "recovered" && event["branch"] == "a"),
        "orphaned claim recovery was not journaled",
    );
}

#[test]
fn coordinator_exit_terminates_its_active_gate_process_group() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");
    let gate_started = fixture._temp.path().join("gate-started");
    let gate = format!(
        "printf started >{}; while :; do /bin/sleep 1; done",
        gate_started.display(),
    );

    let mut coordinator = fixture
        .mq_command(&["run", "--once"], &gate)
        .spawn()
        .expect("start coordinator with a blocking gate");
    let coordinator_pid = coordinator.id() as i32;

    let gate_pgid_path = fixture.state.join("gate.lock/gate_pgid");
    let deadline = Instant::now() + Duration::from_secs(20);
    while (!gate_started.exists() || !gate_pgid_path.exists()) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(gate_started.exists(), "blocking gate never started");
    let gate_pgid: i32 = fs::read_to_string(&gate_pgid_path)
        .expect("active gate PGID was not recorded in the lock")
        .trim()
        .parse()
        .expect("recorded gate PGID is numeric");
    let _gate_guard = ProcessGroupGuard(gate_pgid);

    let killed = Command::new("kill")
        .args(["-TERM", &coordinator_pid.to_string()])
        .status()
        .expect("terminate coordinator");
    assert!(killed.success(), "could not terminate coordinator");
    let status = coordinator.wait().expect("reap terminated coordinator");
    assert!(!status.success(), "terminated coordinator exited successfully");

    let deadline = Instant::now() + Duration::from_secs(5);
    while process_group_is_alive(gate_pgid) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        !process_group_is_alive(gate_pgid),
        "gate process group survived coordinator exit"
    );
    assert!(
        !fixture.state.join("gate.lock").exists(),
        "coordinator exit left the serialized gate lock behind"
    );
}

#[test]
fn dependency_submission_keeps_stable_ids_reports_readiness_and_rejects_cycles() {
    let fixture = QueueFixture::stack(&["a.txt", "b.txt"]);
    submit_stack(&fixture, &["a", "b"]);
    let first_id = fixture.change("a")["change_id"]
        .as_str()
        .expect("change id is a string")
        .to_owned();
    let first_attempt = fixture.change("a")["current_attempt"].clone();

    let status = fixture.status();
    let child = status["queue"]
        .as_array()
        .expect("queue is an array")
        .iter()
        .find(|entry| entry["branch"] == "b")
        .expect("child is queued");
    assert_eq!(child["readiness"], "waiting");
    assert_eq!(child["waiting_on"][0]["branch"], "a");

    run_git(&fixture.root, &["switch", "a"]);
    fs::write(fixture.root.join("a2.txt"), "a2\n").expect("update queued parent");
    run_git(&fixture.root, &["add", "a2.txt"]);
    run_git(&fixture.root, &["commit", "-m", "update a"]);
    run_git(&fixture.root, &["switch", "master"]);
    fixture.mq_ok(&["submit", "a"], "true");
    assert_eq!(fixture.change("a")["change_id"], first_id);
    assert_ne!(fixture.change("a")["current_attempt"], first_attempt);
    assert_eq!(
        fs::read_dir(fixture.state.join("queue"))
            .expect("read queue")
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .count(),
        2,
        "resubmit duplicated the logical change",
    );

    let cycle = fixture.mq(&["submit", "--after", "b", "a"], "true");
    assert!(!cycle.status.success(), "dependency cycle was accepted");
    assert!(
        String::from_utf8_lossy(&cycle.stderr).contains("would create a cycle"),
        "cycle rejection was not explicit: {}",
        String::from_utf8_lossy(&cycle.stderr),
    );
}

#[test]
fn submit_precheck_writes_only_to_an_ephemeral_object_database() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    let bin = fixture._temp.path().join("git-wrapper-bin");
    fs::create_dir(&bin).expect("create fake git bin");
    let real_git = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .expect("locate real git");
    assert!(real_git.status.success(), "git is required by the queue harness");
    let real_git = String::from_utf8(real_git.stdout)
        .expect("git path is utf8")
        .trim()
        .to_owned();
    let wrapper = bin.join("git");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\ncase \" $* \" in\n  *\" merge-tree --write-tree \"*)\n    [ -n \"${{GIT_OBJECT_DIRECTORY:-}}\" ] || exit 73\n    [ -n \"${{GIT_ALTERNATE_OBJECT_DIRECTORIES:-}}\" ] || exit 74\n    ;;\nesac\nexec {real_git} \"$@\"\n"
        ),
    )
    .expect("write git wrapper");
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755))
        .expect("chmod git wrapper");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let output = fixture
        .mq_command(&["submit", "a"], "true")
        .env("PATH", path)
        .output()
        .expect("submit through read-only-object precheck fixture");
    assert!(
        output.status.success(),
        "submit precheck required repository object writes:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn front_resubmission_moves_an_existing_change_to_the_actual_queue_head() {
    let fixture = QueueFixture::stack(&["a.txt", "b.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");
    fixture.mq_ok(&["submit", "b"], "true");

    fixture.mq_ok(&["submit", "--front", "b"], "true");

    let status = fixture.status();
    let queue = status["queue"].as_array().expect("queue is an array");
    assert_eq!(queue.len(), 2);
    assert_eq!(queue[0]["branch"], "b");
    assert_eq!(queue[1]["branch"], "a");
}

#[test]
fn urgent_front_submission_preempts_snapshot_preparation() {
    let fixture = QueueFixture::stack(&["std/a.witchy"]);
    run_git(&fixture.root, &["switch", "-c", "b", "master"]);
    fs::create_dir_all(fixture.root.join("scripts")).expect("create script directory");
    fs::write(fixture.root.join("scripts/merge-queue.sh"), "urgent\n")
        .expect("write urgent queue change");
    run_git(&fixture.root, &["add", "scripts/merge-queue.sh"]);
    run_git(&fixture.root, &["commit", "-m", "urgent queue fix"]);
    run_git(&fixture.root, &["switch", "master"]);
    fixture.mq_ok(&["submit", "a"], "true");

    let fake_bin = fixture._temp.path().join("fake-cargo");
    let preparation_started = fixture._temp.path().join("preparation-started");
    let preparation_release = fixture._temp.path().join("preparation-release");
    fs::create_dir(&fake_bin).expect("create fake cargo directory");
    let cargo = fake_bin.join("cargo");
    fs::write(
        &cargo,
        format!(
            "#!/bin/sh\ntouch '{}'\nwhile [ ! -f '{}' ]; do sleep 0.01; done\nexit 1\n",
            preparation_started.display(),
            preparation_release.display(),
        ),
    )
    .expect("write paused cargo");
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755))
        .expect("chmod paused cargo");
    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let gate_order = fixture._temp.path().join("gate-order");
    let gate = format!(
        "if [ -f std/a.witchy ]; then printf 'a\\n'; else printf 'b\\n'; fi >>'{}'",
        gate_order.display(),
    );
    let runner = fixture
        .mq_command(&["run", "--once"], &gate)
        .env("PATH", path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start queue during snapshot preparation");

    let deadline = Instant::now() + Duration::from_secs(30);
    while !preparation_started.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        preparation_started.exists(),
        "candidate did not reach snapshot preparation"
    );
    fixture.mq_ok(&["submit", "--front", "b"], "true");
    fs::write(&preparation_release, "go\n").expect("release snapshot preparation");

    let output = runner.wait_with_output().expect("wait for preempting queue run");
    assert!(
        output.status.success(),
        "queue run failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        fs::read_to_string(&gate_order).expect("read gate order"),
        "b\na\n",
        "the already-prepared normal candidate gated before the urgent front submission"
    );
    assert!(fixture.journal().iter().any(|event| {
        event["event"] == "requeued"
            && event["branch"] == "a"
            && event["reason"] == "urgent front submission arrived during preparation"
            && event["urgent_branch"] == "b"
    }));
}

#[test]
fn reused_branch_gets_a_new_change_id_without_forgetting_old_dependencies() {
    let fixture = QueueFixture::stack(&["a.txt", "b.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");
    let old_id = fixture.change("a")["change_id"]
        .as_str()
        .expect("change id is a string")
        .to_owned();
    fixture.mq_ok(&["run", "--once"], "true");

    fixture.mq_ok(&["submit", "--after", "a", "b"], "true");
    run_git(&fixture.root, &["branch", "a", "master"]);
    run_git(&fixture.root, &["switch", "a"]);
    fs::write(fixture.root.join("a-next.txt"), "next generation\n")
        .expect("write next branch generation");
    run_git(&fixture.root, &["add", "a-next.txt"]);
    run_git(&fixture.root, &["commit", "-m", "reuse branch a"]);
    run_git(&fixture.root, &["switch", "master"]);
    fixture.mq_ok(&["submit", "a"], "true");

    let new_id = fixture.change("a")["change_id"]
        .as_str()
        .expect("new change id is a string")
        .to_owned();
    assert_ne!(new_id, old_id, "completed branch generation reused its old ID");
    let old_record: serde_json::Value = serde_json::from_slice(
        &fs::read(fixture.state.join(format!("changes/history-{old_id}.json")))
            .expect("old change generation remains addressable"),
    )
    .expect("old change record is JSON");
    assert_eq!(old_record["state"], "merged");

    let status = fixture.status();
    let old_child = status["queue"]
        .as_array()
        .expect("queue is an array")
        .iter()
        .find(|entry| entry["branch"] == "b")
        .expect("old child remains queued");
    assert_eq!(old_child["readiness"], "ready");
    assert_eq!(old_child["dependencies"][0]["change_id"], old_id);
}

#[test]
fn resubmission_during_gate_is_not_deleted_or_falsely_marked_merged() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");
    let change_id = fixture.change("a")["change_id"].clone();
    let first_attempt = fixture.change("a")["current_attempt"].clone();
    let started = fixture._temp.path().join("gate-started");
    let proceed = fixture._temp.path().join("gate-proceed");
    let gate = format!(
        "touch '{}'; while [ ! -f '{}' ]; do sleep 0.01; done; true",
        started.display(),
        proceed.display(),
    );
    let mut runner = fixture.mq_command(&["run", "--once"], &gate);
    let runner = runner
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start paused queue gate");
    // Checkout and replay are outside the gate lock and can take several
    // seconds on a loaded shared machine. Wait for the explicit gate marker;
    // this outer bound is deadlock protection, not the synchronization rule.
    let deadline = Instant::now() + Duration::from_secs(30);
    while !started.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(started.exists(), "gate did not reach its pause point");

    run_git(&fixture.root, &["switch", "a"]);
    fs::write(fixture.root.join("a2.txt"), "updated while gating\n")
        .expect("write in-flight update");
    run_git(&fixture.root, &["add", "a2.txt"]);
    run_git(&fixture.root, &["commit", "-m", "update a while gating"]);
    run_git(&fixture.root, &["switch", "master"]);
    fixture.mq_ok(&["submit", "a"], "true");
    assert_eq!(fixture.change("a")["change_id"], change_id);
    let second_attempt = fixture.change("a")["current_attempt"].clone();
    assert_ne!(second_attempt, first_attempt);
    fs::write(&proceed, "go\n").expect("release paused gate");

    let output = runner.wait_with_output().expect("wait for queue drain");
    assert!(
        output.status.success(),
        "queue run failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        run_git(&fixture.root, &["show", "master:a2.txt"])
            .status
            .success()
    );
    assert_eq!(fixture.change("a")["state"], "merged");
    let journal = fixture.journal();
    let merged: Vec<_> = journal
        .iter()
        .filter(|event| event["event"] == "merged" && event["branch"] == "a")
        .collect();
    assert_eq!(
        merged.len(),
        1,
        "superseded and updated attempts both landed"
    );
    assert_eq!(merged[0]["attempt_id"], second_attempt);
    assert!(
        journal.iter().any(|event| {
            event["event"] == "requeued"
                && event["attempt_id"] == first_attempt
                && event["reason"] == "submission changed during gate"
        }),
        "superseded green attempt was not rejected before landing: {journal:?}",
    );
}

#[test]
fn concurrent_dependency_updates_cannot_commit_opposite_cycle_edges() {
    let fixture = QueueFixture::stack(&["a.txt", "b.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");
    fixture.mq_ok(&["submit", "b"], "true");

    let mut left = fixture.mq_command(&["submit", "--after", "b", "a"], "true");
    let mut right = fixture.mq_command(&["submit", "--after", "a", "b"], "true");
    let left = left
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start first concurrent dependency update");
    let right = right
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start second concurrent dependency update");
    let left = left.wait_with_output().expect("wait for first dependency update");
    let right = right.wait_with_output().expect("wait for second dependency update");

    assert_ne!(
        left.status.success(),
        right.status.success(),
        "exactly one opposite edge must commit:\nleft: {}\nright: {}",
        String::from_utf8_lossy(&left.stderr),
        String::from_utf8_lossy(&right.stderr),
    );
    let edge_count = fixture.change("a")["after"].as_array().unwrap().len()
        + fixture.change("b")["after"].as_array().unwrap().len();
    assert_eq!(edge_count, 1, "both cycle edges were persisted");
    assert!(!fixture.state.join("change.lock").exists(), "metadata lock leaked");
}

#[test]
fn merge_commit_submission_is_rejected_without_gating_or_erasing_resolutions() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    run_git(&fixture.root, &["switch", "-c", "side", "master"]);
    fs::write(fixture.root.join("side.txt"), "side\n").expect("write side branch");
    run_git(&fixture.root, &["add", "side.txt"]);
    run_git(&fixture.root, &["commit", "-m", "side change"]);
    run_git(&fixture.root, &["switch", "a"]);
    run_git(&fixture.root, &["merge", "--no-ff", "side", "-m", "merge with resolution boundary"]);
    run_git(&fixture.root, &["switch", "master"]);

    fixture.mq_ok(&["submit", "a"], "true");
    let marker = fixture.root.join("gate-must-not-run");
    let gate = format!("printf gated >{}", marker.display());
    fixture.mq_ok(&["run", "--once"], &gate);

    assert!(!marker.exists(), "merge-commit candidate reached the gate");
    assert_eq!(fixture.change("a")["state"], "conflict");
    assert!(fixture
        .journal()
        .iter()
        .any(|event| event["event"] == "conflict" && event["branch"] == "a"));
}

#[test]
fn red_parent_blocks_child_until_the_same_change_is_resubmitted_green() {
    let fixture = QueueFixture::stack(&["a.txt", "b.txt", "c.txt"]);
    submit_stack(&fixture, &["a", "b", "c"]);
    let parent_id = fixture.change("a")["change_id"].clone();

    // This assertion owns dependency blocking, not red-batch attribution. Keep
    // the red attempt to its parent; the dedicated prefix-split fixture covers
    // batch blame and replay independently.
    let red = fixture
        .mq_command(&["run", "--once"], "false")
        .env("MERGE_QUEUE_BATCH_MAX", "1")
        .output()
        .expect("run isolated red parent");
    assert!(
        red.status.success(),
        "isolated red-parent run failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&red.stdout),
        String::from_utf8_lossy(&red.stderr),
    );
    let status = fixture.status();
    let child = status["queue"]
        .as_array()
        .expect("queue is an array")
        .iter()
        .find(|entry| entry["branch"] == "b")
        .expect("red parent's child remains queued");
    assert_eq!(child["readiness"], "blocked");
    assert_eq!(child["blocked_by"][0]["branch"], "a");
    assert_eq!(child["blocked_by"][0]["state"], "red");
    let grandchild = status["queue"]
        .as_array()
        .expect("queue is an array")
        .iter()
        .find(|entry| entry["branch"] == "c")
        .expect("red parent's grandchild remains queued");
    assert_eq!(grandchild["readiness"], "blocked");
    assert!(grandchild["blocked_by"]
        .as_array()
        .expect("blocked_by is an array")
        .iter()
        .any(|dependency| dependency["branch"] == "a" && dependency["state"] == "red"));
    assert!(
        !git_output(&fixture.root, &["show", "master:b.txt"]).status.success(),
        "child landed after a red parent",
    );

    fixture.mq_ok(&["submit", "a"], "true");
    assert_eq!(fixture.change("a")["change_id"], parent_id);
    fixture.mq_ok(&["run", "--once"], "true");
    assert!(run_git(&fixture.root, &["show", "master:c.txt"]).status.success());
    assert_eq!(fixture.change("a")["state"], "merged");
    assert_eq!(fixture.change("b")["state"], "merged");
    assert_eq!(fixture.change("c")["state"], "merged");
}

#[test]
fn represented_child_does_not_wait_for_an_obsolete_red_parent() {
    let fixture = QueueFixture::stack(&["a.txt", "b.txt", "c.txt"]);
    submit_stack(&fixture, &["a", "b", "c"]);

    // This assertion owns stale dependency representation, not red-batch
    // attribution. Keep the initial red to the parent; prefix splitting has a
    // dedicated fixture below.
    let red = fixture
        .mq_command(&["run", "--once"], "false")
        .env("MERGE_QUEUE_BATCH_MAX", "1")
        .output()
        .expect("run isolated obsolete parent");
    assert!(
        red.status.success(),
        "isolated obsolete-parent run failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&red.stdout),
        String::from_utf8_lossy(&red.stderr),
    );
    assert_eq!(fixture.change("a")["state"], "red");
    assert_eq!(fixture.change("b")["state"], "queued");

    // Model a canonical integration tip landing the parent and child patches
    // without rewriting this obsolete queue graph.
    run_git(&fixture.root, &["cherry-pick", "a"]);
    run_git(&fixture.root, &["cherry-pick", "b"]);

    fixture.mq_ok(&["run", "--once"], "true");
    assert_eq!(fixture.change("a")["state"], "red");
    assert_eq!(fixture.change("b")["state"], "merged");
    assert_eq!(fixture.change("c")["state"], "queued");
    assert!(fixture.journal().iter().any(|event| {
        event["event"] == "already_merged" && event["branch"] == "b"
    }));
    assert!(
        !git_output(&fixture.root, &["show", "master:c.txt"])
            .status
            .success(),
        "unrepresented grandchild bypassed its red dependency",
    );
}

#[test]
fn operator_drop_retires_only_the_pending_attempt_and_preserves_its_branch() {
    let fixture = QueueFixture::stack(&["a.txt", "b.txt"]);
    submit_stack(&fixture, &["a", "b"]);
    let branch_sha = git(&fixture.root, &["rev-parse", "a"]);

    fixture.mq_ok(
        &["drop", "a", "superseded by canonical integration tip"],
        "true",
    );

    assert_eq!(git(&fixture.root, &["rev-parse", "a"]), branch_sha);
    assert_eq!(fixture.change("a")["state"], "dropped");
    assert_eq!(
        fixture.change("a")["drop_reason"],
        "superseded by canonical integration tip"
    );
    assert_eq!(fixture.change("b")["state"], "queued");
    let status = fixture.status();
    let queued = status["queue"].as_array().expect("queue is an array");
    assert!(queued.iter().all(|entry| entry["branch"] != "a"));
    assert!(queued.iter().any(|entry| {
        entry["branch"] == "b"
            && entry["readiness"] == "blocked"
            && entry["blocked_by"][0]["state"] == "dropped"
    }));
    assert!(fixture.journal().iter().any(|event| {
        event["event"] == "dropped"
            && event["branch"] == "a"
            && event["reason"] == "superseded by canonical integration tip"
            && event["via"] == "operator drop"
    }));
}

#[test]
fn operator_drop_refuses_an_attempt_that_has_entered_the_gate() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");
    let started = fixture._temp.path().join("drop-gate-started");
    let proceed = fixture._temp.path().join("drop-gate-proceed");
    let gate = format!(
        "touch '{}'; while [ ! -f '{}' ]; do sleep 0.01; done; true",
        started.display(),
        proceed.display(),
    );
    let mut runner = fixture.mq_command(&["run", "--once"], &gate);
    let runner = runner
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start paused queue gate");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !started.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(started.exists(), "gate did not reach its pause point");

    let drop = fixture.mq(&["drop", "a", "too late"], "true");
    assert!(!drop.status.success(), "active gate was dropped");
    assert!(
        String::from_utf8_lossy(&drop.stderr).contains("current attempt is gating"),
        "drop refusal did not name the active state: {}",
        String::from_utf8_lossy(&drop.stderr),
    );
    fs::write(&proceed, "go\n").expect("release paused gate");
    let output = runner.wait_with_output().expect("wait for queue drain");
    assert!(
        output.status.success(),
        "queue run failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(fixture.change("a")["state"], "merged");
}

#[test]
fn green_dependency_stack_gates_the_tip_once_and_lands_the_whole_stack() {
    let fixture = QueueFixture::stack(&["a.txt", "b.txt", "c.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");
    fixture.mq_ok(&["submit", "b"], "true");
    fixture.mq_ok(&["submit", "--after", "a", "--after", "b", "c"], "true");
    fixture.mq_ok(&["run", "--once"], "true");

    assert!(run_git(&fixture.root, &["show", "master:c.txt"]).status.success());
    let merged: Vec<_> = fixture
        .journal()
        .into_iter()
        .filter(|event| event["event"] == "merged")
        .collect();
    assert_eq!(merged.len(), 3);
    assert!(merged.iter().all(|event| event["batch"] == "3"));
    let logs: BTreeSet<_> = merged
        .iter()
        .map(|event| event["log"].as_str().expect("merged event has log"))
        .collect();
    assert_eq!(logs.len(), 1, "stack used more than one green gate");
}

#[test]
fn markdown_only_stack_can_exceed_the_semantic_batch_limit() {
    let fixture = QueueFixture::stack(&["a.md", "b.md"]);
    submit_stack(&fixture, &["a", "b"]);
    let output = fixture
        .mq_command(&["run", "--once"], "true")
        .env("MERGE_QUEUE_BATCH_MAX", "1")
        .env("MERGE_QUEUE_DOCS_BATCH_MAX", "20")
        .output()
        .expect("run documentation batch");
    assert!(
        output.status.success(),
        "documentation batch failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(run_git(&fixture.root, &["show", "master:b.md"]).status.success());
    let merged: Vec<_> = fixture
        .journal()
        .into_iter()
        .filter(|event| event["event"] == "merged")
        .collect();
    assert_eq!(merged.len(), 2);
    assert!(merged.iter().all(|event| event["batch"] == "2"));
}

#[test]
fn documentation_batch_rejects_a_non_markdown_candidate() {
    let fixture = QueueFixture::stack(&["a.md", "b.txt", "c.md"]);
    submit_stack(&fixture, &["a", "b", "c"]);
    fixture.mq_ok(&["run", "--once"], "true");

    assert!(run_git(&fixture.root, &["show", "master:a.md"]).status.success());
    let merged: Vec<_> = fixture
        .journal()
        .into_iter()
        .filter(|event| event["event"] == "merged")
        .collect();
    assert_eq!(merged.len(), 3);
    assert_eq!(merged[0]["batch"], "1");
    assert_ne!(merged[0]["log"], merged[1]["log"]);
    assert_eq!(merged[1]["batch"], "2");
    assert_eq!(merged[1]["log"], merged[2]["log"]);
}

#[test]
fn queue_substrate_change_requests_the_isolated_fixture_shard() {
    let fixture = QueueFixture::stack(&["scripts/merge-queue.sh"]);
    fixture.mq_ok(&["submit", "a"], "true");
    fixture.mq_ok(
        &["run", "--once"],
        "test \"$WITCHY_GATE_QUEUE_INFRA\" = 1",
    );
    assert!(
        run_git(&fixture.root, &["show", "master:scripts/merge-queue.sh"])
            .status
            .success()
    );
}

#[test]
fn queue_core_only_change_runs_the_hermetic_shard_instead_of_the_product_gate() {
    let fixture = QueueFixture::stack(&["scripts/merge-queue.sh"]);
    let check = fixture.root.join("scripts/check.sh");
    fs::create_dir_all(check.parent().expect("check script has a parent"))
        .expect("create fixture scripts directory");
    fs::write(
        &check,
        "#!/bin/sh\nset -eu\ntest \"$1\" = --queue-infra\ntest \"$WITCHY_GATE_QUEUE_INFRA\" = 1\n: >\"$QUEUE_SHARD_MARKER\"\n",
    )
    .expect("write fixture queue shard");
    fs::set_permissions(&check, fs::Permissions::from_mode(0o755))
        .expect("make fixture queue shard executable");
    run_git(&fixture.root, &["add", "scripts/check.sh"]);
    run_git(&fixture.root, &["commit", "-m", "add fixture product gate"]);
    run_git(&fixture.root, &["switch", "a"]);
    fs::write(
        fixture.root.join("scripts/MERGE-QUEUE.md"),
        "queue operator documentation\n",
    )
    .expect("write queue operator documentation");
    run_git(&fixture.root, &["add", "scripts/MERGE-QUEUE.md"]);
    run_git(&fixture.root, &["commit", "-m", "document queue substrate"]);
    run_git(&fixture.root, &["switch", "master"]);
    fixture.mq_ok(&["submit", "a"], "true");

    let marker = fixture._temp.path().join("queue-shard-ran");
    let mut command = fixture.mq_command(&["run", "--once"], "false");
    let output = command
        .env_remove("MERGE_QUEUE_GATE_CMD")
        .env("QUEUE_SHARD_MARKER", &marker)
        .output()
        .expect("run queue-core-only gate");
    assert!(
        output.status.success(),
        "queue-only gate failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(marker.exists(), "full product command replaced the queue shard");
    assert!(run_git(&fixture.root, &["show", "master:scripts/merge-queue.sh"])
        .status
        .success());
}

/// The coordinator's refusal to gate against a dirty shared checkout was only
/// visible in its own log and in `doctor`. The submitter is the one agent
/// certain to care, and is standing right there, so `submit` says it too.
#[test]
fn submit_warns_that_a_dirty_main_checkout_blocks_the_queue() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fs::write(fixture.root.join("base.txt"), "locally edited\n")
        .expect("dirty the main master checkout");

    let output = fixture.mq_ok(&["submit", "a"], "true");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("CANNOT gate until the shared checkout is clean"),
        "submit did not warn about the dirty shared checkout: {stderr}",
    );
    assert!(
        stderr.contains("base.txt"),
        "the warning did not name the blocking file: {stderr}",
    );
    assert_eq!(fixture.change("a")["state"], "queued", "the warning must not block submission");
}

/// ...and stays quiet when the checkout is clean, so the warning keeps meaning
/// something.
#[test]
fn submit_is_quiet_about_the_checkout_when_it_is_clean() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    let output = fixture.mq_ok(&["submit", "a"], "true");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("CANNOT gate"),
        "submit warned about a clean checkout: {stderr}",
    );
}

#[test]
fn dirty_main_master_defers_the_queue_before_running_the_gate() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");
    fs::write(fixture.root.join("base.txt"), "locally edited\n")
        .expect("dirty the main master checkout");

    let marker = fixture.root.join("gate-ran");
    let gate = format!("printf ran > {}", marker.display());
    fixture.mq_ok(&["run", "--once"], &gate);

    assert!(
        !marker.exists(),
        "the full gate ran despite tracked changes in the main master checkout"
    );
    assert_eq!(fixture.change("a")["state"], "queued");
    assert_eq!(
        fs::read_dir(fixture.state.join("queue"))
            .expect("read deferred queue")
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|extension| extension == "json"))
            .count(),
        1,
        "the deferred submission was consumed"
    );
    assert!(fixture.journal().iter().any(|event| {
        event["event"] == "requeued"
            && event["branch"] == "a"
            && event["reason"] == "main master checkout has tracked changes before gate"
    }));
}

// The deferral above is the queue's one silent failure mode: a ready entry with
// a free gate lock reads as a healthy idle queue, so an operator watching
// `status` sees nothing while the coordinator requeues every poll. `blocked_on`
// names the cause and the offending paths.
#[test]
fn status_names_the_dirty_main_checkout_that_blocks_landing() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");
    assert!(
        fixture.status()["blocked_on"].is_null(),
        "a clean main checkout must not report a landing blocker"
    );

    fs::write(fixture.root.join("base.txt"), "locally edited\n")
        .expect("dirty the main master checkout");
    let blocked = fixture.status()["blocked_on"]
        .as_str()
        .expect("blocked_on names the dirty checkout")
        .to_string();
    assert!(blocked.contains("tracked changes"), "unexpected reason: {blocked}");
    assert!(blocked.contains("base.txt"), "reason omits the offending path: {blocked}");

    // Untracked files are deliberately not a blocker: they are common in the
    // shared checkout and only conflict when a candidate writes the same path.
    run_git(&fixture.root, &["checkout", "--", "base.txt"]);
    fs::write(fixture.root.join("scratch-note.txt"), "untracked\n").expect("add untracked file");
    assert!(
        fixture.status()["blocked_on"].is_null(),
        "untracked local state must not report a landing blocker"
    );
}

#[test]
fn status_reports_multiple_queue_entries_from_one_registry_snapshot() {
    let fixture = QueueFixture::stack(&["a.txt", "b.txt"]);
    submit_stack(&fixture, &["a", "b"]);

    let status = fixture.status();
    let queue = status["queue"].as_array().expect("queue is an array");
    assert_eq!(queue.len(), 2);
    let a = queue.iter().find(|entry| entry["branch"] == "a").expect("a is queued");
    let b = queue.iter().find(|entry| entry["branch"] == "b").expect("b is queued");
    assert_eq!(a["readiness"], "ready");
    assert_eq!(b["readiness"], "waiting");
    assert_eq!(b["waiting_on"][0]["branch"], "a");
}

#[test]
fn red_dependency_stack_bisects_and_lands_only_the_green_prefix() {
    let fixture = QueueFixture::stack(&["a.txt", "b.txt", "c.txt"]);
    submit_stack(&fixture, &["a", "b", "c"]);
    fixture.mq_ok(&["run", "--once"], "test ! -f c.txt");

    assert!(run_git(&fixture.root, &["show", "master:a.txt"]).status.success());
    assert!(run_git(&fixture.root, &["show", "master:b.txt"]).status.success());
    let missing_c = Command::new("git")
        .args(["-C", fixture.root.to_str().unwrap(), "show", "master:c.txt"])
        .output()
        .expect("inspect red suffix on master");
    assert!(!missing_c.status.success(), "red stack suffix landed");

    let journal = fixture.journal();
    let split = journal
        .iter()
        .find(|event| event["event"] == "batch_red")
        .expect("initial stack red was journaled");
    assert_eq!(split["strategy"], "prefix_split");
    assert_eq!(split["members"], "a b c");
    assert!(journal.iter().any(|event| event["event"] == "merged" && event["branch"] == "a"));
    assert!(journal.iter().any(|event| event["event"] == "merged" && event["branch"] == "b"));
    assert!(journal.iter().any(|event| event["event"] == "red" && event["branch"] == "c"));
    let gate_logs: BTreeSet<_> = journal
        .iter()
        .filter(|event| {
            event["event"] == "batch_red" || event["event"] == "merged" || event["event"] == "red"
        })
        .filter_map(|event| event["log"].as_str())
        .collect();
    assert_eq!(gate_logs.len(), 3, "expected full stack, prefix, and suffix gates");
}

#[test]
fn daemon_enters_an_independent_process_group() {
    let root = workspace_root();
    let temp = TempDir::new();
    let isolated_root = temp.path().join("isolated-root");
    let isolated_scripts = isolated_root.join("scripts");
    fs::create_dir_all(&isolated_scripts).expect("create isolated script directory");
    let queue = isolated_scripts.join("merge-queue.sh");
    fs::copy(root.join("scripts/merge-queue.sh"), &queue).expect("copy queue script");
    fs::copy(
        root.join("scripts/state-paths.sh"),
        isolated_scripts.join("state-paths.sh"),
    )
    .expect("copy state path script");
    let state_root = temp.path().join("state-root");
    let state = state_root.join("merge-queue");
    let gate_worktree = temp.path().join("unused-gate-worktree");

    // Keep the daemon command's launcher alive in a dedicated process group so
    // this test can simulate the tool host reaping its whole group. Querying
    // PGIDs with `ps` is denied in the same sandbox where the coordinator gate
    // runs, while survival after group termination tests the actual invariant.
    let mut launcher = Command::new("bash")
        .args([
            "-c",
            r#""$1" daemon || exit $?; sleep 30"#,
            "merge-queue-daemon-launcher",
        ])
        .arg(&queue)
        .env("MERGE_QUEUE_TEST_ROOT", &root)
        .env("MERGE_QUEUE_ALLOW_TEST_ROOT", "1")
        .env("WITCHY_STATE_DIR", &state_root)
        .env("MERGE_QUEUE_GATE_WT", &gate_worktree)
        .env("MERGE_QUEUE_GATE_CMD", "true")
        .env("MERGE_QUEUE_COORDINATOR_SCRIPT", &queue)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("start isolated coordinator daemon launcher");
    let launcher_group = launcher.id() as i32;

    let pid_path = state.join("coordinator.pid");
    // The daemon command has its own readiness handshake. This outer bound is
    // only deadlock protection and remains below nextest's hard timeout.
    let deadline = Instant::now() + Duration::from_secs(90);
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
    assert_ne!(
        process_group(pid),
        launcher_group,
        "coordinator {pid} remained in the launcher's process group"
    );
    let killed = Command::new("kill")
        .args(["-TERM", "--", &format!("-{launcher_group}")])
        .status()
        .expect("terminate daemon launcher process group");
    assert!(killed.success(), "could not terminate daemon launcher group");
    let _ = launcher.wait();

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

#[test]
fn coordinator_reexecs_committed_script_between_attempts_without_losing_singleton() {
    let source_root = workspace_root();
    let temp = TempDir::new();
    let root = temp.path().join("repo");
    let state = temp.path().join("state");
    let gate_worktree = temp.path().join("gate");
    let scripts = root.join("scripts");
    fs::create_dir_all(&scripts).expect("create fixture scripts directory");
    fs::copy(
        source_root.join("scripts/merge-queue.sh"),
        scripts.join("merge-queue.sh"),
    )
    .expect("copy coordinator script");
    fs::copy(
        source_root.join("scripts/state-paths.sh"),
        scripts.join("state-paths.sh"),
    )
    .expect("copy state path helper");
    fs::write(root.join("base.txt"), "base\n").expect("write base file");
    run_git(&root, &["init", "-b", "master"]);
    run_git(
        &root,
        &["config", "user.email", "queue-test@witchy.invalid"],
    );
    run_git(&root, &["config", "user.name", "Witchy Queue Test"]);
    run_git(&root, &["add", "."]);
    run_git(&root, &["commit", "-m", "base"]);

    let marker = temp.path().join("reexec-marker");
    run_git(&root, &["switch", "-c", "a"]);
    fs::write(root.join("a.txt"), "a\n").expect("write first change");
    let script_path = scripts.join("merge-queue.sh");
    let original = fs::read_to_string(&script_path).expect("read coordinator script");
    let instrumented = original.replacen(
        "set -euo pipefail\n",
        "set -euo pipefail\n[ -z \"${MERGE_QUEUE_REEXEC_TEST_MARKER:-}\" ] || printf '%s\\n' \"$$\" >\"$MERGE_QUEUE_REEXEC_TEST_MARKER\"\n",
        1,
    );
    assert_ne!(
        instrumented, original,
        "could not instrument reexec generation"
    );
    fs::write(&script_path, instrumented).expect("write second coordinator generation");
    run_git(&root, &["add", "a.txt", "scripts/merge-queue.sh"]);
    run_git(&root, &["commit", "-m", "update coordinator"]);
    run_git(&root, &["switch", "-c", "b"]);
    fs::write(root.join("b.txt"), "b\n").expect("write second change");
    run_git(&root, &["add", "b.txt"]);
    run_git(&root, &["commit", "-m", "add second change"]);
    run_git(&root, &["switch", "master"]);

    let fixture = QueueFixture {
        _temp: temp,
        root,
        state,
        gate_worktree,
    };
    let coordinator_script = fixture.root.join("scripts/merge-queue.sh");
    fixture
        .mq_command(&["submit", "a"], "true")
        .env("MERGE_QUEUE_COORDINATOR_SCRIPT", &coordinator_script)
        .output()
        .expect("submit coordinator update");
    fixture
        .mq_command(&["submit", "--after", "a", "b"], "true")
        .env("MERGE_QUEUE_COORDINATOR_SCRIPT", &coordinator_script)
        .output()
        .expect("submit change behind coordinator update");

    let mut coordinator = fixture
        .mq_command(&["run"], "true")
        .env("MERGE_QUEUE_COORDINATOR_SCRIPT", &coordinator_script)
        .env("MERGE_QUEUE_REEXEC_TEST_MARKER", &marker)
        .env("MERGE_QUEUE_BATCH_MAX", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("start persistent coordinator");
    let pid = coordinator.id() as i32;
    let guard = ProcessGroupGuard(pid);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let journal = fs::read_to_string(fixture.state.join("journal.jsonl")).unwrap_or_default();
        let merged = journal
            .lines()
            .filter(|line| line.contains("\"event\":\"merged\""))
            .count();
        if merged == 2 && marker.exists() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "coordinator did not reload and drain both attempts; journal:\n{journal}",
        );
        thread::sleep(Duration::from_millis(25));
    }

    assert_eq!(
        fs::read_to_string(&marker)
            .expect("read reexec marker")
            .trim(),
        pid.to_string(),
        "reexec changed the coordinator pid",
    );
    assert_eq!(
        fs::read_to_string(fixture.state.join("coordinator.lock/pid"))
            .expect("read singleton lock owner")
            .trim(),
        pid.to_string(),
        "reexec lost singleton ownership",
    );
    let reexecs = fixture
        .journal()
        .iter()
        .filter(|event| event["event"] == "coordinator_reexec")
        .count();
    assert_eq!(reexecs, 1, "expected exactly one committed-script reload");
    assert!(process_is_alive(pid), "coordinator exited after reexec");

    drop(guard);
    let _ = coordinator.wait();
}

#[test]
fn concurrent_daemon_starts_create_exactly_one_coordinator() {
    let root = workspace_root();
    let temp = TempDir::new();
    let state = temp.path().join("state");
    let gate_worktree = temp.path().join("unused-gate-worktree");

    let mut starters = Vec::new();
    for _ in 0..8 {
        starters.push(
            Command::new(root.join("scripts/merge-queue.sh"))
                .arg("daemon")
                .env("MERGE_QUEUE_STATE_DIR", &state)
            .env("MERGE_QUEUE_AUTOSTART", "0")
                .env("MERGE_QUEUE_GATE_WT", &gate_worktree)
                .env("MERGE_QUEUE_GATE_CMD", "true")
                .env(
                    "MERGE_QUEUE_COORDINATOR_SCRIPT",
                    root.join("scripts/merge-queue.sh"),
                )
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("race coordinator daemon start"),
        );
    }
    for mut starter in starters {
        assert!(starter.wait().expect("wait for daemon starter").success());
    }

    let pid: i32 = fs::read_to_string(state.join("coordinator.pid"))
        .expect("read winning coordinator pid")
        .trim()
        .parse()
        .expect("parse winning coordinator pid");
    let guard = ProcessGroupGuard(pid);
    assert!(process_is_alive(pid), "winning coordinator is not alive");
    assert_eq!(
        fs::read_to_string(state.join("coordinator.lock/pid"))
            .expect("read singleton lock owner")
            .trim(),
        pid.to_string(),
    );
    let log = fs::read_to_string(state.join("coordinator.log"))
        .expect("read concurrent-start coordinator log");
    assert_eq!(
        log.matches("coordinator up (pid ").count(),
        1,
        "more than one persistent loop started:\n{log}",
    );

    drop(guard);
    let deadline = Instant::now() + Duration::from_secs(5);
    while process_is_alive(pid) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(!process_is_alive(pid), "winning coordinator ignored termination");
}


/// RFC-0159 §D. The receipt mechanism exists; what was missing is that nothing
/// journaled it, so "do pre-submit shards predict gate outcomes?" stayed
/// unanswerable. Absence must be an explicit `false`, never a missing field, or
/// adoption cannot be counted.
#[test]
fn submit_journals_whether_a_presubmit_receipt_covers_the_tip() {
    let fixture = QueueFixture::stack(&["a.txt"]);

    fixture.mq_ok(&["submit", "a"], "true");
    let submitted = fixture
        .journal()
        .into_iter()
        .filter(|event| event["event"] == "submitted")
        .next_back()
        .expect("a submitted event");
    assert_eq!(
        submitted["presubmit_sha_match"], "false",
        "absence must be recorded explicitly: {submitted}"
    );
    assert_eq!(submitted["presubmit_eligible"], "true");

    // Now mint a receipt for the branch tip, exactly as a green shard would.
    let tip = String::from_utf8(
        git_output(&fixture.root, &["rev-parse", "refs/heads/a"]).stdout,
    )
    .expect("tip sha is utf8")
    .trim()
    .to_owned();
    let receipts = fixture.state.join("receipts");
    fs::create_dir_all(&receipts).expect("create receipts dir");
    fs::write(
        receipts.join(format!("{tip}.json")),
        format!(
            "{{\"sha\":\"{tip}\",\"shards\":[\"fast\",\"wasm\"],\"durations_s\":{{\"edit\":37}},\"at\":\"2026-08-26T00:00:00Z\"}}"
        ),
    )
    .expect("write receipt");

    fixture.mq_ok(&["submit", "a"], "true");
    let resubmitted = fixture
        .journal()
        .into_iter()
        .filter(|event| event["event"] == "resubmitted")
        .next_back()
        .expect("a resubmitted event");
    assert_eq!(resubmitted["presubmit_sha_match"], "true");
    assert_eq!(
        resubmitted["presubmit_shard"], "fast,wasm",
        "every shard that covered the tip must survive, not just the last writer"
    );
    assert_eq!(resubmitted["presubmit_at"], "2026-08-26T00:00:00Z");
    assert_eq!(resubmitted["presubmit_edit_elapsed_s"], "37");
}

/// The opt-in submit wrapper is queue tooling. Keep its submissions in the
/// receipt-adoption denominator so the metric measures the workflow it names.
#[test]
fn submit_wrapper_changes_are_presubmit_eligible_queue_infrastructure() {
    for path in [
        "scripts/agent-check.sh",
        "scripts/test-agent-check-qos.sh",
        "scripts/submit-with-presubmit.sh",
        "scripts/test-submit-with-presubmit.sh",
    ] {
        let fixture = QueueFixture::stack(&[path]);

        fixture.mq_ok(&["submit", "a"], "true");
        let submitted = fixture
            .journal()
            .into_iter()
            .filter(|event| event["event"] == "submitted")
            .next_back()
            .expect("a submitted event");
        assert_eq!(submitted["presubmit_eligible"], "true", "path: {path}");
    }
}

/// A prose-only branch cannot obtain a receipt at all: `test-for-paths` reports
/// no focused checks and a docs-scope gate writes none. Warning on every
/// documentation change trains submitters to ignore the warning, which costs
/// exactly the branches where it matters.
#[test]
fn a_prose_only_submission_is_not_warned_about_a_missing_shard() {
    let fixture = QueueFixture::stack(&["a.txt"]);

    // Replace the branch's payload with a documentation-only change.
    run_git(&fixture.root, &["checkout", "--quiet", "a"]);
    fs::create_dir_all(fixture.root.join("rfcs")).expect("create rfcs dir");
    fs::write(fixture.root.join("rfcs/0001-note.md"), "prose\n")
        .expect("write prose file");
    fs::remove_file(fixture.root.join("a.txt")).ok();
    run_git(&fixture.root, &["add", "-A"]);
    run_git(&fixture.root, &["commit", "--quiet", "-m", "docs only"]);
    run_git(&fixture.root, &["checkout", "--quiet", "master"]);

    let output = fixture.mq_ok(&["submit", "a"], "true");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("no green shard recorded"),
        "prose-only submission warned about an unobtainable shard:\n{stderr}"
    );

    // The field is still journaled — suppressing the warning must not suppress
    // the measurement.
    let submitted = fixture
        .journal()
        .into_iter()
        .filter(|event| event["event"] == "submitted")
        .next_back()
        .expect("a submitted event");
    assert_eq!(submitted["presubmit_sha_match"], "false");
    assert_eq!(
        submitted["presubmit_eligible"], "false",
        "a branch with no possible focused shard must not dilute adoption"
    );
}

/// A code change with no receipt must still warn — the suppression above is
/// scoped to diffs with nothing to validate, not a blanket removal.
#[test]
fn a_code_submission_without_a_receipt_still_warns() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    let output = fixture.mq_ok(&["submit", "a"], "true");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no green shard recorded"),
        "code submission lost its missing-shard warning:\n{stderr}"
    );
    assert!(
        stderr.contains("./scripts/agent-check.sh edit")
            && stderr.contains("./scripts/submit-with-presubmit.sh' (current branch)")
            && stderr.contains("resubmit the same tip")
            && stderr.contains("queue position is preserved"),
        "warning did not give the exact-tip recovery workflow:\n{stderr}"
    );
}

#[test]
fn a_clean_linked_submission_only_runs_the_focused_shard_when_opted_in() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    let linked = fixture._temp.path().join("linked-a");
    run_git(
        &fixture.root,
        &["worktree", "add", "--quiet", linked.to_str().unwrap(), "a"],
    );

    let linked_scripts = linked.join("scripts");
    fs::create_dir_all(&linked_scripts).expect("create linked script directory");
    let source_root = workspace_root();
    fs::copy(
        source_root.join("scripts/merge-queue.sh"),
        linked_scripts.join("merge-queue.sh"),
    )
    .expect("copy merge queue script");
    fs::copy(
        source_root.join("scripts/state-paths.sh"),
        linked_scripts.join("state-paths.sh"),
    )
    .expect("copy state path helper");
    let marker = fixture._temp.path().join("automatic-edit-ran");
    let agent_check = linked_scripts.join("agent-check.sh");
    fs::write(
        &agent_check,
        format!(
            "#!/usr/bin/env bash\nset -eu\ncd \"$(dirname \"$0\")/..\"\nprintf 'ran\\n' > '{}'\nsha=$(git rev-parse HEAD)\ndir=\"$MERGE_QUEUE_STATE_DIR/receipts\"\nmkdir -p \"$dir\"\nprintf '{{\"sha\":\"%s\",\"shards\":[\"edit\"],\"durations_s\":{{\"edit\":1}},\"at\":\"2026-09-15T00:00:00Z\"}}\\n' \"$sha\" > \"$dir/$sha.json\"\n",
            marker.display()
        ),
    )
    .expect("write focused-check fixture");
    fs::set_permissions(&agent_check, fs::Permissions::from_mode(0o755))
        .expect("chmod focused-check fixture");
    run_git(&linked, &["add", "scripts"]);
    run_git(&linked, &["commit", "--quiet", "-m", "fixture focused check"]);

    let mut default_command = Command::new("bash");
    default_command
        .arg(linked_scripts.join("merge-queue.sh"))
        .args(["submit", "a"])
        .env("MERGE_QUEUE_TEST_ROOT", &fixture.root)
        .env("MERGE_QUEUE_ALLOW_TEST_ROOT", "1")
        .env("MERGE_QUEUE_STATE_DIR", &fixture.state)
        .env("MERGE_QUEUE_AUTOSTART", "0")
        .env("MERGE_QUEUE_ALLOW_MERGE", "1");
    let default_output = default_command.output().expect("run default linked submit");
    assert!(
        default_output.status.success(),
        "default linked submit failed:\n{}",
        String::from_utf8_lossy(&default_output.stderr)
    );
    assert!(
        !marker.exists(),
        "default submission unexpectedly blocked on the focused shard"
    );
    let submitted = fixture
        .journal()
        .into_iter()
        .find(|event| event["event"] == "submitted")
        .expect("default submission was journaled");
    assert_eq!(submitted["presubmit_sha_match"], "false");

    let mut opted_in_command = Command::new("bash");
    opted_in_command
        .arg(linked_scripts.join("merge-queue.sh"))
        .args(["submit", "a"])
        .env("MERGE_QUEUE_TEST_ROOT", &fixture.root)
        .env("MERGE_QUEUE_ALLOW_TEST_ROOT", "1")
        .env("MERGE_QUEUE_STATE_DIR", &fixture.state)
        .env("MERGE_QUEUE_AUTOSTART", "0")
        .env("MERGE_QUEUE_AUTO_PRESUBMIT", "1")
        .env("MERGE_QUEUE_ALLOW_MERGE", "1");
    let output = opted_in_command
        .output()
        .expect("run opted-in linked automatic submit");
    assert!(
        output.status.success(),
        "automatic linked submit failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(marker.exists(), "automatic focused shard was not invoked");
    let resubmitted = fixture
        .journal()
        .into_iter()
        .find(|event| event["event"] == "resubmitted")
        .expect("opted-in submission was journaled");
    assert_eq!(
        resubmitted["presubmit_sha_match"],
        "true",
        "automatic submit did not mint a receipt; stderr={} stdout={} marker_exists={}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
        marker.exists()
    );
    assert_eq!(resubmitted["presubmit_shard"], "edit");
    assert_eq!(resubmitted["presubmit_edit_elapsed_s"], "1");
}

#[test]
fn a_dirty_linked_submission_skips_the_automatic_focused_shard() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    let linked = fixture._temp.path().join("linked-a");
    run_git(
        &fixture.root,
        &["worktree", "add", "--quiet", linked.to_str().unwrap(), "a"],
    );
    let linked_scripts = linked.join("scripts");
    fs::create_dir_all(&linked_scripts).expect("create linked script directory");
    let source_root = workspace_root();
    fs::copy(
        source_root.join("scripts/merge-queue.sh"),
        linked_scripts.join("merge-queue.sh"),
    )
    .expect("copy merge queue script");
    fs::copy(
        source_root.join("scripts/state-paths.sh"),
        linked_scripts.join("state-paths.sh"),
    )
    .expect("copy state path helper");
    let marker = fixture._temp.path().join("automatic-edit-ran");
    let agent_check = linked_scripts.join("agent-check.sh");
    fs::write(
        &agent_check,
        format!(
            "#!/usr/bin/env bash\nprintf 'ran\\n' > '{}'\n",
            marker.display()
        ),
    )
    .expect("write focused-check fixture");
    fs::set_permissions(&agent_check, fs::Permissions::from_mode(0o755))
        .expect("chmod focused-check fixture");

    let mut command = Command::new("bash");
    command
        .arg(linked_scripts.join("merge-queue.sh"))
        .args(["submit", "a"])
        .env("MERGE_QUEUE_TEST_ROOT", &fixture.root)
        .env("MERGE_QUEUE_ALLOW_TEST_ROOT", "1")
        .env("MERGE_QUEUE_STATE_DIR", &fixture.state)
        .env("MERGE_QUEUE_AUTOSTART", "0")
        .env("MERGE_QUEUE_AUTO_PRESUBMIT", "1")
        .env("MERGE_QUEUE_ALLOW_MERGE", "1");
    let output = command.output().expect("run dirty linked automatic submit");
    assert!(output.status.success(), "dirty submit failed");
    assert!(!marker.exists(), "dirty linked checkout ran focused checks");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no green shard recorded"), "{stderr}");
    let submitted = fixture
        .journal()
        .into_iter()
        .find(|event| event["event"] == "submitted")
        .expect("dirty submission was journaled");
    assert_eq!(submitted["presubmit_sha_match"], "false");
}

/// `status` has always carried per-change `readiness`, but `doctor` -- the view
/// an operator reads -- printed queue FILENAMES. A change stuck behind a failed
/// parent therefore looked identical to one about to gate, and sat at the HEAD
/// of the listing where it read as "next up". That cost two hours of waiting on
/// a change that was never going to be picked.
#[test]
fn doctor_names_the_branch_and_says_when_one_is_blocked() {
    let fixture = QueueFixture::stack(&["a.txt", "b.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");
    fixture.mq_ok(&["submit", "--after", "a", "b"], "true");

    let doctor = fixture.mq_ok(&["doctor"], "true");
    let out = format!(
        "{}{}",
        String::from_utf8_lossy(&doctor.stdout),
        String::from_utf8_lossy(&doctor.stderr)
    );
    // Branch names, not queue filenames.
    assert!(out.contains(" a"), "doctor did not name branch a:\n{out}");
    assert!(out.contains(" b"), "doctor did not name branch b:\n{out}");
    // And the dependent must be visibly not-ready rather than silently listed.
    assert!(
        out.contains("waiting") || out.contains("BLOCKED"),
        "doctor did not report the dependent as waiting/blocked:\n{out}"
    );
    assert!(
        !out.contains(".json"),
        "doctor still prints raw queue filenames:\n{out}"
    );
}

/// The sweep is otherwise opportunistic -- it runs after a merge -- so a
/// worktree whose coordinator died before sweeping, or whose merge predates the
/// current coordinator, is orphaned forever. Measured 2026-08-27:
/// .claude/worktrees had reached 518 GB on a volume 88% full, with 18
/// landed+clean worktrees idle for 2-9 days. A full disk fails builds in ways
/// indistinguishable from a flaky gate, so catch-up is a reliability fix.
#[test]
fn the_coordinator_sweeps_landed_worktrees_at_startup() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");
    fixture.mq_ok(&["run", "--once"], "true");
    assert!(
        fixture.journal().iter().any(|e| e["event"] == "merged"),
        "fixture branch should have merged"
    );

    // A second coordinator start must sweep even with nothing newly merged:
    // the catch-up pass is what reclaims history.
    // A second coordinator start must sweep even with nothing newly merged:
    // the catch-up pass is what reclaims history.
    let out = fixture.mq(&["run", "--once"], "true");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("sweep:"),
        "coordinator startup did not run a catch-up sweep:\n{combined}"
    );
}

#[test]
fn sweep_caches_merged_history_but_keeps_queue_admission_live() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    run_git(&fixture.root, &["merge", "--ff-only", "a"]);
    let queued_worktree = fixture._temp.path().join("queued-worktree");
    run_git(
        &fixture.root,
        &["worktree", "add", queued_worktree.to_str().unwrap(), "a"],
    );
    run_git(
        &fixture.root,
        &["branch", "landed-without-worktree", "master"],
    );
    run_git(&fixture.root, &["branch", "unrelated", "master"]);

    fixture.mq_ok(&["status"], "true");
    fs::write(
        fixture.state.join("journal.jsonl"),
        concat!(
            "{\"event\":\"merged\",\"branch\":\"a\"}\n",
            "{\"event\":\"merged\",\"branch\":\"landed-without-worktree\"}\n",
        ),
    )
    .expect("write merged branch history");
    let queued = fixture.state.join("queue/queued.json");
    fs::write(&queued, "{\"branch\":\"a\"}\n").expect("write live queue entry");

    fixture.mq_ok(&["sweep"], "true");
    assert!(
        queued_worktree.exists(),
        "sweep removed a worktree whose branch is currently queued",
    );
    assert!(
        !git_output(
            &fixture.root,
            &["show-ref", "--verify", "refs/heads/landed-without-worktree"],
        )
        .status
        .success(),
        "sweep retained a journal-merged local branch with no worktree",
    );
    assert!(
        git_output(
            &fixture.root,
            &["show-ref", "--verify", "refs/heads/unrelated"],
        )
        .status
        .success(),
        "sweep removed a branch absent from merged history",
    );

    fs::remove_file(queued).expect("remove live queue entry");
    fixture.mq_ok(&["sweep"], "true");
    assert!(
        !queued_worktree.exists(),
        "sweep did not reclaim the merged worktree after its queue entry left",
    );
}

#[test]
fn sweep_recycles_a_landed_pool_worktree_in_place() {
    let fixture = QueueFixture::stack(&["a.txt", "b.txt"]);
    run_git(&fixture.root, &["merge", "--ff-only", "a"]);
    fs::write(fixture.root.join(".git/info/exclude"), "target/\nstate/\n.claude/\n")
        .expect("ignore build caches like the real checkout");
    let pool = fixture.root.join(".claude/worktrees");
    let landed = pool.join("pool-00");
    let dirty = pool.join("pool-01");
    run_git(&fixture.root, &["worktree", "add", landed.to_str().unwrap(), "a"]);
    run_git(&fixture.root, &["branch", "c", "master"]);
    run_git(&fixture.root, &["worktree", "add", dirty.to_str().unwrap(), "c"]);
    let state = landed.join("target/debug/incremental/witchy_types-fixture/s-1");
    fs::create_dir_all(state.parent().unwrap()).expect("create slot target");
    fs::write(&state, "incremental\n").expect("write incremental state");
    fs::write(dirty.join("stray.txt"), "unfinished\n").expect("dirty the second slot");
    // Landed after the gate resolved a conflict: its commit is not
    // patch-equivalent to anything on master, but it is still exactly the
    // submitted tip the journal says merged. One more commit makes the
    // fourth slot's tip newer than what landed, so it must be kept.
    let resolved = pool.join("pool-02");
    let advanced = pool.join("pool-03");
    for (slot, branch) in [(&resolved, "d"), (&advanced, "e")] {
        run_git(&fixture.root, &["branch", branch, "master"]);
        run_git(&fixture.root, &["worktree", "add", slot.to_str().unwrap(), branch]);
        fs::write(slot.join("conflicted.txt"), "resolved differently on master\n").expect("write");
        run_git(slot, &["add", "conflicted.txt"]);
        run_git(slot, &["commit", "-m", "landed with a conflict resolution"]);
    }
    let landed_tip = |slot: &std::path::Path| {
        String::from_utf8_lossy(&git_output(slot, &["rev-parse", "HEAD"]).stdout).trim().to_owned()
    };
    let (resolved_tip, advanced_tip) = (landed_tip(&resolved), landed_tip(&advanced));
    fs::write(advanced.join("later.txt"), "after submission\n").expect("write");
    run_git(&advanced, &["add", "later.txt"]);
    run_git(&advanced, &["commit", "-m", "work after the landed submission"]);

    fixture.mq_ok(&["status"], "true");
    fs::write(
        fixture.state.join("journal.jsonl"),
        format!(
            "{{\"event\":\"merged\",\"branch\":\"a\"}}\n{{\"event\":\"merged\",\"branch\":\"c\"}}\n\
             {{\"event\":\"merged\",\"branch\":\"d\",\"submitted_sha\":\"{resolved_tip}\"}}\n\
             {{\"event\":\"merged\",\"branch\":\"e\",\"submitted_sha\":\"{advanced_tip}\"}}\n"
        ),
    )
    .expect("write merged branch history");
    // Ready work defers the full sweep; a busy queue is when agents claim
    // slots, so recycling must not wait for it to drain.
    fixture.mq_ok(&["submit", "b"], "true");
    let witchy_state = fixture._temp.path().join("witchy-state");
    let output = fixture
        .mq_command(&["sweep"], "true")
        .env("WITCHY_STATE_DIR", &witchy_state)
        .output()
        .expect("run sweep");
    assert!(output.status.success(), "sweep failed: {}", String::from_utf8_lossy(&output.stderr));

    assert!(landed.is_dir(), "sweep deleted a landed pool worktree instead of recycling it");
    assert!(state.is_file(), "recycling discarded the slot's incremental state");
    let head = git_output(&landed, &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert_eq!(String::from_utf8_lossy(&head.stdout).trim(), "HEAD", "recycled slot is not detached");
    assert!(witchy_state.join("worktree-pool/free/00").is_file(), "recycled slot was not marked free");
    assert!(
        !git_output(&fixture.root, &["show-ref", "--verify", "refs/heads/a"]).status.success(),
        "sweep kept the landed branch ref of a recycled slot",
    );
    let journal = fs::read_to_string(fixture.state.join("journal.jsonl")).expect("read journal");
    assert!(
        journal.lines().any(|line| line.contains("\"swept\"") && line.contains("\"recycled\"")),
        "recycle was not journaled as a sweep: {journal}",
    );

    assert!(dirty.join("stray.txt").is_file(), "sweep touched a dirty pool worktree");
    let dirty_head = git_output(&dirty, &["branch", "--show-current"]);
    assert_eq!(String::from_utf8_lossy(&dirty_head.stdout).trim(), "c");
    assert!(!witchy_state.join("worktree-pool/free/01").exists(), "a dirty slot was marked free");
    assert!(
        witchy_state.join("worktree-pool/free/02").is_file(),
        "a slot still at its landed submission was not recycled",
    );
    assert!(
        !witchy_state.join("worktree-pool/free/03").exists(),
        "a slot with work after its landed submission was recycled",
    );
}

#[test]
fn sweep_defers_branch_pruning_while_ready_work_waits() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fixture.mq_ok(&["status"], "true");
    run_git(
        &fixture.root,
        &["branch", "landed-without-worktree", "master"],
    );
    fs::write(
        fixture.state.join("journal.jsonl"),
        "{\"event\":\"merged\",\"branch\":\"landed-without-worktree\"}\n",
    )
    .expect("write merged branch history");
    fixture.mq_ok(&["submit", "a"], "true");

    let deferred = fixture.mq_ok(&["sweep"], "true");
    let deferred_output = format!(
        "{}{}",
        String::from_utf8_lossy(&deferred.stdout),
        String::from_utf8_lossy(&deferred.stderr),
    );
    assert!(
        deferred_output.contains("ready queue work is pending; deferring cleanup"),
        "sweep did not yield to ready admission: {deferred_output}",
    );
    assert!(
        git_output(
            &fixture.root,
            &["show-ref", "--verify", "refs/heads/landed-without-worktree"],
        )
        .status
        .success(),
        "sweep pruned history while ready work was waiting",
    );

    fixture.mq_ok(&["drop", "a", "fixture cleanup"], "true");
    fixture.mq_ok(&["sweep"], "true");
    assert!(
        !git_output(
            &fixture.root,
            &["show-ref", "--verify", "refs/heads/landed-without-worktree"],
        )
        .status
        .success(),
        "deferred branch cleanup did not resume after admission drained",
    );
}

#[test]
fn sweep_batches_historical_branch_pruning() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fixture.mq_ok(&["status"], "true");
    let mut journal = String::new();
    for index in 0..33 {
        let branch = format!("landed-batch-{index:02}");
        run_git(&fixture.root, &["branch", &branch, "master"]);
        journal.push_str(&format!(
            "{{\"event\":\"merged\",\"branch\":\"{branch}\"}}\n"
        ));
    }
    fs::write(fixture.state.join("journal.jsonl"), journal)
        .expect("write merged branch history");

    let output = fixture.mq_ok(&["sweep"], "true");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        combined.contains("checked 33 historical branch candidate(s) in 3 batch(es)"),
        "sweep did not prune in bounded batches: {combined}",
    );
    for index in 0..33 {
        assert!(
            !git_output(
                &fixture.root,
                &[
                    "show-ref",
                    "--verify",
                    &format!("refs/heads/landed-batch-{index:02}"),
                ],
            )
            .status
            .success(),
            "sweep retained landed-batch-{index:02}",
        );
    }
}

#[test]
fn sweep_skips_only_unchanged_superseded_refs_already_swept() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fixture.mq_ok(&["status"], "true");
    let submitted = git(&fixture.root, &["rev-parse", "a"]);
    let landed = git(&fixture.root, &["rev-parse", "master"]);
    assert_ne!(submitted, landed, "fixture needs a rebased-style original tip");
    for branch in ["unchanged", "advanced", "resubmitted"] {
        run_git(&fixture.root, &["branch", branch, &submitted]);
    }
    run_git(&fixture.root, &["checkout", "advanced"]);
    fs::write(fixture.root.join("advanced.txt"), "advanced\n").expect("advance branch");
    run_git(&fixture.root, &["add", "advanced.txt"]);
    run_git(&fixture.root, &["commit", "-m", "advance branch"]);
    run_git(&fixture.root, &["checkout", "master"]);

    let mut journal = String::new();
    for branch in ["unchanged", "advanced", "resubmitted"] {
        journal.push_str(&format!(
            "{{\"event\":\"submitted\",\"branch\":\"{branch}\"}}\n\
             {{\"event\":\"merged\",\"branch\":\"{branch}\",\"submitted_sha\":\"{submitted}\",\"sha\":\"{landed}\"}}\n\
             {{\"event\":\"swept\",\"branch\":\"{branch}\"}}\n"
        ));
    }
    journal.push_str("{\"event\":\"submitted\",\"branch\":\"resubmitted\"}\n");
    fs::write(fixture.state.join("journal.jsonl"), journal)
        .expect("write branch lifecycles");

    let output = fixture.mq_ok(&["sweep"], "true");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        combined.contains("skipped 1 unchanged superseded branch ref(s) already swept"),
        "sweep did not skip only the exact unchanged lifecycle: {combined}",
    );
    assert!(
        combined.contains("checked 2 historical branch candidate(s) in 1 batch(es)"),
        "advanced and resubmitted refs did not keep safe deletion checks: {combined}",
    );
    for branch in ["unchanged", "advanced", "resubmitted"] {
        assert!(
            git_output(
                &fixture.root,
                &["show-ref", "--verify", &format!("refs/heads/{branch}")],
            )
            .status
            .success(),
            "sweep unexpectedly deleted unmerged branch {branch}",
        );
    }
}

#[test]
fn sweep_yields_when_ready_work_arrives_during_branch_pruning() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fixture.mq_ok(&["status"], "true");
    let mut journal = String::new();
    for index in 0..32 {
        let branch = format!("landed-{index:02}");
        run_git(&fixture.root, &["branch", &branch, "master"]);
        journal.push_str(&format!(
            "{{\"event\":\"merged\",\"branch\":\"{branch}\"}}\n"
        ));
    }
    fs::write(fixture.state.join("journal.jsonl"), journal)
        .expect("write merged branch history");
    let marker = fixture._temp.path().join("branch-pruning-started");
    let sweep = fixture
        .mq_command(&["sweep"], "true")
        .env("MERGE_QUEUE_TEST_SWEEP_BRANCH_MARKER", &marker)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start concurrent sweep");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !marker.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(marker.exists(), "sweep did not enter historical branch pruning");
    fixture.mq_ok(&["submit", "a"], "true");
    fs::write(format!("{}.continue", marker.display()), "continue\n")
        .expect("release branch pruning fixture");
    let output = sweep.wait_with_output().expect("wait for concurrent sweep");
    assert!(output.status.success(), "concurrent sweep failed");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        combined.contains("ready queue work arrived; deferring"),
        "sweep did not yield after admission changed: {combined}",
    );
    let retained = (0..32)
        .filter(|index| {
            git_output(
                &fixture.root,
                &[
                    "show-ref",
                    "--verify",
                    &format!("refs/heads/landed-{index:02}"),
                ],
            )
            .status
            .success()
        })
        .count();
    assert!(retained > 0, "sweep finished every branch before yielding");
}

#[test]
fn ready_queue_items_run_before_post_merge_worktree_sweep() {
    let fixture = QueueFixture::stack(&["docs/a.md", "src/b.rs"]);
    let a_worktree = fixture._temp.path().join("worktree-a");
    let b_worktree = fixture._temp.path().join("worktree-b");
    run_git(
        &fixture.root,
        &["worktree", "add", a_worktree.to_str().unwrap(), "a"],
    );
    run_git(
        &fixture.root,
        &["worktree", "add", b_worktree.to_str().unwrap(), "b"],
    );
    fixture.mq_ok(&["submit", "a"], "true");
    fixture.mq_ok(&["submit", "b"], "true");
    fixture.mq_ok(&["run", "--once"], "true");

    let journal = fixture.journal();
    let merged_b = journal
        .iter()
        .position(|event| event["event"] == "merged" && event["branch"] == "b")
        .expect("second ready branch merged");
    let first_sweep = journal
        .iter()
        .position(|event| event["event"] == "swept")
        .expect("deferred reclamation ran after the queue drained");
    assert!(
        merged_b < first_sweep,
        "worktree sweep blocked a ready queue item: {journal:?}"
    );
    assert!(!a_worktree.exists(), "first landed worktree was not reclaimed");
    assert!(!b_worktree.exists(), "second landed worktree was not reclaimed");
}

/// A branch that keeps failing the same test used to pay full test-execution
/// cost on every attempt to rediscover it -- RFC-0158 did that eight times,
/// with the whole serialized queue stuck behind it. Retries do not get cheaper
/// either: measured over 173 paired attempts, TRY2/TRY1 duration is p50 1.00.
///
/// So a red must leave behind the tests that failed, and the next attempt must
/// replay them first.
#[test]
fn a_red_remembers_its_failing_tests_for_the_next_attempt() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    // A gate that fails on nextest's retry line, followed by its generic final
    // error. Triage and replay must retain the specific test rather than the
    // less useful trailing error.
    let gate = "printf '==> [1] tests (workspace) (t+0s)\\n   TRY 2 FAIL [   1.0s] ( 1/10) witchy::misc some_module::the_broken_test\\nerror: test run failed\\n'; exit 1";
    fixture.mq_ok(&["submit", "a"], gate);
    let out = fixture.mq(&["run", "--once"], gate);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let journal = fixture.journal();
    let red = journal
        .iter()
        .find(|event| event["event"] == "red")
        .unwrap_or_else(|| panic!("expected a red:\n{combined}"));
    assert!(
        red["failure_line"]
            .as_str()
            .is_some_and(|line| line.contains("the_broken_test")),
        "journal replaced the specific retry failure with a generic error: {red}"
    );

    let recorded = fs::read_dir(fixture.state.join("changes"))
        .expect("read changes")
        .filter_map(Result::ok)
        .filter_map(|e| fs::read_to_string(e.path()).ok())
        .find(|s| s.contains("preflight"))
        .expect("a change recorded a preflight expression");
    assert!(
        recorded.contains("the_broken_test"),
        "preflight did not name the failing test: {recorded}"
    );
    assert!(
        recorded.contains("test(/^"),
        "preflight is not a nextest filter expression: {recorded}"
    );
    let recorded_json: serde_json::Value =
        serde_json::from_str(&recorded).expect("change record is JSON");
    assert_eq!(
        recorded_json["preflight_nextest"],
        "-p witchy --test misc",
        "preflight did not retain the single failing Cargo target: {recorded}",
    );

    fixture.mq_ok(&["submit", "a"], gate);
    let resubmitted = fixture.change("a");
    assert!(
        resubmitted["preflight"]
            .as_str()
            .is_some_and(|expr| expr.contains("the_broken_test")),
        "resubmission discarded the focused replay: {resubmitted}"
    );
    assert_eq!(resubmitted["preflight_nextest"], "-p witchy --test misc");
}

#[test]
fn mixed_failure_targets_keep_the_broad_preflight_build_scope() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    let gate = "printf 'FAIL [ 0.1s] ( 1/2) witchy-lower::codegen tests::first_red\nFAIL [ 0.1s] ( 2/2) witchy::misc tests::second_red\n'; exit 1";
    fixture.mq_ok(&["submit", "a"], gate);
    fixture.mq_ok(&["run", "--once"], gate);

    let change = fixture.change("a");
    assert!(
        change["preflight"]
            .as_str()
            .is_some_and(|expr| expr.contains("first_red") && expr.contains("second_red")),
        "both failures must remain in the replay expression: {change}",
    );
    assert!(
        change.get("preflight_nextest").is_none(),
        "mixed Cargo targets must not narrow the next preflight: {change}",
    );
}

#[test]
fn a_linker_failure_during_test_build_is_classified_as_compile() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    let gate = "printf '==> [1] tests (workspace) (t+0s)\\nerror: linking with `cc` failed: exit status: 1\\n'; exit 1";
    fixture.mq_ok(&["submit", "a"], gate);
    let out = fixture.mq(&["run", "--once"], gate);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let journal = fixture.journal();
    let red = journal
        .iter()
        .find(|event| event["event"] == "red")
        .unwrap_or_else(|| panic!("expected a red:\n{combined}"));

    assert_eq!(red["failing_stage"], "tests (workspace)");
    assert_eq!(red["red_class"], "compile");
    assert!(
        red["failure_line"]
            .as_str()
            .is_some_and(|line| line.contains("linking with `cc` failed")),
        "journal lost the linker diagnostic: {red}"
    );
}

#[test]
fn a_replayed_preflight_test_failure_is_classified_as_test() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    let first_gate = "printf '==> [1] tests (workspace) (t+0s)\\n   TRY 1 FAIL [ 0.8s] ( 1/1) witchy::misc some_module::the_broken_test\\nerror: test run failed\\n'; exit 1";
    fixture.mq_ok(&["submit", "a"], first_gate);
    fixture.mq_ok(&["run", "--once"], first_gate);

    let preflight_gate = "printf '==> [1] preflight (tests that red the last attempt) (t+0s)\\nTRY 1 FAIL [ 0.8s] ( 1/1) witchy::cli_subcommands fetch_root_requires_an_explicit_valid_origin_for_source_and_wasm\\n'; exit 1";
    fixture.mq_ok(&["submit", "a"], preflight_gate);
    let out = fixture.mq_ok(&["run", "--once"], preflight_gate);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let journal = fixture.journal();
    let red = journal
        .iter()
        .filter(|event| event["event"] == "red")
        .last()
        .unwrap_or_else(|| panic!("expected a red:\n{combined}"));

    assert_eq!(red["failing_stage"], "preflight (tests that red the last attempt)");
    assert_eq!(red["red_class"], "test");
    assert!(red["failure_line"].as_str().is_some_and(|line| line.contains("fetch_root_requires_an_explicit_valid_origin")));
}

/// One agent's ad-hoc `with-lock` suite blocks that agent; the coordinator
/// blocks EVERY queued branch. The gate lock had no fairness, so measured
/// 2026-08-27 the coordinator's lock wait went p50 0s -> 75s and p90 221s ->
/// 763s once several agents ran benchmarks under with-lock, and the queue
/// stopped draining. with-lock now yields to a waiting coordinator -- but must
/// fail OPEN, or a dead coordinator would wedge every agent.
#[test]
fn with_lock_fails_open_when_the_coordinator_intent_is_stale() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    // The state dir is created lazily on first use.
    fixture.mq_ok(&["status"], "true");
    let intent = fixture.state.join("gate.lock.intent");
    // A pid that cannot be alive.
    fs::write(&intent, "999999\n").expect("write stale intent");

    let out = fixture.mq(&["with-lock", "--", "true"], "true");
    assert!(
        out.status.success(),
        "with-lock wedged on a stale coordinator intent:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !intent.exists(),
        "a stale intent marker was left behind to block the next caller"
    );
}

/// BUG-632. The fail-open above was bounded by a per-CALLER deadline
/// (`MERGE_QUEUE_INTENT_MAX_WAIT`, 900s) while the intent's lifetime is the
/// coordinator's ENTIRE wait. So each arriving with-lock caller waited its own
/// cap, failed open, and took the lock, putting the coordinator behind a fresh
/// holder every time -- one caller per cap starved it forever. Observed
/// 2026-08-27: 100 minutes with zero merges while the queue grew 10 -> 18.
///
/// The bound is now LIVENESS, not a deadline: an intent whose owner is alive but
/// has stopped refreshing it is abandoned, and only THAT fails open. A pid that
/// is alive is not sufficient -- this test would pass under the old code for the
/// wrong reason if it only checked liveness, so it backdates a LIVE pid's
/// marker, which the old code would have waited 900s on.
#[test]
fn with_lock_fails_open_on_a_live_pid_whose_intent_stopped_refreshing() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fixture.mq_ok(&["status"], "true");
    let intent = fixture.state.join("gate.lock.intent");

    // OUR pid: unambiguously alive for the whole test.
    fs::write(&intent, format!("{}\n", std::process::id())).expect("write intent");
    // Backdate it well past the staleness horizon. `touch -A` adjusts the
    // existing timestamp, which avoids formatting an absolute stamp here.
    let touched = std::process::Command::new("touch")
        .args(["-A", "-001000", intent.to_str().expect("intent path")])
        .status()
        .expect("run touch");
    assert!(touched.success(), "could not backdate the intent marker");

    let started = std::time::Instant::now();
    let out = fixture.mq(&["with-lock", "--", "true"], "true");
    assert!(
        out.status.success(),
        "with-lock wedged on an abandoned intent:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // The point of the fix: it must not serve a per-caller sentence first.
    assert!(
        started.elapsed() < std::time::Duration::from_secs(60),
        "with-lock waited {:?} on an abandoned intent; it must fail open promptly",
        started.elapsed()
    );
}

/// Fake gate: fails with a stale-incremental link error (`.llvm.<n>` symbol)
/// while `$LINK_FAILURES` runs remain, then passes. Each run plants a
/// workspace crate's incremental cache so the purge can be observed.
const STALE_LINK_GATE: &str = "\
inc=\"$PWD/$CARGO_TARGET_DIR/debug/incremental\"
mkdir -p \"$inc/witchy_lower-0123456789abcdef\"
printf '%s' \"$inc\" > \"$INCREMENTAL_PROBE\"
runs=$(cat \"$RUN_COUNT\" 2>/dev/null || echo 0)
runs=$((runs + 1)); printf '%s' \"$runs\" > \"$RUN_COUNT\"
printf '==> [1] tests (workspace) (t+0s)\\n'
if [ \"$runs\" -le \"$LINK_FAILURES\" ]; then
  printf 'error: linking with `cc` failed: exit status: 1\\n'
  printf '  = note: Undefined symbols for architecture arm64:\\n'
  printf '            \"__RINvNtCs0_4core3ptr9drop_glue.llvm.13568411306068324051\", referenced from:\\n'
  printf '          ld: symbol(s) not found for architecture arm64\\n'
  exit 1
fi
exit 0
";

fn run_stale_link_gate(fixture: &QueueFixture, link_failures: &str, runs: usize) {
    for _ in 0..runs {
        let _ = fixture
            .mq_command(&["run", "--once"], STALE_LINK_GATE)
            .env("LINK_FAILURES", link_failures)
            .env("RUN_COUNT", fixture.state.join("run-count"))
            .env("INCREMENTAL_PROBE", fixture.state.join("incremental-probe"))
            .output()
            .expect("run the stale-link gate");
    }
}

/// A `.llvm.*` link failure is the incremental cache's fault, not the
/// candidate's: the coordinator purges the workspace crates' incremental
/// state and re-gates the same commit once, which then lands.
#[test]
fn a_stale_incremental_link_failure_is_purged_and_regated_once() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");
    run_stale_link_gate(&fixture, "1", 2);

    let journal = fixture.journal();
    assert!(
        journal.iter().any(|event| event["event"] == "requeued"
            && event["reason"] == "stale incremental link state purged"),
        "the stale link failure was not requeued: {journal:?}",
    );
    assert!(
        journal.iter().any(|event| event["event"] == "merged" && event["branch"] == "a"),
        "the re-gated commit did not land: {journal:?}",
    );
    assert!(journal.iter().all(|event| event["event"] != "red"), "a stale link failure went red: {journal:?}");
}

/// The retry is once per commit: a link failure that persists after the
/// purge is a real one and goes red.
#[test]
fn a_persistent_link_failure_goes_red_after_one_purge() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");
    run_stale_link_gate(&fixture, "99", 3);

    let journal = fixture.journal();
    let requeues = journal.iter()
        .filter(|event| event["event"] == "requeued" && event["reason"] == "stale incremental link state purged")
        .count();
    assert_eq!(requeues, 1, "expected exactly one purge-and-retry: {journal:?}");
    assert!(
        journal.iter().any(|event| event["event"] == "red" && event["branch"] == "a"),
        "a persistent link failure never went red: {journal:?}",
    );
    let incremental = std::path::PathBuf::from(
        std::fs::read_to_string(fixture.state.join("incremental-probe")).expect("gate reported its incremental dir"),
    );
    assert!(incremental.exists(), "the gate target's incremental dir itself must survive");
}
