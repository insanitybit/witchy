//! Browser-host parity for the complete compiled encoding ABI (BUG-158).

#[test]
fn browser_encoding_abi_matches_native() {
    super::run_node_driver(
        "web/witchy-runtime/encoding-abi.test.mjs",
        &[super::BIN],
        "ENCODING-ABI OK",
        "encoding ABI",
    );
}
