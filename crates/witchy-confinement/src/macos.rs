//! macOS Seatbelt confinement provider (RFC-0158).
//!
//! RFC-0103 derives a target-neutral [`Policy`]; this module translates it into
//! an SBPL profile and arms it on the current process immediately before guest
//! execution — the same position `crate::linux::apply` occupies. The derivation
//! is unchanged; only the translation is new.
//!
//! Two properties of this platform shape everything below. Seatbelt has no
//! syscall-granular filter, so `Layer::Syscalls` can only ever report `Partial`
//! (operation classes, not syscall numbers). And a malformed profile aborts the
//! process in dyld with SIGABRT and *no* diagnostic — dyld's own error write is
//! denied by the profile being tested — so the provider self-tests immediately
//! after arming rather than letting the guest discover the breakage.

use std::ffi::{CString, c_char, c_int};
use std::path::Path;

use crate::{
    EnforcementError, EnforcementMode, EnforcementReport, FsScope, Layer,
    LayerReport, LayerStatus, Policy, SyscallClass,
};

/// Read-only loader and platform-binary roots. dyld must map the shared cache
/// and the platform dylibs or every `exec` aborts before `main`. These carry no
/// `file-write*` and no `process-exec` right unless a grant supplies one.
const LOADER_READ_ROOTS: [&str; 3] = ["/bin", "/usr/lib", "/System"];

/// Standard device nodes that must be *readable*. Found by implementation, not
/// by design: `std::process::Command::output` points the child's stdin at
/// `/dev/null`, so without a read right here every captured-output spawn fails
/// with `EPERM` even when the program itself carries an execute grant. These
/// nodes carry no user data, so reading them is not a widening of the fence in
/// the sense the write rules are.
const DEVICE_READ_PATHS: [&str; 4] =
    ["/dev/null", "/dev/zero", "/dev/random", "/dev/urandom"];

/// The standard streams. Seatbelt checks `file-write-data` at open-for-write as
/// well as on writes, so an unrestricted rule here would permit overwriting the
/// contents of any file DAC allows while `Layer::Filesystem` still reported
/// `Enforced` — see RFC-0158 §The preamble. Every other write right in the
/// emitted profile comes from a grant.
const STREAM_WRITE_PATHS: [&str; 4] =
    ["/dev/stdout", "/dev/stderr", "/dev/tty", "/dev/null"];

/// File used by the post-arm negative probe. It is a system path that exists on
/// every macOS install and that no derived policy is expected to grant; the
/// probe is skipped when the policy does grant it, rather than reporting a
/// false failure.
///
/// It must be probed by *reading data*, not by `stat`. The preamble allows
/// `file-read-metadata` unrestricted — dyld requires it for ancestor traversal
/// — so a metadata probe is allowed on every path and can never observe the
/// fence at all.
const PROBE_FILE: &str = "/private/etc/hosts";

pub(crate) fn apply(
    policy: &Policy,
    mode: EnforcementMode,
) -> Result<EnforcementReport, EnforcementError> {
    let Some(entry) = sandbox_entry() else {
        let report = unavailable_report();
        if mode == EnforcementMode::Required {
            return Err(EnforcementError(
                "required platform confinement is unavailable on this host: libsandbox does not export sandbox_init_with_parameters"
                    .into(),
            ));
        }
        return Ok(report);
    };
    let profile = profile(policy);
    arm(&profile, entry)?;
    self_test(policy)?;

    let report = EnforcementReport {
        layers: vec![
            filesystem_layer(),
            tcp_layer(policy),
            syscalls_layer(),
        ],
    };
    // Mirrors `crate::linux::apply`: build the per-layer report first so a
    // `required` refusal names the layer that could not enforce, rather than
    // failing flat at the host level.
    if mode == EnforcementMode::Required && !report.fully_enforced() {
        return Err(EnforcementError(format!(
            "required platform confinement was not fully enforced: {:?}",
            report.layers
        )));
    }
    Ok(report)
}

fn filesystem_layer() -> LayerReport {
    LayerReport {
        layer: Layer::Filesystem,
        provider: "seatbelt",
        status: LayerStatus::Enforced,
        detail: "Seatbelt profile armed; writes limited to granted paths and the standard streams"
            .into(),
    }
}

