use std::fmt;

use crate::{EnforcementMode, Policy};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Layer {
    Filesystem,
    Tcp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayerStatus {
    Disabled,
    Enforced,
    Partial,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LayerReport {
    pub layer: Layer,
    pub provider: &'static str,
    pub status: LayerStatus,
    pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnforcementReport {
    pub layers: Vec<LayerReport>,
}

impl EnforcementReport {
    pub fn fully_enforced(&self) -> bool {
        self.layers
            .iter()
            .all(|layer| layer.status == LayerStatus::Enforced)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnforcementError(pub String);

impl fmt::Display for EnforcementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for EnforcementError {}

pub fn apply(
    policy: &Policy,
    mode: EnforcementMode,
) -> Result<EnforcementReport, EnforcementError> {
    if mode == EnforcementMode::Disabled {
        return Ok(EnforcementReport {
            layers: vec![
                LayerReport {
                    layer: Layer::Filesystem,
                    provider: platform_provider(),
                    status: LayerStatus::Disabled,
                    detail: "process-wide confinement was not requested".into(),
                },
                LayerReport {
                    layer: Layer::Tcp,
                    provider: platform_provider(),
                    status: LayerStatus::Disabled,
                    detail: "process-wide confinement was not requested".into(),
                },
            ],
        });
    }

    #[cfg(target_os = "linux")]
    {
        crate::linux::apply(policy, mode)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = policy;
        let report = EnforcementReport {
            layers: vec![
                LayerReport {
                    layer: Layer::Filesystem,
                    provider: platform_provider(),
                    status: LayerStatus::Unavailable,
                    detail: "no native outer-filesystem provider on this platform".into(),
                },
                LayerReport {
                    layer: Layer::Tcp,
                    provider: platform_provider(),
                    status: LayerStatus::Unavailable,
                    detail: "no native TCP confinement provider on this platform".into(),
                },
            ],
        };
        if mode == EnforcementMode::Required {
            Err(EnforcementError(
                "required platform confinement is unavailable on this host".into(),
            ))
        } else {
            Ok(report)
        }
    }
}

const fn platform_provider() -> &'static str {
    if cfg!(target_os = "linux") {
        "landlock"
    } else {
        "none"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_mode_never_mutates_process_policy() {
        let report = apply(&Policy::default(), EnforcementMode::Disabled).unwrap();
        assert!(report
            .layers
            .iter()
            .all(|layer| layer.status == LayerStatus::Disabled));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn required_mode_rejects_a_host_without_a_provider() {
        let error = apply(&Policy::default(), EnforcementMode::Required).unwrap_err();
        assert!(error.0.contains("unavailable"));
    }
}
