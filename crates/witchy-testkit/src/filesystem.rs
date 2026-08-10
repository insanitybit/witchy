use std::collections::{BTreeMap, BTreeSet};

use witchy_cap_model::{dir_admits, dir_only};

use crate::{
    FilesystemEntry, FixtureCall, FixtureErrorCode, FixtureFailure, FixtureFamily, FixtureHandle,
    FixtureOutcome, FixturePlan, FixtureSession, FixtureValue, SourceLocation,
};
use crate::hex::{decode as decode_hex, encode as encode_hex};

pub type FilesystemProviderResult<T> = Result<T, FixtureFailure>;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    Directory,
    File(Vec<u8>),
}

#[derive(Debug, Clone)]
enum HandleState {
    Dir {
        base: String,
        rights: BTreeSet<String>,
        policy: String,
    },
    File {
        path: String,
        rights: BTreeSet<String>,
    },
}

#[derive(Debug)]
pub(crate) struct FilesystemProviderState {
    configured: bool,
    root_rights: BTreeSet<String>,
    root_policy: String,
    nodes: BTreeMap<String, Node>,
    handles: BTreeMap<u64, HandleState>,
}

impl FilesystemProviderState {
    pub(crate) fn new(plan: &FixturePlan) -> Self {
        let Some(fixture) = &plan.filesystem else {
            return Self {
                configured: false,
                root_rights: BTreeSet::new(),
                root_policy: String::new(),
                nodes: BTreeMap::new(),
                handles: BTreeMap::new(),
            };
        };
        let mut nodes = BTreeMap::from([(String::new(), Node::Directory)]);
        for (path, entry) in &fixture.entries {
            add_parent_directories(&mut nodes, path);
            let node = match entry {
                FilesystemEntry::Directory => Node::Directory,
                FilesystemEntry::File { hex } => Node::File(decode_hex(hex)),
            };
            nodes.insert(path.clone(), node);
        }
        Self {
            configured: true,
            root_rights: fixture.rights.iter().cloned().collect(),
            root_policy: fixture.entry_policy.clone().unwrap_or_default(),
            nodes,
            handles: BTreeMap::new(),
        }
    }

    pub(crate) const fn configured(&self) -> bool {
        self.configured
    }

    fn dir(&self, handle: &FixtureHandle) -> FilesystemProviderResult<(&str, &BTreeSet<String>, &str)> {
        match self.handles.get(&handle.id()) {
            Some(HandleState::Dir {
                base,
                rights,
                policy,
            }) => Ok((base, rights, policy)),
            _ => Err(fs_failure(
                FixtureErrorCode::PermissionDenied,
                "invalid or foreign Dir fixture handle",
            )),
        }
    }

    fn file(&self, handle: &FixtureHandle) -> FilesystemProviderResult<(&str, &BTreeSet<String>)> {
        match self.handles.get(&handle.id()) {
            Some(HandleState::File { path, rights }) => Ok((path, rights)),
            _ => Err(fs_failure(
                FixtureErrorCode::PermissionDenied,
                "invalid or foreign File fixture handle",
            )),
        }
    }
}

impl FixtureSession {
    pub fn mint_fixture_dir(
        &mut self,
        source: Option<SourceLocation>,
    ) -> FilesystemProviderResult<FixtureHandle> {
        if !self.filesystem.configured {
            return Err(fs_failure(
                FixtureErrorCode::PermissionDenied,
                "filesystem fixture was not declared",
            ));
        }
        let mut call = FixtureCall::new(FixtureFamily::Filesystem, "mint_dir");
        call.effective_rights = self.filesystem.root_rights.iter().cloned().collect();
        call.source = source;
        let outcome = self.dispatch_filesystem(
            call,
            FixtureOutcome::Return {
                value: FixtureValue::String("Dir".into()),
            },
        );
        outcome_marker(outcome, "Dir", "mint_dir")?;
        let handle = self
            .basic
            .mint_handle(FixtureFamily::Filesystem, BTreeSet::new());
        self.filesystem.handles.insert(
            handle.id(),
            HandleState::Dir {
                base: String::new(),
                rights: self.filesystem.root_rights.clone(),
                policy: self.filesystem.root_policy.clone(),
            },
        );
        Ok(handle)
    }

