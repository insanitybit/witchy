//! RFC-0013 capability grant documents.
//!
//! A grant document is a declarative manifest of the authority the host hands to
//! `main` — a reviewable, diffable alternative to a sprawl of CLI flags. Crucially
//! it is a *request the host approves*, never a self-grant (RFC-0003), and because
//! witchy already COMPUTES a program's capability footprint, a grant can be
//! cross-checked against it: an over-request (authority the code never exercises)
//! is flagged, and an under-grant (authority the code needs but the grant withholds)
//! fails early. This is the launch-side mirror of the package manager's
//! publish-time footprint gate.
//!
//! ```toml
//! [files]
//! config = { path = "config.toml", rights = ["Read"] }
//! [dirs]
//! data = { root = "./data", rights = ["Read", "Write"] }
//! [net]
//! github = ["github.com:443"]
//! [fetch]
//! api = ["https://api.github.com"]
//! [env]
//! runtime = ["HOME", "LANG"]
//! [exec]
//! runner = ["bin/git"]
//! [secrets]
//! gh = { from = "env:GITHUB_OAUTH" }
//! [user_caps]
//! ui = { type = "UiRoot", policy = "coven-web" }
//! ```

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::capabilities::{CapSet, Rights};

/// A parsed grant document: the capabilities the host will hand to `main`, keyed
/// by the `main` parameter name they bind to.
#[derive(Debug, Default, Deserialize)]
pub struct GrantDoc {
    #[serde(default)]
    pub files: BTreeMap<String, FileGrant>,
    #[serde(default)]
    pub dirs: BTreeMap<String, DirGrant>,
    /// Each entry is a scheme-agnostic `host:port` allowlist (RFC-0011 policy
    /// values; `tls:` is a connect-time choice, not an allowlist scheme).
    #[serde(default)]
    pub net: BTreeMap<String, Vec<String>>,
    /// Origin allowlists keyed by the `Fetch` parameter they bind to.
    #[serde(default)]
    pub fetch: BTreeMap<String, Vec<String>>,
    /// Environment-variable name allowlists keyed by the `Env` parameter they
    /// bind to. An empty list grants a real Env that can read no names.
    #[serde(default)]
    pub env: BTreeMap<String, Vec<String>>,
    /// Executable-name allowlists and optional inherited child-needs keyed by
    /// the `Exec` parameter they bind to.
    #[serde(default)]
    pub exec: BTreeMap<String, ExecGrant>,
    #[serde(default)]
    pub secrets: BTreeMap<String, SecretGrant>,
    /// RFC-0038: bare, library-defined `grantable` capabilities the host mints at
    /// the root, keyed by the `main` parameter they bind to. Each carries a `type`
    /// (the grantable capability's name) plus the policy fields its constructor
    /// consumes. Bare caps confer NO host authority, so they are absent from
    /// `cap_set` (the host cross-check); they are matched on a separate axis
    /// against the program's required grantable caps.
    #[serde(default)]
    pub user_caps: BTreeMap<String, UserCapGrant>,
}

#[derive(Debug, Deserialize)]
pub struct FileGrant {
    pub path: String,
    #[serde(default)]
    pub rights: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct DirGrant {
    pub root: String,
    #[serde(default)]
    pub rights: Vec<String>,
}

#[derive(Debug)]
pub enum ExecGrant {
    Programs(Vec<String>),
    Detailed(ExecGrantDetail),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecGrantDetail {
    #[serde(default)]
    programs: Vec<String>,
    #[serde(default, rename = "child-paths")]
    child_paths: Vec<String>,
}

impl ExecGrant {
    pub fn programs(&self) -> &[String] {
        match self {
            Self::Programs(programs) => programs,
            Self::Detailed(detail) => &detail.programs,
        }
    }

