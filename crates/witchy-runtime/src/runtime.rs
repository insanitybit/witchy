//! Wasmtime sandbox and capability host for compiled Witchy programs.
//!
//! Each VM runs in its own wasmtime `Store` (its own linear memory, its own
//! stack). A VM can only reach the outside world through host functions
//! that the runtime explicitly links into *that VM's* `Linker`. Those host
//! functions ARE the capabilities: if a capability was not granted, the import
//! is simply absent and the VM fails to instantiate. There is no ambient
//! authority anywhere.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use std::collections::BTreeMap;

use wasmtime::{
    bail, Cache, CacheConfig, Caller, Config, Engine, Error, Extern, ExternRef, Linker, Memory,
    Module, Result, Store, StoreLimits, StoreLimitsBuilder, Val,
};
use witchy_wir::layout::{HEAP_REDZONE, LayoutBundle};
use witchy_wir::{
    AdmittedCanonicalArtifact, CanonicalEntry, CapabilityKind, CanonicalEntryResult,
};
use witchy_wir::canonical_safewir::ExternalWasmIdentity;
use witchy_wir::abi_catalog::abi_import_info;

mod compiler;
pub mod comptime;
mod host;
mod launch_arguments;
#[cfg(feature = "test-fixtures")]
mod console_fixtures;

const LAYOUT_SECTION: &str = "witchy.layouts";

/// Validate the canonical physical-layout graph and every imported host layout
/// contract before Wasmtime compiles or instantiates an artifact. A missing
/// section is valid only while the imported contracts accept no specialized
/// layouts; duplicate sections, unknown schemas, bad descriptor digests,
/// dangling roots, non-canonical graphs, and uncataloged imports fail closed.
fn validate_layout_metadata(wasm: &[u8]) -> Result<()> {
    // Wasmtime also accepts textual WAT in developer/test entry points. Text
    // is not a distributable binary artifact and cannot carry this custom
    // section; every binary module takes the fail-closed path below.
    if !wasm.starts_with(b"\0asm") {
        return Ok(());
    }
    let mut decoded = None;
    let mut imports = Vec::new();
    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        let payload = payload
            .map_err(|error| Error::msg(format!("invalid wasm metadata: {error}")))?;
        match payload {
            wasmparser::Payload::ImportSection(reader) => {
                for import in reader.into_imports() {
                    let import = import.map_err(|error| {
                        Error::msg(format!("invalid wasm import metadata: {error}"))
                    })?;
                    if import.module == "witchy"
                        && matches!(import.ty, wasmparser::TypeRef::Func(_))
                    {
                        imports.push(import.name.to_owned());
                    }
                }
            }
            wasmparser::Payload::CustomSection(section) if section.name() == LAYOUT_SECTION => {
                if decoded.is_some() {
                    return Err(Error::msg(format!(
                        "invalid `{LAYOUT_SECTION}` metadata: duplicate section"
                    )));
                }
                decoded = Some(LayoutBundle::decode_canonical(section.data()).map_err(|error| {
                    Error::msg(format!("invalid `{LAYOUT_SECTION}` metadata: {error}"))
                })?);
            }
            _ => {}
        }
    }
    for import in imports {
        let info = abi_import_info(&import).ok_or_else(|| {
            Error::msg(format!("host import `{import}` has no generated ABI metadata"))
        })?;
        info.specialized_layouts
            .authenticate(decoded.as_ref().map(|(bundle, layouts)| (bundle, layouts)))
            .map_err(|error| {
                Error::msg(format!("host import `{import}` layout contract rejected: {error}"))
            })?;
    }
    Ok(())
}

/// An on-disk Cranelift compilation cache so re-running the same program skips
/// recompiling its WAT (the ~3 ms compile cost). Keyed by wasm content +
/// wasmtime version, so it is transparent and self-invalidating. Best-effort:
/// returns `None` if a cache directory can't be set up.
const MAX_DEBUG_COMPILATION_CACHE_GENERATIONS: usize = 64;

/// Wasmtime gives every debug executable mtime its own cache directory. Those
/// generations can never be cache hits after the executable changes, but its
/// asynchronous worker intentionally leaves their directories behind. Keep a
/// small overlap for concurrently-running test binaries and remove older,
/// unusable generations before configuring the cache.
fn prune_stale_debug_compilation_cache_generations(cache_dir: &std::path::Path) {
    let modules_dir = cache_dir.join("modules");
    let Ok(entries) = std::fs::read_dir(modules_dir) else {
        return;
    };

    let mut generations = entries
        .flatten()
        .filter_map(|entry| entry.file_type().ok()?.is_dir().then_some(entry))
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            // Wasmtime names debug namespaces
            // `<compiler>-<version>-<executable-mtime-millis>`. Release
            // namespaces do not have this numeric suffix and remain intact.
            let (_, mtime_millis) = name.rsplit_once('-')?;
            let mtime_millis = mtime_millis.parse::<u128>().ok()?;
            Some((mtime_millis, entry.path()))
        })
        .collect::<Vec<_>>();
    generations.sort_unstable_by_key(|(mtime_millis, _)| std::cmp::Reverse(*mtime_millis));

    for (_, stale) in generations.into_iter().skip(MAX_DEBUG_COMPILATION_CACHE_GENERATIONS) {
        // The compilation cache is best-effort. A concurrent process can race
        // this removal, in which case Wasmtime recompiles and atomically
        // repopulates its own generation rather than affecting correctness.
        let _ = std::fs::remove_dir_all(stale);
    }
}

fn compilation_cache() -> Option<Cache> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    let cache_dir = base.join("witchy").join("wasm");
    static PRUNED_DEBUG_COMPILATION_CACHE: OnceLock<()> = OnceLock::new();
    PRUNED_DEBUG_COMPILATION_CACHE.get_or_init(|| {
        prune_stale_debug_compilation_cache_generations(&cache_dir);
    });

    let mut cfg = CacheConfig::new();
    cfg.with_directory(cache_dir);
    Cache::new(cfg).ok()
}

/// Directory for validated Binaryen output. This cache stores ordinary wasm,
/// never serialized native code: every hit still enters Wasmtime through the
/// safe `Module::new` validation path and then benefits from Wasmtime's own
/// compilation cache configured below.
fn optimized_wasm_cache_dir() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))?;
    let dir = base.join("witchy").join("optimized-wasm");
    std::fs::create_dir_all(&dir).ok()?;
    // The cache is not a security boundary (`Module::new` validates every hit),
    // but owner-only permissions also prevent another local user from replacing
    // a valid optimized module with different, still-valid wasm.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    Some(dir)
}

const OPT_CACHE_MAGIC: &[u8; 8] = b"WYOPT002";
const OPT_CACHE_HEADER_LEN: usize = OPT_CACHE_MAGIC.len() + 32 + 32 + 8;
const BINARYEN_RECIPE_SCHEMA: &[u8] = b"witchy-wasm-opt-v3";
const BINARYEN_ARGS: [&str; 6] = [
    "-O4",
    "--all-features",
    "--inlining-optimizing",
    "--inline-functions-with-loops",
    "--flexible-inline-max-function-size=100",
    "--one-caller-inline-max-function-size=200",
];
const PINNED_BINARYEN_VERSION: &[u8] = b"wasm-opt version 131 (version_131)\n";
const BINARYEN_BUNDLE_SCHEMA: &[u8] = b"witchy-binaryen-bundle-v1";
const BUNDLED_BINARYEN_RELATIVE_PATH: &str = "libexec/witchy-binaryen/bin/wasm-opt";
#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
const RELEASE_BINARYEN_EXECUTABLE_SHA256: &str =
    "573d9420ea76dbc1e17b5b7debc1de4182f541e59fa4238761b658d795ddb167";
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
const RELEASE_BINARYEN_EXECUTABLE_SHA256: &str =
    "bad4b6524b2c8e4b27b9aa69bde1a4b9a05ec8887c77ef0d34300f5825acd97c";
#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
const RELEASE_BINARYEN_EXECUTABLE_SHA256: &str =
    "13560cec4415f7fbc0225d8f31168b0d45674e6763f1d93225936cf7d000288f";
#[cfg(all(target_arch = "x86_64", target_os = "macos"))]
const RELEASE_BINARYEN_EXECUTABLE_SHA256: &str =
    "e022347d1375780a09fa412ccef91dfeb136a6a29d6ed5a3101ab42fdba086ea";
#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
const RELEASE_BINARYEN_LIBRARY_SHA256: &str =
    "41916b14f654eff44814d21ed0d73a21913819378fc70dc524cd21430e9db476";
#[cfg(all(target_arch = "x86_64", target_os = "macos"))]
const RELEASE_BINARYEN_LIBRARY_SHA256: &str =
    "ef0aea581645fe624dd9049da13889784e009cc7734717236b4630ddd06a2688";

fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).into()
}

fn sha256_hex(digest: &[u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn binaryen_recipe_evidence() -> String {
    BINARYEN_ARGS.join("|")
}

fn optimized_wasm_input_hash(wasm: &[u8], binaryen_identity: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(BINARYEN_RECIPE_SCHEMA);
    for argument in BINARYEN_ARGS {
        digest.update((argument.len() as u64).to_le_bytes());
        digest.update(argument.as_bytes());
    }
    digest.update((binaryen_identity.len() as u64).to_le_bytes());
    digest.update(binaryen_identity);
    digest.update((wasm.len() as u64).to_le_bytes());
    digest.update(wasm);
    digest.finalize().into()
}

#[derive(Debug)]
struct BinaryenTool {
    executable: std::path::PathBuf,
    executable_digest: [u8; 32],
    runtime_library: Option<(std::path::PathBuf, [u8; 32])>,
    identity: Vec<u8>,
}

impl BinaryenTool {
    fn staged_bytes_are_authentic(&self) -> bool {
        regular_file_sha256(&self.executable) == Some(self.executable_digest)
            && self.runtime_library.as_ref().is_none_or(|(path, expected)| {
                regular_file_sha256(path) == Some(*expected)
            })
    }
}

fn decode_sha256_hex(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut digest = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let nibble = |byte| match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        };
        digest[index] = nibble(pair[0])?.checked_mul(16)?.checked_add(nibble(pair[1])?)?;
    }
    Some(digest)
}

#[cfg(any(
    all(target_arch = "aarch64", target_os = "linux"),
    all(target_arch = "x86_64", target_os = "linux"),
    all(target_arch = "aarch64", target_os = "macos"),
    all(target_arch = "x86_64", target_os = "macos"),
))]
fn release_binaryen_digests() -> Option<([u8; 32], Option<[u8; 32]>)> {
    let executable = decode_sha256_hex(RELEASE_BINARYEN_EXECUTABLE_SHA256)?;
    #[cfg(target_os = "macos")]
    let library = Some(decode_sha256_hex(RELEASE_BINARYEN_LIBRARY_SHA256)?);
    #[cfg(target_os = "linux")]
    let library = None;
    Some((executable, library))
}

#[cfg(not(any(
    all(target_arch = "aarch64", target_os = "linux"),
    all(target_arch = "x86_64", target_os = "linux"),
    all(target_arch = "aarch64", target_os = "macos"),
    all(target_arch = "x86_64", target_os = "macos"),
)))]
fn release_binaryen_digests() -> Option<([u8; 32], Option<[u8; 32]>)> {
    None
}

fn binaryen_staging_root() -> Option<std::path::PathBuf> {
    optimized_wasm_cache_dir().map(|dir| dir.join("trusted-binaryen-v2"))
}

fn regular_file_bytes(path: &std::path::Path) -> Option<Vec<u8>> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return None;
    }
    std::fs::read(path).ok()
}

fn regular_file_sha256(path: &std::path::Path) -> Option<[u8; 32]> {
    regular_file_bytes(path).map(|bytes| sha256(&bytes))
}

fn binaryen_bundle_identity(
    executable_digest: [u8; 32],
    runtime_library_digest: Option<[u8; 32]>,
) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(BINARYEN_BUNDLE_SCHEMA);
    digest.update(b"bin/wasm-opt");
    digest.update(executable_digest);
    if let Some(library_digest) = runtime_library_digest {
        digest.update(b"lib/libbinaryen.dylib");
        digest.update(library_digest);
    }
    digest.finalize().into()
}

fn stage_binaryen_file(
    bytes: &[u8],
    expected_digest: [u8; 32],
    destination: &std::path::Path,
    executable: bool,
) -> Option<()> {
    let parent = destination.parent()?;
    std::fs::create_dir_all(parent).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).ok()?;
    }
    if regular_file_sha256(destination) != Some(expected_digest) {
        let mut random = [0_u8; 8];
        getrandom::fill(&mut random).ok()?;
        let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let temporary = parent.join(format!(".binaryen-{suffix}.tmp"));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary).ok()?;
        std::io::Write::write_all(&mut file, bytes).ok()?;
        file.sync_all().ok()?;
        drop(file);
        if std::fs::rename(&temporary, destination).is_err() {
            let _ = std::fs::remove_file(&temporary);
            return None;
        }
    }
    if regular_file_sha256(destination) != Some(expected_digest) {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if executable { 0o500 } else { 0o400 };
        std::fs::set_permissions(destination, std::fs::Permissions::from_mode(mode)).ok()?;
    }
    Some(())
}

fn authenticate_binaryen(
    configured: &std::path::Path,
    expected_executable_digest: [u8; 32],
    expected_runtime_library_digest: Option<[u8; 32]>,
    staging_root: &std::path::Path,
) -> Option<BinaryenTool> {
    if !configured.is_absolute() {
        return None;
    }
    let canonical = configured.canonicalize().ok()?;
    let executable_bytes = regular_file_bytes(&canonical)?;
    if sha256(&executable_bytes) != expected_executable_digest {
        return None;
    }

    #[cfg(target_os = "macos")]
    let runtime_library = {
        let expected = expected_runtime_library_digest?;
        let distribution_root = canonical.parent()?.parent()?;
        if canonical.parent()?.file_name()? != "bin" {
            return None;
        }
        let path = distribution_root.join("lib").join("libbinaryen.dylib");
        let bytes = regular_file_bytes(&path)?;
        if sha256(&bytes) != expected {
            return None;
        }
        Some((bytes, expected))
    };
    #[cfg(target_os = "linux")]
    let runtime_library: Option<(Vec<u8>, [u8; 32])> = {
        if expected_runtime_library_digest.is_some() {
            return None;
        }
        None
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let runtime_library: Option<(Vec<u8>, [u8; 32])> = return None;

    let bundle_identity = binaryen_bundle_identity(
        expected_executable_digest,
        runtime_library.as_ref().map(|(_, digest)| *digest),
    );
    let digest_hex = sha256_hex(&bundle_identity);
    let directory = staging_root.join(digest_hex);
    let executable = directory.join("bin").join("wasm-opt");
    stage_binaryen_file(
        &executable_bytes,
        expected_executable_digest,
        &executable,
        true,
    )?;
    let staged_runtime_library = if let Some((bytes, digest)) = runtime_library {
        let path = directory.join("lib").join("libbinaryen.dylib");
        stage_binaryen_file(&bytes, digest, &path, false)?;
        Some((path, digest))
    } else {
        None
    };
    let tool = BinaryenTool {
        executable,
        executable_digest: expected_executable_digest,
        runtime_library: staged_runtime_library,
        identity: [bundle_identity.as_slice(), PINNED_BINARYEN_VERSION].concat(),
    };
    if !tool.staged_bytes_are_authentic() {
        return None;
    }
    let output = std::process::Command::new(&tool.executable)
        .arg("--version")
        .env_clear()
        .output()
        .ok()?;
    if !output.status.success() || output.stdout != PINNED_BINARYEN_VERSION {
        return None;
    }
    Some(tool)
}

#[derive(Debug)]
struct ConfiguredBinaryenPin {
    path: Option<std::path::PathBuf>,
    executable_digest: Option<String>,
    runtime_library_digest: Option<String>,
}

impl ConfiguredBinaryenPin {
    fn is_present(&self) -> bool {
        self.path.is_some()
            || self.executable_digest.is_some()
            || self.runtime_library_digest.is_some()
    }
}

fn resolve_configured_binaryen_pin(
    compile: ConfiguredBinaryenPin,
    runtime: ConfiguredBinaryenPin,
    release: ([u8; 32], Option<[u8; 32]>),
) -> std::result::Result<(std::path::PathBuf, [u8; 32], Option<[u8; 32]>), &'static str> {
    let (selected, incomplete) = if compile.is_present() {
        (compile, "compile-time pin is incomplete")
    } else if runtime.is_present() {
        (runtime, "runtime pin is incomplete")
    } else {
        return Err("pin is not configured");
    };
    let path = selected.path.ok_or(incomplete)?;
    if !path.is_absolute() {
        return Err("configured executable path is not absolute");
    }
    let executable_digest = decode_sha256_hex(
        selected.executable_digest.as_deref().ok_or(incomplete)?,
    )
    .ok_or("executable digest is not exact lowercase SHA-256")?;
    let runtime_library_digest = match selected.runtime_library_digest {
        Some(digest) => Some(
            decode_sha256_hex(&digest)
                .ok_or("runtime-library digest is not exact lowercase SHA-256")?,
        ),
        None => None,
    };
    if executable_digest != release.0 || runtime_library_digest != release.1 {
        return Err("configured identity is not the reviewed Binaryen release pin");
    }
    Ok((path, executable_digest, runtime_library_digest))
}

