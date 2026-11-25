#[test]
fn actual_test_environment() {
    println!("MANIFEST\t{}", env!("CARGO_MANIFEST_DIR"));
    for name in ["DYLD_FALLBACK_LIBRARY_PATH", "DYLD_LIBRARY_PATH", "DYLD_INSERT_LIBRARIES",
                 "LEAN_PATH", "WITCHY_LEAN_BIN"] {
        println!("ENV\t{name}\t{}", std::env::var_os(name).map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| "<unset>".into()));
    }
}
