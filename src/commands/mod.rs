//! Native command services.

pub(crate) mod build_steps;
pub(crate) mod dispatch;
pub(crate) mod embedded_pm;
pub(crate) mod sandbox;
pub(crate) mod test_runner;
pub(crate) mod wasm_exec;
// The web/static-site commands (`new|build|test|doctor --web`, `dev`) serve
// glamour and the docs site, not the compiler. They are ~28% of this crate
// and cost every build: after a compiler-crate change, rebuilding the witchy
// binary and `misc` took 46.4 CPU s with them and 35.0 without (best of 3,
// alternating, 2026-09-21). Non-default `web` feature; see Cargo.toml.
#[cfg(feature = "web")]
pub(crate) mod web;
#[cfg(not(feature = "web"))]
pub(crate) mod web {
    /// Without the `web` feature every web form fails loudly instead of
    /// falling through to the package manager's `new`/`build`/`test`.
    pub(crate) struct WebCommandError(String);
    impl WebCommandError {
        pub(crate) fn exit_code(&self) -> i32 { 2 }
    }
    impl std::fmt::Display for WebCommandError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(&self.0)
        }
    }
    pub(crate) fn run(args: &[String]) -> Result<Option<String>, WebCommandError> {
        let Some(command) = args.first().map(String::as_str) else { return Ok(None) };
        let web_flag = args.iter().any(|argument| argument == "--web");
        match command {
            "new" | "build" | "test" | "doctor" if web_flag => Err(unavailable(command)),
            "dev" => Err(unavailable(command)),
            _ => Ok(None),
        }
    }
    fn unavailable(command: &str) -> WebCommandError {
        WebCommandError(format!(
            "`witchy {command}` web support is not in this build; rebuild with \
             `cargo build --features web` (the web commands are optional since RFC-0168)"
        ))
    }
}
pub(crate) mod capabilities;
pub(crate) mod compile;
pub(crate) mod confinement;
pub(crate) mod execution;
pub(crate) mod frontend;
