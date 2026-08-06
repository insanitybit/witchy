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
    let report = witchy_confinement::apply(&caps.confinement_policy(), mode)
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
