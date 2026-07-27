use std::collections::{BTreeMap, BTreeSet};

use crate::{
    FixtureCall, FixtureErrorCode, FixtureExecResponse, FixtureFailure, FixtureFamily,
    FixtureHandle, FixtureOutcome, FixturePlan, FixtureSession, FixtureValue, SourceLocation,
};

pub type ExecProviderResult<T> = Result<T, FixtureFailure>;

#[derive(Debug)]
pub(crate) struct ExecProviderState {
    configured: bool,
    root_tools: BTreeSet<String>,
    handles: BTreeMap<u64, BTreeSet<String>>,
}

impl ExecProviderState {
    pub(crate) fn new(plan: &FixturePlan) -> Self {
        let Some(fixture) = &plan.exec else {
            return Self {
                configured: false,
                root_tools: BTreeSet::new(),
                handles: BTreeMap::new(),
            };
        };
        Self {
            configured: true,
            root_tools: fixture.tools.iter().cloned().collect(),
            handles: BTreeMap::new(),
        }
    }

    pub(crate) const fn configured(&self) -> bool {
        self.configured
    }
}

impl FixtureSession {
    pub fn mint_fixture_exec(
        &mut self,
        source: Option<SourceLocation>,
    ) -> ExecProviderResult<FixtureHandle> {
        if !self.exec.configured {
            return Err(exec_failure(
                FixtureErrorCode::PermissionDenied,
                "Exec fixture was not declared",
            ));
        }
        let mut call = FixtureCall::new(FixtureFamily::Exec, "mint_exec");
        call.effective_rights = exec_rights(&self.exec.root_tools);
        call.source = source;
        exec_marker(
            self.observe(
                call,
                FixtureOutcome::Return {
                    value: FixtureValue::String("Exec".into()),
                },
            ),
            "mint_exec",
        )?;
        let handle = self
            .basic
            .mint_handle(FixtureFamily::Exec, BTreeSet::new());
        self.exec
            .handles
            .insert(handle.id(), self.exec.root_tools.clone());
        Ok(handle)
    }

    pub fn exec_only(
        &mut self,
        handle: &FixtureHandle,
        tools: &[String],
        source: Option<SourceLocation>,
    ) -> ExecProviderResult<FixtureHandle> {
        let current = self.exec_tools(handle)?;
        let requested: BTreeSet<String> = tools.iter().cloned().collect();
        if !requested.is_subset(&current) {
            return Err(exec_failure(
                FixtureErrorCode::PermissionDenied,
                "Exec.only cannot widen its tool allow-list",
            ));
        }
        let mut call = FixtureCall::new(FixtureFamily::Exec, "exec_only");
        call.arguments.insert(
            "tools".into(),
            FixtureValue::List(
                tools
                    .iter()
                    .cloned()
                    .map(FixtureValue::String)
                    .collect(),
            ),
        );
        call.effective_rights = exec_rights(&current);
        call.source = source;
        exec_marker(
            self.observe(
                call,
                FixtureOutcome::Return {
                    value: FixtureValue::String("Exec".into()),
                },
            ),
            "exec_only",
        )?;
        let narrowed = self
            .basic
            .mint_handle(FixtureFamily::Exec, BTreeSet::new());
        self.exec.handles.insert(narrowed.id(), requested);
        Ok(narrowed)
    }

    pub fn exec_run(
        &mut self,
        exec: &FixtureHandle,
        dir: &FixtureHandle,
        path: &str,
        arguments: &[String],
        stdin: &str,
        source: Option<SourceLocation>,
    ) -> ExecProviderResult<FixtureExecResponse> {
        let tools = self.exec_tools(exec)?;
        if !tools.contains(path) {
            return Err(exec_failure(
                FixtureErrorCode::PermissionDenied,
                format!("exec: `{path}` is not in this Exec fixture's allow-list"),
            ));
        }
        if arguments
            .iter()
            .any(|argument| argument.contains('\0'))
        {
            return Err(exec_failure(
                FixtureErrorCode::InvalidRequest,
                "exec.run: an argument may not contain a NUL byte",
            ));
        }
        let dir_rights = self
            .authorize_exec_target(dir, path)
            .map_err(|error| exec_failure(error.code, error.message))?;
        let mut call = FixtureCall::new(FixtureFamily::Exec, "exec_run");
        call.target = Some(path.into());
        call.arguments.insert(
            "args".into(),
            FixtureValue::List(
                arguments
                    .iter()
                    .cloned()
                    .map(FixtureValue::String)
                    .collect(),
            ),
        );
        call.arguments
            .insert("stdin".into(), FixtureValue::String(stdin.into()));
        call.effective_rights = exec_rights(&tools);
        call.effective_rights
            .extend(dir_rights.into_iter().map(|right| format!("dir:{right}")));
        call.source = source;
        decode_exec_response(self.scripted_call(call))
    }

