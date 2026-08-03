//! RFC-0109 integrated Glamour project and production-build commands.
//!
//! This module owns the native orchestration boundary. Application code remains
//! Witchy, Glamour remains a toolchain-owned rune, and the browser receives only
//! content-addressed Wasm plus the deny-by-omission runtime.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

mod dev;
mod static_site;

use static_site::{
    check_static_project, evaluate_static_site, write_static_production,
};

const WEB_USAGE: &str = "\
web command usage:
  witchy new --web <directory>
  witchy test --web [directory]
  witchy build --web [--out <directory>] [directory]
  witchy doctor --web [--format human|json] [directory]
  witchy dev [--host 127.0.0.1] [--port 3000] [directory]";

const REQUIRED_EXPORTS: &[&str] = &[
    "__glamour_protocol_version",
    "__glamour_input_reserve",
    "__glamour_init",
    "__glamour_resume",
    "__glamour_dispatch",
    "__glamour_output_length",
    "__glamour_output_release",
    "__glamour_dispose",
];

const FORBIDDEN_PRODUCTION_EXPORTS: &[&str] = &[
    "__glamour_dev_snapshot",
    "__glamour_dev_snapshot_length",
    "__glamour_dev_restore",
    "__glamour_dev_metadata",
    "__glamour_dev_changes",
    "__glamour_dev_changes_length",
];

#[derive(Debug)]
pub(crate) struct WebCommandError {
    message: String,
    usage: bool,
}

impl WebCommandError {
    fn usage(message: impl Into<String>) -> Self {
        Self { message: message.into(), usage: true }
    }

    fn failure(message: impl Into<String>) -> Self {
        Self { message: message.into(), usage: false }
    }

    pub(crate) fn exit_code(&self) -> i32 {
        if self.usage { 2 } else { 1 }
    }
}

impl std::fmt::Display for WebCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.usage {
            write!(f, "{}\n{WEB_USAGE}", self.message)
        } else {
            f.write_str(&self.message)
        }
    }
}