fn bundled_binaryen_path_for_executable(
    executable: &std::path::Path,
) -> std::result::Result<std::path::PathBuf, &'static str> {
    let bin = executable
        .parent()
        .ok_or("installed executable has no parent directory")?;
    if bin.file_name() != Some(std::ffi::OsStr::new("bin")) {
        return Err("installed executable is not in the release bin directory");
    }
    let root = bin
        .parent()
        .ok_or("installed executable has no release root")?;
    Ok(root.join(BUNDLED_BINARYEN_RELATIVE_PATH))
}

fn bundled_binaryen_path() -> std::result::Result<std::path::PathBuf, &'static str> {
    let executable = std::env::current_exe()
        .map_err(|_| "installed executable path is unavailable")?;
    bundled_binaryen_path_for_executable(&executable)
}

fn binaryen_tool() -> std::result::Result<&'static BinaryenTool, &'static str> {
    static TOOL: OnceLock<std::result::Result<BinaryenTool, &'static str>> = OnceLock::new();
    TOOL.get_or_init(|| {
        let compile = ConfiguredBinaryenPin {
            path: option_env!("WITCHY_PINNED_WASM_OPT_PATH")
                .map(std::path::PathBuf::from),
            executable_digest: option_env!("WITCHY_PINNED_WASM_OPT_SHA256")
                .map(str::to_owned),
            runtime_library_digest: option_env!("WITCHY_PINNED_BINARYEN_LIB_SHA256")
                .map(str::to_owned),
        };
        let runtime = ConfiguredBinaryenPin {
            path: std::env::var_os("WITCHY_PINNED_WASM_OPT_PATH").map(Into::into),
            executable_digest: std::env::var("WITCHY_PINNED_WASM_OPT_SHA256").ok(),
            runtime_library_digest: std::env::var("WITCHY_PINNED_BINARYEN_LIB_SHA256").ok(),
        };
        let release = release_binaryen_digests()
            .ok_or("platform has no reviewed Binaryen release pin")?;
        let bundled = option_env!("WITCHY_BUNDLED_BINARYEN") == Some("1");
        let (configured, executable_digest, runtime_library_digest) = if bundled {
            if compile.is_present() {
                return Err("bundled release cannot also embed an absolute Binaryen pin");
            }
            // Runtime pin variables cannot redirect an installed release. The
            // bundle location and reviewed digests are compiled into the
            // artifact, and the executable is resolved relative to witchy.
            (bundled_binaryen_path()?, release.0, release.1)
        } else {
            resolve_configured_binaryen_pin(compile, runtime, release)?
        };
        authenticate_binaryen(
            &configured,
            executable_digest,
            runtime_library_digest,
            &binaryen_staging_root().ok_or("private Binaryen staging is unavailable")?,
        )
        .ok_or("reviewed Binaryen bundle failed authentication or version check")
    })
    .as_ref()
    .map_err(|reason| *reason)
}

fn binaryen_required() -> bool {
    option_env!("WITCHY_REQUIRE_PINNED_BINARYEN") == Some("1")
        || std::env::var_os("WITCHY_REQUIRE_PINNED_BINARYEN")
            .is_some_and(|value| value == std::ffi::OsStr::new("1"))
}

fn resolve_binaryen_result<'a>(
    required: bool,
    resolved: std::result::Result<&'a BinaryenTool, &'static str>,
) -> Result<Option<&'a BinaryenTool>> {
    match resolved {
        Ok(tool) => Ok(Some(tool)),
        Err(reason) if required => Err(Error::msg(format!(
            "required pinned Binaryen unavailable: {reason}"
        ))),
        Err(reason) => {
            if std::env::var_os("WIRDIAG").is_some() {
                eprintln!("WIRBAIL wasm-opt-unavailable: {reason}");
            }
            Ok(None)
        }
    }
}

fn resolve_binaryen_for_build(required: bool) -> Result<Option<&'static BinaryenTool>> {
    if !binaryen_enabled() {
        return Ok(None);
    }
    resolve_binaryen_result(required, binaryen_tool())
}

/// Bind cached optimized wasm to both its original input and its own contents.
/// The envelope detects truncation, corruption, and accidental cache-file swaps
/// before Wasmtime sees the payload. `Module::new` remains the authoritative
/// validator and the memory-safety boundary.
fn encode_optimized_wasm(input_hash: [u8; 32], payload: &[u8]) -> Vec<u8> {
    let payload_len = u64::try_from(payload.len()).expect("a wasm buffer length fits in u64");
    let mut out = Vec::with_capacity(OPT_CACHE_HEADER_LEN + payload.len());
    out.extend_from_slice(OPT_CACHE_MAGIC);
    out.extend_from_slice(&input_hash);
    out.extend_from_slice(&sha256(payload));
    out.extend_from_slice(&payload_len.to_le_bytes());
    out.extend_from_slice(payload);
    debug_assert_eq!(out.len(), OPT_CACHE_HEADER_LEN + payload.len());
    out
}

fn decode_optimized_wasm<'a>(expected_input_hash: &[u8; 32], bytes: &'a [u8]) -> Option<&'a [u8]> {
    let header = bytes.get(..OPT_CACHE_HEADER_LEN)?;
    if header.get(..OPT_CACHE_MAGIC.len())? != OPT_CACHE_MAGIC {
        return None;
    }
    let input_start = OPT_CACHE_MAGIC.len();
    let payload_hash_start = input_start + 32;
    let len_start = payload_hash_start + 32;
    if header.get(input_start..payload_hash_start)? != expected_input_hash {
        return None;
    }
    let expected_payload_hash: [u8; 32] = header.get(payload_hash_start..len_start)?.try_into().ok()?;
    let payload_len = u64::from_le_bytes(header.get(len_start..OPT_CACHE_HEADER_LEN)?.try_into().ok()?);
    let payload_len = usize::try_from(payload_len).ok()?;
    let end = OPT_CACHE_HEADER_LEN.checked_add(payload_len)?;
    if end != bytes.len() {
        return None;
    }
    let payload = bytes.get(OPT_CACHE_HEADER_LEN..end)?;
    (sha256(payload) == expected_payload_hash).then_some(payload)
}

fn optimized_wasm_cache_path(input_hash: &[u8; 32]) -> Option<std::path::PathBuf> {
    let hex: String = input_hash.iter().map(|b| format!("{b:02x}")).collect();
    optimized_wasm_cache_dir().map(|dir| dir.join(format!("{hex}.wasm-cache")))
}

/// (RFC-0034 L1) Run Binaryen `wasm-opt -O4` on the module — a heavier optimizer
/// than Cranelift's Speed tier (GVN, inlining, DCE, local CSE). It runs ONLY on
/// the cold compile path below (successful output is cached as validated wasm),
/// so warm runs skip Binaryen. Optional + graceful: returns the input unchanged
/// if the digest-pinned `wasm-opt` is unavailable or fails, or if the
/// `wasm-opt` lever is off (`WITCHY_OPT=-wasm-opt`). Ambient `PATH` is never
/// consulted. `--all-features` accepts witchy's bulk-memory (`memory.copy`);
/// -O4 never introduces new features, so the output runs under the same
/// wasmtime config.
fn binaryen_enabled() -> bool {
    witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::WasmOpt)
}

fn binaryen_optimize<'a>(
    wasm: &'a [u8],
    tool: Option<&BinaryenTool>,
    required: bool,
) -> Result<std::borrow::Cow<'a, [u8]>> {
    use std::borrow::Cow;
    let fallback = |reason: &str| {
        if required {
            Err(Error::msg(format!("required pinned Binaryen failed: {reason}")))
        } else {
            if std::env::var_os("WIRDIAG").is_some() {
                eprintln!("WIRBAIL wasm-opt-failed: {reason}");
            }
            Ok(Cow::Borrowed(wasm))
        }
    };
    let Some(tool) = tool else {
        return Ok(Cow::Borrowed(wasm));
    };
    if !tool.staged_bytes_are_authentic() {
        return fallback("staged bundle changed after authentication");
    }
    let Ok(dir) = tempfile::Builder::new().prefix("witchy-wasm-opt-").tempdir() else {
        return fallback("private temporary directory is unavailable");
    };
    let inp = dir.path().join("input.wasm");
    let outp = dir.path().join("output.wasm");
    if std::fs::write(&inp, wasm).is_err() {
        return fallback("optimizer input could not be written");
    }
    let output = match std::process::Command::new(&tool.executable)
        .args(BINARYEN_ARGS)
        .arg("-o")
        .arg(&outp)
        .arg(&inp)
        .env_clear()
        .current_dir(dir.path())
        .output()
    {
        Ok(output) => output,
        Err(_) => return fallback("optimizer process could not be started"),
    };
    if !output.status.success() {
        return fallback("optimizer process exited unsuccessfully");
    }
    match std::fs::read(&outp) {
        Ok(bytes) => Ok(Cow::Owned(bytes)),
        Err(_) => fallback("optimizer output could not be read"),
    }
}

fn trace_binaryen_applied(tool: &BinaryenTool, input: &[u8], output: &[u8]) {
    if std::env::var_os("WIRDIAG").is_none() {
        return;
    }
    let bundle_digest: [u8; 32] = tool.identity[..32]
        .try_into()
        .expect("Binaryen identity begins with its bundle SHA-256");
    let library_digest = tool
        .runtime_library
        .as_ref()
        .map_or_else(|| "none".to_string(), |(_, digest)| sha256_hex(digest));
    eprintln!(
        "WIRTRACE binaryen-applied release=version_131 version_stdout_sha256={} recipe_schema={} args={} bundle_sha256={} executable_sha256={} library_sha256={} input_sha256={} output_sha256={}",
        sha256_hex(&sha256(PINNED_BINARYEN_VERSION)),
        String::from_utf8_lossy(BINARYEN_RECIPE_SCHEMA),
        binaryen_recipe_evidence(),
        sha256_hex(&bundle_digest),
        sha256_hex(&tool.executable_digest),
        library_digest,
        sha256_hex(&sha256(input)),
        sha256_hex(&sha256(output)),
    );
}

fn trace_binaryen_input(input: &[u8]) {
    if std::env::var_os("WIRDIAG").is_some() {
        eprintln!(
            "WIRTRACE binaryen-input input_sha256={}",
            sha256_hex(&sha256(input))
        );
    }
}

/// Build a `Module`. On the cacheable (non-preempt) engine, successful Binaryen
/// output is cached as validated wasm; every cold and warm path enters Wasmtime
/// through safe `Module::new`. Wasmtime's configured compilation cache handles
/// native-code reuse internally. `cacheable` is false on the preempt engine,
/// whose differing config must not share artifacts.
fn module_from_binaryen_output(
    engine: &Engine,
    original_wasm: &[u8],
    optimized: &std::borrow::Cow<'_, [u8]>,
    required: bool,
) -> Result<(Module, bool)> {
    match Module::new(engine, optimized.as_ref()) {
        Ok(module) => Ok((module, true)),
        Err(err) if matches!(optimized, std::borrow::Cow::Owned(_)) => {
            if required {
                return Err(Error::msg(format!(
                    "required pinned Binaryen produced invalid Wasm: {err}"
                )));
            }
            if std::env::var_os("WIRDIAG").is_some() {
                eprintln!("WIRBAIL wasm-opt-output-rejected: {err}");
            }
            Ok((Module::new(engine, original_wasm)?, false))
        }
        Err(err) => Err(err),
    }
}

fn build_module_with_binaryen<'a>(
    engine: &Engine,
    opt_wasm: &[u8],
    cacheable: bool,
    required: bool,
    resolve_binaryen: impl FnOnce() -> Result<Option<&'a BinaryenTool>>,
) -> Result<Module> {
    validate_layout_metadata(opt_wasm)?;
    if !cacheable {
        return Module::new(engine, opt_wasm);
    }
    trace_binaryen_input(opt_wasm);
    let binaryen = resolve_binaryen()?;
    let input_hash = binaryen
        .map(|tool| optimized_wasm_input_hash(opt_wasm, &tool.identity))
        .unwrap_or_else(|| sha256(opt_wasm));
    let path = binaryen.and_then(|_| optimized_wasm_cache_path(&input_hash));
    if !required && let Some(path) = &path {
        if let Ok(envelope) = std::fs::read(path) {
            if let Some(cached_wasm) = decode_optimized_wasm(&input_hash, &envelope) {
                if let Ok(module) = Module::new(engine, cached_wasm) {
                    return Ok(module);
                }
            }
        }
    }
    let optimized = binaryen_optimize(opt_wasm, binaryen, required)?;
    let (module, optimized_valid) =
        module_from_binaryen_output(engine, opt_wasm, &optimized, required)?;
    if optimized_valid
        && let (Some(tool), std::borrow::Cow::Owned(optimized_wasm)) = (binaryen, &optimized)
    {
        trace_binaryen_applied(tool, opt_wasm, optimized_wasm);
    }
    if let (Some(path), std::borrow::Cow::Owned(optimized_wasm)) = (&path, &optimized)
        && optimized_valid
    {
        let envelope = encode_optimized_wasm(input_hash, optimized_wasm);
        // Write-then-rename so a reader never sees a partial envelope. The temp
        // name carries a random suffix so concurrent compilers do not share a
        // temporary path; atomic rename publishes the complete validated wasm.
        {
            let mut rnd = [0u8; 8];
            getrandom::fill(&mut rnd).ok();
            let suffix: String = rnd.iter().map(|b| format!("{b:02x}")).collect();
            let tmp = path.with_extension(format!("{suffix}.tmp"));
            if std::fs::write(&tmp, envelope).is_ok() {
                let _ = std::fs::rename(&tmp, path);
            }
        }
    }
    Ok(module)
}

/// Admitted modules declare a 64-bit linear memory addressed by
/// zero-extended 32-bit values (RFC-0179). A memory that never moves keeps
/// its whole reservation plus guard mapped or faulting, so the vendored
/// range-based elision drops every explicit bounds check on those
/// addresses, as for a 32-bit memory.
fn enable_bounded_memory64(config: &mut Config) {
    config.wasm_memory64(true);
    config.memory_may_move(false);
}

fn build_module(engine: &Engine, opt_wasm: &[u8], cacheable: bool) -> Result<Module> {
    let required = binaryen_required();
    build_module_with_binaryen(engine, opt_wasm, cacheable, required, || {
        resolve_binaryen_for_build(required)
    })
}

/// Build exactly the bytes supplied by an admitted canonical artifact.
///
/// This path deliberately does not inspect optimizer policy, consult the
/// Binaryen configuration, read an optimized-module cache, or rewrite bytes.
/// The artifact was already admitted by SafeWIR; Wasmtime's structural
/// validation remains mandatory at this final runtime boundary.
fn build_admitted_module(engine: &Engine, wasm: &[u8]) -> Result<Module> {
    validate_layout_metadata(wasm)?;
    Module::new(engine, wasm)
}

pub type VmId = u32;

