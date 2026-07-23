//! RFC-0102 host grant menus.
//!
//! A menu says what one concrete host can originate. A program requirement is
//! derived from the compiler's capability footprint, then checked by subset:
//! every required family and right must be present in the menu. Non-capability
//! host inputs such as argv and VM workers are tracked as typed facilities so
//! consumers do not grow ad-hoc side lists.

use std::collections::BTreeMap;

use serde::Deserialize;
use witchy_cap_model::{CapabilityClass, CapabilityKind, CapabilityRight};

use crate::capabilities::CapSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MenuAxis {
    Runtime,
    Build,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Determinism {
    Deterministic,
    Nondeterministic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostFacility {
    Argv,
    Vm,
}

impl HostFacility {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Argv => "argv",
            Self::Vm => "vm",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "argv" => Some(Self::Argv),
            "vm" => Some(Self::Vm),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityRequirement {
    pub kind: CapabilityKind,
    pub rights: Vec<CapabilityRight>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostRequirements {
    pub capabilities: Vec<CapabilityRequirement>,
    pub facilities: Vec<HostFacility>,
}

impl HostRequirements {
    pub fn from_cap_set(capabilities: &CapSet) -> Result<Self, String> {
        let mut requirements = Vec::with_capacity(capabilities.len());
        for (name, rights) in capabilities {
            let kind = CapabilityKind::from_name(name)
                .ok_or_else(|| format!("unknown capability `{name}` in host requirements"))?;
            let mut typed_rights = Vec::with_capacity(rights.len());
            for right in rights {
                let right = kind.right(right).ok_or_else(|| {
                    format!("unknown right `{right}` for capability `{}`", kind.name())
                })?;
                if !typed_rights.contains(&right) {
                    typed_rights.push(right);
                }
            }
            order_rights(kind, &mut typed_rights);
            requirements.push(CapabilityRequirement {
                kind,
                rights: typed_rights,
            });
        }
        Ok(Self {
            capabilities: requirements,
            facilities: Vec::new(),
        })
    }

    pub fn require_facility(&mut self, facility: HostFacility) {
        if !self.facilities.contains(&facility) {
            self.facilities.push(facility);
        }
    }
}

#[derive(Debug, Clone)]
pub struct HostGrant {
    pub kind: CapabilityKind,
    pub rights: Vec<CapabilityRight>,
    pub provider: String,
    pub determinism: Determinism,
    /// Provider-specific grant shape, retained for launch/binder consumers.
    pub settings: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone)]
pub struct FacilityGrant {
    pub facility: HostFacility,
    pub provider: String,
    pub determinism: Determinism,
    pub settings: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone)]
pub struct HostMenu {
    pub host: String,
    pub axis: MenuAxis,
    pub grants: Vec<HostGrant>,
    pub facilities: Vec<FacilityGrant>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct PortabilityReport {
    pub missing_capabilities: Vec<CapabilityRequirement>,
    pub missing_rights: Vec<CapabilityRequirement>,
    pub missing_facilities: Vec<HostFacility>,
}

impl PortabilityReport {
    pub fn portable(&self) -> bool {
        self.missing_capabilities.is_empty()
            && self.missing_rights.is_empty()
            && self.missing_facilities.is_empty()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMenu {
    host: String,
    axis: MenuAxis,
    grants: BTreeMap<String, RawGrant>,
    #[serde(default)]
    facilities: BTreeMap<String, RawGrant>,
}

#[derive(Debug, Deserialize)]
struct RawGrant {
    provider: String,
    determinism: Determinism,
    #[serde(default)]
    rights: Vec<String>,
    #[serde(flatten)]
    settings: BTreeMap<String, toml::Value>,
}

impl HostMenu {
    pub fn parse(src: &str) -> Result<Self, String> {
        let raw: RawMenu =
            toml::from_str(src).map_err(|error| format!("host menu is not valid TOML: {error}"))?;
        if raw.host.trim().is_empty() {
            return Err("host menu has an empty `host` name".to_string());
        }

        let expected_class = match raw.axis {
            MenuAxis::Runtime => CapabilityClass::Host,
            MenuAxis::Build => CapabilityClass::Build,
        };
        let mut grants = Vec::with_capacity(raw.grants.len());
        for (name, raw_grant) in raw.grants {
            let kind = CapabilityKind::from_name(&name)
                .ok_or_else(|| format!("host menu contains unknown capability `{name}`"))?;
            if kind.class() != expected_class {
                return Err(format!(
                    "{} menu cannot grant `{}` ({:?} capability)",
                    axis_name(raw.axis),
                    kind.name(),
                    kind.class()
                ));
            }
            if raw_grant.provider.trim().is_empty() {
                return Err(format!(
                    "host menu capability `{}` has an empty provider",
                    kind.name()
                ));
            }
            let rights = parse_rights(kind, raw_grant.rights)?;
            grants.push(HostGrant {
                kind,
                rights,
                provider: raw_grant.provider,
                determinism: raw_grant.determinism,
                settings: raw_grant.settings,
            });
        }

        let mut facilities = Vec::with_capacity(raw.facilities.len());
        for (name, raw_grant) in raw.facilities {
            let facility = HostFacility::from_name(&name)
                .ok_or_else(|| format!("host menu contains unknown facility `{name}`"))?;
            if raw.axis == MenuAxis::Build {
                return Err(format!(
                    "build menu cannot provide runtime facility `{}`",
                    facility.name()
                ));
            }
            if !raw_grant.rights.is_empty() {
                return Err(format!(
                    "host facility `{}` cannot declare capability rights",
                    facility.name()
                ));
            }
            if raw_grant.provider.trim().is_empty() {
                return Err(format!(
                    "host menu facility `{}` has an empty provider",
                    facility.name()
                ));
            }
            facilities.push(FacilityGrant {
                facility,
                provider: raw_grant.provider,
                determinism: raw_grant.determinism,
                settings: raw_grant.settings,
            });
        }

        Ok(Self {
            host: raw.host,
            axis: raw.axis,
            grants,
            facilities,
        })
    }

    pub fn check(&self, requirements: &HostRequirements) -> PortabilityReport {
        let mut report = PortabilityReport::default();
        for requirement in &requirements.capabilities {
            let Some(grant) = self
                .grants
                .iter()
                .find(|grant| grant.kind == requirement.kind)
            else {
                report.missing_capabilities.push(requirement.clone());
                continue;
            };
            let missing: Vec<_> = requirement
                .rights
                .iter()
                .copied()
                .filter(|right| !grant.rights.contains(right))
                .collect();
            if !missing.is_empty() {
                report.missing_rights.push(CapabilityRequirement {
                    kind: requirement.kind,
                    rights: missing,
                });
            }
        }
        for facility in &requirements.facilities {
            if !self
                .facilities
                .iter()
                .any(|grant| grant.facility == *facility)
            {
                report.missing_facilities.push(*facility);
            }
        }
        report
    }
}

fn parse_rights(
    kind: CapabilityKind,
    names: Vec<String>,
) -> Result<Vec<CapabilityRight>, String> {
    if kind.rights().is_empty() && !names.is_empty() {
        return Err(format!(
            "right-free capability `{}` cannot declare rights",
            kind.name()
        ));
    }
    if !kind.rights().is_empty() && names.is_empty() {
        return Err(format!(
            "host menu capability `{}` must explicitly declare its provided rights",
            kind.name()
        ));
    }
    let mut rights = Vec::with_capacity(names.len());
    for name in names {
        let right = kind
            .right(&name)
            .ok_or_else(|| format!("unknown right `{name}` for capability `{}`", kind.name()))?;
        if rights.contains(&right) {
            return Err(format!(
                "duplicate right `{name}` for capability `{}`",
                kind.name()
            ));
        }
        rights.push(right);
    }
    order_rights(kind, &mut rights);
    Ok(rights)
}

fn order_rights(kind: CapabilityKind, rights: &mut [CapabilityRight]) {
    rights.sort_by_key(|right| {
        kind.rights()
            .iter()
            .position(|candidate| candidate == right)
            .unwrap_or(usize::MAX)
    });
}

const fn axis_name(axis: MenuAxis) -> &'static str {
    match axis {
        MenuAxis::Runtime => "runtime",
        MenuAxis::Build => "build",
    }
}

pub const NATIVE_MENU: &str = include_str!("../../../menus/native.toml");
pub const BROWSER_MENU: &str = include_str!("../../../menus/browser.toml");
pub const TRUSTED_EXE_MENU: &str = include_str!("../../../menus/trusted-exe.toml");
pub const BUILD_MENU: &str = include_str!("../../../menus/build.toml");

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::capabilities::Rights;

    fn requirements(entries: &[(&'static str, &[&'static str])]) -> HostRequirements {
        let caps: CapSet = entries
            .iter()
            .map(|(kind, rights)| {
                (
                    *kind,
                    rights.iter().copied().collect::<BTreeSet<&'static str>>(),
                )
            })
            .collect();
        HostRequirements::from_cap_set(&caps).expect("valid test requirements")
    }

    #[test]
    fn every_published_menu_is_valid_and_typed() {
        for source in [NATIVE_MENU, BROWSER_MENU, TRUSTED_EXE_MENU, BUILD_MENU] {
            let menu = HostMenu::parse(source).expect("published menu must parse");
            assert!(!menu.host.is_empty());
        }
    }

    #[test]
    fn browser_menu_matches_the_current_provider_boundary() {
        let menu = HostMenu::parse(BROWSER_MENU).unwrap();
        let supported = requirements(&[
            ("Console", &[]),
            ("Clock", &[]),
            ("Env", &[]),
            ("Dir", &["Read", "Write"]),
        ]);
        assert!(menu.check(&supported).portable());

        for denied in ["Rand", "Secret", "SecretStore", "File", "Net", "Exec"] {
            let requirement = if denied == "File" {
                requirements(&[(denied, &["Read"])])
            } else if denied == "Net" {
                requirements(&[(denied, &["Connect", "Tcp"])])
            } else {
                requirements(&[(denied, &[])])
            };
            assert_eq!(
                menu.check(&requirement).missing_capabilities[0].kind.name(),
                denied
            );
        }

        let mut facilities = HostRequirements::default();
        facilities.require_facility(HostFacility::Argv);
        facilities.require_facility(HostFacility::Vm);
        assert_eq!(
            menu.check(&facilities).missing_facilities,
            vec![HostFacility::Argv, HostFacility::Vm]
        );
    }

    #[test]
    fn portability_is_right_precise() {
        let menu = HostMenu::parse(
            "host = \"read-only\"\naxis = \"runtime\"\n\
             [grants.Dir]\nprovider = \"memory\"\ndeterminism = \"deterministic\"\nrights = [\"Read\"]\n",
        )
        .unwrap();
        let report = menu.check(&requirements(&[("Dir", &["Read", "Write"])]));
        assert!(report.missing_capabilities.is_empty());
        assert_eq!(report.missing_rights[0].rights, vec![CapabilityRight::Write]);
    }

    #[test]
    fn malformed_or_non_root_grants_fail_closed() {
        for (source, expected) in [
            (
                "host = \"bad\"\naxis = \"runtime\"\n\
                 [grants.Socket]\nprovider = \"native\"\ndeterminism = \"nondeterministic\"\n",
                "cannot grant `Socket`",
            ),
            (
                "host = \"bad\"\naxis = \"runtime\"\n\
                 [grants.Dir]\nprovider = \"memory\"\ndeterminism = \"deterministic\"\nrights = [\"Execute\"]\n",
                "unknown right `Execute`",
            ),
            (
                "host = \"bad\"\naxis = \"runtime\"\n\
                 [grants.Clock]\nprovider = \"native\"\ndeterminism = \"nondeterministic\"\nrights = [\"Read\"]\n",
                "right-free capability `Clock`",
            ),
        ] {
            let error = HostMenu::parse(source).unwrap_err();
            assert!(error.contains(expected), "{error}");
        }
    }

    #[test]
    fn unknown_footprint_entries_do_not_become_stringly_requirements() {
        let caps: CapSet = [("MadeUp", Rights::new())].into_iter().collect();
        assert_eq!(
            HostRequirements::from_cap_set(&caps).unwrap_err(),
            "unknown capability `MadeUp` in host requirements"
        );
    }
}