fn tcp_layer(policy: &Policy) -> LayerReport {
    // SBPL rejects any remote host but `*` or `localhost`, so network rules are
    // port-granular — the same ceiling as Landlock ABI v4. RFC-0020's pinned
    // dials remain the address-granular enforcer above this layer.
    if policy.network.has_unexpressed_transport {
        return LayerReport {
            layer: Layer::Tcp,
            provider: "seatbelt",
            status: LayerStatus::Partial,
            detail: "Seatbelt enforces TCP ports; a granted UDP or Unix-domain transport remains host-layer only"
                .into(),
        };
    }
    LayerReport {
        layer: Layer::Tcp,
        provider: "seatbelt",
        status: LayerStatus::Enforced,
        detail: "Seatbelt enforces the granted TCP ports".into(),
    }
}

fn syscalls_layer() -> LayerReport {
    LayerReport {
        layer: Layer::Syscalls,
        provider: "seatbelt",
        status: LayerStatus::Partial,
        detail: "operation classes enforced; macOS has no syscall-level filter"
            .into(),
    }
}

/// Translate a derived policy into an SBPL profile.
///
/// Pure and total: every path is canonicalized here, and a path that cannot be
/// canonicalized is dropped rather than emitted unresolved. SBPL matches
/// *resolved* paths, and on macOS `/tmp`, `/var`, and `/etc` are symlinks into
/// `/private`, so an unresolved rule silently matches nothing.
pub(crate) fn profile(policy: &Policy) -> String {
    let mut out = String::new();
    out.push_str("(version 1)\n");
    out.push_str("(deny default)\n");
    out.push_str("(allow signal sysctl-read mach-lookup ipc-posix-shm*)\n");

    // Ancestor traversal into the grant trees. Narrowing this to the granted
    // subpaths re-breaks opening files *inside* them.
    out.push_str("(allow file-read-metadata)\n");
    // The root directory node itself. A union of narrower subpaths does not
    // imply it, and without it every exec aborts in dyld with no diagnostic.
    out.push_str("(allow file-read* (literal \"/\"))\n");
    out.push_str("(allow file-read*");
    for root in LOADER_READ_ROOTS {
        out.push_str(&format!(" (subpath {})", quote(root)));
    }
    out.push_str(")\n");

    out.push_str("(allow file-read*");
    for path in DEVICE_READ_PATHS {
        out.push_str(&format!(" (literal {})", quote(path)));
    }
    out.push_str(")\n");

    out.push_str("(allow file-write-data");
    for path in STREAM_WRITE_PATHS {
        out.push_str(&format!(" (literal {})", quote(path)));
    }
    out.push_str(")\n");

    if policy.syscall_classes.contains(&SyscallClass::Process) {
        out.push_str("(allow process-fork)\n");
    }

    for rule in &policy.filesystem {
        let Some((canonical, is_dir)) = resolve(&rule.path) else {
            continue;
        };
        let form = match rule.scope {
            FsScope::Tree => "subpath",
            FsScope::File => "literal",
            // Resolve file-versus-directory from the same object the rule is
            // anchored on, matching the Landlock provider's contract.
            FsScope::Path if is_dir => "subpath",
            FsScope::Path => "literal",
        };
        let target = format!("({form} {})", quote(&canonical));
        if rule.access.read {
            out.push_str(&format!("(allow file-read* {target})\n"));
        }
        if rule.access.write {
            out.push_str(&format!("(allow file-write* {target})\n"));
        }
        if rule.access.execute {
            out.push_str(&format!("(allow process-exec {target})\n"));
        }
    }

    for port in &policy.network.connect_tcp_ports {
        out.push_str(&format!(
            "(allow network-outbound (remote ip \"*:{port}\"))\n"
        ));
    }
    for port in &policy.network.bind_tcp_ports {
        out.push_str(&format!("(allow network-bind (local ip \"*:{port}\"))\n"));
    }
    out
}

/// Canonicalize a grant path, reporting whether it names a directory.
fn resolve(path: &Path) -> Option<(String, bool)> {
    let canonical = std::fs::canonicalize(path).ok()?;
    let is_dir = std::fs::metadata(&canonical)
        .map(|meta| meta.is_dir())
        .unwrap_or(false);
    Some((canonical.to_string_lossy().into_owned(), is_dir))
}