    fn exec_tools(&self, handle: &FixtureHandle) -> ExecProviderResult<BTreeSet<String>> {
        if !self.basic.validate_handle(handle, FixtureFamily::Exec) {
            return Err(exec_failure(
                FixtureErrorCode::PermissionDenied,
                "invalid or foreign Exec fixture handle",
            ));
        }
        self.exec
            .handles
            .get(&handle.id())
            .cloned()
            .ok_or_else(|| {
                exec_failure(
                    FixtureErrorCode::PermissionDenied,
                    "invalid or foreign Exec fixture handle",
                )
            })
    }
}

fn exec_rights(tools: &BTreeSet<String>) -> Vec<String> {
    tools.iter().map(|tool| format!("exec:{tool}")).collect()
}

fn exec_marker(outcome: FixtureOutcome, operation: &str) -> ExecProviderResult<()> {
    match outcome {
        FixtureOutcome::Return {
            value: FixtureValue::String(value),
        } if value == "Exec" => Ok(()),
        FixtureOutcome::Fail { error } => Err(error),
        _ => Err(exec_failure(
            FixtureErrorCode::InvalidData,
            format!("{operation} returned an invalid Exec marker"),
        )),
    }
}

fn decode_exec_response(outcome: FixtureOutcome) -> ExecProviderResult<FixtureExecResponse> {
    let FixtureOutcome::Return {
        value: FixtureValue::Map(mut fields),
    } = outcome
    else {
        return match outcome {
            FixtureOutcome::Fail { error } => Err(error),
            _ => Err(exec_failure(
                FixtureErrorCode::InvalidData,
                "Exec fixture returned an invalid response",
            )),
        };
    };
    let exit_code = take_string(&mut fields, "exit_code")?
        .parse::<i32>()
        .map_err(|_| exec_failure(FixtureErrorCode::InvalidData, "invalid Exec exit code"))?;
    let stdout = take_string(&mut fields, "stdout")?;
    let stderr = take_string(&mut fields, "stderr")?;
    if !fields.is_empty() {
        return Err(exec_failure(
            FixtureErrorCode::InvalidData,
            "Exec response contains unknown fields",
        ));
    }
    Ok(FixtureExecResponse {
        exit_code,
        stdout,
        stderr,
    })
}

fn take_string(
    fields: &mut BTreeMap<String, FixtureValue>,
    name: &str,
) -> ExecProviderResult<String> {
    match fields.remove(name) {
        Some(FixtureValue::String(value)) => Ok(value),
        _ => Err(exec_failure(
            FixtureErrorCode::InvalidData,
            format!("Exec response `{name}` must be a string"),
        )),
    }
}

