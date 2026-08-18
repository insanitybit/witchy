use super::*;

fn fixture_with_gate_worktree() -> QueueFixture {
    let fixture = QueueFixture::stack(&["a.txt"]);
    run_git(
        &fixture.root,
        &[
            "worktree",
            "add",
            "--detach",
            fixture.gate_worktree.to_str().unwrap(),
            "master",
        ],
    );
    fixture
}

fn wait_for_branch_to_land(fixture: &QueueFixture, context: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !git_output(&fixture.root, &["show", "master:a.txt"])
        .status
        .success()
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        git_output(&fixture.root, &["show", "master:a.txt"])
            .status
            .success(),
        "{context}",
    );
}

fn stop_coordinator(
    mut coordinator: std::process::Child,
    guard: ProcessGroupGuard,
    context: &str,
) {
    drop(guard);
    let deadline = Instant::now() + Duration::from_secs(5);
    while coordinator
        .try_wait()
        .expect("poll terminated coordinator")
        .is_none()
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        coordinator
            .try_wait()
            .expect("reap terminated coordinator")
            .is_some(),
        "{context}",
    );
}

#[test]
fn idle_prewarm_wraps_every_workload_in_one_cancellable_utility_group() {
    let fixture = fixture_with_gate_worktree();

    let bin = fixture._temp.path().join("priority-prewarm-bin");
    fs::create_dir(&bin).expect("create fake priority bin");
    let workload_log = fixture._temp.path().join("priority-prewarm-workloads");
    let started = fixture._temp.path().join("priority-prewarm-started");
    let cancelled = fixture._temp.path().join("priority-prewarm-cancelled");
    let gate_ran = fixture._temp.path().join("priority-gate-ran");
    let gate_release = fixture._temp.path().join("priority-gate-release");
    let coordinator_log = fixture._temp.path().join("priority-coordinator.log");

    let taskpolicy = bin.join("taskpolicy");
    fs::write(
        &taskpolicy,
        r#"#!/bin/sh
pgid="$(perl -e 'print getpgrp(0)')"
printf 'taskpolicy|%s|%s|%s %s\n' "$$" "$pgid" "$1" "$2" >>"__LOG__"
[ "$1" = -c ] && [ "$2" = utility ] || exit 92
export WITCHY_PREWARM_QOS_FIXTURE=utility
shift 2
exec "$@"
"#
        .replace("__LOG__", &workload_log.display().to_string()),
    )
    .expect("write fake taskpolicy");
    fs::set_permissions(&taskpolicy, fs::Permissions::from_mode(0o755))
        .expect("chmod fake taskpolicy");

    let rustup = bin.join("rustup");
    fs::write(
        &rustup,
        r#"#!/bin/sh
pgid="$(perl -e 'print getpgrp(0)')"
printf 'rustup|%s|%s|%s\n' "${WITCHY_PREWARM_QOS_FIXTURE:-normal}" "$pgid" "$*" >>"__LOG__"
case "$1" in
  target) exit 0 ;;
  which) printf '__BIN__/rustc\n'; exit 0 ;;
