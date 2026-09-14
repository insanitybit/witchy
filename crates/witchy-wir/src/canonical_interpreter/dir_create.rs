//! Recursive mkdir changes only the virtual fixture tree. Ambient metadata is
//! a read-only fallback, never permission to mutate the filesystem. Preflight
//! keeps policy errors, symlinks, and non-directory prefixes from being
//! mistaken for a missing node or a successful idempotent directory creation.
use super::*;

/// Confined mkdir accepts CurDir components, unlike the older read-path ABI.
/// Reject parent traversal before normalization, even when it would cancel a
/// preceding name. No normalized component can acquire an ambient root.
pub(super) fn normalize_path(path: &str) -> Result<String, InterpretError> {
    if path.starts_with('/') || path.contains('\0') || path.split('/').any(|part| part == "..") {
        return Err(InterpretError::ExplicitTrap);
    }
    Ok(path.split('/').filter(|part| !part.is_empty() && *part != ".").collect::<Vec<_>>().join("/"))
}

impl OracleFixture {
    pub(super) fn make_directory(&mut self, path: &str) -> Result<(), InterpretError> {
        let path = normalize_path(path)?;
        let mut directories = Vec::new();
        let mut prefix = String::new();
        let mut fallback = self.root.clone();
        if let Some(root) = &fallback {
            let metadata = std::fs::symlink_metadata(root).map_err(|_| InterpretError::ExplicitTrap)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() { return Err(InterpretError::ExplicitTrap); }
        }
        for part in path.split('/').filter(|part| !part.is_empty()) {
            if !prefix.is_empty() { prefix.push('/'); }
            prefix.push_str(part);
            if self.files.contains_key(&prefix) || self.dir_mutations.links.contains_key(&prefix) {
                return Err(InterpretError::ExplicitTrap);
            }
            if self.backing_hidden(&prefix) { fallback = None; }
            if let Some(parent) = &fallback {
                let candidate = parent.join(part);
                match std::fs::symlink_metadata(&candidate) {
                    Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => fallback = Some(candidate),
                    Ok(_) => return Err(InterpretError::ExplicitTrap),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => fallback = None,
                    Err(_) => return Err(InterpretError::ExplicitTrap),
                }
            }
            directories.push(prefix.clone());
        }
        self.directories.extend(directories);
        Ok(())
    }
}
