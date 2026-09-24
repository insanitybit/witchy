//! Directory metadata, including captured authority and aliases.

#[test]
fn canonical_dir_metadata_source_requires_read_right() {
    let source = "fn main(root: Dir[Write]):\n    let value = root.is_dir(\"nested\")\n    ()\n";
    let error = witchy::resolve_std_only_checked(source).err().expect("Write does not imply Read").to_string();
    assert!(error.contains("Read"), "{error}");
}
