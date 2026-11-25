//! Ordinary publishing builds with bounded helpers and one pinned toolchain selection.
use super::{files, proof_session::Session, Measurements};
use std::path::Path;

pub struct BuildSession {
    checker: Option<Session>,
    failed: bool,
}
impl BuildSession {
    /// No ready binding selects the strict fresh checker before any proof attempt.
    /// A configured binding is an explicit operator no-mutation lifetime promise.
    pub fn new() -> Result<Self, String> {
        Ok(Self { checker: Session::publishing().map_err(display)?, failed: false })
    }
    pub fn build(&mut self, input: &Path, output: &Path, expected_source: Option<&str>) -> Result<(String, Measurements), String> {
        self.ready()?;
        let result = files::build_in(input, output, expected_source, self.checker.as_mut(), false);
        if result.is_err() {
            self.failed = true;
            if let Some(checker) = &mut self.checker { checker.refuse(); }
        }
        result
    }
    fn ready(&mut self) -> Result<(), String> {
        if self.failed { return Err("build session refused; restart required".into()); }
        if self.checker.as_ref().is_some_and(|s| !s.available()) {
            if let Err(error) = self.checker.as_mut().unwrap().renew() {
                self.failed = true;
                self.checker.as_mut().unwrap().refuse();
                return Err(display(error));
            }
        }
        Ok(())
    }

    fn native_reply(&mut self, input: &Path, output: &Path, expected: &str, id: &str, target: &str, manifest: Option<(&Path, &str)>) -> Result<String, String> {
        #[cfg(all(target_arch = "aarch64", any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))))]
        if target == super::native::TARGET {
            self.ready()?;
            return match files::build_native_in(input, output, Some(expected), self.checker.as_mut(), false, manifest.map(|(path, _)| path), manifest.map(|(_, digest)| digest)) {
                Ok(times) => Ok(format!(r#"{{"schema":"witchy.native-build.v1","request_id":"{id}","measurements":{}}}"#, times.json())),
                Err(error) => {
                    self.failed = true;
                    if let Some(checker) = &mut self.checker { checker.refuse(); }
                    Err(error)
                }
            };
        }
        let _ = (input, output, expected, id, manifest);
        Err(format!("W0401: {target} requires witchyc built for that host"))
    }

    pub fn finish(&mut self) -> Result<(), String> {
        if self.failed { return Err("build session failed; no fallback was attempted".into()); }
        if let Some(checker) = &mut self.checker {
            if let Err(error) = checker.validate_publication().and_then(|_| checker.finish()) {
                self.failed = true; checker.refuse(); return Err(display(error));
            }
        }
        // Finishing ends this session, including its body-library state.
        self.failed = true;
        Ok(())
    }
}
fn display(e: crate::diagnostic::Diagnostic) -> String {
    if e.span == (0..0) { return crate::diagnostic::unplaced(e.code, &e.message); }
    format!("{} {:?}: {}", e.code, e.span, e.message)
}

pub fn loop_build(input: &Path, output: &Path) -> Result<(), String> {
    loop_target(input, output, None, None)
}

/// With a launch manifest, each request names its digest too
/// (`build REQUEST_ID SOURCE_SHA256 MANIFEST_SHA256`), so a request made for one manifest cannot
/// publish an executable bound to another.
pub fn loop_target(input: &Path, output: &Path, target: Option<&str>, manifest: Option<&Path>) -> Result<(), String> {
    if manifest.is_some() && target.is_none() { return Err("a launch manifest selects a native target".into()); }
    use std::io::Write;
    let signals = super::build_signal::Signals::install()?;
    let mut session = BuildSession::new()?;
    let mut stdout = std::io::stdout().lock();
    loop {
        let mut bytes = Vec::new();
        loop {
            if signals.stopped() { return session.finish(); }
            if !signals.stdin_ready()? { continue; }
            let Some(byte) = signals.byte()? else {
                if bytes.is_empty() { return session.finish(); }
                return Err("truncated build request".into());
            };
            bytes.push(byte);
            if bytes.len() > 256 { return Err("build request exceeds 256 bytes".into()); }
            if byte == b'\n' { break; }
        }
        let line = String::from_utf8(bytes).map_err(|_| "build request must be UTF8")?;
        let parts = line.split_whitespace().collect::<Vec<_>>();
        let (id, expected, manifest_id) = match (parts.as_slice(), manifest) {
            (["build", id, expected], None) => (*id, *expected, None),
            (["build", id, expected, digest], Some(_)) => (*id, *expected, Some(*digest)),
            (_, None) => return Err("expected build REQUEST_ID SOURCE_SHA256".into()),
            (_, Some(_)) => return Err("expected build REQUEST_ID SOURCE_SHA256 MANIFEST_SHA256".into()),
        };
        if id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            || !super::proof_wire2::hash(expected) || manifest_id.is_some_and(|d| !super::proof_wire2::hash(d)) { return Err("build request identity refused".into()); }
        let reply = if let Some(target) = target {
            session.native_reply(input, output, expected, id, target, manifest.zip(manifest_id))?
        } else {
            let (report, measurements) = session.build(input, output, Some(expected))?;
            format!(r#"{{"schema":"witchy.build.v1","request_id":"{id}","check":{report},"measurements":{}}}"#, measurements.json())
        };
        writeln!(stdout, "{reply}").map_err(|e| e.to_string())?;
        stdout.flush().map_err(|e| e.to_string())?;
    }
}

pub fn watch(input: &Path, output: &Path) -> Result<(), String> {
    let signals = super::build_signal::Signals::install()?;
    let mut session = BuildSession::new()?;
    let mut previous = None;
    loop {
        if signals.stopped() { return session.finish(); }
        let source = files::read_source(input)?;
        let identity = super::proof_digest::bytes(source.as_bytes()).map_err(|e| e.to_string())?;
        if previous.as_ref() != Some(&identity) {
            let (_, measurements) = session.build(input, output, Some(&identity))?;
            eprintln!("built {} ({} bytes, {} proof batch)", output.display(), measurements.wasm_bytes, measurements.lean_invocations);
            previous = Some(identity);
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}
