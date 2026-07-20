//! Frontend inspection command services: `check`, `expand`, and `doc`.

use super::compile::compile_checked_to_wasm;
use crate::{comptime, doc, enforce_performance_modes, link_file_checked, linked_has_main, parser};
use crate::source::expand_file_source;

#[derive(Debug, PartialEq, Eq)]
struct CommandOutput {
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
}

impl CommandOutput {
    fn success(stdout: String) -> Self {
        Self {
            stdout,
            stderr: String::new(),
            exit_code: None,
        }
    }

    fn failure(exit_code: i32, diagnostic: String) -> Self {
        Self {
            stdout: String::new(),
            stderr: format!("{diagnostic}\n"),
            exit_code: Some(exit_code),
        }
    }

    fn emit(self) {
        print!("{}", self.stdout);
        eprint!("{}", self.stderr);
        if let Some(code) = self.exit_code {
            std::process::exit(code);
        }
    }
}

/// Run `witchy doc` when it is the requested command.
///
/// The boolean tells the thin CLI whether this service handled argv. Errors
/// retain the command's established process status and diagnostics.
pub(crate) fn run_document() -> bool {
    if std::env::args().nth(1).as_deref() != Some("doc") {
        return false;
    }
    let files: Vec<String> = std::env::args().skip(2).collect();
    document_command(&files).emit();
    true
}

fn document_command(files: &[String]) -> CommandOutput {
    if files.is_empty() {
        return CommandOutput::failure(2, "usage: witchy doc <file.witchy>...".to_string());
    }
    match document_files(files) {
        Ok(out) => CommandOutput::success(out),
        Err(error) => CommandOutput::failure(1, error),
    }
}

/// Render Markdown API documentation for each source file in argument order.
fn document_files(files: &[String]) -> Result<String, String> {
    use std::path::Path;

    let mut out = String::from("# API reference\n\n");
    for f in files {
        let stem = Path::new(f)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(f);
        let src = std::fs::read_to_string(f)
            .map_err(|e| format!("cannot read `{f}`: {e}"))?;
        let mut module = parser::parse_module(&src)
            .map_err(|e| format!("{f}: {e}"))?;
        comptime::expand(stem, &mut module)
            .map_err(|e| format!("{f}: {e}"))?;
        let markdown = doc::render_module(stem, &src, &module)
            .map_err(|e| format!("{f}: {e}"))?;
        out.push_str(&markdown);
    }
    Ok(out)
}

/// Run `witchy expand` when it is the requested command.
pub(crate) fn run_expand() -> bool {
    if std::env::args().nth(1).as_deref() != Some("expand") {
        return false;
    }
    let path = std::env::args().nth(2);
    expand_command(path.as_deref()).emit();
    true
}

fn expand_command(path: Option<&str>) -> CommandOutput {
    let Some(path) = path else {
        return CommandOutput::failure(2, "usage: witchy expand <file.witchy>".to_string());
    };
    match expand_file(path) {
        Ok(src) => CommandOutput::success(src),
        Err(error) => CommandOutput::failure(1, error),
    }
}

/// Expand a source entrypoint to canonical source form.
fn expand_file(path: &str) -> Result<String, String> {
    expand_file_source(path)
}

/// Run `witchy check` when it is the requested command.
pub(crate) fn run_check() -> bool {
    if std::env::args().nth(1).as_deref() != Some("check") {
        return false;
    }
    let path = std::env::args().nth(2);
    check_command(path.as_deref()).emit();
    true
}

fn check_command(path: Option<&str>) -> CommandOutput {
    let Some(path) = path else {
        return CommandOutput::failure(1, "usage: witchy check <file.witchy>".to_string());
    };
    match check_file(path) {
        Ok(()) => CommandOutput::success(format!("{path}: ok\n")),
        Err(error) => CommandOutput::failure(1, error),
    }
}

