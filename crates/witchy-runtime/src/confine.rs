//! Handle-anchored filesystem confinement for `Dir`, `File`, and build
//! capabilities.
//!
//! Ambient paths are consumed exactly once, when the host admits a root grant.
//! Every guest-directed operation after that is relative to an open directory
//! handle. `cap-std` performs the platform-specific component walk and rejects
//! paths which leave that handle, so renaming a checked parent or swapping it for
//! a symlink cannot redirect a later operation. The interpreter oracle and the
//! compiled-Wasm host carry these same types and call these same methods.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// A confinement violation (an escape attempt, or an inaccessible base/target).
/// Carries the human-readable message both backends surface, and the kind a
/// fallible capability operation reports as its `CapError` variant.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfineError {
    pub kind: ConfineErrorKind,
    pub message: String,
}

/// Why a confined operation failed, in `CapError` variant order (RFC-0126
/// rule 3): the capability does not cover the request, or the request was
/// covered and the filesystem said no.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfineErrorKind {
    /// An escaping, absolute, or aliased path, or an entry the policy excludes.
    Denied,
    NotFound,
    /// A directory where a file was expected.
    NotAFile,
    AlreadyExists,
    /// Any other failure, including contents that are not UTF-8.
    Io,
}

impl ConfineErrorKind {
    /// The kind of a host I/O failure. The OS refusing access, including
    /// `cap-std` refusing a symlink that leaves the root, is a denial.
    pub fn of_io(error: &std::io::Error) -> Self {
        match error.kind() {
            std::io::ErrorKind::NotFound => Self::NotFound,
            std::io::ErrorKind::PermissionDenied => Self::Denied,
            std::io::ErrorKind::IsADirectory => Self::NotAFile,
            std::io::ErrorKind::AlreadyExists => Self::AlreadyExists,
            _ => Self::Io,
        }
    }

    /// The host response status that selects this `CapError` variant: its
    /// ordinal plus one (0 is success).
    pub fn status(self) -> u8 {
        match self {
            Self::Denied => 1,
            Self::NotFound => 2,
            Self::NotAFile => 3,
            Self::AlreadyExists => 4,
            Self::Io => 5,
        }
    }
}

impl ConfineError {
    pub fn new(kind: ConfineErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }

    /// A request the capability does not cover.
    pub fn denied(message: impl Into<String>) -> Self {
        Self::new(ConfineErrorKind::Denied, message)
    }
}

impl std::fmt::Display for ConfineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ConfineError {}

fn err<T>(message: impl Into<String>) -> Result<T, ConfineError> {
    Err(ConfineError::denied(message))
}

#[cfg(target_arch = "wasm32")]
fn unavailable<T>() -> Result<T, ConfineError> {
    Err(ConfineError::new(
        ConfineErrorKind::Io,
        "host filesystem capabilities are unavailable on this target",
    ))
}

fn validate_relative(path: &Path) -> Result<(), ConfineError> {
    if path.is_absolute() {
        return err("absolute paths are not allowed (a Dir capability is a subtree)");
    }
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => return err("`..` escapes the Dir capability"),
            Component::RootDir | Component::Prefix(_) => {
                return err("invalid path component in a Dir-relative path");
            }
        }
    }
    Ok(())
}

fn denotes_self(path: &Path) -> bool {
    path.components().all(|component| component == Component::CurDir)
}

fn inaccessible(path: &Path, error: std::io::Error) -> ConfineError {
    ConfineError::new(
        ConfineErrorKind::of_io(&error),
        format!("cannot access `{}`: {error}", path.display()),
    )
}

#[cfg(target_arch = "wasm32")]
fn unsupported() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "host filesystem capabilities are unavailable on this target",
    )
}

/// An admitted directory grant backed by an open directory handle.
///
/// `display` is diagnostic provenance only. It is never used for authority or
/// filesystem access after construction.
#[derive(Clone)]
pub struct ConfinedDir {
    #[cfg(not(target_arch = "wasm32"))]
    inner: Arc<cap_std::fs::Dir>,
    display: Arc<PathBuf>,
}