    pub fn dir_only(
        &mut self,
        handle: &FixtureHandle,
        refine: &str,
        source: Option<SourceLocation>,
    ) -> FilesystemProviderResult<FixtureHandle> {
        let (base, rights, policy) = self.checked_dir(handle)?;
        require_right(&rights, "Read", "Dir")?;
        let narrowed = dir_only(&policy, refine);
        let mut call = fs_call("dir_only", Some(refine), &rights, source);
        call.arguments
            .insert("policy".into(), FixtureValue::String(refine.into()));
        outcome_marker(
            self.dispatch_filesystem(
                call,
                FixtureOutcome::Return {
                    value: FixtureValue::String("Dir".into()),
                },
            ),
            "Dir",
            "dir_only",
        )?;
        self.insert_dir_handle(base, rights, narrowed)
    }

    pub fn dir_subdir(
        &mut self,
        handle: &FixtureHandle,
        name: &str,
        source: Option<SourceLocation>,
    ) -> FilesystemProviderResult<FixtureHandle> {
        let (base, rights, policy) = self.checked_dir(handle)?;
        require_right(&rights, "Read", "Dir")?;
        let path = join_path(&base, name)?;
        self.guard_policy(&policy, name, true)?;
        match self.filesystem.nodes.get(&path) {
            Some(Node::Directory) => {}
            Some(Node::File(_)) => {
                return Err(fs_failure(
                    FixtureErrorCode::NotDirectory,
                    format!("`{name}` is not a directory"),
                ));
            }
            None => {
                return Err(fs_failure(
                    FixtureErrorCode::NotFound,
                    format!("directory `{name}` was not found"),
                ));
            }
        }
        let call = fs_call("dir_subdir", Some(name), &rights, source);
        outcome_marker(
            self.dispatch_filesystem(
                call,
                FixtureOutcome::Return {
                    value: FixtureValue::String("Dir".into()),
                },
            ),
            "Dir",
            "dir_subdir",
        )?;
        self.insert_dir_handle(path, rights, policy)
    }

    pub fn dir_read(
        &mut self,
        handle: &FixtureHandle,
        relative: &str,
        source: Option<SourceLocation>,
    ) -> FilesystemProviderResult<Vec<u8>> {
        let (base, rights, policy) = self.checked_dir(handle)?;
        require_right(&rights, "Read", "Dir")?;
        let path = join_path(&base, relative)?;
        self.guard_policy(&policy, relative, false)?;
        let value = match self.filesystem.nodes.get(&path) {
            Some(Node::File(bytes)) => bytes.clone(),
            Some(Node::Directory) => {
                return Err(fs_failure(
                    FixtureErrorCode::InvalidData,
                    format!("`{relative}` is a directory"),
                ));
            }
            None => {
                return Err(fs_failure(
                    FixtureErrorCode::NotFound,
                    format!("file `{relative}` was not found"),
                ));
            }
        };
        let call = fs_call("dir_read_len", Some(relative), &rights, source);
        outcome_bytes(
            self.dispatch_filesystem(
                call,
                FixtureOutcome::Return {
                    value: FixtureValue::Bytes(encode_hex(&value)),
                },
            ),
            "dir_read_len",
        )
    }

    /// RFC-0095: byte-read. The fixture stores files as raw bytes, so a byte-read
    /// is identical to a text read here — the UTF-8 distinction only matters on the
    /// real filesystem.
    pub fn dir_read_bytes(
        &mut self,
        handle: &FixtureHandle,
        relative: &str,
        source: Option<SourceLocation>,
    ) -> FilesystemProviderResult<Vec<u8>> {
        self.dir_read(handle, relative, source)
    }

    pub fn dir_exists(
        &mut self,
        handle: &FixtureHandle,
        relative: &str,
        source: Option<SourceLocation>,
    ) -> FilesystemProviderResult<bool> {
        let (base, rights, _) = self.checked_dir(handle)?;
        require_right(&rights, "Read", "Dir")?;
        let exists = join_path(&base, relative)
            .ok()
            .is_some_and(|path| self.filesystem.nodes.contains_key(&path));
        let call = fs_call("dir_exists", Some(relative), &rights, source);
        outcome_bool(
            self.dispatch_filesystem(
                call,
                FixtureOutcome::Return {
                    value: FixtureValue::Bool(exists),
                },
            ),
            "dir_exists",
        )
    }

