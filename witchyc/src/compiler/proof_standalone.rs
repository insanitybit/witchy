//! Fresh Lean processes bound to a checked operator installation.
use super::{Goal, Result, audit_dependencies, batches, digest, error, failing, io, validate_output};
use crate::compiler::proof_module::{self, Namespace};
use crate::compiler::proof_measure::Measurements;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug)]
pub(super) struct CheckedOutput {
    pub(super) text: String,
    _scratch: Scratch,
}

pub(super) struct Sdk {
    root: PathBuf,
    executable: PathBuf,
    identity: String,
}

fn fingerprint(root: &std::path::Path) -> Result<String> {
    match digest::installation(root) {
        Err(reason) if reason.kind() == std::io::ErrorKind::Interrupted
            && super::super::compiler_session::cancelled() =>
        {
            Err(error("Lean installation fingerprint cancelled by compiler session shutdown"))
        }
        result => io(result),
    }
}

impl Sdk {
    pub(super) fn snapshot(measurements: &mut Measurements) -> Result<Self> {
        let phase = Instant::now();
        // The operator supplies a trusted installation that stays immutable during checking.
        let configured = std::env::var_os("WITCHY_WASM_LEAN_ROOT")
            .ok_or_else(|| error("scalar proof requires trusted operator WITCHY_WASM_LEAN_ROOT"))?;
        let configured = PathBuf::from(configured);
        if !configured.is_absolute() {
            return Err(error("trusted Lean installation must be absolute"));
        }
        let root = io(fs::canonicalize(configured))?;
        let executable = root.join("bin/lean");
        if io(fs::canonicalize(&executable))? != executable {
            return Err(error("Lean executable symlink refused"));
        }
        measurements.configuration_ns += phase.elapsed().as_nanos();
        let phase = Instant::now();
        let identity = fingerprint(&root)?;
        measurements.installation_hash_a_ns += phase.elapsed().as_nanos();
        measurements.installation_snapshots += 1;
        Ok(Self { root, executable, identity })
    }

    pub(super) fn check(
        &self,
        module: &str,
        marker: &str,
        measurements: &mut Measurements,
        on_rejection: impl FnOnce(&str),
    ) -> Result<CheckedOutput> {
        let phase = Instant::now();
        let path = std::env::temp_dir().join(format!(
            "witchy-scalar-proof-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        let mut directory = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            directory.mode(0o700);
        }
        io(directory.create(&path))?;
        let scratch = Scratch(path);
        let input = scratch.0.join("ScalarProof.lean");
        io(fs::write(&input, module))?;
        // Bound file-backed output so a child cannot block on a full pipe.
        let stdout = scratch.0.join("stdout");
        let stderr = scratch.0.join("stderr");
        let output_file = io(fs::File::create(&stdout))?;
        let error_file = io(fs::File::create(&stderr))?;
        measurements.scratch_setup_write_ns += phase.elapsed().as_nanos();
        if super::super::compiler_session::cancelled() {
            return Err(error("standalone Lean proof check cancelled by compiler session shutdown"));
        }
        let child_start = Instant::now();
        let mut child = io(Command::new(&self.executable)
            .args(["--trust=0", "--threads=1", "--memory=2048"])
            .arg(&input)
            .env_clear()
            .env("LEAN_PATH", self.root.join("lib/lean"))
            .current_dir(&scratch.0)
            .stdin(Stdio::null())
            .stdout(output_file)
            .stderr(error_file)
            .spawn())?;
        let start = Instant::now();
        let status = loop {
            if super::super::compiler_session::cancelled() {
                break Err(error("standalone Lean proof check cancelled by compiler session shutdown"));
            }
            let Ok(status) = child.try_wait() else {
                break Err(error("cannot observe the standalone Lean proof checker process"));
            };
            let (Ok(output), Ok(errors)) = (fs::metadata(&stdout), fs::metadata(&stderr)) else {
                break Err(error("cannot inspect the standalone Lean proof checker output files"));
            };
            if output.len() > 65536 || errors.len() > 65536 {
                break Err(error("standalone Lean proof checker exceeded its 65,536-byte output limit"));
            }
            if start.elapsed() > Duration::from_secs(10) {
                break Err(error("standalone Lean proof checker exceeded its 10-second deadline"));
            }
            if let Some(status) = status {
                break Ok(status);
            }
            std::thread::sleep(Duration::from_millis(5));
        }.inspect_err(|_| {
            let _ = child.kill();
            let _ = child.wait();
        })?;
        let request = child_start.elapsed().as_nanos();
        measurements.child_spawn_to_exit_ns += request;
        measurements.slowest_request_ns = measurements.slowest_request_ns.max(request);
        let phase = Instant::now();
        let text = io(fs::read_to_string(&stdout))?;
        let errors = io(fs::read_to_string(&stderr))?;
        if let Err(error) = validate_output(status.success(), &text, &errors, marker) {
            if !status.success() {
                on_rejection(&text);
            }
            return Err(error);
        }
        measurements.output_audit_parse_ns += phase.elapsed().as_nanos();
        Ok(CheckedOutput { text, _scratch: scratch })
    }

    pub(super) fn confirm(&self, measurements: &mut Measurements) -> Result<()> {
        let phase = Instant::now();
        let after = fingerprint(&self.root)?;
        measurements.installation_hash_b_ns += phase.elapsed().as_nanos();
        measurements.installation_snapshots += 1;
        if after != self.identity {
            return Err(error("trusted Lean installation changed during checking"));
        }
        Ok(())
    }

    pub(super) fn into_identity(self) -> String {
        self.identity
    }

    // The build confirms the SDK after all batches, before admitting any proof.
    pub(super) fn run_batch(
        &self,
        module: &str,
        marker: &str,
        goal_count: usize,
        measurements: &mut Measurements,
        on_rejection: impl FnOnce(&str),
    ) -> Result<String> {
        let output = self.check(module, marker, measurements, on_rejection)?;
        let phase = Instant::now();
        let dependencies = audit_dependencies(&output.text, goal_count)?;
        measurements.output_audit_parse_ns += phase.elapsed().as_nanos();
        Ok(dependencies)
    }

    /// Locate errors in isolated goals for a build that has already been refused.
    pub(super) fn refused_goals(&self, goals: &[Goal]) -> Option<Vec<usize>> {
        let mut measurements = Measurements::default();
        let mut refused = Vec::new();
        const MARKER: &str = "WITCHY_SCALAR_DIAGNOSTIC_V1";
        for range in batches(goals).ok()? {
            let references = goals[range.clone()].iter().collect::<Vec<_>>();
            let module = proof_module::emit(Namespace::Current, &references, None)
                .ok()?
                .audited(MARKER)
                .ok()?;
            let mut named = Vec::new();
            match self.check(&module, MARKER, &mut measurements, |output| {
                named = failing(&module, output, 0);
            }) {
                Ok(output) => {
                    audit_dependencies(&output.text, references.len()).ok()?;
                }
                Err(_) if named.is_empty() => return None,
                Err(_) => refused.extend(named.into_iter().map(|(index, _)| range.start + index)),
            }
        }
        // Keep the original admission attempt and this diagnostic batch in the
        // same identity window. A changed SDK cannot supply isolated locations.
        self.confirm(&mut measurements).ok()?;
        Some(refused)
    }
}