esac
exit 1
"#
        .replace("__LOG__", &workload_log.display().to_string())
        .replace("__BIN__", &bin.display().to_string()),
    )
    .expect("write fake priority rustup");
    fs::set_permissions(&rustup, fs::Permissions::from_mode(0o755))
        .expect("chmod fake priority rustup");

    let cargo = bin.join("cargo");
    fs::write(
        &cargo,
        r#"#!/bin/sh
pgid="$(perl -e 'print getpgrp(0)')"
printf 'cargo|%s|%s|%s|%s\n' "${WITCHY_PREWARM_QOS_FIXTURE:-normal}" "$pgid" "${CARGO_TARGET_DIR:-unset}" "$*" >>"__LOG__"
exit 0
"#
        .replace("__LOG__", &workload_log.display().to_string()),
    )
    .expect("write fake priority cargo");
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755))
        .expect("chmod fake priority cargo");

    let cache_warmer = fixture.gate_worktree.join("scripts/warm-witchy-caches.sh");
    fs::create_dir_all(cache_warmer.parent().unwrap()).expect("create cache warmer directory");
    fs::write(
        &cache_warmer,
        r#"#!/bin/sh
pgid="$(perl -e 'print getpgrp(0)')"
printf 'cache|%s|%s|warm-witchy-caches\n' "${WITCHY_PREWARM_QOS_FIXTURE:-normal}" "$pgid" >>"__LOG__"
trap 'printf cancelled >"__CANCELLED__"; exit 143' TERM INT
printf started >"__STARTED__"
while :; do /bin/sleep 1; done
"#
        .replace("__LOG__", &workload_log.display().to_string())
        .replace("__CANCELLED__", &cancelled.display().to_string())
        .replace("__STARTED__", &started.display().to_string()),
    )
    .expect("write blocking cache warmer");
    fs::set_permissions(&cache_warmer, fs::Permissions::from_mode(0o755))
        .expect("chmod blocking cache warmer");

    let queue_script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/merge-queue.sh");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let gate = format!(
        "printf ran >{}; while [ ! -e {} ]; do /bin/sleep 0.05; done",
        gate_ran.display(),
        gate_release.display(),
    );
    let coordinator = Command::new("bash")
        .arg(&queue_script)
        .arg("run")
        .env("PATH", path)
        .env("MERGE_QUEUE_TEST_ROOT", &fixture.root)
        .env("MERGE_QUEUE_ALLOW_TEST_ROOT", "1")
        .env("MERGE_QUEUE_STATE_DIR", &fixture.state)
        .env("MERGE_QUEUE_GATE_WT", &fixture.gate_worktree)
        .env("MERGE_QUEUE_GATE_CMD", gate)
        .env("MERGE_QUEUE_ALLOW_MERGE", "1")
        .env("MERGE_QUEUE_MONITOR_INTERVAL", "0.05")
        .env("MERGE_QUEUE_POLL_INTERVAL", "0.05")
        .env("MERGE_QUEUE_RETRY_INTERVAL", "0.05")
        .stdout(Stdio::null())
        .stderr(fs::File::create(&coordinator_log).expect("create priority coordinator log"))
        .process_group(0)
        .spawn()
        .expect("start priority fixture coordinator");
    let coordinator_pid = coordinator.id() as i32;
    let coordinator_guard = ProcessGroupGuard(coordinator_pid);

    let gate_pgid_path = fixture.state.join("gate.lock/gate_pgid");
    let deadline = Instant::now() + Duration::from_secs(20);
    while (!started.exists() || !gate_pgid_path.exists()) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        started.exists(),
        "coordinator never reached the cache-warm leg:\nworkloads:\n{}\ncoordinator:\n{}",
        fs::read_to_string(&workload_log).unwrap_or_default(),
        fs::read_to_string(&coordinator_log).unwrap_or_default(),
    );
    let prewarm_pgid: i32 = fs::read_to_string(&gate_pgid_path)
        .expect("read priority prewarm pgid")
        .trim()
        .parse()
        .expect("priority prewarm pgid is numeric");
    let prewarm_guard = ProcessGroupGuard(prewarm_pgid);
    let records = fs::read_to_string(&workload_log).expect("read priority workload log");
    let mut seen = BTreeSet::new();
    for record in records.lines() {
        let fields: Vec<_> = record.split('|').collect();
        match fields.as_slice() {
            ["taskpolicy", pid, pgid, "-c utility"] => {
                assert_eq!(pid.parse::<i32>().unwrap(), prewarm_pgid);
                assert_eq!(pgid.parse::<i32>().unwrap(), prewarm_pgid);
                seen.insert("taskpolicy".to_owned());
            }
            ["rustup", "utility", pgid, args] => {
                assert_eq!(pgid.parse::<i32>().unwrap(), prewarm_pgid);
                seen.insert(format!("rustup:{args}"));
            }
            ["cargo", "utility", pgid, target, args] => {
                assert_eq!(pgid.parse::<i32>().unwrap(), prewarm_pgid);
                seen.insert(format!("cargo:{target}:{args}"));
            }
            ["cache", "utility", pgid, "warm-witchy-caches"] => {
                assert_eq!(pgid.parse::<i32>().unwrap(), prewarm_pgid);
                seen.insert("cache:warm-witchy-caches".to_owned());
            }
            _ => panic!("unexpected prewarm workload record: {record}"),
        }
    }
    let expected = BTreeSet::from([
        "taskpolicy".to_owned(),
        "rustup:target add wasm32-unknown-unknown".to_owned(),
        "rustup:which --toolchain stable rustc".to_owned(),
        "cargo:target-prewarm:build --workspace".to_owned(),
        "cargo:target-prewarm:test --workspace --no-run".to_owned(),
        "cargo:target-prewarm:build --lib --no-default-features --target wasm32-unknown-unknown"
            .to_owned(),
        "cargo:target-prewarm-clippy:clippy --workspace --all-targets -- -D clippy::correctness -D clippy::suspicious -D unused_must_use".to_owned(),
        "cargo:target-prewarm-check:check --workspace --all-targets".to_owned(),
        "cache:warm-witchy-caches".to_owned(),
    ]);
    assert_eq!(seen, expected, "not every prewarm workload inherited utility QoS");

    fixture.mq_ok(&["submit", "a"], "true");
    let deadline = Instant::now() + Duration::from_secs(20);
    while (!cancelled.exists() || !gate_ran.exists()) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(cancelled.exists(), "queued work did not cancel the priority-wrapped prewarm");
    assert!(gate_ran.exists(), "queued gate did not run after priority prewarm cancellation");
    let deadline = Instant::now() + Duration::from_secs(5);
    while process_group_is_alive(prewarm_pgid) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        !process_group_is_alive(prewarm_pgid),
        "the gate.lock prewarm process group survived cancellation",
    );
    std::mem::forget(prewarm_guard);

    fs::write(&gate_release, "go\n").expect("release priority fixture gate");
    wait_for_branch_to_land(
        &fixture,
        "queued branch did not land after priority prewarm cancellation",
    );

    stop_coordinator(
        coordinator,
        coordinator_guard,
        "priority fixture coordinator ignored process-group termination",
    );
}