    pub fn dir_is_dir(
        &mut self,
        handle: &FixtureHandle,
        relative: &str,
        source: Option<SourceLocation>,
    ) -> FilesystemProviderResult<bool> {
        let (base, rights, _) = self.checked_dir(handle)?;
        require_right(&rights, "Read", "Dir")?;
        let is_dir = join_path(&base, relative)
            .ok()
            .and_then(|path| self.filesystem.nodes.get(&path))
            .is_some_and(|node| matches!(node, Node::Directory));
        let call = fs_call("dir_is_dir", Some(relative), &rights, source);
        outcome_bool(
            self.dispatch_filesystem(
                call,
                FixtureOutcome::Return {
                    value: FixtureValue::Bool(is_dir),
                },
            ),
            "dir_is_dir",
        )
    }

    pub fn dir_list(
        &mut self,
        handle: &FixtureHandle,
        source: Option<SourceLocation>,
    ) -> FilesystemProviderResult<Vec<String>> {
        let (base, rights, _) = self.checked_dir(handle)?;
        require_right(&rights, "Read", "Dir")?;
        let prefix = if base.is_empty() {
            String::new()
        } else {
            format!("{base}/")
        };
        let mut names = BTreeSet::new();
        for path in self.filesystem.nodes.keys() {
            let Some(rest) = path.strip_prefix(&prefix) else {
                continue;
            };
            if !rest.is_empty() {
                names.insert(rest.split('/').next().unwrap_or_default().to_string());
            }
        }
        let names: Vec<String> = names.into_iter().collect();
        let call = fs_call("dir_list_size", None, &rights, source);
        outcome_strings(
            self.dispatch_filesystem(
                call,
                FixtureOutcome::Return {
                    value: FixtureValue::List(
                        names
                            .iter()
                            .cloned()
                            .map(FixtureValue::String)
                            .collect(),
                    ),
                },
            ),
            "dir_list_size",
        )
    }

    pub fn dir_write(
        &mut self,
        handle: &FixtureHandle,
        relative: &str,
        bytes: &[u8],
        source: Option<SourceLocation>,
    ) -> FilesystemProviderResult<usize> {
        self.write_from_dir(handle, relative, bytes, false, "dir_write", source)
    }

    /// RFC-0095: byte-write. Identical to `dir_write` at the fixture level (which is
    /// byte-native); the UTF-8 distinction only matters on the real filesystem.
    pub fn dir_write_bytes(
        &mut self,
        handle: &FixtureHandle,
        relative: &str,
        bytes: &[u8],
        source: Option<SourceLocation>,
    ) -> FilesystemProviderResult<usize> {
        self.write_from_dir(handle, relative, bytes, false, "dir_write", source)
    }

    pub fn dir_append(
        &mut self,
        handle: &FixtureHandle,
        relative: &str,
        bytes: &[u8],
        source: Option<SourceLocation>,
    ) -> FilesystemProviderResult<usize> {
        self.write_from_dir(handle, relative, bytes, true, "dir_append", source)
    }

    pub fn dir_make_dir(
        &mut self,
        handle: &FixtureHandle,
        relative: &str,
        source: Option<SourceLocation>,
    ) -> FilesystemProviderResult<()> {
        let (base, rights, policy) = self.checked_dir(handle)?;
        require_right(&rights, "Write", "Dir")?;
        self.guard_policy(&policy, relative, true)?;
        let path = join_path(&base, relative)?;
        require_parent_directory(&self.filesystem.nodes, &path)?;
        if matches!(self.filesystem.nodes.get(&path), Some(Node::File(_))) {
            return Err(fs_failure(
                FixtureErrorCode::AlreadyExists,
                format!("file `{relative}` already exists"),
            ));
        }
        let call = fs_call("dir_make_dir", Some(relative), &rights, source);
        outcome_unit(
            self.dispatch_filesystem(
                call,
                FixtureOutcome::Return {
                    value: FixtureValue::Null,
                },
            ),
            "dir_make_dir",
        )?;
        self.filesystem.nodes.insert(path, Node::Directory);
        Ok(())
    }

    pub fn dir_open(
        &mut self,
        handle: &FixtureHandle,
        relative: &str,
        source: Option<SourceLocation>,
    ) -> FilesystemProviderResult<FixtureHandle> {
        self.open_from_dir(handle, relative, false, source)
    }

    pub fn dir_create(
        &mut self,
        handle: &FixtureHandle,
        relative: &str,
        source: Option<SourceLocation>,
    ) -> FilesystemProviderResult<FixtureHandle> {
        self.open_from_dir(handle, relative, true, source)
    }