fn exec_failure(code: FixtureErrorCode, message: impl Into<String>) -> FixtureFailure {
    FixtureFailure {
        code,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ExecFixture, FilesystemEntry, FilesystemFixture, FixtureStep, TestResult,
    };
    use crate::test_support;

    fn response(code: &str, stdout: &str, stderr: &str) -> FixtureOutcome {
        FixtureOutcome::Return {
            value: FixtureValue::Map(BTreeMap::from([
                ("exit_code".into(), FixtureValue::String(code.into())),
                ("stdout".into(), FixtureValue::String(stdout.into())),
                ("stderr".into(), FixtureValue::String(stderr.into())),
            ])),
        }
    }

    fn plan(script: Vec<FixtureStep>) -> FixturePlan {
        FixturePlan {
            filesystem: Some(FilesystemFixture {
                entries: BTreeMap::from([(
                    "tool".into(),
                    FilesystemEntry::File {
                        hex: "66697874757265".into(),
                    },
                )]),
                rights: vec!["Read".into()],
                entry_policy: None,
                script: Vec::new(),
            }),
            exec: Some(ExecFixture {
                tools: vec!["tool".into()],
                script,
            }),
            ..Default::default()
        }
    }

    fn run_step(outcome: FixtureOutcome) -> FixtureStep {
        test_support::step("exec_run", Some("tool"), BTreeMap::from([
                (
                    "args".into(),
                    FixtureValue::List(vec![FixtureValue::String("--check".into())]),
                ),
                ("stdin".into(), FixtureValue::String("input".into())),
            ]), Some(vec!["exec:tool".into(), "dir:Read".into()]), outcome)
    }

    #[test]
    fn scripted_process_result_never_spawns_a_host_process() {
        let mut session =
            FixtureSession::new(plan(vec![run_step(response("7", "out", "err"))]))
                .expect("session");
        let dir = session.mint_fixture_dir(None).expect("dir");
        let exec = session.mint_fixture_exec(None).expect("exec");
        let result = session
            .exec_run(
                &exec,
                &dir,
                "tool",
                &["--check".into()],
                "input",
                None,
            )
            .expect("scripted result");
        assert_eq!(result.exit_code, 7);
        assert_eq!(result.stdout, "out");
        assert_eq!(result.stderr, "err");
        assert_eq!(session.finish(TestResult::Passed).events.len(), 3);
    }

    #[test]
    fn allowlist_dir_policy_and_argument_injection_fail_before_script() {
        let mut fixture = plan(vec![run_step(response("0", "ok", ""))]);
        fixture
            .filesystem
            .as_mut()
            .expect("filesystem")
            .entry_policy = Some("ext:.allowed".into());
        let mut session = FixtureSession::new(fixture).expect("session");
        let dir = session.mint_fixture_dir(None).expect("dir");
        let exec = session.mint_fixture_exec(None).expect("exec");
        assert_eq!(
            session
                .exec_run(&exec, &dir, "other", &[], "", None)
                .expect_err("allowlist")
                .code,
            FixtureErrorCode::PermissionDenied
        );
        assert_eq!(
            session
                .exec_run(&exec, &dir, "tool", &["bad\0arg".into()], "", None)
                .expect_err("NUL")
                .code,
            FixtureErrorCode::InvalidRequest
        );
        assert_eq!(
            session
                .exec_run(&exec, &dir, "tool", &[], "", None)
                .expect_err("Dir policy")
                .code,
            FixtureErrorCode::PermissionDenied
        );
    }

    #[test]
    fn narrowing_and_foreign_handles_cannot_widen_authority() {
        let fixture = plan(Vec::new());
        let mut first = FixtureSession::new(fixture.clone()).expect("first");
        let mut second = FixtureSession::new(fixture).expect("second");
        let exec = first.mint_fixture_exec(None).expect("exec");
        let none = first.exec_only(&exec, &[], None).expect("narrowed");
        assert_eq!(
            first
                .exec_only(&none, &["tool".into()], None)
                .expect_err("widen")
                .code,
            FixtureErrorCode::PermissionDenied
        );
        assert_eq!(
            second
                .exec_only(&exec, &[], None)
                .expect_err("foreign")
                .code,
            FixtureErrorCode::PermissionDenied
        );
    }

    #[test]
    fn timeout_and_spawn_failure_are_scripted_outcomes() {
        for code in [FixtureErrorCode::Timeout, FixtureErrorCode::SpawnFailed] {
            let mut session = FixtureSession::new(plan(vec![run_step(
                FixtureOutcome::Fail {
                    error: exec_failure(code, "configured process failure"),
                },
            )]))
            .expect("session");
            let dir = session.mint_fixture_dir(None).expect("dir");
            let exec = session.mint_fixture_exec(None).expect("exec");
            assert_eq!(
                session
                    .exec_run(
                        &exec,
                        &dir,
                        "tool",
                        &["--check".into()],
                        "input",
                        None,
                    )
                    .expect_err("failure")
                    .code,
                code
            );
        }
    }
}
