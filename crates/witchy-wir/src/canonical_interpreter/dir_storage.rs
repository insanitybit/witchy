//! Independent directory storage over the virtual fixture namespace. Fallback
//! metadata and reads describe a stable explicit fixture tree; they never grant
//! ambient writes. Read responses own their bytes, rather than a mutable path.
use super::*;
use crate::canonical_safewir::AccessMode;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind { Missing, File, Directory, LeafSymlink }

fn refused(message: &str) -> InterpretError { InterpretError::RegistryFixture(message.into()) }

/// The native slice ABI reports nonnegative i32 lengths, not every u32 Size.
/// Keep this allocation-free so the boundary is testable without a huge file.
pub(super) fn response_length(length: usize) -> Result<u64, InterpretError> {
    i32::try_from(length).map(|length| length as u64)
        .map_err(|_| refused("Dir byte response exceeds the guest ABI size limit"))
}

impl OracleFixture {
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
            let virtual_dir = self.directories.contains(&prefix)
                || self.directories.iter().any(|name| name.starts_with(&children))
                || self.files.keys().any(|name| name.starts_with(&children));
            if virtual_file && virtual_dir { return Err(refused("directory storage fixture has a file/directory collision")); }
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
            let kind = if virtual_file { EntryKind::File } else if virtual_dir { EntryKind::Directory } else { physical };
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
        match (name, arguments, destinations) {
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