    /// RFC-0118 atomic exclusive-create. Returns `true` when this call created
    /// the file and `false` when it was already present (the race-loser signal);
    /// the fixture host runs single-threaded, so "check-then-insert" is one
    /// indivisible step, matching the native `O_CREAT|O_EXCL` observable result.
    pub fn dir_create_new(
        &mut self,
        handle: &FixtureHandle,
        relative: &str,
        bytes: &[u8],
        source: Option<SourceLocation>,
    ) -> FilesystemProviderResult<bool> {
        let (base, rights, policy) = self.checked_dir(handle)?;
        require_right(&rights, "Write", "Dir")?;
        self.guard_policy(&policy, relative, false)?;
        let path = join_path(&base, relative)?;
        require_parent_directory(&self.filesystem.nodes, &path)?;
        if matches!(self.filesystem.nodes.get(&path), Some(Node::Directory)) {
            return Err(fs_failure(
                FixtureErrorCode::InvalidData,
                format!("`{relative}` is a directory"),
            ));
        }
        if matches!(self.filesystem.nodes.get(&path), Some(Node::File(_))) {
            return Ok(false);
        }
        let mut call = fs_call("dir_create_new", Some(relative), &rights, source);
        call.arguments
            .insert("bytes".into(), FixtureValue::Bytes(encode_hex(bytes)));
        outcome_unit(
            self.dispatch_filesystem(
                call,
                FixtureOutcome::Return { value: FixtureValue::Null },
            ),
            "dir_create_new",
        )?;
        self.filesystem.nodes.insert(path, Node::File(bytes.to_vec()));
        Ok(true)
    }

    /// RFC-0118 atomic whole-file replace (creating if absent). A map insert is
    /// never torn, matching native's temp+rename observable result.
    pub fn dir_replace(
        &mut self,
        handle: &FixtureHandle,
        relative: &str,
        bytes: &[u8],
        source: Option<SourceLocation>,
    ) -> FilesystemProviderResult<()> {
        let (base, rights, policy) = self.checked_dir(handle)?;
        require_right(&rights, "Write", "Dir")?;
        self.guard_policy(&policy, relative, false)?;
        let path = join_path(&base, relative)?;
        require_parent_directory(&self.filesystem.nodes, &path)?;
        if matches!(self.filesystem.nodes.get(&path), Some(Node::Directory)) {
            return Err(fs_failure(
                FixtureErrorCode::InvalidData,
                format!("`{relative}` is a directory"),
            ));
        }
        let mut call = fs_call("dir_replace", Some(relative), &rights, source);
        call.arguments
            .insert("bytes".into(), FixtureValue::Bytes(encode_hex(bytes)));
        outcome_unit(
            self.dispatch_filesystem(
                call,
                FixtureOutcome::Return { value: FixtureValue::Null },
            ),
            "dir_replace",
        )?;
        self.filesystem.nodes.insert(path, Node::File(bytes.to_vec()));
        Ok(())
    }

    /// RFC-0118 atomic rename/replace within the Dir authority.
    pub fn dir_rename(
        &mut self,
        handle: &FixtureHandle,
        from: &str,
        to: &str,
        source: Option<SourceLocation>,
    ) -> FilesystemProviderResult<()> {
        let (base, rights, policy) = self.checked_dir(handle)?;
        require_right(&rights, "Write", "Dir")?;
        self.guard_policy(&policy, from, false)?;
        self.guard_policy(&policy, to, false)?;
        let from_path = join_path(&base, from)?;
        let to_path = join_path(&base, to)?;
        require_parent_directory(&self.filesystem.nodes, &to_path)?;
        let moved = match self.filesystem.nodes.get(&from_path) {
            Some(Node::File(existing)) => Node::File(existing.clone()),
            Some(Node::Directory) => {
                return Err(fs_failure(
                    FixtureErrorCode::InvalidData,
                    format!("`{from}` is a directory"),
                ));
            }
            None => {
                return Err(fs_failure(
                    FixtureErrorCode::NotFound,
                    format!("`{from}` was not found"),
                ));
            }
        };
        if matches!(self.filesystem.nodes.get(&to_path), Some(Node::Directory)) {
            return Err(fs_failure(
                FixtureErrorCode::InvalidData,
                format!("`{to}` is a directory"),
            ));
        }
        let call = fs_call("dir_rename", Some(from), &rights, source);
        outcome_unit(
            self.dispatch_filesystem(
                call,
                FixtureOutcome::Return { value: FixtureValue::Null },
            ),
            "dir_rename",
        )?;
        self.filesystem.nodes.remove(&from_path);
        self.filesystem.nodes.insert(to_path, moved);
        Ok(())
    }

