//! Independent directory storage over the virtual fixture namespace. Fallback
//! metadata and reads describe a stable explicit fixture tree; they never grant
//! ambient writes. Read responses own their bytes, rather than a mutable path.
use super::*;
use crate::canonical_safewir::AccessMode;
use std::path::PathBuf;

/// Execution-local overlay on a stable, explicit backing fixture. A tombstone
/// hides only the backing tree: later virtual creation at that name is visible.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OracleDirMutations {
    pub removed: BTreeSet<String>,
    /// Leaf links moved by rename, never followed by file read/write/chmod.
    pub links: BTreeMap<String, PathBuf>,
    /// The +x operation adds execute bits without rewriting other mode bits.
    /// No source operation in this family exposes a complete permission mode.
    pub executable: BTreeSet<String>,
}

pub(super) const INSTALLER_IMPORTS: [&str; 3] = [
    "dir_append_bytes_at", "dir_set_executable_at", "dir_rename_at",
];

pub(super) fn is_import(name: &str) -> bool {
    crate::canonical_safewir::DIR_STORAGE_IMPORT_NAMES.contains(&name)
        || INSTALLER_IMPORTS.contains(&name)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind { Missing, File, Directory, LeafSymlink }

enum MovedEntry { File(Vec<u8>), Directory, Link(PathBuf) }

fn refused(message: &str) -> InterpretError { InterpretError::RegistryFixture(message.into()) }

/// The native slice ABI reports nonnegative i32 lengths, not every u32 Size.
/// Keep this allocation-free so the boundary is testable without a huge file.
pub(super) fn response_length(length: usize) -> Result<u64, InterpretError> {
    i32::try_from(length).map(|length| length as u64)
        .map_err(|_| refused("Dir byte response exceeds the guest ABI size limit"))
}

impl OracleFixture {
    pub(super) fn backing_hidden(&self, path: &str) -> bool {
        self.dir_mutations.removed.iter().any(|removed|
            path == removed || path.strip_prefix(removed).is_some_and(|rest| rest.starts_with('/')))
    }

    /// Resolve every prefix before deciding that an existing leaf is the
    /// create-new race-loser case. Only NotFound denotes absence. Leaf links
    /// are classified without following them; intermediate links, non-file
    /// nodes, missing parents, and metadata errors remain failures.
    fn storage_entry(&self, path: &str) -> Result<(EntryKind, Option<PathBuf>), InterpretError> {
        let mut fallback = self.root.clone();
        if let Some(root) = &fallback {
            let metadata = std::fs::symlink_metadata(root).map_err(|_| refused("directory storage root metadata failed"))?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(refused("directory storage root is not a confined directory"));
            }
        }
        let path = dir_create::normalize_path(path)?;
        if path.is_empty() { return Err(refused("directory storage path does not name a file")); }
        let mut prefix = String::new();
        let mut parts = path.split('/').peekable();
        while let Some(part) = parts.next() {
            if !prefix.is_empty() { prefix.push('/'); }
            prefix.push_str(part);
            let children = format!("{prefix}/");
            let virtual_file = self.files.contains_key(&prefix);
            let virtual_link = self.dir_mutations.links.contains_key(&prefix);
            let virtual_dir = self.directories.contains(&prefix)
                || self.directories.iter().any(|name| name.starts_with(&children))
                || self.files.keys().any(|name| name.starts_with(&children))
                || self.dir_mutations.links.keys().any(|name| name.starts_with(&children));
            if (virtual_file && (virtual_dir || virtual_link)) || (virtual_dir && virtual_link) {
                return Err(refused("directory storage fixture has a file/directory collision"));
            }
            if self.backing_hidden(&prefix) { fallback = None; }
            let physical = if let Some(parent) = &fallback {
                let candidate = parent.join(part);
                match std::fs::symlink_metadata(&candidate) {
                    Ok(metadata) => {
                        let kind = if metadata.file_type().is_symlink() {
                            if parts.peek().is_some() { return Err(refused("directory storage refuses intermediate symlinks")); }
                            EntryKind::LeafSymlink
                        } else if metadata.is_file() { EntryKind::File }
                            else if metadata.is_dir() { EntryKind::Directory }
                            else { return Err(refused("directory storage refuses non-file entries")); };
                        fallback = Some(candidate);
                        kind
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => { fallback = None; EntryKind::Missing }
                    Err(_) => return Err(refused("directory storage entry metadata failed")),
                }
            } else { EntryKind::Missing };
            if (virtual_file && physical == EntryKind::Directory)
                || (virtual_dir && matches!(physical, EntryKind::File | EntryKind::LeafSymlink))
            {
                return Err(refused("directory storage overlay changes an entry kind"));
            }
            // A fixture replacement shadows the old leaf link as a regular
            // file. It does not change or traverse that ambient link's target.
            let kind = if virtual_file { EntryKind::File } else if virtual_link { EntryKind::LeafSymlink }
                else if virtual_dir { EntryKind::Directory } else { physical };
            if parts.peek().is_none() { return Ok((kind, fallback)); }
            if kind != EntryKind::Directory { return Err(refused("directory storage parent does not exist or is not a directory")); }
        }
        unreachable!()
    }

    fn storage_write(&mut self, path: &str, bytes: Vec<u8>, create_new: bool, replace: bool) -> Result<bool, InterpretError> {
        let (kind, _) = self.storage_entry(path)?;
        // A named existing directory also loses O_EXCL creation; it never
        // acquires a second file entry. The grant itself is not a file name.
        if create_new && kind != EntryKind::Missing { return Ok(false); }
        if kind == EntryKind::Directory { return Err(refused("directory storage cannot replace a directory")); }
        if kind == EntryKind::LeafSymlink && !replace { return Err(refused("directory storage write refuses a leaf symlink")); }
        if replace {
            self.dir_mutations.links.remove(path);
            self.dir_mutations.executable.remove(path);
        }
        self.files.insert(path.to_owned(), bytes);
        Ok(true)
    }

    fn storage_read(&self, path: &str) -> Result<Vec<u8>, InterpretError> {
        let (kind, fallback) = self.storage_entry(path)?;
        if kind != EntryKind::File { return Err(refused("directory storage read requires an existing file")); }
        if let Some(bytes) = self.files.get(path) { return Ok(bytes.clone()); }
        let path = fallback.ok_or_else(|| refused("directory storage lost its file"))?;
        std::fs::read(path).map_err(|_| refused("directory storage file read failed"))
    }

    fn storage_append(&mut self, path: &str, bytes: &[u8]) -> Result<(), InterpretError> {
        let (kind, _) = self.storage_entry(path)?;
        let mut contents = match kind {
            EntryKind::Missing => Vec::new(),
            EntryKind::File => self.storage_read(path)?,
            _ => return Err(refused("directory append requires a regular file")),
        };
        contents.try_reserve(bytes.len()).map_err(|_| refused("directory append size overflow"))?;
        contents.extend_from_slice(bytes);
        self.files.insert(path.to_owned(), contents);
        Ok(())
    }

    fn storage_set_executable(&mut self, path: &str) -> Result<(), InterpretError> {
        let (kind, _) = self.storage_entry(path)?;
        // The provider opens without following the leaf and ORs mode with 0111.
        // Existing directories may also be opened; missing nodes and links fail.
        if !matches!(kind, EntryKind::File | EntryKind::Directory) {
            return Err(refused("directory set_executable requires an existing non-link entry"));
        }
        self.dir_mutations.executable.insert(path.to_owned());
        Ok(())
    }

    fn storage_children(&self, path: &str, fallback: Option<PathBuf>) -> Result<BTreeSet<String>, InterpretError> {
        let prefix = format!("{path}/");
        let mut children = BTreeSet::new();
        for key in self.files.keys().chain(self.directories.iter()).chain(self.dir_mutations.links.keys()) {
            if let Some(rest) = key.strip_prefix(&prefix) {
                if let Some(first) = rest.split('/').next().filter(|name| !name.is_empty()) {
                    children.insert(first.to_owned());
                }
            }
        }
        if let Some(directory) = fallback {
            for entry in std::fs::read_dir(directory).map_err(|_| refused("directory rename listing failed"))? {
                let entry = entry.map_err(|_| refused("directory rename entry failed"))?;
                let name = entry.file_name().into_string().map_err(|_| refused("directory rename fixture name is not UTF-8"))?;
                if !self.backing_hidden(&format!("{prefix}{name}")) { children.insert(name); }
            }
        }
        Ok(children)
    }

    fn storage_rename(&mut self, from: &str, to: &str) -> Result<(), InterpretError> {
        let (source_kind, _) = self.storage_entry(from)?;
        let (target_kind, target_backing) = self.storage_entry(to)?;
        if source_kind == EntryKind::Missing { return Err(refused("directory rename source does not exist")); }
        if from == to { return Ok(()); }
        if to.starts_with(&format!("{from}/")) { return Err(refused("directory rename destination is inside source")); }
        if target_kind != EntryKind::Missing && (source_kind == EntryKind::Directory) != (target_kind == EntryKind::Directory) {
            return Err(refused("directory rename file/directory mismatch"));
        }
        if target_kind == EntryKind::Directory && !self.storage_children(to, target_backing)?.is_empty() {
            return Err(refused("directory rename destination is not empty"));
        }
        // Snapshot the complete subtree before publishing anything. Unlike the
        // ordinary listing convenience API, failed backing reads/listings are
        // errors, not an empty successful move. Leaf links move as links.
        let mut pending = vec![from.to_owned()];
        let mut moved = Vec::new();
        while let Some(path) = pending.pop() {
            let (kind, backing) = self.storage_entry(&path)?;
            let entry = match kind {
                EntryKind::File => MovedEntry::File(self.storage_read(&path)?),
                EntryKind::Directory => {
                    for child in self.storage_children(&path, backing)? { pending.push(format!("{path}/{child}")); }
                    MovedEntry::Directory
                }
                EntryKind::LeafSymlink => {
                    let target = if let Some(target) = self.dir_mutations.links.get(&path) { target.clone() }
                        else { std::fs::read_link(backing.ok_or_else(|| refused("directory rename lost link"))?)
                            .map_err(|_| refused("directory rename link read failed"))? };
                    MovedEntry::Link(target)
                }
                EntryKind::Missing => return Err(refused("directory rename fixture changed during snapshot")),
            };
            let destination = format!("{to}{}", &path[from.len()..]);
            moved.push((destination, entry, self.dir_mutations.executable.contains(&path)));
        }
        let in_tree = |path: &str, root: &str| path == root || path.strip_prefix(root).is_some_and(|suffix| suffix.starts_with('/'));
        self.files.retain(|path, _| !in_tree(path, from) && !in_tree(path, to));
        self.directories.retain(|path| !in_tree(path, from) && !in_tree(path, to));
        self.dir_mutations.links.retain(|path, _| !in_tree(path, from) && !in_tree(path, to));
        self.dir_mutations.executable.retain(|path| !in_tree(path, from) && !in_tree(path, to));
        // Both replaced backing names stay hidden even after a later virtual
        // rename or recreation. Do not resurrect destination leftovers either.
        self.dir_mutations.removed.extend([from.to_owned(), to.to_owned()]);
        for (path, entry, executable) in moved {
            match entry {
                MovedEntry::File(bytes) => { self.files.insert(path.clone(), bytes); }
                MovedEntry::Directory => { self.directories.insert(path.clone()); }
                MovedEntry::Link(target) => { self.dir_mutations.links.insert(path.clone(), target); }
            }
            if executable { self.dir_mutations.executable.insert(path); }
        }
        // Parents inferred from fixture children are real directories, not
        // records to disappear when their final child is moved elsewhere.
        for path in [from, to] {
            let mut parent = path;
            while let Some((next, _)) = parent.rsplit_once('/') {
                self.directories.insert(next.to_owned());
                parent = next;
            }
        }
        Ok(())
    }
}

impl Engine<'_> {
    fn storage_token<'a>(&self, id: ValueId, values: &'a BTreeMap<ValueId, InterpreterValue>) -> Result<&'a CapabilityToken, InterpretError> {
        match values.get(&id) {
            Some(InterpreterValue::Capability(token)) => Ok(token),
            _ => Err(refused("directory storage capability value")),
        }
    }

    fn storage_input(&self, place: &Place, values: &BTreeMap<ValueId, InterpreterValue>) -> Result<Vec<u8>, InterpretError> {
        if place.access() != AccessMode::Read { return Err(refused("directory storage input requires a read slice")); }
        self.host_bytes(place, values)
    }

    fn storage_path(&self, dir: ValueId, place: &Place, values: &BTreeMap<ValueId, InterpreterValue>) -> Result<String, InterpretError> {
        let scope = self.registry.storage_scope(self.storage_token(dir, values)?)?;
        let path = String::from_utf8(self.storage_input(place, values)?)
            .map_err(|_| InterpretError::TypeMismatch("host string is not UTF-8"))?;
        let relative = dir_create::normalize_path(&path)?;
        if relative.is_empty() { return Err(refused("directory storage path does not name a file")); }
        self.host_path(&scope, &relative)
    }

    pub(super) fn dir_storage_host_call(&mut self, name: &str, destinations: &[ValueId],
        arguments: &[HostCallArgument], values: &mut BTreeMap<ValueId, InterpreterValue>) -> Result<(), InterpretError>
    {
        if self.isolated { return Err(InterpretError::UnsupportedHostImport); }
        self.server.before_host_call(name)?;
        if self.detached_dir_host_call(name, destinations, arguments, values)? { return Ok(()); }
        match (name, arguments, destinations) {
            ("dir_append_bytes_at", [HostCallArgument::Capability(dir), HostCallArgument::Buffer { place: path, .. },
                HostCallArgument::Buffer { place: data, .. }], []) => {
                let path = self.storage_path(*dir, path, values)?;
                let bytes = self.storage_input(data, values)?;
                self.config.fixture.storage_append(&path, &bytes)?;
            }
            ("dir_set_executable_at", [HostCallArgument::Capability(dir), HostCallArgument::Buffer { place: path, .. }], []) => {
                let path = self.storage_path(*dir, path, values)?;
                self.config.fixture.storage_set_executable(&path)?;
            }
            ("dir_rename_at", [HostCallArgument::Capability(dir), HostCallArgument::Buffer { place: from, .. },
                HostCallArgument::Buffer { place: to, .. }], []) => {
                let from = self.storage_path(*dir, from, values)?;
                let to = self.storage_path(*dir, to, values)?;
                self.config.fixture.storage_rename(&from, &to)?;
                self.registry.relocate_directories(&from, &to);
            }
            ("dir_create_new_at" | "dir_replace_at" | "dir_write_bytes_at",
                [HostCallArgument::Capability(dir), HostCallArgument::Buffer { place: path, .. }, HostCallArgument::Buffer { place: data, .. }], results)
            => {
                let create_new = name == "dir_create_new_at";
                if (create_new && results.len() != 1) || (!create_new && !results.is_empty()) {
                    return Err(refused("directory storage write result arity"));
                }
                let path = self.storage_path(*dir, path, values)?;
                let data = self.storage_input(data, values)?;
                if name != "dir_write_bytes_at" && std::str::from_utf8(&data).is_err() {
                    return Err(InterpretError::TypeMismatch("host string is not UTF-8"));
                }
                // All guest input is snapshotted before even the existing-file
                // false result. No fixture mutation occurs on transport errors.
                let created = self.config.fixture.storage_write(&path, data, create_new, name == "dir_replace_at")?;
                if create_new { values.insert(results[0], InterpreterValue::Bool(created)); }
            }
            ("dir_read_bytes_at", [HostCallArgument::Capability(dir), HostCallArgument::Buffer { place, .. }], [destination]) => {
                let path = self.storage_path(*dir, place, values)?;
                let bytes = self.config.fixture.storage_read(&path)?;
                response_length(bytes.len())?;
                let response = self.registry.mint_dir_bytes(bytes);
                values.insert(*destination, InterpreterValue::Capability(response));
            }
            ("dir_bytes_response_size", [HostCallArgument::Capability(response)], [destination]) => {
                let bytes = self.registry.dir_bytes(self.storage_token(*response, values)?)?;
                values.insert(*destination, InterpreterValue::Size(response_length(bytes.len())?));
            }
            ("dir_bytes_response_into", [HostCallArgument::Capability(response), HostCallArgument::Buffer { place, .. }], [destination]) => {
                let bytes = self.registry.dir_bytes(self.storage_token(*response, values)?)?;
                let required = response_length(bytes.len())?;
                let range = self.registry_output(place, values)?;
                self.registry_write(range, &bytes)?;
                values.insert(*destination, InterpreterValue::Size(required));
            }
            _ => return Err(refused("unsupported directory storage host arguments")),
        }
        Ok(())
    }
}