#[derive(Clone, Debug)]
struct Project {
    root: PathBuf,
    name: String,
    version: String,
    delivery: Delivery,
    entry: PathBuf,
    index: PathBuf,
    public: PathBuf,
    manifest: PathBuf,
    grants: Option<PathBuf>,
    css: Option<PathBuf>,
    content: Option<PathBuf>,
    hosting: HostingProfile,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Delivery {
    Client,
    Static,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HostingProfile {
    Portable,
    HeadersRequired,
}

impl HostingProfile {
    fn name(self) -> &'static str {
        match self {
            Self::Portable => "portable",
            Self::HeadersRequired => "headers-required",
        }
    }

    fn response_headers_required(self) -> bool {
        self == Self::HeadersRequired
    }
}

#[derive(Clone, Debug)]
struct CheckedWeb {
    project: Project,
    grant: WebUiGrant,
    wasm: Vec<u8>,
    exports: BTreeSet<String>,
    host_imports: BTreeSet<String>,
    protocol_manifest: Value,
    source_origins: Value,
    tagged_origins: Value,
    compiler_sources: BTreeSet<PathBuf>,
    source_functions: Value,
    wasm_functions: Value,
    templates: Vec<witchy_lower::codegen::GlamourTemplateMetadata>,
    islands: Vec<witchy_lower::codegen::GlamourIslandMetadata>,
    development: Option<witchy_lower::codegen::GlamourDevelopmentMetadata>,
    packages: Vec<PackageRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebUiGrant {
    schema: &'static str,
    parameter: String,
    capability: &'static str,
    policy: String,
    digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct PackageRecord {
    source: String,
    name: String,
    version: String,
}

struct CompiledWeb {
    wasm: Vec<u8>,
    source_origins: Value,
    tagged_origins: Value,
    compiler_sources: BTreeSet<PathBuf>,
    source_functions: Value,
    wasm_functions: Value,
    templates: Vec<witchy_lower::codegen::GlamourTemplateMetadata>,
    islands: Vec<witchy_lower::codegen::GlamourIslandMetadata>,
    development: Option<witchy_lower::codegen::GlamourDevelopmentMetadata>,
    packages: Vec<PackageRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StaticPage {
    route: String,
    output: PathBuf,
    html: String,
    island_keys: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct StaticActionField {
    name: String,
    label: String,
    kind: String,
    required: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct StaticAction {
    id: String,
    method: String,
    action: String,
    fields: Vec<StaticActionField>,
    /// Compiler-owned identity of the ordered, public-only action input wire shape.
    #[serde(rename = "inputSchema", default, skip_deserializing)]
    input_schema: u32,
    /// Compiler-owned identity of the closed action completion message shape.
    #[serde(rename = "resultSchema", default, skip_deserializing)]
    result_schema: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct StaticStyle {
    id: String,
    scope: String,
    origin: String,
    text: String,
    classes: Vec<String>,
    routes: Vec<String>,
    critical_routes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct StaticPreload {
    route: String,
    href: String,
    kind: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct StaticAsset {
    href: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct StaticIsland {
    key: String,
    #[serde(skip)]
    source_identity: String,
    mode: String,
    activation: String,
    media: Option<String>,
    prefetch: String,
    prefetch_media: Option<String>,
    diagnostic_name: Option<String>,
    state: Option<String>,
    html: String,
    resume: StaticIslandResumeNode,
    template: StaticIslandTemplateNode,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum StaticIslandResumeNode {
    Text,
    Frame {
        renderer: String,
        max_grant_bytes: usize,
        max_event_bytes: usize,
        grant: String,
        fallback: String,
        id: String,
        decoder: String,
    },
    Keyed {
        key: String,
        child: Box<StaticIslandResumeNode>,
    },
    Branch {
        id: String,
        active: bool,
        child: Box<StaticIslandResumeNode>,
    },
    Child {
        id: String,
        mounted: bool,
        child: Box<StaticIslandResumeNode>,
    },
    Element {
        tag: String,
        attributes: Vec<StaticIslandResumeAttribute>,
        events: Vec<StaticIslandResumeEvent>,
        children: Vec<StaticIslandResumeNode>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct StaticIslandResumeAttribute {
    kind: String,
    name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct StaticIslandResumeEvent {
    id: String,
    event: String,
    kind: String,
    prevent_default: bool,
    stop_propagation: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum StaticIslandTemplateNode {
    Text {
        value: String,
    },
    Frame {
        renderer: String,
        max_grant_bytes: usize,
        max_event_bytes: usize,
        grant: String,
        fallback: String,
        id: String,
        decoder: String,
    },
    Keyed {
        key: String,
        child: Box<StaticIslandTemplateNode>,
    },
    Branch {
        id: String,
        child: Box<StaticIslandTemplateNode>,
    },
    Child {
        id: String,
        child: Box<StaticIslandTemplateNode>,
    },
    Element {
        tag: String,
        attributes: Vec<StaticIslandTemplateAttribute>,
        children: Vec<StaticIslandTemplateNode>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct StaticIslandTemplateAttribute {
    kind: String,
    name: String,
    value: String,
    enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct StaticIslandWorkDescriptor {
    id: u32,
    handler: String,
    result_schema: u32,
    completion_id: u32,
    owner_scope: u32,
    semantic: String,
    policy: Value,
    #[serde(skip)]
    completion_source: String,
    #[serde(skip)]
    completion_captures: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct StaticIslandCompiledPlan {
    key: String,
    mode: String,
    artifact: String,
    wire_id: u32,
    registry_id: u32,
    program_name: String,
    auth_type: String,
    model_type: String,
    #[serde(skip)]
    model_type_name: Option<String>,
    message_type: String,
    html: String,
    nodes: Vec<StaticIslandNodeRecord>,
    attributes: Vec<StaticIslandAttributeRecord>,
    regions: Vec<StaticIslandRegionRecord>,
    #[serde(skip)]
    text_nodes: Vec<StaticIslandTextNodeRecord>,
    events: Vec<StaticIslandEventRecord>,
    frames: Vec<StaticIslandFrameRecord>,
    effect_descriptors: Vec<StaticIslandWorkDescriptor>,
    subscription_descriptors: Vec<StaticIslandWorkDescriptor>,
    fresh: Option<StaticIslandFreshPlan>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct StaticIslandFrameRecord {
    node: u32,
    event_plan: u32,
    renderer: String,
    max_grant_bytes: usize,
    max_event_bytes: usize,
    grant: String,
    fallback: String,
    artifact: String,
    url: String,
    nonce: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct StaticIslandFreshPlan {
    route: String,
    bootstrap: String,
    template: u32,
    instance: u32,
    slots: Vec<StaticIslandTemplateSlotRecord>,
    root: Value,
    regions: Value,
    events: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct StaticIslandNodeRecord {
    id: u32,
    path: Vec<u32>,
    #[serde(skip)]
    dom_path: Vec<u32>,
    #[serde(skip)]
    live: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StaticIslandTextNodeRecord {
    id: u32,
    path: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct StaticIslandAttributeRecord {
    id: u32,
    node: u32,
    index: u32,
    kind: String,
    name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct StaticIslandRegionRecord {
    id: u32,
    parent: u32,
    kind: StaticIslandRegionKind,
    before: Vec<u32>,
    #[serde(skip)]
    live: bool,
    #[serde(skip)]
    path: Vec<u32>,
    keys: Vec<StaticIslandRegionKeyRecord>,
    dynamic: Option<StaticIslandRegionKeyRecord>,
    child: Option<StaticIslandRegionChildRecord>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum StaticIslandRegionKind {
    List,
    Branch,
    Child,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct StaticIslandRegionChildRecord {
    root: u32,
    nodes: Vec<u32>,
    template: u32,
    slots: Vec<StaticIslandTemplateSlotRecord>,
    #[serde(skip)]
    mounted: bool,
    #[serde(skip)]
    template_root: Value,
    #[serde(skip)]
    template_regions: Value,
    #[serde(skip)]
    template_events: Value,
    #[serde(skip)]
    path: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct StaticIslandRegionKeyRecord {
    id: u32,
    root: u32,
    nodes: Vec<u32>,
    template: u32,
    slots: Vec<StaticIslandTemplateSlotRecord>,
    #[serde(skip)]
    template_root: Value,
    #[serde(skip)]
    template_regions: Value,
    #[serde(skip)]
    template_events: Value,
    #[serde(skip)]
    source: String,
    #[serde(skip)]
    path: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct StaticIslandTemplateSlotRecord {
    id: u32,
    node: u32,
    kind: String,
    sink: u32,
    #[serde(skip)]
    source_kind: String,
    #[serde(skip)]
    path: Vec<u32>,
    #[serde(skip)]
    index: Option<u32>,
    #[serde(skip)]
    name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct StaticIslandEventRecord {
    id: String,
    name: String,
    node: u32,
    plan: u32,
    event_class: u32,
    prevent_default: bool,
    stop_propagation: bool,
    read_value: bool,
    read_checked: bool,
    read_key: bool,
    fallback: Option<StaticIslandProgressiveFallback>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum StaticIslandProgressiveFallback {
    Navigate { href: String },
    Submit { action: String, method: String },
}

#[derive(Clone, Debug)]
struct StaticIslandPublication {
    build_identity: String,
    manifest: Value,
    artifact_manifest: Value,
    route_manifests: BTreeMap<String, StaticIslandRouteManifest>,
    pages: BTreeMap<String, String>,
    artifacts: Vec<StaticIslandArtifact>,
    workers: Vec<StaticWorkerArtifact>,
    frames: Vec<StaticFrameArtifact>,
}

#[derive(Clone, Debug)]
struct StaticIslandRouteManifest {
    file: String,
    manifest: Value,
}

#[derive(Clone, Debug)]
struct StaticIslandArtifact {
    identity: String,
    file: String,
    wasm: Vec<u8>,
}

#[derive(Clone, Debug)]
struct StaticWorkerArtifact {
    identity: String,
    file: String,
    wasm: Vec<u8>,
}

#[derive(Clone, Debug)]
struct StaticFrameArtifact {
    identity: String,
    file: String,
    html: Vec<u8>,
}

#[derive(Clone, Debug)]
struct CheckedStaticSite {
    project: Project,
    pages: Vec<StaticPage>,
    actions: Vec<StaticAction>,
    styles: Vec<StaticStyle>,
    preloads: Vec<StaticPreload>,
    assets: Vec<StaticAsset>,
    islands: Vec<StaticIsland>,
    island_plans: Vec<StaticIslandCompiledPlan>,
    island_publication: Option<StaticIslandPublication>,
    content_inputs: Vec<ArtifactRecord>,
    packages: Vec<PackageRecord>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct ArtifactRecord {
    path: String,
    bytes: usize,
    sha256: String,
}

#[derive(Debug, Serialize)]
struct DoctorCheck {
    id: &'static str,
    status: &'static str,
    detail: String,
    remediation: Option<String>,
}

pub(crate) fn run(args: &[String]) -> Result<Option<String>, WebCommandError> {
    let Some(command) = args.first().map(String::as_str) else {
        return Ok(None);
    };
    let web_flag = args.iter().any(|argument| argument == "--web");
    match command {
        "new" if web_flag => new_command(&args[1..]).map(Some),
        "build" if web_flag => build_command(&args[1..]).map(Some),
        "test" if web_flag => test_command(&args[1..]).map(Some),
        "doctor" if web_flag => doctor_command(&args[1..]).map(Some),
        "dev" => dev::command(&args[1..]).map(Some),
        _ => Ok(None),
    }
}

fn new_command(args: &[String]) -> Result<String, WebCommandError> {
    if args.len() != 2 || args[0] != "--web" {
        return Err(WebCommandError::usage(
            "`witchy new --web` requires exactly one destination directory",
        ));
    }
    let destination = PathBuf::from(&args[1]);
    validate_destination(&destination)?;
    let name = project_name(&destination)?;
    let source_name = name.replace('-', "_");
    let files = starter_files(&name, &source_name);
    create_project_atomically(&destination, &files)?;
    Ok(format!(
        "created Glamour web project `{name}` at {}",
        destination.display()
    ))
}

fn build_command(args: &[String]) -> Result<String, WebCommandError> {
    let (root, output) = parse_build_args(args)?;
    let project = load_project(&root)?;
    match project.delivery {
        Delivery::Client => {
            let checked = check_loaded_project(project, false)?;
            let records = write_production(&checked, &output)?;
            Ok(format!(
                "built {} -> {} ({} artifacts)",
                checked.project.name,
                output.display(),
                records.len()
            ))
        }
        Delivery::Static => {
            let checked = check_static_project(project)?;
            let records = write_static_production(&checked, &output)?;
            Ok(format!(
                "built static site {} -> {} ({} routes, {} artifacts)",
                checked.project.name,
                output.display(),
                checked.pages.len(),
                records.len()
            ))
        }
    }
}

fn test_command(args: &[String]) -> Result<String, WebCommandError> {
    let root = parse_single_project_arg(args, "test")?;
    let project = load_project(&root)?;
    match project.delivery {
        Delivery::Client => {
            let checked = check_loaded_project(project, false)?;
            let second = compile_project(&checked.project)?;
            if checked.wasm != second {
                return Err(WebCommandError::failure(
                    "web target is not deterministic: two clean in-memory compilations differ",
                ));
            }
            Ok(format!(
                "PASS {}: typed source, empty host authority, RFC-0108 ABI, manifest, and deterministic Wasm",
                checked.project.name
            ))
        }
        Delivery::Static => {
            let checked = check_static_project(project)?;
            let second = evaluate_static_site(&checked.project)?;
            if checked.pages != second.0
                || checked.actions != second.1
                || checked.styles != second.2
                || checked.preloads != second.3
                || checked.assets != second.4
            {
                return Err(WebCommandError::failure(
                    "static site is not deterministic: two capability-free evaluations differ",
                ));
            }
            let runtime = if checked.islands.is_empty() {
                "zero browser runtime".to_string()
            } else {
                format!(
                    "{} compiler-generated interactive region{}",
                    checked.islands.len(),
                    if checked.islands.len() == 1 { "" } else { "s" },
                )
            };
            Ok(format!(
                "PASS {}: typed static Site, {} deterministic routes, and {runtime}",
                checked.project.name,
                checked.pages.len()
            ))
        }
    }
}

fn doctor_command(args: &[String]) -> Result<String, WebCommandError> {
    let mut format = "human";
    let mut root = None;
    let mut rest = args.iter();
    let Some(flag) = rest.next() else {
        return Err(WebCommandError::usage("`witchy doctor --web` requires `--web`"));
    };
    if flag != "--web" {
        return Err(WebCommandError::usage("expected `--web` after `doctor`"));
    }
    while let Some(argument) = rest.next() {
        match argument.as_str() {
            "--format" => {
                format = rest.next().map(String::as_str).ok_or_else(|| {
                    WebCommandError::usage("`--format` requires `human` or `json`")
                })?;
            }
            value if value.starts_with("--format=") => {
                format = &value["--format=".len()..];
            }
            value if value.starts_with('-') => {
                return Err(WebCommandError::usage(format!("unknown doctor option `{value}`")));
            }
            value if root.is_none() => root = Some(PathBuf::from(value)),
            value => {
                return Err(WebCommandError::usage(format!(
                    "unexpected doctor argument `{value}`"
                )));
            }
        }
    }
    if format != "human" && format != "json" {
        return Err(WebCommandError::usage(format!("unknown doctor format `{format}`")));
    }
    let root = root.unwrap_or_else(|| PathBuf::from("."));
    let mut checks = Vec::new();
    let outcome = match load_project(&root) {
        Ok(project) => {
            checks.push(pass("project", format!("resolved `{}`", project.name)));
            match project.delivery {
                Delivery::Client => doctor_client_project(project, &mut checks),
                Delivery::Static => doctor_static_project(project, &mut checks),
            }
        }
        Err(error) => {
            checks.push(fail(
                "project",
                error.to_string(),
                "run from a web project or pass its directory",
            ));
            Err(())
        }
    };
    let rendered = render_doctor(format, &checks).map_err(WebCommandError::failure)?;
    if outcome.is_err() {
        return Err(WebCommandError::failure(rendered));
    }
    Ok(rendered)
}

fn doctor_client_project(
    project: Project,
    checks: &mut Vec<DoctorCheck>,
) -> Result<(), ()> {
    match check_loaded_project(project, false) {
        Ok(checked) => {
            checks.push(pass("target", "compiled capability-safe browser Wasm"));
            checks.push(pass(
                "protocol",
                format!("RFC-0108 exports: {}", REQUIRED_EXPORTS.join(", ")),
            ));
            checks.push(pass(
                "authority",
                format!(
                    "host imports: {}",
                    if checked.host_imports.is_empty() {
                        "(none)".to_string()
                    } else {
                        checked
                            .host_imports
                            .iter()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", ")
                    }
                ),
            ));
            match compile_project(&checked.project) {
                Ok(second) if second == checked.wasm => {
                    checks.push(pass(
                        "determinism",
                        "two in-memory builds are byte-identical",
                    ));
                    Ok(())
                }
                Ok(_) => {
                    checks.push(fail(
                        "determinism",
                        "two in-memory builds differ",
                        "remove nondeterministic compile-time inputs",
                    ));
                    Err(())
                }
                Err(error) => {
                    checks.push(fail(
                        "determinism",
                        error.to_string(),
                        "fix the second production compilation",
                    ));
                    Err(())
                }
            }
        }
        Err(error) => {
            checks.push(fail(
                "target",
                error.to_string(),
                "run `witchy test --web` after correcting the reported source or manifest",
            ));
            Err(())
        }
    }
}

fn doctor_static_project(
    project: Project,
    checks: &mut Vec<DoctorCheck>,
) -> Result<(), ()> {
    match check_static_project(project) {
        Ok(checked) => {
            checks.push(pass(
                "target",
                format!(
                    "evaluated typed static Site with {} routes",
                    checked.pages.len()
                ),
            ));
            checks.push(pass(
                "authority",
                "static declaration evaluated with no minted capabilities",
            ));
            checks.push(pass(
                "runtime",
                if checked.islands.is_empty() {
                    "zero JavaScript and zero Wasm required"
                } else {
                    "runtime is scoped to declared interactive regions"
                },
            ));
            match evaluate_static_site(&checked.project) {
                Ok((pages, actions, styles, preloads, assets, islands))
                    if pages == checked.pages
                        && actions == checked.actions
                        && styles == checked.styles
                        && preloads == checked.preloads
                        && assets == checked.assets
                        && islands == checked.islands =>
                {
                    checks.push(pass(
                        "determinism",
                        "two capability-free evaluations are identical",
                    ));
                    Ok(())
                }
                Ok(_) => {
                    checks.push(fail(
                        "determinism",
                        "two static evaluations differ",
                        "remove nondeterministic inputs from `web()`",
                    ));
                    Err(())
                }
                Err(error) => {
                    checks.push(fail(
                        "determinism",
                        error.to_string(),
                        "fix the second static evaluation",
                    ));
                    Err(())
                }
            }
        }
        Err(error) => {
            checks.push(fail(
                "target",
                error.to_string(),
                "make `web()` return an authenticated `glamour.Site`",
            ));
            Err(())
        }
    }
}

fn render_doctor(format: &str, checks: &[DoctorCheck]) -> Result<String, String> {
    if format == "json" {
        serde_json::to_string_pretty(&json!({
            "schema": "witchy.web.doctor.v1",
            "ok": checks.iter().all(|check| check.status == "pass"),
            "checks": checks,
        }))
        .map_err(|error| error.to_string())
    } else {
        Ok(checks
            .iter()
            .map(|check| {
                let mut line = format!("{} {}: {}", check.status.to_uppercase(), check.id, check.detail);
                if let Some(remediation) = &check.remediation {
                    line.push_str(&format!("\n  fix: {remediation}"));
                }
                line
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

fn pass(id: &'static str, detail: impl Into<String>) -> DoctorCheck {
    DoctorCheck { id, status: "pass", detail: detail.into(), remediation: None }
}

fn fail(
    id: &'static str,
    detail: impl Into<String>,
    remediation: impl Into<String>,
) -> DoctorCheck {
    DoctorCheck {
        id,
        status: "fail",
        detail: detail.into(),
        remediation: Some(remediation.into()),
    }
}

fn parse_build_args(args: &[String]) -> Result<(PathBuf, PathBuf), WebCommandError> {
    let mut root = None;
    let mut output = None;
    let mut rest = args.iter();
    let Some(flag) = rest.next() else {
        return Err(WebCommandError::usage("`witchy build --web` requires `--web`"));
    };
    if flag != "--web" {
        return Err(WebCommandError::usage("expected `--web` after `build`"));
    }
    while let Some(argument) = rest.next() {
        match argument.as_str() {
            "--out" => {
                let value = rest.next().ok_or_else(|| {
                    WebCommandError::usage("`--out` requires a directory")
                })?;
                if output.replace(PathBuf::from(value)).is_some() {
                    return Err(WebCommandError::usage("`--out` was supplied more than once"));
                }
            }
            value if value.starts_with("--out=") => {
                if output.replace(PathBuf::from(&value["--out=".len()..])).is_some() {
                    return Err(WebCommandError::usage("`--out` was supplied more than once"));
                }
            }
            value if value.starts_with('-') => {
                return Err(WebCommandError::usage(format!("unknown build option `{value}`")));
            }
            value if root.is_none() => root = Some(PathBuf::from(value)),
            value => {
                return Err(WebCommandError::usage(format!(
                    "unexpected build argument `{value}`"
                )));
            }
        }
    }
    let root = root.unwrap_or_else(|| PathBuf::from("."));
    let output = output.unwrap_or_else(|| root.join("dist"));
    Ok((root, output))
}

fn parse_single_project_arg(
    args: &[String],
    command: &str,
) -> Result<PathBuf, WebCommandError> {
    if args.first().map(String::as_str) != Some("--web") {
        return Err(WebCommandError::usage(format!(
            "`witchy {command} --web` requires `--web` immediately after the command"
        )));
    }
    match &args[1..] {
        [] => Ok(PathBuf::from(".")),
        [root] if !root.starts_with('-') => Ok(PathBuf::from(root)),
        [unknown, ..] => Err(WebCommandError::usage(format!(
            "unexpected {command} option or argument `{unknown}`"
        ))),
    }
}

#[cfg(test)]
fn check_project(root: &Path) -> Result<CheckedWeb, WebCommandError> {
    let project = load_project(root)?;
    check_loaded_project(project, false)
}

fn check_project_development(root: &Path) -> Result<CheckedWeb, WebCommandError> {
    let project = load_project(root)?;
    if project.delivery != Delivery::Client {
        return Err(WebCommandError::failure(
            "`witchy dev` static-site support is not implemented yet; use `witchy build --web`",
        ));
    }
    check_loaded_project(project, true)
}

fn check_loaded_project(
    project: Project,
    development: bool,
) -> Result<CheckedWeb, WebCommandError> {
    let grant = load_web_ui_grant(&project, true)?.expect("required web grant was checked");
    let footprint = super::capabilities::analyze_file(path_text(&project.entry)?)
        .map_err(WebCommandError::failure)?;
    if !footprint.total.is_empty() || !footprint.build.is_empty() {
        return Err(WebCommandError::failure(format!(
            "web entry demands host authority; runtime={} build={}",
            witchy_caps::capabilities::show_caps(&footprint.total),
            witchy_caps::capabilities::show_caps(&footprint.build),
        )));
    }
    let compiled = compile_project_with_origins(&project, development)?;
    let CompiledWeb {
        wasm,
        source_origins,
        tagged_origins,
        compiler_sources,
        source_functions,
        wasm_functions,
        templates,
        islands,
        development,
        packages,
    } = compiled;
    let (exports, host_imports) = inspect_wasm(&wasm)?;
    for required in REQUIRED_EXPORTS {
        if !exports.contains(*required) {
            return Err(WebCommandError::failure(format!(
                "web Wasm is missing required RFC-0108 export `{required}`"
            )));
        }
    }
    for development_export in FORBIDDEN_PRODUCTION_EXPORTS {
        let present = exports.contains(*development_export);
        let snapshot_export = matches!(
            *development_export,
            "__glamour_dev_snapshot"
                | "__glamour_dev_snapshot_length"
                | "__glamour_dev_restore"
        );
        let expected = development
            .as_ref()
            .is_some_and(|metadata| !snapshot_export || metadata.supports_snapshot());
        if !expected && present {
            return Err(WebCommandError::failure(format!(
                "Wasm exposes unrequested development-only export `{development_export}`"
            )));
        }
        if expected && !present {
            return Err(WebCommandError::failure(format!(
                "development Wasm is missing compiler-owned export `{development_export}`"
            )));
        }
    }
    let protocol_manifest = read_protocol_manifest(&project.manifest)?;
    Ok(CheckedWeb {
        project,
        grant,
        wasm,
        exports,
        host_imports,
        protocol_manifest,
        source_origins,
        tagged_origins,
        compiler_sources,
        source_functions,
        wasm_functions,
        templates,
        islands,
        development,
        packages,
    })
}

fn compile_project(project: &Project) -> Result<Vec<u8>, WebCommandError> {
    compile_project_with_origins(project, false).map(|compiled| compiled.wasm)
}

fn compile_project_with_origins(
    project: &Project,
    development: bool,
) -> Result<CompiledWeb, WebCommandError> {
    let (checked, _) = crate::link_file_checked(path_text(&project.entry)?)
        .map_err(WebCommandError::failure)?;
    let mut source_origins = serde_json::to_value(checked.origins().nodes())
        .map_err(|error| WebCommandError::failure(error.to_string()))?;
    let mut tagged_origins = authenticated_tagged_origins(&checked)?;
    normalize_source_modules(&mut source_origins, &project.root);
    normalize_source_modules(&mut tagged_origins, &project.root);
    let packages = package_records(&checked)?;
    let templates = witchy_lower::codegen::checked_glamour_templates(&checked)
        .map_err(|error| WebCommandError::failure(error.to_string()))?;
    let islands = witchy_lower::codegen::checked_glamour_islands(&checked)
        .map_err(|error| WebCommandError::failure(error.to_string()))?;
    let (wasm, development_metadata, source_instructions, source_expressions) = if development {
        let compiled = super::compile::compile_checked_to_development_wasm_cached(&checked)
            .map_err(WebCommandError::failure)?;
        (
            compiled.wasm,
            compiled.glamour,
            compiled.source_instructions,
            compiled.source_expressions,
        )
    } else {
        (
            super::compile::compile_checked_to_wasm_cached(&checked)
                .map_err(WebCommandError::failure)?,
            None,
            Vec::new(),
            Vec::new(),
        )
    };
    let source_graph = if development {
        development_source_graph(project)?
    } else {
        DevelopmentSourceGraph::default()
    };
    let wasm_functions = if development {
        development_wasm_functions(
            &wasm,
            &source_graph.functions,
            &source_instructions,
            &source_expressions,
        )?
    } else {
        Value::Array(Vec::new())
    };
    Ok(CompiledWeb {
        wasm,
        source_origins,
        tagged_origins,
        compiler_sources: source_graph.paths,
        source_functions: development_source_function_registry(&source_graph.functions),
        wasm_functions,
        templates,
        islands,
        development: development_metadata,
        packages,
    })
}

fn package_records(
    checked: &witchy_interp::pipeline::CheckedModule,
) -> Result<Vec<PackageRecord>, WebCommandError> {
    Ok(checked
        .runtime_declaration_catalog()
        .map_err(|error| WebCommandError::failure(error.to_string()))?
        .identities()
        .map(|identity| {
            let package = identity.package();
            let source = match package.source() {
                witchy_types::runtime_type::PackageSource::Toolchain => "toolchain".to_string(),
                witchy_types::runtime_type::PackageSource::Workspace => "workspace".to_string(),
                witchy_types::runtime_type::PackageSource::Registry(registry) => {
                    format!("registry:{registry}")
                }
            };
            PackageRecord {
                source,
                name: package.name().to_string(),
                version: package.version().to_string(),
            }
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}

fn authenticated_tagged_origins(
    checked: &witchy_interp::pipeline::CheckedModule,
) -> Result<Value, WebCommandError> {
    use witchy_types::runtime_type::{DeclarationKind, PackageSource};

    let catalog = checked
        .runtime_declaration_catalog()
        .map_err(|error| WebCommandError::failure(error.to_string()))?;
    let mut records = Vec::with_capacity(checked.origins().tagged_literals().len());
    for origin in checked.origins().tagged_literals() {
        let declaration = catalog
            .resolve(&origin.tag, DeclarationKind::Function)
            .ok_or_else(|| {
                WebCommandError::failure(format!(
                    "tagged literal `{}` has no loader-authenticated declaration identity",
                    origin.tag
                ))
            })?;
        let source = match declaration.package().source() {
            PackageSource::Toolchain => "toolchain".to_string(),
            PackageSource::Workspace => "workspace".to_string(),
            PackageSource::Registry(registry) => format!("registry:{registry}"),
        };
        let mut record = serde_json::to_value(origin)
            .map_err(|error| WebCommandError::failure(error.to_string()))?;
        record
            .as_object_mut()
            .expect("tagged literal origin serializes as an object")
            .insert(
                "declarationIdentity".into(),
                json!({
                    "source": source,
                    "package": declaration.package().name(),
                    "version": declaration.package().version(),
                    "module": declaration.module(),
                    "name": declaration.name(),
                }),
            );
        records.push(record);
    }
    Ok(Value::Array(records))
}

fn normalize_source_modules(value: &mut Value, root: &Path) {
    match value {
        Value::Array(values) => {
            for value in values {
                normalize_source_modules(value, root);
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                if key == "module" {
                    if let Value::String(module) = value {
                        *module = source_module_name(root, module);
                    }
                } else {
                    normalize_source_modules(value, root);
                }
            }
        }
        _ => {}
    }
}

fn source_module_name(root: &Path, module: &str) -> String {
    let path = Path::new(module);
    let relative = if path.is_absolute() {
        match path.strip_prefix(root) {
            Ok(relative) => relative,
            Err(_) => {
                return path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| format!("<external>/{name}"))
                    .unwrap_or_else(|| "<external>".to_string());
            }
        }
    } else {
        path
    };
    if relative
        .components()
        .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
    {
        let normalized = relative
            .components()
            .filter_map(|component| match component {
                Component::Normal(value) => value.to_str(),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("/");
        if normalized.is_empty() { "<generated>".to_string() } else { normalized }
    } else {
        relative
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| format!("<external>/{name}"))
            .unwrap_or_else(|| "<external>".to_string())
    }
}

#[derive(Clone, Debug)]
struct DevelopmentSourceFunction {
    module: String,
    name: String,
    compiled_names: Vec<String>,
    compiled_suffix: Option<String>,
    start_line: u32,
    end_line: u32,
    expression_spans: Vec<witchy_syntax::parser::ExpressionSyntaxSpan>,
}

#[derive(Default)]
struct DevelopmentSourceGraph {
    paths: BTreeSet<PathBuf>,
    functions: Vec<DevelopmentSourceFunction>,
}

fn development_source_graph(
    project: &Project,
) -> Result<DevelopmentSourceGraph, WebCommandError> {
    use std::collections::VecDeque;

    let mut queue = VecDeque::from([project.entry.clone()]);
    let mut graph = DevelopmentSourceGraph::default();
    while let Some(path) = queue.pop_front() {
        let path = std::fs::canonicalize(&path).map_err(|error| {
            WebCommandError::failure(format!(
                "cannot resolve development source `{}`: {error}",
                path.display()
            ))
        })?;
        if !graph.paths.insert(path.clone()) {
            continue;
        }
        let source = std::fs::read_to_string(&path).map_err(|error| {
            WebCommandError::failure(format!(
                "cannot read development source `{}`: {error}",
                path.display()
            ))
        })?;
        let (parsed, expression_spans) = witchy_syntax::parser::parse_module_with_expression_spans(
            &source,
        )
            .map_err(|error| WebCommandError::failure(format!("{}: {error}", path.display())))?;
        let module_name = path
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| WebCommandError::failure("development source has no module name"))?
            .to_string();
        let module = source_module_name(&project.root, &path.to_string_lossy());
        let source_lines = u32::try_from(source.lines().count()).unwrap_or(u32::MAX);
        for (index, item) in parsed.items.iter().enumerate() {
            let item_end = parsed
                .item_lines
                .get(index + 1)
                .copied()
                .filter(|line| *line != u32::MAX)
                .unwrap_or(source_lines.saturating_add(1));
            match item {
                witchy_syntax::ast::Item::Function(function) => {
                    let start_line = function.line;
                    if start_line == 0 || start_line == u32::MAX {
                        continue;
                    }
                    graph.functions.push(DevelopmentSourceFunction {
                        module: module.clone(),
                        name: function.name.clone(),
                        compiled_names: vec![
                            function.name.clone(),
                            format!("{module_name}.{}", function.name),
                        ],
                        compiled_suffix: None,
                        start_line,
                        end_line: item_end.saturating_sub(1).max(start_line),
                        expression_spans: expression_spans
                            .iter()
                            .filter(|span| {
                                span.source.start.line >= start_line
                                    && span.source.end.line
                                        <= item_end.saturating_sub(1).max(start_line)
                            })
                            .cloned()
                            .collect(),
                    });
                }
                witchy_syntax::ast::Item::Impl(implementation) => {
                    for (method_index, method) in implementation.methods.iter().enumerate() {
                        let start_line = method.line;
                        if start_line == 0 || start_line == u32::MAX {
                            continue;
                        }
                        let end_line = implementation
                            .methods
                            .get(method_index + 1)
                            .map(|next| next.line)
                            .filter(|line| *line != 0 && *line != u32::MAX)
                            .unwrap_or(item_end)
                            .saturating_sub(1)
                            .max(start_line);
                        let suffix = format!("__{}__{}", implementation.type_name, method.name);
                        let compiled_name = if implementation.trait_args.is_empty() {
                            Some(match &implementation.trait_name {
                                Some(trait_name) => format!(
                                    "{trait_name}__{}__{}",
                                    implementation.type_name, method.name
                                ),
                                None => format!("{}__{}", implementation.type_name, method.name),
                            })
                        } else {
                            None
                        };
                        let compiled_names = compiled_name
                            .into_iter()
                            .flat_map(|name| [name.clone(), format!("{module_name}.{name}")])
                            .collect();
                        graph.functions.push(DevelopmentSourceFunction {
                            module: module.clone(),
                            name: method.name.clone(),
                            compiled_names,
                            compiled_suffix: Some(suffix),
                            start_line,
                            end_line,
                            expression_spans: expression_spans
                                .iter()
                                .filter(|span| {
                                    span.source.start.line >= start_line
                                        && span.source.end.line <= end_line
                                })
                                .cloned()
                                .collect(),
                        });
                    }
                }
                _ => {}
            }
        }
        let parent = path.parent().unwrap_or(&project.root);
        for import in parsed.imports {
            let sibling = parent.join(format!("{import}.witchy"));
            if sibling.is_file() {
                queue.push_back(sibling);
            }
        }
    }
    graph.functions.sort_by(|left, right| {
        (&left.module, left.start_line, &left.name).cmp(&(
            &right.module,
            right.start_line,
            &right.name,
        ))
    });
    Ok(graph)
}

fn development_source_function_registry(functions: &[DevelopmentSourceFunction]) -> Value {
    Value::Array(
        functions
            .iter()
            .map(|function| {
                json!({
                    "module": &function.module,
                    "name": &function.name,
                    "source": {
                        "module": &function.module,
                        "start": {"line": function.start_line, "column": 1},
                        "end": {"line": function.end_line, "column": 1},
                    },
                    "expressionSpans": function.expression_spans.iter().map(|span| json!({
                        "start": {"line": span.source.start.line, "column": span.source.start.column},
                        "end": {"line": span.source.end.line, "column": span.source.end.column},
                        "statementLine": span.statement_line,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

fn development_wasm_functions(
    wasm: &[u8],
    sources: &[DevelopmentSourceFunction],
    source_instructions: &[witchy_wir::wir_encode::SourceInstructionRange],
    source_expressions: &[witchy_wir::wir_encode::SourceExpressionInstructionRange],
) -> Result<Value, WebCommandError> {
    use wasmparser::{KnownCustom, Name, Payload, TypeRef};

    let mut imported_functions = 0_u32;
    let mut code_index = 0_u32;
    let mut names = BTreeMap::new();
    let mut bodies = Vec::new();
    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        match payload.map_err(|error| WebCommandError::failure(error.to_string()))? {
            Payload::ImportSection(section) => {
                for import in section.into_imports() {
                    let import = import
                        .map_err(|error| WebCommandError::failure(error.to_string()))?;
                    if matches!(import.ty, TypeRef::Func(_)) {
                        imported_functions = imported_functions.saturating_add(1);
                    }
                }
            }
            Payload::CodeSectionEntry(body) => {
                let range = body.range();
                let instruction_offsets = body
                    .get_operators_reader()
                    .map_err(|error| WebCommandError::failure(error.to_string()))?
                    .into_iter_with_offsets()
                    .map(|operator| {
                        operator
                            .map(|(_, offset)| offset)
                            .map_err(|error| WebCommandError::failure(error.to_string()))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                bodies.push((
                    imported_functions + code_index,
                    range.start,
                    range.end,
                    instruction_offsets,
                ));
                code_index = code_index.saturating_add(1);
            }
            Payload::CustomSection(section) => {
                if let KnownCustom::Name(section) = section.as_known() {
                    for subsection in section {
                        let subsection = subsection
                            .map_err(|error| WebCommandError::failure(error.to_string()))?;
                        if let Name::Function(map) = subsection {
                            for naming in map {
                                let naming = naming.map_err(|error| {
                                    WebCommandError::failure(error.to_string())
                                })?;
                                names.insert(naming.index, naming.name.to_string());
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let mut source_ranges = BTreeMap::<u32, Vec<_>>::new();
    for range in source_instructions {
        source_ranges.entry(range.function_index).or_default().push(range);
    }
    for ranges in source_ranges.values_mut() {
        ranges.sort_by_key(|range| {
            (range.instruction_start, range.instruction_end, range.line)
        });
    }
    let mut expression_ranges = BTreeMap::<u32, Vec<_>>::new();
    for range in source_expressions {
        expression_ranges
            .entry(range.function_index)
            .or_default()
            .push(range);
    }
    for ranges in expression_ranges.values_mut() {
        ranges.sort_by_key(|range| {
            (range.instruction_start, range.instruction_end, range.line)
        });
    }
    let mut records = Vec::new();
    for (index, start, end, instruction_offsets) in bodies {
        let Some(name) = names.get(&index) else {
            continue;
        };
        let tail = name.rsplit('.').next().unwrap_or(name);
        let candidates = sources
            .iter()
            .filter(|source| {
                source
                    .compiled_names
                    .iter()
                    .any(|candidate| candidate == name || candidate == tail)
                    || source
                        .compiled_suffix
                        .as_ref()
                        .is_some_and(|suffix| tail.ends_with(suffix))
            })
            .collect::<Vec<_>>();
        let exact = candidates
            .iter()
            .copied()
            .filter(|source| {
                source
                    .compiled_names
                    .iter()
                    .any(|candidate| candidate == name || candidate == tail)
            })
            .collect::<Vec<_>>();
        let source = (exact.len() == 1)
            .then(|| exact[0])
            .or_else(|| (exact.is_empty() && candidates.len() == 1).then(|| candidates[0]));
        let statements = source
            .into_iter()
            .flat_map(|source| {
                source_ranges
                    .get(&index)
                    .into_iter()
                    .flatten()
                    .map(move |range| (source, range))
            })
            .filter_map(|(source, range)| {
                if range.line < source.start_line || range.line > source.end_line {
                    return None;
                }
                let instruction_start = usize::try_from(range.instruction_start).ok()?;
                let instruction_end = usize::try_from(range.instruction_end).ok()?;
                let byte_start = *instruction_offsets.get(instruction_start)?;
                let byte_end = *instruction_offsets.get(instruction_end)?;
                (byte_start < byte_end).then(|| {
                    json!({
                        "source": {
                            "module": &source.module,
                            "start": {"line": range.line, "column": 1},
                            "end": {"line": range.line, "column": 1},
                        },
                        "instructionStart": range.instruction_start,
                        "instructionEnd": range.instruction_end,
                        "byteStart": byte_start,
                        "byteEnd": byte_end,
                    })
                })
            })
            .collect::<Vec<_>>();
        let expressions = source
            .map(|source| {
                source
                    .expression_spans
                    .iter()
                    .map(|span| {
                        let mut record = json!({
                            "module": &source.module,
                            "start": {
                                "line": span.source.start.line,
                                "column": span.source.start.column,
                            },
                            "end": {
                                "line": span.source.end.line,
                                "column": span.source.end.column,
                            },
                            "statementLine": span.statement_line,
                        });
                        let matching = source_ranges
                            .get(&index)
                            .into_iter()
                            .flatten()
                            .filter(|range| range.line == span.statement_line)
                            .collect::<Vec<_>>();
                        if let [range] = matching.as_slice()
                            && let (Ok(instruction_start), Ok(instruction_end)) = (
                                usize::try_from(range.instruction_start),
                                usize::try_from(range.instruction_end),
                            )
                            && let (Some(byte_start), Some(byte_end)) = (
                                instruction_offsets.get(instruction_start),
                                instruction_offsets.get(instruction_end),
                            )
                        {
                            record["statementMapping"] = json!({
                                "instructionStart": range.instruction_start,
                                "instructionEnd": range.instruction_end,
                                "byteStart": byte_start,
                                "byteEnd": byte_end,
                            });
                        }
                        let expression_matching = expression_ranges
                            .get(&index)
                            .into_iter()
                            .flatten()
                            .filter(|range| range.line == span.statement_line)
                            .collect::<Vec<_>>();
                        if let [range] = expression_matching.as_slice()
                            && let (Ok(instruction_start), Ok(instruction_end)) = (
                                usize::try_from(range.instruction_start),
                                usize::try_from(range.instruction_end),
                            )
                            && let (Some(byte_start), Some(byte_end)) = (
                                instruction_offsets.get(instruction_start),
                                instruction_offsets.get(instruction_end),
                            )
                        {
                            record["expressionMapping"] = json!({
                                "instructionStart": range.instruction_start,
                                "instructionEnd": range.instruction_end,
                                "byteStart": byte_start,
                                "byteEnd": byte_end,
                            });
                        }
                        record
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        records.push(json!({
            "functionIndex": index,
            "name": name,
            "bodyOffset": start,
            "bodyEnd": end,
            "instructionOffsets": instruction_offsets,
            "statementMappings": statements,
            "expressionSpans": expressions,
            "source": source.map(|source| json!({
                "module": &source.module,
                "start": {"line": source.start_line, "column": 1},
                "end": {"line": source.end_line, "column": 1},
            })),
        }));
    }
    Ok(Value::Array(records))
}

fn inspect_wasm(
    wasm: &[u8],
) -> Result<(BTreeSet<String>, BTreeSet<String>), WebCommandError> {
    let mut exports = BTreeSet::new();
    let mut imports = BTreeSet::new();
    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        match payload.map_err(|error| WebCommandError::failure(error.to_string()))? {
            wasmparser::Payload::ExportSection(section) => {
                for export in section {
                    let export = export
                        .map_err(|error| WebCommandError::failure(error.to_string()))?;
                    exports.insert(export.name.to_string());
                }
            }
            wasmparser::Payload::ImportSection(section) => {
                for import in section.into_imports() {
                    let import = import
                        .map_err(|error| WebCommandError::failure(error.to_string()))?;
                    imports.insert(format!("{}.{}", import.module, import.name));
                }
            }
            _ => {}
        }
    }
    Ok((exports, imports))
}

fn load_project(root: &Path) -> Result<Project, WebCommandError> {
    let requested_root = absolute_lexical(root)?;
    let root = std::fs::canonicalize(&requested_root).map_err(|error| {
        WebCommandError::failure(format!(
            "cannot resolve web project `{}`: {error}",
            requested_root.display()
        ))
    })?;
    if !root.is_dir() {
        return Err(WebCommandError::failure(format!(
            "web project root `{}` is not a directory",
            root.display()
        )));
    }
    let manifest_path = root.join("witchy.toml");
    let source = std::fs::read_to_string(&manifest_path).map_err(|error| {
        WebCommandError::failure(format!("cannot read `{}`: {error}", manifest_path.display()))
    })?;
    let document: toml::Value = toml::from_str(&source).map_err(|error| {
        WebCommandError::failure(format!("{}: {error}", manifest_path.display()))
    })?;
    let rune = document.get("rune").and_then(toml::Value::as_table).ok_or_else(|| {
        WebCommandError::failure("witchy.toml is missing `[rune]`")
    })?;
    let name = rune.get("name").and_then(toml::Value::as_str).ok_or_else(|| {
        WebCommandError::failure("witchy.toml is missing `rune.name`")
    })?.to_string();
    let version = rune.get("version").and_then(toml::Value::as_str).unwrap_or("0.0.0").to_string();
    let web = document.get("web").and_then(toml::Value::as_table).ok_or_else(|| {
        WebCommandError::failure("witchy.toml is missing RFC-0109's `[web]` table")
    })?;
    let delivery = match web
        .get("delivery")
        .and_then(toml::Value::as_str)
        .unwrap_or("static")
    {
        "client" => Delivery::Client,
        "static" => Delivery::Static,
        value => {
            return Err(WebCommandError::failure(format!(
                "web.delivery must be `client` or `static`, not `{value}`"
            )));
        }
    };
    let hosting = match web
        .get("hosting")
        .and_then(toml::Value::as_str)
        .unwrap_or("portable")
    {
        "portable" => HostingProfile::Portable,
        "headers-required" => HostingProfile::HeadersRequired,
        value => {
            return Err(WebCommandError::failure(format!(
                "web.hosting must be `portable` or `headers-required`, not `{value}`"
            )));
        }
    };
    let relative = |key: &str, fallback: &str| -> Result<PathBuf, WebCommandError> {
        let value = web.get(key).and_then(toml::Value::as_str).unwrap_or(fallback);
        safe_project_path(&root, value, key)
    };
    let entry = confined_existing(&root, &relative("entry", "src/main.witchy")?, "entry")?;
    let index_path = relative("index", "web/index.html")?;
    let public_path = relative("public", "web/public")?;
    let manifest_path = relative("manifest", "web/glamour-manifest.json")?;
    let grants = web
        .get("grants")
        .and_then(toml::Value::as_str)
        .map(|value| {
            let path = safe_project_path(&root, value, "grants")?;
            confined_existing(&root, &path, "grants")
        })
        .transpose()?;
    let index = if delivery == Delivery::Client || index_path.exists() {
        confined_existing(&root, &index_path, "index")?
    } else {
        index_path
    };
    let public = if delivery == Delivery::Client || public_path.exists() {
        confined_existing(&root, &public_path, "public")?
    } else {
        public_path
    };
    let manifest = if delivery == Delivery::Client || manifest_path.exists() {
        confined_existing(&root, &manifest_path, "manifest")?
    } else {
        manifest_path
    };
    let css = web
        .get("css")
        .and_then(toml::Value::as_str)
        .map(|value| {
            let path = safe_project_path(&root, value, "css")?;
            confined_existing(&root, &path, "css")
        })
        .transpose()?;
    let content = web
        .get("content")
        .and_then(toml::Value::as_str)
        .map(|value| declared_content_root(&root, value))
        .transpose()?;
    if web.contains_key("ports") {
        return Err(WebCommandError::failure(
            "web.ports is not available until the typed host-custody registry is implemented",
        ));
    }
    if delivery == Delivery::Client && content.is_some() {
        return Err(WebCommandError::failure(
            "web.content is available only when web.delivery = \"static\"",
        ));
    }
    let mut required_files = vec![("entry", &entry)];
    if delivery == Delivery::Client {
        required_files.extend([("index", &index), ("manifest", &manifest)]);
    }
    for (label, path) in required_files {
        if !path.is_file() {
            return Err(WebCommandError::failure(format!(
                "web {label} `{}` is missing",
                path.display()
            )));
        }
    }
    if delivery == Delivery::Client && !public.is_dir() {
        return Err(WebCommandError::failure(format!(
            "web public directory `{}` is missing",
            public.display()
        )));
    }
    if css.as_ref().is_some_and(|path| !path.is_file()) {
        return Err(WebCommandError::failure("configured web CSS file is missing"));
    }
    Ok(Project {
        root,
        name,
        version,
        delivery,
        entry,
        index,
        public,
        manifest,
        grants,
        css,
        content,
        hosting,
    })
}

fn load_web_ui_grant(
    project: &Project,
    required: bool,
) -> Result<Option<WebUiGrant>, WebCommandError> {
    let Some(path) = &project.grants else {
        if required {
            return Err(WebCommandError::failure(
                "runnable web output requires `web.grants` naming one authenticated UiRoot grant",
            ));
        }
        return Ok(None);
    };
    let source = std::fs::read_to_string(path).map_err(|error| {
        WebCommandError::failure(format!("cannot read web grants `{}`: {error}", path.display()))
    })?;
    let document = witchy_caps::grants::GrantDoc::parse(&source)
        .map_err(WebCommandError::failure)?;
    if !document.files.is_empty()
        || !document.dirs.is_empty()
        || !document.net.is_empty()
        || !document.fetch.is_empty()
        || !document.env.is_empty()
        || !document.exec.is_empty()
        || !document.secrets.is_empty()
    {
        return Err(WebCommandError::failure(
            "a web mount grant may contain only one public `[user_caps]` UiRoot entry",
        ));
    }
    let mut entries = document.user_caps.into_iter();
    let Some((parameter, grant)) = entries.next() else {
        return Err(WebCommandError::failure(
            "a web mount grant must contain exactly one `[user_caps]` UiRoot entry",
        ));
    };
    if entries.next().is_some() || grant.cap_type != "UiRoot" {
        return Err(WebCommandError::failure(
            "a web mount grant must contain exactly one `[user_caps]` entry of type `UiRoot`",
        ));
    }
    if grant.fields.len() != 1 {
        return Err(WebCommandError::failure(
            "a web UiRoot grant must contain exactly the public string field `policy`",
        ));
    }
    let policy = grant
        .fields
        .get("policy")
        .and_then(toml::Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 256)
        .ok_or_else(|| {
            WebCommandError::failure(
                "a web UiRoot grant requires a nonempty `policy` string of at most 256 bytes",
            )
        })?
        .to_string();
    let canonical = serde_json::to_vec(&json!({
        "schema": "witchy.web.ui-root-grant.v1",
        "parameter": parameter,
        "capability": "UiRoot",
        "policy": policy,
    }))
    .map_err(|error| WebCommandError::failure(error.to_string()))?;
    Ok(Some(WebUiGrant {
        schema: "witchy.web.ui-root-grant.v1",
        parameter,
        capability: "UiRoot",
        policy,
        digest: sha256(&canonical),
    }))
}

fn read_protocol_manifest(path: &Path) -> Result<Value, WebCommandError> {
    let source = std::fs::read_to_string(path).map_err(|error| {
        WebCommandError::failure(format!("cannot read `{}`: {error}", path.display()))
    })?;
    let value: Value = serde_json::from_str(&source).map_err(|error| {
        WebCommandError::failure(format!("{}: {error}", path.display()))
    })?;
    let object = value.as_object().ok_or_else(|| {
        WebCommandError::failure("Glamour manifest must be a JSON object")
    })?;
    for key in ["appId", "buildId", "templates", "nodes", "regions", "eventClasses", "eventPlans"] {
        if !object.contains_key(key) {
            return Err(WebCommandError::failure(format!(
                "Glamour manifest is missing `{key}`"
            )));
        }
    }
    let app_id = object["appId"]
        .as_u64()
        .filter(|value| *value > 0 && *value <= u32::MAX as u64)
        .ok_or_else(|| WebCommandError::failure("Glamour `appId` must be a positive u32"))?;
    let build_id = match &object["buildId"] {
        Value::String(value) => value.parse::<u64>().ok(),
        Value::Number(value) => value.as_u64(),
        _ => None,
    }
    .filter(|value| *value > 0)
    .ok_or_else(|| {
        WebCommandError::failure("Glamour `buildId` must be a positive decimal u64")
    })?;
    let registry = |key: &str| -> Result<Value, WebCommandError> {
        match object.get(key) {
            Some(value @ Value::Object(_)) => Ok(value.clone()),
            None => Ok(json!({})),
            Some(_) => Err(WebCommandError::failure(format!(
                "Glamour `{key}` must be an object keyed by numeric identity"
            ))),
        }
    };
    let mut normalized = serde_json::Map::new();
    normalized.insert("appId".into(), json!(app_id));
    normalized.insert("buildId".into(), json!(build_id.to_string()));
    for key in [
        "templates",
        "nodes",
        "regions",
        "properties",
        "attributes",
        "aria",
        "ownerInstances",
        "eventClasses",
        "eventPlans",
        "effectDescriptors",
        "subscriptionDescriptors",
    ] {
        normalized.insert(key.into(), registry(key)?);
    }
    match object.get("limits") {
        Some(limits @ Value::Object(_)) => {
            normalized.insert("limits".into(), limits.clone());
        }
        Some(_) => return Err(WebCommandError::failure("Glamour `limits` must be an object")),
        None => {}
    }
    let actions = match object.get("actions") {
        Some(value) => {
            let mut actions: Vec<StaticAction> = serde_json::from_value(value.clone()).map_err(
                |error| WebCommandError::failure(format!("Glamour `actions` are invalid: {error}")),
            )?;
            validate_client_actions(&actions)?;
            authenticate_action_schemas(&mut actions)?;
            serde_json::to_value(actions)
                .map_err(|error| WebCommandError::failure(error.to_string()))?
        }
        None => json!([]),
    };
    normalized.insert("actions".into(), actions);
    Ok(Value::Object(normalized))
}

fn validate_client_actions(actions: &[StaticAction]) -> Result<(), WebCommandError> {
    if actions.len() > 256 {
        return Err(WebCommandError::failure(
            "Glamour `actions` exceed the 256-action limit",
        ));
    }
    let mut action_ids = BTreeSet::new();
    for action in actions {
        if !static_site::valid_form_identity(&action.id) || !action_ids.insert(&action.id) {
            return Err(WebCommandError::failure(format!(
                "Glamour action has invalid or duplicate identity `{}`",
                action.id
            )));
        }
        if !matches!(action.method.as_str(), "GET" | "POST") {
            return Err(WebCommandError::failure(format!(
                "Glamour action `{}` has unsupported method `{}`",
                action.id, action.method
            )));
        }
        if !static_site::valid_static_form_url(&action.action) {
            return Err(WebCommandError::failure(format!(
                "Glamour action `{}` has an unsafe URL",
                action.id
            )));
        }
        if action.fields.len() > 256 {
            return Err(WebCommandError::failure(format!(
                "Glamour action `{}` exceeds the 256-field limit",
                action.id
            )));
        }
        let mut field_names = BTreeSet::new();
        for field in &action.fields {
            if !static_site::valid_static_identifier(&field.name)
                || !field_names.insert(&field.name)
                || field.label.len() > 1024
                || !matches!(
                    field.kind.as_str(),
                    "text" | "email" | "number" | "checkbox" | "secret"
                )
            {
                return Err(WebCommandError::failure(format!(
                    "Glamour action `{}` has invalid field `{}`",
                    action.id, field.name
                )));
            }
            if action.method == "GET" && field.kind == "secret" {
                return Err(WebCommandError::failure(format!(
                    "Glamour action `{}` secret field `{}` requires POST",
                    action.id, field.name
                )));
            }
        }
    }
    Ok(())
}

fn authenticate_action_schemas(actions: &mut [StaticAction]) -> Result<(), WebCommandError> {
    let mut input_schemas = BTreeSet::new();
    let mut result_schemas = BTreeSet::new();
    for action in actions {
        action.input_schema = action_schema_identity(b"witchy.glamour.action-input.v1", action);
        action.result_schema =
            action_schema_identity(b"witchy.glamour.action-result.v1", action);
        if action.input_schema == action.result_schema {
            return Err(WebCommandError::failure(
                "Glamour action input and result schema identities collide",
            ));
        }
        if !input_schemas.insert(action.input_schema) {
            return Err(WebCommandError::failure(
                "Glamour action input schema identity collision",
            ));
        }
        if !result_schemas.insert(action.result_schema) {
            return Err(WebCommandError::failure(
                "Glamour action result schema identity collision",
            ));
        }
    }
    Ok(())
}

fn action_schema_identity(domain: &[u8], action: &StaticAction) -> u32 {
    let mut material = format!(
        "{}:{}|{}:{}|{}:{}|{}",
        action.id.len(),
        action.id,
        action.method.len(),
        action.method,
        action.action.len(),
        action.action,
        action.fields.len(),
    );
    for (ordinal, field) in action.fields.iter().enumerate() {
        material.push_str(&format!(
            "|{ordinal}:{}:{}:{}:{}:{}",
            field.name.len(),
            field.name,
            field.kind.len(),
            field.kind,
            u8::from(field.required),
        ));
    }
    let mut hash = Sha256::new();
    hash.update(domain);
    hash.update(b"\n");
    hash.update(material.as_bytes());
    let digest = hash.finalize();
    let identity = u32::from_be_bytes(digest[..4].try_into().expect("SHA-256 prefix"));
    identity.max(1)
}

fn write_production(
    checked: &CheckedWeb,
    output: &Path,
) -> Result<Vec<ArtifactRecord>, WebCommandError> {
    if checked.development.is_some() {
        return Err(WebCommandError::failure(
            "production writer received development-only Wasm",
        ));
    }
    write_artifacts(checked, output, "production")
}

fn write_development(
    checked: &CheckedWeb,
    output: &Path,
) -> Result<Vec<ArtifactRecord>, WebCommandError> {
    write_artifacts(checked, output, "development")
}

fn write_artifacts(
    checked: &CheckedWeb,
    output: &Path,
    mode: &'static str,
) -> Result<Vec<ArtifactRecord>, WebCommandError> {
    publish_artifacts(output, &checked.project.root, |staging| {
        populate_artifacts(checked, staging, mode)
    })
}

fn publish_artifacts(
    output: &Path,
    project_root: &Path,
    populate: impl FnOnce(&Path) -> Result<Vec<ArtifactRecord>, WebCommandError>,
) -> Result<Vec<ArtifactRecord>, WebCommandError> {
    let output = absolute_lexical(output)?;
    if output == project_root || output.starts_with(project_root.join("src")) {
        return Err(WebCommandError::failure(
            "web output may not replace the project root or source tree",
        ));
    }
    let parent = output.parent().ok_or_else(|| {
        WebCommandError::failure("web output has no parent directory")
    })?;
    std::fs::create_dir_all(parent).map_err(|error| {
        WebCommandError::failure(format!("cannot create `{}`: {error}", parent.display()))
    })?;
    let name = output.file_name().and_then(|value| value.to_str()).ok_or_else(|| {
        WebCommandError::failure("web output directory has an invalid name")
    })?;
    let staging = parent.join(format!(".{name}.witchy-stage-{}", std::process::id()));
    let backup = parent.join(format!(".{name}.witchy-backup-{}", std::process::id()));
    if staging.exists() || backup.exists() {
        return Err(WebCommandError::failure(
            "a prior web build staging directory still exists; remove that exact stale directory",
        ));
    }
    std::fs::create_dir(&staging).map_err(|error| {
        WebCommandError::failure(format!("cannot create staging directory: {error}"))
    })?;
    let result = populate(&staging);
    if let Err(error) = result {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    let records = result.expect("error handled above");
    if output.exists() {
        std::fs::rename(&output, &backup).map_err(|error| {
            WebCommandError::failure(format!(
                "cannot preserve previous output `{}`: {error}",
                output.display()
            ))
        })?;
    }
    if let Err(error) = std::fs::rename(&staging, &output) {
        if backup.exists() {
            let _ = std::fs::rename(&backup, &output);
        }
        let _ = std::fs::remove_dir_all(&staging);
        return Err(WebCommandError::failure(format!(
            "cannot publish web output `{}`: {error}",
            output.display()
        )));
    }
    if backup.exists() {
        std::fs::remove_dir_all(&backup).map_err(|error| {
            WebCommandError::failure(format!(
                "published output but could not remove preserved predecessor `{}`: {error}",
                backup.display()
            ))
        })?;
    }
    Ok(records)
}

fn populate_artifacts(
    checked: &CheckedWeb,
    staging: &Path,
    mode: &'static str,
) -> Result<Vec<ArtifactRecord>, WebCommandError> {
    let assets = staging.join("assets");
    std::fs::create_dir(&assets).map_err(|error| WebCommandError::failure(error.to_string()))?;
    copy_public(&checked.project.public, staging)?;

    let wasm = embed_web_mount_grant(checked.wasm.clone(), &checked.grant, None, None)?;
    let wasm_name = content_name("app", "wasm", &wasm);
    let witchy_runtime = include_str!("../../web/witchy-runtime/witchy-runtime.mjs").as_bytes();
    let witchy_name = content_name("witchy-runtime", "mjs", witchy_runtime);
    let glamour_runtime =
        bundled_glamour_runtime(&witchy_name, checked.development.is_some());
    let glamour_name = content_name("glamour-runtime", "mjs", glamour_runtime.as_bytes());

    write_file(&assets.join(&wasm_name), &wasm)?;
    write_file(&assets.join(&witchy_name), witchy_runtime)?;
    write_file(&assets.join(&glamour_name), glamour_runtime.as_bytes())?;

    let css_record = if let Some(css) = &checked.project.css {
        let bytes = std::fs::read(css).map_err(|error| {
            WebCommandError::failure(format!("cannot read `{}`: {error}", css.display()))
        })?;
        if bytes.is_empty() {
            None
        } else {
            let name = content_name("app", "css", &bytes);
            write_file(&assets.join(&name), &bytes)?;
            Some(name)
        }
    } else {
        None
    };

    let index_template = std::fs::read_to_string(&checked.project.index).map_err(|error| {
        WebCommandError::failure(format!("cannot read `{}`: {error}", checked.project.index.display()))
    })?;
    let tags = asset_tags(&glamour_name, &wasm_name, css_record.as_deref());
    let index = inject_assets(&index_template, &tags)?;
    let protocol_bytes = serde_json::to_vec(&checked.protocol_manifest)
        .map_err(|error| WebCommandError::failure(error.to_string()))?;
    let grant_bytes = serde_json::to_vec(&checked.grant)
        .map_err(|error| WebCommandError::failure(error.to_string()))?;
    let input_artifacts = serde_json::to_vec(&artifact_records(staging)?)
        .map_err(|error| WebCommandError::failure(error.to_string()))?;
    let input_identity = sha256_many([
        wasm.as_slice(),
        witchy_runtime,
        glamour_runtime.as_bytes(),
        protocol_bytes.as_slice(),
        grant_bytes.as_slice(),
        input_artifacts.as_slice(),
        index.as_bytes(),
    ]);
    let mut web_manifest = checked.protocol_manifest.clone();
    let object = web_manifest.as_object_mut().expect("validated object");
    object.insert("schema".into(), json!("witchy.web.manifest.v1"));
    object.insert("protocolVersion".into(), json!({"major": 1, "minor": 4}));
    object.insert("application".into(), json!({
        "name": checked.project.name,
        "version": checked.project.version,
    }));
    object.insert(
        "mountGrant".into(),
        serde_json::to_value(&checked.grant)
            .map_err(|error| WebCommandError::failure(error.to_string()))?,
    );
    object.insert("buildIdentity".into(), json!(input_identity));
    object.insert("routeBase".into(), json!("/"));
    object.insert("features".into(), json!({
        "mode": mode,
        "developmentExports": checked.development.is_some(),
        "hotSwap": checked.development.as_ref().is_some_and(|value| value.supports_snapshot()),
        "sourceMaps": mode == "development",
    }));
    if let Some(development) = &checked.development {
        object.insert("development".into(), json!({
            "abi": 1,
            "snapshotFormat": development.snapshot_format(),
            "applicationIdentity": application_identity(checked)?,
            "modelSchema": development.model_schema_hex(),
            "authorizationSchema": development.authorization_schema_hex(),
            "templateSchema": template_schema(checked)?,
            "migrationSchemas": development.migration_schema_hexes(),
            "maxSnapshotBytes": if development.supports_snapshot() { 1024 * 1024 } else { 0 },
        }));
    }
    let mut asset_names = vec![wasm_name.clone(), witchy_name.clone(), glamour_name.clone()];
    if let Some(css) = &css_record {
        asset_names.push(css.clone());
    }
    object.insert("assets".into(), json!(asset_names));
    let manifest_bytes = pretty_json(&web_manifest)?;
    write_file(&staging.join("witchy-web-manifest.json"), &manifest_bytes)?;

    write_file(&staging.join("index.html"), index.as_bytes())?;

    let headers = "\
/*
  Content-Security-Policy: default-src 'self'; base-uri 'none'; object-src 'none'; frame-ancestors 'none'; script-src 'self'; style-src 'self'; connect-src 'self'
  Referrer-Policy: no-referrer
  X-Content-Type-Options: nosniff
  Permissions-Policy: camera=(), microphone=(), geolocation=()

/assets/*.wasm
  Content-Type: application/wasm
  Cache-Control: public, max-age=31536000, immutable

/assets/*
  Cache-Control: public, max-age=31536000, immutable
";
    write_file(&staging.join("_headers"), headers.as_bytes())?;

    let mut components = checked
        .packages
        .iter()
        .map(|package| {
            let reference = format!(
                "witchy:{}:{}@{}",
                package.source, package.name, package.version
            );
            json!({
                "bom-ref": reference,
                "type": "library",
                "name": package.name,
                "version": package.version,
                "properties": [
                    {"name": "witchy.package.source", "value": package.source}
                ],
            })
        })
        .collect::<Vec<_>>();
    components.extend([
        json!({
            "bom-ref": format!("witchy-runtime@{}", env!("CARGO_PKG_VERSION")),
            "type": "framework",
            "name": "witchy-runtime",
            "version": env!("CARGO_PKG_VERSION"),
        }),
        json!({
            "bom-ref": format!("glamour-runtime@{}", env!("CARGO_PKG_VERSION")),
            "type": "framework",
            "name": "glamour-runtime",
            "version": env!("CARGO_PKG_VERSION"),
        }),
    ]);
    let application_reference =
        format!("witchy-app:{}@{}", checked.project.name, checked.project.version);
    let component_references = components
        .iter()
        .filter_map(|component| component["bom-ref"].as_str())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let dependencies = std::iter::once(json!({
        "ref": application_reference,
        "dependsOn": component_references,
    }))
        .chain(component_references.iter().map(|reference| {
            json!({"ref": reference, "dependsOn": []})
        }))
        .collect::<Vec<_>>();
    let sbom = json!({
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "version": 1,
        "metadata": {
            "component": {
                "bom-ref": application_reference,
                "type": "application",
                "name": checked.project.name,
                "version": checked.project.version,
            },
            "properties": [
                {"name": "witchy.package.graph", "value": "authenticated linked declarations"}
            ],
        },
        "components": components,
        "dependencies": dependencies,
    });
    write_file(&staging.join("witchy-sbom.cdx.json"), &pretty_json(&sbom)?)?;

    let mut records = artifact_records(staging)?;
    let report = json!({
        "schema": "witchy.web.build-report.v1",
        "mode": mode,
        "application": {"name": checked.project.name, "version": checked.project.version},
        "buildIdentity": input_identity,
        "compiler": {
            "version": env!("CARGO_PKG_VERSION"),
            "commit": option_env!("WITCHY_BUILD_COMMIT"),
        },
        "capabilities": {
            "runtime": [],
            "build": [],
            "user": ["UiRoot"],
        },
        "protocol": {"major": 1, "minor": 4},
        "wasm": {
            "exports": checked.exports,
            "imports": checked.host_imports,
            "developmentExports": checked.development.is_some(),
            "hotSwap": checked.development.as_ref().is_some_and(|value| value.supports_snapshot()),
        },
        "sourceMaps": {"emitted": false, "sourcesEmbedded": false},
        "headers": {"crossOriginIsolation": false, "file": "_headers"},
        "packages": checked.packages,
        "artifacts": records,
    });
    write_file(&staging.join("witchy-build-report.json"), &pretty_json(&report)?)?;
    audit_generated_artifacts(staging, checked, mode)?;
    records = artifact_records(staging)?;
    Ok(records)
}

fn bundled_glamour_runtime(
    witchy_runtime_name: &str,
    development: bool,
) -> String {
    let protocol = strip_exports(&strip_static_imports(include_str!(
        "../../web/witchy-runtime/glamour-protocol.mjs"
    )));
    let effects = strip_exports(&strip_static_imports(include_str!(
        "../../web/witchy-runtime/glamour-effect-host.mjs"
    )));
    let completion_codecs = strip_exports(&strip_static_imports(include_str!(
        "../../web/witchy-runtime/glamour-completion-codecs.mjs"
    )));
    let forms = strip_exports(&strip_static_imports(include_str!(
        "../../web/witchy-runtime/glamour-forms.mjs"
    )));
    let optimized = strip_exports(&strip_static_imports(include_str!(
        "../../web/witchy-runtime/glamour-optimized.mjs"
    )));
    let development_runtime = if development {
        let source = strip_exports(&strip_static_imports(include_str!(
            "../../web/witchy-runtime/glamour-development.mjs"
        )));
        format!(
            "const {{ installDevelopmentSwap }} = (() => {{\n{source}\n  return {{ installDevelopmentSwap }};\n}})();"
        )
    } else {
        "const installDevelopmentSwap = null;".to_string()
    };
    format!(
        "import {{ instantiate as instantiateWitchy }} from \"./{witchy_runtime_name}\";\n\
         const __witchyProtocol = (() => {{\n{protocol}\n\
           return {{ ActionCompletionStatus, ActionFieldKind, CompletionSource, CompletionStatus, FrameKind, createOutputValidator, encodeActionCompletionFrame, encodeActionInputFrame, encodeActivationFrame, encodeEffectCompletionFrame, encodeEventFrame, encodeOutputFrame }};\n\
         }})();\n\
         const {{ ActionCompletionStatus, ActionFieldKind, CompletionSource, CompletionStatus, FrameKind, createOutputValidator, encodeActionCompletionFrame, encodeActionInputFrame, encodeActivationFrame, encodeEffectCompletionFrame, encodeEventFrame, encodeOutputFrame }} = __witchyProtocol;\n\
         const {{ createEffectHost }} = (() => {{\n{effects}\n\
           return {{ createEffectHost }};\n\
         }})();\n\
         const {{ encodeCompletionResult }} = (() => {{\n{completion_codecs}\n\
           return {{ encodeCompletionResult }};\n\
         }})();\n\
         const {{ installProgressiveForms }} = (() => {{\n{forms}\n\
           return {{ installProgressiveForms }};\n\
         }})();\n\
         const {{ mountOptimized }} = (() => {{\n{optimized}\n\
           return {{ mountOptimized }};\n\
         }})();\n\
         {development_runtime}\n\
         export {{ mountOptimized }};\n\
         async function authenticateMountGrant(wasmBytes, mountGrant, artifact = null) {{\n\
           const module = await WebAssembly.compile(wasmBytes);\n\
           const sections = WebAssembly.Module.customSections(module, \"witchy.web.mount-grant\");\n\
           if (sections.length !== 1) throw new Error(\"Glamour executable mount grant is missing or duplicated\");\n\
           let embedded;\n\
           try {{ embedded = JSON.parse(new TextDecoder(\"utf-8\", {{ fatal: true }}).decode(sections[0])); }} catch {{ throw new Error(\"Glamour executable mount grant is malformed\"); }}\n\
           if (embedded?.schema !== \"witchy.web.mount-grant-section.v1\" || JSON.stringify(embedded.grant) !== JSON.stringify(mountGrant) || embedded.artifact !== artifact || embedded.artifactGrant !== null) throw new Error(\"Glamour executable mount grant does not match its manifest\");\n\
           return module;\n\
         }}\n\
         async function bootWitchyApplication(script) {{\n\
           const root = document.querySelector(script.dataset.witchyRoot || \"#app\");\n\
           if (!root) throw new Error(\"Glamour root element is missing\");\n\
           const [wasmResponse, manifestResponse] = await Promise.all([\n\
             fetch(script.dataset.witchyWasm, {{ credentials: \"same-origin\" }}),\n\
             fetch(script.dataset.witchyManifest, {{ credentials: \"same-origin\" }}),\n\
           ]);\n\
           if (!wasmResponse.ok || !manifestResponse.ok) throw new Error(\"Glamour application artifacts could not be loaded\");\n\
           const manifest = await manifestResponse.json();\n\
           const startFrame = encodeOutputFrame({{\n\
             kind: FrameKind.Start,\n\
             appId: manifest.appId,\n\
             buildId: BigInt(manifest.buildId),\n\
             sequence: 0n,\n\
           }});\n\
           const mountGrant = manifest.mountGrant;\n\
           if (!mountGrant || mountGrant.schema !== \"witchy.web.ui-root-grant.v1\" || mountGrant.capability !== \"UiRoot\" || typeof mountGrant.policy !== \"string\") throw new Error(\"Glamour application mount grant is invalid\");\n\
           const instantiateOptions = {{ userCaps: [[mountGrant.policy]] }};\n\
           const wasmModule = await authenticateMountGrant(await wasmResponse.arrayBuffer(), mountGrant);\n\
           const application = await mountOptimized(wasmModule, root, {{\n\
             manifest,\n\
             startFrame,\n\
             instantiateOptions,\n\
           }});\n\
           if (manifest.features?.mode === \"development\" && installDevelopmentSwap) {{\n\
             installDevelopmentSwap({{ application, root, manifest, mountOptimized, instantiateOptions }});\n\
           }}\n\
           return application;\n\
         }}\n\
         const witchyScript = document.querySelector(\"script[data-witchy-app]\");\n\
         if (witchyScript) bootWitchyApplication(witchyScript).catch((error) => {{\n\
           const root = document.querySelector(witchyScript.dataset.witchyRoot || \"#app\");\n\
           if (root) root.textContent = `Application failed to start: ${{error instanceof Error ? error.message : \"unknown error\"}}`;\n\
         }});\n"
    )
}

fn embed_web_mount_grant(
    mut wasm: Vec<u8>,
    grant: &WebUiGrant,
    artifact: Option<&str>,
    artifact_grant: Option<&Value>,
) -> Result<Vec<u8>, WebCommandError> {
    use std::borrow::Cow;
    use wasm_encoder::Section as _;

    let data = serde_json::to_vec(&json!({
        "schema": "witchy.web.mount-grant-section.v1",
        "grant": grant,
        "artifact": artifact,
        "artifactGrant": artifact_grant,
    }))
    .map_err(|error| WebCommandError::failure(error.to_string()))?;
    wasm_encoder::CustomSection {
        name: Cow::Borrowed("witchy.web.mount-grant"),
        data: Cow::Owned(data),
    }
    .append_to(&mut wasm);
    Ok(wasm)
}

fn static_island_runtime_modules() -> Vec<(String, Vec<u8>)> {
    let mut modules = Vec::new();
    let mut add = |stem: &str, source: String| {
        let name = content_name(stem, "mjs", source.as_bytes());
        modules.push((name.clone(), source.into_bytes()));
        name
    };
    let protocol = add(
        "glamour-protocol",
        include_str!("../../web/witchy-runtime/glamour-protocol.mjs").to_string(),
    );
    let completion = add(
        "glamour-completion-codecs",
        include_str!("../../web/witchy-runtime/glamour-completion-codecs.mjs")
            .replace("./glamour-protocol.mjs", &format!("./{protocol}")),
    );
    let effects = add(
        "glamour-effect-host",
        include_str!("../../web/witchy-runtime/glamour-effect-host.mjs")
            .replace("./glamour-protocol.mjs", &format!("./{protocol}")),
    );
    let forms = add(
        "glamour-forms",
        include_str!("../../web/witchy-runtime/glamour-forms.mjs").to_string(),
    );
    let witchy_source = include_str!("../../web/witchy-runtime/witchy-runtime.mjs");
    let witchy = add("witchy-runtime", witchy_source.to_string());
    let worker_shell = add(
        "glamour-worker-shell",
        include_str!("../../web/witchy-runtime/glamour-worker-shell.mjs")
            .replace("./witchy-runtime.mjs", &format!("./{witchy}")),
    );
    let worker = add(
        "glamour-worker",
        include_str!("../../web/witchy-runtime/glamour-worker.mjs").to_string(),
    );
    let frame = add(
        "glamour-frame",
        include_str!("../../web/witchy-runtime/glamour-frame.mjs").to_string(),
    );
    let optimized = add(
        "glamour-optimized",
        include_str!("../../web/witchy-runtime/glamour-optimized.mjs")
            .replace("./witchy-runtime.mjs", &format!("./{witchy}"))
            .replace("./glamour-protocol.mjs", &format!("./{protocol}"))
            .replace("./glamour-effect-host.mjs", &format!("./{effects}"))
            .replace("./glamour-completion-codecs.mjs", &format!("./{completion}"))
            .replace("./glamour-forms.mjs", &format!("./{forms}")),
    );
    let islands = add(
        "glamour-islands",
        include_str!("../../web/witchy-runtime/glamour-islands.mjs").to_string(),
    );
    let storage = add(
        "glamour-storage",
        include_str!("../../web/witchy-runtime/glamour-storage.mjs").to_string(),
    );
    let boot = static_island_boot_module(
        &protocol,
        &optimized,
        &islands,
        &storage,
        &worker,
        &worker_shell,
        &frame,
    );
    add("glamour-island-boot", boot);
    modules
}

fn static_island_boot_module(
    protocol: &str,
    optimized: &str,
    islands: &str,
    storage: &str,
    worker: &str,
    worker_shell: &str,
    frame: &str,
) -> String {
    format!(r#"import {{ FrameKind, encodeOutputFrame }} from "./{protocol}";
import {{ mountOptimized }} from "./{optimized}";
import {{ installPublishedIslands }} from "./{islands}";
import {{ createStorageEffectHandler }} from "./{storage}";
import {{ createWorkerEffectHandler }} from "./{worker}";
import {{ installFrameCompartments }} from "./{frame}";

const utf8 = new TextEncoder();
const publicationRoot = new URL("../", import.meta.url);
const publicationUrl = (logical) => new URL(String(logical).replace(/^\/+/, ""), publicationRoot);

function requestFields(source, count) {{
  const fields = [];
  let cursor = 0;
  for (let index = 0; index < count; index += 1) {{
    const colon = source.indexOf(":", cursor);
    if (colon < cursor) throw new Error("Glamour host request is malformed");
    const length = Number(source.slice(cursor, colon));
    if (!Number.isSafeInteger(length) || length < 0) throw new Error("Glamour host request length is invalid");
    const start = colon + 1;
    let end = start;
    while (end <= source.length && utf8.encode(source.slice(start, end)).byteLength < length) end += 1;
    if (utf8.encode(source.slice(start, end)).byteLength !== length) throw new Error("Glamour host request field is truncated");
    fields.push(source.slice(start, end));
    cursor = end;
  }}
  if (cursor !== source.length) throw new Error("Glamour host request has trailing data");
  return fields;
}}

function browserHandlers(artifact) {{
  const methods = (value) => [...new Set(value.split(",").map((item) => item.trim()))].sort();
  const within = (value, prefix) =>
    typeof value === "string" && value.startsWith("/") && !value.startsWith("//") &&
    !/[\\\\#\\0]/.test(value) &&
    (prefix === "/" || value === prefix || value.startsWith(prefix.endsWith("/") ? prefix : `${{prefix}}/`));
  const descriptorPolicy = (channel, descriptor) => {{
    const table = channel === "effect" ? artifact.effectDescriptors : artifact.subscriptionDescriptors;
    const policy = table[String(descriptor)]?.policy;
    if (!policy || typeof policy !== "object") throw new Error("descriptor has no build-authenticated policy");
    return policy;
  }};
  const storage = createStorageEffectHandler({{ artifact }});
  const worker = createWorkerEffectHandler({{
    artifact,
    shellUrl: new URL("./{worker_shell}", import.meta.url).href,
    resolveUrl: publicationUrl,
  }});
  return {{
    effects: {{
      timer({{ request, signal, descriptor }}) {{
        const delay = Number(request);
        const policy = descriptorPolicy("effect", descriptor);
        if (policy.kind !== "timer" || !Number.isSafeInteger(delay) || delay < policy.minimum || delay > 2147483647) return Promise.reject(new Error("timer exceeds its build-authenticated policy"));
        return new Promise((resolve, reject) => {{
          const timer = setTimeout(resolve, delay);
          signal.addEventListener("abort", () => {{ clearTimeout(timer); reject(new Error("cancelled")); }}, {{ once: true }});
        }});
      }},
      async request({{ request, signal, descriptor }}) {{
        const [scope, methodText, prefix, method, url, body] = requestFields(request, 6);
        const policy = descriptorPolicy("effect", descriptor);
        const requestedMethods = methods(methodText);
        const granted = policy.kind === "fetch" && policy.scope === scope && policy.prefix === prefix &&
          JSON.stringify(policy.methods) === JSON.stringify(requestedMethods);
        if (!granted || !requestedMethods.includes(method) || !within(url, prefix)) throw new Error("request exceeds its build-authenticated policy");
        const response = await fetch(url, {{ method, body: body === "" ? undefined : body, credentials: "same-origin", signal }});
        return {{ status: response.status, body: await response.text() }};
      }},
      navigation({{ request, descriptor }}) {{
        const [base, rights, path] = requestFields(request, 3);
        const policy = descriptorPolicy("effect", descriptor);
        const granted = policy.kind === "navigation" && policy.base === base && policy.rights === rights;
        if (!granted || !within(path, base)) return Promise.reject(new Error("navigation exceeds its build-authenticated policy"));
        if (rights === "replace") history.replaceState(null, "", path);
        else history.pushState(null, "", path);
        return path;
      }},
      port({{ request, descriptor }}) {{
        const policy = descriptorPolicy("effect", descriptor);
        const semantic = artifact.effectDescriptors[String(descriptor)]?.semantic;
        if (semantic === "host-port") {{
          const [adapter, endpoint, typedRequest] = requestFields(request, 3);
          const admitted = policy.kind === "host-port" && policy.adapter === adapter && policy.endpoint === endpoint &&
            policy.maxRequestBytes === 61440 && policy.maxResultBytes === 512 &&
            (adapter === "credential.get-exchange.v1" || adapter === "credential.create-exchange.v1") &&
            utf8.encode(typedRequest).byteLength <= policy.maxRequestBytes;
          if (!admitted) return Promise.reject(new Error("host port exceeds its build-authenticated policy"));
          return Promise.reject(new Error("credential exchange requires an approved host-custody implementation"));
        }}
        const fields = requestFields(request, 2);
        const name = semantic === "secret" ? fields[1] : fields[0];
        if (policy.kind !== "port" || policy.name !== name) return Promise.reject(new Error("port exceeds its build-authenticated policy"));
        return Promise.reject(new Error("no production port implementation is granted"));
      }},
      storage,
      worker,
    }},
    subscriptions: {{
      interval({{ request, signal, emit, descriptor }}) {{
        const delay = Number(request);
        const policy = descriptorPolicy("subscription", descriptor);
        if (policy.kind !== "timer" || !Number.isSafeInteger(delay) || delay < Math.max(1, policy.minimum) || delay > 2147483647) throw new Error("interval exceeds its build-authenticated policy");
        const timer = setInterval(() => emit(undefined), delay);
        const cancel = () => clearInterval(timer);
        signal.addEventListener("abort", cancel, {{ once: true }});
        return cancel;
      }},
    }},
  }};
}}

async function boot() {{
  const script = document.querySelector("script[data-witchy-islands]");
  const manifestFile = script?.getAttribute("data-witchy-islands-manifest");
  if (!/^islands-[0-9a-f]{{16}}\.json$/.test(manifestFile || "")) throw new Error("Glamour route manifest is missing or invalid");
  const [manifestResponse, artifactResponse] = await Promise.all([
    fetch(publicationUrl(manifestFile), {{ credentials: "same-origin" }}),
    fetch(publicationUrl("witchy-island-artifacts.json"), {{ credentials: "same-origin" }}),
  ]);
  if (!manifestResponse.ok || !artifactResponse.ok) throw new Error("Glamour island publication could not be loaded");
  const manifest = await manifestResponse.json();
  const artifacts = await artifactResponse.json();
  const loader = installPublishedIslands({{
    root: document,
    manifest,
    artifacts,
    fetch: (logical, options) => fetch(publicationUrl(logical), options),
    onError: (error, record) => console.error(`Glamour island ${{record?.key || "unknown"}} failed:`, error),
    mountArtifact: ({{ module, element, artifact, mountGrant, mode, state, trigger }}) => {{
      const handlers = browserHandlers(artifact);
      const startPayload = mode === "resume"
        ? state
        : JSON.stringify({{ route: artifact.fresh?.route || "/", bootstrap: artifact.fresh?.bootstrap || "" }});
      return mountOptimized(module, element, {{
        manifest: artifact,
        startFrame: encodeOutputFrame({{
          kind: FrameKind.Start,
          appId: artifact.appId,
          buildId: BigInt(artifact.buildId),
          payloads: [utf8.encode(startPayload)],
        }}),
        resume: mode === "resume" ? true : undefined,
        replaceRoot: mode === "fresh" ? true : undefined,
        activationEvent: trigger || undefined,
        instantiateOptions: {{ userCaps: [[mountGrant.policy]] }},
        effectHandlers: handlers.effects,
        subscriptionHandlers: handlers.subscriptions,
        installFrames: (options) => installFrameCompartments(options),
      }});
    }},
  }});
  await Promise.all(
    manifest.islands
      .filter((record) => record.activation === "load")
      .map((record) => loader.activate(record.key)),
  );
  document.documentElement.setAttribute("data-witchy-islands-ready", "");
  document.dispatchEvent(new Event("witchy-islands-ready"));
  return loader;
}}

boot().catch((error) => {{
  document.documentElement.setAttribute("data-witchy-islands-failed", "");
  document.dispatchEvent(new Event("witchy-islands-failed"));
  console.error("Glamour islands failed:", error);
}});
"#)
}

fn strip_exports(source: &str) -> String {
    source
        .lines()
        .map(|line| line.strip_prefix("export ").unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn strip_static_imports(source: &str) -> String {
    let mut output = String::new();
    let mut skipping = false;
    for line in source.lines() {
        if !skipping && line.trim_start().starts_with("import ") {
            skipping = !line.contains(';');
            continue;
        }
        if skipping {
            if line.contains(';') {
                skipping = false;
            }
            continue;
        }
        output.push_str(line);
        output.push('\n');
    }
    output
}

fn asset_tags(runtime: &str, wasm: &str, css: Option<&str>) -> String {
    let style = css
        .map(|name| format!("  <link rel=\"stylesheet\" href=\"./assets/{name}\">\n"))
        .unwrap_or_default();
    format!(
        "{style}  <script type=\"module\" src=\"./assets/{runtime}\" data-witchy-app \
         data-witchy-root=\"#app\" data-witchy-wasm=\"./assets/{wasm}\" \
         data-witchy-manifest=\"./witchy-web-manifest.json\"></script>"
    )
}

fn inject_assets(template: &str, tags: &str) -> Result<String, WebCommandError> {
    if template.contains("{{witchy-assets}}") {
        return Ok(template.replace("{{witchy-assets}}", tags));
    }
    if let Some(index) = template.rfind("</body>") {
        let mut output = String::with_capacity(template.len() + tags.len() + 1);
        output.push_str(&template[..index]);
        output.push_str(tags);
        output.push('\n');
        output.push_str(&template[index..]);
        return Ok(output);
    }
    Err(WebCommandError::failure(
        "web index must contain `{{witchy-assets}}` or a closing `</body>`",
    ))
}

fn copy_public(source: &Path, destination: &Path) -> Result<(), WebCommandError> {
    copy_public_except(source, destination, &BTreeSet::new())
}

fn copy_public_except(
    source: &Path,
    destination: &Path,
    excluded: &BTreeSet<PathBuf>,
) -> Result<(), WebCommandError> {
    copy_public_tree(
        source,
        destination,
        destination,
        Path::new(""),
        excluded,
    )
}

fn copy_public_tree(
    source: &Path,
    destination: &Path,
    output_root: &Path,
    relative_root: &Path,
    excluded: &BTreeSet<PathBuf>,
) -> Result<(), WebCommandError> {
    for entry in std::fs::read_dir(source).map_err(|error| {
        WebCommandError::failure(format!("cannot read `{}`: {error}", source.display()))
    })? {
        let entry = entry.map_err(|error| WebCommandError::failure(error.to_string()))?;
        let ty = entry.file_type().map_err(|error| WebCommandError::failure(error.to_string()))?;
        if ty.is_symlink() {
            return Err(WebCommandError::failure(format!(
                "public asset `{}` is a symlink; web builds copy regular files only",
                entry.path().display()
            )));
        }
        let relative = relative_root.join(entry.file_name());
        if excluded.contains(&relative) {
            if !ty.is_file() {
                return Err(WebCommandError::failure(format!(
                    "typed public asset `{}` must be a regular file",
                    entry.path().display()
                )));
            }
            continue;
        }
        let target = destination.join(entry.file_name());
        if reserved_output_path(&target, output_root) {
            return Err(WebCommandError::failure(format!(
                "public asset `{}` collides with a generated artifact",
                entry.path().display()
            )));
        }
        if ty.is_dir() {
            std::fs::create_dir(&target)
                .map_err(|error| WebCommandError::failure(error.to_string()))?;
            copy_public_tree(
                &entry.path(),
                &target,
                output_root,
                &relative,
                excluded,
            )?;
        } else if ty.is_file() {
            std::fs::copy(entry.path(), &target)
                .map_err(|error| WebCommandError::failure(error.to_string()))?;
        } else {
            return Err(WebCommandError::failure("public assets must be regular files"));
        }
    }
    Ok(())
}

fn reserved_output_path(path: &Path, root: &Path) -> bool {
    let relative = path.strip_prefix(root).unwrap_or(path);
    matches!(
        relative.to_str(),
        Some(
            "index.html"
                | "assets"
                | "witchy-web-manifest.json"
                | "witchy-build-report.json"
                | "witchy-sbom.cdx.json"
                | "_headers"
        )
    )
}

fn audit_generated_artifacts(
    root: &Path,
    checked: &CheckedWeb,
    mode: &'static str,
) -> Result<(), WebCommandError> {
    let read_json = |name: &str| -> Result<Value, WebCommandError> {
        let path = root.join(name);
        serde_json::from_slice(&std::fs::read(&path).map_err(|error| {
            WebCommandError::failure(format!("cannot audit `{}`: {error}", path.display()))
        })?)
        .map_err(|error| {
            WebCommandError::failure(format!("generated `{name}` is invalid JSON: {error}"))
        })
    };
    let manifest = read_json("witchy-web-manifest.json")?;
    let report = read_json("witchy-build-report.json")?;
    let sbom = read_json("witchy-sbom.cdx.json")?;
    let root_text = checked.project.root.to_string_lossy();
    if !root_text.is_empty() {
        for record in artifact_records(root)? {
            let bytes = std::fs::read(root.join(&record.path))
                .map_err(|error| WebCommandError::failure(error.to_string()))?;
            if bytes.windows(root_text.len()).any(|window| window == root_text.as_bytes()) {
                return Err(WebCommandError::failure(format!(
                    "generated artifact `{}` leaks the absolute project path",
                    record.path
                )));
            }
        }
    }
    if manifest["buildIdentity"] != report["buildIdentity"] {
        return Err(WebCommandError::failure(
            "generated manifest and build report disagree on build identity",
        ));
    }
    if report["mode"] != mode {
        return Err(WebCommandError::failure(
            "generated build report records the wrong build mode",
        ));
    }
    let assets = manifest["assets"].as_array().ok_or_else(|| {
        WebCommandError::failure("generated manifest asset graph is not an array")
    })?;
    for asset in assets {
        let name = asset.as_str().ok_or_else(|| {
            WebCommandError::failure("generated manifest asset identity is not text")
        })?;
        if Path::new(name).components().count() != 1 {
            return Err(WebCommandError::failure(
                "generated manifest asset identity is not a basename",
            ));
        }
        let bytes = std::fs::read(root.join("assets").join(name)).map_err(|error| {
            WebCommandError::failure(format!("generated manifest asset `{name}` is absent: {error}"))
        })?;
        if !name.contains(&sha256(&bytes)[..16]) {
            return Err(WebCommandError::failure(format!(
                "generated asset `{name}` is not content-addressed"
            )));
        }
    }

    let actual = artifact_records(root)?
        .into_iter()
        .filter(|record| record.path != "witchy-build-report.json")
        .collect::<Vec<_>>();
    if report["artifacts"] != json!(actual) {
        return Err(WebCommandError::failure(
            "generated build report does not match the emitted artifact graph",
        ));
    }

    let components = sbom["components"].as_array().ok_or_else(|| {
        WebCommandError::failure("generated SBOM has no component inventory")
    })?;
    let references = components
        .iter()
        .filter_map(|component| component["bom-ref"].as_str())
        .collect::<BTreeSet<_>>();
    if references.len() != checked.packages.len() + 2 {
        return Err(WebCommandError::failure(
            "generated SBOM does not cover the authenticated package graph and runtimes",
        ));
    }
    let application_reference = sbom["metadata"]["component"]["bom-ref"]
        .as_str()
        .ok_or_else(|| WebCommandError::failure("generated SBOM has no application identity"))?;
    let application_dependencies = sbom["dependencies"]
        .as_array()
        .and_then(|dependencies| {
            dependencies
                .iter()
                .find(|dependency| dependency["ref"] == application_reference)
        })
        .and_then(|dependency| dependency["dependsOn"].as_array())
        .ok_or_else(|| {
            WebCommandError::failure("generated SBOM has no application dependency graph")
        })?;
    if application_dependencies.len() != references.len() {
        return Err(WebCommandError::failure(
            "generated SBOM application dependency graph is incomplete",
        ));
    }

    let runtime_name = assets
        .iter()
        .filter_map(Value::as_str)
        .find(|name| name.starts_with("glamour-runtime-"))
        .ok_or_else(|| WebCommandError::failure("generated Glamour runtime is absent"))?;
    let runtime = std::fs::read_to_string(root.join("assets").join(runtime_name))
        .map_err(|error| WebCommandError::failure(error.to_string()))?;
    let has_development_bridge = runtime.contains("__WITCHY_GLAMOUR_DEV__");
    if has_development_bridge != (mode == "development") {
        return Err(WebCommandError::failure(
            "generated runtime development bridge does not match build mode",
        ));
    }
    Ok(())
}

fn artifact_records(root: &Path) -> Result<Vec<ArtifactRecord>, WebCommandError> {
    fn walk(root: &Path, current: &Path, output: &mut Vec<ArtifactRecord>) -> Result<(), WebCommandError> {
        for entry in std::fs::read_dir(current)
            .map_err(|error| WebCommandError::failure(error.to_string()))?
        {
            let entry = entry.map_err(|error| WebCommandError::failure(error.to_string()))?;
            let ty = entry.file_type().map_err(|error| WebCommandError::failure(error.to_string()))?;
            if ty.is_dir() {
                walk(root, &entry.path(), output)?;
            } else if ty.is_file() {
                let bytes = std::fs::read(entry.path())
                    .map_err(|error| WebCommandError::failure(error.to_string()))?;
                output.push(ArtifactRecord {
                    path: slash_path(entry.path().strip_prefix(root).expect("walk stays below root")),
                    bytes: bytes.len(),
                    sha256: sha256(&bytes),
                });
            }
        }
        Ok(())
    }
    let mut records = Vec::new();
    walk(root, root, &mut records)?;
    records.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(records)
}

fn pretty_json(value: &Value) -> Result<Vec<u8>, WebCommandError> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| WebCommandError::failure(error.to_string()))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn content_name(stem: &str, extension: &str, bytes: &[u8]) -> String {
    format!("{stem}-{}.{}", &sha256(bytes)[..16], extension)
}

fn sha256(bytes: &[u8]) -> String {
    hex_digest(&Sha256::digest(bytes))
}

fn sha256_many<'a>(parts: impl IntoIterator<Item = &'a [u8]>) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    hex_digest(&hash.finalize())
}

fn application_identity(checked: &CheckedWeb) -> Result<String, WebCommandError> {
    let app_id = checked.protocol_manifest["appId"]
        .as_u64()
        .ok_or_else(|| WebCommandError::failure("Glamour manifest has no application identity"))?;
    Ok(sha256_many([
        checked.project.name.as_bytes(),
        checked.project.version.as_bytes(),
        &app_id.to_le_bytes(),
    ]))
}

fn template_schema(checked: &CheckedWeb) -> Result<String, WebCommandError> {
    let object = checked.protocol_manifest.as_object().ok_or_else(|| {
        WebCommandError::failure("Glamour manifest is not an object")
    })?;
    let mut schema = serde_json::Map::new();
    for key in [
        "templates",
        "nodes",
        "regions",
        "properties",
        "attributes",
        "aria",
        "ownerInstances",
        "eventClasses",
        "eventPlans",
        "effectDescriptors",
        "subscriptionDescriptors",
        "limits",
    ] {
        if let Some(value) = object.get(key) {
            schema.insert(key.to_string(), value.clone());
        }
    }
    schema.insert(
        "compilerTemplates".into(),
        compiler_template_registry(&checked.templates, false),
    );
    let bytes = serde_json::to_vec(&schema)
        .map_err(|error| WebCommandError::failure(error.to_string()))?;
    Ok(sha256(&bytes))
}

fn compiler_template_registry(
    templates: &[witchy_lower::codegen::GlamourTemplateMetadata],
    include_origins: bool,
) -> Value {
    Value::Array(
        templates
            .iter()
            .map(|template| {
                let mut record = json!({
                    "identity": &template.identity,
                    "wireId": template.wire_id,
                    "root": compiler_template_node(&template.root),
                    "slots": template.slots.iter().map(|slot| {
                        json!({
                            "index": slot.index,
                            "wireId": slot.wire_id,
                            "node": slot.node,
                            "kind": &slot.kind,
                            "name": &slot.name,
                        })
                    }).collect::<Vec<_>>(),
                });
                if include_origins {
                    record["originIds"] = Value::Array(template.origins.iter().map(|origin| {
                        json!({
                            "module": &origin.id.module,
                            "ordinal": origin.id.ordinal,
                        })
                    }).collect());
                }
                record
            })
            .collect(),
    )
}

fn compiler_operation_source_registry(
    templates: &[witchy_lower::codegen::GlamourTemplateMetadata],
) -> Value {
    let mut records = Vec::new();
    for template in templates {
        for origin in &template.origins {
            records.push(json!({
                "operation": "mount",
                "template": template.wire_id,
                "templateIdentity": &template.identity,
                "originId": {
                    "module": &origin.id.module,
                    "ordinal": origin.id.ordinal,
                },
                "source": origin.invocation,
            }));
            for slot in &template.slots {
                let source = usize::try_from(slot.index)
                    .ok()
                    .and_then(|index| origin.holes.get(index))
                    .unwrap_or(&origin.invocation);
                records.push(json!({
                    "operation": "slot",
                    "template": template.wire_id,
                    "templateIdentity": &template.identity,
                    "slot": slot.wire_id,
                    "slotIndex": slot.index,
                    "node": slot.node,
                    "kind": &slot.kind,
                    "name": &slot.name,
                    "originId": {
                        "module": &origin.id.module,
                        "ordinal": origin.id.ordinal,
                    },
                    "source": source,
                }));
            }
        }
    }
    Value::Array(records)
}

fn append_compiler_work_source_registry(
    mappings: &mut Value,
    islands: &[witchy_lower::codegen::GlamourIslandMetadata],
) {
    let Some(records) = mappings.as_array_mut() else { return };
    for island in islands {
        for work in &island.work {
            records.push(json!({
                "operation": "descriptor",
                "channel": work.channel,
                "kind": work.kind,
                "handler": work.handler,
                "descriptor": work.descriptor_id,
                "resultSchema": work.result_schema_id,
                "completion": work.completion_id,
                "ownerScope": work.owner_scope_id,
                "semantic": browser_policy_semantic(&work.browser_policy),
                "island": island.identity,
                "owner": declaration_identity_json(&work.owner),
            }));
        }
        for work in &island.mapped_work {
            records.push(json!({
                "operation": "descriptor",
                "channel": work.channel,
                "kind": work.kind,
                "handler": work.handler,
                "descriptor": work.descriptor_id,
                "resultSchema": work.result_schema_id,
                "completion": work.completion_id,
                "ownerScope": work.owner_scope_id,
                "semantic": browser_policy_semantic(&work.browser_policy),
                "island": island.identity,
                "owner": declaration_identity_json(&work.owner),
                "mappedFrom": work.previous_descriptor_id,
                "mapper": work.mapper_id,
            }));
        }
    }
}

fn browser_policy_semantic(
    policy: &witchy_lower::codegen::GlamourBrowserPolicyMetadata,
) -> &'static str {
    match policy {
        witchy_lower::codegen::GlamourBrowserPolicyMetadata::Fetch { .. } => "resource",
        witchy_lower::codegen::GlamourBrowserPolicyMetadata::Navigation { .. } => "route",
        witchy_lower::codegen::GlamourBrowserPolicyMetadata::Timer { .. } => "timer",
        witchy_lower::codegen::GlamourBrowserPolicyMetadata::Storage { .. } => "storage",
        witchy_lower::codegen::GlamourBrowserPolicyMetadata::Worker { .. } => "worker",
        witchy_lower::codegen::GlamourBrowserPolicyMetadata::HostPort { .. } => "host-port",
        witchy_lower::codegen::GlamourBrowserPolicyMetadata::Port { .. } => "port",
        witchy_lower::codegen::GlamourBrowserPolicyMetadata::SecretField { .. } => "secret",
    }
}

fn declaration_identity_json(
    identity: &witchy_types::runtime_type::DeclarationIdentity,
) -> Value {
    let source = match identity.package().source() {
        witchy_types::runtime_type::PackageSource::Toolchain => "toolchain".to_string(),
        witchy_types::runtime_type::PackageSource::Workspace => "workspace".to_string(),
        witchy_types::runtime_type::PackageSource::Registry(name) => format!("registry:{name}"),
    };
    json!({
        "source": source,
        "package": identity.package().name(),
        "version": identity.package().version(),
        "module": identity.module(),
        "kind": format!("{:?}", identity.kind()),
        "name": identity.name(),
    })
}

fn compiler_template_node(
    node: &witchy_lower::codegen::GlamourTemplateNodeMetadata,
) -> Value {
    use witchy_lower::codegen::GlamourTemplateNodeMetadata;

    match node {
        GlamourTemplateNodeMetadata::Element {
            node,
            tag,
            attributes,
            children,
        } => json!({
            "kind": "element",
            "node": node,
            "tag": tag,
            "attributes": attributes.iter().map(|attribute| {
                (&attribute.name, &attribute.value)
            }).collect::<BTreeMap<_, _>>(),
            "children": children.iter().map(compiler_template_node).collect::<Vec<_>>(),
        }),
        GlamourTemplateNodeMetadata::Text { node, text } => json!({
            "kind": "text",
            "node": node,
            "text": text,
        }),
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), WebCommandError> {
    std::fs::write(path, bytes).map_err(|error| {
        WebCommandError::failure(format!("cannot write `{}`: {error}", path.display()))
    })
}

fn starter_files(name: &str, source_name: &str) -> BTreeMap<PathBuf, String> {
    let mut files = BTreeMap::new();
    files.insert(
        PathBuf::from("witchy.toml"),
        format!(
            "[rune]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n\
             [capabilities]\nruntime = []\n\n\
             [dependencies]\n\n\
             [web]\nentry = \"src/{source_name}.witchy\"\n\
             delivery = \"static\"\ngrants = \"web/grants.toml\"\n"
        ),
    );
    files.insert(PathBuf::from(format!("src/{source_name}.witchy")), STARTER_SOURCE.into());
    files.insert(
        PathBuf::from("web/grants.toml"),
        format!("[user_caps]\nui = {{ type = \"UiRoot\", policy = \"{name}\" }}\n"),
    );
    files
}

#[cfg(test)]
fn client_fixture_files(name: &str, source_name: &str) -> BTreeMap<PathBuf, String> {
    let mut files = BTreeMap::new();
    files.insert(
        PathBuf::from("witchy.toml"),
        format!(
            "[rune]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n\
             [capabilities]\nruntime = []\n\n\
             [dependencies]\n\n\
             [web]\nentry = \"src/{source_name}.witchy\"\n\
             delivery = \"client\"\n\
             index = \"web/index.html\"\npublic = \"web/public\"\n\
             manifest = \"web/glamour-manifest.json\"\ngrants = \"web/grants.toml\"\n"
        ),
    );
    files.insert(
        PathBuf::from(format!("src/{source_name}.witchy")),
        CLIENT_FIXTURE_SOURCE.into(),
    );
    files.insert(PathBuf::from("web/index.html"), CLIENT_FIXTURE_INDEX.into());
    files.insert(
        PathBuf::from("web/glamour-manifest.json"),
        CLIENT_FIXTURE_MANIFEST.into(),
    );
    files.insert(
        PathBuf::from("web/grants.toml"),
        format!("[user_caps]\nui = {{ type = \"UiRoot\", policy = \"{name}\" }}\n"),
    );
    files
}

#[cfg(test)]
fn static_fixture_files(name: &str, source_name: &str) -> BTreeMap<PathBuf, String> {
    let mut files = client_fixture_files(name, source_name);
    let manifest = files
        .get_mut(Path::new("witchy.toml"))
        .expect("client fixture manifest");
    *manifest = manifest.replace("delivery = \"client\"", "delivery = \"static\"");
    files
}

fn create_project_atomically(
    destination: &Path,
    files: &BTreeMap<PathBuf, String>,
) -> Result<(), WebCommandError> {
    let parent = destination.parent().filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|error| {
        WebCommandError::failure(format!("cannot create `{}`: {error}", parent.display()))
    })?;
    let name = destination.file_name().and_then(|value| value.to_str()).ok_or_else(|| {
        WebCommandError::failure("destination has an invalid directory name")
    })?;
    let staging = parent.join(format!(".{name}.witchy-new-{}", std::process::id()));
    if staging.exists() {
        return Err(WebCommandError::failure(
            "a prior project-creation staging directory still exists",
        ));
    }
    std::fs::create_dir(&staging)
        .map_err(|error| WebCommandError::failure(error.to_string()))?;
    std::fs::create_dir_all(staging.join("web/public"))
        .map_err(|error| WebCommandError::failure(error.to_string()))?;
    for (relative, contents) in files {
        let path = staging.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| WebCommandError::failure(error.to_string()))?;
        }
        if let Err(error) = std::fs::write(&path, contents) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(WebCommandError::failure(format!(
                "cannot write `{}`: {error}",
                path.display()
            )));
        }
    }
    if let Err(error) = std::fs::rename(&staging, destination) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(WebCommandError::failure(format!(
            "cannot publish new project `{}`: {error}",
            destination.display()
        )));
    }
    Ok(())
}

fn validate_destination(path: &Path) -> Result<(), WebCommandError> {
    if path.as_os_str().is_empty() || path == Path::new(".") || path == Path::new("..") {
        return Err(WebCommandError::failure(
            "web project destination must name a new child directory",
        ));
    }
    if path.exists() {
        return Err(WebCommandError::failure(format!(
            "destination `{}` already exists; web project creation never overwrites",
            path.display()
        )));
    }
    Ok(())
}

fn project_name(path: &Path) -> Result<String, WebCommandError> {
    let name = path.file_name().and_then(|value| value.to_str()).ok_or_else(|| {
        WebCommandError::failure("destination does not have a UTF-8 project name")
    })?;
    if name.is_empty()
        || !name.bytes().next().is_some_and(|byte| byte.is_ascii_alphabetic())
        || !name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(WebCommandError::failure(
            "web project name must start with a letter and contain only letters, digits, or `-`",
        ));
    }
    Ok(name.to_ascii_lowercase())
}

fn safe_project_path(
    root: &Path,
    value: &str,
    key: &str,
) -> Result<PathBuf, WebCommandError> {
    let relative = Path::new(value);
    if relative.is_absolute()
        || relative.components().any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(WebCommandError::failure(format!(
            "web `{key}` must be a normalized relative project path"
        )));
    }
    Ok(root.join(relative))
}

fn declared_content_root(root: &Path, value: &str) -> Result<PathBuf, WebCommandError> {
    let relative = Path::new(value);
    if relative.is_absolute() {
        return Err(WebCommandError::failure(
            "web.content must be a relative path so the declaration remains portable",
        ));
    }
    let requested = absolute_lexical(&root.join(relative))?;
    if std::fs::symlink_metadata(&requested)
        .map_err(|error| {
            WebCommandError::failure(format!(
                "cannot inspect web content `{}`: {error}",
                requested.display()
            ))
        })?
        .file_type()
        .is_symlink()
    {
        return Err(WebCommandError::failure(
            "web.content must name a directory directly, not through a symlink",
        ));
    }
    let canonical = std::fs::canonicalize(&requested).map_err(|error| {
        WebCommandError::failure(format!(
            "cannot resolve web content `{}`: {error}",
            requested.display()
        ))
    })?;
    if !canonical.is_dir() {
        return Err(WebCommandError::failure(format!(
            "web content `{}` is not a directory",
            canonical.display()
        )));
    }
    Ok(canonical)
}

fn confined_existing(
    root: &Path,
    path: &Path,
    label: &str,
) -> Result<PathBuf, WebCommandError> {
    let canonical = std::fs::canonicalize(path).map_err(|error| {
        WebCommandError::failure(format!(
            "cannot resolve web {label} `{}`: {error}",
            path.display()
        ))
    })?;
    if !canonical.starts_with(root) {
        return Err(WebCommandError::failure(format!(
            "web {label} `{}` resolves outside the project",
            path.display()
        )));
    }
    Ok(canonical)
}

fn absolute_lexical(path: &Path) -> Result<PathBuf, WebCommandError> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| WebCommandError::failure(error.to_string()))?
            .join(path)
    };
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => output.push(prefix.as_os_str()),
            Component::RootDir => output.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !output.pop() {
                    return Err(WebCommandError::failure("path escapes the filesystem root"));
                }
            }
            Component::Normal(value) => output.push(value),
        }
    }
    Ok(output)
}

fn path_text(path: &Path) -> Result<&str, WebCommandError> {
    path.to_str().ok_or_else(|| WebCommandError::failure("web project path is not UTF-8"))
}

fn slash_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
const CLIENT_FIXTURE_INDEX: &str = "\
<!doctype html>
<html lang=\"en\">
<head>
  <meta charset=\"utf-8\">
  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">
  <title>Witchy app</title>
</head>
<body>
  <main id=\"app\" aria-live=\"polite\"></main>
{{witchy-assets}}
</body>
</html>
";

#[cfg(test)]
const CLIENT_FIXTURE_MANIFEST: &str = "\
{
  \"appId\": 1,
  \"buildId\": \"1\",
  \"templates\": {
    \"1\": {
      \"root\": {
        \"kind\": \"element\",
        \"tag\": \"h1\",
        \"node\": 1,
        \"children\": [
          {\"kind\": \"text\", \"node\": 2, \"text\": \"Hello from Witchy\"}
        ]
      }
    }
  },
  \"nodes\": {\"1\": {}, \"2\": {}},
  \"regions\": {},
  \"properties\": {},
  \"attributes\": {},
  \"aria\": {},
  \"ownerInstances\": {},
  \"eventClasses\": {},
  \"eventPlans\": {},
  \"effectDescriptors\": {},
  \"subscriptionDescriptors\": {}
}
";

#[cfg(test)]
const CLIENT_FIXTURE_SOURCE: &str = r#"import bytes
from glamour import UiRoot

type BrowserState:
    BrowserState(Bool)

fn put_u8(var output: List(Int), value: Int):
    output.push(value % 256)

fn put_u16(var output: List(Int), value: Int):
    put_u8(output, value)
    put_u8(output, value / 256)

fn put_u32(var output: List(Int), value: Int):
    put_u16(output, value % 65536)
    put_u16(output, value / 65536)

fn put_u64(var output: List(Int), value: Int):
    put_u32(output, value % 4294967296)
    put_u32(output, value / 4294967296)

fn output_frame() -> Bytes:
    var output: List(Int) = []
    for byte in [71, 76, 77, 82]:
        put_u8(output, byte)
    put_u16(output, 1)
    put_u16(output, 0)
    put_u8(output, 16)
    put_u8(output, 0)
    put_u16(output, 48)
    put_u32(output, 76)
    put_u32(output, 1)
    put_u32(output, 1)
    put_u64(output, 1)
    put_u64(output, 0)
    put_u32(output, 0)
    put_u32(output, 0)
    put_u16(output, 1)
    put_u16(output, 0)
    put_u32(output, 28)
    put_u32(output, 1)
    put_u32(output, 1)
    put_u32(output, 0)
    put_u32(output, 0)
    put_u32(output, 0)
    match bytes.from_list(output):
        Ok(frame) -> frame
        Err(_) -> bytes.from_string("")

@browser
pub fn glamour_init(_root: UiRoot, _input: Bytes) -> BrowserState:
    BrowserState(true)

@browser
pub fn glamour_dispatch(state: BrowserState, _input: Bytes) -> BrowserState:
    state

@browser
pub fn glamour_emit(_state: BrowserState) -> Bytes:
    output_frame()

@browser
pub fn glamour_release(own state: BrowserState):
    match state:
        BrowserState(_) -> Nil
"#;

const STARTER_SOURCE: &str = r#"from glamour import Cmd, Program, Site, Start, Sub, Ui, UiRoot

type Msg:
    Increment

fn authorize(_root: UiRoot) -> Nil:
    Nil

fn initial(_start: Start) -> Int:
    0

fn start(_auth: Nil, _model: Int) -> Cmd(Msg):
    NoCmd

fn update(_auth: Nil, model: Int, _message: Msg) -> (Int, Cmd(Msg)):
    (model + 1, NoCmd)

fn view(model: Int) -> Ui(Msg):
    glamour.ui(glamour.element("button", [glamour.attribute("type", "button"), glamour.on_event("counter.increment", "click", glamour.event_msg(Increment))], [glamour.text("Count: ${model}")]))

fn subscriptions(_auth: Nil, _model: Int) -> Sub(Msg):
    NoSub

fn app() -> Program(Nil, Int, Msg):
    glamour.program(authorize, initial, start, update, view, subscriptions)

pub fn web() -> Site:
    let counter = glamour.interactive(app(), 0).named("counter").activate(glamour.OnInteraction)
    glamour.site([glamour.static_page("/", glamour.ui(html"<main><h1>Witchy app</h1>${glamour.embed(counter)}</main>"))])
"#;

#[cfg(test)]
mod tests;