    pub fn file_read(
        &mut self,
        handle: &FixtureHandle,
        source: Option<SourceLocation>,
    ) -> FilesystemProviderResult<Vec<u8>> {
        let (path, rights) = self.checked_file(handle)?;
        require_right(&rights, "Read", "File")?;
        let bytes = match self.filesystem.nodes.get(&path) {
            Some(Node::File(bytes)) => bytes.clone(),
            _ => {
                return Err(fs_failure(
                    FixtureErrorCode::NotFound,
                    "fixture file no longer exists",
                ));
            }
        };
        let call = fs_call("file_read_len", None, &rights, source);
        outcome_bytes(
            self.dispatch_filesystem(
                call,
                FixtureOutcome::Return {
                    value: FixtureValue::Bytes(encode_hex(&bytes)),
                },
            ),
            "file_read_len",
        )
    }

    pub fn file_write(
        &mut self,
        handle: &FixtureHandle,
        bytes: &[u8],
        source: Option<SourceLocation>,
    ) -> FilesystemProviderResult<usize> {
        let (path, rights) = self.checked_file(handle)?;
        require_right(&rights, "Write", "File")?;
        let mut call = fs_call("file_write", None, &rights, source);
        call.arguments
            .insert("bytes".into(), FixtureValue::Bytes(encode_hex(bytes)));
        let written = outcome_count(
            self.dispatch_filesystem(
                call,
                FixtureOutcome::Return {
                    value: FixtureValue::String(bytes.len().to_string()),
                },
            ),
            "file_write",
            bytes.len(),
        )?;
        self.filesystem
            .nodes
            .insert(path, Node::File(bytes[..written].to_vec()));
        Ok(written)
    }

    fn write_from_dir(
        &mut self,
        handle: &FixtureHandle,
        relative: &str,
        bytes: &[u8],
        append: bool,
        operation: &str,
        source: Option<SourceLocation>,
    ) -> FilesystemProviderResult<usize> {
        let (base, rights, policy) = self.checked_dir(handle)?;
        require_right(&rights, "Write", "Dir")?;
        self.guard_policy(&policy, relative, false)?;
        let path = join_path(&base, relative)?;
        require_parent_directory(&self.filesystem.nodes, &path)?;
        if matches!(self.filesystem.nodes.get(&path), Some(Node::Directory)) {
            return Err(fs_failure(
                FixtureErrorCode::InvalidData,
                format!("`{relative}` is a directory"),
            ));
        }
        let mut call = fs_call(operation, Some(relative), &rights, source);
        call.arguments
            .insert("bytes".into(), FixtureValue::Bytes(encode_hex(bytes)));
        let written = outcome_count(
            self.dispatch_filesystem(
                call,
                FixtureOutcome::Return {
                    value: FixtureValue::String(bytes.len().to_string()),
                },
            ),
            operation,
            bytes.len(),
        )?;
        if append {
            let node = self
                .filesystem
                .nodes
                .entry(path)
                .or_insert_with(|| Node::File(Vec::new()));
            if let Node::File(existing) = node {
                existing.extend_from_slice(&bytes[..written]);
            }
        } else {
            self.filesystem
                .nodes
                .insert(path, Node::File(bytes[..written].to_vec()));
        }
        Ok(written)
    }

    fn open_from_dir(
        &mut self,
        handle: &FixtureHandle,
        relative: &str,
        create: bool,
        source: Option<SourceLocation>,
    ) -> FilesystemProviderResult<FixtureHandle> {
        let (base, rights, policy) = self.checked_dir(handle)?;
        let required = if create { "Write" } else { "Read" };
        require_right(&rights, required, "Dir")?;
        self.guard_policy(&policy, relative, false)?;
        let path = join_path(&base, relative)?;
        if create {
            require_parent_directory(&self.filesystem.nodes, &path)?;
            if matches!(self.filesystem.nodes.get(&path), Some(Node::Directory)) {
                return Err(fs_failure(
                    FixtureErrorCode::AlreadyExists,
                    format!("directory `{relative}` already exists"),
                ));
            }
        } else if !matches!(self.filesystem.nodes.get(&path), Some(Node::File(_))) {
            return Err(fs_failure(
                FixtureErrorCode::NotFound,
                format!("file `{relative}` was not found"),
            ));
        }
        let operation = if create { "dir_create" } else { "dir_open" };
        let call = fs_call(operation, Some(relative), &rights, source);
        outcome_marker(
            self.dispatch_filesystem(
                call,
                FixtureOutcome::Return {
                    value: FixtureValue::String("File".into()),
                },
            ),
            "File",
            operation,
        )?;
        if create {
            self.filesystem
                .nodes
                .insert(path.clone(), Node::File(Vec::new()));
        }
        let file_rights = BTreeSet::from([required.to_string()]);
        let file = self
            .basic
            .mint_handle(FixtureFamily::Filesystem, BTreeSet::new());
        self.filesystem.handles.insert(
            file.id(),
            HandleState::File {
                path,
                rights: file_rights,
            },
        );
        Ok(file)
    }

