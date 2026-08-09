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
/// Carries the human-readable message both backends surface.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfineError(pub String);

impl std::fmt::Display for ConfineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfineError {}

fn err<T>(message: impl Into<String>) -> Result<T, ConfineError> {
    Err(ConfineError(message.into()))
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
    ConfineError(format!("cannot access `{}`: {error}", path.display()))
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
                ConfineError(format!("invalid Dir base `{}`: {error}", path.display()))
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
            err("host filesystem capabilities are unavailable on this target")
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
            ConfineError(format!("cannot access `{}`: path does not name a file", self.display.join(path).display()))
        })?;
        let parent_path = path.parent().unwrap_or_else(|| Path::new(""));
        let parent = if parent_path.as_os_str().is_empty() {
            self.clone()
        } else {
            let parent = parent_path.to_str().ok_or_else(|| {
                ConfineError(format!(
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
            err("host filesystem capabilities are unavailable on this target")
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
            err("host filesystem capabilities are unavailable on this target")
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
            err("host filesystem capabilities are unavailable on this target")
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
            err("host filesystem capabilities are unavailable on this target")
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
            err("host filesystem capabilities are unavailable on this target")
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
            err("host filesystem capabilities are unavailable on this target")
        }
    }
}

/// A fixed file name paired with its already-open, confined parent directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfinedFile {
    parent: ConfinedDir,
    name: PathBuf,
    display: PathBuf,
}

impl ConfinedFile {
    /// Admit a direct host `File` grant without retaining its ambient path as
    /// authority.
    pub fn open_ambient(path: &Path) -> Result<Self, ConfineError> {
        let name = path.file_name().ok_or_else(|| {
            ConfineError(format!("invalid File grant `{}`: path does not name a file", path.display()))
        })?;
        let parent_path = path.parent().unwrap_or_else(|| Path::new("."));
        let parent = ConfinedDir::open_ambient(parent_path)?;
        Ok(Self {
            parent,
            name: PathBuf::from(name),
            display: path.to_path_buf(),
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
                return err(format!("File grant `{}` is not a file", self.display.display()));
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
            err("host filesystem capabilities are unavailable on this target")
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
        let mut child = command.spawn()?;
        if let Some(mut input) = child.stdin.take() {
            use std::io::Write;
            input.write_all(stdin.as_bytes())?;
        }
        child.wait_with_output()
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
            let mut child = Command::new(path)
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;
            if let Some(mut input) = child.stdin.take() {
                input.write_all(stdin.as_bytes())?;
            }
            return child.wait_with_output();
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

        let mut child = Command::new(&executable_path)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        if let Some(mut input) = child.stdin.take() {
            input.write_all(stdin.as_bytes())?;
        }
        let output = child.wait_with_output();
        drop(temp);
        output
    }

    #[cfg(any(not(unix), target_arch = "wasm32"))]
    pub fn run(&self, _args: &[&str], _stdin: &str) -> std::io::Result<std::process::Output> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "race-free confined executable launch is unavailable on this target",
        ))
    }
}

#[cfg(all(test, unix))]
mod tests {
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

    #[test]
    fn regular_files_and_directories_work() {
        let scratch = Scratch::new();
        std::fs::create_dir(scratch.join("sub")).unwrap();
        std::fs::write(scratch.join("sub/input"), "hello").unwrap();
        let root = ConfinedDir::open_ambient(&scratch.0).unwrap();
        assert_eq!(root.file("sub/input", true).unwrap().read_to_string().unwrap(), "hello");
        let output = root.file("sub/output", false).unwrap();
        output.write_all(b"one").unwrap();
        output.append_all(b" two").unwrap();
        assert_eq!(std::fs::read_to_string(scratch.join("sub/output")).unwrap(), "one two");
        assert_eq!(root.open_dir("sub").unwrap().entries().unwrap(), vec!["input", "output"]);
        root.make_dir("fresh/nested").unwrap();
        root.make_dir("fresh/nested").unwrap();
        assert!(root.open_dir("fresh/nested").is_ok());
    }

    #[test]
    fn atomic_create_new_replace_and_rename() {
        let scratch = Scratch::new();
        let root = ConfinedDir::open_ambient(&scratch.0).unwrap();
        // create_new: first wins (true), second loses (false), contents untouched.
        assert!(root.create_new("marker", b"one").unwrap());
        assert!(!root.create_new("marker", b"two").unwrap());
        assert_eq!(std::fs::read_to_string(scratch.join("marker")).unwrap(), "one");
        // replace: whole-file swap, creating if absent then overwriting.
        root.replace("doc", b"first").unwrap();
        assert_eq!(std::fs::read_to_string(scratch.join("doc")).unwrap(), "first");
        root.replace("doc", b"second").unwrap();
        assert_eq!(std::fs::read_to_string(scratch.join("doc")).unwrap(), "second");
        // rename: moves and replaces the destination.
        root.rename("doc", "marker").unwrap();
        assert_eq!(std::fs::read_to_string(scratch.join("marker")).unwrap(), "second");
        assert!(!root.exists("doc").unwrap());
        // the atomic ops stay confined.
        assert!(root.create_new("../escape", b"x").is_err());
        assert!(root.replace("../escape", b"x").is_err());
        assert!(root.rename("marker", "../escape").is_err());
    }

    #[test]
    fn lexical_escape_is_rejected() {
        let scratch = Scratch::new();
        let root = ConfinedDir::open_ambient(&scratch.0).unwrap();
        assert!(root.file("../secret", true).is_err());
        assert!(root.file("/etc/passwd", true).is_err());
        assert!(root.open_dir("a/../../b").is_err());
        assert!(root.make_dir("../outside").is_err());
    }

    #[test]
    fn a_fresh_operation_rejects_a_swapped_parent() {
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

    #[test]
    fn retained_file_and_subdir_authority_survive_name_replacement() {
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

    #[test]
    fn a_symlink_leaf_is_never_followed_for_writes() {
        let scratch = Scratch::new();
        let root = ConfinedDir::open_ambient(&scratch.0).unwrap();
        std::fs::write(scratch.join("secret"), "private").unwrap();
        std::os::unix::fs::symlink("secret", scratch.join("leaf")).unwrap();
        let leaf = root.file("leaf", false).unwrap();
        assert!(leaf.write_all(b"changed").is_err());
        assert!(leaf.append_all(b"changed").is_err());
        assert_eq!(std::fs::read_to_string(scratch.join("secret")).unwrap(), "private");
    }
}
