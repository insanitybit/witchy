//! Where the engine finds AWS-LC's FIPS module when it is a shared library (macOS builds the
//! FIPS module dynamic; Linux links it statically): beside the engine only. The module is
//! copied there from the build (into the profile directory and `deps`, for the engine and its
//! tests); a release snapshot ships it beside the binary. No rpath points into a build
//! directory (review: whoever could write there would supply the "FIPS module"). The
//! directory holding the engine must not be writable by others.
fn main() {
    println!("cargo:rerun-if-env-changed=DEP_AWS_LC_FIPS_0_14_4_LIBDIR");
    println!("cargo:rerun-if-env-changed=DEP_AWS_LC_FIPS_0_14_4_LIBCRYPTO_PATH");
    if std::env::var("DEP_AWS_LC_FIPS_0_14_4_LINK_KIND").as_deref() != Ok("dylib") { return; }
    if std::env::var("CARGO_CFG_TARGET_VENDOR").as_deref() == Ok("apple") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path");
    } else {
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
    }
    let (Ok(library), Ok(out)) = (std::env::var("DEP_AWS_LC_FIPS_0_14_4_LIBCRYPTO_PATH"), std::env::var("OUT_DIR")) else { return };
    let library = std::path::PathBuf::from(library);
    // OUT_DIR is PROFILE/build/PACKAGE-HASH/out, or PROFILE/build/PACKAGE/HASH/out.
    let Some(profile) = std::path::Path::new(&out).ancestors().find(|dir| dir.file_name().is_some_and(|name| name == "build")).and_then(|build| build.parent()) else { return };
    for dir in [profile.to_path_buf(), profile.join("deps")] {
        if let Some(name) = library.file_name() { let _ = std::fs::copy(&library, dir.join(name)); }
    }
}
