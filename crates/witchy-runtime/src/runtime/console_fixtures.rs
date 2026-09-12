//! Artifact-bound fixture launch. Inert providers never pick an export, supply
//! Wasm bytes, install production authority, or make an aborted heap look empty.
use super::*;

/// Private transport from a source-bearing diagnostic import to fixture
/// finalization. Display retains normal runtime rendering; transcript data
/// comes from these fields, never from parsing or trimming that rendering.
#[derive(Debug)]
struct FixtureDiagnostic {
    function: String,
    line: u32,
    message: String,
}

impl std::fmt::Display for FixtureDiagnostic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&witchy_syntax::diag::runtime_error(&self.function, self.line, &self.message))
    }
}
impl std::error::Error for FixtureDiagnostic {}

pub(super) fn diagnostic(function: &str, line: u32, message: String) -> Error {
    Error::new(FixtureDiagnostic { function: function.to_owned(), line, message })
}

fn fixture_error_message(error: &Error) -> String {
    let Some(diagnostic) = error.downcast_ref::<FixtureDiagnostic>() else {
        // Unstructured host/provider errors are not eligible for normalization.
        return error.root_cause().to_string();
    };
    if diagnostic.line == 0 {
        diagnostic.message.clone()
    } else if diagnostic.function.is_empty() {
        format!("line {}: {}", diagnostic.line, diagnostic.message)
    } else {
        format!("`{}`, line {}: {}", diagnostic.function, diagnostic.line, diagnostic.message)
    }
}

impl Runtime {
    pub fn run_admitted_fixtures(
        &mut self, artifact: &AdmittedCanonicalArtifact, plan: witchy_testkit::FixturePlan,
        memory_pages_max: usize,
    ) -> Result<FixtureWasmOutcome> {
        witchy_test_host::validate_console_output_plan(&plan).map_err(Error::msg)?;
        let entry = artifact.entry().cloned().ok_or_else(|| Error::msg("admitted fixture entry missing"))?;
        if entry.result() != CanonicalEntryResult::Unit || !entry.argument_positions().is_empty()
            || entry.capabilities().is_empty()
            || entry.capabilities().iter().any(|kind| *kind != CanonicalEntryCapability::Console)
        {
            bail!("admitted Console fixture requires a Nil entry with only Console parameters; other recipes are unsupported");
        }
        let host = witchy_test_host::FixtureHost::new(plan)
            .map_err(|error| Error::msg(format!("invalid fixture plan: {error}")))?;
        let module = WorkerModule::from_admitted(&self.engine, artifact)?;
        let caps = Capabilities::none();
        let limits = StoreLimitsBuilder::new().memory_size(memory_pages_max * 64 * 1024).build();
        let mut state = vmstate_from_caps(self.next_id, &caps, limits, None, &self.engine,
            module.module(), self.preempt, self.compiler_services.clone())?;
        state.module = module.clone();
        state.fixture_host = Some(host);
        state.fixture_console = Some(Arc::new(host::canonical_fixture::Session));
        let mut store = Store::new(&self.engine, state);
        store.limiter(|state| &mut state.limits);
        if self.preempt { store.set_epoch_deadline(1); }
        let mut linker = Linker::new(&self.engine);
        link_capability_imports(&mut linker, &caps)?;
        host::canonical_fixture::link(&mut linker)?;
        host::closure_owner::register(&mut linker, artifact)?;
        let instance = linker.instantiate(&mut store, module.module())?;
        self.next_id += 1;
        let mut vm = Vm { store, instance, aborted: false, entry: Some(entry) };
        let mut execution = vm.run();
        let leaked_objects = if execution.is_ok() {
            let leaks = vm.canonical_heap_leaks()?
                .ok_or_else(|| Error::msg("admitted fixture lacks canonical heap accounting"))?;
            if leaks != 0 { execution = Err(Error::msg(format!("admitted fixture leaked {leaks} objects"))); }
            Some(leaks)
        } else { None };
        let output = vm.output();
        let transcript_result = match &execution {
            Ok(()) => witchy_testkit::TestResult::Passed,
            Err(error) => witchy_testkit::TestResult::Failed { message: fixture_error_message(error) },
        };
        let transcript = vm.store.data_mut().fixture_host.take()
            .expect("admitted fixture owns its provider session").finish(transcript_result);
        // Script exhaustion/order obligations can fail an otherwise successful
        // entry. The finalized transcript, not merely return-from-Wasm, decides.
        let result = match &transcript.result {
            witchy_testkit::TestResult::Passed => FixtureWasmResult::Passed,
            witchy_testkit::TestResult::Failed { message }
            | witchy_testkit::TestResult::InfrastructureError { message } => FixtureWasmResult::Failed { error: message.clone() },
        };
        Ok(FixtureWasmOutcome { result, output, transcript, leaked_objects })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_console_fixture_serializes_only_structured_diagnostics() {
        let error = diagnostic("suite.test_console", 3, "runtime error: literal payload".into());
        assert_eq!(error.to_string(), "runtime error: `suite.test_console`, line 3: runtime error: literal payload");
        assert_eq!(fixture_error_message(&error), "`suite.test_console`, line 3: runtime error: literal payload");
        let error = error.context("Wasm execution context");
        assert_eq!(fixture_error_message(&error), "`suite.test_console`, line 3: runtime error: literal payload");
        assert_eq!(fixture_error_message(&diagnostic("", 0, "runtime error: literal".into())), "runtime error: literal");
        assert_eq!(fixture_error_message(&diagnostic("", 4, "runtime error: literal".into())), "line 4: runtime error: literal");
        assert_eq!(fixture_error_message(&Error::msg("runtime error: arbitrary host text")), "runtime error: arbitrary host text");
    }
}