/// One named secret backing the `SecretStore` capability: its name, its raw bytes
/// (kept host-side — the guest only ever holds an opaque externref), and whether
/// it is **sealed** (RFC-0060). A sealed secret is still consumable by
/// reference (`crypto.sign`, TLS serving), but `crypto.reveal` on it errors — so
/// key material a program *serves* with can never be read back into guest memory.
/// The default (`sealed == false`) is revealable, preserving existing behavior.
#[derive(Clone, Debug, Default)]
pub struct SecretGrant {
    pub name: String,
    pub bytes: Vec<u8>,
    pub sealed: bool,
}

impl SecretGrant {
    /// A revealable named secret (the default grant shape).
    pub fn new(name: impl Into<String>, bytes: Vec<u8>) -> Self {
        Self { name: name.into(), bytes, sealed: false }
    }
}

/// The set of capabilities granted to a VM at spawn time. Each `true` flag
/// causes the corresponding host function to be linked into the VM.
/// Everything defaults to denied.
#[derive(Clone, Debug, Default)]
pub struct Capabilities {
    /// May write to the host's stdout via `witchy.print`.
    pub print: bool,
    /// May read lines from the host through `Console[Read]`.
    pub console_read: bool,
    /// Deterministic input fixture. `None` uses process stdin.
    pub console_input: Option<Vec<String>>,
    /// May print an integer via `witchy.print_int` (used by compiled witchy).
    pub print_int: bool,
    /// Capture output without echoing it to stdout (used by tests,
    /// which compares the captured lines rather than showing them).
    pub quiet: bool,
    /// Bound captured Console output before allocating host-side text. Used by
    /// compiler-owned evaluation; ordinary runs retain their existing limit.
    pub capture_output_limit_bytes: Option<usize>,
    /// Stream Console output live to stdout as each line is printed, cleanly
    /// (no `[vm N]` debug prefix) and flushed immediately. This is for
    /// long-running servers whose `main` never returns: the
    /// default path buffers output and drains it only after the run completes,
    /// so a server's log/readiness lines would never appear. Independent of
    /// `quiet` (which governs the debug echo); when `live_output` is set the
    /// clean stream replaces that echo.
    pub live_output: bool,
    /// May read the wall clock via `witchy.now` (a `Clock` capability).
    pub clock: bool,
    /// May draw cryptographic randomness via `witchy.rand_u64` (a `Rand` capability).
    pub rand: bool,
    /// May read process environment variables via `witchy.env_*` (an `Env`
    /// capability).
    pub env: bool,
    /// Optional per-key restriction for ordinary `Env` host reads. `None`
    /// preserves the coarse grant; `Some(keys)` refuses every unnamed key.
    pub env_allow: Option<Vec<String>>,
    /// The directory subtree backing the first root `Dir` grant.
    /// `None` denies the filesystem entirely; the `dir_read`/`dir_write` flags
    /// pick which operation families are linked within it.
    pub dir_root: Option<std::path::PathBuf>,
    /// Host roots admitted before outer confinement activation. Rights are
    /// aligned before `dir_root` and `dir_roots`; these handles are never
    /// reopened through their diagnostic paths.
    pub preopened_dirs: Vec<crate::confine::ConfinedDir>,
    /// Additional root `Dir` grants, for a `main` that takes several `Dir`
    /// parameters. The generated wrapper mints each parameter from its grant
    /// ordinal; the ordinal never becomes the guest's `Dir` value.
    pub dir_roots: Vec<std::path::PathBuf>,
    /// Per-Dir-grant rights, aligned with `preopened_dirs`, then `dir_root`,
    /// then `dir_roots`. Empty means "use the coarse `dir_read`/`dir_write`
    /// grant for each Dir grant", preserving older callers that grant one
    /// uniform root.
    pub dir_rights: Vec<FsRights>,
    /// Direct `File` grants (RFC-0012): the i-th `File` parameter of `main` maps to
    /// the i-th path here. Backed by `--file` or `[files]`.
    pub file_grants: Vec<std::path::PathBuf>,
    /// Direct File grants admitted before outer confinement activation. Rights
    /// are aligned before `file_grants`; retained parent handles remain the
    /// authority after launch-path replacement.
    pub preopened_files: Vec<crate::confine::ConfinedFile>,
    /// Per-File-grant rights, aligned with `preopened_files`, then
    /// `file_grants`. Empty means full read and write, which preserves the
    /// historical precompiled-wasm `--file` behavior until that launch surface
    /// grows explicit rights syntax.
    pub file_rights: Vec<FsRights>,
    /// May read within `dir_root` (read/exists/is_dir/list/subdir).
    pub dir_read: bool,
    /// May write within `dir_root` (write/make_dir).
    pub dir_write: bool,
    /// May spawn a native subprocess via `exec` (an `Exec` capability). The
    /// executable is named + confined through a `Dir[Read]` capability, so `exec`
    /// without filesystem read is useless — but the flag is tracked separately so
    /// `Exec` appears as its own authority. See rfcs/0004-self-hosted-cli.md.
    pub exec: bool,
    /// Optional per-tool restriction for native subprocess execution. `None`
    /// preserves the ordinary coarse `Exec` grant; `Some(tools)` is used by
    /// BuildExec and refuses every tool not named here.
    pub exec_allow: Option<Vec<String>>,
    /// Host-declared read-only paths needed by native children. These widen only
    /// the inherited outer fence; they are never minted as guest Dir/File values.
    pub exec_child_paths: Vec<std::path::PathBuf>,
    /// BuildExec names paired with the exact ambient paths selected by the
    /// build driver before confinement. Guest-provided names never reach an
    /// ambient PATH lookup.
    pub build_exec_tools: Vec<(String, std::path::PathBuf)>,
    /// Read/execute-only platform roots needed by dynamically linked BuildExec
    /// children. These are launcher-derived child needs, not guest grants.
    pub build_exec_runtime_roots: Vec<std::path::PathBuf>,
    /// The `host:port` allowlist backing the root `Net` capability.
    /// `None` denies the network entirely; the verb flags below pick which
    /// operation families are linked within it.
    pub net_allow: Option<Vec<String>>,
    /// Additional root `Net` grants, aligned after `net_allow`, for entrypoints
    /// with several independently bound network parameters.
    pub net_grants: Vec<Vec<String>>,
    /// Root Fetch grants, one origin allowlist per `Fetch` parameter in `main`.
    pub fetch_grants: Vec<Vec<String>>,
    /// The `host:port` allowlist backing a build step's `BuildNet` capability.
    /// Kept separate from runtime `Net` so precompiled modules granted network
    /// authority cannot import the build-only `fetch_build` primitive.
    pub build_net_allow: Option<Vec<String>>,
    /// May dial out (`connect`/`restrict`) to allowlisted addresses.
    pub net_connect: bool,
    /// May bind and accept (`listen`/`accept`) on allowlisted addresses.
    pub net_listen: bool,
    /// The program's command-line arguments (`main(args: List(String))`).
    /// Pure input chosen by the host, not authority.
    pub args: Vec<String>,
    /// RFC-0038: `[user_caps]` grant field values for a `main` binding bare
    /// grantable capabilities — outer index is the grantable-cap parameter
    /// (declaration order), inner is its policy fields in order. Empty otherwise.
    pub user_cap_fields: Vec<Vec<String>>,
    /// The Ed25519 seed backing the root `Secret` capability. The key
    /// material stays host-side; the guest only ever sees signatures and the
    /// public key.
    pub signing_key: Option<[u8; 32]>,
    /// Named secrets backing the `SecretStore` capability. The guest never sees an
    /// index into this vector: `secretstore_lookup` returns an opaque `Secret`
    /// externref, and the root signing-key `Secret` is minted by `mint_secret`.
    /// The `signing` entry (from `--signing-key`) is kept here so
    /// `SecretStore.require("signing")` and bare `Secret` share one identity.
    pub secrets: Vec<SecretGrant>,
    /// The confined output directory backing a build step's `BuildOut`
    /// capability — where `write_out` may write generated source, and nowhere
    /// else. `None` denies build-time writes entirely.
    pub build_out: Option<std::path::PathBuf>,
    /// The confined read roots backing a build step's `BuildRead` capability —
    /// `read_build` resolves a path against the first root that holds it. Empty
    /// denies build-time reads.
    pub build_read_roots: Vec<std::path::PathBuf>,
    /// Immutable snapshot backing `BuildEnv`. Map membership is the allow-list;
    /// `None` means the named variable was allowed but unset. Keeping the values
    /// in the grant makes the size/fill protocol deterministic and avoids mutable
    /// process-global environment access while a VM is running.
    pub build_env: Option<std::collections::BTreeMap<String, Option<String>>>,
}

impl Capabilities {
    /// No authority at all.
    pub fn none() -> Self {
        Self::default()
    }

    /// Derive the target-neutral outer-confinement policy from the exact
    /// concrete grants admitted for this VM. Platform providers consume this
    /// object; none may independently reinterpret `Capabilities`.
    pub fn confinement_policy(&self) -> witchy_confinement::Policy {
        use witchy_confinement::{
            FsAccess, FsScope, Policy, SyscallClass,
        };

        let mut policy = Policy::default();
        let coarse_dir_rights = FsRights::new(self.dir_read, self.dir_write);
        for (index, root) in self
            .preopened_dirs
            .iter()
            .map(crate::confine::ConfinedDir::display_path)
            .chain(self.dir_root.iter().map(std::path::PathBuf::as_path))
            .chain(self.dir_roots.iter().map(std::path::PathBuf::as_path))
            .enumerate()
        {
            let rights = self
                .dir_rights
                .get(index)
                .copied()
                .unwrap_or(coarse_dir_rights);
            policy.add_fs_rule(
                root,
                FsScope::Tree,
                FsAccess::new(rights.read, rights.write, self.exec && rights.read),
            );
        }
        for (index, path) in self
            .preopened_files
            .iter()
            .map(crate::confine::ConfinedFile::display_path)
            .chain(self.file_grants.iter().map(std::path::PathBuf::as_path))
            .enumerate()
        {
            let rights = self
                .file_rights
                .get(index)
                .copied()
                .unwrap_or_else(FsRights::full);
            policy.add_fs_rule(
                path,
                FsScope::File,
                FsAccess::new(rights.read, rights.write, false),
            );
        }
        if let Some(path) = &self.build_out {
            // The output tree needs READ as well as write: writing into it
            // requires opening/traversing the directory, and the build host
            // lists it afterward (`std::fs::read_dir` in build_steps). A
            // write-only Landlock rule denies that open with "reading output
            // dir: Permission denied" once confinement is enforced (Linux).
            policy.add_fs_rule(
                path,
                FsScope::Tree,
                FsAccess::new(true, true, false),
            );
        }
        for path in &self.build_read_roots {
            policy.add_fs_rule(
                path,
                FsScope::Tree,
                FsAccess::new(true, false, false),
            );
        }
        for (_, path) in &self.build_exec_tools {
            policy.add_fs_rule(
                path,
                FsScope::File,
                FsAccess::new(true, false, true),
            );
        }
        for path in &self.build_exec_runtime_roots {
            policy.add_fs_rule(
                path,
                FsScope::Tree,
                FsAccess::new(true, false, true),
            );
        }
        for path in &self.exec_child_paths {
            policy.add_fs_rule(
                path,
                FsScope::Path,
                FsAccess::new(true, false, false),
            );
        }

        if self.net_connect {
            policy.network.connect_requested = true;
            for target in self
                .net_allow
                .iter()
                .flatten()
                .chain(self.net_grants.iter().flatten())
            {
                policy.add_connect_target(target);
            }
        }
        if self.net_listen {
            policy.network.bind_requested = true;
            for target in self
                .net_allow
                .iter()
                .flatten()
                .chain(self.net_grants.iter().flatten())
            {
                policy.add_bind_target(target);
            }
        }
        for origin in self.fetch_grants.iter().flatten() {
            policy.add_fetch_origin(origin);
        }
        if let Some(targets) = &self.build_net_allow
            && !targets.is_empty()
        {
            policy.network.connect_requested = true;
            for target in targets {
                policy.add_connect_target(target);
            }
        }
        if self.exec
            || self
                .exec_allow
                .as_ref()
                .is_some_and(|tools| !tools.is_empty())
        {
            policy.syscall_classes.insert(SyscallClass::Process);
        }
        policy.normalize_classes();
        policy
    }
}

