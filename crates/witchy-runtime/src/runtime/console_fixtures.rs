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
        let entry = artifact.entry().cloned().ok_or_else(|| Error::msg("admitted fixture entry missing"))?;
        witchy_test_host::validate_console_output_plan_with_argv(&plan, entry.argument_positions().len()).map_err(Error::msg)?;
        if entry.result() != CanonicalEntryResult::Unit
            || entry.capabilities().is_empty()
            || entry.capabilities().iter().any(|kind| *kind != witchy_wir::CapabilityKind::Console)
        {
            bail!("admitted Console fixture requires a unit entry with only Console and argv parameters; other recipes are unsupported");
        }
        let mut host = witchy_test_host::FixtureHost::new(plan)
            .map_err(|error| Error::msg(format!("invalid fixture plan: {error}")))?;
        let engine = self.engine_for(artifact.heap_mode())?;
        let module = WorkerModule::from_admitted(&engine, artifact)?;
        let mut caps = Capabilities::none();
        for _ in entry.argument_positions() {
            caps.args = match host.invoke(witchy_test_host::HostRequest::Argv, None)
                .map_err(|failure| Error::msg(format!("fixture {:?}: {}", failure.code, failure.message)))?
            {
                witchy_test_host::HostResponse::Strings(arguments) => arguments,
                _ => bail!("internal error: argv fixture returned a non-string list"),
            };
        }
        let limits = StoreLimitsBuilder::new().memory_size(memory_pages_max * 64 * 1024).build();
        let mut state = vmstate_from_caps(self.next_id, &caps, limits, None, &engine,
            module.module(), self.preempt, self.compiler_services.clone())?;
        state.module = module.clone();
        state.limits.set_launch(artifact.heap_mode(), memory_pages_max * 64 * 1024);
        state.fixture_host = Some(host);
        state.fixture_console = Some(Arc::new(host::canonical_fixture::Session));
        let mut store = Store::new(&engine, state);
        store.limiter(|state| &mut state.limits);
        if self.preempt { store.set_epoch_deadline(1); }
        let mut linker = Linker::new(&engine);
        link_capability_imports(&mut linker, &caps)?;
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
