use std::collections::BTreeSet;
use std::fmt;

use witchy_cap_model::CapabilityKind;

use crate::{
    ExecFixture, FilesystemEntry, FilesystemFixture, FixturePlan, FixtureStep, FixtureValue,
    TEST_TRANSCRIPT_VERSION,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlanValidationLimits {
    pub max_json_bytes: usize,
    pub max_collection_items: usize,
    pub max_script_steps: usize,
    pub max_value_depth: usize,
    pub max_string_bytes: usize,
    pub max_fixture_bytes: usize,
}

impl Default for PlanValidationLimits {
    fn default() -> Self {
        Self {
            max_json_bytes: 4 * 1024 * 1024,
            max_collection_items: 16_384,
            max_script_steps: 100_000,
            max_value_depth: 64,
            max_string_bytes: 1024 * 1024,
            max_fixture_bytes: 16 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanValidationError {
    path: String,
    message: String,
}

impl PlanValidationError {
    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for PlanValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

impl std::error::Error for PlanValidationError {}

fn invalid(path: impl Into<String>, message: impl Into<String>) -> PlanValidationError {
    PlanValidationError {
        path: path.into(),
        message: message.into(),
    }
}

impl FixturePlan {
    pub fn validate(&self) -> Result<(), PlanValidationError> {
        self.validate_with(&PlanValidationLimits::default())
    }

    pub(crate) fn validate_with(
        &self,
        limits: &PlanValidationLimits,
    ) -> Result<(), PlanValidationError> {
        self.validate_with_limits(limits)
    }

    fn validate_with_limits(
        &self,
        limits: &PlanValidationLimits,
    ) -> Result<(), PlanValidationError> {
        if self.version != crate::FIXTURE_PLAN_VERSION {
            return Err(invalid(
                "version",
                format!(
                    "unsupported fixture plan version {}; expected {}",
                    self.version,
                    crate::FIXTURE_PLAN_VERSION
                ),
            ));
        }
        if TEST_TRANSCRIPT_VERSION != 1 {
            return Err(invalid("version", "unsupported transcript contract"));
        }

        let mut total_steps = 0usize;
        for (path, steps) in self.scripts() {
            total_steps = total_steps
                .checked_add(steps.len())
                .ok_or_else(|| invalid(path, "script count overflow"))?;
            for (index, step) in steps.iter().enumerate() {
                validate_step(step, &format!("{path}.script[{index}]"), limits)?;
            }
        }
        if total_steps > limits.max_script_steps {
            return Err(invalid(
                "fixture plan",
                format!(
                    "contains {total_steps} script steps; limit is {}",
                    limits.max_script_steps
                ),
            ));
        }

        if let Some(clock) = &self.clock {
            if !clock.script.is_empty() && (clock.start_ns.is_some() || clock.step_ns.is_some()) {
                return Err(invalid(
                    "clock",
                    "script cannot be combined with start_ns or step_ns",
                ));
            }
            if clock.step_ns.is_some() && clock.start_ns.is_none() {
                return Err(invalid("clock.step_ns", "requires clock.start_ns"));
            }
            if clock.repeat_last && clock.script.is_empty() {
                return Err(invalid("clock.repeat_last", "requires a clock script"));
            }
        }
        if let Some(rand) = &self.rand
            && rand.seed.is_some()
            && !rand.script.is_empty()
        {
            return Err(invalid("rand", "seed cannot be combined with a rand script"));
        }
        if let Some(env) = &self.env {
            check_count("env.values", env.values.len(), limits)?;
            check_unique_strings("env.allow", &env.allow, limits)?;
            for (name, value) in &env.values {
                check_name(&format!("env.values.{name}"), name, limits)?;
                check_string(&format!("env.values.{name}"), value, limits)?;
            }
            for name in &env.allow {
                check_name("env.allow", name, limits)?;
            }
        }
        if let Some(filesystem) = &self.filesystem {
            validate_filesystem(filesystem, limits)?;
        }
        if let Some(fetch) = &self.fetch {
            check_unique_strings("fetch.origins", &fetch.origins, limits)?;
            for (index, origin) in fetch.origins.iter().enumerate() {
                let parsed = witchy_cap_model::FetchOrigin::parse(origin)
                    .map_err(|error| invalid(format!("fetch.origins[{index}]"), error.to_string()))?;
                if parsed.as_str() != *origin {
                    return Err(invalid(
                        format!("fetch.origins[{index}]"),
                        format!("origin must use canonical spelling `{}`", parsed.as_str()),
                    ));
                }
            }
            for (index, step) in fetch.script.iter().enumerate() {
                validate_fetch_step(step, index)?;
            }
        }
        if let Some(secrets) = &self.secrets {
            check_count("secrets.entries", secrets.entries.len(), limits)?;
            let mut total_bytes = 0usize;
            for (name, secret) in &secrets.entries {
                check_name(&format!("secrets.entries.{name}"), name, limits)?;
                let bytes = validate_hex(
                    &format!("secrets.entries.{name}.hex"),
                    &secret.hex,
                )?;
                if secret.usage == crate::SecretUsage::Signing && bytes != 32 {
                    return Err(invalid(
                        format!("secrets.entries.{name}.hex"),
                        "signing secrets must contain exactly 32 bytes",
                    ));
                }
                total_bytes = total_bytes
                    .checked_add(bytes)
                    .ok_or_else(|| invalid("secrets.entries", "byte count overflow"))?;
            }
            if total_bytes > limits.max_fixture_bytes {
                return Err(invalid(
                    "secrets.entries",
                    format!(
                        "contains {total_bytes} bytes; limit is {}",
                        limits.max_fixture_bytes
                    ),
                ));
            }
        }
        if let Some(exec) = &self.exec {
            validate_exec(exec, limits)?;
        }
        if let Some(argv) = &self.argv {
            check_count("argv", argv.len(), limits)?;
            for (index, argument) in argv.iter().enumerate() {
                check_string(&format!("argv[{index}]"), argument, limits)?;
            }
        }
        check_unique_families(
            "expectations.absent_families",
            &self.expectations.absent_families,
        )?;
        check_count(
            "expectations.ordered_calls",
            self.expectations.ordered_calls.len(),
            limits,
        )?;
        for (index, expectation) in self.expectations.ordered_calls.iter().enumerate() {
            check_name(
                &format!("expectations.ordered_calls[{index}].operation"),
                &expectation.operation,
                limits,
            )?;
            if let Some(target) = &expectation.target {
                check_string(
                    &format!("expectations.ordered_calls[{index}].target"),
                    target,
                    limits,
                )?;
            }
            if let Some(rights) = &expectation.effective_rights {
                check_unique_strings(
                    &format!("expectations.ordered_calls[{index}].effective_rights"),
                    rights,
                    limits,
                )?;
            }
        }
        check_count(
            "expectations.calls",
            self.expectations.calls.len(),
            limits,
        )?;
        for (index, expectation) in self.expectations.calls.iter().enumerate() {
            check_name(
                &format!("expectations.calls[{index}].operation"),
                &expectation.operation,
                limits,
            )?;
            if let (Some(minimum), Some(maximum)) =
                (&expectation.minimum, &expectation.maximum)
                && minimum.get() > maximum.get()
            {
                return Err(invalid(
                    format!("expectations.calls[{index}]"),
                    "minimum exceeds maximum",
                ));
            }
        }
        Ok(())
    }

    fn scripts(&self) -> Vec<(&'static str, &[FixtureStep])> {
        let mut scripts = Vec::with_capacity(9);
        if let Some(value) = &self.console {
            scripts.push(("console", value.script.as_slice()));
        }
        if let Some(value) = &self.clock {
            scripts.push(("clock", value.script.as_slice()));
        }
        if let Some(value) = &self.rand {
            scripts.push(("rand", value.script.as_slice()));
        }
        if let Some(value) = &self.env {
            scripts.push(("env", value.script.as_slice()));
        }
        if let Some(value) = &self.filesystem {
            scripts.push(("filesystem", value.script.as_slice()));
        }
        if let Some(value) = &self.fetch {
            scripts.push(("fetch", value.script.as_slice()));
        }
        if let Some(value) = &self.secrets {
            scripts.push(("secrets", value.script.as_slice()));
        }
        if let Some(value) = &self.exec {
            scripts.push(("exec", value.script.as_slice()));
        }
        scripts
    }
}

fn validate_step(
    step: &FixtureStep,
    path: &str,
    limits: &PlanValidationLimits,
) -> Result<(), PlanValidationError> {
    check_name(&format!("{path}.operation"), &step.operation, limits)?;
    if let Some(target) = &step.target {
        check_string(&format!("{path}.target"), target, limits)?;
    }
    check_count(
        &format!("{path}.arguments"),
        step.arguments.len(),
        limits,
    )?;
    for (name, value) in &step.arguments {
        check_name(&format!("{path}.arguments.{name}"), name, limits)?;
        validate_value(
            value,
            &format!("{path}.arguments.{name}"),
            0,
            limits,
        )?;
    }
    if let Some(rights) = &step.effective_rights {
        check_unique_strings(&format!("{path}.effective_rights"), rights, limits)?;
    }
    let value = match &step.outcome {
        crate::FixtureOutcome::Return { value } => value,
        crate::FixtureOutcome::Fail { error } => {
            check_string(&format!("{path}.outcome.error.message"), &error.message, limits)?;
            return Ok(());
        }
    };
    validate_value(value, &format!("{path}.outcome.value"), 0, limits)
}

fn validate_value(
    value: &FixtureValue,
    path: &str,
    depth: usize,
    limits: &PlanValidationLimits,
) -> Result<(), PlanValidationError> {
    if depth > limits.max_value_depth {
        return Err(invalid(
            path,
            format!("value depth exceeds limit {}", limits.max_value_depth),
        ));
    }
    match value {
        FixtureValue::Null | FixtureValue::Bool(_) => Ok(()),
        FixtureValue::String(value) => check_string(path, value, limits),
        FixtureValue::Bytes(value) => {
            let bytes = validate_hex(path, value)?;
            if bytes > limits.max_fixture_bytes {
                Err(invalid(
                    path,
                    format!("contains {bytes} bytes; limit is {}", limits.max_fixture_bytes),
                ))
            } else {
                Ok(())
            }
        }
        FixtureValue::List(values) => {
            check_count(path, values.len(), limits)?;
            for (index, value) in values.iter().enumerate() {
                validate_value(value, &format!("{path}[{index}]"), depth + 1, limits)?;
            }
            Ok(())
        }
        FixtureValue::Map(values) => {
            check_count(path, values.len(), limits)?;
            for (name, value) in values {
                check_name(path, name, limits)?;
                validate_value(value, &format!("{path}.{name}"), depth + 1, limits)?;
            }
            Ok(())
        }
    }
}

fn validate_filesystem(
    filesystem: &FilesystemFixture,
    limits: &PlanValidationLimits,
) -> Result<(), PlanValidationError> {
    check_count("filesystem.entries", filesystem.entries.len(), limits)?;
    check_unique_strings("filesystem.rights", &filesystem.rights, limits)?;
    for right in &filesystem.rights {
        if CapabilityKind::Dir.right(right).is_none() {
            return Err(invalid(
                "filesystem.rights",
                format!("unknown Dir right `{right}`"),
            ));
        }
    }

    let mut total_bytes = 0usize;
    for (path, entry) in &filesystem.entries {
        validate_fixture_path(path)
            .map_err(|message| invalid(format!("filesystem.entries.{path}"), message))?;
        if let FilesystemEntry::File { hex } = entry {
            total_bytes = total_bytes
                .checked_add(validate_hex(&format!("filesystem.entries.{path}.hex"), hex)?)
                .ok_or_else(|| invalid("filesystem.entries", "byte count overflow"))?;
        }
    }
    for (path, entry) in &filesystem.entries {
        if matches!(entry, FilesystemEntry::File { .. }) {
            let prefix = format!("{path}/");
            if filesystem.entries.keys().any(|candidate| candidate.starts_with(&prefix)) {
                return Err(invalid(
                    format!("filesystem.entries.{path}"),
                    "file cannot contain another fixture entry",
                ));
            }
        }
    }
    if total_bytes > limits.max_fixture_bytes {
        return Err(invalid(
            "filesystem.entries",
            format!(
                "contains {total_bytes} bytes; limit is {}",
                limits.max_fixture_bytes
            ),
        ));
    }
    if let Some(policy) = &filesystem.entry_policy {
        check_string("filesystem.entry_policy", policy, limits)?;
    }
    Ok(())
}

fn validate_exec(exec: &ExecFixture, limits: &PlanValidationLimits) -> Result<(), PlanValidationError> {
    check_unique_strings("exec.tools", &exec.tools, limits)?;
    for (index, tool) in exec.tools.iter().enumerate() {
        check_name(&format!("exec.tools[{index}]"), tool, limits)?;
        if tool.contains(['/', '\\', '\0']) || tool == "." || tool == ".." {
            return Err(invalid(
                format!("exec.tools[{index}]"),
                "tool must be a logical name, not a path",
            ));
        }
    }
    Ok(())
}

fn validate_fetch_step(
    step: &FixtureStep,
    index: usize,
) -> Result<(), PlanValidationError> {
    let path = format!("fetch.script[{index}]");
    if step.operation != "fetch_send_len" {
        return Err(invalid(
            format!("{path}.operation"),
            "Fetch scripts support only `fetch_send_len`",
        ));
    }
    let crate::FixtureOutcome::Return {
        value: crate::FixtureValue::Map(fields),
    } = &step.outcome
    else {
        return Ok(());
    };
    let Some(crate::FixtureValue::String(status)) = fields.get("status") else {
        return Err(invalid(
            format!("{path}.outcome.value.status"),
            "Fetch response status must be an unsigned decimal string",
        ));
    };
    let status = status.parse::<u16>().map_err(|_| {
        invalid(
            format!("{path}.outcome.value.status"),
            "Fetch response status is out of range",
        )
    })?;
    if (300..400).contains(&status) {
        return Err(invalid(
            format!("{path}.outcome"),
            "redirects must be scripted as a `redirect` failure",
        ));
    }
    Ok(())
}

fn validate_fixture_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("path must not be empty".into());
    }
    if path.starts_with('/') || path.contains('\\') || path.contains('\0') {
        return Err("path must be a normalized relative slash path".into());
    }
    if path
        .split('/')
        .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err("path must not contain empty, `.` or `..` components".into());
    }
    Ok(())
}

fn validate_hex(path: &str, value: &str) -> Result<usize, PlanValidationError> {
    if !value.len().is_multiple_of(2) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid(path, "expected an even-length hexadecimal byte string"));
    }
    Ok(value.len() / 2)
}

