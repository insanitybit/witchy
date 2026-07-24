use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{
    FixtureCall, FixtureErrorCode, FixtureFailure, FixtureFamily, FixtureOutcome, FixturePlan,
    FixtureSession, FixtureValue, SourceLocation,
};

pub type ProviderResult<T> = Result<T, FixtureFailure>;

static NEXT_SESSION_BRAND: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureHandle {
    brand: u64,
    id: u64,
    family: FixtureFamily,
}

#[derive(Debug)]
pub(crate) struct BasicProviderState {
    brand: u64,
    next_handle: u64,
    console: bool,
    clock_next: Option<u64>,
    clock_step: u64,
    rand_state: Option<u64>,
    env_values: Option<BTreeMap<String, String>>,
    env_root_allow: BTreeSet<String>,
    env_handles: BTreeMap<u64, BTreeSet<String>>,
    argv: Option<Vec<String>>,
}

impl BasicProviderState {
    pub(crate) fn new(plan: &FixturePlan) -> Self {
        let env_values = plan.env.as_ref().map(|fixture| fixture.values.clone());
        let env_root_allow = plan.env.as_ref().map_or_else(BTreeSet::new, |fixture| {
            if fixture.allow.is_empty() {
                fixture.values.keys().cloned().collect()
            } else {
                fixture.allow.iter().cloned().collect()
            }
        });
        Self {
            brand: NEXT_SESSION_BRAND.fetch_add(1, Ordering::Relaxed),
            next_handle: 0,
            console: plan.console.is_some(),
            clock_next: plan
                .clock
                .as_ref()
                .map(|fixture| fixture.start_ns.as_ref().map_or(0, crate::U64Text::get)),
            clock_step: plan
                .clock
                .as_ref()
                .and_then(|fixture| fixture.step_ns.as_ref())
                .map_or(0, crate::U64Text::get),
            rand_state: plan
                .rand
                .as_ref()
                .map(|fixture| fixture.seed.as_ref().map_or(0, crate::U64Text::get)),
            env_values,
            env_root_allow,
            env_handles: BTreeMap::new(),
            argv: plan.argv.clone(),
        }
    }

    pub(crate) fn mint_handle(
        &mut self,
        family: FixtureFamily,
        rights: BTreeSet<String>,
    ) -> FixtureHandle {
        let id = self.next_handle;
        self.next_handle = self.next_handle.saturating_add(1);
        if family == FixtureFamily::Env {
            self.env_handles.insert(id, rights);
        }
        FixtureHandle {
            brand: self.brand,
            id,
            family,
        }
    }

    pub(crate) fn validate_handle(
        &self,
        handle: &FixtureHandle,
        family: FixtureFamily,
    ) -> bool {
        handle.brand == self.brand && handle.family == family && handle.id < self.next_handle
    }

    fn validate_env_handle(&self, handle: &FixtureHandle) -> ProviderResult<&BTreeSet<String>> {
        if handle.brand != self.brand
            || handle.family != FixtureFamily::Env
            || !self.env_handles.contains_key(&handle.id)
        {
            return Err(failure(
                FixtureErrorCode::PermissionDenied,
                "invalid or foreign Env fixture handle",
            ));
        }
        self.env_handles.get(&handle.id).ok_or_else(|| {
            failure(
                FixtureErrorCode::PermissionDenied,
                "invalid or foreign Env fixture handle",
            )
        })
    }
}

impl FixtureHandle {
    pub(crate) const fn id(&self) -> u64 {
        self.id
    }
}

impl FixtureSession {
    pub fn has_fixture(&self, family: FixtureFamily) -> bool {
        match family {
            FixtureFamily::Console => self.basic.console,
            FixtureFamily::Clock => self.basic.clock_next.is_some(),
            FixtureFamily::Rand => self.basic.rand_state.is_some(),
            FixtureFamily::Env => self.basic.env_values.is_some(),
            FixtureFamily::Argv => self.basic.argv.is_some(),
            FixtureFamily::Fetch => self.fetch.configured(),
            FixtureFamily::SecretStore => self.secrets.configured(),
            FixtureFamily::Exec => self.exec.configured(),
            FixtureFamily::Filesystem => self.filesystem.configured(),
        }
    }

    pub fn console_read(&mut self, source: Option<SourceLocation>) -> ProviderResult<String> {
        if !self.basic.console {
            return Err(failure(
                FixtureErrorCode::PermissionDenied,
                "Console fixture was not declared",
            ));
        }
        let mut call = FixtureCall::new(FixtureFamily::Console, "console_read_len");
        call.effective_rights.push("Read".into());
        call.source = source;
        outcome_string(self.scripted_call(call), "Console read")
    }