/// The no-follow kind of one direct filesystem entry. Entry policies operate on
/// this identity rather than on a caller-supplied guess about the path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfinedEntryKind {
    File,
    Directory,
    Other,
}

impl std::fmt::Debug for ConfinedDir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ConfinedDir").field(&self.display).finish()
    }
}

impl PartialEq for ConfinedDir {
    fn eq(&self, other: &Self) -> bool {
        self.display == other.display
    }
}

impl Eq for ConfinedDir {}

impl ConfinedDir {
    /// Consume ambient authority while admitting a host-provided root.
    pub fn open_ambient(path: &Path) -> Result<Self, ConfineError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let inner = cap_std::fs::Dir::open_ambient_dir(
                path,
                cap_std::ambient_authority(),
            )
            .map_err(|error| {
                ConfineError::new(ConfineErrorKind::of_io(&error), format!("invalid Dir base `{}`: {error}", path.display()))
            })?;
            Ok(Self {
                inner: Arc::new(inner),
                display: Arc::new(path.to_path_buf()),
            })
        }
        #[cfg(target_arch = "wasm32")]
        {
            Ok(Self {
                display: Arc::new(path.to_path_buf()),
            })
        }
    }

    /// Diagnostic provenance for this authority. Never use this path to open.
    pub fn display_path(&self) -> &Path {
        self.display.as_path()
    }

    /// Attenuate to an already-existing subdirectory.
    pub fn open_dir(&self, rel: &str) -> Result<Self, ConfineError> {
        let path = Path::new(rel);
        validate_relative(path)?;
        if denotes_self(path) {
            return Ok(self.clone());
        }
        let display = self.display.join(path);
        #[cfg(not(target_arch = "wasm32"))]
        {
            let inner = self
                .inner
                .open_dir(path)
                .map_err(|error| inaccessible(&display, error))?;
            Ok(Self {
                inner: Arc::new(inner),
                display: Arc::new(display),
            })
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = display;
            unavailable()
        }
    }

    /// Bind one relative file name to an anchored parent handle.
    ///
    /// When `must_exist` is true the file is opened now, preserving `Dir.open`'s
    /// eager failure. Later reads still reopen relative to the retained parent so
    /// the capability continues to denote the name while remaining confined.
    pub fn file(&self, rel: &str, must_exist: bool) -> Result<ConfinedFile, ConfineError> {
        let path = Path::new(rel);
        validate_relative(path)?;
        let name = path.file_name().ok_or_else(|| {
            ConfineError::new(ConfineErrorKind::NotAFile, format!("cannot access `{}`: path does not name a file", self.display.join(path).display()))
        })?;
        let parent_path = path.parent().unwrap_or_else(|| Path::new(""));
        let parent = if parent_path.as_os_str().is_empty() {
            self.clone()
        } else {
            let parent = parent_path.to_str().ok_or_else(|| {
                ConfineError::new(ConfineErrorKind::Io, format!(
                    "cannot access `{}`: path is not valid UTF-8",
                    self.display.join(parent_path).display()
                ))
            })?;
            self.open_dir(parent)?
        };
        let file = ConfinedFile {
            display: self.display.join(path),
            parent,
            name: PathBuf::from(name),
            no_follow: false,
        };
        if must_exist {
            file.open_read_handle()
                .map_err(|error| inaccessible(&file.display, error))?;
        }
        Ok(file)
    }

    /// Test for an entry without exposing an ambient path.
    pub fn exists(&self, rel: &str) -> Result<bool, ConfineError> {
        let path = Path::new(rel);
        validate_relative(path)?;
        if denotes_self(path) {
            return Ok(true);
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.inner
                .try_exists(path)
                .map_err(|error| inaccessible(&self.display.join(path), error))
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = path;
            unavailable()
        }
    }

    /// Test whether an entry is a directory without exposing an ambient path.
    pub fn is_dir(&self, rel: &str) -> Result<bool, ConfineError> {
        let path = Path::new(rel);
        validate_relative(path)?;
        if denotes_self(path) {
            return Ok(true);
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            Ok(self.inner.is_dir(path))
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = path;
            unavailable()
        }
    }

    /// Reject an absolute or `..` path before any filesystem access, so the
    /// refusal cannot depend on what exists outside the capability.
    pub fn check_relative(rel: &str) -> Result<(), ConfineError> {
        validate_relative(Path::new(rel))
    }

    /// Inspect an entry without following its leaf and require the caller's
    /// spelling to be the actual directory-entry spelling. This closes both
    /// symlink and case-folding aliases for a finer-grained `Dir.only` policy.
    /// A missing entry is `Ok(None)`; confinement and I/O failures remain loud.
    pub fn entry_kind_exact(&self, rel: &str) -> Result<Option<ConfinedEntryKind>, ConfineError> {
        match self.entry_identity(rel)? {
            Some((_, false)) => err(format!(
                "`{rel}` resolves through a differently named filesystem entry"
            )),
            Some((kind, true)) => Ok(Some(kind)),
            None => Ok(None),
        }
    }

    /// `entry_kind_exact` without the refusal: a differently spelled entry
    /// (a case-folding alias) is `Other`, an entry no policy can admit.
    pub fn entry_kind_spelled(&self, rel: &str) -> Result<Option<ConfinedEntryKind>, ConfineError> {
        Ok(self.entry_identity(rel)?.map(|(kind, exact)| if exact { kind } else { ConfinedEntryKind::Other }))
    }

    /// The no-follow kind of `rel`'s leaf and whether the caller spelled it
    /// exactly as its directory entry.
    fn entry_identity(&self, rel: &str) -> Result<Option<(ConfinedEntryKind, bool)>, ConfineError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let file = self.file(rel, false)?;
            let metadata = match file.parent.inner.symlink_metadata(&file.name) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(inaccessible(&file.display, error)),
            };
            let requested = file.name.file_name().ok_or_else(|| {
                ConfineError::new(ConfineErrorKind::NotAFile, format!("cannot access `{}`: path does not name an entry", file.display.display()))
            })?;
            let exact = file
                .parent
                .inner
                .entries()
                .map_err(|error| inaccessible(&file.display, error))?
                .filter_map(Result::ok)
                .any(|entry| entry.file_name() == requested);
            let file_type = metadata.file_type();
            let kind = if file_type.is_file() {
                ConfinedEntryKind::File
            } else if file_type.is_dir() {
                ConfinedEntryKind::Directory
            } else {
                ConfinedEntryKind::Other
            };
            Ok(Some((kind, exact)))
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = rel;
            unavailable()
        }
    }

    /// Return immediate UTF-8 entry names in deterministic order.
    pub fn entries(&self) -> std::io::Result<Vec<String>> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut names = self
                .inner
                .entries()?
                .map(|entry| {
                    entry?.file_name().into_string().map_err(|_| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "directory entry name is not valid UTF-8",
                        )
                    })
                })
                .collect::<std::io::Result<Vec<_>>>()?;
            names.sort();
            Ok(names)
        }
        #[cfg(target_arch = "wasm32")]
        {
            Err(unsupported())
        }
    }

    /// Create one confined directory path, idempotently.
    pub fn make_dir(&self, rel: &str) -> Result<(), ConfineError> {
        let path = Path::new(rel);
        validate_relative(path)?;
        if denotes_self(path) {
            return Ok(());
        }
        let display = self.display.join(path);
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.inner
                .create_dir_all(path)
                .map_err(|error| inaccessible(&display, error))
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = display;
            unavailable()
        }
    }

    /// Atomically create `rel` with `contents` if and only if it does not already
    /// exist (a single `O_CREAT|O_EXCL` open). Returns `Ok(true)` when this call
    /// created the file and `Ok(false)` when it was already present — the
    /// race-loser signal. Two concurrent `create_new`s of the same path see
    /// exactly one `true` and one `false`, so a marker written this way is
    /// single-owner (RFC-0118). The parent directory must already exist.
    pub fn create_new(&self, rel: &str, contents: &[u8]) -> Result<bool, ConfineError> {
        let file = self.file(rel, false)?;
        #[cfg(not(target_arch = "wasm32"))]
        {
            file.create_new_all(contents)
                .map_err(|error| inaccessible(&file.display, error))
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = contents;
            unavailable()
        }
    }

    /// Atomically replace `rel`'s contents with `contents` (creating it if
    /// absent), by writing a unique temp sibling and renaming it over `rel`. A
    /// concurrent reader observes either the whole old file or the whole new
    /// file, never a torn or absent intermediate (RFC-0118).
    pub fn replace(&self, rel: &str, contents: &[u8]) -> Result<(), ConfineError> {
        let file = self.file(rel, false)?;
        #[cfg(not(target_arch = "wasm32"))]
        {
            file.replace_all(contents)
                .map_err(|error| inaccessible(&file.display, error))
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = contents;
            unavailable()
        }
    }

    /// Atomically move `from` to `to` within this authority, replacing `to` if it
    /// exists (POSIX `renameat` replace semantics). Both paths stay confined to
    /// the Dir subtree; cross-authority rename is not offered (RFC-0118).
    pub fn rename(&self, from: &str, to: &str) -> Result<(), ConfineError> {
        let src = self.file(from, false)?;
        let dst = self.file(to, false)?;
        #[cfg(not(target_arch = "wasm32"))]
        {
            src.parent
                .inner
                .rename(&src.name, &dst.parent.inner, &dst.name)
                .map_err(|error| inaccessible(&dst.display, error))
        }
        #[cfg(target_arch = "wasm32")]
        {
            unavailable()
        }
    }
}