    fn checked_dir(
        &self,
        handle: &FixtureHandle,
    ) -> FilesystemProviderResult<(String, BTreeSet<String>, String)> {
        if !self
            .basic
            .validate_handle(handle, FixtureFamily::Filesystem)
        {
            return Err(fs_failure(
                FixtureErrorCode::PermissionDenied,
                "invalid or foreign Dir fixture handle",
            ));
        }
        self.filesystem
            .dir(handle)
            .map(|(base, rights, policy)| (base.to_string(), rights.clone(), policy.to_string()))
    }

    fn checked_file(
        &self,
        handle: &FixtureHandle,
    ) -> FilesystemProviderResult<(String, BTreeSet<String>)> {
        if !self
            .basic
            .validate_handle(handle, FixtureFamily::Filesystem)
        {
            return Err(fs_failure(
                FixtureErrorCode::PermissionDenied,
                "invalid or foreign File fixture handle",
            ));
        }
        self.filesystem
            .file(handle)
            .map(|(path, rights)| (path.to_string(), rights.clone()))
    }

    fn insert_dir_handle(
        &mut self,
        base: String,
        rights: BTreeSet<String>,
        policy: String,
    ) -> FilesystemProviderResult<FixtureHandle> {
        let handle = self
            .basic
            .mint_handle(FixtureFamily::Filesystem, BTreeSet::new());
        self.filesystem.handles.insert(
            handle.id(),
            HandleState::Dir {
                base,
                rights,
                policy,
            },
        );
        Ok(handle)
    }

    fn guard_policy(
        &self,
        policy: &str,
        relative: &str,
        is_dir: bool,
    ) -> FilesystemProviderResult<()> {
        if dir_admits(policy, relative, is_dir) {
            Ok(())
        } else {
            Err(fs_failure(
                FixtureErrorCode::PermissionDenied,
                format!("`{relative}` is not permitted by this Dir entry policy"),
            ))
        }
    }

    fn dispatch_filesystem(
        &mut self,
        call: FixtureCall,
        fallback: FixtureOutcome,
    ) -> FixtureOutcome {
        if self.has_script(FixtureFamily::Filesystem) {
            self.scripted_call(call)
        } else {
            self.observe(call, fallback)
        }
    }

    pub(crate) fn authorize_exec_target(
        &self,
        handle: &FixtureHandle,
        relative: &str,
    ) -> FilesystemProviderResult<Vec<String>> {
        let (base, rights, policy) = self.checked_dir(handle)?;
        require_right(&rights, "Read", "Dir")?;
        self.guard_policy(&policy, relative, false)?;
        let path = join_path(&base, relative)?;
        if !matches!(self.filesystem.nodes.get(&path), Some(Node::File(_))) {
            return Err(fs_failure(
                FixtureErrorCode::NotFound,
                format!("executable `{relative}` was not found in the fixture Dir"),
            ));
        }
        Ok(rights.into_iter().collect())
    }
}

fn fs_call(
    operation: &str,
    target: Option<&str>,
    rights: &BTreeSet<String>,
    source: Option<SourceLocation>,
) -> FixtureCall {
    let mut call = FixtureCall::new(FixtureFamily::Filesystem, operation);
    call.target = target.map(str::to_string);
    call.effective_rights = rights.iter().cloned().collect();
    call.source = source;
    call
}

fn require_right(
    rights: &BTreeSet<String>,
    right: &str,
    family: &str,
) -> FilesystemProviderResult<()> {
    if rights.contains(right) {
        Ok(())
    } else {
        Err(fs_failure(
            FixtureErrorCode::PermissionDenied,
            format!("this {family} capability does not grant {right}"),
        ))
    }
}