    pub fn console_write(
        &mut self,
        text: impl Into<String>,
        source: Option<SourceLocation>,
    ) -> ProviderResult<()> {
        if !self.basic.console {
            return Err(failure(
                FixtureErrorCode::PermissionDenied,
                "Console fixture was not declared",
            ));
        }
        let text = text.into();
        let mut call = FixtureCall::new(FixtureFamily::Console, "print");
        call.arguments
            .insert("text".into(), FixtureValue::String(text.clone()));
        call.effective_rights.push("Write".into());
        call.source = source;
        let outcome = if self.has_script(FixtureFamily::Console) {
            self.scripted_call(call)
        } else {
            self.observe(
                call,
                FixtureOutcome::Return {
                    value: FixtureValue::Null,
                },
            )
        };
        outcome_unit(outcome, "Console write")?;
        self.capture_stdout(text);
        Ok(())
    }

    pub fn clock_now(&mut self, source: Option<SourceLocation>) -> ProviderResult<u64> {
        let Some(next) = self.basic.clock_next else {
            return Err(failure(
                FixtureErrorCode::PermissionDenied,
                "Clock fixture was not declared",
            ));
        };
        let mut call = FixtureCall::new(FixtureFamily::Clock, "now");
        call.source = source;
        if self.has_script(FixtureFamily::Clock) {
            return outcome_u64(self.scripted_call(call), "Clock now");
        }
        self.basic.clock_next = Some(next.wrapping_add(self.basic.clock_step));
        let outcome = self.observe(
            call,
            FixtureOutcome::Return {
                value: FixtureValue::String(next.to_string()),
            },
        );
        outcome_u64(outcome, "Clock now")
    }

    pub fn rand_u64(&mut self, source: Option<SourceLocation>) -> ProviderResult<u64> {
        let Some(mut state) = self.basic.rand_state else {
            return Err(failure(
                FixtureErrorCode::PermissionDenied,
                "Rand fixture was not declared",
            ));
        };
        let mut call = FixtureCall::new(FixtureFamily::Rand, "rand_u64");
        call.source = source;
        if self.has_script(FixtureFamily::Rand) {
            return outcome_u64(self.scripted_call(call), "Rand draw");
        }
        let value = seeded_next(&mut state);
        self.basic.rand_state = Some(state);
        let outcome = self.observe(
            call,
            FixtureOutcome::Return {
                value: FixtureValue::String(value.to_string()),
            },
        );
        outcome_u64(outcome, "Rand draw")
    }

    pub fn mint_env(
        &mut self,
        source: Option<SourceLocation>,
    ) -> ProviderResult<FixtureHandle> {
        if self.basic.env_values.is_none() {
            return Err(failure(
                FixtureErrorCode::PermissionDenied,
                "Env fixture was not declared",
            ));
        }
        let mut call = FixtureCall::new(FixtureFamily::Env, "mint_env");
        call.source = source;
        let outcome = if self.has_script(FixtureFamily::Env) {
            self.scripted_call(call)
        } else {
            self.observe(
                call,
                FixtureOutcome::Return {
                    value: FixtureValue::String("Env".into()),
                },
            )
        };
        outcome_handle(outcome, "Env mint")?;
        let rights = self.basic.env_root_allow.clone();
        Ok(self.basic.mint_handle(FixtureFamily::Env, rights))
    }

    pub fn env_only(
        &mut self,
        handle: &FixtureHandle,
        names: &[String],
        source: Option<SourceLocation>,
    ) -> ProviderResult<FixtureHandle> {
        let parent = match self.basic.validate_env_handle(handle) {
            Ok(parent) => parent.clone(),
            Err(error) => {
                let mut call = FixtureCall::new(FixtureFamily::Env, "env_only");
                call.source = source;
                return outcome_handle(
                    self.observe(call, FixtureOutcome::Fail { error }),
                    "Env only",
                )
                .and_then(|()| {
                    Err(failure(
                        FixtureErrorCode::ProviderFailure,
                        "invalid Env narrowing outcome",
                    ))
                });
            }
        };
        let requested: BTreeSet<String> = names.iter().cloned().collect();
        if !requested.is_subset(&parent) {
            let mut call = env_only_call(names, source.clone());
            call.effective_rights = parent.iter().cloned().collect();
            let outcome = self.record_failure(
                call,
                FixtureErrorCode::PermissionDenied,
                "Env.only cannot widen its allow-list",
            );
            outcome_handle(outcome, "Env only")?;
        }
        let mut call = env_only_call(names, source);
        call.effective_rights = parent.iter().cloned().collect();
        let outcome = if self.has_script(FixtureFamily::Env) {
            self.scripted_call(call)
        } else {
            self.observe(
                call,
                FixtureOutcome::Return {
                    value: FixtureValue::String("Env".into()),
                },
            )
        };
        outcome_handle(outcome, "Env only")?;
        Ok(self.basic.mint_handle(FixtureFamily::Env, requested))
    }

