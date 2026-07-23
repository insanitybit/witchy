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