fn require_parent_directory(
    nodes: &BTreeMap<String, Node>,
    path: &str,
) -> FilesystemProviderResult<()> {
    let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
    match nodes.get(parent) {
        Some(Node::Directory) => Ok(()),
        Some(Node::File(_)) => Err(fs_failure(
            FixtureErrorCode::NotDirectory,
            format!("parent `{parent}` is not a directory"),
        )),
        None => Err(fs_failure(
            FixtureErrorCode::NotFound,
            format!("parent directory `{parent}` was not found"),
        )),
    }
}

fn join_path(base: &str, relative: &str) -> FilesystemProviderResult<String> {
    if relative.is_empty()
        || relative.starts_with('/')
        || relative.contains(['\\', '\0'])
        || relative
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(fs_failure(
            FixtureErrorCode::PermissionDenied,
            format!("`{relative}` escapes the Dir capability"),
        ));
    }
    Ok(if base.is_empty() {
        relative.to_string()
    } else {
        format!("{base}/{relative}")
    })
}

fn add_parent_directories(nodes: &mut BTreeMap<String, Node>, path: &str) {
    let mut offset = 0;
    while let Some(index) = path[offset..].find('/') {
        offset += index;
        nodes
            .entry(path[..offset].to_string())
            .or_insert(Node::Directory);
        offset += 1;
    }
}

fn outcome_marker(
    outcome: FixtureOutcome,
    expected: &str,
    operation: &str,
) -> FilesystemProviderResult<()> {
    match outcome {
        FixtureOutcome::Return {
            value: FixtureValue::String(value),
        } if value == expected => Ok(()),
        FixtureOutcome::Fail { error } => Err(error),
        _ => Err(fs_failure(
            FixtureErrorCode::InvalidData,
            format!("{operation} returned an invalid {expected} handle marker"),
        )),
    }
}

fn outcome_bytes(
    outcome: FixtureOutcome,
    operation: &str,
) -> FilesystemProviderResult<Vec<u8>> {
    match outcome {
        FixtureOutcome::Return {
            value: FixtureValue::Bytes(value),
        } => Ok(decode_hex(&value)),
        FixtureOutcome::Fail { error } => Err(error),
        _ => Err(fs_failure(
            FixtureErrorCode::InvalidData,
            format!("{operation} returned invalid bytes"),
        )),
    }
}

fn outcome_bool(outcome: FixtureOutcome, operation: &str) -> FilesystemProviderResult<bool> {
    match outcome {
        FixtureOutcome::Return {
            value: FixtureValue::Bool(value),
        } => Ok(value),
        FixtureOutcome::Fail { error } => Err(error),
        _ => Err(fs_failure(
            FixtureErrorCode::InvalidData,
            format!("{operation} returned an invalid boolean"),
        )),
    }
}

fn outcome_strings(
    outcome: FixtureOutcome,
    operation: &str,
) -> FilesystemProviderResult<Vec<String>> {
    match outcome {
        FixtureOutcome::Return {
            value: FixtureValue::List(values),
        } => values
            .into_iter()
            .map(|value| match value {
                FixtureValue::String(value) => Ok(value),
                _ => Err(fs_failure(
                    FixtureErrorCode::InvalidData,
                    format!("{operation} returned a non-string list item"),
                )),
            })
            .collect(),
        FixtureOutcome::Fail { error } => Err(error),
        _ => Err(fs_failure(
            FixtureErrorCode::InvalidData,
            format!("{operation} returned an invalid list"),
        )),
    }
}

fn outcome_unit(outcome: FixtureOutcome, operation: &str) -> FilesystemProviderResult<()> {
    match outcome {
        FixtureOutcome::Return {
            value: FixtureValue::Null,
        } => Ok(()),
        FixtureOutcome::Fail { error } => Err(error),
        _ => Err(fs_failure(
            FixtureErrorCode::InvalidData,
            format!("{operation} returned an invalid value"),
        )),
    }
}

fn outcome_count(
    outcome: FixtureOutcome,
    operation: &str,
    offered: usize,
) -> FilesystemProviderResult<usize> {
    match outcome {
        FixtureOutcome::Return {
            value: FixtureValue::String(value),
        } => value
            .parse::<usize>()
            .ok()
            .filter(|written| *written <= offered)
            .ok_or_else(|| {
                fs_failure(
                    FixtureErrorCode::InvalidData,
                    format!("{operation} returned an invalid partial byte count"),
                )
            }),
        FixtureOutcome::Fail { error } => Err(error),
        _ => Err(fs_failure(
            FixtureErrorCode::InvalidData,
            format!("{operation} returned an invalid byte count"),
        )),
    }
}

