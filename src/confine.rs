//! Path confinement for the `Dir` capability — the SINGLE, shared implementation
//! of the subtree escape checks (lexical `..`/absolute rejection + symlink-aware
//! canonicalization). Both the compiled-WASM sandbox (`runtime.rs`) and the
//! interpreter oracle resolve through these functions, so the two backends can
//! never diverge on which paths a `Dir` capability reaches. Keeping it one
//! implementation is a security invariant — see `docs/binary-distribution.md`.

use std::path::{Component, Path, PathBuf};

/// A confinement violation (an escape attempt, or an inaccessible base/target).
/// Carries the human-readable message both backends surface; the interpreter
/// wraps it into its `RuntimeError`, the runtime sandbox into a host trap.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfineError(pub String);

impl std::fmt::Display for ConfineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfineError {}

fn err<T>(message: &str) -> Result<T, ConfineError> {
    Err(ConfineError(message.to_string()))
}

/// Resolve `rel` against a `Dir` `base`, confining it to the subtree: reject
/// absolute paths and `..`, then canonicalize and require the result to stay
/// under the (canonicalized) base, so a symlink can't escape.
///
/// Note: canonicalize-then-use is mildly TOCTOU; the race-free fix is
/// syscall-level confinement (openat2/O_NOFOLLOW, i.e. the cap-std crate), which
/// is what the planned WASI-preopen substrate gives us.
pub fn resolve(base: &Path, rel: &str) -> Result<PathBuf, ConfineError> {
    let p = Path::new(rel);
    if p.is_absolute() {
        return err("absolute paths are not allowed (a Dir capability is a subtree)");
    }
    for comp in p.components() {
        match comp {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => return err("`..` escapes the Dir capability"),
            _ => return err("invalid path component in a Dir-relative path"),
        }
    }
    let joined = base.join(rel);
    let real = std::fs::canonicalize(&joined)
        .map_err(|e| ConfineError(format!("cannot access `{}`: {e}", joined.display())))?;
    let real_base = std::fs::canonicalize(base)
        .map_err(|e| ConfineError(format!("invalid Dir base `{}`: {e}", base.display())))?;
    if !real.starts_with(&real_base) {
        return err("path escapes the Dir capability (via symlink)");
    }
    Ok(real)
}

/// Like `resolve`, but for writing: the target file need not exist, so
/// confinement is checked against its parent directory (which must exist and lie
/// within the capability's subtree). The lexical `..`/absolute checks still apply.
pub fn resolve_write(base: &Path, rel: &str) -> Result<PathBuf, ConfineError> {
    let p = Path::new(rel);
    if p.is_absolute() {
        return err("absolute paths are not allowed (a Dir capability is a subtree)");
    }
    for comp in p.components() {
        match comp {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => return err("`..` escapes the Dir capability"),
            _ => return err("invalid path component in a Dir-relative path"),
        }
    }
    let joined = base.join(rel);
    let parent = joined.parent().unwrap_or(base);
    let real_parent = std::fs::canonicalize(parent)
        .map_err(|e| ConfineError(format!("cannot access `{}`: {e}", parent.display())))?;
    let real_base = std::fs::canonicalize(base)
        .map_err(|e| ConfineError(format!("invalid Dir base `{}`: {e}", base.display())))?;
    if !real_parent.starts_with(&real_base) {
        return err("path escapes the Dir capability (via symlink)");
    }
    // The parent is confined, but the final component itself could be a
    // pre-existing symlink pointing outside the subtree (unlike `read`, we can't
    // canonicalize a not-yet-existing target). Refuse to write *through* a
    // symlink leaf. Same canonicalize-then-use TOCTOU caveat as `resolve` — the
    // race-free fix is the planned `openat2`/WASI-preopen substrate.
    if let Ok(meta) = std::fs::symlink_metadata(&joined) {
        if meta.file_type().is_symlink() {
            return err("path escapes the Dir capability (the target is a symlink)");
        }
    }
    Ok(joined)
}