fn check_name(
    path: &str,
    value: &str,
    limits: &PlanValidationLimits,
) -> Result<(), PlanValidationError> {
    if value.is_empty() {
        return Err(invalid(path, "must not be empty"));
    }
    check_string(path, value, limits)
}

fn check_string(
    path: &str,
    value: &str,
    limits: &PlanValidationLimits,
) -> Result<(), PlanValidationError> {
    if value.len() > limits.max_string_bytes {
        return Err(invalid(
            path,
            format!(
                "contains {} bytes; limit is {}",
                value.len(),
                limits.max_string_bytes
            ),
        ));
    }
    if value.contains('\0') {
        return Err(invalid(path, "must not contain NUL"));
    }
    Ok(())
}

fn check_count(
    path: &str,
    count: usize,
    limits: &PlanValidationLimits,
) -> Result<(), PlanValidationError> {
    if count > limits.max_collection_items {
        Err(invalid(
            path,
            format!(
                "contains {count} items; limit is {}",
                limits.max_collection_items
            ),
        ))
    } else {
        Ok(())
    }
}

fn check_unique_strings(
    path: &str,
    values: &[String],
    limits: &PlanValidationLimits,
) -> Result<(), PlanValidationError> {
    check_count(path, values.len(), limits)?;
    let mut unique = BTreeSet::new();
    for value in values {
        check_string(path, value, limits)?;
        if !unique.insert(value) {
            return Err(invalid(path, format!("duplicate value `{value}`")));
        }
    }
    Ok(())
}