/// Parse, link, type-check, and verify compiled-backend acceptance without
/// running the program. Library modules without `main` stop after checking.
pub(crate) fn check_file(path: &str) -> Result<(), String> {
    let (checked, stem) = link_file_checked(path)?;
    let linked = checked.module();
    enforce_performance_modes(linked, &stem)?;
    if linked_has_main(linked) {
        let _ = compile_checked_to_wasm(&checked)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixtureDir(std::path::PathBuf);

    impl FixtureDir {
        fn new(label: &str) -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "witchy_frontend_commands_{label}_{}_{nonce}",
                std::process::id(),
            ));
            std::fs::create_dir_all(&path).expect("create frontend-command fixture directory");
            Self(path)
        }

        fn write(&self, name: &str, source: &str) -> String {
            let path = self.0.join(name);
            std::fs::write(&path, source).expect("write frontend-command fixture");
            path.to_str().expect("UTF-8 fixture path").to_string()
        }
    }

    impl Drop for FixtureDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0)
                .expect("remove frontend-command fixture directory");
        }
    }

    #[test]
    fn checked_file_pipeline_authenticates_the_module_given_to_codegen() {
        let dir = FixtureDir::new("valid");
        let path = dir.write("main.witchy", "fn main() -> Int:\n    7\n");

        let (checked, stem) = link_file_checked(&path)
            .expect("link and check fixture");
        assert_eq!(stem, "main");
        let bytes = compile_checked_to_wasm(&checked).expect("compile checked fixture");
        assert!(!bytes.is_empty());

        let output = check_command(Some(&path));
        assert_eq!(
            output,
            CommandOutput::success(format!("{path}: ok\n")),
        );
    }

    #[test]
    fn checked_file_pipeline_rejects_before_codegen() {
        let dir = FixtureDir::new("invalid");
        let path = dir.write("main.witchy", "fn main() -> Int:\n    \"wrong\"\n");

        let error = check_file(&path)
            .expect_err("type-invalid source cannot construct checked codegen input");
        assert!(error.contains("expected `Int`"), "{error}");
        assert!(error.contains(&path), "{error}");
    }

    #[test]
    fn check_accepts_a_library_without_codegen_entrypoint() {
        let dir = FixtureDir::new("library");
        let path = dir.write("words.witchy", "pub fn word() -> String:\n    \"ok\"\n");

        check_file(&path).expect("check library fixture");
    }

    #[test]
    fn document_files_keeps_argument_order_and_single_heading() {
        let dir = FixtureDir::new("document");
        let alpha = dir.write(
            "alpha.witchy",
            "/// Alpha docs.\npub fn alpha() -> Int:\n    1\n",
        );
        let beta = dir.write(
            "beta.witchy",
            "/// Beta docs.\npub fn beta() -> Int:\n    2\n",
        );

        let output = document_files(&[alpha, beta]).expect("render fixture docs");
        assert_eq!(output.matches("# API reference").count(), 1, "{output}");
        let alpha_at = output.find("alpha").expect("alpha docs");
        let beta_at = output.find("beta").expect("beta docs");
        assert!(alpha_at < beta_at, "{output}");
        assert!(output.contains("Alpha docs."), "{output}");
        assert!(output.contains("Beta docs."), "{output}");
    }

    #[test]
    fn document_files_prefixes_read_errors_with_the_command_diagnostic() {
        let dir = FixtureDir::new("missing_doc");
        let path = dir.0.join("missing.witchy");
        let path = path.to_str().expect("UTF-8 fixture path").to_string();

        let error = document_files(std::slice::from_ref(&path)).expect_err("missing source fails");
        assert!(error.starts_with(&format!("cannot read `{path}`:")), "{error}");
    }

    #[test]
    fn document_files_prefixes_parse_errors_with_the_file() {
        let dir = FixtureDir::new("invalid_doc");
        let path = dir.write("broken.witchy", "pub fn broken(\n");

        let error = document_files(std::slice::from_ref(&path)).expect_err("invalid source fails");
        assert!(error.starts_with(&format!("{path}: ")), "{error}");
    }

    #[test]
    fn expand_file_returns_canonical_source() {
        let dir = FixtureDir::new("expand");
        let path = dir.write("main.witchy", "fn main()->Int:\n  7\n");

        let output = expand_file(&path).expect("expand fixture source");
        assert!(output.contains("fn main() -> Int:"), "{output}");
        assert!(output.ends_with('\n'), "{output:?}");
    }

    #[test]
    fn expand_file_preserves_source_loader_diagnostics() {
        let dir = FixtureDir::new("missing_expand");
        let path = dir.0.join("missing.witchy");
        let path = path.to_str().expect("UTF-8 fixture path").to_string();

        let error = expand_file(&path).expect_err("missing source fails");
        assert!(error.contains(&path), "{error}");
    }

    #[test]
    fn document_usage_preserves_status_and_diagnostic() {
        assert_eq!(
            document_command(&[]),
            CommandOutput::failure(2, "usage: witchy doc <file.witchy>...".to_string()),
        );
    }

    #[test]
    fn expand_usage_preserves_status_and_diagnostic() {
        assert_eq!(
            expand_command(None),
            CommandOutput::failure(2, "usage: witchy expand <file.witchy>".to_string()),
        );
    }

    #[test]
    fn check_usage_preserves_status_and_diagnostic() {
        assert_eq!(
            check_command(None),
            CommandOutput::failure(1, "usage: witchy check <file.witchy>".to_string()),
        );
    }

    #[test]
    fn command_failures_append_one_diagnostic_newline() {
        let output = CommandOutput::failure(1, "failure".to_string());

        assert_eq!(output.stdout, "");
        assert_eq!(output.stderr, "failure\n");
        assert_eq!(output.exit_code, Some(1));
    }
}