/// A fixed file name paired with its already-open, confined parent directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfinedFile {
    parent: ConfinedDir,
    name: PathBuf,
    display: PathBuf,
    /// Set when an entry policy admitted this file on its no-follow identity:
    /// a later read must not follow a symlink swapped in at the name.
    no_follow: bool,
}

impl ConfinedFile {
    /// Admit a direct host `File` grant without retaining its ambient path as
    /// authority.
    pub fn open_ambient(path: &Path) -> Result<Self, ConfineError> {
        let name = path.file_name().ok_or_else(|| {
            ConfineError::new(ConfineErrorKind::NotAFile, format!("invalid File grant `{}`: path does not name a file", path.display()))
        })?;
        let parent_path = path.parent().unwrap_or_else(|| Path::new("."));
        let parent = ConfinedDir::open_ambient(parent_path)?;
        Ok(Self {
            parent,
            name: PathBuf::from(name),
            display: path.to_path_buf(),
            no_follow: false,
        })
    }

    /// Validate a direct host grant's existing leaf and declared access without
    /// surrendering the parent handle retained by this authority.
    pub fn validate_access(&self, read: bool, write: bool) -> Result<(), ConfineError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let metadata = self
                .parent
                .inner
                .metadata(&self.name)
                .map_err(|error| inaccessible(&self.display, error))?;
            if !metadata.is_file() {
                return Err(ConfineError::new(ConfineErrorKind::NotAFile, format!("File grant `{}` is not a file", self.display.display())));
            }
            if read {
                self.open_read_handle()
                    .map_err(|error| inaccessible(&self.display, error))?;
            }
            if write {
                use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
                let mut options = cap_std::fs::OpenOptions::new();
                options.write(true).follow(FollowSymlinks::No);
                self.parent
                    .inner
                    .open_with(&self.name, &options)
                    .map_err(|error| inaccessible(&self.display, error))?;
            }
            Ok(())
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (read, write);
            unavailable()
        }
    }

    /// Diagnostic provenance for this authority. Never use this path to open.
    pub fn display_path(&self) -> &Path {
        &self.display
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn open_read_handle(&self) -> std::io::Result<cap_std::fs::File> {
        self.parent.inner.open(&self.name)
    }

    #[cfg(target_arch = "wasm32")]
    fn open_read_handle(&self) -> std::io::Result<()> {
        Err(unsupported())
    }

    /// macOS will not execute `/dev/fd`, and copied Apple platform binaries are
    /// killed because the trust cache is path-bound. Permit the original path
    /// only when it still names the opened inode and every directory which can
    /// redirect it is root-owned and non-writable. User-mutable trees always use
    /// the opened-file snapshot below.
    #[cfg(target_os = "macos")]
    fn immutable_system_exec_path(
        &self,
        opened: &cap_std::fs::File,
    ) -> Option<PathBuf> {
        use cap_std::fs::MetadataExt as _;
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let path = std::fs::canonicalize(&self.display).ok()?;
        let opened_meta = opened.metadata().ok()?;
        let path_meta = std::fs::metadata(&path).ok()?;
        if opened_meta.dev() != path_meta.dev() || opened_meta.ino() != path_meta.ino() {
            return None;
        }
        for ancestor in path.parent()?.ancestors() {
            let metadata = std::fs::symlink_metadata(ancestor).ok()?;
            if metadata.uid() != 0 || metadata.permissions().mode() & 0o022 != 0 {
                return None;
            }
        }
        Some(path)
    }

    pub fn read_to_string(&self) -> std::io::Result<String> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            use std::io::Read;
            let mut file = self.open_read_handle()?;
            let mut contents = String::new();
            file.read_to_string(&mut contents)?;
            Ok(contents)
        }
        #[cfg(target_arch = "wasm32")]
        {
            Err(unsupported())
        }
    }

    /// `read_bytes` without following a symlink leaf, so an entry policy
    /// judged on the leaf's no-follow identity still holds if the name is
    /// swapped for a link after that judgment.
    pub fn read_bytes_no_follow(&self) -> std::io::Result<Vec<u8>> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
            use std::io::Read;
            let mut options = cap_std::fs::OpenOptions::new();
            options.read(true).follow(FollowSymlinks::No);
            let mut file = self.parent.inner.open_with(&self.name, &options)?;
            let mut contents = Vec::new();
            file.read_to_end(&mut contents)?;
            Ok(contents)
        }
        #[cfg(target_arch = "wasm32")]
        {
            Err(unsupported())
        }
    }

    /// This file as admitted by an entry policy: its reads never follow a
    /// symlink leaf, even one swapped in after the policy judged the name.
    pub fn without_following(mut self) -> Self {
        self.no_follow = true;
        self
    }

    /// Read this file's bytes for a guest that branches on the failure
    /// (RFC-0126 rule 3). A failed read through a symlink leaf is `Denied`
    /// whatever its target is, so a link leaving the root never reports
    /// whether its outside target exists; any other failure is classified by
    /// its I/O kind.
    pub fn read_bytes_classified(&self) -> Result<Vec<u8>, ConfineError> {
        let read = if self.no_follow { self.read_bytes_no_follow() } else { self.read_bytes() };
        read.map_err(|error| {
            let through_link = self.name.to_str().is_some_and(|name| {
                matches!(self.parent.entry_kind_spelled(name), Ok(Some(ConfinedEntryKind::Other)))
            });
            let kind = if through_link { ConfineErrorKind::Denied } else { ConfineErrorKind::of_io(&error) };
            ConfineError::new(kind, format!("read failed for `{}`: {error}", self.display.display()))
        })
    }

    /// Read the confined file's RAW bytes without UTF-8 validation — for binary
    /// artifacts (RFC-0095). Same confinement as `read_to_string`.
    pub fn read_bytes(&self) -> std::io::Result<Vec<u8>> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            use std::io::Read;
            let mut file = self.open_read_handle()?;
            let mut contents = Vec::new();
            file.read_to_end(&mut contents)?;
            Ok(contents)
        }
        #[cfg(target_arch = "wasm32")]
        {
            Err(unsupported())
        }
    }

    pub fn write_all(&self, contents: &[u8]) -> std::io::Result<()> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
            use std::io::Write;
            let mut options = cap_std::fs::OpenOptions::new();
            options
                .write(true)
                .create(true)
                .truncate(true)
                .follow(FollowSymlinks::No);
            self.parent
                .inner
                .open_with(&self.name, &options)?
                .write_all(contents)
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = contents;
            Err(unsupported())
        }
    }

    pub fn append_all(&self, contents: &[u8]) -> std::io::Result<()> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
            use std::io::Write;
            let mut options = cap_std::fs::OpenOptions::new();
            options
                .append(true)
                .create(true)
                .follow(FollowSymlinks::No);
            self.parent
                .inner
                .open_with(&self.name, &options)?
                .write_all(contents)
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = contents;
            Err(unsupported())
        }
    }

    /// Add the execute bits (`+x`, i.e. `chmod` the mode up to include `0o111`) to
    /// this confined file, leaving the read/write bits untouched. Symlinks are never
    /// followed. Backs `dir.set_executable` (RFC-0095): a freshly installed
    /// trusted-publisher is written non-executable, and the installer flips it runnable.
    pub fn set_executable(&self) -> std::io::Result<()> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
            use cap_std::fs::PermissionsExt as _;
            let mut options = cap_std::fs::OpenOptions::new();
            options.read(true).follow(FollowSymlinks::No);
            let file = self.parent.inner.open_with(&self.name, &options)?;
            let mut permissions = file.metadata()?.permissions();
            permissions.set_mode(permissions.mode() | 0o111);
            file.set_permissions(permissions)
        }
        #[cfg(target_arch = "wasm32")]
        {
            Err(unsupported())
        }
    }

    /// Atomically create this file with `contents` iff absent (`O_CREAT|O_EXCL`,
    /// symlinks never followed). `Ok(true)` = created here; `Ok(false)` = it was
    /// already present. Backs `ConfinedDir::create_new` (RFC-0118).
    #[cfg(not(target_arch = "wasm32"))]
    fn create_new_all(&self, contents: &[u8]) -> std::io::Result<bool> {
        use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
        use std::io::Write;
        let mut options = cap_std::fs::OpenOptions::new();
        options.write(true).create_new(true).follow(FollowSymlinks::No);
        match self.parent.inner.open_with(&self.name, &options) {
            Ok(mut file) => {
                file.write_all(contents)?;
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Atomically replace this file's contents by writing a unique temp sibling
    /// and renaming it over the name — the reader never sees a torn write. Backs
    /// `ConfinedDir::replace` (RFC-0118).
    #[cfg(not(target_arch = "wasm32"))]
    fn replace_all(&self, contents: &[u8]) -> std::io::Result<()> {
        use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
        use std::io::Write;
        let temp = self.temp_sibling();
        let mut options = cap_std::fs::OpenOptions::new();
        options.write(true).create_new(true).follow(FollowSymlinks::No);
        {
            let mut file = self.parent.inner.open_with(&temp, &options)?;
            file.write_all(contents)?;
        }
        // Rename the finished temp over the target: a concurrent reader of `name`
        // observes all-old or all-new, never the half-written temp.
        match self.parent.inner.rename(&temp, &self.parent.inner, &self.name) {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = self.parent.inner.remove_file(&temp);
                Err(error)
            }
        }
    }

    /// A collision-free temp name beside this file: distinct per process (pid)
    /// and per call within a process (an atomic counter), so concurrent workers —
    /// separate processes, distinct pids — never share a temp path.
    #[cfg(not(target_arch = "wasm32"))]
    fn temp_sibling(&self) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);
        let n = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let base = self.name.file_name().and_then(|s| s.to_str()).unwrap_or("tmp");
        PathBuf::from(format!(".{base}.tmp.{}.{n}", std::process::id()))
    }

    /// Execute this already-confined file without reopening its original ambient
    /// pathname. Linux passes the open executable as fd 3 so scripts keep working
    /// after the kernel starts their shebang interpreter.
    #[cfg(all(
        any(target_os = "linux", target_os = "android"),
        not(target_arch = "wasm32")
    ))]
    pub fn run(&self, args: &[&str], stdin: &str) -> std::io::Result<std::process::Output> {
        use cap_std_ext::cmdext::{CapStdExtCommandExt, CmdFds};
        use std::os::fd::OwnedFd;
        use std::process::{Command, Stdio};

        let std_file = self.open_read_handle()?.into_std();
        let executable = Arc::new(OwnedFd::from(std_file));
        let executable_path = "/proc/self/fd/3";
        let mut fds = CmdFds::new();
        fds.take_fd_n(executable, 3);
        let mut command = Command::new(executable_path);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .take_fds(fds);
        Self::collect_output_with_input(command.spawn()?, stdin)
    }

    /// macOS and the other supported Unix hosts lack a public descriptor-exec
    /// primitive (`/dev/fd` is non-executable on macOS). Snapshot the already-open
    /// file into a private temporary directory and execute that stable name. The
    /// original capability path is never reopened, and the directory remains
    /// alive until the child exits.
    #[cfg(all(
        unix,
        not(any(target_os = "linux", target_os = "android")),
        not(target_arch = "wasm32")
    ))]
    pub fn run(&self, args: &[&str], stdin: &str) -> std::io::Result<std::process::Output> {
        use cap_std::fs::PermissionsExt as _;
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt as _;
        use std::process::{Command, Stdio};

        let mut source = self.open_read_handle()?;
        let mode = source.metadata()?.permissions().mode();
        if mode & 0o111 == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "confined executable does not have an execute bit",
            ));
        }
        #[cfg(target_os = "macos")]
        if let Some(path) = self.immutable_system_exec_path(&source) {
            let child = Command::new(path)
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;
            return Self::collect_output_with_input(child, stdin);
        }
        let temp = tempfile::Builder::new()
            .prefix("witchy-confined-exec-")
            .tempdir()?;
        let executable_path = temp.path().join("program");
        let mut executable = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&executable_path)?;
        std::io::copy(&mut source, &mut executable)?;
        executable.set_permissions(std::fs::Permissions::from_mode(0o700))?;
        drop(executable);

        let child = Command::new(&executable_path)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let output = Self::collect_output_with_input(child, stdin);
        drop(temp);
        output
    }

    /// Feed stdin while wait_with_output concurrently drains both output pipes.
    /// In particular, a child echoing more than a pipe's capacity must not block
    /// our stdin writer while we wait to begin reading its stdout. Even when
    /// writing fails, finish collecting/reaping before reporting that error.
    #[cfg(all(unix, not(target_arch = "wasm32")))]
    fn collect_output_with_input(
        mut child: std::process::Child, stdin: &str,
    ) -> std::io::Result<std::process::Output> {
        use std::io::Write;
        let input = child.stdin.take();
        std::thread::scope(|scope| {
            let writer = match std::thread::Builder::new()
                .name("witchy-tool-stdin".into())
                .spawn_scoped(scope, move || -> std::io::Result<()> {
                    if let Some(mut input) = input {
                        input.write_all(stdin.as_bytes())?;
                    }
                    // Dropping the pipe signals EOF on success and failure.
                    Ok(())
                })
            {
                Ok(writer) => writer,
                Err(error) => {
                    // No writer exists to drive the child. Do not leave a
                    // subprocess behind if the host cannot create the thread.
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
            };
            let output = child.wait_with_output();
            let written = writer.join()
                .map_err(|_| std::io::Error::other("confined executable stdin writer panicked"))?;
            written?;
            output
        })
    }

    #[cfg(any(not(unix), target_arch = "wasm32"))]
    pub fn run(&self, _args: &[&str], _stdin: &str) -> std::io::Result<std::process::Output> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "race-free confined executable launch is unavailable on this target",
        ))
    }
}