fn fs_failure(code: FixtureErrorCode, message: impl Into<String>) -> FixtureFailure {
    FixtureFailure {
        code,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{test_support, FilesystemFixture, TestResult};

    fn plan(rights: &[&str], policy: Option<&str>) -> FixturePlan {
        FixturePlan {
            filesystem: Some(FilesystemFixture {
                entries: BTreeMap::from([
                    ("cache".into(), FilesystemEntry::Directory),
                    (
                        "cache/a.txt".into(),
                        FilesystemEntry::File {
                            hex: "6869".into(),
                        },
                    ),
                    (
                        "secret.bin".into(),
                        FilesystemEntry::File { hex: "00ff".into() },
                    ),
                ]),
                rights: rights.iter().map(|right| (*right).into()).collect(),
                entry_policy: policy.map(str::to_string),
                script: Vec::new(),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn reads_writes_subdirs_and_file_handles_share_one_state() {
        let mut session =
            FixtureSession::new(plan(&["Read", "Write"], None)).expect("valid session");
        let root = session.mint_fixture_dir(None).expect("root");
        assert_eq!(session.dir_list(&root, None).expect("list"), vec!["cache", "secret.bin"]);
        let cache = session.dir_subdir(&root, "cache", None).expect("subdir");
        assert_eq!(session.dir_read(&cache, "a.txt", None).expect("read"), b"hi");
        assert_eq!(
            session.dir_write(&cache, "b.txt", b"new", None).expect("write"),
            3
        );
        let file = session.dir_open(&cache, "b.txt", None).expect("open");
        assert_eq!(session.file_read(&file, None).expect("file read"), b"new");
        let created = session.dir_create(&cache, "c.txt", None).expect("create");
        session.file_write(&created, b"value", None).expect("file write");
        assert_eq!(session.dir_read(&cache, "c.txt", None).expect("shared"), b"value");
    }

    #[test]
    fn rights_policy_escape_and_cross_session_handles_fail_closed() {
        let fixture = plan(&["Read"], Some("ext:.txt"));
        let mut first = FixtureSession::new(fixture.clone()).expect("first");
        let mut second = FixtureSession::new(fixture).expect("second");
        let root = first.mint_fixture_dir(None).expect("root");
        assert_eq!(
            first.dir_read(&root, "secret.bin", None).expect_err("policy").code,
            FixtureErrorCode::PermissionDenied
        );
        assert_eq!(
            first.dir_read(&root, "../secret.bin", None).expect_err("escape").code,
            FixtureErrorCode::PermissionDenied
        );
        assert_eq!(
            first.dir_write(&root, "x.txt", b"x", None).expect_err("rights").code,
            FixtureErrorCode::PermissionDenied
        );
        assert_eq!(
            second.dir_list(&root, None).expect_err("foreign").code,
            FixtureErrorCode::PermissionDenied
        );
    }

    #[test]
    fn scripted_partial_write_and_failure_are_observable() {
        let mut fixture = plan(&["Read", "Write"], None);
        fixture.filesystem.as_mut().expect("fixture").script = vec![
            test_support::step(
                "mint_dir", None, BTreeMap::new(),
                Some(vec!["Read".into(), "Write".into()]),
                test_support::returned("Dir"),
            ),
            test_support::step(
                "dir_write", Some("partial.txt"), BTreeMap::from([(
                    "bytes".into(),
                    FixtureValue::Bytes("61626364".into()),
                )]), Some(vec!["Read".into(), "Write".into()]),
                test_support::returned("2"),
            ),
            test_support::step(
                "dir_read_len", Some("partial.txt"), BTreeMap::new(),
                Some(vec!["Read".into(), "Write".into()]),
                FixtureOutcome::Fail { error: fs_failure(FixtureErrorCode::Timeout, "configured timeout") },
            ),
        ];
        fixture.expectations.require_complete_scripts = true;
        let mut session = FixtureSession::new(fixture).expect("session");
        let root = session.mint_fixture_dir(None).expect("root");
        assert_eq!(
            session
                .dir_write(&root, "partial.txt", b"abcd", None)
                .expect("partial"),
            2
        );
        assert_eq!(
            session.dir_read(&root, "partial.txt", None).expect_err("timeout").code,
            FixtureErrorCode::Timeout
        );
        assert!(matches!(
            session.finish(TestResult::Passed).result,
            TestResult::Passed
        ));
    }
}
