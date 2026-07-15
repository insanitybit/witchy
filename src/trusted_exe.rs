//! RFC-0092 trusted-application executable envelope.
//!
//! The host executable remains a normal platform image. Witchy appends the
//! ordinary compiled WASM, a checked binding plan, and this fixed descriptor.
//! Platform loaders tolerate that overlay, while the launcher can recover and
//! validate it before parsing any command-line arguments.

use sha2::{Digest, Sha256};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use crate::{artifact, ast, capabilities, grants, runtime};

const MAGIC: &[u8; 16] = b"WITCHY-EXE-0092!";
const FORMAT_VERSION: u32 = 1;
/// Version of the Witchy host ABI implemented by this launcher.
pub const HOST_ABI: u32 = 1;

const DIGEST_LEN: usize = 32;
const DESCRIPTOR_PREFIX_LEN: usize = 16 + 4 + 4 + 8 + 8 + 8 + 8 + DIGEST_LEN * 3;
const DESCRIPTOR_LEN: usize = DESCRIPTOR_PREFIX_LEN + DIGEST_LEN;

/// A validated application recovered from a trusted executable image.
#[derive(Debug, Clone, Copy)]
pub struct EmbeddedApplication<'a> {
    pub wasm: &'a [u8],
    /// Canonical, versioned capability-binding plan. Its schema is owned by the
    /// target-binding layer; the envelope treats it as authenticated bytes.
    pub bindings: &'a [u8],
}

#[derive(Debug)]
pub struct OwnedEmbeddedApplication {
    pub wasm: Vec<u8>,
    pub bindings: Vec<u8>,
}