#[test]
fn queued_work_preempts_an_idle_prewarm_process_group() {
    // Keep rustup blocked before Cargo: the production stall occurred when
    // this setup ran synchronously before the cancellable prewarm PGID existed.
    let fixture = fixture_with_gate_worktree();
    let bin = fixture._temp.path().join("prewarm-bin");
    fs::create_dir(&bin).expect("create fake prewarm bin");
    let started = fixture._temp.path().join("prewarm-started");
    let cancelled = fixture._temp.path().join("prewarm-cancelled");
    let rustup_pid_file = fixture._temp.path().join("prewarm-rustup-pid");
    let cargo_env = fixture._temp.path().join("prewarm-cargo-env");
    let gate_ran = fixture._temp.path().join("gate-ran");
    let gate_proceed = fixture._temp.path().join("gate-proceed");
    let cargo = bin.join("cargo");
    fs::write(
        &cargo,
        format!(
            "#!/bin/sh\nif [ -e \"{}\" ]; then\n  printf '%s|%s|%s|%s' \"${{CARGO_INCREMENTAL-unset}}\" \"${{RUSTC_WRAPPER-unset}}\" \"${{CARGO_BUILD_RUSTC_WRAPPER-unset}}\" \"${{CARGO_PROFILE_TEST_STRIP-unset}}\" >\"{}\"\nfi\nexit 0\n",
            gate_proceed.display(),
            cargo_env.display(),
        ),
    )
    .expect("write fake prewarm cargo");
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755))
        .expect("chmod fake cargo");
    let rustup = bin.join("rustup");
    fs::write(
        &rustup,
        format!(
            "#!/bin/sh\nif [ \"$1\" = target ] && [ ! -e \"{}\" ]; then\n  trap 'printf cancelled >\"{}\"; exit 143' TERM INT\n  printf '%s' \"$$\" >\"{}\"\n  printf started >\"{}\"\n  while :; do /bin/sleep 1; done\nfi\ncase \"$1\" in\n  target) exit 0 ;;\n  which) printf '/usr/bin/rustc\\n'; exit 0 ;;\nesac\nexit 1\n",
            started.display(),
            cancelled.display(),
            rustup_pid_file.display(),
            started.display(),
        ),
    )
    .expect("write blocking rustup");
    fs::set_permissions(&rustup, fs::Permissions::from_mode(0o755))
        .expect("chmod fake rustup");

    let queue_script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/merge-queue.sh");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let gate = format!(
        "printf ran >{}; while [ ! -e {} ]; do /bin/sleep 0.05; done",
        gate_ran.display(),
        gate_proceed.display(),
    );
    let coordinator = Command::new("bash")
        .arg(&queue_script)
        .arg("run")
        .env("PATH", path)
        .env("MERGE_QUEUE_TEST_ROOT", &fixture.root)
        .env("MERGE_QUEUE_ALLOW_TEST_ROOT", "1")
        .env("MERGE_QUEUE_STATE_DIR", &fixture.state)
        .env("MERGE_QUEUE_GATE_WT", &fixture.gate_worktree)
        .env("MERGE_QUEUE_GATE_CMD", gate)
        .env("MERGE_QUEUE_ALLOW_MERGE", "1")
        .env("MERGE_QUEUE_POLL_INTERVAL", "1")
        .env("MERGE_QUEUE_RETRY_INTERVAL", "1")
        .env("CARGO_INCREMENTAL", "1")
        .env("RUSTC_WRAPPER", "forbidden-wrapper")
        .env("CARGO_BUILD_RUSTC_WRAPPER", "forbidden-wrapper")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("start persistent coordinator");
    let coordinator_pid = coordinator.id() as i32;
    let guard = ProcessGroupGuard(coordinator_pid);

    let deadline = Instant::now() + Duration::from_secs(20);
    while !started.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(started.exists(), "coordinator never entered prewarm");
    let rustup_pid = fs::read_to_string(&rustup_pid_file)
        .expect("read blocking rustup pid")
        .parse::<i32>()
        .expect("parse blocking rustup pid");
    let prewarm_guard = ProcessGroupGuard(process_group(rustup_pid));
    assert!(
        fixture.state.join("gate.lock").exists(),
        "prewarm did not retain the serialized gate lock"
    );

    fixture.mq_ok(&["submit", "a"], "true");
    let deadline = Instant::now() + Duration::from_secs(20);
    while (!cancelled.exists() || !gate_ran.exists()) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(cancelled.exists(), "queued work did not cancel idle prewarm");
    assert!(gate_ran.exists(), "coordinator did not advance queued work after prewarm");
    drop(prewarm_guard);
    assert!(
        !fixture.state.join("prewarmed").exists(),
        "cancelled prewarm was recorded as complete"
    );
    fs::write(&gate_proceed, "go\n").expect("release fake gate");
    wait_for_branch_to_land(&fixture, "queued branch did not land");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !cargo_env.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(
        fs::read_to_string(&cargo_env).expect("read prewarm Cargo environment"),
        "unset|||symbols",
        "idle prewarm did not match the serialized gate Cargo profile",
    );

    stop_coordinator(
        coordinator,
        guard,
        "coordinator ignored process-group termination"
    );
}