    pub fn env_get(
        &mut self,
        handle: &FixtureHandle,
        name: &str,
        source: Option<SourceLocation>,
    ) -> ProviderResult<Option<String>> {
        let allowed = match self.basic.validate_env_handle(handle) {
            Ok(allowed) => allowed.clone(),
            Err(error) => {
                let call = env_get_call(name, source);
                let outcome = self.observe(call, FixtureOutcome::Fail { error });
                return outcome_optional_string(outcome, "Env read");
            }
        };
        let mut call = env_get_call(name, source);
        call.effective_rights = allowed.iter().cloned().collect();
        if !allowed.contains(name) {
            let outcome = self.record_failure(
                call,
                FixtureErrorCode::PermissionDenied,
                format!("environment name `{name}` is not allowed"),
            );
            return outcome_optional_string(outcome, "Env read");
        }
        if self.has_script(FixtureFamily::Env) {
            return outcome_optional_string(self.scripted_call(call), "Env read");
        }
        let value = self
            .basic
            .env_values
            .as_ref()
            .and_then(|values| values.get(name))
            .cloned()
            .map_or(FixtureValue::Null, FixtureValue::String);
        outcome_optional_string(
            self.observe(call, FixtureOutcome::Return { value }),
            "Env read",
        )
    }

    pub fn argv(&mut self, source: Option<SourceLocation>) -> ProviderResult<Vec<String>> {
        let Some(arguments) = self.basic.argv.clone() else {
            return Err(failure(
                FixtureErrorCode::PermissionDenied,
                "argv fixture was not declared",
            ));
        };
        let mut call = FixtureCall::new(FixtureFamily::Argv, "args");
        call.source = source;
        let value = FixtureValue::List(
            arguments
                .iter()
                .cloned()
                .map(FixtureValue::String)
                .collect(),
        );
        match self.observe(call, FixtureOutcome::Return { value }) {
            FixtureOutcome::Return {
                value: FixtureValue::List(_),
            } => Ok(arguments),
            FixtureOutcome::Fail { error } => Err(error),
            _ => Err(failure(
                FixtureErrorCode::InvalidData,
                "argv fixture returned an invalid value",
            )),
        }
    }
}

fn env_only_call(names: &[String], source: Option<SourceLocation>) -> FixtureCall {
    let mut call = FixtureCall::new(FixtureFamily::Env, "env_only");
    call.arguments.insert(
        "names".into(),
        FixtureValue::List(names.iter().cloned().map(FixtureValue::String).collect()),
    );
    call.source = source;
    call
}

fn env_get_call(name: &str, source: Option<SourceLocation>) -> FixtureCall {
    let mut call = FixtureCall::new(FixtureFamily::Env, "env_len");
    call.target = Some(name.into());
    call.source = source;
    call
}

fn outcome_string(outcome: FixtureOutcome, operation: &str) -> ProviderResult<String> {
    match outcome {
        FixtureOutcome::Return {
            value: FixtureValue::String(value),
        } => Ok(value),
        FixtureOutcome::Fail { error } => Err(error),
        _ => Err(failure(
            FixtureErrorCode::InvalidData,
            format!("{operation} fixture returned an invalid value"),
        )),
    }
}

fn outcome_optional_string(
    outcome: FixtureOutcome,
    operation: &str,
) -> ProviderResult<Option<String>> {
    match outcome {
        FixtureOutcome::Return {
            value: FixtureValue::String(value),
        } => Ok(Some(value)),
        FixtureOutcome::Return {
            value: FixtureValue::Null,
        } => Ok(None),
        FixtureOutcome::Fail { error } => Err(error),
        _ => Err(failure(
            FixtureErrorCode::InvalidData,
            format!("{operation} fixture returned an invalid value"),
        )),
    }
}

fn outcome_u64(outcome: FixtureOutcome, operation: &str) -> ProviderResult<u64> {
    let text = outcome_string(outcome, operation)?;
    text.parse::<u64>().map_err(|_| {
        failure(
            FixtureErrorCode::InvalidData,
            format!("{operation} fixture returned a non-u64 value"),
        )
    })
}

fn outcome_unit(outcome: FixtureOutcome, operation: &str) -> ProviderResult<()> {
    match outcome {
        FixtureOutcome::Return {
            value: FixtureValue::Null,
        } => Ok(()),
        FixtureOutcome::Fail { error } => Err(error),
        _ => Err(failure(
            FixtureErrorCode::InvalidData,
            format!("{operation} fixture returned an invalid value"),
        )),
    }
}