    pub fn child_paths(&self) -> &[String] {
        match self {
            Self::Programs(_) => &[],
            Self::Detailed(detail) => &detail.child_paths,
        }
    }
}

impl<'de> Deserialize<'de> for ExecGrant {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = toml::Value::deserialize(deserializer)?;
        match value {
            toml::Value::Array(values) => Vec::<String>::deserialize(
                toml::Value::Array(values),
            )
            .map(Self::Programs)
            .map_err(serde::de::Error::custom),
            toml::Value::Table(values) => ExecGrantDetail::deserialize(
                toml::Value::Table(values),
            )
            .map(Self::Detailed)
            .map_err(serde::de::Error::custom),
            _ => Err(serde::de::Error::custom(
                "Exec grant must be a program array or a { programs, child-paths } table",
            )),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretGrant {
    /// Where the host RESOLVES the secret from (e.g. `env:GITHUB_OAUTH`). The
    /// document never carries a secret value.
    pub from: String,
    /// (RFC-0060) When true the secret is usable by handle (sign/verify) but NOT
    /// revealable — `crypto.reveal` errors, the same guarantee `--secret …,use-only`
    /// confers on the CLI. Defaults to revealable. `deny_unknown_fields` above makes a
    /// misspelled modifier (`use_only`, `useonly`) a loud parse error rather than a
    /// silently-revealable secret.
    #[serde(default, rename = "use-only")]
    pub use_only: bool,
}

/// RFC-0038: a bare grantable user capability the host mints at the root.
#[derive(Debug, Deserialize)]
pub struct UserCapGrant {
    /// The grantable capability type this binds to (e.g. `UiRoot`).
    #[serde(rename = "type")]
    pub cap_type: String,
    /// The remaining keys: the capability's policy fields, passed to its
    /// constructor. Bare caps carry only policy data (no host authority).
    #[serde(flatten)]
    pub fields: BTreeMap<String, toml::Value>,
}

impl GrantDoc {
    /// Parse a grant document from TOML.
    pub fn parse(src: &str) -> Result<GrantDoc, String> {
        toml::from_str(src).map_err(|e| format!("grant document is not valid TOML: {e}"))
    }

    /// The capability footprint this grant CONFERS — the same `CapSet` shape as a
    /// program's *computed* footprint, so the two diff directly. `Dir`/`File`
    /// carry their declared rights; a `[net]` block confers `Net` (the address
    /// allowlist is enforced separately, so it is presence-level here); `[secrets]`
    /// confers `Secret`/`SecretStore`.
    pub fn cap_set(&self) -> CapSet {
        let mut cs = CapSet::new();
        if !self.files.is_empty() {
            cs.insert("File", collect_rights(self.files.values().flat_map(|f| &f.rights)));
        }
        if !self.dirs.is_empty() {
            cs.insert("Dir", collect_rights(self.dirs.values().flat_map(|d| &d.rights)));
        }
        if !self.net.is_empty() {
            // The doc gives addresses, not verbs; confer `Net` at full verbs (the
            // cross-check treats `Net` at presence level, see `cross_check`).
            cs.insert("Net", ["Connect", "Listen", "Tcp", "Udp", "Uds"].into_iter().collect());
        }
        if !self.fetch.is_empty() {
            cs.insert("Fetch", Rights::new());
        }
        if !self.env.is_empty() {
            cs.insert("Env", Rights::new());
        }
        if !self.exec.is_empty() {
            cs.insert("Exec", Rights::new());
        }
        if !self.secrets.is_empty() {
            // A `[secrets]` section models NAMED store secrets, reached through a
            // `SecretStore` (`SecretStore.get`/`require`). It does NOT confer a bare
            // `Secret` — the sign-only signing key, which is granted at LAUNCH by
            // `--signing-key`, not by this document. Conferring both collapsed the two
            // distinct capabilities, so a `[secrets] signing = {…}` grant silently
            // satisfied a program that actually needs the bare signing-key `Secret`
            // (BUG-117). Bind precisely: `[secrets]` -> `SecretStore` only, so a
            // bare-`Secret` program reports the missing `Secret` as an under-grant.
            cs.insert("SecretStore", Rights::new());
        }
        cs
    }
}

/// Resolve the shared RFC-0013/RFC-0092 environment-backed secret provider.
/// The declarative document or executable plan carries only `env:NAME`; secret
/// bytes are acquired by the trusted host at launch and never serialized.
pub fn resolve_secret_provider(from: &str) -> Result<Vec<u8>, String> {
    if let Some(variable) = from.strip_prefix("env:") {
        if variable.is_empty() {
            return Err("secret resolver `env:` has an empty variable name".to_string());
        }
        std::env::var(variable)
            .map(String::into_bytes)
            .map_err(|_| format!("secret resolver `env:{variable}`: ${variable} is not set"))
    } else {
        Err(format!(
            "unsupported secret resolver `{from}` (expected `env:VAR`)"
        ))
    }
}

/// Canonicalize a declared right string to the static name the footprint uses.
fn right_name(s: &str) -> Option<&'static str> {
    match s {
        "Read" => Some("Read"),
        "Write" => Some("Write"),
        "Exec" => Some("Exec"),
        _ => None,
    }
}

fn collect_rights<'a>(rights: impl Iterator<Item = &'a String>) -> Rights {
    rights.filter_map(|r| right_name(r)).collect()
}

/// The result of cross-checking a grant against a program's computed footprint.
#[derive(Debug, Default)]
pub struct CrossCheck {
    /// Authority the grant confers that the code never exercises — an over-request
    /// (the classic trojan-permission smell). A **warning**, surfaced for review.
    pub over_grant: CapSet,
    /// Authority the code needs that the grant withholds — an under-grant. A **hard
    /// error** at launch (the program would fail at the missing capability anyway).
    pub under_grant: CapSet,
}

impl CrossCheck {
    /// The grant is sufficient (no under-grant). A clean launch may still warn on
    /// an over-grant.
    pub fn sufficient(&self) -> bool {
        self.under_grant.is_empty()
    }