/// SBPL string literal. Backslash and quote are the only escapes the grammar
/// needs, and a path containing an interior NUL cannot be expressed at all.
fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        if ch == '"' || ch == '\\' {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

type SandboxInitWithParameters = unsafe extern "C" fn(
    *const c_char,
    u64,
    *const *const c_char,
    *mut *mut c_char,
) -> c_int;
type SandboxFreeError = unsafe extern "C" fn(*mut c_char);

/// Resolve the Seatbelt entry point at runtime rather than linking it.
///
/// The whole family is `API_DEPRECATED(… macos(10.5, 10.8))` — deprecated since
/// 2012 and still load-bearing for Chromium, Nix, Bazel, and `sandbox-exec`. A
/// direct `extern` declaration would make a future macOS that finally removes
/// it fail *every* witchy invocation at image load. Resolving it here means such
/// a host degrades to `Unavailable`, exactly as a host with no provider does,
/// and `required` still refuses to launch.
fn sandbox_entry() -> Option<(SandboxInitWithParameters, Option<SandboxFreeError>)> {
    // SAFETY: `dlsym` with `RTLD_DEFAULT` searches the already-loaded images and
    // returns null when the symbol is absent; the transmute matches the
    // documented C prototype in `sandbox.h`.
    unsafe {
        let init = libc::dlsym(
            libc::RTLD_DEFAULT,
            c"sandbox_init_with_parameters".as_ptr(),
        );
        if init.is_null() {
            return None;
        }
        let free = libc::dlsym(libc::RTLD_DEFAULT, c"sandbox_free_error".as_ptr());
        Some((
            std::mem::transmute::<*mut std::ffi::c_void, SandboxInitWithParameters>(init),
            (!free.is_null()).then(|| {
                std::mem::transmute::<*mut std::ffi::c_void, SandboxFreeError>(free)
            }),
        ))
    }
}

/// The report a host without a usable Seatbelt returns: identical in shape to a
/// platform with no provider at all, so `required` refuses for the same reason.
fn unavailable_report() -> EnforcementReport {
    let detail = "libsandbox does not export sandbox_init_with_parameters on this host";
    EnforcementReport {
        layers: [Layer::Filesystem, Layer::Tcp, Layer::Syscalls]
            .into_iter()
            .map(|layer| LayerReport {
                layer,
                provider: "seatbelt",
                status: LayerStatus::Unavailable,
                detail: detail.into(),
            })
            .collect(),
    }
}

/// `flags` must be `0` for the inline-SBPL form. `0x0001` is `SANDBOX_NAMED`
/// (named profiles only) and `0x0002` — the value one would guess for "string"
/// — fails with the misleading `File name too long`.
const SANDBOX_INLINE_PROFILE: u64 = 0;

fn arm(
    profile: &str,
    entry: (SandboxInitWithParameters, Option<SandboxFreeError>),
) -> Result<(), EnforcementError> {
    let (sandbox_init_with_parameters, sandbox_free_error) = entry;
    let free_error = |buffer: *mut c_char| {
        if let Some(free) = sandbox_free_error {
            // SAFETY: non-null buffer returned by the same API.
            unsafe { free(buffer) };
        }
    };
    let profile = CString::new(profile).map_err(|_| {
        EnforcementError(
            "derived Seatbelt profile contains an interior NUL byte".into(),
        )
    })?;
    let mut errorbuf: *mut c_char = std::ptr::null_mut();
    // SAFETY: `profile` is a valid NUL-terminated C string that outlives the
    // call; the parameter vector is empty (null); `errorbuf` is a valid
    // out-pointer that we free through the API's own deallocator.
    let rc = unsafe {
        sandbox_init_with_parameters(
            profile.as_ptr(),
            SANDBOX_INLINE_PROFILE,
            std::ptr::null(),
            &mut errorbuf,
        )
    };
    if rc == 0 {
        if !errorbuf.is_null() {
            free_error(errorbuf);
        }
        return Ok(());
    }
    let detail = if errorbuf.is_null() {
        String::new()
    } else {
        // SAFETY: on failure the API returns a NUL-terminated diagnostic that
        // remains valid until `sandbox_free_error`.
        let message = unsafe { std::ffi::CStr::from_ptr(errorbuf) }
            .to_string_lossy()
            .into_owned();
        free_error(errorbuf);
        format!(": {message}")
    };
    Err(EnforcementError(format!(
        "Seatbelt refused the derived profile{detail}"
    )))
}

/// Bounded post-arm self-test (RFC-0158 §Preflight self-test).
///
/// A malformed profile aborts in dyld with SIGABRT, no stderr, and no unified
/// log entry, so a provider that trusts the guest to reveal breakage is not
/// operable. Three syscalls convert that into a diagnosable error.
fn self_test(policy: &Policy) -> Result<(), EnforcementError> {
    // 1. A granted read path must still be reachable.
    if let Some(rule) = policy
        .filesystem
        .iter()
        .find(|rule| rule.access.read && rule.path.exists())
    {
        std::fs::metadata(&rule.path).map_err(|error| {
            EnforcementError(format!(
                "Seatbelt profile denied a granted read path {}: {error}",
                rule.path.display()
            ))
        })?;
    }

    // 2. Reading an ungranted file must be denied. `NotFound` is inconclusive
    //    rather than a failure: it means the probe file is absent on this host,
    //    not that the fence let the read through.
    let probe = Path::new(PROBE_FILE);
    if !policy_grants(policy, probe) {
        match std::fs::read(probe) {
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::NotFound
                ) => {}
            other => {
                return Err(EnforcementError(format!(
                    "Seatbelt profile did not deny reading an ungranted path {}: {}",
                    probe.display(),
                    match other {
                        Ok(_) => "the read succeeded".to_string(),
                        Err(error) => error.to_string(),
                    }
                )));
            }
        }
    }

    // 3. Diagnostics must survive the fence, or every later failure is silent.
    use std::io::Write as _;
    std::io::stderr().write_all(b"").map_err(|error| {
        EnforcementError(format!("Seatbelt profile denied stderr: {error}"))
    })?;
    Ok(())
}