#[cfg(all(feature = "self-checks", unix))]
#[doc(hidden)]
pub mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("witchy-confine-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn join(&self, path: &str) -> PathBuf {
            self.0.join(path)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub fn lexical_escape_is_rejected() {
        let scratch = Scratch::new();
        let root = ConfinedDir::open_ambient(&scratch.0).unwrap();
        assert!(root.file("../secret", true).is_err());
        assert!(root.file("/etc/passwd", true).is_err());
        assert!(root.open_dir("a/../../b").is_err());
        assert!(root.make_dir("../outside").is_err());
    }

    pub fn a_fresh_operation_rejects_a_swapped_parent() {
        let scratch = Scratch::new();
        std::fs::create_dir(scratch.join("root")).unwrap();
        std::fs::create_dir(scratch.join("root/parent")).unwrap();
        std::fs::create_dir(scratch.join("outside")).unwrap();
        std::fs::write(scratch.join("outside/secret"), "private").unwrap();
        let root = ConfinedDir::open_ambient(&scratch.join("root")).unwrap();

        std::fs::rename(scratch.join("root/parent"), scratch.join("root/original")).unwrap();
        std::os::unix::fs::symlink("../../outside", scratch.join("root/parent")).unwrap();

        assert!(root.file("parent/secret", true).is_err());
        assert!(root.file("parent/secret", false).is_err());
        assert!(root.open_dir("parent").is_err());
        assert!(!root.exists("parent/secret").unwrap_or(false));
        assert!(!root.is_dir("parent").unwrap_or(false));
        assert_eq!(std::fs::read_to_string(scratch.join("outside/secret")).unwrap(), "private");
    }

    pub fn retained_file_and_subdir_authority_survive_name_replacement() {
        let scratch = Scratch::new();
        std::fs::create_dir(scratch.join("root")).unwrap();
        std::fs::create_dir(scratch.join("root/parent")).unwrap();
        std::fs::create_dir(scratch.join("outside")).unwrap();
        std::fs::write(scratch.join("root/parent/file"), "inside").unwrap();
        std::fs::write(scratch.join("outside/file"), "outside").unwrap();
        let root = ConfinedDir::open_ambient(&scratch.join("root")).unwrap();
        let file = root.file("parent/file", true).unwrap();
        let subdir = root.open_dir("parent").unwrap();

        std::fs::rename(scratch.join("root/parent"), scratch.join("root/original")).unwrap();
        std::os::unix::fs::symlink("../../outside", scratch.join("root/parent")).unwrap();

        file.write_all(b"retained").unwrap();
        subdir.file("new", false).unwrap().write_all(b"new").unwrap();
        assert_eq!(std::fs::read_to_string(scratch.join("root/original/file")).unwrap(), "retained");
        assert_eq!(std::fs::read_to_string(scratch.join("root/original/new")).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(scratch.join("outside/file")).unwrap(), "outside");
    }

    pub fn a_symlink_leaf_is_never_followed_for_writes() {
        let scratch = Scratch::new();
        let root = ConfinedDir::open_ambient(&scratch.0).unwrap();
        std::fs::write(scratch.join("secret"), "private").unwrap();
        std::os::unix::fs::symlink("secret", scratch.join("leaf")).unwrap();
        let leaf = root.file("leaf", false).unwrap();
        assert!(leaf.write_all(b"changed").is_err());
        assert!(leaf.append_all(b"changed").is_err());
        assert_eq!(std::fs::read_to_string(scratch.join("secret")).unwrap(), "private");
    }

    pub fn exact_entry_identity_rejects_symlink_and_case_aliases() {
        let scratch = Scratch::new();
        let root = ConfinedDir::open_ambient(&scratch.0).unwrap();
        std::fs::write(scratch.join("secret.key"), "private").unwrap();
        std::os::unix::fs::symlink("secret.key", scratch.join("alias.txt")).unwrap();

        assert_eq!(
            root.entry_kind_exact("secret.key").unwrap(),
            Some(ConfinedEntryKind::File)
        );
        assert_eq!(
            root.entry_kind_exact("alias.txt").unwrap(),
            Some(ConfinedEntryKind::Other)
        );

        std::fs::write(scratch.join("classified.TXT"), "case-private").unwrap();
        if root.exists("classified.txt").unwrap_or(false) {
            assert!(
                root.entry_kind_exact("classified.txt").is_err(),
                "a case-folded alias must not satisfy an exact entry policy"
            );
        }
    }
}
