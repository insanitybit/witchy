//! Stamp the build with a digest of this crate's sources.
//!
//! The SafeWIR checker's persistent verdict store (`canonical_safewir::check::cache`)
//! must never replay a verdict produced by a different checker. A git commit
//! id would miss uncommitted edits, and hashing the compiler binary at run
//! time would cost every test process tens of milliseconds, so the stamp is a
//! digest of every source file in this crate, computed here and embedded as
//! `WITCHY_WIR_SOURCE_DIGEST`. Any edit under `src/` changes it.

use std::fs;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=build.rs");
    let mut files = Vec::new();
    collect(Path::new("src"), &mut files);
    files.sort();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"witchy-wir/source-digest/v1\0");
    for path in files {
        let bytes = fs::read(&path).expect("crate source is readable");
        hasher.update(path.to_string_lossy().as_bytes());
        hasher.update(&[0]);
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    hasher.update(&fs::read("Cargo.toml").expect("crate manifest is readable"));
    println!("cargo:rustc-env=WITCHY_WIR_SOURCE_DIGEST={}", hasher.finalize().to_hex());
}

fn collect(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, files);
        } else {
            files.push(path);
        }
    }
}