    /// Grant == footprint: nothing to question.
    pub fn clean(&self) -> bool {
        self.under_grant.is_empty() && self.over_grant.is_empty()
    }
}

/// The capabilities a grant document models. `Console`/`Clock` are currently
/// host-provided outside the document.
const GRANTABLE: &[&str] =
    &["Dir", "File", "Net", "Fetch", "Env", "Exec", "Secret", "SecretStore"];

fn grantable_only(cs: &CapSet) -> CapSet {
    cs.iter()
        .filter(|(k, _)| GRANTABLE.contains(*k))
        .map(|(k, v)| (*k, v.clone()))
        .collect()
}

/// Compare a grant's conferred authority (`grant`) to a program's required
/// footprint (`footprint`, typically `analyze(module).total`). Only the
/// grant-document-modeled capabilities (`GRANTABLE`) are compared. `Net` is
/// compared at presence level (the document expresses addresses, not the verb
/// rights the footprint tracks), so a verb-level delta on a shared `Net` is not a
/// finding.
pub fn cross_check(grant: &CapSet, footprint: &CapSet) -> CrossCheck {
    let grant = grantable_only(grant);
    let footprint = grantable_only(footprint);
    let mut over_grant = crate::capabilities::cap_delta(&grant, &footprint);
    let mut under_grant = crate::capabilities::cap_delta(&footprint, &grant);
    // `Net`: if BOTH sides have it, the capability is satisfied; a verb-level
    // difference is not expressible in the document, so it is not a finding.
    if grant.contains_key("Net") && footprint.contains_key("Net") {
        over_grant.remove("Net");
        under_grant.remove("Net");
    }
    CrossCheck { over_grant, under_grant }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cs(entries: &[(&'static str, &[&'static str])]) -> CapSet {
        entries
            .iter()
            .map(|(c, rs)| (*c, rs.iter().copied().collect::<Rights>()))
            .collect()
    }