/// Resolve host-authored child-needs without canonicalizing and reopening them.
/// The provider opens each resulting pathname when it installs the rule.
pub fn resolve_exec_child_paths(
    paths: &[String],
    base: &std::path::Path,
) -> Result<Vec<std::path::PathBuf>, String> {
    paths
        .iter()
        .map(|declared| {
            if declared.is_empty() {
                return Err("Exec child path must not be empty".to_string());
            }
            let configured = if declared == "~" || declared.starts_with("~/") {
                let home = std::env::var_os("HOME")
                    .ok_or_else(|| format!("Exec child path `{declared}` needs $HOME, but it is unset"))?;
                std::path::PathBuf::from(home)
                    .join(declared.strip_prefix("~/").unwrap_or(""))
            } else {
                std::path::PathBuf::from(declared)
            };
            let path = if configured.is_absolute() {
                configured
            } else {
                base.join(configured)
            };
            std::fs::metadata(&path).map_err(|error| {
                format!(
                    "cannot resolve Exec child path `{}`: {error}",
                    path.display()
                )
            })?;
            Ok(path)
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FsRights {
    pub read: bool,
    pub write: bool,
}

impl FsRights {
    pub const fn new(read: bool, write: bool) -> Self {
        Self { read, write }
    }

    const fn full() -> Self {
        Self { read: true, write: true }
    }
}

#[derive(Clone, Debug)]
enum DirBacking {
    Fs(crate::confine::ConfinedDir),
}

#[derive(Clone, Debug)]
struct DirAuthority {
    backing: DirBacking,
    policy: String,
    rights: FsRights,
}

#[derive(Clone, Debug)]
enum FileBacking {
    Fs(crate::confine::ConfinedFile),
}

#[derive(Clone, Debug)]
struct FileAuthority {
    backing: FileBacking,
    rights: FsRights,
}

use crate::net::Stream;

#[derive(Clone, Debug)]
struct NetAuthority {
    allow: Vec<String>,
    // Preserved by attenuation; Fetch derivation authenticates the actual VM.
    issuer: Arc<()>,
}

#[derive(Clone, Debug)]
struct EnvAuthority {
    allow: Option<std::collections::BTreeSet<String>>,
}

#[derive(Clone, Debug)]
struct ExecAuthority {
    allow: Option<std::collections::BTreeSet<String>>,
}

#[derive(Clone)]
struct SocketResource {
    stream: Arc<Mutex<std::io::BufReader<Box<dyn Stream>>>>,
}

#[derive(Clone)]
struct ListenerResource {
    listener: std::sync::Arc<std::net::TcpListener>,
    tls: Option<crate::net::ServerTlsConfig>,
}

#[derive(Clone, Debug)]
struct SecretMaterial {
    bytes: Vec<u8>,
    sealed: bool,
}

impl SecretMaterial {
    fn from_grant(grant: &SecretGrant) -> Self {
        Self {
            bytes: grant.bytes.clone(),
            sealed: grant.sealed,
        }
    }
}

/// Host-side state owned by a spawned VM's `Store`: its capability grant, output
/// buffer, and the host-side root-capability/build grant material. One state type
/// means one set of capability host functions.
const HEAP_POISON: u8 = 0xDB;

/// Keep executable bytes and their sealed admission product inseparable when
/// cloning a program into a fresh worker. Raw Wasm metadata is not admission.
#[derive(Clone)]
enum WorkerModule {
    Unadmitted(Module),
    Admitted(Arc<AdmittedWorkerModule>),
}

struct AdmittedWorkerModule {
    module: Module,
    artifact: AdmittedCanonicalArtifact,
}

impl WorkerModule {
    fn from_admitted(engine: &Engine, artifact: &AdmittedCanonicalArtifact) -> Result<Self> {
        Ok(Self::Admitted(Arc::new(AdmittedWorkerModule {
            module: build_admitted_module(engine, artifact.wasm())?,
            artifact: artifact.clone(),
        })))
    }

    fn module(&self) -> &Module {
        match self { Self::Unadmitted(module) => module, Self::Admitted(program) => &program.module }
    }

    fn admitted(&self) -> Result<&Arc<AdmittedWorkerModule>> {
        match self {
            Self::Admitted(program) if program.artifact.entry().is_some() => Ok(program),
            _ => bail!("server worker requires an admitted program with canonical entry metadata"),
        }
    }
}

pub struct VmState {
    owner_bridge_counts: Option<std::collections::BTreeMap<&'static str, u64>>,
    compiler_syntax: Option<comptime::SessionState>,
    id: VmId,
    caps: Capabilities,
    /// The compiler services exposed to trusted programs; injected from above
    /// so `runtime/compiler.rs` depends on the interface, not the
    /// implementation.
    compiler_services: Arc<dyn compiler::CompilerServices>,
    pub(crate) limits: StoreLimits,
    /// Everything the VM has printed (via the `print`/`print_int`
    /// capabilities), so the host can observe a compiled program's output.
    pub(crate) output: Arc<Mutex<Vec<String>>>,
    pub(crate) captured_output_bytes: usize,
    /// Remaining Console input fixture; `None` delegates to process stdin.
    pub(crate) console_input: Option<std::collections::VecDeque<String>>,
    /// Root `Dir` grants awaiting wrapper-local `mint_dir(i)`. A live guest `Dir`
    /// is an externref carrying a cloned `DirAuthority`, not this index.
    dirs: Vec<DirAuthority>,
    /// Direct `File` grants awaiting wrapper-local `mint_file(i)`. A live guest
    /// `File` is an externref carrying a cloned `FileAuthority`, not this index.
    files: Vec<FileAuthority>,
    /// A host->guest transfer staged by a size-probing call (`dir_read_len`,
    /// `net_recv_*_len`, ...) and consumed by the matching `fill_pending`, so
    /// the data is read once with no time-of-check/time-of-use gap.
    pub(crate) pending: Option<Vec<u8>>,
    /// A staged directory listing (`dir_list_size` -> `dir_list_write`).
    pub(crate) pending_list: Option<Vec<String>>,
    /// (RFC-0032) Staged scalar results of `vm.par_map` (`vm_par_map_run` computes
    /// them — in parallel on worker VMs — then `vm_par_map_write` lays out the
    /// `List(Int)` `[count][i64..]` into the guest's reserved block).
    pub(crate) pending_ints: Option<Vec<i64>>,
    /// One admitted scalar worker result, sealed to its source site and
    /// consumed exactly once by the matching checked write call.
    pub(in crate::runtime) pending_admitted_par_map: Option<host::vm_worker::PendingAdmittedParMap>,
    /// Successful fresh admitted worker launches with an empty grant set.
    pub(in crate::runtime) admitted_zero_grant_worker_launches: usize,
    /// (RFC-0032) Staged raw-byte results of a `Bytes` `vm.par_map` — kept as raw
    /// `Vec<u8>` (NOT `String`) so arbitrary binary survives the round-trip.
    pub(crate) pending_bytes: Option<Vec<Vec<u8>>>,
    /// RFC-0038: `[user_caps]` grant field values for a `main` binding bare
    /// grantable capabilities — outer index = grantable-cap parameter (declaration
    /// order), inner = its policy fields in order. Materialized into the guest by
    /// `user_cap_field_len` + `fill_pending` and wrapped in a record by codegen.
    pub(crate) user_cap_fields: Vec<Vec<String>>,
    /// Root `Net` grants awaiting wrapper-local `mint_net(i)`. Live `Net`,
    /// `Socket`, and `Listener` values are externrefs carrying host-side resources,
    /// not guest-forgeable indices into these grants.
    nets: Vec<NetAuthority>,
    /// Declarative Fetch grants awaiting wrapper-local `mint_fetch(i)`.
    fetch_grants: Vec<Vec<String>>,
    /// Fresh per Store, shared by its Net-derived/direct Fetch roots and results.
    fetch_issuer: Arc<()>,
    /// Canonical Clock, SecretStore and Secret tokens belong to this instance.
    registry_issuer: Arc<()>,
    /// (RFC-0032) When this VM is a `serve` POOL WORKER, the shared listener (plus
    /// its TLS config) it must reuse instead of binding its own. `listen` returns
    /// this; `serve_pool` sees it set and does NOT spawn another pool (only the
    /// primary spawns workers).
    worker_listener: Option<(std::sync::Arc<std::net::TcpListener>, Option<crate::net::ServerTlsConfig>)>,
    /// A build step's confined output directory (`BuildOut`) and read roots
    /// (`BuildRead`) — host-side. Canonical entry tokens are instance-bound;
    /// legacy build imports retain their separate grant-gated registration.
    build_out: Option<crate::confine::ConfinedDir>,
    build_out_issuer: Arc<()>,
    build_read_roots: Vec<crate::confine::ConfinedDir>,
    build_read_issuer: Arc<()>,
    build_env_issuer: Arc<()>,
    build_exec_issuer: Arc<()>,
    build_net_issuer: Arc<()>,
    build_exec_tools:
        std::collections::BTreeMap<String, crate::confine::ConfinedFile>,
    /// (RFC-0023) Checked-heap shadow. Each `heap_register(start,end)` the guest's
    /// checked allocators emit records an object's `[start,end)`; the host poisons
    /// `[end, end+HEAP_REDZONE)` and the post-run sweep traps if any poison byte was
    /// overwritten. Exact object retirement removes entries when UAF mode quarantines
    /// and poisons a complete allocation. Empty unless checked codegen ran.
    pub(crate) heap_objects: Vec<(u32, u32)>,
    /// (`Rand`) splitmix64 state for the seeded test/parity path (`WITCHY_RAND_SEED`);
    /// `None` means `rand_u64` draws from the OS CSPRNG instead.
    rand_state: Option<u64>,
    /// (RFC-0032) The engine + compiled module + preemption flag, stashed so a host
    /// op (`vm.par_map`) can instantiate fresh worker VMs on OS threads from inside a
    /// call. `Engine`/`Module` are `Arc`-backed (cheap clone, `Send`+`Sync`).
    engine: Engine,
    module: WorkerModule,
    preempt: bool,
    server_startup: Option<host::vm_worker::ServerWorkerStartup>,
    vm_services: Arc<Mutex<VmServiceMetrics>>,
    #[cfg(feature = "test-fixtures")]
    pub(crate) fixture_host: Option<witchy_test_host::FixtureHost>,
    #[cfg(feature = "test-fixtures")]
    fixture_console: Option<Arc<host::canonical_fixture::Session>>,
}

#[cfg(feature = "test-fixtures")]
#[derive(Debug)]
pub enum FixtureWasmResult {
    Passed,
    Failed { error: String },
}

#[cfg(feature = "test-fixtures")]
#[derive(Debug)]
pub struct FixtureWasmOutcome {
    /// Measured only after normal completion, never inferred after an abort.
    pub leaked_objects: Option<usize>,
    pub result: FixtureWasmResult,
    pub output: Vec<String>,
    pub transcript: witchy_testkit::TestTranscript,
}

/// A spawned VM plus the entrypoint we can drive.
pub struct Vm {
    store: Store<VmState>,
    instance: wasmtime::Instance,
    aborted: bool,
    /// Set for a VM spawned from an admitted canonical artifact: the validated
    /// entry the launcher calls, with host-minted capability tokens. `None`
    /// is the frozen compiler's parameterless `run` export protocol.
    entry: Option<CanonicalEntry>,
}

/// Host-observed isolated service lifecycle. Retiring the last state root by
/// destroying a child Store is not a claim that its guest heap had zero leaks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VmServiceMetrics {
    pub started: u64,
    pub successful_steps: u64,
    pub closed: u64,
    pub failed: u64,
    pub live_workers: u64,
    pub live_state_roots: u64,
    pub retired_state_roots: u64,
    pub isolated_memories: u64,
}

/// The opaque host value behind a `Console` capability token minted at launch
/// for a canonical entry. The `witchy.print` import is itself gated by the
/// `print` grant, so the token carries identity, not extra authority; it
/// exists so the guest's `Cap<Console>` is an unforgeable host reference and
/// never an integer or a null.
struct ConsoleCapability;

impl Vm {
    /// Actual admitted worker VM launches through the zero-grant scalar map path.
    pub fn admitted_zero_grant_worker_launches(&self) -> usize {
        self.store.data().admitted_zero_grant_worker_launches
    }
    /// Read-only evidence from actual host service invocations, not source
    /// output or a guest-selected counter.
    pub fn vm_service_metrics(&self) -> VmServiceMetrics {
        self.store.data().vm_services.lock().expect("service metrics lock").clone()
    }

    /// Run the program to completion.
    ///
    /// A frozen-compiler module is entered through its parameterless `run`
    /// export. An admitted canonical artifact is entered through the validated
    /// entry recorded at admission: the host mints one capability token per
    /// declared capability parameter and prints an `Int`/`Float` result under
    /// the same exit-code convention the frozen `run` wrapper implemented.
    pub fn run(&mut self) -> Result<()> {
        if self.aborted {
            bail!("an aborted Witchy VM cannot be run again");
        }
        let outcome = match self.entry.clone() {
            Some(entry) => self.call_canonical_entry(&entry),
            None => {
                let run = self
                    .instance
                    .get_typed_func::<(), ()>(&mut self.store, "run")?;
                run.call(&mut self.store, ())
            }
        };
        if let Err(error) = outcome {
            // A trap does not run guest cleanup. Make that terminal: retaining the
            // host handle is harmless, but no later call may observe a partially
            // unwound stack or abandoned guest roots.
            self.aborted = true;
            return Err(self.contextualize_run_error(error));
        }
        if let Err(error) = self.heap_sweep() {
            self.aborted = true;
            return Err(error);
        }
        if let Some(counts) = &self.store.data().owner_bridge_counts {
            for (recipe, calls) in counts { eprintln!("owner_bridge {recipe} calls={calls}"); }
        }
        if std::env::var_os("WITCHY_REGION_STATS").is_some_and(|v| v == "1") {
            if let Some(bytes) = self.region_copy_bytes() {
                eprintln!("region copy-out: {bytes} byte(s)");
            }
        }
        Ok(())
    }

    /// The host reference for one entry capability: the per-kind host adapter
    /// minting the `ordinal`-th grant of `kind`. Admission only admits root
    /// kinds; any other answers an error rather than a token.
    fn mint_entry_capability(&mut self, kind: CapabilityKind, ordinal: usize) -> Result<wasmtime::Rooted<ExternRef>> {
        let missing = || Error::msg(format!(
            "the program's `main` requires a {kind:?} capability for parameter {ordinal} that was not granted"
        ));
        let data = self.store.data();
        match kind {
            CapabilityKind::Console => {
                #[cfg(feature = "test-fixtures")]
                if data.fixture_console.is_some() {
                    return host::canonical_fixture::mint(&mut self.store);
                }
                if !data.caps.print && !data.caps.console_read { return Err(missing()); }
                ExternRef::new(&mut self.store, ConsoleCapability)
            }
            CapabilityKind::Clock => {
                let authority = host::clock_env_rand::mint_clock(data)?;
                ExternRef::new(&mut self.store, authority)
            }
            CapabilityKind::Env => {
                if !data.caps.env { return Err(missing()); }
                let authority = host::clock_env_rand::env_authority(data);
                ExternRef::new(&mut self.store, authority)
            }
            CapabilityKind::SecretStore => {
                let authority = host::secret::mint_store(data);
                ExternRef::new(&mut self.store, authority)
            }
            CapabilityKind::Secret => {
                let authority = host::secret::mint_canonical_secret(data)?;
                ExternRef::new(&mut self.store, authority)
            }
            CapabilityKind::Dir => {
                let authority = host::filesystem::dir_grant(data, ordinal).ok_or_else(missing)?;
                ExternRef::new(&mut self.store, authority)
            }
            CapabilityKind::File => {
                let authority = data.files.get(ordinal).cloned().ok_or_else(missing)?;
                ExternRef::new(&mut self.store, authority)
            }
            CapabilityKind::Net => {
                let authority = data.nets.get(ordinal).cloned().ok_or_else(missing)?;
                ExternRef::new(&mut self.store, authority)
            }
            CapabilityKind::Fetch => {
                let authority = host::fetch::mint(data, ordinal)?;
                ExternRef::new(&mut self.store, authority)
            }
            CapabilityKind::Exec => {
                let authority = host::filesystem::exec_grant(data)?;
                ExternRef::new(&mut self.store, authority)
            }
            CapabilityKind::BuildOut => {
                let authority = host::build_out::mint(data)?;
                ExternRef::new(&mut self.store, authority)
            }
            CapabilityKind::BuildRead => {
                let authority = host::build_read::mint(data)?;
                ExternRef::new(&mut self.store, authority)
            }
            CapabilityKind::BuildEnv => {
                let authority = host::build_env::mint(data)?;
                ExternRef::new(&mut self.store, authority)
            }
            CapabilityKind::BuildExec => {
                let authority = host::build_exec::mint(data)?;
                ExternRef::new(&mut self.store, authority)
            }
            CapabilityKind::BuildNet => {
                let authority = host::build_net::mint(data)?;
                ExternRef::new(&mut self.store, authority)
            }
            CapabilityKind::Rand | CapabilityKind::Socket | CapabilityKind::Listener => {
                bail!("{kind:?} is not a launch-supplied entry capability")
            }
        }
    }

    fn call_canonical_entry(&mut self, entry: &CanonicalEntry) -> Result<()> {
        let func = self
            .instance
            .get_func(&mut self.store, entry.export())
            .ok_or_else(|| Error::msg(format!("admitted artifact lacks its entry export `{}`", entry.export())))?;
        let mut params = Vec::with_capacity(entry.capabilities().len());
        // Each parameter is minted from its grant ordinal among THIS entry's
        // parameters of the same kind, so the i-th `Dir` in `main`'s signature
        // is backed by the i-th `Dir` grant (and likewise `File`, `Net`, `Fetch`).
        let mut ordinals = std::collections::BTreeMap::<CapabilityKind, usize>::new();
        for &kind in entry.capabilities() {
            let ordinal = ordinals.entry(kind).or_default();
            let token = self.mint_entry_capability(kind, *ordinal)?;
            *ordinal += 1;
            params.push(Val::ExternRef(Some(token)));
        }
        // Capabilities are all checked before writing launch-owned memory.
        // Insert data arguments in checked ABI order, never as authority.
        for position in entry.argument_positions() {
            let pointer = self.materialize_entry_arguments()?;
            params.insert(usize::from(*position), Val::I32(pointer as i32));
        }
        let mut results = vec![
            Val::I32(0);
            match entry.result() {
                CanonicalEntryResult::Unit => 0,
                CanonicalEntryResult::Int | CanonicalEntryResult::Float => 1,
            }
        ];
        func.call(&mut self.store, &params, &mut results)?;
        let rendered = match (entry.result(), results.first()) {
            (CanonicalEntryResult::Int, Some(Val::I64(n))) => Some(n.to_string()),
            (CanonicalEntryResult::Float, Some(Val::F64(bits))) => {
                Some(witchy_syntax::fmt::render_float(f64::from_bits(*bits)))
            }
            (CanonicalEntryResult::Unit, _) => None,
            (kind, other) => bail!("entry result {other:?} does not match its validated kind {kind:?}"),
        };
        if let Some(text) = rendered {
            // The frozen `run` wrapper printed `main`'s Int/Float result through
            // the `print_int`/`print_float` imports; the host does the same
            // rendering here so both routes agree on what a run shows.
            let state = self.store.data();
            if state.caps.live_output {
                use std::io::Write;
                println!("{text}");
                let _ = std::io::stdout().flush();
            } else if !state.caps.quiet {
                println!("[vm {}] {text}", state.id);
            }
            state.output.lock().unwrap().push(text);
        }
        Ok(())
    }

    /// Invoke a pure `pub fn export_*(Bytes) -> Bytes` wrapper.
    ///
    /// The host copies one length-prefixed input frame into guest memory and
    /// copies one bounded output frame back. The VM receives no authority
    /// beyond the capabilities supplied at spawn.
    pub fn invoke_bytes(
        &mut self,
        export: &str,
        input: &[u8],
        max_output_bytes: usize,
    ) -> Result<Vec<u8>> {
        if self.aborted {
            bail!("an aborted Witchy VM cannot be invoked again");
        }
        let input_len = i32::try_from(input.len())
            .map_err(|_| Error::msg("Witchy byte-export input exceeds i32::MAX bytes"))?;
        let allocation_len = input_len
            .checked_add(4)
            .ok_or_else(|| Error::msg("Witchy byte-export input length overflow"))?;
        let galloc = self
            .instance
            .get_typed_func::<i32, i32>(&mut self.store, "__galloc")?;
        let call = self
            .instance
            .get_typed_func::<(i32, i32), i32>(&mut self.store, export)?;
        let memory = self
            .instance
            .get_memory(&mut self.store, "memory")
            .ok_or_else(|| Error::msg("Witchy byte-export module has no `memory` export"))?;
        let input_ptr = match galloc.call(&mut self.store, allocation_len) {
            Ok(ptr) => ptr,
            Err(error) => {
                self.aborted = true;
                return Err(self.contextualize_run_error(error));
            }
        };
        if let Err(error) = memory.write(
            &mut self.store,
            input_ptr as usize,
            &input_len.to_le_bytes(),
        ) {
            self.aborted = true;
            return Err(Error::msg(format!("writing Witchy byte-export input length: {error}")));
        }
        if let Err(error) = memory.write(&mut self.store, input_ptr as usize + 4, input) {
            self.aborted = true;
            return Err(Error::msg(format!("writing Witchy byte-export input: {error}")));
        }
        let output_ptr = match call.call(&mut self.store, (input_ptr, input_len)) {
            Ok(ptr) => ptr,
            Err(error) => {
                self.aborted = true;
                return Err(self.contextualize_run_error(error));
            }
        };
        let output = match read_wbytes_bounded(
            memory.data(&self.store),
            output_ptr,
            max_output_bytes,
        ) {
            Ok(output) => output,
            Err(error) => {
                self.aborted = true;
                return Err(error);
            }
        };
        if let Err(error) = self.heap_sweep() {
            self.aborted = true;
            return Err(error);
        }
        Ok(output)
    }

    /// Attach the source site published immediately before a host-backed
    /// operation. Guest-routed aborts already carry their complete diagnostic;
    /// capability host failures arrive as a bare core and are completed here.
    fn contextualize_run_error(&mut self, error: Error) -> Error {
        let core = error.root_cause().to_string();
        if core.starts_with("runtime error: ") {
            return error;
        }
        let site = self
            .instance
            .get_global(&mut self.store, "__witchy_diagnostic_site")
            .and_then(|global| global.get(&mut self.store).i64())
            .unwrap_or(0);
        if site == 0 {
            return error;
        }
        let (func_ptr, line) = witchy_syntax::diag::unpack_site(site);
        let func = if func_ptr == 0 {
            String::new()
        } else {
            self.instance
                .get_memory(&mut self.store, "memory")
                .and_then(|memory| read_wstr(memory.data(&self.store), func_ptr as i32).ok())
                .unwrap_or_default()
        };
        Error::msg(witchy_syntax::diag::runtime_error(&func, line, &core))
    }

    /// (RFC-0023) After the run, prove every checked allocation's trailing redzone is
    /// intact. A flipped poison byte means a write ran past the object's end — an
    /// out-of-object overrun that is in-bounds for the linear memory, so wasmtime never
    /// saw it. A no-op (and free) when nothing was registered, i.e. on uninstrumented
    /// builds. Drains the shadow so a reused VM starts clean.
    /// Audit an admitted canonical module's heap after its entry returned:
    /// every reference-counted object between the heap base and the bump
    /// pointer must be on the reclaimed free list. Under callee-drops a
    /// surviving object is a reclamation defect in the program's lowering,
    /// never a legitimate state. `None` when the module is not canonical.
    pub fn canonical_heap_leaks(&mut self) -> Result<Option<usize>> {
        let mut read_global = |name: &str| -> Option<u32> {
            let global = self.instance.get_global(&mut self.store, name)?;
            global.get(&mut self.store).i32().map(|value| value as u32)
        };
        let (Some(top), Some(free), Some(base)) = (
            read_global(witchy_wir::HEAP_TOP_EXPORT),
            read_global(witchy_wir::HEAP_FREE_EXPORT),
            read_global(witchy_wir::HEAP_BASE_EXPORT),
        ) else {
            return Ok(None);
        };
        let Some(memory) = self.instance.get_memory(&mut self.store, "memory") else {
            return Ok(None);
        };
        let data = memory.data(&self.store);
        let word = |at: u32| -> Result<u32> {
            let at = at as usize;
            data.get(at..at + 4)
                .map(|bytes| u32::from_le_bytes(bytes.try_into().expect("four bytes")))
                .ok_or_else(|| Error::msg(format!("HEAP CHECK: heap word {at:#x} is outside linear memory")))
        };
        // Reclaim links an object into the free list through its first word.
        let mut freed = std::collections::BTreeSet::new();
        let mut cursor = free;
        while cursor != 0 {
            if !freed.insert(cursor) {
                return Err(Error::msg("HEAP CHECK: the reclaimed free list is cyclic"));
            }
            cursor = word(cursor)?;
        }
        // Objects are laid out back to back: [rc][size][size bytes].
        let mut leaks = 0usize;
        let mut cursor = base;
        while cursor.saturating_add(8) <= top {
            let size = word(cursor + 4)?;
            let object = cursor + 8;
            if !freed.contains(&object) {
                leaks += 1;
            }
            cursor = object.saturating_add(size);
            if size == 0 {
                return Err(Error::msg(format!("HEAP CHECK: object at {object:#x} has a zero-length header")));
            }
        }
        Ok(Some(leaks))
    }

    fn heap_sweep(&mut self) -> Result<()> {
        let objs = std::mem::take(&mut self.store.data_mut().heap_objects);
        if objs.is_empty() {
            return Ok(());
        }
        let Some(mem) = self.instance.get_memory(&mut self.store, "memory") else {
            return Ok(());
        };
        let data = mem.data(&self.store);
        for (start, end) in objs {
            let rz_start = end as usize;
            let rz_end = rz_start + HEAP_REDZONE;
            if rz_end > data.len() {
                continue;
            }
            if let Some(i) = data[rz_start..rz_end].iter().position(|&b| b != HEAP_POISON) {
                return Err(Error::msg(format!(
                    "HEAP CHECK: object [{start:#x},{end:#x}) redzone byte {i} was \
                     overwritten — a write ran past the object end (a wrong field offset \
                     or a missing ensure())"
                )));
            }
        }
        Ok(())
    }

    /// The total bytes the `region:` copy-outs moved (the exported
    /// `__region_copy_bytes` counter), or None when the module has no regions.
    /// The exported re-own counter: how many in-place accumulation sites
    /// entered with a zero ownership token (each one copies). None when the
    /// module has no in-place machinery. (A test/diagnostic API.)
    pub fn reowns(&mut self) -> Option<i64> {
        self.instance
            .get_global(&mut self.store, "__witchy_reowns")
            .and_then(|g| g.get(&mut self.store).i64())
    }

    /// RFC-0110 state-bearing calls that executed through a typed indirect
    /// table signature. Direct and devirtualized calls are deliberately absent.
    pub fn indirect_ownership_calls(&mut self) -> Option<i64> {
        self.instance
            .get_global(&mut self.store, "__witchy_indirect_ownership_calls")
            .and_then(|g| g.get(&mut self.store).i64())
    }

    /// Source boundaries that repaired a missing uniqueness proof by making
    /// the one explicit re-own copy permitted in normal mode.
    pub fn boundary_reown_copies(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_boundary_reown_copies")
    }

    /// Ownership-state inputs reconstructed rather than forwarded from an
    /// already checked access envelope.
    pub fn ownership_token_repairs(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_ownership_token_repairs")
    }

    /// `var` calls lowered directly onto caller storage after all place and
    /// overlap proofs succeeded.
    pub fn direct_storage_var_accesses(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_direct_storage_var_accesses")
    }

    /// Unique-result sites which forwarded a caller destination selected by a
    /// canonical RFC-0111 layout descriptor.
    pub fn destination_candidates_forwarded(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_destination_candidates_forwarded")
    }

    pub fn packed_alloc_calls(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_packed_alloc_calls")
    }

    pub fn packed_alloc_bytes(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_packed_alloc_bytes")
    }

    pub fn region_copy_bytes(&mut self) -> Option<i64> {
        self.instance
            .get_global(&mut self.store, "__region_copy_bytes")
            .and_then(|g| g.get(&mut self.store).i64())
    }

    /// (RFC-0016) The total bytes `$rc_alloc` handed back out of the RC-floor
    /// free-list (reused rather than freshly bumped) — the exported
    /// `__rc_reused_bytes` counter. 0 unless the free-at-overwrite rule (gated
    /// `WITCHY_OPT=rc-floor`) populated the list, so it proves the reclamation
    /// actually fired and recycled. None when the module has no heap.
    pub fn rc_reused_bytes(&mut self) -> Option<i64> {
        self.instance
            .get_global(&mut self.store, "__rc_reused_bytes")
            .and_then(|g| g.get(&mut self.store).i64())
    }

    /// (RFC-0035) Live rc_alloc objects at program end — the exported
    /// `__witchy_live_cells` counter (`$rc_alloc` +1, `$rc_free` -1). A leak metric:
    /// for a fully-reclaiming rc-floor program it stays bounded (the reachable roots);
    /// an unbounded leak grows it with the input. 0 unless a `$rc_free` fired.
    pub fn live_cells(&mut self) -> Option<i64> {
        self.instance
            .get_global(&mut self.store, "__witchy_live_cells")
            .and_then(|g| g.get(&mut self.store).i64())
    }

    fn i64_counter(&mut self, name: &str) -> Option<i64> {
        self.instance
            .get_global(&mut self.store, name)
            .and_then(|g| g.get(&mut self.store).i64())
    }

    pub fn rc_alloc_calls(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_rc_alloc_calls")
    }

    /// Logical payload bytes requested from `$rc_alloc`, excluding its hidden
    /// RC/size header and independent of free-list reuse.
    pub fn rc_alloc_requested_bytes(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_rc_alloc_requested_bytes")
    }

    pub fn bump_alloc_calls(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_bump_alloc_calls")
    }

    pub fn rc_reuse_calls(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_rc_reuse_calls")
    }

    pub fn rc_free_calls(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_rc_free_calls")
    }

    pub fn region_rewind_calls(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_region_rewind_calls")
    }

    pub fn list_header_loads(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_list_header_loads")
    }

    pub fn checked_indexed_loads(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_checked_indexed_loads")
    }

    pub fn checked_indexed_stores(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_checked_indexed_stores")
    }

    pub fn cursorized_indexed_accesses(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_cursorized_indexed_accesses")
    }

    pub fn sequence_bounds_checks_coalesced(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_sequence_bounds_checks_coalesced")
    }

    pub fn sequence_forwarded_loads(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_sequence_forwarded_loads")
    }

    pub fn sequence_small_loops_unrolled(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_sequence_small_loops_unrolled")
    }

    /// Executed `task.run` slot retags.  This is a deterministic scheduler
    /// transition count, available only from instrumented `witchy stats` builds.
    pub fn scheduler_slot_transitions(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_scheduler_slot_transitions")
    }

    /// Exact Wasm-GC allocations of compiler-owned scheduler frame entries.
    /// This intentionally excludes generic allocator traffic and in-place
    /// scheduler slot retags.
    pub fn scheduler_frame_allocation_calls(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_scheduler_frame_allocation_calls")
    }

    /// The eager-ready-probe subset of compiler-owned frame allocations.
    pub fn scheduler_ready_frame_allocation_calls(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_scheduler_ready_frame_allocation_calls")
    }

    /// Executed transitions into one of the scheduler's `Wait*` slot states.
    /// It counts installation events, not an approximation of peak live waits.
    pub fn scheduler_wait_installs(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_scheduler_wait_installs")
    }

    /// Executed claims that resume an ordinary scheduler wait slot. A claim is
    /// counted only after the scheduler has selected a concrete wake outcome.
    pub fn scheduler_wake_claims(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_scheduler_wake_claims")
    }

    /// One-time installations of compiler-owned reusable select wait frames.
    pub fn scheduler_reusable_wait_installs(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_scheduler_reusable_wait_installs")
    }

    /// Live compiler-owned frame registrations at program completion, keyed by
    /// `task.run` slot. A cleanly completed bounded scheduler run leaves this
    /// at zero.
    pub fn scheduler_frame_live_wait_registrations(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_scheduler_frame_live_wait_registrations")
    }

    /// Exact high-water number of parked compiler-owned `task.run` frame
    /// slots. Re-arming a wait in the same slot does not increase this number.
    pub fn scheduler_frame_peak_live_wait_registrations(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_scheduler_frame_peak_live_wait_registrations")
    }

    /// Compiler-frame wait-to-wait slot retags, which retain the same
    /// scheduler-slot registration identity rather than installing another
    /// live registration.
    pub fn scheduler_frame_wait_registration_rearms(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_scheduler_frame_wait_registration_rearms")
    }

    /// Nominal `Selected*` values constructed by the ordinary select adapter.
    /// A direct local-result lane must leave this at zero.
    pub fn selected_result_materializations(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_selected_result_materializations")
    }

    pub fn extract_searches(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_extract_searches")
    }

    pub fn extract_key_comparisons(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_extract_key_comparisons")
    }

    pub fn extract_copied_bytes(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_extract_copied_bytes")
    }

    pub fn extract_retains(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_extract_retains")
    }

    pub fn extract_drops(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_extract_drops")
    }

    pub fn dict_hashes(&mut self) -> Option<i64> { self.i64_counter("__witchy_dict_hashes") }
    pub fn dict_probe_groups(&mut self) -> Option<i64> { self.i64_counter("__witchy_dict_probe_groups") }
    pub fn dict_h2_candidates(&mut self) -> Option<i64> { self.i64_counter("__witchy_dict_h2_candidates") }
    pub fn dict_rebuilds(&mut self) -> Option<i64> { self.i64_counter("__witchy_dict_rebuilds") }
    pub fn dict_grows(&mut self) -> Option<i64> { self.i64_counter("__witchy_dict_grows") }
    pub fn dict_order_bytes_moved(&mut self) -> Option<i64> { self.i64_counter("__witchy_dict_order_bytes_moved") }
    pub fn dict_index_bytes(&mut self) -> Option<i64> { self.i64_counter("__witchy_dict_index_bytes") }

    /// (RFC-0030) The allocator's bump frontier above its heap base at
    /// program end: the heap high-water in bytes, since reclaimed objects are
    /// reused from the free list below it. For a fixed program the delta
    /// across `WITCHY_OPT` settings shows an allocation optimization firing — e.g.
    /// in-place push keeps an accumulation O(n) where forced-copy is O(n^2). A
    /// test/diagnostic API consumed by `witchy stats`.
    pub fn heap_bytes(&mut self) -> Option<i64> {
        let mut read_global = |name: &str| -> Option<u32> {
            let global = self.instance.get_global(&mut self.store, name)?;
            global.get(&mut self.store).i32().map(|value| value as u32)
        };
        let top = read_global(witchy_wir::HEAP_TOP_EXPORT)?;
        let base = read_global(witchy_wir::HEAP_BASE_EXPORT)?;
        Some(i64::from(top) - i64::from(base))
    }

    /// The current capacity in bytes of Wasmtime's GC backing heap.
    ///
    /// This is authoritative runtime accounting, not process RSS or Witchy's
    /// linear-memory `$heap`. Wasmtime 45's deferred-reference-counting heap
    /// only adds capacity, so reading this after execution is that VM's GC-heap
    /// capacity high-water. It does not expose live-object bytes or an object
    /// count.
    pub fn gc_heap_capacity_bytes(&self) -> usize {
        self.store.gc_heap_capacity()
    }

    pub fn enable_owner_bridge_metrics(&mut self) {
        self.store.data_mut().owner_bridge_counts = Some(Default::default());
    }

    pub fn owner_bridge_counts(&self) -> Option<&std::collections::BTreeMap<&'static str, u64>> {
        self.store.data().owner_bridge_counts.as_ref()
    }

    /// Everything this VM has printed so far, in order. (Used by tests to
    /// assert a compiled program's behavior end to end.)
    pub fn output(&self) -> Vec<String> {
        self.store.data().output.lock().unwrap().clone()
    }
}

/// The runtime owns the wasm engine and the shared mailbox registry, and hands
/// out VM ids.
pub struct Runtime {
    engine: Engine,
    next_id: VmId,
    preempt: bool,
    /// Whether the ordinary optimized-Wasm and Wasmtime compilation caches are
    /// enabled for this runtime.
    executable_cache: bool,
    /// Injected into every VM's state; the Wasmtime compiler-service adapters
    /// call this interface rather than the compiler implementation directly.
    compiler_services: Arc<dyn compiler::CompilerServices>,
}

/// Verified bytes of one separately compiled scalar-only Wasm library.
///
/// The caller artifact selects this value by the complete identity triple,
/// never by `module`/`export` text.  Construction recomputes the Wasm digest;
/// the package loader is separately responsible for deriving the binding and
/// sidecar commitments from its signed manifest and bounded declaration bundle.
#[derive(Clone, Debug)]
pub struct AuthenticatedExternalLibrary {
    identity: ExternalWasmIdentity,
    wasm: Vec<u8>,
}

impl AuthenticatedExternalLibrary {
    pub fn from_verified_bytes(
        identity: ExternalWasmIdentity,
        wasm: Vec<u8>,
    ) -> Result<Self> {
        if sha256(&wasm) != identity.wasm_sha256 {
            bail!("external Wasm bytes differ from authenticated library identity");
        }
        validate_closed_external_wasm(&wasm)?;
        Ok(Self { identity, wasm })
    }

    pub fn identity(&self) -> ExternalWasmIdentity { self.identity }
}

fn validate_closed_external_wasm(wasm: &[u8]) -> Result<()> {
    if !wasm.starts_with(b"\0asm") {
        bail!("external library must be binary Wasm");
    }
    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        match payload.map_err(|error| Error::msg(format!("invalid external Wasm: {error}")))? {
            wasmparser::Payload::ImportSection(reader) => {
                if reader.count() != 0 {
                    bail!("external scalar library imports are unsupported");
                }
            }
            wasmparser::Payload::StartSection { .. } => {
                bail!("external scalar library start functions are unsupported");
            }
            _ => {}
        }
    }
    Ok(())
}

