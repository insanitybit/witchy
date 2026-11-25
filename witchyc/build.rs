use std::{env, path::{Path, PathBuf}, process::Command};

// Build the fixed runtime once, then embed its objects in witchyc.
fn main() {
    println!("cargo:rustc-check-cfg=cfg(kani)");

    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("aarch64") {
        return;
    }
    let macos = match env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") => true,
        Ok("linux") if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu") => false,
        _ => return,
    };
    println!("cargo:rustc-check-cfg=cfg(witchy_native_aws_lc)");
    println!("cargo:rerun-if-env-changed=WITCHY_NATIVE_AWS_LC_ROOT");
    let aws_lc = env::var_os("WITCHY_NATIVE_AWS_LC_ROOT").map(|root| {
        let root = PathBuf::from(root);
        assert!(
            root.is_absolute(),
            "AWS-LC installation must be an absolute path"
        );
        let root = root.canonicalize().expect("AWS-LC installation");
        let library = root.join(if macos {
            "lib/libaws_lc_fips_0_14_4_crypto.dylib"
        } else {
            "lib/libaws_lc_fips_0_14_4_crypto.a"
        });
        assert!(library.is_file(), "missing pinned AWS-LC library");
        println!("cargo:rerun-if-changed={}", library.display());
        println!("cargo:rerun-if-changed={}", root.join("include").display());
        println!("cargo:rustc-cfg=witchy_native_aws_lc");
        println!(
            "cargo:rustc-env=WITCHY_NATIVE_AWS_LC_INSTALLED={}",
            root.display()
        );
        root
    });
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"));
    println!("cargo:rerun-if-changed=runtime/native");
    println!("cargo:rerun-if-changed=src/compiler/exec_env_policy.rs");
    println!("cargo:rerun-if-changed=src/compiler/pkix.rs");
    println!("cargo:rerun-if-changed=src/compiler/pkix/ffi.rs");
    println!("cargo:rerun-if-changed=src/compiler/tls_signature.rs");
    println!("cargo:rerun-if-changed=src/compiler/exec_env_refused.txt");
    println!("cargo:rustc-check-cfg=cfg(witchy_native_sanitized)");
    println!("cargo:rerun-if-env-changed=WITCHY_NATIVE_SANITIZER_RUSTC");
    let sanitizer_rustc = env::var_os("WITCHY_NATIVE_SANITIZER_RUSTC");
    println!("cargo:rerun-if-env-changed=WITCHY_NATIVE_SANITIZER_CLANG");
    let sanitizer_clang = env::var_os("WITCHY_NATIVE_SANITIZER_CLANG");
    println!("cargo:rerun-if-env-changed=WITCHY_NATIVE_UB_CHECKS");
    let ub_checks = env::var_os("WITCHY_NATIVE_UB_CHECKS");
    assert!(
        ub_checks.is_none() || ub_checks.as_deref() == Some(std::ffi::OsStr::new("1")),
        "WITCHY_NATIVE_UB_CHECKS must be 1 when set"
    );
    assert_eq!(
        sanitizer_rustc.is_some(),
        sanitizer_clang.is_some(),
        "configure both native sanitizer compilers"
    );
    let mut rustc = if let Some(path) = sanitizer_rustc {
        assert!(macos, "native sanitizer mode requires macOS ARM64");
        println!("cargo:rustc-cfg=witchy_native_sanitized");
        println!("cargo:rustc-env=WITCHY_NATIVE_SANITIZER_RUSTC={}", PathBuf::from(&path).display());
        let mut command = Command::new(path);
        command.args(["-Zsanitizer=address", "-Cdebuginfo=1"]);
        command
    } else {
        Command::new(env::var_os("RUSTC").expect("Cargo Rust compiler"))
    };
    if ub_checks.is_some() {
        rustc.args(["-Zub-checks=yes", "--cfg", "witchy_native_ub_checks"]);
    }
    // The runtime's bounds checks and shared parsing code reference core.
    // Bundle its support once; the native linker strips unreachable sections.
    let runtime_archive = "libwitchy_runtime.a";
    rustc.args(["--crate-type=staticlib"]);
    rustc
        .args([
            "--edition=2024",
            "-D",
            "clashing-extern-declarations",
            "-C",
            "opt-level=2",
            "-C",
            "panic=abort",
        ])
        .arg("--target")
        .arg(env::var_os("TARGET").expect("Cargo target"))
        .arg("runtime/native/runtime_abi.rs");
    let runtime_path = out.join(runtime_archive);
    let plain_path = if macos && aws_lc.is_some() {
        let mut plain = Command::new(rustc.get_program());
        plain.args(rustc.get_args());
        let plain_path = out.join("libwitchy_runtime_plain.a");
        compile_runtime(&mut plain, &plain_path);
        Some(plain_path)
    } else {
        None
    };
    if aws_lc.is_some() {
        rustc.args(["--cfg", "witchy_native_aws_lc"]);
    }
    compile_runtime(&mut rustc, &runtime_path);
    if let Some(plain_path) = plain_path {
        share_archives(&plain_path, &runtime_path, &out);
    }
    println!("cargo:rustc-env=WITCHY_NATIVE_ARCHIVE_FILE={runtime_archive}");
    if let Some(path) = sanitizer_clang.as_ref() {
        println!(
            "cargo:rustc-env=WITCHY_NATIVE_SANITIZER_CLANG={}",
            PathBuf::from(path).display()
        );
    }
}

fn compile_runtime(command: &mut Command, output: &Path) {
    let status = command.arg("-o").arg(output).status().expect("start Rust compiler for native Rust runtime");
    assert!(status.success(), "compile native Rust runtime: {status}");
}

fn share_archives(plain: &Path, provider: &Path, out: &Path) {
    let plain = std::fs::read(plain).expect("read provider-free runtime archive");
    let provider = std::fs::read(provider).expect("read provider runtime archive");
    // Rust bundles identical support objects at the end of both archives.
    // Keep the provider archive whole and recover the other archive byte for byte.
    let shared = plain.iter().rev().zip(provider.iter().rev()).take_while(|(a, b)| a == b).count();
    std::fs::write(out.join("runtime_plain_prefix"), &plain[..plain.len() - shared])
        .expect("write provider-free archive prefix");
    std::fs::write(out.join("runtime_shared_suffix_len"), (shared as u64).to_le_bytes())
        .expect("write shared archive suffix length");
}
