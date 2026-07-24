use std::collections::{BTreeMap, VecDeque};

use crate::basic::BasicProviderState;
use crate::exec::ExecProviderState;
use crate::filesystem::FilesystemProviderState;
use crate::fetch::FetchProviderState;
use crate::secret::SecretProviderState;
use crate::{
    Expectations, FixtureErrorCode, FixtureFailure, FixtureFamily, FixtureOutcome, FixturePlan,
    FixtureStep, FixtureValue, PlanValidationError, SourceLocation, TEST_TRANSCRIPT_VERSION,
    TestEvent, TestResult, TestTranscript, U64Text,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureCall {
    pub family: FixtureFamily,
    pub operation: String,
    pub target: Option<String>,
    pub arguments: BTreeMap<String, FixtureValue>,
    pub effective_rights: Vec<String>,
    pub source: Option<SourceLocation>,
}

impl FixtureCall {
    pub fn new(family: FixtureFamily, operation: impl Into<String>) -> Self {
        Self {
            family,
            operation: operation.into(),
            target: None,
            arguments: BTreeMap::new(),
            effective_rights: Vec::new(),
            source: None,
        }
    }
}

#[derive(Debug)]
pub struct FixtureSession {
    scripts: BTreeMap<FixtureFamily, VecDeque<FixtureStep>>,
    expectations: Expectations,
    next_ordered_call: usize,
    calls: BTreeMap<(FixtureFamily, String), u64>,
    events: Vec<TestEvent>,
    stdout: Vec<String>,
    stderr: Vec<String>,
    seed: Option<U64Text>,
    max_events: usize,
    pub(crate) basic: BasicProviderState,
    pub(crate) exec: ExecProviderState,
    pub(crate) filesystem: FilesystemProviderState,
    pub(crate) fetch: FetchProviderState,
    pub(crate) secrets: SecretProviderState,
}

impl FixtureSession {
    pub fn new(mut plan: FixturePlan) -> Result<Self, PlanValidationError> {
        let limits = crate::PlanValidationLimits::default();
        plan.validate_with(&limits)?;
        let basic = BasicProviderState::new(&plan);
        let exec = ExecProviderState::new(&plan);
        let filesystem = FilesystemProviderState::new(&plan);
        let fetch = FetchProviderState::new(&plan);
        let secrets = SecretProviderState::new(&plan);

        let mut scripts = BTreeMap::new();
        macro_rules! take_script {
            ($field:ident, $family:expr) => {
                if let Some(fixture) = &mut plan.$field {
                    if !fixture.script.is_empty() {
                        scripts.insert($family, std::mem::take(&mut fixture.script).into());
                    }
                }
            };
        }
        take_script!(console, FixtureFamily::Console);
        take_script!(clock, FixtureFamily::Clock);
        take_script!(rand, FixtureFamily::Rand);
        take_script!(env, FixtureFamily::Env);
        take_script!(filesystem, FixtureFamily::Filesystem);
        take_script!(fetch, FixtureFamily::Fetch);
        take_script!(secrets, FixtureFamily::SecretStore);
        take_script!(exec, FixtureFamily::Exec);
        take_script!(vm, FixtureFamily::Vm);

        let seed = plan.rand.as_ref().and_then(|fixture| fixture.seed.clone());
        Ok(Self {
            scripts,
            expectations: plan.expectations,
            next_ordered_call: 0,
            calls: BTreeMap::new(),
            events: Vec::new(),
            stdout: Vec::new(),
            stderr: Vec::new(),
            seed,
            max_events: limits.max_script_steps,
            basic,
            exec,
            filesystem,
            fetch,
            secrets,
        })
    }

    pub fn scripted_call(&mut self, call: FixtureCall) -> FixtureOutcome {
        if let Some(message) = self.call_contract_mismatch(&call) {
            return self.record_failure(call, FixtureErrorCode::UnexpectedCall, message);
        }

        let Some(step) = self
            .scripts
            .get(&call.family)
            .and_then(|steps| steps.front())
            .cloned()
        else {
            return self.record_failure(
                call,
                FixtureErrorCode::Exhausted,
                "fixture script is exhausted",
            );
        };

        if step.operation != call.operation {
            let message = format!(
                "expected operation `{}`, received `{}`",
                step.operation, call.operation
            );
            return self.record_failure(
                call,
                FixtureErrorCode::UnexpectedCall,
                message,
            );
        }
        if step.target != call.target {
            let message = format!(
                "expected target {}, received {}",
                display_optional(&step.target),
                display_optional(&call.target)
            );
            return self.record_failure(
                call,
                FixtureErrorCode::UnexpectedCall,
                message,
            );
        }
        if step.arguments != call.arguments {
            return self.record_failure(
                call,
                FixtureErrorCode::UnexpectedCall,
                "fixture call arguments do not match the next script step",
            );
        }
        if let Some(expected) = &step.effective_rights
            && expected != &call.effective_rights
        {
            let message = format!(
                "expected effective rights {expected:?}, received {:?}",
                call.effective_rights
            );
            return self.record_failure(
                call,
                FixtureErrorCode::UnexpectedCall,
                message,
            );
        }

        let Some(step) = self
            .scripts
            .get_mut(&call.family)
            .and_then(VecDeque::pop_front)
        else {
            return self.record_failure(
                call,
                FixtureErrorCode::ProviderFailure,
                "fixture script changed while dispatching",
            );
        };
        self.record(call, step.outcome)
    }

    pub fn observe(&mut self, call: FixtureCall, outcome: FixtureOutcome) -> FixtureOutcome {
        if let Some(message) = self.call_contract_mismatch(&call) {
            return self.record_failure(call, FixtureErrorCode::UnexpectedCall, message);
        }
        self.record(call, outcome)
    }

    pub fn capture_stdout(&mut self, value: impl Into<String>) {
        self.stdout.push(value.into());
    }

    pub fn capture_stderr(&mut self, value: impl Into<String>) {
        self.stderr.push(value.into());
    }

    pub fn finish(self, result: TestResult) -> TestTranscript {
        let assertion_failure = self.completion_failure();
        let result = assertion_failure.map_or(result, |message| TestResult::Failed { message });
        TestTranscript {
            version: TEST_TRANSCRIPT_VERSION,
            seed: self.seed,
            events: self.events,
            stdout: self.stdout,
            stderr: self.stderr,
            result,
        }
    }

    fn call_contract_mismatch(&self, call: &FixtureCall) -> Option<String> {
        if self.expectations.absent_families.contains(&call.family) {
            return Some(format!(
                "fixture family `{:?}` was declared absent",
                call.family
            ));
        }
        let expected = self
            .expectations
            .ordered_calls
            .get(self.next_ordered_call)?;
        if expected.family != call.family
            || expected.operation != call.operation
            || expected.target != call.target
            || expected
                .effective_rights
                .as_ref()
                .is_some_and(|rights| rights != &call.effective_rights)
        {
            return Some(format!(
                "ordered call {} expected {:?}.{} target {}, received {:?}.{} target {}",
                self.next_ordered_call,
                expected.family,
                expected.operation,
                display_optional(&expected.target),
                call.family,
                call.operation,
                display_optional(&call.target)
            ));
        }
        None
    }

    pub(crate) fn has_script(&self, family: FixtureFamily) -> bool {
        self.scripts.contains_key(&family)
    }

    pub(crate) fn record_failure(
        &mut self,
        call: FixtureCall,
        code: FixtureErrorCode,
        message: impl Into<String>,
    ) -> FixtureOutcome {
        self.record(
            call,
            FixtureOutcome::Fail {
                error: FixtureFailure {
                    code,
                    message: message.into(),
                },
            },
        )
    }

    pub(crate) fn record(
        &mut self,
        call: FixtureCall,
        outcome: FixtureOutcome,
    ) -> FixtureOutcome {
        let count = self
            .calls
            .entry((call.family, call.operation.clone()))
            .or_default();
        *count = count.saturating_add(1);

        if self
            .expectations
            .ordered_calls
            .get(self.next_ordered_call)
            .is_some_and(|expected| {
                expected.family == call.family
                    && expected.operation == call.operation
                    && expected.target == call.target
                    && expected
                        .effective_rights
                        .as_ref()
                        .is_none_or(|rights| rights == &call.effective_rights)
            })
        {
            self.next_ordered_call = self.next_ordered_call.saturating_add(1);
        }

        if self.events.len() < self.max_events {
            self.events.push(TestEvent {
                sequence: U64Text::new(self.events.len() as u64),
                family: call.family,
                operation: call.operation,
                target: call.target,
                arguments: call.arguments,
                effective_rights: call.effective_rights,
                outcome: outcome.clone(),
                source: call.source,
            });
            outcome
        } else {
            FixtureOutcome::Fail {
                error: FixtureFailure {
                    code: FixtureErrorCode::ProviderFailure,
                    message: format!("fixture event limit {} exceeded", self.max_events),
                },
            }
        }
    }

    fn completion_failure(&self) -> Option<String> {
        if self.next_ordered_call < self.expectations.ordered_calls.len() {
            return Some(format!(
                "ordered call {} was not observed",
                self.next_ordered_call
            ));
        }

        if self.expectations.require_complete_scripts {
            for (family, steps) in &self.scripts {
                if let Some(step) = steps.iter().find(|step| step.required) {
                    return Some(format!(
                        "required {:?}.{} fixture step was not consumed",
                        family, step.operation
                    ));
                }
            }
        }

        for expectation in &self.expectations.calls {
            let actual = self
                .calls
                .get(&(expectation.family, expectation.operation.clone()))
                .copied()
                .unwrap_or(0);
            if expectation
                .minimum
                .as_ref()
                .is_some_and(|minimum| actual < minimum.get())
            {
                return Some(format!(
                    "{:?}.{} was called {actual} times, below minimum {}",
                    expectation.family,
                    expectation.operation,
                    expectation.minimum.as_ref().map_or(0, U64Text::get)
                ));
            }
            if expectation
                .maximum
                .as_ref()
                .is_some_and(|maximum| actual > maximum.get())
            {
                return Some(format!(
                    "{:?}.{} was called {actual} times, above maximum {}",
                    expectation.family,
                    expectation.operation,
                    expectation.maximum.as_ref().map_or(0, U64Text::get)
                ));
            }
        }
        None
    }
}

fn display_optional(value: &Option<String>) -> String {
    value
        .as_ref()
        .map_or_else(|| "<none>".to_owned(), |value| format!("`{value}`"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CallExpectation, ConsoleFixture, FixtureFailure, OrderedCallExpectation, SourceLocation,
    };

    fn returned(value: &str) -> FixtureOutcome {
        FixtureOutcome::Return {
            value: FixtureValue::String(value.into()),
        }
    }

    fn console_step(operation: &str, value: &str) -> FixtureStep {
        FixtureStep {
            operation: operation.into(),
            target: None,
            arguments: BTreeMap::new(),
            effective_rights: Some(vec!["Read".into()]),
            outcome: returned(value),
            required: true,
        }
    }

    #[test]
    fn ordered_calls_record_exact_provenance_and_outcome() {
        let plan = FixturePlan {
            console: Some(ConsoleFixture {
                script: vec![console_step("read", "line")],
            }),
            expectations: Expectations {
                require_complete_scripts: true,
                ordered_calls: vec![OrderedCallExpectation {
                    family: FixtureFamily::Console,
                    operation: "read".into(),
                    target: None,
                    effective_rights: Some(vec!["Read".into()]),
                }],
                calls: vec![CallExpectation {
                    family: FixtureFamily::Console,
                    operation: "read".into(),
                    minimum: Some(U64Text::new(1)),
                    maximum: Some(U64Text::new(1)),
                }],
                absent_families: Vec::new(),
            },
            ..Default::default()
        };
        let mut session = FixtureSession::new(plan).expect("valid session");
        let mut call = FixtureCall::new(FixtureFamily::Console, "read");
        call.effective_rights = vec!["Read".into()];
        call.source = Some(SourceLocation {
            module: "main".into(),
            line: U64Text::new(4),
            column: U64Text::new(8),
        });
        assert_eq!(session.scripted_call(call), returned("line"));

        let transcript = session.finish(TestResult::Passed);
        assert_eq!(transcript.result, TestResult::Passed);
        assert_eq!(transcript.events.len(), 1);
        assert_eq!(
            transcript.events[0].source.as_ref().map(|source| source.line.get()),
            Some(4)
        );
    }

    #[test]
    fn mismatch_does_not_consume_the_expected_step() {
        let plan = FixturePlan {
            console: Some(ConsoleFixture {
                script: vec![console_step("read", "line")],
            }),
            expectations: Expectations {
                require_complete_scripts: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut session = FixtureSession::new(plan).expect("valid session");
        let outcome =
            session.scripted_call(FixtureCall::new(FixtureFamily::Console, "write"));
        assert!(matches!(
            outcome,
            FixtureOutcome::Fail {
                error: FixtureFailure {
                    code: FixtureErrorCode::UnexpectedCall,
                    ..
                }
            }
        ));
        let transcript = session.finish(TestResult::Passed);
        assert!(matches!(transcript.result, TestResult::Failed { .. }));
    }

    #[test]
    fn exhausted_script_never_falls_back_to_fake_behavior() {
        let plan = FixturePlan {
            console: Some(ConsoleFixture {
                script: vec![console_step("read", "line")],
            }),
            ..Default::default()
        };
        let mut session = FixtureSession::new(plan).expect("valid session");
        let mut first = FixtureCall::new(FixtureFamily::Console, "read");
        first.effective_rights = vec!["Read".into()];
        assert_eq!(session.scripted_call(first), returned("line"));

        let mut exhausted = FixtureCall::new(FixtureFamily::Console, "read");
        exhausted.effective_rights = vec!["Read".into()];
        assert!(matches!(
            session.scripted_call(exhausted),
            FixtureOutcome::Fail {
                error: FixtureFailure {
                    code: FixtureErrorCode::Exhausted,
                    ..
                }
            }
        ));
    }

    #[test]
    fn absent_family_fails_at_the_call_site() {
        let plan = FixturePlan {
            expectations: Expectations {
                absent_families: vec![FixtureFamily::Exec],
                ..Default::default()
            },
            ..Default::default()
        };
        let mut session = FixtureSession::new(plan).expect("valid session");
        let outcome = session.observe(
            FixtureCall::new(FixtureFamily::Exec, "run"),
            returned("should not escape"),
        );
        assert!(matches!(
            outcome,
            FixtureOutcome::Fail {
                error: FixtureFailure {
                    code: FixtureErrorCode::UnexpectedCall,
                    ..
                }
            }
        ));
    }

    #[test]
    fn count_bounds_are_checked_at_completion() {
        let plan = FixturePlan {
            expectations: Expectations {
                calls: vec![CallExpectation {
                    family: FixtureFamily::Fetch,
                    operation: "send".into(),
                    minimum: Some(U64Text::new(1)),
                    maximum: None,
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        let session = FixtureSession::new(plan).expect("valid session");
        let transcript = session.finish(TestResult::Passed);
        assert_eq!(
            transcript.result,
            TestResult::Failed {
                message: "Fetch.send was called 0 times, below minimum 1".into()
            }
        );
    }
}
