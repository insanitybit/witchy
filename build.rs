// Surface cargo's host target triple (e.g. "aarch64-apple-darwin") as a compile-time
// env var. `witchy install` reads it via env!("WITCHY_HOST_TARGET") to default the
// `--target` flag to the host, so the common case needs no flag (RFC-0095 host-target
// auto-detection). `TARGET` is set by cargo for every build.
fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    println!("cargo:rustc-env=WITCHY_HOST_TARGET={target}");
}
