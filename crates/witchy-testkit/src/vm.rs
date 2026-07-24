use std::collections::BTreeMap;

use crate::{
    FixtureCall, FixtureErrorCode, FixtureFailure, FixtureFamily, FixtureOutcome, FixturePlan,
    FixtureSession, FixtureValue, SourceLocation, TestResult, TestTranscript,
};

pub type VmProviderResult<T> = Result<T, FixtureFailure>;

#[derive(Debug)]
pub(crate) struct VmProviderState {
    configured: bool,
    children: BTreeMap<String, Box<FixturePlan>>,
}

impl VmProviderState {
    pub(crate) fn new(plan: &FixturePlan) -> Self {
        let Some(fixture) = &plan.vm else {
            return Self {
                configured: false,
                children: BTreeMap::new(),
            };
        };
        Self {
            configured: true,
            children: fixture.children.clone(),
        }
    }

    pub(crate) const fn configured(&self) -> bool {
        self.configured
    }
}

impl FixtureSession {
    pub fn vm_spawn<F>(
        &mut self,
        module: &str,
        arguments: &[String],
        source: Option<SourceLocation>,
        run: F,
    ) -> VmProviderResult<TestTranscript>
    where
        F: FnOnce(&mut FixtureSession) -> TestResult,
    {
        if !self.vm.configured {
            return Err(vm_failure(
                FixtureErrorCode::PermissionDenied,
                "VM fixture was not declared",
            ));
        }
        let child_plan = self
            .vm
            .children
            .get(module)
            .cloned()
            .ok_or_else(|| {
                vm_failure(
                    FixtureErrorCode::NotFound,
                    format!("VM child fixture `{module}` was not declared"),
                )
            })?;
        let mut call = FixtureCall::new(FixtureFamily::Vm, "vm_spawn");
        call.target = Some(module.into());
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
        call.source = source;
        let outcome = if self.has_script(FixtureFamily::Vm) {
            self.scripted_call(call)
        } else {
            self.observe(
                call,
                FixtureOutcome::Return {
                    value: FixtureValue::String("VM".into()),
                },
            )
        };
        vm_marker(outcome)?;

        let mut child = FixtureSession::new(*child_plan).map_err(|error| {
            vm_failure(
                FixtureErrorCode::InvalidData,
                format!("invalid VM child fixture `{module}`: {error}"),
            )
        })?;
        let result = run(&mut child);
        let transcript = child.finish(result);
        self.attach_child_transcript(transcript.clone())?;
        Ok(transcript)
    }
}

fn vm_marker(outcome: FixtureOutcome) -> VmProviderResult<()> {
    match outcome {
        FixtureOutcome::Return {
            value: FixtureValue::String(value),
        } if value == "VM" => Ok(()),
        FixtureOutcome::Fail { error } => Err(error),
        _ => Err(vm_failure(
            FixtureErrorCode::InvalidData,
            "vm_spawn returned an invalid VM marker",
        )),
    }
}

fn vm_failure(code: FixtureErrorCode, message: impl Into<String>) -> FixtureFailure {
    FixtureFailure {
        code,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::{
        EnvFixture, FixtureStep, VmFixture,
    };

    fn child_plan() -> FixturePlan {
        FixturePlan {
            env: Some(EnvFixture {
                values: BTreeMap::from([("MODE".into(), "child".into())]),
                allow: vec!["MODE".into()],
                script: Vec::new(),
            }),
            argv: Some(vec!["child-arg".into()]),
            ..Default::default()
        }
    }

    fn parent_plan(script: Vec<FixtureStep>) -> FixturePlan {
        FixturePlan {
            vm: Some(VmFixture {
                children: BTreeMap::from([("worker".into(), Box::new(child_plan()))]),
                script,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn child_runs_sequentially_with_exact_plan_and_nested_transcript() {
        let mut parent = FixtureSession::new(parent_plan(Vec::new())).expect("parent");
        let child = parent
            .vm_spawn("worker", &["request".into()], None, |session| {
                assert!(session.has_fixture(FixtureFamily::Env));
                assert!(!session.has_fixture(FixtureFamily::Console));
                assert_eq!(session.argv(None).expect("child argv"), vec!["child-arg"]);
                TestResult::Passed
            })
            .expect("child");
        assert_eq!(child.result, TestResult::Passed);
        assert_eq!(child.events.len(), 1);

        let parent = parent.finish(TestResult::Passed);
        assert_eq!(parent.events.len(), 1);
        assert_eq!(
            parent.events[0]
                .child
                .as_ref()
                .map(|transcript| &transcript.result),
            Some(&TestResult::Passed)
        );
    }

    #[test]
    fn scripted_failure_does_not_invoke_child() {
        let step = FixtureStep {
            operation: "vm_spawn".into(),
            target: Some("worker".into()),
            arguments: BTreeMap::from([(
                "args".into(),
                FixtureValue::List(Vec::new()),
            )]),
            effective_rights: None,
            outcome: FixtureOutcome::Fail {
                error: vm_failure(FixtureErrorCode::Timeout, "configured VM timeout"),
            },
            required: true,
        };
        let mut parent = FixtureSession::new(parent_plan(vec![step])).expect("parent");
        let invoked = Cell::new(false);
        assert_eq!(
            parent
                .vm_spawn("worker", &[], None, |_| {
                    invoked.set(true);
                    TestResult::Passed
                })
                .expect_err("timeout")
                .code,
            FixtureErrorCode::Timeout
        );
        assert!(!invoked.get());
    }

    #[test]
    fn missing_child_and_parent_authority_inheritance_fail_closed() {
        let mut parent = FixtureSession::new(parent_plan(Vec::new())).expect("parent");
        assert_eq!(
            parent
                .vm_spawn("missing", &[], None, |_| TestResult::Passed)
                .expect_err("missing")
                .code,
            FixtureErrorCode::NotFound
        );
        parent
            .vm_spawn("worker", &[], None, |child| {
                assert!(!child.has_fixture(FixtureFamily::Vm));
                assert!(!child.has_fixture(FixtureFamily::Exec));
                TestResult::Passed
            })
            .expect("declared child");
    }
}
