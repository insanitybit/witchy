//! Independent metadata for the oracle's unrestricted fixture grants. Unlike
//! the older read/exists fixture helpers, this predicate admits confined links
//! and '.' exactly as the unfiltered native metadata operation does. It does
//! not call the runtime host, its authority objects, or its path resolver.
use super::OracleFixture;
use std::{collections::VecDeque, ffi::OsString, path::{Component, Path, PathBuf}};

fn relative_parts(path: &str) -> Option<Vec<String>> {
    if path.contains('\0') { return None; }
    let mut result = Vec::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(name) => result.push(name.to_str()?.to_owned()),
            Component::CurDir => {},
            _ => return None,
        }
    }
    Some(result)
}

impl OracleFixture {
    pub(super) fn metadata_is_directory(&self, scope: &str, path: &str) -> bool {
        let Some(relative) = relative_parts(path) else { return false; };
        // A minted directory handle denotes itself, even an empty directory.
        if relative.is_empty() { return true; }
        let Some(mut pieces) = relative_parts(scope) else { return false; };
        for piece in &relative {
            pieces.push(piece.clone());
            if self.files.contains_key(&pieces.join("/")) { return false; }
        }
        let prefix = format!("{}/", pieces.join("/"));
        if self.files.keys().any(|name| name.starts_with(&prefix)) { return true; }
        let Some(root) = &self.root else { return false; };
        fixture_directory(&root.join(scope), relative)
    }
}

/// Resolve a stable fixture tree component by component. Absolute link targets
/// and relative targets crossing the grant root never acquire ambient meaning.
/// Link expansion limits match the platform primitive used by the native host;
/// the oracle keeps its own resolution and metadata implementation.
fn fixture_directory(base: &Path, parts: Vec<String>) -> bool {
    let mut pending: VecDeque<OsString> = parts.into_iter().map(OsString::from).collect();
    let mut resolved = PathBuf::new();
    let mut links = 0;
    let link_limit = if cfg!(windows) { 63 } else { 40 };
    while let Some(part) = pending.pop_front() {
        if part == "." { continue; }
        if part == ".." {
            if !resolved.pop() { return false; }
            continue;
        }
        let candidate = base.join(&resolved).join(&part);
        let Ok(metadata) = std::fs::symlink_metadata(&candidate) else { return false; };
        if metadata.file_type().is_symlink() {
            if links == link_limit { return false; }
            links += 1;
            let Ok(target) = std::fs::read_link(&candidate) else { return false; };
            let mut target_parts = Vec::new();
            for component in target.components() {
                match component {
                    Component::Normal(name) => target_parts.push(name.to_os_string()),
                    Component::CurDir => {},
                    Component::ParentDir => target_parts.push(OsString::from("..")),
                    _ => return false,
                }
            }
            for part in target_parts.into_iter().rev() { pending.push_front(part); }
        } else {
            if !metadata.is_dir() { return false; }
            resolved.push(part);
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dir_metadata_virtual_fixture_distinguishes_self_files_and_directories() {
        let mut fixture = OracleFixture::default();
        fixture.files.insert("inner/file".into(), b"data".to_vec());
        fixture.files.insert("plain".into(), b"data".to_vec());
        for path in ["", ".", "inner", "./inner", "inner//"] {
            assert!(fixture.metadata_is_directory("", path), "{path:?}");
        }
        for path in ["plain", "plain/child", "inner/file", "missing", "../inner", "inner/../", "/inner", "bad\0name"] {
            assert!(!fixture.metadata_is_directory("", path), "{path:?}");
        }
        assert!(fixture.metadata_is_directory("inner", "."));
        assert!(!fixture.metadata_is_directory("inner", "../inner"));
    }
}