#[test]
fn queued_work_cancels_inactive_prewarm_and_preserves_active_generation() {
    let fixture = fixture_with_gate_worktree();
    fs::create_dir_all(&fixture.state).expect("create generation state");
    fs::create_dir_all(fixture.gate_worktree.join("target"))
        .expect("create active gate target");
    fs::write(
        fixture.gate_worktree.join("target/generation-sentinel"),
        "active\n",
    )
    .expect("write active generation sentinel");
    fs::write(fixture.state.join("gate-target"), "target\n")
        .expect("select active gate target");

    let bin = fixture._temp.path().join("inactive-prewarm-bin");
    fs::create_dir(&bin).expect("create fake prewarm bin");
    let started = fixture._temp.path().join("inactive-prewarm-started");
    let cancelled = fixture._temp.path().join("inactive-prewarm-cancelled");
    let cargo_pid_file = fixture._temp.path().join("inactive-prewarm-cargo-pid");
    let prewarm_target = fixture._temp.path().join("inactive-prewarm-target");
    let gate_ran = fixture._temp.path().join("gate-ran");
    let gate_release = fixture._temp.path().join("gate-release");
    let gate_target = fixture._temp.path().join("gate-target-seen");
    let gate_sentinel = fixture._temp.path().join("gate-sentinel-seen");
    let coordinator_log = fixture._temp.path().join("coordinator.log");
    let cargo = bin.join("cargo");
    fs::write(
        &cargo,
        format!(
            "#!/bin/sh\ntarget_dir=\"${{CARGO_TARGET_DIR:-target}}\"\nif [ \"$target_dir\" = target-prewarm ]; then\n  printf '%s' \"$target_dir\" >\"{}\"\n  trap 'printf cancelled >\"{}\"; exit 143' TERM INT\n  mkdir -p \"$target_dir/debug/.fingerprint/inactive-prewarm\"\n  : >\"$target_dir/debug/.fingerprint/inactive-prewarm/invoked.timestamp\"\n  printf '%s' \"$$\" >\"{}\"\n  printf started >\"{}\"\n  while :; do /bin/sleep 1; done\nfi\nexit 0\n",
            prewarm_target.display(),
            cancelled.display(),
            cargo_pid_file.display(),
            started.display(),
        ),
    )
    .expect("write blocking inactive prewarm Cargo");
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755))
        .expect("chmod blocking inactive prewarm Cargo");
    let rustup = bin.join("rustup");
    fs::write(
        &rustup,
        format!(
            "#!/bin/sh\ncase \"$1\" in\n  target) exit 0 ;;\n  which) printf '{}/rustc\\n'; exit 0 ;;\nesac\nexit 1\n",
            bin.display(),
        ),
    )
    .expect("write fake prewarm rustup");
    fs::set_permissions(&rustup, fs::Permissions::from_mode(0o755))
        .expect("chmod fake prewarm rustup");

    let queue_script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/merge-queue.sh");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let gate = format!(
        "printf '%s' \"${{CARGO_TARGET_DIR:-unset}}\" >{}; cat \"${{CARGO_TARGET_DIR:-target}}/generation-sentinel\" >{}; printf ran >{}; while [ ! -e {} ]; do /bin/sleep 0.05; done",
        gate_target.display(),
        gate_sentinel.display(),
        gate_ran.display(),
        gate_release.display(),
    );
    let coordinator = Command::new("bash")
        .arg(&queue_script)
        .arg("run")
        .env("PATH", path)
        .env("MERGE_QUEUE_TEST_ROOT", &fixture.root)
        .env("MERGE_QUEUE_ALLOW_TEST_ROOT", "1")
        .env("MERGE_QUEUE_STATE_DIR", &fixture.state)
        .env("MERGE_QUEUE_GATE_WT", &fixture.gate_worktree)
        .env("MERGE_QUEUE_GATE_CMD", gate)
        .env("MERGE_QUEUE_ALLOW_MERGE", "1")
        .env("MERGE_QUEUE_MONITOR_INTERVAL", "0.05")
        .env("MERGE_QUEUE_POLL_INTERVAL", "0.05")
        .env("MERGE_QUEUE_RETRY_INTERVAL", "0.05")
        .stdout(Stdio::null())
        .stderr(
            fs::File::create(&coordinator_log).expect("create coordinator diagnostic log"),
        )
        .process_group(0)
        .spawn()
        .expect("start persistent coordinator");
    let coordinator_pid = coordinator.id() as i32;
    let coordinator_guard = ProcessGroupGuard(coordinator_pid);

    let deadline = Instant::now() + Duration::from_secs(10);
    while !started.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        started.exists(),
        "coordinator never entered inactive Cargo prewarm:\n{}",
        fs::read_to_string(&coordinator_log).unwrap_or_default(),
    );
    assert_eq!(
        fs::read_to_string(&prewarm_target).expect("read prewarm target"),
        "target-prewarm",
        "idle prewarm did not use the inactive generation",
    );
    let cargo_pid = fs::read_to_string(&cargo_pid_file)
        .expect("read blocking Cargo pid")
        .parse::<i32>()
        .expect("parse blocking Cargo pid");
    let cargo_pgid = process_group(cargo_pid);
    let coordinator_pgid = process_group(coordinator_pid);
    let cargo_guard = (cargo_pgid != coordinator_pgid).then(|| ProcessGroupGuard(cargo_pgid));

    let submitted_at = Instant::now();
    fixture.mq_ok(&["submit", "a"], "true");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !gate_ran.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        gate_ran.exists(),
        "queued gate waited behind inactive prewarm Cargo:\n{}",
        fs::read_to_string(&coordinator_log).unwrap_or_default()
    );
    assert!(
        submitted_at.elapsed() < Duration::from_secs(10),
        "queued gate did not start promptly after cancelling inactive prewarm"
    );
    assert!(cancelled.exists(), "queued work did not cancel inactive prewarm Cargo");
    assert_eq!(
        fs::read_to_string(&gate_target).expect("read gate target"),
        "target",
        "gate did not retain the active target generation",
    );
    assert_eq!(
        fs::read_to_string(&gate_sentinel).expect("read gate sentinel"),
        "active\n",
        "gate did not see the active target sentinel",
    );
    assert_eq!(
        fs::read_to_string(fixture.state.join("gate-target")).expect("read target pointer"),
        "target\n",
        "cancelled prewarm changed the active target pointer",
    );
    assert_eq!(
        fs::read_to_string(fixture.gate_worktree.join("target/generation-sentinel"))
            .expect("read unchanged active sentinel"),
        "active\n",
        "inactive prewarm mutated the active target",
    );
    assert!(
        !fixture
            .gate_worktree
            .join("target/debug/.fingerprint/inactive-prewarm/invoked.timestamp")
            .exists(),
        "inactive prewarm wrote a fingerprint into the active target",
    );
    assert!(
        fixture
            .gate_worktree
            .join("target-prewarm/debug/.fingerprint/inactive-prewarm/invoked.timestamp")
            .exists(),
        "inactive prewarm never touched its target generation",
    );
    assert!(
        !fixture.state.join("prewarmed").exists(),
        "cancelled prewarm was recorded as complete",
    );
    assert!(
        fixture.state.join("prewarm-incomplete").exists(),
        "cancelled generation lost its incomplete marker",
    );

    fs::write(&gate_release, "go\n").expect("release fake gate");
    wait_for_branch_to_land(
        &fixture,
        "queued branch did not land after inactive prewarm cancellation",
    );
    if cargo_pgid != coordinator_pgid {
        let deadline = Instant::now() + Duration::from_secs(5);
        while process_group_is_alive(cargo_pgid) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(25));
        }
        assert!(!process_group_is_alive(cargo_pgid), "cancelled Cargo group survived");
        std::mem::forget(cargo_guard);
    }
    stop_coordinator(
        coordinator,
        coordinator_guard,
        "coordinator ignored process-group termination",
    );
}

