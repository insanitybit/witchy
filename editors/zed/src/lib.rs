//! Zed extension entry point for witchy. It tells Zed how to launch the witchy
//! language server (`witchy lsp`); syntax highlighting is driven by the bundled
//! Tree-sitter grammar and queries.

use zed_extension_api::{self as zed, Command, LanguageServerId, Result, Worktree};

struct WitchyExtension;

impl zed::Extension for WitchyExtension {
    fn new() -> Self {
        WitchyExtension
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Command> {
        // Honour an explicit override in Zed settings, e.g.
        //   "lsp": { "witchy": { "binary": { "path": "/path/to/witchy" } } }
        let binary = zed::settings::LspSettings::for_worktree("witchy", worktree)
            .ok()
            .and_then(|settings| settings.binary)
            .and_then(|binary| binary.path);

        let command = match binary {
            Some(path) => path,
            // Otherwise expect `witchy` on PATH (install with `cargo install
            // --path .` from the witchy repo).
            None => worktree.which("witchy").ok_or_else(|| {
                "could not find `witchy` on PATH. Build it with `cargo install --path .` \
                 from the witchy repo, or set lsp.witchy.binary.path in your Zed settings."
                    .to_string()
            })?,
        };

        Ok(Command {
            command,
            args: vec!["lsp".to_string()],
            env: Default::default(),
        })
    }
}

zed::register_extension!(WitchyExtension);
