//! Independent metadata for the oracle's unrestricted fixture grants. Unlike
//! the older file-read fixture helper, these predicates admit confined links
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
        self.metadata_kind(scope, path) == Some(true)
    }

    pub(super) fn metadata_exists(&self, scope: &str, path: &str) -> bool {
        self.metadata_kind(scope, path).is_some()
    }

    fn metadata_kind(&self, scope: &str, path: &str) -> Option<bool> {
        let relative = relative_parts(path)?;
        // A minted directory handle denotes itself, even an empty directory.
        if relative.is_empty() { return Some(true); }
        relative_parts(scope)?;
        fixture_kind(self, scope, relative)
    }
}

/// Resolve a stable fixture tree component by component. Absolute link targets
/// and relative targets crossing the grant root never acquire ambient meaning.
/// Link expansion limits match the platform primitive used by the native host;
/// the oracle keeps its own resolution and metadata implementation.
fn fixture_kind(fixture: &OracleFixture, scope: &str, parts: Vec<String>) -> Option<bool> {
    let mut pending: VecDeque<OsString> = parts.into_iter().map(OsString::from).collect();
    let mut resolved = PathBuf::new();
    let mut links = 0;
    let link_limit = if cfg!(windows) { 63 } else { 40 };
    while let Some(part) = pending.pop_front() {
        if part == "." { continue; }
        if part == ".." {
            if !resolved.pop() { return None; }
            continue;
        }
        let relative = Path::new(scope).join(&resolved).join(&part);
        let relative = relative.to_str()?;
        if fixture.files.contains_key(relative) { return pending.is_empty().then_some(false); }
        if fixture.is_directory(relative) { resolved.push(part); continue; }
        let virtual_link = fixture.dir_mutations.links.get(relative);
        if virtual_link.is_none() && fixture.backing_hidden(relative) { return None; }
        // A moved virtual link has no ambient inode. Its target is resolved
        // within this grant using the same component rules as a backing link.
        let target = if let Some(target) = virtual_link { target.clone() } else {
            let base = fixture.root.as_ref()?;
            let candidate = base.join(relative);
            let metadata = std::fs::symlink_metadata(&candidate).ok()?;
            if !metadata.file_type().is_symlink() {
                // Existence includes sockets/FIFOs/devices. Classification is
                // metadata-only: never open a special file to answer a query.
                // Such an entry cannot serve as an intermediate directory.
                return pending.is_empty().then_some(false);
            }
            std::fs::read_link(&candidate).ok()?
        };
        if links == link_limit { return None; }
        links += 1;
        let mut target_parts = Vec::new();
        for component in target.components() {
            match component {
                Component::Normal(name) => target_parts.push(name.to_os_string()),
                Component::CurDir => {},
                Component::ParentDir => target_parts.push(OsString::from("..")),
                _ => return None,
            }
        }
        for part in target_parts.into_iter().rev() { pending.push_front(part); }
    }
    Some(true)
}