impl Runtime {
    /// A runtime whose VMs can be preempted by the scheduler advancing the
    /// engine epoch. Epoch interruption makes the JIT insert a check at
    /// every loop backedge and call, so for run-to-completion single-program
    /// execution prefer [`Runtime::batch`], which omits that per-iteration cost.
    pub fn new() -> Result<Self> {
        Self::configured(true, wasmtime::OptLevel::Speed, false)
    }

    /// A preemptible runtime that also uses Wasmtime's validated compilation
    /// cache. Use this when the same admitted module is launched repeatedly;
    /// epoch interruption remains enabled for bounded execution.
    pub fn new_cached() -> Result<Self> {
        Self::configured(true, wasmtime::OptLevel::Speed, true)
    }

    /// A runtime for run-to-completion execution — the `sandbox`/benchmark path
    /// and differential WASM runs. No epoch interruption, so the generated code
    /// runs without per-backedge preemption checks (a measurable speedup on
    /// tight loops and recursion). There is no scheduler to preempt it; the
    /// capability sandbox (only granted host fns, capped linear memory) is still
    /// fully in force, so this is a speed choice, not a security relaxation.
    pub fn batch() -> Result<Self> {
        Self::configured(false, wasmtime::OptLevel::Speed, true)
    }

    /// Same sandbox and ABI as [`Self::batch`], but Cranelift skips Speed-tier
    /// lowering. Many short-lived modules (the differential suite) spend more
    /// time in Cranelift than in the program; generated-code quality does not
    /// change observable results.
    pub fn batch_quick() -> Result<Self> {
        Self::configured(false, wasmtime::OptLevel::None, true)
    }