fn check_unique_families(
    path: &str,
    values: &[crate::FixtureFamily],
) -> Result<(), PlanValidationError> {
    let mut unique = BTreeSet::new();
    for value in values {
        if !unique.insert(value) {
            return Err(invalid(path, format!("duplicate family `{value:?}`")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClockFixture, RandFixture, U64Text};

    #[test]
    fn mutually_exclusive_generators_are_rejected() {
        let plan = FixturePlan {
            clock: Some(ClockFixture {
                start_ns: Some(U64Text::new(1)),
                script: vec![FixtureStep {
                    operation: "now".into(),
                    target: None,
                    arguments: Default::default(),
                    effective_rights: None,
                    outcome: crate::FixtureOutcome::Return {
                        value: FixtureValue::String("2".into()),
                    },
                    required: true,
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            plan.validate().expect_err("mixed clock modes").message(),
            "script cannot be combined with start_ns or step_ns"
        );

        let plan = FixturePlan {
            rand: Some(RandFixture {
                seed: Some(U64Text::new(7)),
                script: vec![FixtureStep {
                    operation: "rand_u64".into(),
                    target: None,
                    arguments: Default::default(),
                    effective_rights: None,
                    outcome: crate::FixtureOutcome::Return {
                        value: FixtureValue::String("8".into()),
                    },
                    required: true,
                }],
            }),
            ..Default::default()
        };
        assert!(plan.validate().is_err());
    }

    #[test]
    fn paths_rights_origins_and_tool_names_are_bounded() {
        let path_plan =
            crate::parse_fixture_plan(br#"{"version":1,"filesystem":{"entries":{"../host":{"kind":"directory"}}}}"#)
                .expect_err("escaping path");
        assert!(path_plan.message().contains("`..`"));

        let right_plan =
            crate::parse_fixture_plan(br#"{"version":1,"filesystem":{"rights":["Execute"]}}"#)
                .expect_err("unknown right");
        assert!(right_plan.message().contains("unknown Dir right"));

        let origin_plan =
            crate::parse_fixture_plan(br#"{"version":1,"fetch":{"origins":["https://example.com:443/path"]}}"#)
                .expect_err("origin path");
        assert!(origin_plan.message().contains("must not contain a path"));

        let tool_plan =
            crate::parse_fixture_plan(br#"{"version":1,"exec":{"tools":["/bin/sh"]}}"#)
                .expect_err("host path");
        assert!(tool_plan.message().contains("logical name"));
    }

    #[test]
    fn files_cannot_have_descendants() {
        let error = crate::parse_fixture_plan(
            br#"{"version":1,"filesystem":{"entries":{"cache":{"kind":"file","hex":""},"cache/child":{"kind":"file","hex":""}}}}"#,
        )
        .expect_err("file descendants");
        assert!(error.message().contains("file cannot contain"));
    }

    #[test]
    fn fetch_redirects_must_be_failure_outcomes() {
        let error = crate::parse_fixture_plan(
            br#"{"version":1,"fetch":{"origins":["https://example.com:443"],"script":[{"operation":"fetch_send_len","outcome":{"kind":"return","value":{"kind":"map","value":{"status":{"kind":"string","value":"302"},"headers":{"kind":"list","value":[]},"body":{"kind":"bytes","value":""}}}}}]}}"#,
        )
        .expect_err("successful redirect response");
        assert!(error.message().contains("redirects must be scripted"));
    }

    #[test]
    fn vm_is_not_a_fixture_plan_family() {
        let error = crate::parse_fixture_plan(br#"{"version":1,"vm":{}}"#)
            .expect_err("VM is a zero-authority host facility, not a fixture family");
        assert!(error.message().contains("unknown field `vm`"));
    }
}