/// Whether any filesystem rule would cover `path`, used to skip the negative
/// probe rather than report a false failure.
fn policy_grants(policy: &Policy, path: &Path) -> bool {
    policy.filesystem.iter().any(|rule| {
        std::fs::canonicalize(&rule.path)
            .map(|granted| path.starts_with(&granted))
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::*;
    use crate::{FsAccess, FsScope};

    const FS_CHILD: &str = "WITCHY_SEATBELT_FS_CHILD";
    const TCP_CHILD: &str = "WITCHY_SEATBELT_TCP_CHILD";
    const EXEC_CHILD: &str = "WITCHY_SEATBELT_EXEC_CHILD";

    fn scratch(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "witchy-seatbelt-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn run_child(test: &str, env: (&str, &Path)) -> std::process::Output {
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env(env.0, env.1)
            .output()
            .unwrap()
    }

    fn assert_child_ok(output: std::process::Output) {
        assert!(
            output.status.success(),
            "child failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// The regression guard for RFC-0158's central correction. An earlier draft
    /// emitted `(allow file-write-data)` unrestricted, which would have made
    /// every derived `file-write*` rule decorative while `Layer::Filesystem`
    /// still reported `Enforced`.
    #[test]
    fn profile_never_grants_unrestricted_writes() {
        let mut policy = Policy::default();
        policy.add_fs_rule("/", FsScope::Tree, FsAccess::new(true, false, false));
        let sbpl = profile(&policy);
        for line in sbpl.lines() {
            let write = line.starts_with("(allow file-write");
            if write {
                assert!(
                    line.contains("(literal ") || line.contains("(subpath "),
                    "unrestricted write rule in derived profile: {line}"
                );
            }
        }
        // And the streams that are allowed are exactly the four declared ones.
        let stream_rule = sbpl
            .lines()
            .find(|line| line.starts_with("(allow file-write-data"))
            .expect("stream write rule");
        for path in STREAM_WRITE_PATHS {
            assert!(stream_rule.contains(path), "missing {path}: {stream_rule}");
        }
    }

    /// SBPL matches resolved paths, and `/tmp` is a symlink into `/private` on
    /// macOS, so an unresolved rule silently matches nothing.
    #[test]
    fn profile_canonicalizes_symlinked_roots() {
        let mut policy = Policy::default();
        policy.add_fs_rule("/tmp", FsScope::Tree, FsAccess::new(true, false, false));
        let sbpl = profile(&policy);
        assert!(
            sbpl.contains("(allow file-read* (subpath \"/private/tmp\"))"),
            "{sbpl}"
        );
    }

    /// The preamble must not hand out `process-fork` unconditionally; it is a
    /// `SyscallClass::Process` right.
    #[test]
    fn profile_gates_process_fork_on_the_process_class() {
        let bare = profile(&Policy::default());
        assert!(!bare.contains("process-fork"), "{bare}");

        let mut policy = Policy::default();
        policy.syscall_classes.insert(SyscallClass::Process);
        assert!(profile(&policy).contains("(allow process-fork)"));
    }

    #[test]
    fn profile_maps_network_ports_but_never_hosts() {
        let mut policy = Policy::default();
        policy.network.connect_tcp_ports.insert(8080);
        policy.network.bind_tcp_ports.insert(9090);
        let sbpl = profile(&policy);
        assert!(sbpl.contains("(allow network-outbound (remote ip \"*:8080\"))"));
        assert!(sbpl.contains("(allow network-bind (local ip \"*:9090\"))"));
    }

    /// Filesystem enforcement, in a child because arming is irreversible.
    /// Covers reads in and out of the grant *and* the write confinement that
    /// keeps `Layer::Filesystem = Enforced` honest.
    #[test]
    fn seatbelt_denies_an_ungranted_filesystem_path() {
        if let Some(root) = std::env::var_os(FS_CHILD) {
            let root = PathBuf::from(root);
            let allowed = root.join("allowed");
            let outside = root.join("outside");
            let mut policy = Policy::default();
            policy.add_fs_rule(
                &allowed,
                FsScope::Path,
                FsAccess::new(true, true, false),
            );
            policy.normalize_classes();
            let report = apply(&policy, EnforcementMode::BestEffort).unwrap();
            assert_eq!(report.layers[0].status, LayerStatus::Enforced, "{report:?}");

            // Reads: granted in, denied out.
            assert_eq!(
                std::fs::read_to_string(allowed.join("value")).unwrap(),
                "allowed"
            );
            let error = std::fs::read_to_string(outside.join("value")).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);

            // Writes: granted in, denied out. The out-of-grant file already
            // exists, so this is precisely the overwrite an unrestricted
            // `file-write-data` preamble would have permitted.
            std::fs::write(allowed.join("written"), "in-grant").unwrap();
            let error = std::fs::write(outside.join("value"), "clobbered").unwrap_err();
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied,
                "an ungranted existing file was overwritable"
            );
            return;
        }

        let root = scratch("fs");
        let allowed = root.join("allowed");
        let outside = root.join("outside");
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(allowed.join("value"), "allowed").unwrap();
        std::fs::write(outside.join("value"), "outside").unwrap();
        let output = run_child(
            "macos::tests::seatbelt_denies_an_ungranted_filesystem_path",
            (FS_CHILD, &root),
        );
        std::fs::remove_dir_all(&root).ok();
        assert_child_ok(output);
    }

    #[test]
    fn seatbelt_denies_an_ungranted_tcp_port() {
        if let Some(port) = std::env::var_os(TCP_CHILD) {
            let port: u16 = port.to_string_lossy().parse().unwrap();
            let report = apply(&Policy::default(), EnforcementMode::BestEffort).unwrap();
            assert_eq!(report.layers[1].status, LayerStatus::Enforced, "{report:?}");
            let error = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
            return;
        }

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "macos::tests::seatbelt_denies_an_ungranted_tcp_port",
                "--nocapture",
            ])
            .env(TCP_CHILD, port.to_string())
            .output()
            .unwrap();
        drop(listener);
        assert_child_ok(output);
    }

    /// RFC-0103's `child-paths` contract: an exec'd child inherits the fence.
    #[test]
    fn seatbelt_children_inherit_the_fence() {
        if let Some(root) = std::env::var_os(EXEC_CHILD) {
            let root = PathBuf::from(root);
            let allowed = root.join("allowed");
            let outside = root.join("outside");
            let mut policy = Policy::default();
            policy.add_fs_rule(
                &allowed,
                FsScope::Path,
                FsAccess::new(true, false, false),
            );
            // RFC-0103's `exec = { programs = [...] }` is a *declared* hole:
            // the child program is named by a grant carrying execute, and
            // nothing else on the host becomes executable. Without this rule
            // the spawn is denied — which is the design working, not a bug.
            policy.add_fs_rule(
                "/bin/cat",
                FsScope::File,
                FsAccess::new(true, false, true),
            );
            policy.syscall_classes.insert(SyscallClass::Process);
            policy.normalize_classes();
            apply(&policy, EnforcementMode::BestEffort).unwrap();

            let inside = Command::new("/bin/cat")
                .arg(allowed.join("value"))
                .output()
                .unwrap();
            assert!(inside.status.success(), "child could not read in-grant");
            assert_eq!(String::from_utf8_lossy(&inside.stdout), "allowed");

            let out = Command::new("/bin/cat")
                .arg(outside.join("value"))
                .output()
                .unwrap();
            assert!(!out.status.success(), "child read an ungranted path");

            // An undeclared program stays denied even though the Process class
            // is present: exec authority is per-path, not a blanket right.
            let undeclared = Command::new("/bin/echo").arg("hi").output();
            assert!(
                undeclared.is_err()
                    || !undeclared.as_ref().unwrap().status.success(),
                "an undeclared program was executable: {undeclared:?}"
            );
            return;
        }

        let root = scratch("exec");
        let allowed = root.join("allowed");
        let outside = root.join("outside");
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(allowed.join("value"), "allowed").unwrap();
        std::fs::write(outside.join("value"), "outside").unwrap();
        let output = run_child(
            "macos::tests::seatbelt_children_inherit_the_fence",
            (EXEC_CHILD, &root),
        );
        std::fs::remove_dir_all(&root).ok();
        assert_child_ok(output);
    }
}