    /// Same sandbox as [`Self::batch`], but with a specific Cranelift profiling strategy.
    pub fn batch_with_profiler(profiler: wasmtime::ProfilingStrategy) -> Result<Self> {
        let mut rt = Self::configured(false, wasmtime::OptLevel::Speed, true)?;
        let mut config = Config::new();
        config.cranelift_opt_level(wasmtime::OptLevel::Speed);
        config.compiler_inlining(wasmtime::Inlining::Yes);
        config.wasm_reference_types(true);
        config.wasm_function_references(true);
        config.wasm_gc(true);
        config.wasm_simd(true);
        config.wasm_relaxed_simd(true);
        config.wasm_multi_memory(false);
        // Canonical SafeWIR lowers proper tail sites to `return_call`
        // (RFC-0165); frozen modules never emit it, so admitting the proposal
        // changes nothing for them. RFC-0090's resource probes still gate B4.
        config.wasm_tail_call(true);
        enable_bounded_memory64(&mut config);
        config.wasm_wide_arithmetic(false);
        // The encoder hints each check whose failure arm only traps, so
        // Cranelift lays that arm out of line. Hints never change semantics.
        config.wasm_branch_hinting(true);
        config.profiler(profiler);
        if (profiler != wasmtime::ProfilingStrategy::None
            && profiler != wasmtime::ProfilingStrategy::PerfMap)
            || std::env::var_os("WITCHY_DEBUG_INFO").is_some()
        {
            config.debug_info(true);
        }
        rt.engine = match Engine::new(&config) {
            Ok(e) => e,
            Err(err) => {
                eprintln!("warning: profiling strategy failed on this platform ({err}); falling back to none");
                config.profiler(wasmtime::ProfilingStrategy::None);
                Engine::new(&config)?
            }
        };
        Ok(rt)
    }

    /// Parse a profiling strategy name from user configuration or environment variables.
    pub fn profiling_strategy_from_name(name: &str) -> Option<wasmtime::ProfilingStrategy> {
        match name.trim().to_lowercase().as_str() {
            "perfmap" | "perf_map" | "1" | "true" => Some(wasmtime::ProfilingStrategy::PerfMap),
            "jitdump" | "jit_dump" => Some(wasmtime::ProfilingStrategy::JitDump),
            "vtune" => Some(wasmtime::ProfilingStrategy::VTune),
            "pulley" => Some(wasmtime::ProfilingStrategy::Pulley),
            "none" | "0" | "false" => Some(wasmtime::ProfilingStrategy::None),
            _ => None,
        }
    }

    fn configured(
        preempt: bool,
        opt_level: wasmtime::OptLevel,
        executable_cache: bool,
    ) -> Result<Self> {
        Self::configured_with_fuel(preempt, opt_level, executable_cache, false)
    }

    fn configured_with_fuel(
        preempt: bool, opt_level: wasmtime::OptLevel, executable_cache: bool, fuel: bool,
    ) -> Result<Self> {
        let mut config = Config::new();
        config.consume_fuel(fuel);
        // Production batch/new keep Speed. `batch_quick` uses None so a
        // 10-line differential case is not paying an optimizing JIT.
        config.cranelift_opt_level(opt_level);
        // An optimizing engine inlines small guest functions (std wrappers,
        // `var` write-back helpers) into their callers. Abort diagnostics are
        // explicit host calls, so no reported location depends on frames.
        if opt_level != wasmtime::OptLevel::None {
            config.compiler_inlining(wasmtime::Inlining::Yes);
        }
        // (RFC-0005 step 7) Defense in depth: shrink the codegen/runtime surface to
        // exactly what witchy emits. Threads are compiled out of the Wasmtime
        // dependency entirely; pin every other WASM proposal explicitly so a
        // miscompile or a crafted module cannot reach unselected machinery. We KEEP the
        // proposals we DO emit (bulk memory for `MemoryCopy`/`MemoryFill`, multi-value
        // for the `*_cap` ABI) at their defaults, and explicitly keep reference
        // types / function references / GC on for the externref capability core
        // (RFC-0005 steps 2–6): nullable cap values emit `ref.null extern` /
        // `ref.is_null`, and named cap-carrying records emit GC refs. Cranelift's
        // Spectre mitigations (heap + table access) are ON by default and stay on — do
        // NOT set `signals_based_traps(false)`, which would force them off.
        config.wasm_reference_types(true);
        config.wasm_function_references(true);
        config.wasm_gc(true);
        config.wasm_simd(true);
        config.wasm_relaxed_simd(true);
        config.wasm_multi_memory(false);
        // Canonical SafeWIR lowers proper tail sites to `return_call`
        // (RFC-0165); frozen modules never emit it, so admitting the proposal
        // changes nothing for them. RFC-0090's resource probes still gate B4.
        config.wasm_tail_call(true);
        enable_bounded_memory64(&mut config);
        // Wasmtime 49 turned wide arithmetic on by default; witchy emits none
        // of its instructions, so it stays outside the admitted surface.
        config.wasm_wide_arithmetic(false);
        // Failure arms of checks are hinted cold by the encoder.
        config.wasm_branch_hinting(true);
        // Support Cranelift profiling strategies for external profilers (Linux perf,
        // VTune, JIT symbol mapping). Configurable via WITCHY_PROFILE or WITCHY_PERF_MAP.
        if let Ok(strat) = std::env::var("WITCHY_PROFILE") {
            if let Some(profile_strat) = Self::profiling_strategy_from_name(&strat) {
                config.profiler(profile_strat);
                if profile_strat != wasmtime::ProfilingStrategy::None
                    && profile_strat != wasmtime::ProfilingStrategy::PerfMap
                {
                    config.debug_info(true);
                }
            } else {
                eprintln!("warning: unknown WITCHY_PROFILE strategy '{strat}'; expected perfmap, jitdump, vtune, or none");
            }
        } else if std::env::var_os("WITCHY_PERF_MAP").is_some() {
            config.profiler(wasmtime::ProfilingStrategy::PerfMap);
        }
        if std::env::var_os("WITCHY_DEBUG_INFO").is_some() {
            config.debug_info(true);
        }

        // Epoch-based interruption lets the scheduler preempt a runaway VM.
        // It is only worth its per-backedge cost when a
        // scheduler will actually advance the epoch.
        if preempt {
            config.epoch_interruption(true);
        }
        if executable_cache && let Some(cache) = compilation_cache() {
            // The batch path re-runs the same program across invocations (a CLI
            // re-run, a benchmark loop); caching the compile makes the second
            // run onward skip Cranelift. Preemptible runtimes can use the same
            // cache without disabling epoch interruption.
            config.cache(Some(cache));
        }
        // (RFC-0034 L4) Pooling instance allocator: reuse pre-reserved instance
        // slots instead of fresh mmap/teardown per instantiation — the lever for
        // the many-instance paths (serve_pool, a future par_map worker pool). Each
        // slot must reserve witchy's per-instance memory cap (1 GiB), so the pool is
        // kept modest. Opt-in via WITCHY_POOL while we measure whether the up-front
        // reservation pays off for one-shot runs (it likely doesn't — the win is
        // many-instance) before making it default.
        if !preempt && std::env::var_os("WITCHY_POOL").is_some() {
            let mut pool = wasmtime::PoolingAllocationConfig::default();
            pool.total_memories(64);
            pool.total_core_instances(64);
            pool.max_memory_size(16384 * 64 * 1024);
            config.allocation_strategy(wasmtime::InstanceAllocationStrategy::Pooling(pool));
        }
        let engine = match Engine::new(&config) {
            Ok(e) => e,
            Err(err) => {
                eprintln!("warning: failed to initialize wasmtime engine with requested configuration ({err}); falling back to default profiler");
                config.profiler(wasmtime::ProfilingStrategy::None);
                Engine::new(&config)?
            }
        };
        Ok(Self {
            engine,
            next_id: 1,
            preempt,
            executable_cache,
            compiler_services: Arc::new(compiler::RegistryCompilerServices),
        })
    }

    /// Spawn a VM from untrusted external WAT/wasm source, granting it exactly `caps` and
    /// capping its linear memory at `memory_pages_max` 64KiB pages.
    ///
    /// This is the compatibility entry point for externally supplied Wasm and
    /// tests. It may apply the configured Binaryen policy before Wasmtime
    /// instantiation. A raw module carrying canonical entry metadata is
    /// rejected: metadata inside caller-supplied bytes is not an admission
    /// receipt. Canonical compiler output must use
    /// [`Self::spawn_admitted`], whose artifact-typed API cannot be confused
    /// with this raw-byte path and never performs a post-admission rewrite.
    ///
    /// The capability check is structural: a granted host function is linked,
    /// an ungranted one is absent, so a VM that imports an ungranted
    /// capability fails right here at `instantiate`.
    pub fn spawn(
        &mut self,
        wasm: impl AsRef<[u8]>,
        caps: Capabilities,
        memory_pages_max: usize,
    ) -> Result<Vm> {
        let wasm = wasm.as_ref();
        let claimed_entry = if wasm.starts_with(b"\0asm") {
            witchy_wir::canonical_entry_from_wasm(wasm).map_err(Error::msg)?
        } else {
            None
        };
        if claimed_entry.is_some() {
            return Err(Error::msg(
                "raw Wasm cannot claim canonical admission; launch an AdmittedCanonicalArtifact",
            ));
        }
        // `wasm-opt` runs inside `build_module` only on an optimized-wasm cache miss;
        // every hit and miss still enters Wasmtime through safe `Module::new`.
        let module = build_module(&self.engine, wasm, self.executable_cache)?;

        self.instantiate_module(WorkerModule::Unadmitted(module), caps, memory_pages_max, None, &[])
    }

    /// Spawn a VM from an admitted canonical SafeWIR artifact.
    ///
    /// The runtime instantiates exactly [`AdmittedCanonicalArtifact::wasm`]
    /// and performs no Binaryen optimization, cache lookup, or byte mutation.
    /// `Module::new` still performs Wasmtime's structural validation before any
    /// capability imports are linked.
    pub fn spawn_admitted(
        &mut self,
        artifact: &AdmittedCanonicalArtifact,
        caps: Capabilities,
        memory_pages_max: usize,
    ) -> Result<Vm> {
        if artifact.compiler_session().is_some() {
            bail!("compiler recipe requires the dedicated comptime launcher");
        }
        let module = WorkerModule::from_admitted(&self.engine, artifact)?;
        self.instantiate_module(module, caps, memory_pages_max, artifact.entry().cloned(), &[])
    }

    /// Spawn an admitted caller with exact separately compiled scalar Wasm
    /// libraries. The library list is neither ambient nor name-selected: it
    /// must cover exactly the identities retained in the admitted artifact.
    pub fn spawn_admitted_with_external_libraries(
        &mut self,
        artifact: &AdmittedCanonicalArtifact,
        caps: Capabilities,
        memory_pages_max: usize,
        libraries: &[AuthenticatedExternalLibrary],
    ) -> Result<Vm> {
        let module = WorkerModule::from_admitted(&self.engine, artifact)?;
        self.instantiate_module(module, caps, memory_pages_max, artifact.entry().cloned(), libraries)
    }

    fn instantiate_module(
        &mut self,
        module: WorkerModule,
        caps: Capabilities,
        memory_pages_max: usize,
        entry: Option<CanonicalEntry>,
        external_libraries: &[AuthenticatedExternalLibrary],
    ) -> Result<Vm> {
        self.instantiate_module_with_session(module, caps, memory_pages_max, entry, external_libraries, None)
    }