    #[test]
    fn parses_a_grant_document() {
        let doc = GrantDoc::parse(
            "[files]\nconfig = { path = \"config.toml\", rights = [\"Read\"] }\n\
             [dirs]\ndata = { root = \"./data\", rights = [\"Read\", \"Write\"] }\n\
             [net]\ngithub = [\"github.com:443\", \"api.github.com:443\"]\n\
             [fetch]\napi = [\"https://api.github.com\"]\n\
             [env]\nruntime = [\"HOME\", \"LANG\"]\n\
             [secrets]\ngh = { from = \"env:GITHUB_OAUTH\" }\n",
        )
        .expect("valid grant doc");
        assert_eq!(doc.files["config"].path, "config.toml");
        assert_eq!(doc.files["config"].rights, vec!["Read"]);
        assert_eq!(doc.dirs["data"].root, "./data");
        assert_eq!(doc.net["github"], vec!["github.com:443", "api.github.com:443"]);
        assert_eq!(doc.fetch["api"], vec!["https://api.github.com"]);
        assert_eq!(doc.env["runtime"], vec!["HOME", "LANG"]);
        assert_eq!(doc.secrets["gh"].from, "env:GITHUB_OAUTH");
    }

    #[test]
    fn parses_user_caps_section() {
        // (RFC-0038) a `[user_caps]` entry binds a grantable cap by `main` param
        // name, with a `type` and arbitrary policy fields.
        let doc = GrantDoc::parse(
            "[user_caps]\nui = { type = \"UiRoot\", policy = \"coven-web\", app_id = \"web\" }\n",
        )
        .expect("valid grant doc");
        let uc = &doc.user_caps["ui"];
        assert_eq!(uc.cap_type, "UiRoot");
        assert_eq!(uc.fields["policy"].as_str(), Some("coven-web"));
        assert_eq!(uc.fields["app_id"].as_str(), Some("web"));
        // Bare user caps confer no host authority — absent from the host cap_set.
        assert!(doc.cap_set().is_empty(), "bare user caps add no host authority");
    }

    #[test]
    fn cap_set_confers_the_declared_authority() {
        let doc = GrantDoc::parse(
            "[files]\nc = { path = \"c\", rights = [\"Read\"] }\n[dirs]\nd = { root = \".\", rights = [\"Write\"] }\n",
        )
        .unwrap();
        let g = doc.cap_set();
        assert_eq!(g.get("File"), Some(&["Read"].into_iter().collect::<Rights>()));
        assert_eq!(g.get("Dir"), Some(&["Write"].into_iter().collect::<Rights>()));
    }

    #[test]
    fn fetch_section_confers_fetch_without_socket_authority() {
        let doc =
            GrantDoc::parse("[fetch]\napi = [\"https://api.example.com\"]\n").unwrap();
        let grant = doc.cap_set();
        assert!(grant.contains_key("Fetch"));
        assert!(!grant.contains_key("Net"));
    }

    #[test]
    fn env_section_confers_env_including_an_empty_name_set() {
        let named = GrantDoc::parse("[env]\nruntime = [\"HOME\", \"LANG\"]\n").unwrap();
        assert!(named.cap_set().contains_key("Env"));
        let empty = GrantDoc::parse("[env]\nruntime = []\n").unwrap();
        assert!(empty.cap_set().contains_key("Env"));
    }

    #[test]
    fn exec_section_confers_exec_including_an_empty_program_set() {
        let named = GrantDoc::parse("[exec]\nrunner = [\"git\", \"witchy\"]\n").unwrap();
        assert!(named.cap_set().contains_key("Exec"));
        let empty = GrantDoc::parse("[exec]\nrunner = []\n").unwrap();
        assert!(empty.cap_set().contains_key("Exec"));
        let detailed = GrantDoc::parse(
            "[exec]\nrunner = { programs = [\"git\"], child-paths = [\"~/.gitconfig\"] }\n",
        )
        .unwrap();
        assert_eq!(detailed.exec["runner"].programs(), ["git"]);
        assert_eq!(detailed.exec["runner"].child_paths(), ["~/.gitconfig"]);
        let error = GrantDoc::parse(
            "[exec]\nrunner = { programs = [\"git\"], child_paths = [\"oops\"] }\n",
        )
        .unwrap_err();
        assert!(error.contains("unknown field"), "{error}");
    }

