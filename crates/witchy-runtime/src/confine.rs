//! Path confinement for the `Dir` capability — the SINGLE, shared implementation
//! of the subtree escape checks (lexical `..`/absolute rejection + symlink-aware
//! canonicalization). Both the compiled-WASM sandbox (`runtime.rs`) and the
//! interpreter oracle resolve through these functions, so the two backends can
//! never diverge on which paths a `Dir` capability reaches. Keeping it one
//! implementation is a security invariant — see `spec/binary-distribution.md`.
//!
//! `resolve`/`resolve_write` perform the confinement *check* and return a path;
//! both backends then open that path through [`open_read`]/[`open_write`], which
//! pass `O_NOFOLLOW` so the final component cannot be swapped for a symlink
//! between the check and the open (RFC-0103, phase 1 — the leaf `TOCTOU`).
//! Parent-component races and full tree confinement are the follow-up
//! (`openat2`/`RESOLVE_BENEATH` on Linux, a dir-anchored walk elsewhere).

use std::fs::{File, OpenOptions};
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
/// This is the confinement *check*; open the returned path through [`open_read`]
/// so a final-component symlink swapped in after this check cannot redirect the
/// read (the leaf `TOCTOU`). Parent-component races are the RFC-0103 follow-up.
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
    // symlink leaf. This check races the actual open, so EVERY write of this
    // path must go through [`open_write`]/[`open_append`], which re-check
    // atomically with `O_NOFOLLOW`; this early rejection just gives the clearer
    // confinement message in the common (non-racing) case.
    if let Ok(meta) = std::fs::symlink_metadata(&joined) {
        if meta.file_type().is_symlink() {
            return err("path escapes the Dir capability (the target is a symlink)");
        }
    }
    Ok(joined)
}

/// Open a confined path for reading with `O_NOFOLLOW`, so a final-component
/// symlink swapped in after [`resolve`]'s check cannot redirect the read out of
/// the subtree. `resolve` returns a canonical (symlink-free) path, so a legitimate
/// target is never a symlink and this never rejects one; only the racing swap
/// trips it. Both backends read through here so they open identically.
///
/// Non-confinement I/O errors (missing file, permission) surface unchanged for
/// the caller to format; a swapped-in symlink surfaces as the OS `O_NOFOLLOW`
/// error (`ELOOP`), which is an adversarial-only path, never a normal read.
pub fn open_read(path: &Path) -> std::io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.read(true);
    with_nofollow(&mut opts);
    opts.open(path)
}

/// Open a confined path for writing (`create`/`truncate`) with `O_NOFOLLOW`, so a
/// final-component symlink swapped in after [`resolve_write`]'s check cannot make
/// the write escape the subtree. `resolve_write` already rejects a symlink leaf;
/// this closes the race between that check and the open. Both backends write
/// through here.
pub fn open_write(path: &Path) -> std::io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    with_nofollow(&mut opts);
    opts.open(path)
}

/// Open a confined path for appending (`create`/`append`) with `O_NOFOLLOW`, the
/// same leaf-symlink guard as [`open_write`] for the non-truncating write op.
/// `resolve_write` gates this path too, so both backends append through here.
pub fn open_append(path: &Path) -> std::io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.append(true).create(true);
    with_nofollow(&mut opts);
    opts.open(path)
}

/// Add `O_NOFOLLOW` (refuse a symlink final component) where the platform has it.
/// The wasm build has no host filesystem, so its fallback simply omits the flag.
#[cfg(unix)]
fn with_nofollow(opts: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    opts.custom_flags(libc::O_NOFOLLOW);
}

#[cfg(not(unix))]
fn with_nofollow(_opts: &mut OpenOptions) {}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    /// A fresh scratch dir, removed on drop.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!("witchy-confine-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Scratch(p)
        }
        fn join(&self, s: &str) -> PathBuf {
            self.0.join(s)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn open_read_reads_a_regular_file() {
        let s = Scratch::new();
        std::fs::write(s.join("f"), b"hello").unwrap();
        let mut buf = String::new();
        open_read(&s.join("f")).unwrap().read_to_string(&mut buf).unwrap();
        assert_eq!(buf, "hello");
    }

    #[test]
    fn open_write_creates_and_truncates_a_regular_file() {
        let s = Scratch::new();
        open_write(&s.join("f")).unwrap().write_all(b"one").unwrap();
        open_write(&s.join("f")).unwrap().write_all(b"2").unwrap();
        assert_eq!(std::fs::read(s.join("f")).unwrap(), b"2");
    }

    /// The security property: a final-component symlink pointing outside the
    /// subtree (the leaf-swap TOCTOU) is refused at open time, so neither a read
    /// nor a write follows it — even though the path string looks in-subtree.
    #[test]
    fn a_symlink_leaf_is_never_followed() {
        let s = Scratch::new();
        let secret = s.join("secret");
        std::fs::write(&secret, b"private").unwrap();
        let link = s.join("leaf");
        std::os::unix::fs::symlink(&secret, &link).unwrap();

        // Read must not disclose the symlink target.
        assert!(open_read(&link).is_err(), "open_read followed a symlink leaf");

        // Write must not clobber the symlink target.
        assert!(open_write(&link).is_err(), "open_write followed a symlink leaf");
        // Append must not extend the symlink target either.
        assert!(open_append(&link).is_err(), "open_append followed a symlink leaf");
        assert_eq!(std::fs::read(&secret).unwrap(), b"private", "write escaped through the symlink");
    }
}