#[test]
fn successful_prewarm_promotes_inactive_generation_for_next_gate() {
    let fixture = fixture_with_gate_worktree();
    fs::create_dir_all(&fixture.state).expect("create generation state");
    fs::create_dir_all(fixture.gate_worktree.join("target"))
        .expect("create active gate target");
    fs::create_dir_all(fixture.gate_worktree.join("target-clippy"))
        .expect("create active clippy target");
    fs::create_dir_all(fixture.gate_worktree.join("target-check"))
        .expect("create active check target");
    fs::create_dir_all(fixture.gate_worktree.join("target-prewarm-clippy"))
        .expect("create inactive clippy target");
    fs::create_dir_all(fixture.gate_worktree.join("target-prewarm-check"))
        .expect("create inactive check target");
    fs::write(
        fixture.gate_worktree.join("target/generation-sentinel"),
        "old-active\n",
    )
    .expect("write old active generation sentinel");
    fs::write(fixture.state.join("gate-target"), "target\n")
        .expect("select initial gate target");

    let bin = fixture._temp.path().join("promoting-prewarm-bin");
    fs::create_dir(&bin).expect("create fake promoting prewarm bin");
    let started = fixture._temp.path().join("promoting-prewarm-started");
    let prewarm_release = fixture._temp.path().join("promoting-prewarm-release");
    let cargo_targets = fixture._temp.path().join("promoting-prewarm-targets");
    let gate_ran = fixture._temp.path().join("promoted-gate-ran");
    let gate_release = fixture._temp.path().join("promoted-gate-release");
    let gate_target = fixture._temp.path().join("promoted-gate-target");
    let gate_sentinel = fixture._temp.path().join("promoted-gate-sentinel");
    let cargo = bin.join("cargo");
    fs::write(
        &cargo,
        format!(
            "#!/bin/sh\ntarget_dir=\"${{CARGO_TARGET_DIR:-target}}\"\nprintf '%s\\n' \"$target_dir\" >>\"{}\"\nif [ ! -e \"{}\" ]; then\n  mkdir -p \"$target_dir\"\n  printf promoted >\"$target_dir/generation-sentinel\"\n  printf started >\"{}\"\n  while [ ! -e \"{}\" ]; do /bin/sleep 0.05; done\nfi\nexit 0\n",
            cargo_targets.display(),
            started.display(),
            started.display(),
            prewarm_release.display(),
        ),
    )
    .expect("write successful fake prewarm Cargo");
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755))
        .expect("chmod successful fake prewarm Cargo");
    let rustup = bin.join("rustup");
    fs::write(
        &rustup,
        format!(
            "#!/bin/sh\ncase \"$1\" in\n  target) exit 0 ;;\n  which) printf '{}/rustc\\n'; exit 0 ;;\nesac\nexit 1\n",
            bin.display(),
        ),
    )
    .expect("write fake promoting rustup");
    fs::set_permissions(&rustup, fs::Permissions::from_mode(0o755))
        .expect("chmod fake promoting rustup");

    let queue_script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/merge-queue.sh");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let gate = format!(
        "printf '%s' \"${{CARGO_TARGET_DIR:-unset}}\" >{}; cat \"${{CARGO_TARGET_DIR:-target}}/generation-sentinel\" >{}; printf ran >{}; while [ ! -e {} ]; do /bin/sleep 0.05; done",
        gate_target.display(),
        gate_sentinel.display(),
        gate_ran.display(),
        gate_release.display(),
    );
    let coordinator = Command::new("bash")
        .arg(&queue_script)
        .arg("run")
        .env("PATH", path)
        .env("MERGE_QUEUE_TEST_ROOT", &fixture.root)
        .env("MERGE_QUEUE_ALLOW_TEST_ROOT", "1")
        .env("MERGE_QUEUE_STATE_DIR", &fixture.state)
        .env("MERGE_QUEUE_GATE_WT", &fixture.gate_worktree)
        .env("MERGE_QUEUE_GATE_CMD", gate)
        .env("MERGE_QUEUE_ALLOW_MERGE", "1")
        .env("MERGE_QUEUE_MONITOR_INTERVAL", "0.05")
        .env("MERGE_QUEUE_POLL_INTERVAL", "0.05")
        .env("MERGE_QUEUE_RETRY_INTERVAL", "0.05")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("start persistent coordinator");
    let coordinator_pid = coordinator.id() as i32;
    let coordinator_guard = ProcessGroupGuard(coordinator_pid);

    let deadline = Instant::now() + Duration::from_secs(20);
    while !started.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(started.exists(), "coordinator never started inactive prewarm");
    assert_eq!(
        fs::read_to_string(fixture.state.join("gate-target")).expect("read initial pointer"),
        "target\n",
        "target generation was promoted before prewarm completed",
    );
    assert!(
        fixture.state.join("prewarm-incomplete").exists(),
        "in-progress generation was not marked incomplete",
    );
    assert!(!fixture.state.join("prewarmed").exists());
    assert_eq!(
        fs::read_to_string(fixture.gate_worktree.join("target/generation-sentinel"))
            .expect("read old active sentinel"),
        "old-active\n",
        "inactive prewarm mutated the old active generation",
    );

    fs::write(&prewarm_release, "go\n").expect("release successful prewarm");
    let deadline = Instant::now() + Duration::from_secs(20);
    while fs::read_to_string(fixture.state.join("gate-target"))
        .is_ok_and(|target| target.trim() != "target-prewarm")
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(
        fs::read_to_string(fixture.state.join("gate-target")).expect("read promoted pointer"),
        "target-prewarm\n",
        "successful prewarm did not atomically promote the inactive generation",
    );
    assert!(fixture.state.join("prewarmed").exists(), "prewarm completion was not recorded");
    assert!(
        !fixture.state.join("prewarm-incomplete").exists(),
        "successful generation retained the incomplete marker",
    );
    let targets = fs::read_to_string(&cargo_targets).expect("read prewarm Cargo targets");
    assert!(
        targets.lines().any(|target| target == "target-prewarm"),
        "main prewarm did not use target-prewarm: {targets}",
    );
    assert!(
        targets.lines().any(|target| target == "target-prewarm-clippy"),
        "clippy prewarm did not use the inactive suffix: {targets}",
    );
    assert!(
        targets.lines().any(|target| target == "target-prewarm-check"),
        "check prewarm did not use the inactive suffix: {targets}",
    );

    fixture.mq_ok(&["submit", "a"], "true");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !gate_ran.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(gate_ran.exists(), "gate did not run after successful promotion");
    assert_eq!(
        fs::read_to_string(&gate_target).expect("read promoted gate target"),
        "target-prewarm",
        "next gate did not receive the promoted CARGO_TARGET_DIR",
    );
    assert_eq!(
        fs::read_to_string(&gate_sentinel).expect("read promoted gate sentinel"),
        "promoted",
        "next gate did not consume the promoted generation sentinel",
    );
    assert_eq!(
        fs::read_to_string(fixture.state.join("gate-target")).expect("read stable pointer"),
        "target-prewarm\n",
        "gate changed the promoted target pointer",
    );

    fs::write(&gate_release, "go\n").expect("release promoted gate");
    wait_for_branch_to_land(
        &fixture,
        "queued branch did not land through the promoted generation",
    );
    stop_coordinator(
        coordinator,
        coordinator_guard,
        "coordinator ignored process-group termination",
    );
}

