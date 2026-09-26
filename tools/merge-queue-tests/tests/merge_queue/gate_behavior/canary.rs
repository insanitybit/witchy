use super::*;

// The idle master canary: a workspace test pass on each new master tip while
// the queue has no ready work. Report only; it never merges or blocks.

struct Canary {
    fixture: QueueFixture,
    runs: PathBuf,
    started: PathBuf,
    cancelled: PathBuf,
    coordinator_log: PathBuf,
    coordinator: Option<(std::process::Child, ProcessGroupGuard)>,
}

impl Canary {
    fn new() -> Self {
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
        let temp = fixture._temp.path();
        Self {
            runs: temp.join("canary-runs"),
            started: temp.join("canary-started"),
            cancelled: temp.join("canary-cancelled"),
            coordinator_log: temp.join("canary-coordinator.log"),
            fixture,
            coordinator: None,
        }
    }

    /// Start a persistent coordinator whose gate records every run, passes a
    /// branch carrying `a.txt`, fails a tree carrying `broken.txt`, and
    /// otherwise blocks (until TERM) when `block` is set.
    fn start(&mut self, interval: &str, block: bool) {
        let temp = self.fixture._temp.path();
        let bin = temp.join("canary-bin");
        fs::create_dir_all(&bin).expect("create fake canary bin");
        let cargo = bin.join("cargo");
        fs::write(&cargo, "#!/bin/sh\nexit 0\n").expect("write fake prewarm cargo");
        fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755))
            .expect("chmod fake prewarm cargo");
        let gate = temp.join("canary-gate.sh");
        fs::write(
            &gate,
            r#"#!/bin/sh
printf '%s|%s|%s\n' "${WITCHY_GATE_SCOPE-unset}" "${WITCHY_GATE_NEXTEST-unset}" "$(git rev-parse HEAD)" >>"__RUNS__"
[ ! -e a.txt ] || exit 0
if [ -e broken.txt ]; then
    printf '        FAIL [   0.101s] witchy::misc canary_fixture::broken_on_master\n'
    exit 1
fi
if [ "${CANARY_FIXTURE_BLOCK:-0}" = 1 ]; then
    trap 'printf cancelled >"__CANCELLED__"; exit 143' TERM INT
    printf started >"__STARTED__"
    while :; do /bin/sleep 0.05; done
fi
exit 0
"#
            .replace("__RUNS__", &self.runs.display().to_string())
            .replace("__CANCELLED__", &self.cancelled.display().to_string())
            .replace("__STARTED__", &self.started.display().to_string()),
        )
        .expect("write canary fixture gate");
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let child = self
            .fixture
            .mq_command(&["run"], &format!("sh {}", gate.display()))
            .env("PATH", path)
            .env("MERGE_QUEUE_CANARY", "1")
            .env("MERGE_QUEUE_CANARY_INTERVAL", interval)
            .env("CANARY_FIXTURE_BLOCK", if block { "1" } else { "0" })
            .env("MERGE_QUEUE_MONITOR_INTERVAL", "0.05")
            .env("MERGE_QUEUE_POLL_INTERVAL", "0.05")
            .env("MERGE_QUEUE_RETRY_INTERVAL", "0.05")
            .env("MERGE_QUEUE_TEST_TERMINATE_GRACE", "0.1")
            .stdout(Stdio::null())
            .stderr(fs::File::create(&self.coordinator_log).expect("create coordinator log"))
            .process_group(0)
            .spawn()
            .expect("start canary fixture coordinator");
        let guard = ProcessGroupGuard(child.id() as i32);
        self.coordinator = Some((child, guard));
    }

    fn master(&self) -> String {
        String::from_utf8(run_git(&self.fixture.root, &["rev-parse", "master"]).stdout)
            .unwrap()
            .trim()
            .to_owned()
    }

    fn commit_on_master(&self, path: &str, contents: Option<&str>, message: &str) -> String {
        let file = self.fixture.root.join(path);
        match contents {
            Some(contents) => {
                fs::write(&file, contents).expect("write master fixture file");
                run_git(&self.fixture.root, &["add", path]);
            }
            None => {
                run_git(&self.fixture.root, &["rm", "-q", path]);
            }
        }
        run_git(&self.fixture.root, &["commit", "-q", "-m", message]);
        self.master()
    }

    fn events(&self, event: &str) -> Vec<serde_json::Value> {
        fs::read_to_string(self.fixture.state.join("journal.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|value| value["event"] == event)
            .collect()
    }

    fn wait_for(&self, what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(25));
        }
        assert!(
            done(),
            "{what}\njournal:\n{}\ncoordinator:\n{}",
            fs::read_to_string(self.fixture.state.join("journal.jsonl")).unwrap_or_default(),
            fs::read_to_string(&self.coordinator_log).unwrap_or_default(),
        );
    }

    fn wait_for_event(&self, event: &str, sha: &str) -> serde_json::Value {
        self.wait_for(&format!("no {event} for {sha}"), || {
            self.events(event).iter().any(|value| value["sha"] == sha)
        });
        self.events(event)
            .into_iter()
            .find(|value| value["sha"] == sha)
            .unwrap()
    }

    fn runs(&self) -> Vec<String> {
        fs::read_to_string(&self.runs)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn report(&self) -> PathBuf {
        self.fixture.state.join("agents/MASTER-CANARY-RED.md")
    }
}