fn outcome_handle(outcome: FixtureOutcome, operation: &str) -> ProviderResult<()> {
    match outcome {
        FixtureOutcome::Return {
            value: FixtureValue::String(kind),
        } if kind == "Env" => Ok(()),
        FixtureOutcome::Fail { error } => Err(error),
        _ => Err(failure(
            FixtureErrorCode::InvalidData,
            format!("{operation} fixture returned an invalid handle marker"),
        )),
    }
}

fn failure(code: FixtureErrorCode, message: impl Into<String>) -> FixtureFailure {
    FixtureFailure {
        code,
        message: message.into(),
    }
}

fn seeded_next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut value = *state;
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ClockFixture, ConsoleFixture, EnvFixture, Expectations, FixtureStep, RandFixture,
        TestResult, U64Text,
    };

    #[test]
    fn clock_and_rand_are_reproducible_and_transcripted() {
        let plan = FixturePlan {
            clock: Some(ClockFixture {
                start_ns: Some(U64Text::new(100)),
                step_ns: Some(U64Text::new(5)),
                ..Default::default()
            }),
            rand: Some(RandFixture {
                seed: Some(U64Text::new(42)),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut first = FixtureSession::new(plan.clone()).expect("valid session");
        let mut second = FixtureSession::new(plan).expect("valid session");
        assert_eq!(first.clock_now(None), Ok(100));
        assert_eq!(first.clock_now(None), Ok(105));
        assert_eq!(first.rand_u64(None), second.rand_u64(None));
        assert_eq!(first.rand_u64(None), second.rand_u64(None));
        let transcript = first.finish(TestResult::Passed);
        assert_eq!(transcript.events.len(), 4);
        assert_eq!(transcript.seed.as_ref().map(U64Text::get), Some(42));
    }

    #[test]
    fn console_scripts_fail_and_capture_without_host_io() {
        let plan = FixturePlan {
            console: Some(ConsoleFixture {
                script: vec![
                    FixtureStep {
                        operation: "console_read_len".into(),
                        target: None,
                        arguments: BTreeMap::new(),
                        effective_rights: Some(vec!["Read".into()]),
                        outcome: FixtureOutcome::Return {
                            value: FixtureValue::String("hello".into()),
                        },
                        required: true,
                    },
                    FixtureStep {
                        operation: "print".into(),
                        target: None,
                        arguments: BTreeMap::from([(
                            "text".into(),
                            FixtureValue::String("hello".into()),
                        )]),
                        effective_rights: Some(vec!["Write".into()]),
                        outcome: FixtureOutcome::Fail {
                            error: failure(
                                FixtureErrorCode::ProviderFailure,
                                "configured write failure",
                            ),
                        },
                        required: true,
                    },
                ],
            }),
            expectations: Expectations {
                require_complete_scripts: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut session = FixtureSession::new(plan).expect("valid session");
        assert_eq!(session.console_read(None), Ok("hello".into()));
        assert_eq!(
            session.console_write("hello", None).expect_err("scripted failure").message,
            "configured write failure"
        );
        let transcript = session.finish(TestResult::Passed);
        assert!(transcript.stdout.is_empty());
        assert_eq!(transcript.events.len(), 2);
    }

    #[test]
    fn env_handles_are_session_branded_and_cannot_widen() {
        let plan = FixturePlan {
            env: Some(EnvFixture {
                values: BTreeMap::from([
                    ("MODE".into(), "test".into()),
                    ("SECRET".into(), "hidden".into()),
                ]),
                allow: vec!["MODE".into()],
                script: Vec::new(),
            }),
            ..Default::default()
        };
        let mut first = FixtureSession::new(plan.clone()).expect("valid session");
        let mut second = FixtureSession::new(plan).expect("valid session");
        let root = first.mint_env(None).expect("root handle");
        assert_eq!(first.env_get(&root, "MODE", None), Ok(Some("test".into())));
        assert_eq!(
            first.env_get(&root, "SECRET", None).expect_err("not allowed").code,
            FixtureErrorCode::PermissionDenied
        );
        assert_eq!(
            first
                .env_only(&root, &["MODE".into(), "SECRET".into()], None)
                .expect_err("cannot widen")
                .code,
            FixtureErrorCode::PermissionDenied
        );
        assert_eq!(
            second.env_get(&root, "MODE", None).expect_err("foreign handle").code,
            FixtureErrorCode::PermissionDenied
        );
    }

    #[test]
    fn argv_is_immutable_launch_input_with_an_event() {
        let plan = FixturePlan {
            argv: Some(vec!["one".into(), "two".into()]),
            ..Default::default()
        };
        let mut session = FixtureSession::new(plan).expect("valid session");
        let mut returned = session.argv(None).expect("argv");
        returned.push("local mutation".into());
        assert_eq!(session.argv(None).expect("argv again").len(), 2);
        assert_eq!(session.finish(TestResult::Passed).events.len(), 2);
    }
}