    /// (BUG-117) A `[secrets]` section confers `SecretStore` (named store secrets)
    /// but NOT the bare `Secret` (the launch-granted signing key). So a program that
    /// takes a bare `Secret` is UNDER-granted by a `[secrets]` document — the grant
    /// no longer collapses the two distinct capabilities and silently passes.
    #[test]
    fn secrets_section_confers_secretstore_not_bare_secret() {
        let doc = GrantDoc::parse("[secrets]\nsigning = { from = \"env:SIGNING_KEY\" }\n").unwrap();
        let g = doc.cap_set();
        assert!(g.contains_key("SecretStore"), "[secrets] must confer SecretStore");
        assert!(!g.contains_key("Secret"), "[secrets] must NOT confer a bare Secret (the signing key)");

        // A program needing a bare `Secret` is under-granted by this document.
        let bare_secret = cs(&[("Console", &[]), ("Secret", &[])]);
        let r = cross_check(&g, &bare_secret);
        assert!(!r.sufficient(), "a bare-Secret program must not be satisfied by [secrets]");
        assert!(r.under_grant.contains_key("Secret"), "the missing Secret is the finding: {r:?}");

        // A program using a `SecretStore` is satisfied.
        let store = cs(&[("Console", &[]), ("SecretStore", &[])]);
        assert!(cross_check(&g, &store).sufficient(), "[secrets] satisfies a SecretStore program");
    }

    #[test]
    fn cross_check_flags_over_and_under_grants() {
        // Footprint: code reads a File and writes a Dir.
        let footprint = cs(&[("File", &["Read"]), ("Dir", &["Write"]), ("Console", &[])]);

        // Exact grant -> clean.
        let exact = cs(&[("File", &["Read"]), ("Dir", &["Write"]), ("Console", &[])]);
        let r = cross_check(&exact, &footprint);
        assert!(r.clean(), "exact grant is clean: {r:?}");

        // Over-grant: also confers Net, which the code never uses -> warn (not fatal).
        let over = cs(&[("File", &["Read"]), ("Dir", &["Write"]), ("Console", &[]), ("Net", &["Connect"])]);
        let r = cross_check(&over, &footprint);
        assert!(r.sufficient(), "an over-grant still launches");
        assert!(r.over_grant.contains_key("Net"), "Net over-grant flagged: {r:?}");

        // Under-grant: grant withholds the Dir the code needs -> hard error.
        let under = cs(&[("File", &["Read"]), ("Console", &[])]);
        let r = cross_check(&under, &footprint);
        assert!(!r.sufficient(), "an under-grant must fail");
        assert!(r.under_grant.contains_key("Dir"), "Dir under-grant flagged: {r:?}");

        // Rights-level under-grant: grant gives Dir[Read] but code needs Dir[Write].
        let under_rights = cs(&[("File", &["Read"]), ("Dir", &["Read"]), ("Console", &[])]);
        let r = cross_check(&under_rights, &footprint);
        assert!(!r.sufficient(), "a missing right is an under-grant");
        assert!(r.under_grant.get("Dir").is_some_and(|d| d.contains("Write")));
    }

    #[test]
    fn net_is_compared_at_presence_level() {
        // The doc confers Net at full verbs; the footprint needs only Connect+Tcp.
        // A shared Net is satisfied — no verb-level over/under finding.
        let footprint = cs(&[("Net", &["Connect", "Tcp"]), ("Console", &[])]);
        let grant = GrantDoc::parse("[net]\nx = [\"h:1\"]\n").unwrap().cap_set();
        let r = cross_check(&grant, &footprint);
        assert!(r.clean(), "a shared Net is clean regardless of verbs: {r:?}");

        // But Net entirely absent from the grant while the code needs it -> under.
        let r = cross_check(&CapSet::new(), &footprint);
        assert!(r.under_grant.contains_key("Net"), "missing Net is an under-grant");
    }
}