#[test]
fn doctor_treats_denied_process_inspection_as_advisory() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp = TempDir::new();
    let state = temp.path().join("state");
    let bin = temp.path().join("bin");
    fs::create_dir_all(&state).expect("create isolated coordinator state");
    let queue = state.join("queue");
    fs::create_dir(&queue).expect("create isolated queue");
    fs::write(queue.join("0001.json"), "{}\n").expect("write logical queue entry");
    fs::write(queue.join("0001.json.nobatch"), "").expect("write no-batch sidecar");
    fs::write(queue.join("0001.json.batch-limit"), "2\n")
        .expect("write batch-limit sidecar");
    fs::create_dir(&bin).expect("create fake tool directory");
    fs::write(
        state.join("coordinator.pid"),
        format!("{}\n", std::process::id()),
    )
    .expect("write live coordinator pid fixture");
    let ps = bin.join("ps");
    fs::write(&ps, "#!/bin/sh\nexit 126\n").expect("write denied ps fixture");
    fs::set_permissions(&ps, fs::Permissions::from_mode(0o755)).expect("chmod fake ps");

    let output = Command::new(root.join("scripts/merge-queue.sh"))
        .arg("doctor")
        .env("MERGE_QUEUE_STATE_DIR", &state)
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default()),
        )
        .output()
        .expect("run doctor with denied process inspection");
    assert!(
        output.status.success(),
        "doctor made advisory ps failure fatal: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("coordinator : RUNNING"),
        "doctor lost coordinator health output",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("queue       : 1 pending — 0001.json"),
        "doctor counted queue sidecars as logical entries: {stdout}",
    );
    assert!(
        !stdout.contains("0001.json.nobatch") && !stdout.contains("0001.json.batch-limit"),
        "doctor exposed internal queue sidecars: {stdout}",
    );
}
