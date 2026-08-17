use super::*;

#[test]
fn zero_whole_gate_timeout_does_not_kill_a_progressing_gate() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");

    let output = fixture
        .mq_command(&["run", "--once"], "printf 'gate started\\n'; sleep 2")
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
fn workspace_test_budget_kills_a_chatty_progressing_stage() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");

    let gate = "\
printf '==> [1] tests (workspace) (t+0s)\\n'
i=0
while :; do
  i=$((i+1))
  printf '        PASS [  0.010s] (%d/9999) witchy-types types::keeps_printing\\n' \"$i\"
  printf '[1] tests (workspace) still running (heartbeat %d)\\n' \"$i\"
  sleep 0.2
done
";

    let started = Instant::now();
    let output = fixture
        .mq_command(&["run", "--once"], gate)
        .env("MERGE_QUEUE_WORKSPACE_TEST_BUDGET", "1")
        .env("MERGE_QUEUE_STALL_TIMEOUT", "600")
        .env("MERGE_QUEUE_BUSY_SILENCE_MAX", "1800")
        .output()
        .expect("run gate past the workspace-test budget");
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(20),
        "workspace-test budget did not fire promptly: {elapsed:?}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
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
    assert!(
        fixture
            .journal()
            .iter()
            .all(|event| event["event"] != "merged"),
        "over-budget stage was still merged",
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
fn zero_workspace_test_budget_does_not_kill_a_progressing_test_stage() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");

    let output = fixture
        .mq_command(
            &["run", "--once"],
            "printf '==> [1] tests (workspace) (t+0s)\\n'; sleep 2; printf '    [1] tests (workspace) took 2s\\n'",
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
fn docs_only_master_move_does_not_regate_a_green_code_candidate() {
    let fixture = QueueFixture::stack(&["code.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");

    let started = fixture._temp.path().join("gate-started");
    let env_log = fixture._temp.path().join("gate-env");
    let gate = format!(
        "printf 'scope=%s skip_book=%s nextest=[%s]\\n' \
         \"${{WITCHY_GATE_SCOPE:-}}\" \"${{WITCHY_GATE_SKIP_BOOK:-}}\" \"${{WITCHY_GATE_NEXTEST:-}}\" >{env}\n\
         printf started >{started}\n\
         sleep 2\n\
         exit 0\n",
        env = env_log.display(),
        started = started.display(),
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
        "printf 'scope=%s skip_book=%s nextest=[%s] expr=[%s]\\n' \
         \"${{WITCHY_GATE_SCOPE:-}}\" \"${{WITCHY_GATE_SKIP_BOOK:-}}\" \
         \"${{WITCHY_GATE_NEXTEST:-}}\" \"${{WITCHY_GATE_NEXTEST_EXPR:-}}\" >{}",
        env_log.display(),
    );
    fixture.mq_ok(&["run", "--once"], &gate);
    let env = fs::read_to_string(&env_log).expect("read classified gate env");
    assert!(
        env.contains("scope=all"),
        "types-only batch was not a product gate: {env}",
    );
    assert!(
        env.contains("skip_book=1"),
        "types-only batch did not skip the book validator: {env}",
    );
    assert!(
        env.contains("nextest=[-p witchy-types") && env.contains("expr=[package(witchy-types)"),
        "types-only batch did not select the crate/example mapping: {env}",
    );
    assert!(
        !env.contains("nextest=[--workspace") && !env.contains("nextest=[]"),
        "types-only batch launched an unfiltered --workspace nextest: {env}",
    );
}

#[test]
fn example_tests_only_batch_classifies_focused_nextest_and_skips_heavy_legs() {
    let fixture = QueueFixture::stack(&["src/example_tests/rfc0122_wasm_list_carrier.rs"]);
    fixture.mq_ok(&["submit", "a"], "true");
    let env_log = fixture._temp.path().join("gate-env");
    let gate = format!(
        "printf 'scope=%s skip_book=%s skip_rust=%s skip_compile=%s skip_wasm=%s nextest=[%s] expr=[%s]\\n' \
         \"${{WITCHY_GATE_SCOPE:-}}\" \"${{WITCHY_GATE_SKIP_BOOK:-}}\" \
         \"${{WITCHY_GATE_SKIP_RUST_CLASS:-}}\" \"${{WITCHY_GATE_SKIP_COMPILE:-}}\" \
         \"${{WITCHY_GATE_SKIP_WASM:-}}\" \"${{WITCHY_GATE_NEXTEST:-}}\" \
         \"${{WITCHY_GATE_NEXTEST_EXPR:-}}\" >{}",
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
            && env.contains("skip_wasm=1"),
        "example_tests-only batch did not skip book/rust-class/compile/wasm: {env}",
    );
    assert!(
        env.contains("nextest=[-p witchy") && env.contains("expr=[test(/^example_tests::/)"),
        "example_tests-only batch did not select the example_tests area: {env}",
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
}

#[test]
fn crate_plus_example_tests_batch_keeps_compile_legs_and_unions_nextest() {
    let fixture = QueueFixture::stack(&["crates/witchy-interp/src/lib.rs"]);
    run_git(&fixture.root, &["switch", "a"]);
    fs::create_dir_all(fixture.root.join("src/example_tests")).expect("create example_tests dir");
    fs::write(
        fixture.root.join("src/example_tests/rfc0122_wasm_list_carrier.rs"),
        "examples\n",
    )
    .expect("write example_tests file");
    run_git(&fixture.root, &["add", "src/example_tests/rfc0122_wasm_list_carrier.rs"]);
    run_git(&fixture.root, &["commit", "-m", "add example_tests"]);
    run_git(&fixture.root, &["switch", "master"]);
    fixture.mq_ok(&["submit", "a"], "true");
    let env_log = fixture._temp.path().join("gate-env");
    let gate = format!(
        "printf 'skip_rust=%s skip_compile=%s skip_wasm=%s nextest=[%s] expr=[%s]\\n' \
         \"${{WITCHY_GATE_SKIP_RUST_CLASS:-}}\" \"${{WITCHY_GATE_SKIP_COMPILE:-}}\" \
         \"${{WITCHY_GATE_SKIP_WASM:-}}\" \"${{WITCHY_GATE_NEXTEST:-}}\" \
         \"${{WITCHY_GATE_NEXTEST_EXPR:-}}\" >{}",
        env_log.display(),
    );
    fixture.mq_ok(&["run", "--once"], &gate);
    let env = fs::read_to_string(&env_log).expect("read classified gate env");
    assert!(
        env.contains("skip_rust=0") && env.contains("skip_compile=0") && env.contains("skip_wasm=0"),
        "compiler crate batch skipped compile/rust-class/wasm: {env}",
    );
    assert!(
        env.contains("-p witchy-interp") && env.contains("example_tests"),
        "crate+example_tests did not union the mapped crate with example_tests: {env}",
    );
    assert!(
        !env.contains("nextest=[--workspace") && !env.contains("nextest=[]"),
        "crate+example_tests launched an unfiltered --workspace nextest: {env}",
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
    fs::write(&proceed, "go\n").expect("release paused gate");

    let output = runner.wait_with_output().expect("wait for queue drain");
    assert!(
        output.status.success(),
        "queue run failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(run_git(&fixture.root, &["show", "master:a2.txt"]).status.success());
    assert_eq!(fixture.change("a")["state"], "merged");
    let merged = fixture
        .journal()
        .into_iter()
        .filter(|event| event["event"] == "merged" && event["branch"] == "a")
        .count();
    assert_eq!(merged, 2, "updated SHA did not receive its own gate attempt");
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

    fixture.mq_ok(&["run", "--once"], "false");
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

    fixture.mq_ok(&["run", "--once"], "false");
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
    let fixture = QueueFixture::stack(&[
        "a.md", "b.md", "c.md", "d.md", "e.md", "f.md",
    ]);
    submit_stack(&fixture, &["a", "b", "c", "d", "e", "f"]);
    let output = fixture
        .mq_command(&["run", "--once"], "true")
        .env("MERGE_QUEUE_BATCH_MAX", "5")
        .env("MERGE_QUEUE_DOCS_BATCH_MAX", "20")
        .output()
        .expect("run documentation batch");
    assert!(
        output.status.success(),
        "documentation batch failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(run_git(&fixture.root, &["show", "master:f.md"]).status.success());
    let merged: Vec<_> = fixture
        .journal()
        .into_iter()
        .filter(|event| event["event"] == "merged")
        .collect();
    assert_eq!(merged.len(), 6);
    assert!(merged.iter().all(|event| event["batch"] == "6"));
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
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
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
fn concurrent_daemon_starts_create_exactly_one_coordinator() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp = TempDir::new();
    let state = temp.path().join("state");
    let gate_worktree = temp.path().join("unused-gate-worktree");

    let mut starters = Vec::new();
    for _ in 0..8 {
        starters.push(
            Command::new(root.join("scripts/merge-queue.sh"))
                .arg("daemon")
                .env("MERGE_QUEUE_STATE_DIR", &state)
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