    fn instantiate_module_with_session(
        &mut self, module: WorkerModule, caps: Capabilities, memory_pages_max: usize,
        entry: Option<CanonicalEntry>, external_libraries: &[AuthenticatedExternalLibrary],
        session: Option<(comptime::SessionState, u64)>,
    ) -> Result<Vm> {
        let compiler_recipe = matches!(&module, WorkerModule::Admitted(program) if program.artifact.compiler_session().is_some());
        if compiler_recipe != session.is_some() {
            bail!("compiler recipe and dedicated session launch must match");
        }
        let id = self.next_id;

        let limits = StoreLimitsBuilder::new()
            .memory_size(memory_pages_max * 64 * 1024)
            .build();
        let mut state = vmstate_from_caps(
            id,
            &caps,
            limits,
            None,
            &self.engine,
            module.module(),
            self.preempt,
            self.compiler_services.clone(),
        )?;
        state.module = module.clone();

        let compiler_fuel = session.map(|(session, fuel)| {
            state.compiler_syntax = Some(session);
            fuel
        });

        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| &mut s.limits);
        if let Some(fuel) = compiler_fuel { store.set_fuel(fuel)?; }
        // Deadline of 1 epoch; we never advance the engine epoch during a
        // normal run, so this is never tripped. The scheduler advances it to preempt.
        // Only meaningful when the engine has epoch interruption enabled.
        if self.preempt {
            store.set_epoch_deadline(1);
        }

        let mut linker: Linker<VmState> = Linker::new(&self.engine);
        link_capability_imports(&mut linker, &caps)?;
        if compiler_fuel.is_some() { host::compiler_syntax::register(&mut linker)?; }
        if let WorkerModule::Admitted(program) = &module {
            host::closure_owner::register(&mut linker, &program.artifact)?;
            link_authenticated_external_imports(
                &mut linker,
                &mut store,
                &self.engine,
                &program.artifact,
                external_libraries,
            )?;
        } else if !external_libraries.is_empty() {
            bail!("raw Wasm cannot receive authenticated external libraries");
        }

        // Ungranted capability imports are rejected here.
        let instance = linker.instantiate(&mut store, module.module())?;

        // Only commit the id once the VM actually came up.
        self.next_id += 1;
        Ok(Vm { store, instance, aborted: false, entry })
    }

    /// Execute one compiled module under an inert deterministic fixture plan.
    ///
    /// This entry point owns the fixture host for exactly one run and returns
    /// its finalized transcript. Production spawn paths cannot accept a plan.
    #[cfg(feature = "test-fixtures")]
    pub fn run_fixtures(
        &mut self,
        wasm: impl AsRef<[u8]>,
        plan: witchy_testkit::FixturePlan,
        memory_pages_max: usize,
    ) -> Result<FixtureWasmOutcome> {
        let fixture_host = witchy_test_host::FixtureHost::new(plan)
            .map_err(|error| Error::msg(format!("invalid fixture plan: {error}")))?;
        let roots = fixture_host.roots().clone();
        let id = self.next_id;
        let module = build_module(&self.engine, wasm.as_ref(), self.executable_cache)?;
        let limits = StoreLimitsBuilder::new()
            .memory_size(memory_pages_max * 64 * 1024)
            .build();
        let caps = Capabilities::none();
        let mut state = vmstate_from_caps(
            id,
            &caps,
            limits,
            None,
            &self.engine,
            &module,
            self.preempt,
            self.compiler_services.clone(),
        )?;
        state.fixture_host = Some(fixture_host);

        let mut store = Store::new(&self.engine, state);
        store.limiter(|state| &mut state.limits);
        if self.preempt {
            store.set_epoch_deadline(1);
        }
        let mut linker: Linker<VmState> = Linker::new(&self.engine);
        link_capability_imports(&mut linker, &caps)?;
        host::fixture::link_basic(&mut linker, &roots)?;
        host::fixture::link_filesystem(&mut linker, &roots)?;
        host::fixture::link_fetch(&mut linker, &roots)?;
        host::fixture::link_exec(&mut linker, &roots)?;
        if roots.secrets.is_some() {
            host::crypto::link_keyed(&mut linker)?;
        }
        let instance = linker.instantiate(&mut store, &module)?;
        self.next_id += 1;
        let mut vm = Vm {
            store,
            instance,
            aborted: false,
            entry: None,
        };
        let execution = vm.run();
        let output = vm.output();
        let transcript_result = match &execution {
            Ok(()) => witchy_testkit::TestResult::Passed,
            Err(error) => witchy_testkit::TestResult::Failed {
                message: error.root_cause().to_string(),
            },
        };
        let transcript = vm
            .store
            .data_mut()
            .fixture_host
            .take()
            .expect("fixture run installed a fixture host")
            .finish(transcript_result);
        let result = match execution {
            Ok(()) => FixtureWasmResult::Passed,
            Err(error) => FixtureWasmResult::Failed {
                error: error.root_cause().to_string(),
            },
        };
        Ok(FixtureWasmOutcome {
            leaked_objects: None,
            result,
            output,
            transcript,
        })
    }

    /// Run a VM, but preempt it if it runs longer than `budget`. A watchdog
    /// advances the engine epoch once the budget elapses; the VM traps at
    /// its next loop back-edge or call. This is how the scheduler reclaims a
    /// runaway or malicious VM that refuses to yield.
    pub fn run_with_budget(&self, vm: &mut Vm, budget: Duration) -> Result<()> {
        self.with_epoch_budget(budget, || vm.run())
    }

    /// Invoke one pure Bytes export under the same memory and epoch bounds as a
    /// normal preemptible VM.
    pub fn invoke_bytes_with_budget(
        &self,
        vm: &mut Vm,
        export: &str,
        input: &[u8],
        max_output_bytes: usize,
        budget: Duration,
    ) -> Result<Vec<u8>> {
        self.with_epoch_budget(budget, || {
            vm.invoke_bytes(export, input, max_output_bytes)
        })
    }

    fn with_epoch_budget<T>(
        &self,
        budget: Duration,
        execute: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        if !self.preempt {
            bail!("budgeted execution requires a preemptible Witchy runtime");
        }
        let engine = self.engine.clone();
        let (cancel, cancelled) = std::sync::mpsc::sync_channel(0);
        let watchdog = std::thread::spawn(move || {
            if cancelled.recv_timeout(budget).is_err() {
                engine.increment_epoch();
            }
        });
        let result = execute();
        let _ = cancel.send(());
        watchdog.join().ok();
        result
    }
}

// ---------------------------------------------------------------------------
// Host functions = capabilities. Each reads/writes the *calling* VM's own
// linear memory via `Caller`; none can touch another VM's memory.
// ---------------------------------------------------------------------------

/// Register the capability host imports a grant entitles a VM to. Only granted
/// families are defined; a module compiled against an ungranted operation cannot
/// even instantiate.
fn link_authenticated_external_imports(
    linker: &mut Linker<VmState>,
    store: &mut Store<VmState>,
    engine: &Engine,
    artifact: &AdmittedCanonicalArtifact,
    libraries: &[AuthenticatedExternalLibrary],
) -> Result<()> {
    let mut supplied = BTreeMap::new();
    for library in libraries {
        library.verify()?;
        if supplied.insert(library.identity, library).is_some() {
            bail!("duplicate authenticated external library identity");
        }
    }
    let mut expected = BTreeMap::new();
    for import in artifact.external_imports() {
        expected.insert(import.identity(), ());
    }
    if supplied.len() != expected.len() || !supplied.keys().all(|identity| expected.contains_key(identity)) {
        bail!("external library set does not exactly match admitted artifact identities");
    }

    let mut instances = BTreeMap::new();
    for identity in expected.keys() {
        let library = supplied.get(identity).expect("identity set checked");
        let module = Module::new(engine, &library.wasm)
            .map_err(|error| Error::msg(format!("authenticated external Wasm failed Wasmtime validation: {error}")))?;
        let empty: Linker<VmState> = Linker::new(engine);
        let instance = empty.instantiate(&mut *store, &module)
            .map_err(|error| Error::msg(format!("authenticated external Wasm did not instantiate: {error}")))?;
        instances.insert(*identity, instance);
    }
    for import in artifact.external_imports() {
        let instance = instances.get(&import.identity()).expect("identity set checked");
        let function = instance.get_func(&mut *store, import.name()).ok_or_else(|| {
            Error::msg(format!("authenticated external library lacks required export `{}`", import.name()))
        })?;
        // Identity selected the instance above. Module/name here only select
        // the already-authenticated import slot and export; they are never a
        // lookup key for a supplied library.
        linker.define(&mut *store, import.module(), import.name(), function)?;
    }
    Ok(())
}

impl AuthenticatedExternalLibrary {
    fn verify(&self) -> Result<()> {
        if sha256(&self.wasm) != self.identity.wasm_sha256 {
            bail!("external Wasm bytes changed after authenticated-library construction");
        }
        validate_closed_external_wasm(&self.wasm)
    }
}

pub(crate) fn link_capability_imports(
    linker: &mut Linker<VmState>,
    caps: &Capabilities,
) -> Result<()> {
    // (RFC-0023) The checked-heap shadow imports are not capabilities — they grant no
    // authority (they only poison/record/reclaim a redzone), so they are always defined
    // and only the checked codegen ever emits calls to them.
    linker.func_wrap("witchy", "heap_register", host_heap_register)?;
    linker.func_wrap("witchy", "heap_unregister", host_heap_unregister)?;
    linker.func_wrap("witchy", "heap_frontier", host_heap_frontier)?;

    host::console::link_abort(linker)?;
    // These imports expose no guest mint; each call authenticates its token.
    host::clock_env_rand::link_canonical_clock(linker)?;
    host::secret::link_canonical(linker)?;
    host::crypto::link_canonical_keyed(linker)?;
    host::secret_response::link_canonical(linker)?;
    host::cap_response::link(linker)?;
    // Linking the fixture ABI grants no provider or token. Its handler checks
    // the private fixture session before reading guest memory or producing output.
    host::console::link_fixture_console(linker)?;
    // --- capability wiring: only granted host functions are defined ---
    if caps.print {
        host::console::link_print(linker)?;
    }
    if caps.console_read {
        host::console::link_console_read(linker)?;
    }
    if caps.print_int {
        host::console::link_print_values(linker)?;
    }
    if caps.rand {
        host::clock_env_rand::link_rand(linker)?;
    }
    if caps.clock {
        host::clock_env_rand::link_clock(linker)?;
    }
    if caps.env {
        host::clock_env_rand::link_env(linker)?;
    }
    // The Dir family is linked per RIGHT, so a module compiled against a
    // write operation cannot even instantiate under a read-only grant.
    let has_real_dir =
        !caps.preopened_dirs.is_empty() || caps.dir_root.is_some() || !caps.dir_roots.is_empty();
    let dir_read = caps.dir_read || caps.dir_rights.iter().any(|r| r.read);
    let dir_write = caps.dir_write || caps.dir_rights.iter().any(|r| r.write);
    let has_readable_dir = has_real_dir && dir_read;
    if has_real_dir {
        host::filesystem::link_mint_dir(linker)?;
    }
    if has_readable_dir {
        host::filesystem::link_dir_read(linker)?;
    }
    if has_real_dir && dir_write {
        host::filesystem::link_dir_write(linker)?;
    }
    // RFC-0012 File leaf ops: usable on a File obtained by navigation (above) OR
    // granted directly to `main` (`--file`), so they are linked whenever either a
    // Dir grant or a direct File grant is present.
    let has_file_grants = !caps.preopened_files.is_empty() || !caps.file_grants.is_empty();
    let direct_file_read = if caps.file_rights.is_empty() {
        has_file_grants
    } else {
        caps.file_rights.iter().any(|r| r.read)
    };
    let direct_file_write = if caps.file_rights.is_empty() {
        has_file_grants
    } else {
        caps.file_rights.iter().any(|r| r.write)
    };
    if has_readable_dir || direct_file_read {
        host::filesystem::link_file_read(linker)?;
    }
    if (has_real_dir && dir_write) || direct_file_write {
        host::filesystem::link_file_write(linker)?;
    }
    // (RFC-0005 Stage 2) The `run` wrapper mints each `--file` `main` param as an
    // `externref` via `mint_file`; only reachable when a direct File grant exists.
    if has_file_grants {
        host::filesystem::link_mint_file(linker)?;
    }
    // The Exec capability: spawn a confined subprocess. The executable is named
    // through a `Dir[Read]` capability, so `exec_run` reuses `dir_base`/`confine`.
    if caps.exec {
        host::filesystem::link_exec(linker)?;
    }
    let net = caps.net_allow.is_some() || !caps.net_grants.is_empty();
    // The admitted canonical Net ABI authenticates an opaque capability token
    // at each call, so its registration is not authority.  It must be present
    // even when an unreachable public helper mentions Net but `main` does not:
    // Wasmtime resolves all imports before it can start the admitted entry.
    host::network::link_canonical(linker)?;
    // (RFC-0005 Stage 3) The `run` wrapper mints each root `Net` param as an
    // `externref` via `mint_net`; only reachable when a Net grant exists.
    if net {
        host::network::link_mint(linker)?;
    }
    // The legacy Net family remains linked per VERB right. Socket I/O carries
    // no authority of its own (a socket is only obtainable through a granted
    // connect or accept), so it is linked under either verb.
    if net && caps.net_connect {
        host::network::link_connect(linker)?;
    }
    if net && caps.net_listen {
        host::network::link_listen(linker)?;
    }
    if net && (caps.net_connect || caps.net_listen) {
        host::network::link_socket_io(linker)?;
    }
    if !caps.fetch_grants.is_empty() {
        host::fetch::link_mint(linker)?;
    }
    // Canonical linkage is authority-free; every call authenticates a token.
    host::fetch::link(linker)?;
    if !caps.fetch_grants.is_empty() || (net && caps.net_connect) {
        host::fetch::link_ops(linker)?;
    }
    if caps.signing_key.is_some() {
        host::secret::link_mint(linker)?;
    }
    // Any granted Secret (root signing key or named SecretStore secret) can be
    // consumed by the keyed crypto helpers while bytes stay host-side.
    if caps.signing_key.is_some() || !caps.secrets.is_empty() {
        host::crypto::link_keyed(linker)?;
    }
    // The build-time capabilities. Like Dir, each is linked only when granted
    // — a build step compiled against `write_out` cannot even instantiate
    // without a `BuildOut` grant, and `read_build` without a `BuildRead` one.
    // Canonical imports may be present in uncalled public declarations. The
    // operation authenticates a private token; linking itself grants nothing.
    host::build_out::link(linker)?;
    host::build_read::link(linker)?;
    host::build_env::link(linker)?;
    host::build_exec::link(linker)?;
    host::build_net::link(linker)?;
    if caps.build_out.is_some() {
        host::build::link_out(linker)?;
    }
    if !caps.build_read_roots.is_empty() {
        host::build::link_read(linker)?;
    }
    if caps.build_env.is_some() {
        host::build::link_env(linker)?;
    }
    // BuildNet uses its authenticated root/slice import and immutable response.
    // Do not retain the legacy grant-only build_fetch_len/pending route.
    // build_exec_run now has the authenticated token/slice ABI. Do not also
    // register the old zero-representation staging ABI under the same name.
    host::staging::link_staging(linker)?;
    host::vm_worker::link_vm(linker)?;
    // Field-length staging helpers (`[len]` of a host cell's string/list field).
    // They carry no authority — data reads — and the WIR static prelude declares
    // them unconditionally, so define harmless stubs here. Ordinary programs
    // never call them, so the body is unreachable; returning 0 is a safe default.
    linker.func_wrap("witchy", "field_str_len", |_: Caller<'_, VmState>, _: i32| -> i32 { 0 })?;
    linker.func_wrap("witchy", "field_intlist_len", |_: Caller<'_, VmState>, _: i32| -> i32 { 0 })?;
    linker.func_wrap("witchy", "field_strlist_size", |_: Caller<'_, VmState>, _: i32| -> i32 { 0 })?;
    // Native-stdlib functions carry no authority, so they're always
    // available — the same `crypto` module the interpreter exposes, here as a
    // host import that bridges to the shared `native` registry.
    host::crypto::link_pure(linker)?;
    host::secret::link_lookup(linker)?;
    host::crypto::link_reveal(linker)?;
    // The compiler's footprint analyses are pure functions of their source
    // arguments — the toolchain exposed to witchy, same registry bridge.
    compiler::link(linker)?;
    host::staging::link_pure(linker)?;
    Ok(())
}

fn confine<T>(r: std::result::Result<T, crate::confine::ConfineError>) -> Result<T> {
    r.map_err(|e| Error::msg(e.message))
}

/// The single `VmState` constructor: build a VM's host state from the capabilities it
/// is granted. The primary VM (`spawn`) and every worker VM (`spawn_worker`) go through
/// here, so the root Dir/File/Net grant material and build grants are derived
/// from `caps` in exactly one place. `worker_listener` is `Some` only for a
/// `serve` pool worker.
#[allow(clippy::too_many_arguments)]
fn vmstate_from_caps(
    id: VmId,
    caps: &Capabilities,
    limits: StoreLimits,
    worker_listener: Option<(std::sync::Arc<std::net::TcpListener>, Option<crate::net::ServerTlsConfig>)>,
    engine: &Engine,
    module: &Module,
    preempt: bool,
    compiler_services: Arc<dyn compiler::CompilerServices>,
) -> Result<VmState> {
    let default_dir_rights = FsRights::new(caps.dir_read, caps.dir_write);
    let dirs = caps
        .preopened_dirs
        .iter()
        .cloned()
        .map(Ok)
        .chain(
            caps.dir_root
                .iter()
                .cloned()
                .chain(caps.dir_roots.iter().cloned())
                .map(|path| confine(crate::confine::ConfinedDir::open_ambient(&path))),
        )
        .enumerate()
        .map(|(i, dir)| Ok(DirAuthority {
            backing: DirBacking::Fs(dir?),
            policy: String::new(),
            rights: caps.dir_rights.get(i).copied().unwrap_or(default_dir_rights),
        }))
        .collect::<Result<Vec<_>>>()?;
    let files = caps
        .preopened_files
        .iter()
        .cloned()
        .map(Ok)
        .chain(
            caps.file_grants
                .iter()
                .map(|path| confine(crate::confine::ConfinedFile::open_ambient(path))),
        )
        .enumerate()
        .map(|(i, file)| Ok(FileAuthority {
            backing: FileBacking::Fs(file?),
            rights: caps.file_rights.get(i).copied().unwrap_or_else(FsRights::full),
        }))
        .collect::<Result<Vec<_>>>()?;
    let fetch_issuer = Arc::new(());
    let nets = caps
        .net_allow
        .iter()
        .cloned()
        .chain(caps.net_grants.iter().cloned())
        .map(|allow| NetAuthority { allow, issuer: fetch_issuer.clone() })
        .collect();
    let build_out = caps
        .build_out
        .as_deref()
        .map(crate::confine::ConfinedDir::open_ambient)
        .transpose()
        .map_err(|error| Error::msg(error.message))?;
    let build_read_roots = caps
        .build_read_roots
        .iter()
        .map(|root| crate::confine::ConfinedDir::open_ambient(root))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| Error::msg(error.message))?;
    let build_exec_tools = caps
        .build_exec_tools
        .iter()
        .map(|(name, path)| {
            crate::confine::ConfinedFile::open_ambient(path)
                .map(|file| (name.clone(), file))
        })
        .collect::<std::result::Result<_, _>>()
        .map_err(|error| Error::msg(error.message))?;
    Ok(VmState {
        owner_bridge_counts: std::env::var_os("WITCHY_OWNER_METRICS").filter(|value| value == "1").map(|_| Default::default()),
        compiler_syntax: None,
        id,
        caps: caps.clone(),
        compiler_services,
        limits,
        output: Arc::new(Mutex::new(Vec::new())),
        captured_output_bytes: 0,
        console_input: caps.console_input.clone().map(Into::into),
        dirs,
        files,
        pending: None,
        user_cap_fields: caps.user_cap_fields.clone(),
        pending_list: None,
        pending_ints: None,
        pending_admitted_par_map: None,
        admitted_zero_grant_worker_launches: 0,
        pending_bytes: None,
        nets,
        fetch_grants: caps.fetch_grants.clone(),
        fetch_issuer,
        registry_issuer: Arc::new(()),
        worker_listener,
        build_out,
        build_out_issuer: Arc::new(()),
        build_read_roots,
        build_read_issuer: Arc::new(()),
        build_env_issuer: Arc::new(()),
        build_exec_issuer: Arc::new(()),
        build_net_issuer: Arc::new(()),
        build_exec_tools,
        heap_objects: Vec::new(),
        rand_state: crate::rand::seed_from_env(),
        engine: engine.clone(),
        module: WorkerModule::Unadmitted(module.clone()),
        preempt,
        server_startup: None,
        vm_services: Arc::new(Mutex::new(VmServiceMetrics::default())),
        #[cfg(feature = "test-fixtures")]
        fixture_host: None,
        #[cfg(feature = "test-fixtures")]
        fixture_console: None,
    })
}

// --- small helpers for safe guest-memory access ---

/// Read a witchy string value (a `[i32 len][bytes...]` header) at `ptr`.
fn read_wstr(data: &[u8], ptr: i32) -> Result<String> {
    Ok(String::from_utf8_lossy(&read_wbytes(data, ptr)?).into_owned())
}

/// Read a guest `List(String)` — `[count: i32][count x i64 string pointers]`,
/// the layout `write_pending_list` produces and list literals share.
fn read_wstr_list(data: &[u8], ptr: i32) -> Result<Vec<String>> {
    let len_bytes = slice(data, ptr, 4)?;
    let len = i32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]);
    // SEC: `len` is read from guest memory, so a crafted module can claim a huge count.
    // Do NOT pre-reserve from it — `Vec::with_capacity(len)` would try to allocate ~51GB and
    // ABORT the host process. Cap the hint at the most 8-byte slots that could fit in memory
    // (the loop below still fails closed on the first out-of-bounds slot read).
    let mut out = Vec::with_capacity((len.max(0) as usize).min(data.len() / 8));
    for i in 0..len {
        let slot = slice(data, ptr + 4 + 8 * i, 8)?;
        let elem = i64::from_le_bytes([
            slot[0], slot[1], slot[2], slot[3], slot[4], slot[5], slot[6], slot[7],
        ]);
        out.push(read_wstr(data, elem as i32)?);
    }
    Ok(out)
}