const PLAN_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingPlan {
    version: u32,
    declared: BTreeMap<String, BTreeSet<String>>,
    dirs: Vec<DirPlan>,
    files: Vec<FilePlan>,
    net: Vec<NetPlan>,
    exec: Option<ExecPlan>,
    secrets: Vec<SecretPlan>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DirPlan {
    name: String,
    source: DirSource,
    rights: FsRights,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum DirSource {
    Cwd,
    Path { path: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FilePlan {
    name: String,
    path: String,
    rights: FsRights,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FsRights {
    read: bool,
    write: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NetPlan {
    name: String,
    allow: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecPlan {
    name: String,
    /// `None` is explicit unrestricted process execution. `Some` is the
    /// compiled-in executable-name allow-list.
    programs: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SecretPlan {
    name: String,
    from: String,
    use_only: bool,
}

/// Host resources resolved from a checked binding plan at process startup.
#[derive(Debug)]
pub struct ResolvedBindings {
    pub dir_roots: Vec<std::path::PathBuf>,
    pub dir_rights: Vec<runtime::FsRights>,
    pub file_grants: Vec<std::path::PathBuf>,
    pub file_rights: Vec<runtime::FsRights>,
    pub net_grants: Vec<Vec<String>>,
    pub exec: bool,
    pub exec_allow: Option<Vec<String>>,
    pub secrets: Vec<runtime::SecretGrant>,
    pub declared: capabilities::CapSet,
}

#[derive(Debug, Default, Deserialize)]
struct ProjectManifest {
    #[serde(default)]
    targets: TargetTables,
}

#[derive(Debug, Default, Deserialize)]
struct TargetTables {
    #[serde(default, rename = "trusted-exe")]
    trusted_exe: Option<TrustedTarget>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustedTarget {
    #[serde(default)]
    dirs: BTreeMap<String, RawDirBinding>,
    #[serde(default)]
    files: BTreeMap<String, RawFileBinding>,
    #[serde(default)]
    net: BTreeMap<String, RawNetBinding>,
    #[serde(default)]
    exec: BTreeMap<String, RawExecBinding>,
    #[serde(default)]
    secrets: BTreeMap<String, RawSecretBinding>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "from", rename_all = "kebab-case", deny_unknown_fields)]
enum RawDirBinding {
    Cwd,
    Path { path: String },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "from", rename_all = "kebab-case", deny_unknown_fields)]
enum RawFileBinding {
    Path { path: String },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "from", rename_all = "kebab-case", deny_unknown_fields)]
enum RawNetBinding {
    System,
    Allow { addresses: Vec<String> },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "from", rename_all = "kebab-case", deny_unknown_fields)]
enum RawExecBinding {
    System,
    Allow { programs: Vec<String> },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSecretBinding {
    from: String,
    #[serde(default, rename = "use-only")]
    use_only: bool,
}

/// Check the target configuration against `main` and encode the exact launch
/// recipe that will be authenticated by the executable envelope.
pub fn build_binding_plan(module: &ast::Module, manifest: &str) -> Result<Vec<u8>, String> {
    let parsed: ProjectManifest = toml::from_str(manifest)
        .map_err(|error| format!("trusted-exe manifest is not valid TOML: {error}"))?;
    let mut target = parsed.targets.trusted_exe.unwrap_or_default();
    let main = module
        .items
        .iter()
        .find_map(|item| match item {
            ast::Item::Function(function) if function.name == "main" => Some(function),
            _ => None,
        })
        .ok_or_else(|| "trusted-exe target requires a `main` entrypoint".to_string())?;
    let grantable: BTreeSet<&str> = module
        .items
        .iter()
        .filter_map(|item| match item {
            ast::Item::Type(definition) if definition.grantable => Some(definition.name.as_str()),
            _ => None,
        })
        .collect();

    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let mut net = Vec::new();
    let mut exec = None;
    let mut secret_store_count = 0usize;
    for parameter in &main.params {
        let Some(ty) = parameter.ty.as_ref().map(unqualified) else {
            return Err(format!(
                "trusted-exe cannot synthesize unannotated `main` parameter `{}`; give it a supported concrete type",
                parameter.name
            ));
        };
        let ast::Type::Named(name, args) = ty else {
            return Err(unsupported_parameter(&parameter.name, ty));
        };
        match name.as_str() {
            "Console" | "Clock" | "Rand" | "Env" => {}
            "SecretStore" => secret_store_count += 1,
            "Dir" => {
                let binding = target
                    .dirs
                    .remove(&parameter.name)
                    .ok_or_else(|| binding_error(&target, "dirs", &parameter.name, "Dir[...]"))?;
                let source = match binding {
                    RawDirBinding::Cwd => DirSource::Cwd,
                    RawDirBinding::Path { path } if path.is_empty() => {
                        return Err(format!(
                            "trusted-exe directory binding `{}` has an empty `path`",
                            parameter.name
                        ));
                    }
                    RawDirBinding::Path { path } => DirSource::Path { path },
                };
                dirs.push(DirPlan {
                    name: parameter.name.clone(),
                    source,
                    rights: fs_rights(args, &parameter.name, "Dir")?,
                });
            }
            "File" => {
                let binding = target
                    .files
                    .remove(&parameter.name)
                    .ok_or_else(|| binding_error(&target, "files", &parameter.name, "File[...]"))?;
                let RawFileBinding::Path { path } = binding;
                if path.is_empty() {
                    return Err(format!(
                        "trusted-exe file binding `{}` has an empty `path`",
                        parameter.name
                    ));
                }
                files.push(FilePlan {
                    name: parameter.name.clone(),
                    path,
                    rights: fs_rights(args, &parameter.name, "File")?,
                });
            }
            "Net" => {
                let binding = target
                    .net
                    .remove(&parameter.name)
                    .ok_or_else(|| binding_error(&target, "net", &parameter.name, "Net[...]"))?;
                let allow = match binding {
                    RawNetBinding::System => {
                        vec!["0.0.0.0/0:*".to_string(), "::/0:*".to_string()]
                    }
                    RawNetBinding::Allow { addresses } => addresses,
                };
                net.push(NetPlan { name: parameter.name.clone(), allow });
            }
            "Exec" => {
                if exec.is_some() {
                    return Err("trusted-exe currently supports one root `Exec` parameter".into());
                }
                let binding = target
                    .exec
                    .remove(&parameter.name)
                    .ok_or_else(|| binding_error(&target, "exec", &parameter.name, "Exec"))?;
                let programs = match binding {
                    RawExecBinding::System => None,
                    RawExecBinding::Allow { programs } => Some(programs),
                };
                exec = Some(ExecPlan { name: parameter.name.clone(), programs });
            }
            "Secret" => {
                return Err(format!(
                    "trusted-exe cannot bind root parameter `{}: Secret`; use `SecretStore` with a named provider",
                    parameter.name
                ));
            }
            "NativeLoader" => {
                return Err(format!(
                    "trusted-exe cannot bind root parameter `{}: NativeLoader`; fixed approved module sets are not implemented",
                    parameter.name
                ));
            }
            "List" if matches!(args.as_slice(), [ast::Type::Named(item, inner)] if item == "String" && inner.is_empty()) => {}
            other if grantable.contains(other) => {
                return Err(format!(
                    "trusted-exe cannot bind grantable root parameter `{}: {other}`; this target has no checked provider for user-defined capabilities",
                    parameter.name
                ));
            }
            _ => return Err(unsupported_parameter(&parameter.name, ty)),
        }
    }

    if exec.is_some() && !dirs.iter().any(|dir| dir.rights.read) {
        return Err("trusted-exe `Exec` requires at least one bound `Dir[Read]` used to name confined executables".into());
    }
    if secret_store_count == 0 && !target.secrets.is_empty() {
        return Err("trusted-exe configures named secrets, but `main` has no `SecretStore` parameter".into());
    }
    reject_extra("dirs", &target.dirs)?;
    reject_extra("files", &target.files)?;
    reject_extra("net", &target.net)?;
    reject_extra("exec", &target.exec)?;

    let mut secrets = Vec::new();
    for (name, binding) in target.secrets {
        let Some(variable) = binding.from.strip_prefix("env:") else {
            return Err(format!(
                "trusted-exe secret `{name}` uses unsupported provider `{}` (expected `env:VAR`)",
                binding.from
            ));
        };
        if variable.is_empty() {
            return Err(format!("trusted-exe secret `{name}` has an empty `env:` provider"));
        }
        secrets.push(SecretPlan { name, from: binding.from, use_only: binding.use_only });
    }
    let plan = BindingPlan {
        version: PLAN_VERSION,
        declared: owned_contract(&capabilities::run_grant(module)),
        dirs,
        files,
        net,
        exec,
        secrets,
    };
    serde_json::to_vec(&plan)
        .map_err(|error| format!("cannot encode trusted-exe binding plan: {error}"))
}

/// Decode, cross-check, and resolve a binding plan on the target machine.
pub fn resolve_binding_plan(
    bytes: &[u8],
    wasm: &[u8],
    cwd: &std::path::Path,
) -> Result<ResolvedBindings, String> {
    let plan: BindingPlan = serde_json::from_slice(bytes)
        .map_err(|error| format!("invalid trusted-exe binding plan: {error}"))?;
    if plan.version != PLAN_VERSION {
        return Err(format!(
            "trusted-exe binding-plan version {} is unsupported (expected {PLAN_VERSION})",
            plan.version
        ));
    }
    let declared = artifact::launch_contract(wasm)?
        .ok_or_else(|| "trusted executable application has no `witchy.launch` metadata".to_string())?;
    if owned_contract(&declared) != plan.declared {
        return Err("trusted-exe binding plan does not match the embedded `witchy.launch` contract".into());
    }

    let cwd = std::fs::canonicalize(cwd)
        .map_err(|error| format!("trusted-exe cannot resolve launch cwd `{}`: {error}", cwd.display()))?;
    let mut dir_roots = Vec::new();
    let mut dir_rights = Vec::new();
    for dir in plan.dirs {
        let candidate = match dir.source {
            DirSource::Cwd => cwd.clone(),
            DirSource::Path { path } => {
                let path = std::path::PathBuf::from(path);
                if path.is_absolute() { path } else { cwd.join(path) }
            }
        };
        let root = std::fs::canonicalize(&candidate).map_err(|error| {
            format!(
                "trusted-exe cannot resolve directory binding `{}` at `{}`: {error}",
                dir.name,
                candidate.display()
            )
        })?;
        if !root.is_dir() {
            return Err(format!(
                "trusted-exe directory binding `{}` resolves to `{}`, which is not a directory",
                dir.name,
                root.display()
            ));
        }
        dir_roots.push(root);
        dir_rights.push(runtime::FsRights::new(dir.rights.read, dir.rights.write));
    }

    let mut file_grants = Vec::new();
    let mut file_rights = Vec::new();
    for file in plan.files {
        let configured = std::path::PathBuf::from(&file.path);
        let candidate = if configured.is_absolute() { configured } else { cwd.join(configured) };
        let path = std::fs::canonicalize(&candidate).map_err(|error| {
            format!(
                "trusted-exe cannot resolve file binding `{}` at `{}`: {error}",
                file.name,
                candidate.display()
            )
        })?;
        if !path.is_file() {
            return Err(format!(
                "trusted-exe file binding `{}` resolves to `{}`, which is not a file",
                file.name,
                path.display()
            ));
        }
        if file.rights.read || file.rights.write {
            let mut options = std::fs::OpenOptions::new();
            options.read(file.rights.read).write(file.rights.write);
            options.open(&path).map_err(|error| {
                format!(
                    "trusted-exe cannot open file binding `{}` at `{}` with declared rights: {error}",
                    file.name,
                    path.display()
                )
            })?;
        }
        file_grants.push(path);
        file_rights.push(runtime::FsRights::new(file.rights.read, file.rights.write));
    }

    let mut secrets = Vec::new();
    for secret in plan.secrets {
        let value = grants::resolve_secret_provider(&secret.from).map_err(|error| {
            format!(
                "trusted-exe secret provider `{}` for `{}` cannot resolve: {error}",
                secret.from, secret.name
            )
        })?;
        secrets.push(runtime::SecretGrant {
            name: secret.name,
            bytes: value,
            use_only: secret.use_only,
        });
    }
    Ok(ResolvedBindings {
        dir_roots,
        dir_rights,
        file_grants,
        file_rights,
        net_grants: plan.net.into_iter().map(|binding| binding.allow).collect(),
        exec: plan.exec.is_some(),
        exec_allow: plan.exec.and_then(|binding| binding.programs),
        secrets,
        declared,
    })
}

fn unqualified(mut ty: &ast::Type) -> &ast::Type {
    while let ast::Type::Qualified(_, inner) = ty {
        ty = inner;
    }
    ty
}

fn fs_rights(args: &[ast::Type], parameter: &str, kind: &str) -> Result<FsRights, String> {
    let mut rights = FsRights { read: false, write: false };
    for arg in args {
        match unqualified(arg) {
            ast::Type::Named(name, inner) if name == "Read" && inner.is_empty() => rights.read = true,
            ast::Type::Named(name, inner) if name == "Write" && inner.is_empty() => rights.write = true,
            other => {
                return Err(format!(
                    "trusted-exe `{parameter}: {kind}[...]` has unsupported right {other:?}"
                ));
            }
        }
    }
    Ok(rights)
}

fn unsupported_parameter(name: &str, ty: &ast::Type) -> String {
    format!(
        "trusted-exe has no startup provider for `main` parameter `{name}: {ty:?}`; supported injected values are Console, Clock, Rand, Env, Dir, File, Net, Exec, SecretStore, and List[String] argv"
    )
}

fn reject_extra<T>(section: &str, entries: &BTreeMap<String, T>) -> Result<(), String> {
    if let Some(name) = entries.keys().next() {
        Err(format!(
            "trusted-exe has extra `[targets.trusted-exe.{section}].{name}` with no matching `main` parameter"
        ))
    } else {
        Ok(())
    }
}

fn binding_error(target: &TrustedTarget, expected: &str, name: &str, ty: &str) -> String {
    let actual = if target.dirs.contains_key(name) {
        Some("dirs")
    } else if target.files.contains_key(name) {
        Some("files")
    } else if target.net.contains_key(name) {
        Some("net")
    } else if target.exec.contains_key(name) {
        Some("exec")
    } else {
        None
    };
    match actual {
        Some(section) if section != expected => format!(
            "trusted-exe binding `[targets.trusted-exe.{section}].{name}` is type-incompatible with `main` parameter `{name}: {ty}`; move it to `[targets.trusted-exe.{expected}]`"
        ),
        _ => format!(
            "trusted-exe is missing `[targets.trusted-exe.{expected}].{name}` for `main` parameter `{name}: {ty}`"
        ),
    }
}

fn owned_contract(contract: &capabilities::CapSet) -> BTreeMap<String, BTreeSet<String>> {
    contract
        .iter()
        .map(|(name, rights)| {
            (
                (*name).to_string(),
                rights.iter().map(|right| (*right).to_string()).collect(),
            )
        })
        .collect()
}

/// Append a Witchy application to a native launcher template.
pub fn package(template: &[u8], wasm: &[u8], bindings: &[u8]) -> Result<Vec<u8>, String> {
    if probe(template)?.is_some() {
        return Err("trusted executable launcher template already contains an application".into());
    }
    wasmparser::validate(wasm)
        .map_err(|error| format!("cannot package invalid application WASM: {error}"))?;
    let launch = artifact::launch_contract_payload(wasm)?
        .ok_or_else(|| "cannot package application WASM without `witchy.launch` metadata".to_string())?;

    let wasm_offset = u64::try_from(template.len())
        .map_err(|_| "launcher template is too large".to_string())?;
    let wasm_len = u64::try_from(wasm.len()).map_err(|_| "application WASM is too large".to_string())?;
    let bindings_offset = wasm_offset
        .checked_add(wasm_len)
        .ok_or_else(|| "trusted executable payload offset overflow".to_string())?;
    let bindings_len = u64::try_from(bindings.len())
        .map_err(|_| "trusted executable binding plan is too large".to_string())?;

    let mut descriptor = Vec::with_capacity(DESCRIPTOR_LEN);
    descriptor.extend_from_slice(MAGIC);
    descriptor.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    descriptor.extend_from_slice(&HOST_ABI.to_le_bytes());
    descriptor.extend_from_slice(&wasm_offset.to_le_bytes());
    descriptor.extend_from_slice(&wasm_len.to_le_bytes());
    descriptor.extend_from_slice(&bindings_offset.to_le_bytes());
    descriptor.extend_from_slice(&bindings_len.to_le_bytes());
    descriptor.extend_from_slice(&digest(wasm));
    descriptor.extend_from_slice(&digest(launch));
    descriptor.extend_from_slice(&digest(bindings));
    let descriptor_digest = digest(&descriptor);
    descriptor.extend_from_slice(&descriptor_digest);
    debug_assert_eq!(descriptor.len(), DESCRIPTOR_LEN);

    let capacity = template
        .len()
        .checked_add(wasm.len())
        .and_then(|n| n.checked_add(bindings.len()))
        .and_then(|n| n.checked_add(DESCRIPTOR_LEN))
        .ok_or_else(|| "trusted executable image is too large".to_string())?;
    let mut image = Vec::with_capacity(capacity);
    image.extend_from_slice(template);
    image.extend_from_slice(wasm);
    image.extend_from_slice(bindings);
    image.extend_from_slice(&descriptor);
    Ok(image)
}

/// Load an embedded app from a platform executable without imposing a full
/// binary read on ordinary `witchy` invocations. The common case reads only the
/// fixed tail descriptor; the complete image is read only when its magic is
/// present.
pub fn load(path: &std::path::Path) -> Result<Option<OwnedEmbeddedApplication>, String> {
    use std::io::{Read as _, Seek as _, SeekFrom};

    let mut file = std::fs::File::open(path)
        .map_err(|error| format!("cannot open executable `{}`: {error}", path.display()))?;
    let len = file
        .metadata()
        .map_err(|error| format!("cannot inspect executable `{}`: {error}", path.display()))?
        .len();
    if len < DESCRIPTOR_LEN as u64 {
        return Ok(None);
    }
    file.seek(SeekFrom::End(-(DESCRIPTOR_LEN as i64)))
        .map_err(|error| format!("cannot seek executable `{}`: {error}", path.display()))?;
    let mut descriptor = [0u8; DESCRIPTOR_LEN];
    file.read_exact(&mut descriptor)
        .map_err(|error| format!("cannot read executable descriptor `{}`: {error}", path.display()))?;
    if &descriptor[..MAGIC.len()] != MAGIC {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|error| format!("cannot rewind executable `{}`: {error}", path.display()))?;
    let mut image = Vec::new();
    file.read_to_end(&mut image)
        .map_err(|error| format!("cannot read executable `{}`: {error}", path.display()))?;
    let embedded = probe(&image)?
        .ok_or_else(|| "trusted executable descriptor disappeared while reading the artifact".to_string())?;
    Ok(Some(OwnedEmbeddedApplication {
        wasm: embedded.wasm.to_vec(),
        bindings: embedded.bindings.to_vec(),
    }))
}

/// Write one self-contained executable, preserving the launcher template's
/// platform mode bits. A same-directory temporary keeps replacement atomic.
pub fn package_file(
    template_path: &std::path::Path,
    output_path: &std::path::Path,
    wasm: &[u8],
    bindings: &[u8],
) -> Result<(), String> {
    use std::io::Write as _;

    let template_identity = std::fs::canonicalize(template_path)
        .map_err(|error| format!("cannot resolve launcher template `{}`: {error}", template_path.display()))?;
    let overwrites_template = template_path == output_path
        || (output_path.exists()
            && std::fs::canonicalize(output_path)
                .is_ok_and(|output| output == template_identity));
    if overwrites_template {
        return Err("trusted-exe output must not overwrite the running launcher template".into());
    }
    let template = std::fs::read(template_path)
        .map_err(|error| format!("cannot read launcher template `{}`: {error}", template_path.display()))?;
    let image = package(&template, wasm, bindings)?;
    let permissions = std::fs::metadata(template_path)
        .map_err(|error| format!("cannot inspect launcher template `{}`: {error}", template_path.display()))?
        .permissions();
    let parent = output_path.parent().unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create trusted-exe output directory `{}`: {error}", parent.display()))?;
    let file_name = output_path.file_name().and_then(|name| name.to_str()).unwrap_or("application");
    let temporary = parent.join(format!(".{file_name}.witchy-tmp-{}", std::process::id()));
    let result = (|| -> Result<(), String> {
        let mut file = std::fs::File::create(&temporary)
            .map_err(|error| format!("cannot create trusted-exe temporary `{}`: {error}", temporary.display()))?;
        file.write_all(&image)
            .map_err(|error| format!("cannot write trusted-exe temporary `{}`: {error}", temporary.display()))?;
        file.sync_all()
            .map_err(|error| format!("cannot sync trusted-exe temporary `{}`: {error}", temporary.display()))?;
        std::fs::set_permissions(&temporary, permissions)
            .map_err(|error| format!("cannot make trusted executable `{}` runnable: {error}", temporary.display()))?;
        #[cfg(windows)]
        if output_path.exists() {
            std::fs::remove_file(output_path).map_err(|error| {
                format!("cannot replace trusted executable `{}`: {error}", output_path.display())
            })?;
        }
        std::fs::rename(&temporary, output_path).map_err(|error| {
            format!("cannot install trusted executable `{}`: {error}", output_path.display())
        })?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// Detect and validate an embedded trusted application.
///
/// `Ok(None)` identifies the ordinary Witchy toolchain binary. Once the magic
/// is present every malformed field fails closed; it is never reinterpreted as
/// compiler command-line input.
pub fn probe(image: &[u8]) -> Result<Option<EmbeddedApplication<'_>>, String> {
    if image.len() < DESCRIPTOR_LEN {
        return Ok(None);
    }
    let descriptor = &image[image.len() - DESCRIPTOR_LEN..];
    if &descriptor[..MAGIC.len()] != MAGIC {
        return Ok(None);
    }
    let stored_descriptor_digest = &descriptor[DESCRIPTOR_PREFIX_LEN..];
    if digest(&descriptor[..DESCRIPTOR_PREFIX_LEN]) != stored_descriptor_digest {
        return Err("trusted executable descriptor digest mismatch (artifact is corrupt)".into());
    }

    let format = read_u32(descriptor, 16)?;
    if format != FORMAT_VERSION {
        return Err(format!(
            "trusted executable format version {format} is unsupported by this launcher (expected {FORMAT_VERSION})"
        ));
    }
    let host_abi = read_u32(descriptor, 20)?;
    if host_abi != HOST_ABI {
        return Err(format!(
            "trusted executable requires Witchy host ABI {host_abi}, but this launcher implements ABI {HOST_ABI}"
        ));
    }
    let wasm_offset = read_usize(descriptor, 24, "WASM offset")?;
    let wasm_len = read_usize(descriptor, 32, "WASM length")?;
    let bindings_offset = read_usize(descriptor, 40, "binding-plan offset")?;
    let bindings_len = read_usize(descriptor, 48, "binding-plan length")?;
    let tail_offset = image.len() - DESCRIPTOR_LEN;
    let wasm_end = wasm_offset
        .checked_add(wasm_len)
        .ok_or_else(|| "trusted executable WASM range overflows".to_string())?;
    let bindings_end = bindings_offset
        .checked_add(bindings_len)
        .ok_or_else(|| "trusted executable binding-plan range overflows".to_string())?;
    if wasm_end != bindings_offset || bindings_end != tail_offset {
        return Err("trusted executable descriptor contains non-contiguous or out-of-bounds payload ranges".into());
    }
    let wasm = image
        .get(wasm_offset..wasm_end)
        .ok_or_else(|| "trusted executable descriptor points outside the artifact".to_string())?;
    let bindings = image
        .get(bindings_offset..bindings_end)
        .ok_or_else(|| "trusted executable descriptor points outside the artifact".to_string())?;

    require_digest("application WASM", wasm, &descriptor[56..88])?;
    require_digest("binding plan", bindings, &descriptor[120..152])?;
    wasmparser::validate(wasm)
        .map_err(|error| format!("trusted executable contains invalid application WASM: {error}"))?;
    let launch = artifact::launch_contract_payload(wasm)?
        .ok_or_else(|| "trusted executable application has no `witchy.launch` metadata".to_string())?;
    require_digest("`witchy.launch` contract", launch, &descriptor[88..120])?;

    Ok(Some(EmbeddedApplication { wasm, bindings }))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let raw: [u8; 4] = bytes
        .get(offset..offset + 4)
        .and_then(|slice| slice.try_into().ok())
        .ok_or_else(|| "trusted executable descriptor is truncated".to_string())?;
    Ok(u32::from_le_bytes(raw))
}

fn read_usize(bytes: &[u8], offset: usize, label: &str) -> Result<usize, String> {
    let raw: [u8; 8] = bytes
        .get(offset..offset + 8)
        .and_then(|slice| slice.try_into().ok())
        .ok_or_else(|| "trusted executable descriptor is truncated".to_string())?;
    usize::try_from(u64::from_le_bytes(raw))
        .map_err(|_| format!("trusted executable {label} does not fit this platform"))
}

fn require_digest(label: &str, bytes: &[u8], expected: &[u8]) -> Result<(), String> {
    if digest(bytes) == expected {
        Ok(())
    } else {
        Err(format!("trusted executable {label} digest mismatch (artifact is corrupt)"))
    }
}

fn digest(bytes: &[u8]) -> [u8; DIGEST_LEN] {
    Sha256::digest(bytes).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_encoder::Module;

    fn app_wasm() -> Vec<u8> {
        let source = crate::parser::parse_module("fn main(console: Console):\n    return\n").unwrap();
        artifact::embed_launch_contract(Module::new().finish(), &source)
    }

    fn module(source: &str) -> ast::Module {
        crate::parser::parse_module(source).unwrap()
    }

    #[test]
    fn package_round_trips_exact_payloads() {
        let wasm = app_wasm();
        let image = package(b"native-launcher", &wasm, b"bindings-v1").unwrap();
        let embedded = probe(&image).unwrap().expect("embedded app");
        assert_eq!(embedded.wasm, wasm);
        assert_eq!(embedded.bindings, b"bindings-v1");
    }

    #[test]
    fn ordinary_launcher_is_not_misclassified() {
        assert!(probe(b"native-launcher").unwrap().is_none());
    }

    #[test]
    fn corrupt_payload_fails_closed() {
        let wasm = app_wasm();
        let mut image = package(b"native-launcher", &wasm, b"").unwrap();
        image[b"native-launcher".len()] ^= 1;
        let error = probe(&image).expect_err("corruption must be diagnosed");
        assert!(error.contains("application WASM digest mismatch"), "{error}");
    }

    #[test]
    fn corrupt_descriptor_fails_closed() {
        let wasm = app_wasm();
        let mut image = package(b"native-launcher", &wasm, b"").unwrap();
        let abi = image.len() - DESCRIPTOR_LEN + 20;
        image[abi] ^= 1;
        let error = probe(&image).expect_err("descriptor corruption must be diagnosed");
        assert!(error.contains("descriptor digest mismatch"), "{error}");
    }

    #[test]
    fn incompatible_host_abi_fails_before_payload_use() {
        let wasm = app_wasm();
        let mut image = package(b"native-launcher", &wasm, b"").unwrap();
        let descriptor = image.len() - DESCRIPTOR_LEN;
        image[descriptor + 20..descriptor + 24].copy_from_slice(&(HOST_ABI + 1).to_le_bytes());
        let checksum = digest(&image[descriptor..descriptor + DESCRIPTOR_PREFIX_LEN]);
        image[descriptor + DESCRIPTOR_PREFIX_LEN..].copy_from_slice(&checksum);
        let error = probe(&image).expect_err("an incompatible ABI must fail closed");
        assert!(error.contains("requires Witchy host ABI"), "{error}");
    }

    #[test]
    fn checked_plan_binds_each_named_dir_in_parameter_order() {
        let source = module(
            "fn main(console: Console, cwd: Dir[Read], root: Dir[Read], args: List[String]):\n    return\n",
        );
        let manifest = "[targets.trusted-exe.dirs]\n\
                        cwd = { from = \"cwd\" }\n\
                        root = { from = \"path\", path = \"/\" }\n";
        let bytes = build_binding_plan(&source, manifest).unwrap();
        let wasm = artifact::embed_launch_contract(Module::new().finish(), &source);
        let resolved = resolve_binding_plan(&bytes, &wasm, std::path::Path::new(".")).unwrap();
        assert_eq!(resolved.dir_roots.len(), 2);
        assert_eq!(resolved.dir_roots[0], std::fs::canonicalize(".").unwrap());
        assert_eq!(resolved.dir_roots[1], std::path::Path::new("/"));
        assert_eq!(resolved.dir_rights, vec![runtime::FsRights::new(true, false); 2]);
    }

    #[test]
    fn missing_and_extra_resource_bindings_fail_the_build() {
        let source = module("fn main(work: Dir[Read]):\n    return\n");
        let missing = build_binding_plan(&source, "").unwrap_err();
        assert!(missing.contains("missing `[targets.trusted-exe.dirs].work`"), "{missing}");

        let extra = build_binding_plan(
            &module("fn main():\n    return\n"),
            "[targets.trusted-exe.files]\nconfig = { from = \"path\", path = \"x\" }\n",
        )
        .unwrap_err();
        assert!(extra.contains("extra `[targets.trusted-exe.files].config`"), "{extra}");

        let mistyped = build_binding_plan(
            &source,
            "[targets.trusted-exe.files]\nwork = { from = \"path\", path = \"x\" }\n",
        )
        .unwrap_err();
        assert!(mistyped.contains("type-incompatible"), "{mistyped}");

        let duplicate = build_binding_plan(
            &source,
            "[targets.trusted-exe.dirs]\nwork = { from = \"cwd\" }\nwork = { from = \"path\", path = \"/\" }\n",
        )
        .unwrap_err();
        assert!(duplicate.contains("duplicate key"), "{duplicate}");
    }

    #[test]
    fn unsupported_root_secret_fails_the_build() {
        let error = build_binding_plan(
            &module("fn main(key: Secret):\n    return\n"),
            "[targets.trusted-exe.secrets]\nkey = { from = \"env:KEY\" }\n",
        )
        .unwrap_err();
        assert!(error.contains("cannot bind root parameter `key: Secret`"), "{error}");
    }

    #[test]
    fn net_and_exec_require_explicit_compiled_in_policy() {
        let source = module(
            "fn main(root: Dir[Read], network: Net[Connect], listener: Net[Listen], runner: Exec):\n    return\n",
        );
        let manifest = "[targets.trusted-exe.dirs]\n\
                        root = { from = \"cwd\" }\n\
                        [targets.trusted-exe.net]\n\
                        network = { from = \"allow\", addresses = [\"api.example.com:443\"] }\n\
                        listener = { from = \"allow\", addresses = [\"127.0.0.1:8080\"] }\n\
                        [targets.trusted-exe.exec]\n\
                        runner = { from = \"allow\", programs = [\"git\"] }\n";
        let bytes = build_binding_plan(&source, manifest).unwrap();
        let plan: BindingPlan = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(plan.net[0].allow, ["api.example.com:443"]);
        assert_eq!(plan.net[1].allow, ["127.0.0.1:8080"]);
        assert_eq!(plan.exec.unwrap().programs.unwrap(), ["git"]);

        let missing = build_binding_plan(
            &source,
            "[targets.trusted-exe.dirs]\nroot = { from = \"cwd\" }\n",
        )
        .unwrap_err();
        assert!(missing.contains("[targets.trusted-exe.net].network"), "{missing}");
    }

    #[test]
    fn binding_plan_is_tied_to_launch_contract() {
        let console = module("fn main(console: Console):\n    return\n");
        let plan = build_binding_plan(&console, "").unwrap();
        let clock = module("fn main(clock: Clock):\n    return\n");
        let wasm = artifact::embed_launch_contract(Module::new().finish(), &clock);
        let error = resolve_binding_plan(&plan, &wasm, std::path::Path::new("."))
            .expect_err("a plan for another contract must fail");
        assert!(error.contains("does not match"), "{error}");
    }
}
