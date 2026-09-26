//! Directory metadata, including captured authority and aliases.

#[test]
fn canonical_dir_metadata_source_requires_read_right() {
    let source = "fn main(root: Dir[Write]):\n    let value = root.is_dir(\"nested\")\n    ()\n";
    let error = super::source::checked(source).expect_err("Write does not imply Read");
    assert!(error.contains("Read"), "{error}");
}