/// Read a guest `Bytes` — `[len: i32][bytes…]` — as RAW bytes (no UTF-8 decode).
fn read_wbytes(data: &[u8], ptr: i32) -> Result<Vec<u8>> {
    let len_bytes = slice(data, ptr, 4)?;
    let len = i32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]);
    Ok(slice(data, ptr + 4, len)?.to_vec())
}

fn read_wbytes_bounded(data: &[u8], ptr: i32, max_bytes: usize) -> Result<Vec<u8>> {
    let len_bytes = slice(data, ptr, 4)?;
    let len = i32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]);
    let len = usize::try_from(len)
        .map_err(|_| Error::msg("Witchy byte-export returned a negative length"))?;
    if len > max_bytes {
        bail!(
            "Witchy byte-export returned {len} bytes, over the {max_bytes}-byte limit"
        );
    }
    let start = ptr
        .checked_add(4)
        .ok_or_else(|| Error::msg("Witchy byte-export pointer overflow"))?;
    let len = i32::try_from(len)
        .map_err(|_| Error::msg("Witchy byte-export length exceeds i32::MAX"))?;
    Ok(slice(data, start, len)?.to_vec())
}

/// Read a guest `List(Bytes)` — same `[count][i64 ptr…]` layout as `List(String)`, but
/// each element is read as raw bytes.
fn read_wbytes_list(data: &[u8], ptr: i32) -> Result<Vec<Vec<u8>>> {
    let len_bytes = slice(data, ptr, 4)?;
    let len = i32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]);
    let mut out = Vec::with_capacity((len.max(0) as usize).min(data.len() / 8));
    for i in 0..len {
        let slot = slice(data, ptr + 4 + 8 * i, 8)?;
        let elem = i64::from_le_bytes([
            slot[0], slot[1], slot[2], slot[3], slot[4], slot[5], slot[6], slot[7],
        ]);
        out.push(read_wbytes(data, elem as i32)?);
    }
    Ok(out)
}

/// (RFC-0023) Record a checked allocation and poison its trailing redzone. The guest's
/// checked allocators call this right after writing an object spanning `[start,end)`;
/// the host fills `[end, end+HEAP_REDZONE)` with `HEAP_POISON` and remembers the object
/// so [`Vm::heap_sweep`] can later prove the redzone was never overwritten. Out-of-range
/// arguments are ignored — this is a debug oracle, never an authority boundary.
fn host_heap_register(mut caller: Caller<'_, VmState>, start: i32, end: i32) -> Result<()> {
    if start < 0 || end < start {
        return Ok(());
    }
    let (s, e) = (start as u32, end as u32);
    let mem = memory_of(&mut caller)?;
    let rz_start = e as usize;
    let rz_end = rz_start + HEAP_REDZONE;
    let data = mem.data_mut(&mut caller);
    if rz_end <= data.len() {
        data[rz_start..rz_end].fill(HEAP_POISON);
    }
    caller.data_mut().heap_objects.push((s, e));
    Ok(())
}

/// Retire the checked-heap entry whose object begins at `start`. This is exact:
/// higher adjacent objects remain protected, unlike a watermark/frontier reclaim.
/// Removing an unknown, negative, or already-retired start is an idempotent no-op.
fn host_heap_unregister(mut caller: Caller<'_, VmState>, start: i32) -> Result<()> {
    if start < 0 {
        return Ok(());
    }
    let start = start as u32;
    caller.data_mut().heap_objects.retain(|&(object_start, _)| object_start != start);
    Ok(())
}

/// (RFC-0023) Reset-aware reclaim: drop every registered object whose redzone reaches
/// `addr`, since the space at/above `addr` is about to be legitimately reused. Two
/// callers, both passing the low edge of the reclaimed range: the checked `$ensure`
/// passes the current `$heap` (so reuse by *any* allocator after a `$heap = wm` reset is
/// covered — even uninstrumented ones — with no codegen hook on the reset); the
/// `region:` pointer copy-out passes its watermark `wm` *before* sliding the result down
/// over the body's allocations (a raw `memory.copy` that bypasses `$ensure`). The
/// object's body may still be live below `addr`; we only stop guarding its redzone.
fn host_heap_frontier(mut caller: Caller<'_, VmState>, addr: i32) -> Result<()> {
    let frontier = addr.max(0) as u32;
    // Keep only objects whose entire redzone lies strictly below the frontier; evict any
    // whose redzone `[oe, oe+rz)` reaches the frontier, since a write at/above it will
    // legitimately reuse that space.
    caller
        .data_mut()
        .heap_objects
        .retain(|&(_, oe)| oe as u64 + HEAP_REDZONE as u64 <= frontier as u64);
    Ok(())
}

fn memory_of(caller: &mut Caller<'_, VmState>) -> Result<Memory> {
    match caller.get_export("memory") {
        Some(Extern::Memory(m)) => Ok(m),
        _ => Err(Error::msg("VM does not export a linear `memory`")),
    }
}

/// Read a canonical host-call `Read Slice` — the raw `(ptr, len)` bytes the
/// caller handed us, NOT a length-prefixed witchy string object. The canonical
/// ABI types these slices as strings, so invalid UTF-8 is a contract violation
/// and traps here rather than being lossily repaired the way `read_wstr`
/// repairs the frozen route's already-checked objects.
fn read_slice_str(data: &[u8], ptr: i32, len: i32) -> Result<String> {
    if ptr < 0 || len < 0 {
        return Err(Error::msg(format!("negative host slice ({ptr}, {len})")));
    }
    let bytes = slice(data, ptr, len)?;
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|e| Error::msg(format!("host slice is not valid UTF-8: {e}")))
}

/// Fill a canonical host-call `Write Slice`: write `min(payload, out_cap)` bytes
/// at `out_ptr` and report the payload's FULL length. Returning the full length
/// is how the guest learns its buffer was short and calls again with a bigger
/// one; bounding the copy by the capacity the guest declared is why the host
/// never writes past a buffer the guest owns. `Memory::write` bounds-checks the
/// copy against linear memory, so a capacity that does not fit traps.
fn fill_slice(
    caller: &mut Caller<'_, VmState>,
    out_ptr: i32,
    out_cap: i32,
    bytes: &[u8],
    what: &str,
) -> Result<i32> {
    if out_ptr < 0 || out_cap < 0 {
        return Err(Error::msg(format!("negative host out slice ({out_ptr}, {out_cap})")));
    }
    let full = i32::try_from(bytes.len()).map_err(|_| {
        Error::msg(format!("{what} is {} bytes, over the guest ABI size limit", bytes.len()))
    })?;
    let written = bytes.len().min(out_cap as usize);
    let mem = memory_of(caller)?;
    // The declared capacity must lie inside guest memory, not only the
    // written prefix (the contract's rule; a short payload does not excuse an
    // out-of-range slice).
    let end = (out_ptr as usize).checked_add(out_cap as usize).ok_or_else(|| Error::msg("host out slice overflows"))?;
    if end > mem.data_size(&*caller) {
        return Err(Error::msg(format!("host out slice ({out_ptr}, {out_cap}) lies outside guest memory")));
    }
    mem.write(&mut *caller, out_ptr as usize, &bytes[..written])
        .map_err(|e| Error::msg(format!("writing {what} into guest memory: {e}")))?;
    Ok(full)
}

fn slice(data: &[u8], ptr: i32, len: i32) -> Result<&[u8]> {
    let start = ptr as usize;
    let end = start
        .checked_add(len as usize)
        .ok_or_else(|| Error::msg("pointer + length overflows"))?;
    data.get(start..end)
        .ok_or_else(|| Error::msg(format!("out-of-bounds guest memory access ({start}..{end})")))
}

#[cfg(feature = "self-checks")]
#[doc(hidden)]
#[path = "runtime_tests.rs"]
pub mod tests;

/// Every former in-crate test module of the sandbox, re-exported for the
/// one-line drivers in `tests/runtime.rs` (the crate has no lib test target).
#[cfg(feature = "self-checks")]
#[doc(hidden)]
pub mod self_checks {
    pub use super::compiler::doc_tests as compiler_doc_tests;
    pub use super::compiler::tests as compiler_tests;
    pub use super::host::build::tests as host_build_tests;
    pub use super::host::build_env::tests as host_build_env_tests;
    pub use super::host::build_exec::tests as host_build_exec_tests;
    pub use super::host::build_net::tests as host_build_net_tests;
    pub use super::host::build_out::tests as host_build_out_tests;
    pub use super::host::build_read::tests as host_build_read_tests;
    #[cfg(feature = "test-fixtures")]
    pub use super::host::canonical_fixture::tests as host_canonical_fixture_tests;
    pub use super::host::clock_env_rand::tests as host_clock_env_rand_tests;
    pub use super::host::closure_owner::tests as host_closure_owner_tests;
    pub use super::host::console::canonical_console_tests as host_console_canonical_console_tests;
    pub use super::host::crypto::extensions::tests as host_crypto_extensions_tests;
    pub use super::host::crypto::hash_slices_tests as host_crypto_hash_slices_tests;
    pub use super::host::crypto::rsa_slices_tests as host_crypto_rsa_slices_tests;
    pub use super::host::crypto::slices_tests as host_crypto_slices_tests;
    pub use super::host::fetch::tests as host_fetch_tests;
    #[cfg(unix)]
    pub use super::host::filesystem::canonical_exec::tests as host_filesystem_canonical_exec_tests;
    pub use super::host::filesystem::dir_installer_tests as host_filesystem_dir_installer_tests;
    pub use super::host::filesystem::dir_storage_tests as host_filesystem_dir_storage_tests;
    pub use super::host::filesystem::metadata_tests as host_filesystem_metadata_tests;
    pub use super::host::filesystem::tests as host_filesystem_tests;
    pub use super::host::network::tests as host_network_tests;
    pub use super::host::regex::tests as host_regex_tests;
    pub use super::host::secret::registry_tests as host_secret_registry_tests;
    pub use super::host::vm_worker::server_startup_tests as host_vm_worker_server_startup_tests;
    pub use super::host::vm_worker::vm_service::tests as host_vm_worker_vm_service_tests;
    pub use super::launch_arguments::tests as launch_arguments_tests;
    pub use super::tests;
}