impl Drop for Canary {
    fn drop(&mut self) {
        if let Some((mut child, guard)) = self.coordinator.take() {
            drop(guard);
            let deadline = Instant::now() + Duration::from_secs(5);
            while child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(25));
            }
        }
    }
}

#[test]
fn idle_canary_runs_the_workspace_gate_once_per_new_master_tip() {
    let mut canary = Canary::new();
    let first = canary.master();
    canary.start("0", false);
    let green = canary.wait_for_event("master_canary_green", &first);
    let log = green["log"].as_str().expect("canary journals its log");
    assert!(
        log.contains(&format!("-master-canary-{}", &first[..12])) && Path::new(log).exists(),
        "canary log is not named for the tip: {log}",
    );
    assert_eq!(
        canary.runs(),
        vec![format!("all||{first}")],
        "canary must run the unfiltered workspace gate on the exact master tip",
    );

    // Already canaried: idle passes must not re-run it.
    thread::sleep(Duration::from_millis(500));
    assert_eq!(canary.runs().len(), 1, "an already-canaried tip ran again");

    let second = canary.commit_on_master("c.txt", Some("c\n"), "advance master");
    canary.wait_for_event("master_canary_green", &second);
    assert_eq!(canary.runs(), vec![format!("all||{first}"), format!("all||{second}")]);
    assert_eq!(canary.master(), second, "the canary must never move master");
    let status = canary.fixture.status();
    assert_eq!(status["master_canary"]["sha"], second.as_str());
    assert_eq!(status["master_canary"]["result"], "green");
    assert!(status["master_canary_red"].is_null(), "{status}");
}

#[test]
fn canary_is_rate_limited_across_new_tips() {
    let mut canary = Canary::new();
    let first = canary.master();
    canary.start("3600", false);
    canary.wait_for_event("master_canary_green", &first);
    canary.commit_on_master("c.txt", Some("c\n"), "advance master");
    thread::sleep(Duration::from_millis(700));
    assert_eq!(
        canary.runs().len(),
        1,
        "a new tip inside MERGE_QUEUE_CANARY_INTERVAL started another canary",
    );
}

#[test]
fn ready_work_aborts_a_running_canary_and_gates_first() {
    let mut canary = Canary::new();
    let first = canary.master();
    canary.start("0", true);
    canary.wait_for("canary never started", || canary.started.exists());
    let what = fs::read_to_string(canary.fixture.state.join("gate.lock/what"))
        .expect("canary holds the gate lock");
    assert!(what.starts_with("master canary"), "{what}");

    canary.fixture.mq_ok(&["submit", "a"], "true");
    canary.wait_for("queued work did not cancel the canary", || {
        canary.cancelled.exists()
    });
    canary.wait_for_event("master_canary_yielded", &first);
    canary.wait_for("the queued branch did not land after the canary yielded", || {
        git_output(&canary.fixture.root, &["show", "master:a.txt"])
            .status
            .success()
    });
    assert!(
        canary.events("master_canary_green").is_empty()
            && canary.events("master_canary_red").is_empty(),
        "a yielded canary must not report an outcome",
    );
    // The landed tip is idle again and gets its own canary.
    let landed = canary.master();
    assert_ne!(landed, first);
    canary.wait_for_event("master_canary_green", &landed);
}

#[test]
fn red_canary_writes_the_report_and_green_clears_it() {
    let mut canary = Canary::new();
    let good = canary.master();
    canary.start("0", false);
    canary.wait_for_event("master_canary_green", &good);

    let broken = canary.commit_on_master("broken.txt", Some("x\n"), "break master");
    let red = canary.wait_for_event("master_canary_red", &broken);
    assert!(
        red["failing"]
            .as_str()
            .is_some_and(|line| line.contains("canary_fixture::broken_on_master")),
        "{red}",
    );
    assert_eq!(red["last_green"], good.as_str());
    assert_eq!(red["range"], format!("{good}..{broken}").as_str());
    let report_path = canary.report();
    let report = fs::read_to_string(&report_path).expect("red canary writes its report");
    for needle in [
        "canary_fixture::broken_on_master",
        red["log"].as_str().unwrap(),
        "break master",
        &format!("{good}..{broken}"),
    ] {
        assert!(report.contains(needle), "report lacks {needle:?}:\n{report}");
    }
    let status = canary.fixture.status();
    assert_eq!(status["master_canary"]["result"], "red");
    assert_eq!(status["master_canary_red"], report_path.to_str().unwrap());

    let fixed = canary.commit_on_master("broken.txt", None, "fix master");
    canary.wait_for_event("master_canary_green", &fixed);
    assert!(!report_path.exists(), "a green canary must clear the red report");
}
