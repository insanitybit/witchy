//! Single-guest process-confinement activation.
//!
//! This boundary is intentionally above the reusable runtime: native outer
//! policy is irreversible and must never be armed by tests, parity, servers, or
//! a process that intends to host another VM with different grants.

use crate::runtime;
use witchy_confinement::{EnforcementMode, LayerStatus};

pub(crate) fn arm(
    caps: &runtime::Capabilities,
    mode: EnforcementMode,
) -> Result<(), String> {
    if matches!(mode, EnforcementMode::Disabled) {
        return Ok(());
    }
    materialize_write_targets(caps)?;
    let mut policy = caps.confinement_policy();
    add_runtime_cache_rule(&mut policy);
    let report = witchy_confinement::apply(&policy, mode)
        .map_err(|error| error.to_string())?;
    for layer in report.layers {
        let status = match layer.status {
            LayerStatus::Disabled => "disabled",
            LayerStatus::Enforced => "enforced",
            LayerStatus::Partial => "partial",
            LayerStatus::Unavailable => "unavailable",
        };
        eprintln!(
            "confinement: layer={:?} provider={} status={} detail={}",
            layer.layer, layer.provider, status, layer.detail
        );
    }
    Ok(())
}

/// The runtime's own compilation cache is written AFTER the fence goes up:
/// `arm` runs before `Runtime::batch`, and Wasmtime populates
/// `$XDG_CACHE_HOME/witchy` while compiling the guest. That directory is host
/// infrastructure rather than guest authority, so it is added here instead of
/// in `confinement_policy()` — the same separation `linux.rs` uses for the
/// dynamic loader's library roots, and for the same reason: the derived policy
/// stays a deterministic function of the grants.
///
/// This is a real widening, and it is bounded deliberately: read+write on one
/// cache directory, no execute. The cache is not a security boundary — every
/// hit re-enters Wasmtime through `Module::new` validation — so a payload that
/// can write there can waste work, not gain authority.
///
/// Found on macOS, the first platform whose provider actually enforces this
/// path. Landlock reports unavailable in the containerized CI that otherwise
/// covers it, so the gap was invisible on Linux rather than absent there.
fn add_runtime_cache_rule(policy: &mut witchy_confinement::Policy) {
    let Some(base) = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|home| std::path::PathBuf::from(home).join(".cache"))
        })
    else {
        return;
    };
    let dir = base.join("witchy");
    // The rule must anchor on an existing path for the Landlock provider, and
    // creating it here is what the runtime would have done a moment later.
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    policy.add_fs_rule(
        dir,
        witchy_confinement::FsScope::Tree,
        witchy_confinement::FsAccess::new(true, true, false),
    );
}

/// A `File[Write]` grant may name a file that does not exist yet — the program
/// will create it by writing. Landlock rules are anchored on an open fd, so a
/// rule for a not-yet-created file cannot be installed and confinement fails to
/// arm. Materialize each write-capable file grant's target (its parent dirs plus
/// an empty file) BEFORE arming, so the tight file-scoped Landlock rule opens
/// cleanly. This never widens the policy — the rule stays anchored on the exact
/// granted path, not a parent tree. A read-only grant on a missing path is left
/// untouched (there is nothing to read; confinement will surface the error).
fn materialize_write_targets(caps: &runtime::Capabilities) -> Result<(), String> {
    // File grants are ordered preopened_files then file_grants, with file_rights
    // parallel-indexed (an absent entry means full rights) — the exact order and
    // default `confinement_policy()` uses when it builds the file rules.
    let paths = caps
        .preopened_files
        .iter()
        .map(|f| f.display_path().to_path_buf())
        .chain(caps.file_grants.iter().cloned());
    for (index, path) in paths.enumerate() {
        let writable = caps
            .file_rights
            .get(index)
            .map_or(true, |rights| rights.write);
        if !writable || path.exists() {
            continue;
        }
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    format!("cannot prepare write-grant directory `{}`: {error}", parent.display())
                })?;
            }
        }
        // O_CREAT|O_EXCL: create the empty file, tolerating a concurrent create
        // (a racing creator produced the same target we were about to). Any other
        // error is a real preparation failure.
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(format!(
                    "cannot prepare write-grant target `{}`: {error}",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}
